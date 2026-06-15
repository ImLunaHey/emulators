//! x87 FPU (and a sliver of MMX/SSE) state for the IA-32 interpreter.
//!
//! The Xbox CPU is a Pentium III with an on-die x87 floating-point unit. Real
//! game code (Halo 2's init/math) hits the `0xD8..0xDF` ESC opcodes for
//! single-precision/double-precision arithmetic, comparisons and control-word
//! fiddling. This module models that unit.
//!
//! # Where the state lives
//!
//! The architectural integer state lives in [`super::state::Cpu`], which this
//! task may not modify, and the FPU opcode handlers in [`super::exec`] have no
//! place to hang a register stack. The x87 unit is therefore kept in a
//! **process-/thread-local** [`Fpu`] instance accessed through [`with_fpu`].
//!
//! A single guest is single-threaded through the interpreter (one [`Cpu::step`]
//! at a time, no re-entrancy), so a `thread_local!` is a sound place to keep the
//! shared register file for now. If the core ever runs multiple guests on one OS
//! thread this would need to move onto an owned field — but that requires
//! editing `state.rs`, which is out of scope here.
//!
//! # Precision
//!
//! Real x87 keeps an 80-bit extended-precision (`f80`) stack. We approximate it
//! with `f64`, which is exact for the single/double loads and stores game code
//! uses and matches the visible behaviour of FCOM/FNSTSW driven branches. The
//! 80-bit mantissa rounding is deliberately ignored.

use std::cell::RefCell;

/// x87 status-word condition-code bits (C0..C3). FCOM/FUCOM/FTST set these and
/// `FNSTSW AX` copies the whole status word into AX, where game code does
/// `fnstsw ax; sahf; jcc`. The SAHF mapping relies on C0->CF, C2->PF, C3->ZF.
pub const SW_C0: u16 = 1 << 8;
pub const SW_C1: u16 = 1 << 9;
pub const SW_C2: u16 = 1 << 10;
pub const SW_C3: u16 = 1 << 14;
/// Top-of-stack pointer field (bits 11..13) of the status word.
const SW_TOP_SHIFT: u16 = 11;
const SW_TOP_MASK: u16 = 0x7 << SW_TOP_SHIFT;

/// The default control word after FNINIT/reset: all exceptions masked, round to
/// nearest, 64-bit (double extended) precision control. Value 0x037F is what a
/// real x87 loads at init.
pub const CW_DEFAULT: u16 = 0x037F;

/// The eight-deep x87 register stack plus the control/status/tag words.
///
/// The stack is a ring of eight physical registers indexed relative to TOP:
/// `ST(0)` is `regs[top]`, `ST(1)` is `regs[(top + 1) & 7]`, and so on. `FLD`
/// (push) decrements TOP then writes ST(0); `FSTP` (pop) reads ST(0) then
/// increments TOP.
pub struct Fpu {
    /// Physical register file (not yet rotated by TOP).
    regs: [f64; 8],
    /// Top-of-stack pointer (0..7); ST(i) == regs[(top + i) & 7].
    top: u8,
    /// Tag word: 2 bits per physical register (0 = valid, 3 = empty). We only
    /// track valid/empty, which is all FFREE / FNSTSW consumers need.
    tag: u16,
    /// Control word (rounding/precision/exception masks). Honoured for FLDCW/
    /// FNSTCW round-trips; rounding mode is not otherwise modelled (we use the
    /// host f64 rounding, which is round-to-nearest).
    cw: u16,
    /// Status word's condition codes + exception flags (TOP is merged in on
    /// read via [`Fpu::status_word`]).
    sw: u16,
}

impl Default for Fpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Fpu {
    pub fn new() -> Self {
        Fpu {
            regs: [0.0; 8],
            top: 0,
            tag: 0xFFFF, // all empty
            cw: CW_DEFAULT,
            sw: 0,
        }
    }

    /// FNINIT / reset: empty the stack and restore the default control word.
    pub fn init(&mut self) {
        self.regs = [0.0; 8];
        self.top = 0;
        self.tag = 0xFFFF;
        self.cw = CW_DEFAULT;
        self.sw = 0;
    }

    /// Physical index of the logical register ST(i).
    #[inline]
    fn phys(&self, i: usize) -> usize {
        (self.top as usize).wrapping_add(i) & 7
    }

    /// Read ST(i).
    #[inline]
    pub fn st(&self, i: usize) -> f64 {
        self.regs[self.phys(i)]
    }

    /// Write ST(i) (and mark it valid).
    #[inline]
    pub fn set_st(&mut self, i: usize, v: f64) {
        let p = self.phys(i);
        self.regs[p] = v;
        self.set_tag_valid(p, true);
    }

    /// Push a value: decrement TOP, store into the new ST(0).
    #[inline]
    pub fn push(&mut self, v: f64) {
        self.top = self.top.wrapping_sub(1) & 7;
        let p = self.top as usize;
        self.regs[p] = v;
        self.set_tag_valid(p, true);
    }

    /// Pop ST(0): read it, mark empty, increment TOP.
    #[inline]
    pub fn pop(&mut self) -> f64 {
        let p = self.top as usize;
        let v = self.regs[p];
        self.set_tag_valid(p, false);
        self.top = self.top.wrapping_add(1) & 7;
        v
    }

    /// FINCSTP — increment TOP without touching tags/regs.
    #[inline]
    pub fn incstp(&mut self) {
        self.top = self.top.wrapping_add(1) & 7;
    }
    /// FDECSTP — decrement TOP without touching tags/regs.
    #[inline]
    pub fn decstp(&mut self) {
        self.top = self.top.wrapping_sub(1) & 7;
    }

    /// FFREE ST(i): mark the register empty (no stack movement).
    #[inline]
    pub fn free(&mut self, i: usize) {
        let p = self.phys(i);
        self.set_tag_valid(p, false);
    }

    /// Exchange ST(0) and ST(i) (FXCH).
    #[inline]
    pub fn xch(&mut self, i: usize) {
        let a = self.phys(0);
        let b = self.phys(i);
        self.regs.swap(a, b);
    }

    #[inline]
    fn set_tag_valid(&mut self, phys: usize, valid: bool) {
        let shift = (phys & 7) * 2;
        self.tag &= !(0x3 << shift);
        if !valid {
            self.tag |= 0x3 << shift; // 3 = empty
        }
    }

    // ---- control / status words ----
    #[inline]
    pub fn control_word(&self) -> u16 {
        self.cw
    }
    #[inline]
    pub fn set_control_word(&mut self, v: u16) {
        self.cw = v;
    }
    #[inline]
    pub fn tag_word(&self) -> u16 {
        self.tag
    }

    /// The full status word, with the current TOP merged into bits 11..13.
    #[inline]
    pub fn status_word(&self) -> u16 {
        (self.sw & !SW_TOP_MASK) | ((self.top as u16) << SW_TOP_SHIFT)
    }

    /// FNCLEX — clear the exception flags (low byte) and the busy/ES bits.
    #[inline]
    pub fn clear_exceptions(&mut self) {
        self.sw &= !0x80FF;
    }

    /// Set the condition-code bits (C3 C2 C1 C0) from an ordering result, used
    /// by FCOM/FUCOM/FTST.  `Ordering`-style: less / equal / greater / unordered.
    ///
    /// x87 encoding (from the SDM): C3 C2 C0 =
    ///   ST(0) > src  -> 0 0 0
    ///   ST(0) < src  -> 0 0 1
    ///   ST(0) = src  -> 1 0 0
    ///   unordered    -> 1 1 1
    pub fn set_compare(&mut self, a: f64, b: f64) {
        // Clear C0..C3 first.
        self.sw &= !(SW_C0 | SW_C1 | SW_C2 | SW_C3);
        if a.is_nan() || b.is_nan() {
            self.sw |= SW_C0 | SW_C2 | SW_C3; // unordered
        } else if a > b {
            // all clear
        } else if a < b {
            self.sw |= SW_C0;
        } else {
            self.sw |= SW_C3; // equal
        }
    }
}

thread_local! {
    /// The shared x87 unit for the interpreter on this thread. See the module
    /// docs for why this is a thread-local rather than a `Cpu` field.
    static FPU: RefCell<Fpu> = RefCell::new(Fpu::new());
}

/// Run `f` with mutable access to the thread-local FPU.
pub fn with_fpu<R>(f: impl FnOnce(&mut Fpu) -> R) -> R {
    FPU.with(|c| f(&mut c.borrow_mut()))
}

/// Reset the thread-local FPU (used by tests to isolate state; FNINIT also
/// resets it during execution).
pub fn reset_fpu() {
    FPU.with(|c| c.borrow_mut().init());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_is_lifo() {
        let mut f = Fpu::new();
        f.push(1.0);
        f.push(2.0);
        f.push(3.0);
        assert_eq!(f.st(0), 3.0);
        assert_eq!(f.st(1), 2.0);
        assert_eq!(f.st(2), 1.0);
        assert_eq!(f.pop(), 3.0);
        assert_eq!(f.pop(), 2.0);
        assert_eq!(f.st(0), 1.0);
    }

    #[test]
    fn xch_swaps_st0_sti() {
        let mut f = Fpu::new();
        f.push(10.0); // ST(1)
        f.push(20.0); // ST(0)
        f.xch(1);
        assert_eq!(f.st(0), 10.0);
        assert_eq!(f.st(1), 20.0);
    }

    #[test]
    fn init_resets_control_word() {
        let mut f = Fpu::new();
        f.set_control_word(0x1234);
        f.push(5.0);
        f.init();
        assert_eq!(f.control_word(), CW_DEFAULT);
        assert_eq!(f.tag_word(), 0xFFFF, "all empty after init");
    }

    #[test]
    fn compare_sets_condition_codes() {
        let mut f = Fpu::new();
        // greater: all clear
        f.set_compare(3.0, 1.0);
        let sw = f.status_word();
        assert_eq!(sw & (SW_C0 | SW_C2 | SW_C3), 0, "greater -> C0=C2=C3=0");
        // less: C0
        f.set_compare(1.0, 3.0);
        let sw = f.status_word();
        assert_eq!(sw & SW_C0, SW_C0);
        assert_eq!(sw & SW_C3, 0);
        // equal: C3
        f.set_compare(2.0, 2.0);
        let sw = f.status_word();
        assert_eq!(sw & SW_C3, SW_C3);
        assert_eq!(sw & SW_C0, 0);
        // unordered: C0,C2,C3
        f.set_compare(f64::NAN, 1.0);
        let sw = f.status_word();
        assert_eq!(sw & (SW_C0 | SW_C2 | SW_C3), SW_C0 | SW_C2 | SW_C3);
    }

    #[test]
    fn status_word_carries_top() {
        let mut f = Fpu::new();
        f.push(1.0); // top = 7
        let top = (f.status_word() >> 11) & 7;
        assert_eq!(top, 7);
    }
}
