//! Gekko (PowerPC 750) instruction decode + execution.
//!
//! Built from scratch against the PowerPC User ISA ("Book I") opcode tables and
//! YAGCD §2.2. The executor is a plain interpreter: each [`Cpu::step`] checks
//! for a pending external interrupt, fetches the big-endian instruction word at
//! `pc`, decodes it, executes it, and advances `pc` by 4 (PowerPC has **no**
//! branch/load delay slots — the simplest possible step loop).
//!
//! # Encoding (PowerPC, big-endian bit numbering: bit 0 is the MSB)
//!
//! The 6-bit *primary* opcode is bits 0..5, i.e. `word >> 26`. The operand
//! layout depends on the form:
//!
//! * **D-form** (e.g. `addi`, `lwz`, `ori`): `opcd | rD/rS(5) | rA(5) | imm(16)`.
//! * **X-form / XO-form** (primary 31, e.g. `add`, `or`, `cmp`): a 9/10-bit
//!   *extended* opcode in bits 21..30 (`(word >> 1) & 0x3FF`) selects the op.
//! * **I-form** (`b`): `opcd | LI(24) | AA | LK` — a relative/absolute branch.
//! * **B-form** (`bc`): `opcd | BO(5) | BI(5) | BD(14) | AA | LK`.
//! * **M-form** (`rlwinm`): `opcd | rS | rA | SH(5) | MB(5) | ME(5) | Rc`.
//!
//! # Coverage (this foundation)
//!
//! Implemented + unit-tested: `addi`, `addis`, `add`/`add.`, `subf`,
//! `or`/`ori`/`oris`, `and`/`andi.`, `cmp`/`cmpi`, `b`/`bl`, `bc`, `blr`,
//! `lwz`, `stw`, `mfspr`/`mtspr`, `rlwinm`, plus `sc`, `rfi` and the common
//! `ori 0,0,0` no-op. Everything else decodes to [`Decoded::Unimplemented`],
//! which raises a Program exception (a clear, documented seam — **never** a
//! silent no-op). All arithmetic is wrapping `u32`; big-endian throughout.

use super::spr::{self, Exception};
use super::state::{Cpu, CR_EQ, CR_GT, CR_LT, CR_SO};
use crate::bus::Bus;

/// A decoded 32-bit PowerPC instruction word. Field accessors mirror the
/// PowerPC ISA encoding tables; all are cheap bit extracts. Big-endian bit
/// numbering, so field "bit n" is `word >> (31 - n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instr(pub u32);

impl Instr {
    /// Primary opcode, bits 0..5.
    #[inline]
    pub fn opcd(self) -> u32 {
        self.0 >> 26
    }
    /// Extended opcode (X/XO-form), bits 21..30.
    #[inline]
    pub fn xo(self) -> u32 {
        (self.0 >> 1) & 0x3FF
    }
    /// rD / rS field, bits 6..10.
    #[inline]
    pub fn d(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
    /// rA field, bits 11..15.
    #[inline]
    pub fn a(self) -> u32 {
        (self.0 >> 16) & 0x1F
    }
    /// rB field, bits 16..20.
    #[inline]
    pub fn b(self) -> u32 {
        (self.0 >> 11) & 0x1F
    }
    /// Shift amount SH (M-form), bits 16..20 (== rB slot).
    #[inline]
    pub fn sh(self) -> u32 {
        (self.0 >> 11) & 0x1F
    }
    /// Mask begin MB (M-form), bits 21..25.
    #[inline]
    pub fn mb(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }
    /// Mask end ME (M-form), bits 26..30.
    #[inline]
    pub fn me(self) -> u32 {
        (self.0 >> 1) & 0x1F
    }
    /// Record bit Rc (bit 31) — set CR0 from the result.
    #[inline]
    pub fn rc(self) -> bool {
        self.0 & 1 != 0
    }
    /// OE bit (bit 21 within XO-form) — enable overflow recording. We decode it
    /// but the foundation does not yet update XER[OV].
    #[inline]
    pub fn oe(self) -> bool {
        self.0 & (1 << 10) != 0
    }
    /// 16-bit immediate, zero-extended.
    #[inline]
    pub fn uimm(self) -> u32 {
        self.0 & 0xFFFF
    }
    /// 16-bit immediate, sign-extended.
    #[inline]
    pub fn simm(self) -> u32 {
        (self.0 & 0xFFFF) as i16 as i32 as u32
    }
    /// SPR number (mfspr/mtspr) — the split 10-bit field at bits 11..20, with
    /// its two 5-bit halves swapped (PowerPC quirk).
    #[inline]
    pub fn spr(self) -> u32 {
        let raw = (self.0 >> 11) & 0x3FF;
        ((raw & 0x1F) << 5) | ((raw >> 5) & 0x1F)
    }
    /// BO field (branch options), bits 6..10.
    #[inline]
    pub fn bo(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
    /// BI field (CR bit to test), bits 11..15.
    #[inline]
    pub fn bi(self) -> u32 {
        (self.0 >> 16) & 0x1F
    }
    /// crfD field (cmp target CR field), bits 6..8.
    #[inline]
    pub fn crfd(self) -> u32 {
        (self.0 >> 23) & 0x7
    }
    /// AA bit (absolute address) for branches, bit 30.
    #[inline]
    pub fn aa(self) -> bool {
        self.0 & 0b10 != 0
    }
    /// LK bit (link) for branches, bit 31.
    #[inline]
    pub fn lk(self) -> bool {
        self.0 & 1 != 0
    }
    /// Sign-extended 24-bit branch displacement (I-form), `LI << 2`.
    #[inline]
    pub fn li(self) -> u32 {
        let li = self.0 & 0x03FF_FFFC; // bits 6..29, already << 2 aligned
        // sign-extend from bit 25 (the 26-bit signed field).
        (li ^ 0x0200_0000).wrapping_sub(0x0200_0000)
    }
    /// Sign-extended 14-bit conditional-branch displacement (B-form), `BD << 2`.
    #[inline]
    pub fn bd(self) -> u32 {
        let bd = self.0 & 0x0000_FFFC; // bits 16..29, << 2 aligned
        (bd ^ 0x0000_8000).wrapping_sub(0x0000_8000)
    }
}

/// The decode result. A closed enum + exhaustive match per the project idioms.
/// `Unimplemented` is the explicit, documented seam for every opcode the
/// foundation does not yet execute — it raises a Program exception rather than
/// silently doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Addi,
    Addis,
    Add,
    Subf,
    Or,
    Ori,
    Oris,
    And,
    Andi,
    Cmp,
    Cmpi,
    /// Unconditional branch `b`/`ba`/`bl`/`bla`.
    Branch,
    /// Conditional branch `bc`/`bca`/`bcl`/`bcla`.
    BranchCond,
    /// `bclr`/`bclrl` (the common `blr` return form).
    BranchClr,
    Lwz,
    Stw,
    Mfspr,
    Mtspr,
    Rlwinm,
    /// `sc` — system call.
    Sc,
    /// `rfi` — return from interrupt.
    Rfi,
    /// Any opcode not yet handled by this foundation.
    Unimplemented,
}

impl Cpu {
    /// Execute one instruction. PowerPC has no delay slots: sample the interrupt
    /// line, fetch, decode, execute, advance `pc` by 4 (unless a branch moved
    /// it). All accesses are big-endian via [`Bus`].
    pub fn step(&mut self, bus: &mut impl Bus) {
        // Sample the external interrupt line at the instruction boundary. If a
        // device IRQ is pending and MSR[EE] is set, enter the handler instead of
        // executing the fetched instruction.
        if self.irq_pending && self.spr.ee() {
            self.raise_exception(Exception::ExternalInterrupt);
            return;
        }

        // Instruction fetch must be word-aligned; PowerPC raises a (machine
        // check / alignment) for a misaligned PC. We model it as InstructionStorage.
        if self.pc & 3 != 0 {
            self.raise_exception(Exception::InstructionStorage);
            return;
        }
        let word = bus.fetch32(self.pc);
        let i = Instr(word);

        // `pc` advances by 4 by default; a taken branch overwrites it inside the
        // handler. Capture the fall-through here so branches can compute targets
        // and link addresses relative to the *current* instruction.
        let next = self.pc.wrapping_add(4);
        let mut redirect = next;

        match decode(i) {
            Decoded::Addi => self.op_addi(i),
            Decoded::Addis => self.op_addis(i),
            Decoded::Add => self.op_add(i),
            Decoded::Subf => self.op_subf(i),
            Decoded::Or => self.op_or(i),
            Decoded::Ori => self.op_ori(i),
            Decoded::Oris => self.op_oris(i),
            Decoded::And => self.op_and(i),
            Decoded::Andi => self.op_andi(i),
            Decoded::Cmp => self.op_cmp(i),
            Decoded::Cmpi => self.op_cmpi(i),
            Decoded::Branch => redirect = self.op_branch(i),
            Decoded::BranchCond => redirect = self.op_bc(i, next),
            Decoded::BranchClr => redirect = self.op_bclr(i, next),
            Decoded::Lwz => self.op_lwz(i, bus),
            Decoded::Stw => self.op_stw(i, bus),
            Decoded::Mfspr => self.op_mfspr(i),
            Decoded::Mtspr => self.op_mtspr(i),
            Decoded::Rlwinm => self.op_rlwinm(i),
            Decoded::Sc => {
                self.pc = next;
                self.raise_exception(Exception::SystemCall);
                return;
            }
            Decoded::Rfi => {
                self.return_from_interrupt();
                return; // rfi set pc itself
            }
            Decoded::Unimplemented => {
                // The documented seam: an opcode we don't execute yet takes the
                // PowerPC Program exception (illegal/unimplemented instruction),
                // never a silent no-op.
                self.pc = next;
                self.raise_exception(Exception::Program);
                return;
            }
        }

        self.pc = redirect;
    }

    // ===================== integer immediate =====================
    /// `addi rD, rA, SIMM` — rD = (rA|0) + sign-extended immediate.
    fn op_addi(&mut self, i: Instr) {
        let r = self.ra_or_zero(i.a()).wrapping_add(i.simm());
        self.set_gpr(i.d(), r);
    }

    /// `addis rD, rA, SIMM` — rD = (rA|0) + (SIMM << 16).
    fn op_addis(&mut self, i: Instr) {
        let r = self.ra_or_zero(i.a()).wrapping_add(i.uimm() << 16);
        self.set_gpr(i.d(), r);
    }

    /// `ori rA, rS, UIMM` — rA = rS | zero-extended immediate. (`ori 0,0,0` is
    /// the canonical PowerPC nop.)
    fn op_ori(&mut self, i: Instr) {
        let r = self.gpr(i.d()) | i.uimm();
        self.set_gpr(i.a(), r);
    }

    /// `oris rA, rS, UIMM` — rA = rS | (UIMM << 16).
    fn op_oris(&mut self, i: Instr) {
        let r = self.gpr(i.d()) | (i.uimm() << 16);
        self.set_gpr(i.a(), r);
    }

    /// `andi. rA, rS, UIMM` — rA = rS & UIMM; always sets CR0 (record form).
    fn op_andi(&mut self, i: Instr) {
        let r = self.gpr(i.d()) & i.uimm();
        self.set_gpr(i.a(), r);
        self.set_cr0(r);
    }

    /// `cmpi crfD, L, rA, SIMM` — signed compare rA against the immediate.
    fn op_cmpi(&mut self, i: Instr) {
        let a = self.gpr(i.a()) as i32;
        let b = i.simm() as i32;
        let field = self.compare_signed(a, b);
        self.set_cr_field(i.crfd(), field);
    }

    // ===================== integer register (X/XO-form) =====================
    /// `add rD, rA, rB` (+ optional `.` record form).
    fn op_add(&mut self, i: Instr) {
        let r = self.gpr(i.a()).wrapping_add(self.gpr(i.b()));
        self.set_gpr(i.d(), r);
        if i.rc() {
            self.set_cr0(r);
        }
    }

    /// `subf rD, rA, rB` — rD = rB - rA (PowerPC "subtract from"; rA is the
    /// subtrahend). (+ optional `.` record form.)
    fn op_subf(&mut self, i: Instr) {
        let r = self.gpr(i.b()).wrapping_sub(self.gpr(i.a()));
        self.set_gpr(i.d(), r);
        if i.rc() {
            self.set_cr0(r);
        }
    }

    /// `or rA, rS, rB` (+ optional `.`). Note `or rA,rS,rS` is the `mr` mnemonic.
    fn op_or(&mut self, i: Instr) {
        let r = self.gpr(i.d()) | self.gpr(i.b());
        self.set_gpr(i.a(), r);
        if i.rc() {
            self.set_cr0(r);
        }
    }

    /// `and rA, rS, rB` (+ optional `.`).
    fn op_and(&mut self, i: Instr) {
        let r = self.gpr(i.d()) & self.gpr(i.b());
        self.set_gpr(i.a(), r);
        if i.rc() {
            self.set_cr0(r);
        }
    }

    /// `cmp crfD, L, rA, rB` — signed register compare.
    fn op_cmp(&mut self, i: Instr) {
        let a = self.gpr(i.a()) as i32;
        let b = self.gpr(i.b()) as i32;
        let field = self.compare_signed(a, b);
        self.set_cr_field(i.crfd(), field);
    }

    /// Build the 4-bit CR field for a signed comparison, folding in XER[SO].
    #[inline]
    fn compare_signed(&self, a: i32, b: i32) -> u32 {
        let mut field = match a.cmp(&b) {
            core::cmp::Ordering::Less => CR_LT,
            core::cmp::Ordering::Greater => CR_GT,
            core::cmp::Ordering::Equal => CR_EQ,
        };
        if self.xer & spr::XER_SO != 0 {
            field |= CR_SO;
        }
        field
    }

    // ===================== rotate / mask =====================
    /// `rlwinm rA, rS, SH, MB, ME` — Rotate Left Word Immediate then AND with
    /// Mask. Rotate rS left by SH, AND with the mask of 1-bits from bit MB to
    /// bit ME (inclusive, big-endian bit numbering, possibly wrapping).
    fn op_rlwinm(&mut self, i: Instr) {
        let rotated = self.gpr(i.d()).rotate_left(i.sh());
        let mask = mask_mb_me(i.mb(), i.me());
        let r = rotated & mask;
        self.set_gpr(i.a(), r);
        if i.rc() {
            self.set_cr0(r);
        }
    }

    // ===================== branches =====================
    /// `b`/`ba`/`bl`/`bla` — unconditional branch. Returns the new PC.
    fn op_branch(&mut self, i: Instr) -> u32 {
        let target = if i.aa() {
            i.li() // absolute: LI is the target
        } else {
            self.pc.wrapping_add(i.li()) // relative to this instruction
        };
        if i.lk() {
            self.lr = self.pc.wrapping_add(4);
        }
        target
    }

    /// `bc`/`bca`/`bcl`/`bcla` — conditional branch. Decrements CTR when BO
    /// requests it and tests the CR bit selected by BI. `next` is the
    /// fall-through PC. Returns the new PC.
    fn op_bc(&mut self, i: Instr, next: u32) -> u32 {
        let take = self.branch_condition(i.bo(), i.bi());
        if i.lk() {
            self.lr = next;
        }
        if take {
            if i.aa() {
                i.bd()
            } else {
                self.pc.wrapping_add(i.bd())
            }
        } else {
            next
        }
    }

    /// `bclr`/`bclrl` — branch conditionally to the address in LR (the common
    /// `blr` return is `bclr` with a BO of "always"). Returns the new PC.
    fn op_bclr(&mut self, i: Instr, next: u32) -> u32 {
        let take = self.branch_condition(i.bo(), i.bi());
        let target = self.lr & !3;
        if i.lk() {
            self.lr = next;
        }
        if take {
            target
        } else {
            next
        }
    }

    /// Evaluate a PowerPC branch BO/BI condition. Handles the CTR-decrement
    /// (BO bit 2 clear) and the CR-bit test (BO bit 4 clear) per the BO
    /// encoding table; the "branch always" BO (0b10100 / 20) short-circuits.
    fn branch_condition(&mut self, bo: u32, bi: u32) -> bool {
        // BO bits (big-endian numbering within the 5-bit field):
        //   bit0 (0x10): branch if condition true is ignored (CR don't-care)
        //   bit1 (0x08): the desired CR-bit value (1 = true)
        //   bit2 (0x04): don't decrement CTR
        //   bit3 (0x02): the CTR==0 test polarity
        let ctr_ok = if bo & 0x04 != 0 {
            true // don't decrement / don't test CTR
        } else {
            self.ctr = self.ctr.wrapping_sub(1);
            let ctr_zero = self.ctr == 0;
            // bit3 set ⇒ branch when CTR==0; clear ⇒ branch when CTR!=0.
            (bo & 0x02 != 0) == ctr_zero
        };
        let cond_ok = if bo & 0x10 != 0 {
            true // CR don't-care (branch always w.r.t. the condition)
        } else {
            let crbit = (self.cr >> (31 - bi)) & 1;
            crbit == ((bo >> 3) & 1)
        };
        ctr_ok && cond_ok
    }

    // ===================== load / store =====================
    /// `lwz rD, d(rA)` — load word and zero. Big-endian 32-bit load from
    /// `(rA|0) + d`.
    fn op_lwz(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.ra_or_zero(i.a()).wrapping_add(i.simm());
        let v = bus.read32(addr);
        self.set_gpr(i.d(), v);
    }

    /// `stw rS, d(rA)` — store word. Big-endian 32-bit store to `(rA|0) + d`.
    fn op_stw(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.ra_or_zero(i.a()).wrapping_add(i.simm());
        bus.write32(addr, self.gpr(i.d()));
    }

    // ===================== SPR move =====================
    /// `mfspr rD, SPR` — move from special-purpose register. LR/CTR/XER are
    /// owned by the CPU struct; everything else routes through [`spr::Spr`].
    fn op_mfspr(&mut self, i: Instr) {
        let v = match i.spr() {
            spr::SPR_LR => self.lr,
            spr::SPR_CTR => self.ctr,
            spr::SPR_XER => self.xer,
            other => self.spr.read(other),
        };
        self.set_gpr(i.d(), v);
    }

    /// `mtspr SPR, rS` — move to special-purpose register.
    fn op_mtspr(&mut self, i: Instr) {
        let v = self.gpr(i.d());
        match i.spr() {
            spr::SPR_LR => self.lr = v,
            spr::SPR_CTR => self.ctr = v,
            spr::SPR_XER => self.xer = v,
            other => self.spr.write(other, v),
        }
    }
}

/// Build the PowerPC `rlwinm`/`rlwnm` mask from a (MB, ME) pair. The mask has
/// 1-bits from big-endian bit MB through bit ME inclusive. When MB > ME the
/// range wraps around the word end (PowerPC defines this as the complement of
/// the [ME+1, MB-1] run).
#[inline]
fn mask_mb_me(mb: u32, me: u32) -> u32 {
    // big-endian bit n ⇒ little-endian bit (31 - n).
    let begin = 31 - (mb & 31);
    let end = 31 - (me & 31);
    if mb <= me {
        // contiguous run from `end`..=`begin`
        let width = begin - end + 1;
        if width == 32 {
            0xFFFF_FFFF
        } else {
            ((1u32 << width) - 1) << end
        }
    } else {
        // wrapping run: complement of the gap (ME+1 .. MB-1).
        let inv = mask_mb_me(me + 1, mb - 1);
        !inv
    }
}

/// Pure decode of a 32-bit word into a [`Decoded`] op. Separated from execution
/// so it can be unit-tested directly and so the dispatch stays a single
/// exhaustive `match`.
pub fn decode(i: Instr) -> Decoded {
    match i.opcd() {
        7 | 8 => Decoded::Unimplemented, // mulli / subfic (not yet)
        10 => Decoded::Unimplemented,    // cmpli
        11 => Decoded::Cmpi,             // cmpi
        12 | 13 => Decoded::Unimplemented, // addic / addic.
        14 => Decoded::Addi,             // addi
        15 => Decoded::Addis,            // addis
        16 => Decoded::BranchCond,       // bc / bca / bcl / bcla
        17 => Decoded::Sc,               // sc
        18 => Decoded::Branch,           // b / ba / bl / bla
        19 => decode_19(i),              // branch-to-LR/CTR + rfi (XL-form)
        21 => Decoded::Rlwinm,           // rlwinm
        24 => Decoded::Ori,              // ori
        25 => Decoded::Oris,             // oris
        28 => Decoded::Andi,             // andi.
        31 => decode_31(i),             // X/XO-form register ops
        32 => Decoded::Lwz,              // lwz
        36 => Decoded::Stw,              // stw
        _ => Decoded::Unimplemented,
    }
}

/// Decode primary opcode 19 (XL-form: branch-to-link/count, CR ops, rfi).
fn decode_19(i: Instr) -> Decoded {
    match i.xo() {
        16 => Decoded::BranchClr, // bclr / bclrl (blr)
        50 => Decoded::Rfi,       // rfi
        _ => Decoded::Unimplemented,
    }
}

/// Decode primary opcode 31 (the big X/XO-form group). The 10-bit extended
/// opcode selects the operation; the OE/Rc bits are handled by the executor.
fn decode_31(i: Instr) -> Decoded {
    // Strip OE (bit 21 ⇒ mask 0x200 within the 10-bit XO) for the XO-form
    // arithmetic ops so `addo`/`subfo` map to the same handler.
    match i.xo() & 0x1FF {
        266 => Decoded::Add,  // add (XO 266, OE-stripped)
        40 => Decoded::Subf,  // subf (XO 40)
        _ => match i.xo() {
            444 => Decoded::Or,    // or
            28 => Decoded::And,    // and
            0 => Decoded::Cmp,     // cmp
            339 => Decoded::Mfspr, // mfspr
            467 => Decoded::Mtspr, // mtspr
            _ => Decoded::Unimplemented,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::Gc;

    // ---- instruction assembly helpers (PowerPC, big-endian field layout) ----
    fn d_form(opcd: u32, d: u32, a: u32, imm: u32) -> u32 {
        (opcd << 26) | (d << 21) | (a << 16) | (imm & 0xFFFF)
    }
    fn xo_form(opcd: u32, d: u32, a: u32, b: u32, oe: u32, xo: u32, rc: u32) -> u32 {
        (opcd << 26) | (d << 21) | (a << 16) | (b << 11) | (oe << 10) | (xo << 1) | rc
    }
    /// X-form for logical ops (rS in the D slot, rA target, rB source).
    fn x_form(opcd: u32, s: u32, a: u32, b: u32, xo: u32, rc: u32) -> u32 {
        (opcd << 26) | (s << 21) | (a << 16) | (b << 11) | (xo << 1) | rc
    }
    fn m_form(opcd: u32, s: u32, a: u32, sh: u32, mb: u32, me: u32, rc: u32) -> u32 {
        (opcd << 26) | (s << 21) | (a << 16) | (sh << 11) | (mb << 6) | (me << 1) | rc
    }

    /// A test harness: a `Gc` with a small program written into RAM (cached
    /// window 0x8000_0000) and the CPU pointed at it.
    fn harness(program: &[u32]) -> Gc {
        let mut gc = Gc::new();
        let base = 0x8000_0000u32;
        for (idx, &w) in program.iter().enumerate() {
            gc.write32(base + (idx as u32) * 4, w);
        }
        gc.cpu.pc = base;
        gc
    }

    /// Step the CPU `n` times, split-borrowing it out of the `Gc` bus.
    fn run(gc: &mut Gc, n: usize) {
        for _ in 0..n {
            let mut cpu = std::mem::take(&mut gc.cpu);
            cpu.step(gc);
            gc.cpu = cpu;
        }
    }

    fn get(gc: &Gc, r: u32) -> u32 {
        gc.cpu.gpr(r)
    }

    #[test]
    fn addi_and_addis() {
        let mut gc = harness(&[
            d_form(14, 1, 0, 5),       // addi r1, 0, 5  (r0-base ⇒ 0)
            d_form(14, 2, 1, 0xFFFB),  // addi r2, r1, -5
            d_form(15, 3, 0, 0x1234),  // addis r3, 0, 0x1234 ⇒ 0x12340000
        ]);
        run(&mut gc, 3);
        assert_eq!(get(&gc, 1), 5);
        assert_eq!(get(&gc, 2), 0);
        assert_eq!(get(&gc, 3), 0x1234_0000);
    }

    #[test]
    fn addi_ra_is_zero_only_as_base() {
        // addi with rA=r1 (nonzero reg) adds the register; rA=0 means literal 0.
        let mut gc = harness(&[
            d_form(14, 1, 0, 0x100), // r1 = 0x100
            d_form(14, 2, 1, 0x1),   // r2 = r1 + 1 = 0x101
        ]);
        run(&mut gc, 2);
        assert_eq!(get(&gc, 2), 0x101);
    }

    #[test]
    fn add_and_subf() {
        let mut gc = harness(&[
            d_form(14, 1, 0, 20),                 // r1 = 20
            d_form(14, 2, 0, 7),                  // r2 = 7
            xo_form(31, 3, 1, 2, 0, 266, 0),      // add  r3 = r1 + r2 = 27
            xo_form(31, 4, 2, 1, 0, 40, 0),       // subf r4 = r1 - r2 = 13
        ]);
        run(&mut gc, 4);
        assert_eq!(get(&gc, 3), 27);
        assert_eq!(get(&gc, 4), 13);
    }

    #[test]
    fn add_record_form_sets_cr0() {
        let mut gc = harness(&[
            d_form(14, 1, 0, 0xFFFF),          // r1 = -1 (sign-extended)
            d_form(14, 2, 0, 1),               // r2 = 1
            xo_form(31, 3, 1, 2, 0, 266, 1),   // add. r3 = 0 ⇒ CR0 = EQ
        ]);
        run(&mut gc, 3);
        assert_eq!(get(&gc, 3), 0);
        assert_eq!(gc.cpu.cr_field(0), super::super::state::CR_EQ);
    }

    #[test]
    fn logical_or_ori_oris_and_andi() {
        let mut gc = harness(&[
            d_form(15, 1, 0, 0xABCD),          // addis r1 = 0xABCD0000
            d_form(24, 1, 1, 0x1234),          // ori   r1 |= 0x1234 ⇒ 0xABCD1234
            d_form(28, 1, 2, 0x00FF),          // andi. r2 = r1 & 0xFF = 0x34
            d_form(25, 1, 3, 0x0001),          // oris  r3 = r1 | 0x00010000 (rS=r1, rA=r3)
        ]);
        run(&mut gc, 4);
        assert_eq!(get(&gc, 1), 0xABCD_1234);
        assert_eq!(get(&gc, 2), 0x34);
        assert_eq!(get(&gc, 3), 0xABCD_1234 | 0x0001_0000);
        // andi. always records CR0 — result 0x34 > 0 ⇒ GT.
        assert_eq!(gc.cpu.cr_field(0), super::super::state::CR_GT);
    }

    #[test]
    fn or_is_mr_when_rs_eq_rb() {
        let mut gc = harness(&[
            d_form(14, 5, 0, 0x77),       // r5 = 0x77
            x_form(31, 5, 6, 5, 444, 0),  // or r6, r5, r5  (== mr r6, r5)
        ]);
        run(&mut gc, 2);
        assert_eq!(get(&gc, 6), 0x77);
    }

    #[test]
    fn cmp_and_cmpi_set_cr_fields() {
        let mut gc = harness(&[
            d_form(14, 1, 0, 5),                       // r1 = 5
            d_form(14, 2, 0, 9),                       // r2 = 9
            xo_form(31, 0, 1, 2, 0, 0, 0),             // cmp cr0, r1, r2 ⇒ LT
            d_form(11, 0, 1, 5) | (1 << 23),           // cmpi cr1, r1, 5 ⇒ EQ
        ]);
        run(&mut gc, 4);
        assert_eq!(gc.cpu.cr_field(0), super::super::state::CR_LT);
        assert_eq!(gc.cpu.cr_field(1), super::super::state::CR_EQ);
    }

    #[test]
    fn rlwinm_extracts_bitfield() {
        // rlwinm r2, r1, 0, 24, 31  isolates the low byte (mask 0xFF).
        let mut gc = harness(&[
            d_form(15, 1, 0, 0x1234),          // r1 = 0x12340000
            d_form(24, 1, 1, 0x56AB),          // r1 |= 0x56AB ⇒ 0x123456AB
            m_form(21, 1, 2, 0, 24, 31, 0),    // rlwinm r2, r1, 0, 24,31 ⇒ 0xAB
        ]);
        run(&mut gc, 3);
        assert_eq!(get(&gc, 2), 0xAB);
    }

    #[test]
    fn rlwinm_rotate_and_mask() {
        // rlwinm r2, r1, 8, 0, 31  is a plain rotate-left-8 (full mask).
        let mut gc = harness(&[
            d_form(15, 1, 0, 0x1234),          // r1 = 0x12340000
            d_form(24, 1, 1, 0x5678),          // r1 = 0x12345678
            m_form(21, 1, 2, 8, 0, 31, 0),     // rotl 8
        ]);
        run(&mut gc, 3);
        assert_eq!(get(&gc, 2), 0x3456_7812);
    }

    #[test]
    fn lwz_and_stw_big_endian() {
        let mut gc = harness(&[
            d_form(15, 1, 0, 0xDEAD),          // r1 = 0xDEAD0000
            d_form(24, 1, 1, 0xBEEF),          // r1 = 0xDEADBEEF
            d_form(36, 1, 0, 0x200),           // stw  r1, 0x200(0)
            d_form(32, 2, 0, 0x200),           // lwz  r2, 0x200(0)
        ]);
        run(&mut gc, 4);
        assert_eq!(get(&gc, 2), 0xDEAD_BEEF);
        // Verify it was stored big-endian (MSB at the lowest RAM byte).
        assert_eq!(gc.mem.ram[0x200], 0xDE);
        assert_eq!(gc.mem.ram[0x203], 0xEF);
    }

    #[test]
    fn b_and_bl_branch_and_link() {
        // `bl +0x10`: opcode 18, LI=0x10, LK=1 ⇒ 0x4800_0010 | 1.
        let base = 0x8000_0000u32;
        let mut gc = harness(&[0x4800_0010 | 1]);
        run(&mut gc, 1);
        assert_eq!(gc.cpu.pc, base + 0x10, "bl jumps +0x10");
        assert_eq!(gc.cpu.lr, base + 4, "lr = return address");
    }

    #[test]
    fn blr_returns_via_lr() {
        let base = 0x8000_0000u32;
        let mut gc = harness(&[
            0x4E80_0020, // blr  (bclr 20,0 — branch always to LR)
        ]);
        gc.cpu.lr = base + 0x40;
        gc.cpu.pc = base;
        run(&mut gc, 1);
        assert_eq!(gc.cpu.pc, base + 0x40, "blr jumps to LR");
    }

    #[test]
    fn bc_conditional_branch_taken_and_not() {
        // Build CR0 = EQ via cmpi, then `beq` (bc 12,2) should branch.
        let base = 0x8000_0000u32;
        // bc BO=12 (branch if CR bit true), BI=2 (CR0 EQ bit), BD = +8.
        let beq = (16u32 << 26) | (12 << 21) | (2 << 16) | (8 & 0xFFFC);
        let mut gc = harness(&[
            d_form(14, 1, 0, 5),       // r1 = 5
            d_form(11, 0, 1, 5),       // cmpi cr0, r1, 5 ⇒ EQ
            beq,                       // beq +8 ⇒ skip next
            d_form(14, 2, 0, 0xAA),    // r2 = 0xAA (skipped)
            d_form(14, 3, 0, 0xBB),    // r3 = 0xBB (target)
        ]);
        gc.cpu.pc = base;
        run(&mut gc, 4); // addi, cmpi, beq(taken), then the target addi
        assert_eq!(get(&gc, 2), 0, "branch skipped r2");
        assert_eq!(get(&gc, 3), 0xBB, "landed at target");
    }

    #[test]
    fn mfspr_mtspr_lr_ctr_roundtrip() {
        let mut gc = harness(&[
            d_form(14, 1, 0, 0x1234),                  // r1 = 0x1234
            xo_form(31, 1, 0, 0, 0, 467, 0) | encode_spr(spr::SPR_LR), // mtspr LR, r1
            xo_form(31, 2, 0, 0, 0, 339, 0) | encode_spr(spr::SPR_LR), // mfspr r2, LR
        ]);
        run(&mut gc, 3);
        assert_eq!(gc.cpu.lr, 0x1234);
        assert_eq!(get(&gc, 2), 0x1234);
    }

    #[test]
    fn mfspr_pvr_is_gekko() {
        let mut gc = harness(&[
            xo_form(31, 5, 0, 0, 0, 339, 0) | encode_spr(spr::SPR_PVR), // mfspr r5, PVR
        ]);
        run(&mut gc, 1);
        assert_eq!(get(&gc, 5), spr::PVR_GEKKO);
    }

    /// Encode an SPR number into the split 10-bit field (inverse of Instr::spr).
    fn encode_spr(spr: u32) -> u32 {
        let lo = spr & 0x1F;
        let hi = (spr >> 5) & 0x1F;
        ((hi) | (lo << 5)) << 11
    }

    #[test]
    fn sc_raises_system_call() {
        let mut gc = harness(&[0x4400_0002]); // sc
        run(&mut gc, 1);
        assert_eq!(
            gc.cpu.pc,
            super::super::state::VECTOR_BASE_HIGH + Exception::SystemCall.offset()
        );
    }

    #[test]
    fn unimplemented_opcode_raises_program() {
        // Primary opcode 0 is illegal.
        let mut gc = harness(&[0x0000_0000]);
        run(&mut gc, 1);
        assert_eq!(
            gc.cpu.pc,
            super::super::state::VECTOR_BASE_HIGH + Exception::Program.offset()
        );
        assert_eq!(gc.cpu.spr.exceptions, 1);
    }

    #[test]
    fn ori_zero_is_nop() {
        // ori 0,0,0 — the canonical PowerPC nop. Decodes, executes, advances pc.
        let mut gc = harness(&[0x6000_0000]);
        let pc = gc.cpu.pc;
        run(&mut gc, 1);
        assert_eq!(gc.cpu.pc, pc + 4);
        assert_eq!(decode(Instr(0x6000_0000)), Decoded::Ori);
    }

    #[test]
    fn no_delay_slot_sequential_execution() {
        // Unlike MIPS, the instruction after a taken branch is NOT executed.
        let base = 0x8000_0000u32;
        let mut gc = harness(&[
            0x4800_0008,            // b +8 (skip the next)
            d_form(14, 1, 0, 0xAA), // addi r1 (must NOT run — no delay slot)
            d_form(14, 2, 0, 0xBB), // target
        ]);
        gc.cpu.pc = base;
        run(&mut gc, 2);
        assert_eq!(get(&gc, 1), 0, "no delay slot: skipped instruction did not run");
        assert_eq!(get(&gc, 2), 0xBB);
    }

    #[test]
    fn mask_mb_me_contiguous_and_wrapping() {
        assert_eq!(mask_mb_me(24, 31), 0x0000_00FF); // low byte
        assert_eq!(mask_mb_me(0, 31), 0xFFFF_FFFF); // full word
        assert_eq!(mask_mb_me(0, 7), 0xFF00_0000); // high byte
        // wrapping: MB=28, ME=3 ⇒ low nibble + high nibble.
        assert_eq!(mask_mb_me(28, 3), 0xF000_000F);
    }

    #[test]
    fn countdown_loop_with_bc_and_ctr() {
        // Sum 1..=5 into r2 using a CTR-driven loop (bdnz).
        //   r1=5; r2=0; mtctr r1
        // loop: add r2,r2,r1 ; addi r1,r1,-1 ; bdnz loop
        let base = 0x8000_0000u32;
        // bc BO=16 (decrement CTR, branch if CTR!=0), BI=0, BD=-8 (back to add).
        let bdnz = (16u32 << 26) | (16 << 21) | (0xFFF8 & 0xFFFC);
        let mut gc = harness(&[
            d_form(14, 1, 0, 5),                                       // r1 = 5
            d_form(14, 2, 0, 0),                                       // r2 = 0
            xo_form(31, 1, 0, 0, 0, 467, 0) | super::tests::ctr_spr(), // mtctr r1
            xo_form(31, 2, 2, 1, 0, 266, 0),                          // add r2,r2,r1
            d_form(14, 1, 1, 0xFFFF),                                 // addi r1,r1,-1
            bdnz,                                                     // bdnz loop (-8 ⇒ back to add)
        ]);
        gc.cpu.pc = base;
        run(&mut gc, 40);
        assert_eq!(get(&gc, 2), 15, "1+2+3+4+5");
        assert_eq!(gc.cpu.ctr, 0);
    }

    /// mtctr helper: the split SPR field for CTR.
    fn ctr_spr() -> u32 {
        let lo = spr::SPR_CTR & 0x1F;
        let hi = (spr::SPR_CTR >> 5) & 0x1F;
        ((hi) | (lo << 5)) << 11
    }
}
