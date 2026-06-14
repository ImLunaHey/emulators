//! DS CPU executor: the per-step fetch/decode/dispatch loop, mode + banked-
//! register switching, CPSR/SPSR handling, exception entry (SWI/IRQ/FIQ), and
//! the per-core (ARM9 ARMv5TE / ARM7 ARMv4T) bus wiring.
//!
//! Adapted from the GBA core's tested `cpu.rs` (the DS ARM7 *is* the same
//! ARM7TDMI) plus the ARMv5 deltas from ../../ds-recomp/src/cpu/cpu.ts.
//!
//! ## Ownership (see CONTRACT.md)
//!
//! Unlike the GBA `Cpu`, this struct owns its own `CpuState` rather than
//! borrowing one out of the god-struct. There are two CPUs, each with an
//! independent register file + pipeline metadata, so each gets its own `Cpu`
//! instance carrying its `CpuState`. The `Nds.state9`/`state7` fields remain
//! the foundation's register-file owners; the orchestrator that wires the
//! frame loop decides whether to keep the live state inside `Cpu` (recommended:
//! `Cpu` is the single source of truth during a `step`) — either way the
//! executor never needs a simultaneous `&mut CpuState` *inside* `&mut Nds`,
//! which keeps the borrow checker happy when an instruction touches the bus.
//!
//! Memory goes through the `Nds` per-core accessors selected by `self.core`:
//! `read{8,16,32}_arm9/arm7` and `write{8,16,32}_arm9/arm7`. The instruction
//! decoders call the `Cpu::read*`/`write*` shims below so they stay agnostic
//! of which core they're driving.

use crate::nds::{Core, Nds};
use crate::state::{mode, CpuState, FLAG_F, FLAG_I, FLAG_T};

/// The instruction-set generation a core implements. ARM7 is ARMv4T, ARM9 is
/// ARMv5TE — the executor matches on this to gate the v5 decode paths (BLX,
/// CLZ, saturating DSP, LDRD/STRD, MCR/MRC, PC-load interworking).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// ARM7TDMI — ARMv4T (no v5 extensions).
    V4T,
    /// ARM946E-S — ARMv5TE (BLX, CLZ, QADD-family, LDRD/STRD, CP15, …).
    V5TE,
}

impl Arch {
    /// The architecture each DS core implements.
    #[inline]
    pub fn of(core: Core) -> Arch {
        match core {
            Core::Arm9 => Arch::V5TE,
            Core::Arm7 => Arch::V4T,
        }
    }
    #[inline]
    pub fn is_v5(self) -> bool {
        matches!(self, Arch::V5TE)
    }
}

/// One ARM core's execution context: register file + pipeline/IRQ tracking.
/// Two are instantiated — one per DS CPU — each with its own `Core`/`Arch`.
pub struct Cpu {
    pub state: CpuState,
    /// Which DS core this drives — selects the bus accessor + memory map.
    pub core: Core,
    /// ARMv4T vs ARMv5TE — gates the v5-only decode paths.
    pub arch: Arch,

    pub cycles: u64,

    /// Pending IRQ line — IO sets this; the CPU samples it between
    /// instructions and (if CPSR.I is clear) enters the IRQ vector.
    pub irq_line: bool,
    /// Pending FIQ line — sampled like `irq_line` but gated by CPSR.F.
    pub fiq_line: bool,
    /// Halt-wake line: any enabled-and-pending IRQ ignoring IME/CPSR.I. Real
    /// hardware lifts halt as soon as an enabled IRQ arrives even if it
    /// won't be *taken* (SM64DS halts with IME=0 during IPCSYNC). The IO
    /// module sets this; default `false` keeps a halted CPU asleep.
    pub wake_line: bool,

    /// Set by `flush_pipeline()` to tell `step()` that the executed
    /// instruction redirected PC (branch/BX/LDR-PC/...). The interpreter has
    /// no real prefetch buffer, so this just suppresses the auto-advance.
    pub branched: bool,
}

impl Cpu {
    /// Build a core's executor. `Core::Arm9` ⇒ ARMv5TE, `Core::Arm7` ⇒ ARMv4T.
    pub fn new(core: Core) -> Self {
        Cpu {
            state: CpuState::new(),
            core,
            arch: Arch::of(core),
            cycles: 0,
            irq_line: false,
            fiq_line: false,
            wake_line: false,
            branched: false,
        }
    }

    #[inline]
    pub fn is_arm9(&self) -> bool {
        self.core == Core::Arm9
    }

    /// Reset to the post-BIOS state: SYS mode, ARM, with the supplied stacks
    /// and entry PC. Mirrors the GBA `reset` + the DS TS `Cpu.reset`.
    pub fn reset(&mut self, entry_pc: u32, sys_sp: u32, irq_sp: u32, svc_sp: u32) {
        self.state = CpuState::new();
        self.state.cpsr = mode::SVC | FLAG_I | FLAG_F;
        self.state.switch_mode(mode::SYS);
        self.state.r[13] = sys_sp;
        self.state.bank_r13[2] = irq_sp; // IRQ bank
        self.state.bank_r13[3] = svc_sp; // SVC bank
        self.state.r[15] = entry_pc;
        self.branched = false;
    }

    /// Mark that the just-executed instruction redirected PC. Suppresses the
    /// post-execute auto-advance so `step` re-fetches from the new `r[15]`.
    #[inline]
    pub fn flush_pipeline(&mut self) {
        self.branched = true;
    }

    // ─── Per-core bus shims ──────────────────────────────────────────────
    //
    // The decoders call these so they never name a specific core. Each
    // dispatches to the matching `Nds` little-endian accessor.

    #[inline]
    pub fn read8(&self, nds: &mut Nds, addr: u32) -> u32 {
        match self.core {
            Core::Arm9 => nds.read8_arm9(addr),
            Core::Arm7 => nds.read8_arm7(addr),
        }
    }
    #[inline]
    pub fn read16(&self, nds: &mut Nds, addr: u32) -> u32 {
        match self.core {
            Core::Arm9 => nds.read16_arm9(addr),
            Core::Arm7 => nds.read16_arm7(addr),
        }
    }
    #[inline]
    pub fn read32(&self, nds: &mut Nds, addr: u32) -> u32 {
        match self.core {
            Core::Arm9 => nds.read32_arm9(addr),
            Core::Arm7 => nds.read32_arm7(addr),
        }
    }
    #[inline]
    pub fn write8(&self, nds: &mut Nds, addr: u32, v: u32) {
        match self.core {
            Core::Arm9 => nds.write8_arm9(addr, v),
            Core::Arm7 => nds.write8_arm7(addr, v),
        }
    }
    #[inline]
    pub fn write16(&self, nds: &mut Nds, addr: u32, v: u32) {
        match self.core {
            Core::Arm9 => nds.write16_arm9(addr, v),
            Core::Arm7 => nds.write16_arm7(addr, v),
        }
    }
    #[inline]
    pub fn write32(&self, nds: &mut Nds, addr: u32, v: u32) {
        match self.core {
            Core::Arm9 => nds.write32_arm9(addr, v),
            Core::Arm7 => nds.write32_arm7(addr, v),
        }
    }

    // ─── Step ────────────────────────────────────────────────────────────
    //
    // Fetch from r[15] (= next decode address), raise r[15] to the
    // architectural visible PC (decode + prefetch_off) for execute, then
    // advance to the next decode unless the instruction flushed the pipeline.

    /// Execute one instruction (or one halted/IRQ-entry tick). Returns the
    /// number of cycles consumed (always 1 for the interpreter).
    pub fn step(&mut self, nds: &mut Nds) -> u32 {
        if self.state.halted {
            // Lift halt on any enabled-and-pending IRQ, regardless of IME or
            // CPSR.I (the IRQ may still not be *taken* below). The IO module
            // drives `wake_line`; `irq_line` then gates the actual vector.
            if self.wake_line || (self.irq_line && (self.state.cpsr & FLAG_I) == 0) {
                self.state.halted = false;
            }
            self.cycles += 1;
            return 1;
        }

        // FIQ has higher priority than IRQ; both are sampled before fetch.
        if self.fiq_line && (self.state.cpsr & FLAG_F) == 0 {
            self.take_fiq();
        } else if self.irq_line && (self.state.cpsr & FLAG_I) == 0 {
            self.take_irq();
        }

        let is_thumb = (self.state.cpsr & FLAG_T) != 0;
        let insn_size: u32 = if is_thumb { 2 } else { 4 };
        let prefetch_off: u32 = if is_thumb { 4 } else { 8 };
        let decode = self.state.r[15] & (if is_thumb { !1 } else { !3 });
        let instr = if is_thumb {
            self.read16(nds, decode)
        } else {
            self.read32(nds, decode)
        };
        self.state.r[15] = decode.wrapping_add(prefetch_off);
        self.branched = false;

        if is_thumb {
            crate::cpu::thumb::thumb_execute(self, nds, instr);
        } else {
            crate::cpu::arm::arm_execute(self, nds, instr);
        }

        // Auto-advance to the next decode if execute didn't flush the pipeline.
        if !self.branched {
            self.state.r[15] = decode.wrapping_add(insn_size);
        }
        self.cycles += 1;
        1
    }

    // ─── Exceptions ──────────────────────────────────────────────────────

    /// SWI seam consulted by the instruction decoders. BIOS HLE
    /// (`Nds::bios_swi`) gets first refusal: if it handles the call, the SWI is
    /// fully serviced in HLE and we skip the architectural vector. Otherwise we
    /// fall through to the real `software_interrupt` exception entry.
    pub fn swi(&mut self, nds: &mut Nds, comment: u32) {
        // Keep `Nds.state*` in sync with the live `Cpu` state for the duration
        // of the HLE call (the BIOS reads/writes r0..r3 + memory through `Nds`).
        let is_arm9 = self.is_arm9();
        let saved = std::mem::replace(
            if is_arm9 {
                &mut nds.state9
            } else {
                &mut nds.state7
            },
            std::mem::replace(&mut self.state, CpuState::new()),
        );
        let handled = nds.bios_swi(is_arm9, comment);
        // Restore the (possibly BIOS-mutated) state back into the live `Cpu`,
        // and put the foundation owner's slot back.
        self.state = std::mem::replace(
            if is_arm9 {
                &mut nds.state9
            } else {
                &mut nds.state7
            },
            saved,
        );
        if !handled {
            self.software_interrupt(comment);
        }
    }

    /// SWI: enter SVC at the software-interrupt vector. BIOS HLE (`swi` above)
    /// gets first refusal before this real-exception fallback; the executor
    /// itself always takes the architectural path here.
    pub fn software_interrupt(&mut self, _comment: u32) {
        let vector = self.exc_vector(0x08);
        let s = &mut self.state;
        let in_thumb = (s.cpsr & FLAG_T) != 0;
        let ret = if in_thumb {
            s.r[15].wrapping_sub(2)
        } else {
            s.r[15].wrapping_sub(4)
        };
        s.enter_exception(mode::SVC, vector, ret, false);
        self.flush_pipeline();
    }

    /// Undefined-instruction trap: enter UND at vector 0x04.
    pub fn undefined_instruction(&mut self) {
        let vector = self.exc_vector(0x04);
        let s = &mut self.state;
        let in_thumb = (s.cpsr & FLAG_T) != 0;
        let ret = if in_thumb {
            s.r[15].wrapping_sub(2)
        } else {
            s.r[15].wrapping_sub(4)
        };
        s.enter_exception(mode::UND, vector, ret, false);
        self.flush_pipeline();
    }

    /// Take an IRQ. r[15] at entry is the next decode address; the BIOS
    /// returns via `SUBS PC, LR, #4`, so LR = next_decode + 4 lands PC back
    /// at next_decode after restore.
    pub fn take_irq(&mut self) {
        let vector = self.exc_vector(0x18);
        let ret = self.state.r[15].wrapping_add(4);
        self.state.enter_exception(mode::IRQ, vector, ret, false);
        self.flush_pipeline();
    }

    /// Take an FIQ — like IRQ but enters FIQ mode (which also banks R8..R12)
    /// and additionally masks F.
    pub fn take_fiq(&mut self) {
        let vector = self.exc_vector(0x1C);
        let ret = self.state.r[15].wrapping_add(4);
        self.state.enter_exception(mode::FIQ, vector, ret, true);
        self.flush_pipeline();
    }

    /// The exception-vector address for `offset`. The ARM9 runs with high
    /// vectors (CP15 control bit 13) at 0xFFFF0000 on the DS; the ARM7 uses
    /// the low vectors at 0x00000000. We model the ARM9 as high-vector
    /// (matching the DS BIOS) and the ARM7 as low-vector.
    #[inline]
    fn exc_vector(&self, offset: u32) -> u32 {
        match self.core {
            Core::Arm9 => 0xFFFF_0000u32.wrapping_add(offset),
            Core::Arm7 => offset,
        }
    }

    /// HALT (CP15 WFI on ARM9 / HALTCNT on ARM7 — driven by IO/CP15).
    pub fn halt(&mut self) {
        self.state.halted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm9() -> (Cpu, Nds) {
        (Cpu::new(Core::Arm9), Nds::new())
    }
    fn arm7() -> (Cpu, Nds) {
        (Cpu::new(Core::Arm7), Nds::new())
    }

    // Load a small ARM program into ARM9 main RAM and set up SYS mode.
    fn load_arm9(insns: &[u32]) -> (Cpu, Nds) {
        let (mut cpu, mut nds) = arm9();
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0000;
        cpu.state.r[13] = 0x0200_8000;
        cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            nds.write32_arm9(0x0200_0000 + (i as u32) * 4, insn);
        }
        (cpu, nds)
    }

    #[test]
    fn arch_mapping() {
        assert_eq!(Arch::of(Core::Arm9), Arch::V5TE);
        assert_eq!(Arch::of(Core::Arm7), Arch::V4T);
        assert!(Arch::of(Core::Arm9).is_v5());
        assert!(!Arch::of(Core::Arm7).is_v5());
    }

    #[test]
    fn step_executes_add() {
        // ADD R0, R1, R2
        let (mut cpu, mut nds) = load_arm9(&[0xE081_0002]);
        cpu.state.r[1] = 5;
        cpu.state.r[2] = 3;
        cpu.step(&mut nds);
        assert_eq!(cpu.state.r[0], 8);
        assert_eq!(cpu.state.r[15], 0x0200_0004);
    }

    #[test]
    fn irq_entry_arm9_high_vector() {
        let (mut cpu, _nds) = arm9();
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0100;
        cpu.take_irq();
        assert_eq!(cpu.state.mode(), mode::IRQ);
        assert_eq!(cpu.state.r[14], 0x0200_0104); // saved PC + 4
        assert_eq!(cpu.state.r[15], 0xFFFF_0018); // ARM9 high IRQ vector
        assert_eq!(cpu.state.cpsr & FLAG_T, 0);
        assert_eq!(cpu.state.cpsr & FLAG_I, FLAG_I);
    }

    #[test]
    fn irq_entry_arm7_low_vector() {
        let (mut cpu, _nds) = arm7();
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0100;
        cpu.take_irq();
        assert_eq!(cpu.state.r[15], 0x18); // ARM7 low IRQ vector
    }

    #[test]
    fn halted_wakes_on_irq() {
        let (mut cpu, mut nds) = arm9();
        cpu.state.halted = true;
        cpu.state.cpsr = mode::SYS; // I clear
        cpu.irq_line = true;
        cpu.step(&mut nds);
        assert!(!cpu.state.halted);
    }

    #[test]
    fn halted_stays_when_masked() {
        let (mut cpu, mut nds) = arm9();
        cpu.state.halted = true;
        cpu.state.cpsr = mode::SYS | FLAG_I; // masked
        cpu.irq_line = true;
        cpu.wake_line = false;
        cpu.step(&mut nds);
        assert!(cpu.state.halted);
    }
}
