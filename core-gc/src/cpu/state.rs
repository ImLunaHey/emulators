//! Gekko architectural register state — 32 GPRs, 32 FPRs, the branch unit
//! registers (PC, LR, CTR), the condition register (CR), XER, MSR, and the
//! special-purpose register file ([`super::spr::Spr`]).
//!
//! Built from the PowerPC User ISA + OEA and YAGCD §2.2. Unlike the MIPS R3000A
//! in the PS1 core, PowerPC has **no branch delay slot and no load delay slot**,
//! so the register file is a flat array — the complexity of the PS1's `Cpu` is
//! absent. Notable PowerPC facts modelled here:
//!
//! * **r0 is a real register** — it is NOT hardwired to zero. The architecture
//!   only treats r0 specially as the *base operand* of a handful of
//!   address-form instructions (where "r0" in the rA slot means literal 0); the
//!   register file itself can hold any value in r0. (Contrast the MIPS r0.)
//! * **Big-endian, 32-bit instructions.** No delay slots; `pc` advances by 4
//!   each step unless a branch rewrites it.
//! * **FPRs are 64-bit.** The Gekko's paired-single mode views them as two
//!   f32 lanes; we store the raw bits as `u64` (no FP execution yet).
//! * **CR is eight 4-bit fields** (CR0..CR7). Comparisons and record-form (`.`)
//!   instructions set a field to LT/GT/EQ/SO. CR0 is the implicit target of
//!   record-form integer ops.

use super::spr::Spr;

/// Gekko architectural register state.
pub struct Cpu {
    /// General-purpose registers r0..r31 (32-bit). r0 is a normal register
    /// (NOT hardwired to 0; see the rA-operand note in the module docs).
    pub gpr: [u32; 32],

    /// Floating-point registers f0..f31, stored as raw 64-bit bit patterns.
    /// FP/paired-single execution is future work; these exist so `mfspr`-style
    /// FPR moves and `lfd`/`stfd` have a home.
    pub fpr: [u64; 32],

    /// Program counter — address of the instruction currently executing. PowerPC
    /// has no delay slot, so this advances by 4 unless a branch rewrites it.
    pub pc: u32,
    /// Link Register (SPR 8) — subroutine return address; written by the `lk`
    /// form of branches (`bl`, `bcl`) and `blr` reads it.
    pub lr: u32,
    /// Count Register (SPR 9) — loop counter / indirect-branch target (`bctr`).
    pub ctr: u32,

    /// Condition Register — eight 4-bit fields CR0..CR7 packed into a u32
    /// (CR0 in the most-significant nibble, per PowerPC big-endian bit numbering).
    pub cr: u32,

    /// Fixed-point exception register (SPR 1) — carry (CA), overflow (OV) and
    /// summary-overflow (SO) bits plus the string byte-count.
    pub xer: u32,

    /// Supervisor/control SPRs + the exception model (MSR, SRR0/1, DEC, …).
    pub spr: Spr,

    /// Pending external-interrupt line (the PI folds every device IRQ into one
    /// line into the Gekko). Sampled between instructions when MSR[EE] is set.
    /// Wired by the interrupt subsystem later.
    pub irq_pending: bool,
}

/// The Gekko reset vector. At power-on MSR[IP] selects the high vector base
/// `0xFFF0_0000`; the system-reset handler (offset 0x100) is the first code the
/// IPL runs. YAGCD §2.2 / PowerPC OEA.
pub const RESET_VECTOR: u32 = 0xFFF0_0100;

/// The high exception-vector base (MSR[IP]=1). Exception offset added to this.
pub const VECTOR_BASE_HIGH: u32 = 0xFFF0_0000;
/// The low exception-vector base (MSR[IP]=0).
pub const VECTOR_BASE_LOW: u32 = 0x0000_0000;

// ---- Condition Register field bit positions (within a 4-bit CR field, in
//      PowerPC order: bit 0 = LT (MSB of the nibble), 1 = GT, 2 = EQ, 3 = SO).
pub const CR_LT: u32 = 0b1000;
pub const CR_GT: u32 = 0b0100;
pub const CR_EQ: u32 = 0b0010;
pub const CR_SO: u32 = 0b0001;

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            gpr: [0; 32],
            fpr: [0; 32],
            pc: RESET_VECTOR,
            lr: 0,
            ctr: 0,
            cr: 0,
            xer: 0,
            spr: Spr::new(),
            irq_pending: false,
        }
    }

    /// Read a GPR. (All 32 are ordinary registers; r0 is not special here.)
    #[inline]
    pub fn gpr(&self, index: u32) -> u32 {
        self.gpr[(index & 0x1F) as usize]
    }

    /// Write a GPR.
    #[inline]
    pub fn set_gpr(&mut self, index: u32, value: u32) {
        self.gpr[(index & 0x1F) as usize] = value;
    }

    /// The rA operand of address-form instructions: register `index`, except
    /// that `index == 0` means the literal value 0 (the PowerPC "rA|0" rule).
    #[inline]
    pub fn ra_or_zero(&self, index: u32) -> u32 {
        if index == 0 {
            0
        } else {
            self.gpr(index)
        }
    }

    /// Read a 4-bit CR field (`field` in 0..8; CR0 is the most-significant).
    #[inline]
    pub fn cr_field(&self, field: u32) -> u32 {
        let shift = 28 - (field & 7) * 4;
        (self.cr >> shift) & 0xF
    }

    /// Write a 4-bit CR field.
    #[inline]
    pub fn set_cr_field(&mut self, field: u32, value: u32) {
        let shift = 28 - (field & 7) * 4;
        self.cr = (self.cr & !(0xF << shift)) | ((value & 0xF) << shift);
    }

    /// Set CR0 from a signed comparison of `result` against zero, folding in the
    /// current XER[SO] (PowerPC record-form `.` semantics). Used by the `Rc`
    /// variants of the integer ops.
    #[inline]
    pub fn set_cr0(&mut self, result: u32) {
        let r = result as i32;
        let mut field = if r < 0 {
            CR_LT
        } else if r > 0 {
            CR_GT
        } else {
            CR_EQ
        };
        if self.xer & super::spr::XER_SO != 0 {
            field |= CR_SO;
        }
        self.set_cr_field(0, field);
    }

    /// Raise a PowerPC exception: snapshot PC→SRR0 and MSR→SRR1, clear the
    /// MSR bits the architecture clears on entry (EE/PR/translation), and
    /// redirect PC to the handler vector (high or low base per MSR[IP]; we use
    /// the high base, matching the GameCube's reset configuration).
    pub fn raise_exception(&mut self, cause: super::spr::Exception) {
        self.spr.exceptions = self.spr.exceptions.wrapping_add(1);
        self.spr.srr0 = self.pc;
        self.spr.srr1 = self.spr.msr;
        // On entry the processor clears EE (mask further interrupts), PR (enter
        // supervisor), and the translation bits — a simplified subset.
        self.spr.msr &= !(super::spr::MSR_EE
            | super::spr::MSR_PR
            | super::spr::MSR_IR
            | super::spr::MSR_DR);
        self.pc = VECTOR_BASE_HIGH.wrapping_add(cause.offset());
    }

    /// `rfi` (Return From Interrupt): restore MSR from SRR1 and PC from SRR0.
    pub fn return_from_interrupt(&mut self) {
        self.spr.msr = self.spr.srr1;
        self.pc = self.spr.srr0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpr_round_trip_and_r0_is_normal() {
        let mut cpu = Cpu::new();
        cpu.set_gpr(0, 0xDEAD_BEEF);
        assert_eq!(cpu.gpr(0), 0xDEAD_BEEF, "r0 is a normal register");
        // But as an rA base operand, r0 means literal 0.
        assert_eq!(cpu.ra_or_zero(0), 0);
        cpu.set_gpr(5, 0x1234);
        assert_eq!(cpu.ra_or_zero(5), 0x1234);
    }

    #[test]
    fn cr_field_pack_unpack() {
        let mut cpu = Cpu::new();
        cpu.set_cr_field(0, CR_GT);
        cpu.set_cr_field(7, CR_EQ);
        assert_eq!(cpu.cr_field(0), CR_GT);
        assert_eq!(cpu.cr_field(7), CR_EQ);
        // CR0 in the top nibble.
        assert_eq!(cpu.cr >> 28, CR_GT);
    }

    #[test]
    fn set_cr0_signed_compare() {
        let mut cpu = Cpu::new();
        cpu.set_cr0(0);
        assert_eq!(cpu.cr_field(0), CR_EQ);
        cpu.set_cr0(5);
        assert_eq!(cpu.cr_field(0), CR_GT);
        cpu.set_cr0(0xFFFF_FFFF); // -1
        assert_eq!(cpu.cr_field(0), CR_LT);
    }

    #[test]
    fn exception_snapshots_and_vectors() {
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000_1000;
        cpu.spr.msr = super::super::spr::MSR_EE;
        cpu.raise_exception(super::super::spr::Exception::SystemCall);
        assert_eq!(cpu.spr.srr0, 0x8000_1000);
        assert_eq!(cpu.spr.srr1 & super::super::spr::MSR_EE, super::super::spr::MSR_EE);
        assert_eq!(cpu.pc, VECTOR_BASE_HIGH + 0x0C00);
        assert_eq!(cpu.spr.msr & super::super::spr::MSR_EE, 0, "EE cleared on entry");
        cpu.return_from_interrupt();
        assert_eq!(cpu.pc, 0x8000_1000, "rfi restores PC");
        assert_ne!(cpu.spr.msr & super::super::spr::MSR_EE, 0, "rfi restores MSR");
    }
}
