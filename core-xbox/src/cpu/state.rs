//! IA-32 architectural register state — the eight GPRs, six segment registers
//! (selector + cached base), EIP, EFLAGS, the control registers CR0..CR4, and
//! the flag-computation + exception helpers.
//!
//! Built from the Intel IA-32 SDM Vol. 1 (basic architecture) and Vol. 3
//! (system programming). Notable x86 facts modelled here:
//!
//! * **Little-endian, variable-length instructions.** Decode/fetch lives in
//!   [`super::exec`]; this struct is pure state.
//! * **GPRs are addressable at three widths.** `EAX` (32), `AX` (low 16), and
//!   `AL`/`AH` (low/high byte). The 8-bit register encoding interleaves the high
//!   bytes (`AL,CL,DL,BL,AH,CH,DH,BH`), which [`Cpu::reg8`]/[`Cpu::set_reg8`]
//!   handle.
//! * **Segmentation.** Every memory reference adds a segment *base* to the
//!   offset. In real mode the base is `selector << 4`; in protected mode it comes
//!   from a descriptor (we cache it and, for the flat Xbox memory model, treat it
//!   as a loaded value). We store both the selector and the cached base.
//! * **EFLAGS.** The arithmetic flags (CF/PF/AF/ZF/SF/OF) plus the control flags
//!   (IF/DF/TF). Bit 1 reads as 1 always.
//! * **Exceptions.** Without an IDT/GDT model we cannot vector a fault to a
//!   handler, so a raised exception records a [`Fault`] and stops the core; the
//!   orchestrator presents the crash screen. A real CPU would push an interrupt
//!   frame and jump through the IDT — that lives in a future phase.

// ---- GPR indices (the x86 register-encoding order) ----
pub const EAX: usize = 0;
pub const ECX: usize = 1;
pub const EDX: usize = 2;
pub const EBX: usize = 3;
pub const ESP: usize = 4;
pub const EBP: usize = 5;
pub const ESI: usize = 6;
pub const EDI: usize = 7;

// ---- segment-register indices (the x86 segment-encoding order) ----
pub const ES: usize = 0;
pub const CS: usize = 1;
pub const SS: usize = 2;
pub const DS: usize = 3;
pub const FS: usize = 4;
pub const GS: usize = 5;

// ---- EFLAGS bit masks ----
pub const CF: u32 = 1 << 0; // carry
pub const PF: u32 = 1 << 2; // parity
pub const AF: u32 = 1 << 4; // auxiliary (BCD) carry
pub const ZF: u32 = 1 << 6; // zero
pub const SF: u32 = 1 << 7; // sign
pub const TF: u32 = 1 << 8; // trap (single-step)
pub const IF: u32 = 1 << 9; // interrupt enable
pub const DF: u32 = 1 << 10; // direction (string ops)
pub const OF: u32 = 1 << 11; // overflow
/// EFLAGS bit 1 is always set (reserved-one).
pub const EFLAGS_ALWAYS_ONE: u32 = 1 << 1;

// ---- CR0 bits ----
pub const CR0_PE: u32 = 1 << 0; // protected-mode enable
pub const CR0_ET: u32 = 1 << 4; // extension type (387 present) — 1 on the P3
pub const CR0_NW: u32 = 1 << 29; // not-write-through
pub const CR0_CD: u32 = 1 << 30; // cache disable
pub const CR0_PG: u32 = 1 << 31; // paging enable

/// The x86 reset vector. At power-on CS selector is `0xF000` with a cached base
/// of `0xFFFF_0000` and EIP is `0x0000_FFF0`, so the first fetch is the linear
/// address `0xFFFF_FFF0` — inside the flash mirror. IA-32 SDM Vol. 3, "Processor
/// State After Reset".
pub const RESET_EIP: u32 = 0x0000_FFF0;
pub const RESET_CS_SELECTOR: u16 = 0xF000;
pub const RESET_CS_BASE: u32 = 0xFFFF_0000;
/// Pentium III "Coppermine" CPUID signature placed in EDX at reset (family 6,
/// model 8, stepping ~10). Read back by `CPUID`/diagnostics; informational here.
pub const RESET_EDX: u32 = 0x0000_068A;

/// An x86 exception/interrupt vector the foundation can raise. Only the ones the
/// starter interpreter can produce are modelled; the value is the IA-32 vector
/// number (IDT entry index). IA-32 SDM Vol. 3, "Interrupt and Exception
/// Handling".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exception {
    /// #DE — divide error (`DIV`/`IDIV` by zero or overflow).
    DivideError = 0,
    /// #UD — invalid opcode. The documented seam for every opcode the foundation
    /// does not yet execute.
    InvalidOpcode = 6,
    /// #DF — double fault.
    DoubleFault = 8,
    /// #GP — general protection.
    GeneralProtection = 13,
    /// #PF — page fault.
    PageFault = 14,
}

impl Exception {
    #[inline]
    pub fn vector(self) -> u8 {
        self as u8
    }
}

/// A recorded fault: the orchestrator reads this to render the crash screen.
/// (A real CPU would push an interrupt frame and vector through the IDT; without
/// an IDT model we stop and report instead of silently looping.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// The IA-32 vector number (e.g. 6 for #UD).
    pub vector: u8,
    /// Optional error code (0 for faults that don't push one).
    pub error_code: u32,
    /// CS:EIP of the faulting instruction.
    pub cs: u16,
    pub eip: u32,
    /// The opcode byte that triggered it (for #UD), else 0.
    pub opcode: u8,
}

/// IA-32 architectural register state.
pub struct Cpu {
    /// General-purpose registers in encoding order: EAX, ECX, EDX, EBX, ESP,
    /// EBP, ESI, EDI. 16- and 8-bit views are sub-fields (see [`Cpu::reg8`]).
    pub regs: [u32; 8],
    /// Instruction pointer (offset within CS). 16-bit (IP) in real mode.
    pub eip: u32,
    /// EFLAGS — arithmetic + control flags. Bit 1 always reads 1.
    pub eflags: u32,

    /// Segment-register selectors: ES, CS, SS, DS, FS, GS.
    pub seg_sel: [u16; 6],
    /// Cached segment bases (added to the offset to form a linear address). Real
    /// mode keeps these as `selector << 4`; protected mode loads them from a
    /// descriptor (modelled as a directly-set value for the flat Xbox map).
    pub seg_base: [u32; 6],

    /// Control registers CR0..CR4 (CR1 is reserved/unused).
    pub cr: [u32; 5],

    /// Set by `HLT` — the CPU idles until an interrupt. Distinct from a fault.
    pub halted: bool,
    /// A recorded unrecoverable fault (no IDT to vector through). When set the
    /// core stops and the orchestrator shows the crash screen.
    pub fault: Option<Fault>,
    /// Total exceptions raised since reset (the host watches this).
    pub exceptions: u64,
    /// Total instructions retired since reset.
    pub instret: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        let mut cpu = Cpu {
            regs: [0; 8],
            eip: RESET_EIP,
            eflags: EFLAGS_ALWAYS_ONE,
            seg_sel: [0; 6],
            seg_base: [0; 6],
            cr: [0; 5],
            halted: false,
            fault: None,
            exceptions: 0,
            instret: 0,
        };
        cpu.regs[EDX] = RESET_EDX;
        cpu.seg_sel[CS] = RESET_CS_SELECTOR;
        cpu.seg_base[CS] = RESET_CS_BASE;
        // P3 reset CR0: ET=1 (387 present), cache disabled (CD|NW), PE=0 (real).
        cpu.cr[0] = CR0_ET | CR0_CD | CR0_NW;
        cpu
    }

    // ---- mode helpers ----
    /// Real mode (CR0.PE clear). Drives the default operand/address size and the
    /// `selector << 4` segment-base rule.
    #[inline]
    pub fn real_mode(&self) -> bool {
        self.cr[0] & CR0_PE == 0
    }

    /// Default operand/address size in bytes for the current mode: 2 (16-bit) in
    /// real mode, 4 (32-bit) in protected mode. (We assume a 32-bit/`D=1` flat
    /// code segment in protected mode — the Xbox kernel runs flat 32-bit.)
    #[inline]
    pub fn default_opsize(&self) -> u8 {
        if self.real_mode() {
            2
        } else {
            4
        }
    }

    // ---- GPR access at three widths ----
    #[inline]
    pub fn reg32(&self, i: usize) -> u32 {
        self.regs[i & 7]
    }
    #[inline]
    pub fn set_reg32(&mut self, i: usize, v: u32) {
        self.regs[i & 7] = v;
    }
    #[inline]
    pub fn reg16(&self, i: usize) -> u32 {
        self.regs[i & 7] & 0xFFFF
    }
    #[inline]
    pub fn set_reg16(&mut self, i: usize, v: u32) {
        let i = i & 7;
        self.regs[i] = (self.regs[i] & 0xFFFF_0000) | (v & 0xFFFF);
    }
    /// 8-bit register by encoding (0..7 = AL,CL,DL,BL,AH,CH,DH,BH).
    #[inline]
    pub fn reg8(&self, i: usize) -> u32 {
        let i = i & 7;
        if i < 4 {
            self.regs[i] & 0xFF // low byte (AL..BL)
        } else {
            (self.regs[i - 4] >> 8) & 0xFF // high byte (AH..BH)
        }
    }
    #[inline]
    pub fn set_reg8(&mut self, i: usize, v: u32) {
        let i = i & 7;
        let v = v & 0xFF;
        if i < 4 {
            self.regs[i] = (self.regs[i] & 0xFFFF_FF00) | v;
        } else {
            let r = i - 4;
            self.regs[r] = (self.regs[r] & 0xFFFF_00FF) | (v << 8);
        }
    }

    /// Read a GPR of the given byte width (1/2/4).
    #[inline]
    pub fn reg(&self, i: usize, size: u8) -> u32 {
        match size {
            1 => self.reg8(i),
            2 => self.reg16(i),
            _ => self.reg32(i),
        }
    }
    /// Write a GPR of the given byte width (1/2/4).
    #[inline]
    pub fn set_reg(&mut self, i: usize, size: u8, v: u32) {
        match size {
            1 => self.set_reg8(i, v),
            2 => self.set_reg16(i, v),
            _ => self.set_reg32(i, v),
        }
    }

    // ---- EFLAGS helpers ----
    #[inline]
    pub fn flag(&self, mask: u32) -> bool {
        self.eflags & mask != 0
    }
    #[inline]
    pub fn set_flag(&mut self, mask: u32, on: bool) {
        if on {
            self.eflags |= mask;
        } else {
            self.eflags &= !mask;
        }
    }

    /// Set a segment register: store the selector and recompute the cached base.
    /// Real mode uses `selector << 4`; protected mode would consult a descriptor
    /// (we keep the existing/explicit base via [`Cpu::set_seg_base`] there).
    #[inline]
    pub fn set_seg(&mut self, seg: usize, selector: u16) {
        self.seg_sel[seg] = selector;
        if self.real_mode() {
            self.seg_base[seg] = (selector as u32) << 4;
        }
    }
    #[inline]
    pub fn set_seg_base(&mut self, seg: usize, base: u32) {
        self.seg_base[seg] = base;
    }

    // ---- flag computation for arithmetic/logic results ----

    /// The sign-bit mask for an operand size (1/2/4 bytes).
    #[inline]
    fn sign_mask(size: u8) -> u32 {
        match size {
            1 => 0x80,
            2 => 0x8000,
            _ => 0x8000_0000,
        }
    }
    /// The value mask for an operand size.
    #[inline]
    pub fn size_mask(size: u8) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    /// Set SF/ZF/PF from a result (the common subset all logic/arith ops share).
    #[inline]
    fn set_szp(&mut self, res: u32, size: u8) {
        let m = Self::size_mask(size);
        let r = res & m;
        self.set_flag(ZF, r == 0);
        self.set_flag(SF, r & Self::sign_mask(size) != 0);
        // Parity is computed over the low 8 bits only (x86 quirk).
        self.set_flag(PF, (r as u8).count_ones() % 2 == 0);
    }

    /// Flags for `a + b = res` (ADD/INC-with-carry semantics for ADD).
    pub fn flags_add(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Self::size_mask(size);
        let res = (a.wrapping_add(b)) & m;
        self.set_szp(res, size);
        self.set_flag(CF, res < (a & m)); // unsigned wrap
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        let sign = Self::sign_mask(size);
        // Overflow: operands same sign, result differs.
        self.set_flag(OF, ((!(a ^ b)) & (a ^ res) & sign) != 0);
        res
    }

    /// Flags for `a - b = res` (SUB/CMP semantics).
    pub fn flags_sub(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Self::size_mask(size);
        let res = (a.wrapping_sub(b)) & m;
        self.set_szp(res, size);
        self.set_flag(CF, (a & m) < (b & m)); // borrow
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        let sign = Self::sign_mask(size);
        // Overflow: operands differ in sign and result sign != a's sign.
        self.set_flag(OF, ((a ^ b) & (a ^ res) & sign) != 0);
        res
    }

    /// Flags for a logical result (AND/OR/XOR/TEST): CF=OF=0, SF/ZF/PF from res,
    /// AF undefined (left clear).
    pub fn flags_logic(&mut self, res: u32, size: u8) -> u32 {
        let r = res & Self::size_mask(size);
        self.set_szp(r, size);
        self.set_flag(CF, false);
        self.set_flag(OF, false);
        self.set_flag(AF, false);
        r
    }

    /// Flags for INC (`a + 1`) / DEC (`a - 1`): like add/sub but **CF is
    /// preserved** (the x86 quirk that makes INC/DEC usable in carry chains).
    pub fn flags_inc(&mut self, a: u32, size: u8) -> u32 {
        let cf = self.flag(CF);
        let res = self.flags_add(a, 1, size);
        self.set_flag(CF, cf);
        res
    }
    pub fn flags_dec(&mut self, a: u32, size: u8) -> u32 {
        let cf = self.flag(CF);
        let res = self.flags_sub(a, 1, size);
        self.set_flag(CF, cf);
        res
    }

    /// Raise an exception: record a [`Fault`] (first one wins) and stop the core.
    pub fn raise(&mut self, ex: Exception, error_code: u32, opcode: u8) {
        self.exceptions = self.exceptions.wrapping_add(1);
        if self.fault.is_none() {
            self.fault = Some(Fault {
                vector: ex.vector(),
                error_code,
                cs: self.seg_sel[CS],
                eip: self.eip,
                opcode,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_state_matches_x86() {
        let cpu = Cpu::new();
        assert!(cpu.real_mode());
        assert_eq!(cpu.eip, 0xFFF0);
        assert_eq!(cpu.seg_sel[CS], 0xF000);
        assert_eq!(cpu.seg_base[CS], 0xFFFF_0000);
        assert_eq!(cpu.regs[EDX], RESET_EDX);
        assert_eq!(cpu.eflags & EFLAGS_ALWAYS_ONE, EFLAGS_ALWAYS_ONE);
        // The first instruction fetch is the linear reset vector.
        assert_eq!(cpu.seg_base[CS].wrapping_add(cpu.eip), 0xFFFF_FFF0);
    }

    #[test]
    fn gpr_widths_alias_correctly() {
        let mut cpu = Cpu::new();
        cpu.set_reg32(EAX, 0x1122_3344);
        assert_eq!(cpu.reg16(EAX), 0x3344);
        assert_eq!(cpu.reg8(EAX), 0x44); // AL
        // AH is encoding 4 (since EAX=0): high byte of EAX.
        assert_eq!(cpu.reg8(4), 0x33);
        cpu.set_reg8(4, 0xFF); // AH = 0xFF
        assert_eq!(cpu.reg32(EAX), 0x1122_FF44);
        cpu.set_reg16(EAX, 0xBEEF);
        assert_eq!(cpu.reg32(EAX), 0x1122_BEEF, "upper 16 preserved");
    }

    #[test]
    fn add_sub_flags() {
        let mut cpu = Cpu::new();
        // 0xFF + 1 (byte) => 0, CF + ZF set, no OF (signed -1 + 1 = 0).
        let r = cpu.flags_add(0xFF, 1, 1);
        assert_eq!(r, 0);
        assert!(cpu.flag(ZF) && cpu.flag(CF));
        assert!(!cpu.flag(OF));
        // 0x7F + 1 (byte) => 0x80, signed overflow.
        let r = cpu.flags_add(0x7F, 1, 1);
        assert_eq!(r, 0x80);
        assert!(cpu.flag(OF) && cpu.flag(SF));
        // 5 - 9 (byte) => borrow (CF) and negative (SF).
        let r = cpu.flags_sub(5, 9, 1);
        assert_eq!(r, 0xFC);
        assert!(cpu.flag(CF) && cpu.flag(SF));
    }

    #[test]
    fn inc_preserves_carry() {
        let mut cpu = Cpu::new();
        cpu.set_flag(CF, true);
        cpu.flags_inc(0x10, 4);
        assert!(cpu.flag(CF), "INC must not touch CF");
    }

    #[test]
    fn raise_records_first_fault_only() {
        let mut cpu = Cpu::new();
        cpu.raise(Exception::InvalidOpcode, 0, 0x0F);
        cpu.raise(Exception::GeneralProtection, 0, 0x00);
        assert_eq!(cpu.exceptions, 2);
        assert_eq!(cpu.fault.unwrap().vector, 6, "first fault wins");
    }
}
