//! MIPS R3000A register state — GPRs, HI/LO, the branch-delay PC pair, and
//! the load-delay slot. Built from psx-spx "CPU Specifications".
//!
//! Two quirks of the architecture are modelled here and are the whole reason
//! this struct is more than a flat register array:
//!
//! * **Branch delay slot.** A jump/branch takes effect *after* the following
//!   instruction. We track this with two program counters: `pc` (the
//!   instruction being executed) and `next_pc` (the one to fetch next). A
//!   branch sets `next_pc` to the target; the instruction in the slot still
//!   runs from the old `pc`+4 because that value was already latched into
//!   `next_pc` before the branch redirected it.
//!
//! * **Load delay slot.** The result of a load (`LW`, `LH`, …) is not visible
//!   to the instruction immediately following it — the destination register
//!   updates one instruction later. We model the pending write as a
//!   [`LoadSlot`] that is committed at the top of the *next* instruction,
//!   unless that instruction overwrites the same register first.
//!
//! Register r0 is hardwired to zero: writes are discarded and reads always
//! return 0. We enforce this in [`Cpu::set_reg`].

use super::cop0::Cop0;

/// A pending load-delay write: register `reg` becomes `value` after the next
/// instruction's slot-commit. `reg == 0` means "no pending load" (writes to r0
/// are no-ops, so this sentinel is free).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadSlot {
    pub reg: u32,
    pub value: u32,
}

/// MIPS R3000A architectural register state.
pub struct Cpu {
    /// General-purpose registers r0..r31. r0 is hardwired to 0 (see
    /// [`Cpu::reg`] / [`Cpu::set_reg`]); the slot is kept for a uniform index.
    pub regs: [u32; 32],

    /// Multiply/divide result registers.
    pub hi: u32,
    pub lo: u32,

    /// Address of the instruction currently executing.
    pub pc: u32,
    /// Address of the instruction to fetch next (branch-delay: a taken branch
    /// rewrites this, the delay-slot instruction still runs from the prior
    /// value).
    pub next_pc: u32,
    /// Address of the instruction in the *current* delay slot, captured at
    /// branch time so an exception in the slot can set CAUSE.BD / point EPC at
    /// the branch. Equivalently: the `pc` of the instruction before this one.
    pub current_pc: u32,

    /// True when the instruction now executing sits in a branch delay slot
    /// (the previous instruction was a taken branch/jump). Drives CAUSE.BD.
    pub in_delay_slot: bool,
    /// Set by a branch/jump while executing; folded into `in_delay_slot` for
    /// the following instruction.
    pub branch_taken: bool,

    /// Pending load-delay write, committed before the next instruction unless
    /// shadowed. `reg == 0` ⇒ empty.
    pub load: LoadSlot,

    /// System-control coprocessor (exceptions, status, cause).
    pub cop0: Cop0,

    /// Pending hardware-interrupt line (mirrors I_STAT & I_MASK); the CPU
    /// samples it between instructions. Wired by the IRQ subsystem later.
    pub irq_pending: bool,
}

/// The R3000A reset vector: execution begins in the BIOS (KSEG1, uncached).
pub const RESET_VECTOR: u32 = 0xBFC0_0000;

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
            pc: RESET_VECTOR,
            next_pc: RESET_VECTOR.wrapping_add(4),
            current_pc: RESET_VECTOR,
            in_delay_slot: false,
            branch_taken: false,
            load: LoadSlot::default(),
            cop0: Cop0::new(),
            irq_pending: false,
        }
    }

    /// Read a GPR. r0 always reads as 0.
    #[inline]
    pub fn reg(&self, index: u32) -> u32 {
        // r0 is hardwired to 0; we keep regs[0] == 0 invariant in set_reg, so a
        // plain index is correct, but the explicit guard documents the rule and
        // is robust even if regs[0] is ever transiently dirtied.
        if index == 0 {
            0
        } else {
            self.regs[(index & 0x1F) as usize]
        }
    }

    /// Write a GPR. Writes to r0 are discarded (it stays 0).
    #[inline]
    pub fn set_reg(&mut self, index: u32, value: u32) {
        let i = (index & 0x1F) as usize;
        self.regs[i] = value;
        self.regs[0] = 0; // keep r0 == 0 even after an r0-targeted write
    }

    /// Queue a load-delay write to `reg` (committed after the next
    /// instruction). A pending load targeting r0 is a no-op.
    #[inline]
    pub fn queue_load(&mut self, reg: u32, value: u32) {
        self.load = LoadSlot {
            reg: reg & 0x1F,
            value,
        };
    }

    /// Commit any pending load-delay write into the register file. Called at
    /// the start of each instruction *before* decoding it; the executing
    /// instruction may then overwrite the same register, which is why the
    /// commit happens first. Clears the slot.
    #[inline]
    pub fn commit_load(&mut self) {
        let slot = self.load;
        self.load = LoadSlot::default();
        if slot.reg != 0 {
            self.regs[slot.reg as usize] = slot.value;
            self.regs[0] = 0;
        }
    }

    /// Cancel any pending load-delay write whose destination is `reg` — used
    /// when an instruction writes a register that a just-issued load also
    /// targets (the instruction's write wins, the load is dropped). On the
    /// R3000A a load followed by an op writing the *same* register sees the
    /// op's value, not the load's.
    #[inline]
    pub fn shadow_load(&mut self, reg: u32) {
        if self.load.reg == (reg & 0x1F) {
            self.load = LoadSlot::default();
        }
    }

    /// True if the cache-isolation bit (SR.IsC) is set; stores then hit the
    /// cache rather than RAM. The bus consults this on writes.
    #[inline]
    pub fn cache_isolated(&self) -> bool {
        self.cop0.cache_isolated()
    }

    /// Raise an exception: delegate to COP0 for CAUSE/EPC/SR bookkeeping, then
    /// redirect the PC pair to the handler vector. The branch-delay state is
    /// reset because the handler starts a fresh (non-delay) instruction stream.
    pub fn raise_exception(&mut self, cause: super::cop0::Exception) {
        let vector = self
            .cop0
            .enter_exception(cause, self.current_pc, self.in_delay_slot);
        self.pc = vector;
        self.next_pc = vector.wrapping_add(4);
        self.in_delay_slot = false;
        self.branch_taken = false;
    }
}

#[cfg(test)]
mod tests {
    use super::super::cop0::{Exception, VECTOR_ROM};
    use super::*;

    #[test]
    fn r0_is_hardwired_zero() {
        let mut cpu = Cpu::new();
        cpu.set_reg(0, 0xDEAD_BEEF);
        assert_eq!(cpu.reg(0), 0);
        cpu.set_reg(5, 0x1234);
        assert_eq!(cpu.reg(5), 0x1234);
    }

    #[test]
    fn load_delay_commits_one_instruction_later() {
        let mut cpu = Cpu::new();
        cpu.queue_load(8, 0xAA);
        // The load is not yet visible.
        assert_eq!(cpu.reg(8), 0);
        // Commit at the start of the next instruction.
        cpu.commit_load();
        assert_eq!(cpu.reg(8), 0xAA);
    }

    #[test]
    fn shadowed_load_is_dropped() {
        let mut cpu = Cpu::new();
        cpu.queue_load(8, 0xAA);
        // An instruction writing r8 cancels the pending load to r8.
        cpu.shadow_load(8);
        cpu.commit_load();
        assert_eq!(cpu.reg(8), 0);
    }

    #[test]
    fn exception_vectors_to_bios_when_bev_set() {
        let mut cpu = Cpu::new();
        cpu.current_pc = 0x1234;
        cpu.raise_exception(Exception::Syscall);
        assert_eq!(cpu.pc, VECTOR_ROM);
        assert_eq!(cpu.cop0.epc, 0x1234);
        assert!(!cpu.in_delay_slot);
    }
}
