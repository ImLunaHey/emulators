//! VR4300 (MIPS III) architectural register state — 64-bit GPRs, HI/LO, the
//! branch-delay PC pair, and the exception entry point.
//!
//! Built from the VR4300 user manual. The N64 CPU is a 64-bit MIPS III part,
//! so the GPRs and HI/LO are `u64`. We model the one structural quirk:
//!
//! * **Branch delay slot.** A jump/branch takes effect *after* the following
//!   instruction. Two program counters track this: `pc` (the instruction being
//!   executed) and `next_pc` (the one fetched next). A taken branch rewrites
//!   `next_pc`; the delay-slot instruction still runs from the value already
//!   latched.
//!
//! Unlike the R3000A, MIPS III has **no architectural load-delay slot** (the
//! pipeline interlocks), so a load's result is visible to the very next
//! instruction. We therefore do not model a load slot — loads write the GPR
//! immediately.
//!
//! Register r0 is hardwired to zero (enforced in [`Cpu::set_reg`]).

use super::cop0::{Cop0, Exception};
use super::cop1::Cop1;

/// The VR4300 reset/NMI vector — execution begins in the PIF boot ROM
/// (KSEG1, uncached).
pub const RESET_VECTOR: u64 = 0xFFFF_FFFF_BFC0_0000;

/// VR4300 architectural register state.
pub struct Cpu {
    /// General-purpose registers r0..r31, 64-bit. r0 is hardwired to 0.
    pub regs: [u64; 32],

    /// Multiply/divide result registers (64-bit on MIPS III).
    pub hi: u64,
    pub lo: u64,

    /// Load-linked bit (for LL/SC). True between an LL and a matching SC.
    pub ll_bit: bool,

    /// Address of the instruction currently executing.
    pub pc: u64,
    /// Address of the instruction to fetch next. A taken branch rewrites this;
    /// the delay-slot instruction still runs from the prior value.
    pub next_pc: u64,
    /// Address captured at branch time so an exception in the delay slot can
    /// set Cause.BD and point EPC at the branch.
    pub current_pc: u64,

    /// True when the instruction now executing sits in a branch delay slot.
    pub in_delay_slot: bool,
    /// Set by a branch/jump while executing; folded into `in_delay_slot` for
    /// the next instruction.
    pub branch_taken: bool,

    /// System-control coprocessor (exceptions, status, cause, TLB, timer).
    pub cop0: Cop0,
    /// Floating-point coprocessor.
    pub cop1: Cop1,

    /// Set when an instruction raised an exception this step; the step loop
    /// then skips its normal PC advance (the exception already redirected PC).
    pub exception_pending: bool,

    /// Cycle counter — drives the Count/Compare timer. Not architectural.
    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            ll_bit: false,
            pc: RESET_VECTOR,
            next_pc: RESET_VECTOR.wrapping_add(4),
            current_pc: RESET_VECTOR,
            in_delay_slot: false,
            branch_taken: false,
            cop0: Cop0::new(),
            cop1: Cop1::new(),
            exception_pending: false,
            cycles: 0,
        }
    }

    /// Read a GPR (full 64-bit). r0 always reads as 0.
    #[inline]
    pub fn reg(&self, index: u32) -> u64 {
        let i = (index & 0x1F) as usize;
        if i == 0 {
            0
        } else {
            self.regs[i]
        }
    }

    /// Read the low 32 bits of a GPR.
    #[inline]
    pub fn reg32(&self, index: u32) -> u32 {
        self.reg(index) as u32
    }

    /// Write a GPR (full 64-bit). Writes to r0 are discarded.
    #[inline]
    pub fn set_reg(&mut self, index: u32, value: u64) {
        let i = (index & 0x1F) as usize;
        if i != 0 {
            self.regs[i] = value;
        }
    }

    /// Write a GPR from a 32-bit value, sign-extended to 64 bits — the MIPS III
    /// convention for all 32-bit ALU results (ADDU/SLL/LW/…). This keeps the
    /// upper half a correct sign extension so a later 64-bit op sees the right
    /// value.
    #[inline]
    pub fn set_reg32(&mut self, index: u32, value: u32) {
        self.set_reg(index, value as i32 as i64 as u64);
    }

    /// Raise an exception: COP0 does the Cause/EPC/Status bookkeeping and
    /// returns the handler vector; we redirect the PC pair there. `coprocessor`
    /// is the CU number for a CoprocessorUnusable exception (0 otherwise).
    pub fn raise(&mut self, cause: Exception, coprocessor: u32) {
        let vector = self.cop0.enter_exception(
            cause,
            self.current_pc,
            self.in_delay_slot,
            coprocessor,
        );
        // Vectors are physical KSEG addresses; sign-extend to the 64-bit PC.
        let v = vector as i32 as i64 as u64;
        self.pc = v;
        self.next_pc = v.wrapping_add(4);
        self.in_delay_slot = false;
        self.branch_taken = false;
        self.exception_pending = true;
    }

    /// Set the bad virtual address and raise an address-error exception.
    pub fn raise_address_error(&mut self, vaddr: u64, store: bool) {
        self.cop0.reg[super::cop0::R_BADVADDR] = vaddr;
        let cause = if store {
            Exception::AddressErrorStore
        } else {
            Exception::AddressErrorLoad
        };
        self.raise(cause, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::cop0::{VEC_GENERAL_BEV1, ST_BEV};

    #[test]
    fn r0_is_hardwired_zero() {
        let mut cpu = Cpu::new();
        cpu.set_reg(0, 0xDEAD_BEEF_CAFE_BABE);
        assert_eq!(cpu.reg(0), 0);
        cpu.set_reg(5, 0x1234_5678_9ABC_DEF0);
        assert_eq!(cpu.reg(5), 0x1234_5678_9ABC_DEF0);
    }

    #[test]
    fn set_reg32_sign_extends() {
        let mut cpu = Cpu::new();
        cpu.set_reg32(5, 0x8000_0000);
        assert_eq!(cpu.reg(5), 0xFFFF_FFFF_8000_0000);
        cpu.set_reg32(6, 0x0000_0001);
        assert_eq!(cpu.reg(6), 0x0000_0000_0000_0001);
    }

    #[test]
    fn reset_vector_is_pif_rom() {
        let cpu = Cpu::new();
        assert_eq!(cpu.pc, RESET_VECTOR);
        assert!(cpu.cop0.status() & ST_BEV != 0);
    }

    #[test]
    fn raise_redirects_to_handler_vector() {
        let mut cpu = Cpu::new();
        cpu.current_pc = 0xFFFF_FFFF_8000_1000;
        cpu.raise(Exception::Syscall, 0);
        // BEV is set at reset -> ROM general vector, sign-extended.
        assert_eq!(cpu.pc, VEC_GENERAL_BEV1 as i32 as i64 as u64);
        assert_eq!(cpu.cop0.epc(), 0xFFFF_FFFF_8000_1000);
    }
}
