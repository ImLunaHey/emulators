//! ARM7TDMI step driver, reset, BIOS-stub installation, exception entry,
//! and IRQ dispatch. Ported 1:1 from src/cpu/cpu.ts.
//!
//! Cpu groups state, pipeline tracking, exception vectors, and the per-step
//! dispatch. (The recompiler is dropped in the Rust port — we ship a pure
//! interpreter.)
//!
//! Unlike the TS `Cpu`, this struct does NOT store the bus. `step`,
//! `software_interrupt`, and `take_irq` take `bus: &mut dyn crate::bus::Bus`
//! as a parameter. `reset` takes `&mut crate::bus::Mem` because the BIOS
//! stub writes raw bytes into the BIOS region (the Bus write path ignores
//! region 0), so it writes directly into `mem.bios[..]`.

use crate::bus::{Bus, Mem};
use crate::state::{mode, CpuState, FLAG_F, FLAG_I, FLAG_T};

pub struct Cpu {
    pub state: CpuState,
    pub cycles: u64,
    // Pending IRQ line — IO sets this; CPU samples it between instructions.
    pub irq_line: bool,
    // Set by flush_pipeline() to tell step() that PC was redirected by the
    // executed instruction (branch, BX, LDR PC, etc.). Replaces the buggy
    // "compare r[15] to visible PC" heuristic: BX to addr (= decode + 8)
    // was indistinguishable from no-branch under that check.
    pub branched: bool,
    // Pipeline flush flag. The Rust interpreter re-fetches from r[15] every
    // step (no real prefetch buffer), so this is only used to detect that the
    // executed instruction redirected PC (set by flush_pipeline / cleared each
    // step) and to honor a post-savestate pipeline clear.
    prefetched_valid: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            state: CpuState::new(),
            cycles: 0,
            irq_line: false,
            branched: false,
            prefetched_valid: false,
        }
    }

    pub fn reset(&mut self, mem: &mut Mem) {
        self.state = CpuState::new();
        self.state.cpsr = mode::SVC | FLAG_F | FLAG_I;
        // Cartridge boot bypass: CPU starts in System mode with SP at the
        // canonical IWRAM stack, mirroring what the real BIOS post-init state
        // looks like to the game.
        self.state.switch_mode(mode::SYS);
        self.state.r[13] = 0x0300_7F00;
        self.state.bank_r13[2] = 0x0300_7FA0; // IRQ
        self.state.bank_r13[3] = 0x0300_7FE0; // SVC
        self.state.r[15] = 0x0800_0000;
        self.prefetched_valid = false;
        self.install_bios_stub(mem);
    }

    // Install a minimal BIOS stub:
    //   - 0x00: reset vector (cart bypass: just branches to ROM entry)
    //   - 0x18: IRQ vector → BIOS handler that calls user handler at
    //           [0x03007FFC] and returns with the canonical SUBS PC, LR, #4.
    // Without this, halted CPUs that get IRQ'd land in zero-filled BIOS and
    // wander off into open bus.
    fn install_bios_stub(&mut self, mem: &mut Mem) {
        let bios = &mut mem.bios;
        let wr32 = |bios: &mut [u8], off: usize, v: u32| {
            bios[off] = (v & 0xFF) as u8;
            bios[off + 1] = ((v >> 8) & 0xFF) as u8;
            bios[off + 2] = ((v >> 16) & 0xFF) as u8;
            bios[off + 3] = ((v >> 24) & 0xFF) as u8;
        };
        // 0x00 reset: branch to ROM entry via LDR PC, [PC, #0x140] (load from
        // literal in unused BIOS space). ARM B can't encode the ±32MB jump
        // from 0x0 to 0x08000000 directly.
        wr32(bios, 0x00, 0xE59F_F140); // LDR PC, [PC, #0x140] — addr = 0x00+8+0x140 = 0x148
        wr32(bios, 0x148, 0x0800_0000); // ROM entry literal
        // 0x04 undef
        wr32(bios, 0x04, 0xEAFF_FFFE);
        // 0x08 swi:   the CPU only lands here on SWI when HLE refuses. We
        //              emulate the BIOS SWI handler in HLE, so loop forever.
        wr32(bios, 0x08, 0xEAFF_FFFE);
        // 0x0C prefetch abort
        wr32(bios, 0x0C, 0xEAFF_FFFE);
        // 0x10 data abort
        wr32(bios, 0x10, 0xEAFF_FFFE);
        // 0x14 reserved
        wr32(bios, 0x14, 0xEAFF_FFFE);
        // 0x18 IRQ: B 0x128 — jump to the dispatcher below.
        // offset24 = (0x128 - (0x18 + 8)) / 4 = 0x108/4 = 0x42
        wr32(bios, 0x18, 0xEA00_0042);
        // 0x1C FIQ
        wr32(bios, 0x1C, 0xEAFF_FFFE);

        // IRQ dispatcher at 0x128 — calls user handler stored at 0x03007FFC.
        wr32(bios, 0x128, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
        wr32(bios, 0x12C, 0xE3A0_0301); // MOV R0, #0x4000000
        wr32(bios, 0x130, 0xE28F_E000); // ADR LR, 0x138
        wr32(bios, 0x134, 0xE510_F004); // LDR PC, [R0, #-4]    ; loads from 0x03FFFFFC
                                        // — actually we use 0x03007FFC; the standard
                                        //   trick is MOV R0,#0x4000000 + LDR [R0,#-4]
                                        //   reads 0x03FFFFFC, which is mirrored from
                                        //   0x03007FFC. We mirror IWRAM at the bus,
                                        //   so this works.
        wr32(bios, 0x138, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
        wr32(bios, 0x13C, 0xE25E_F004); // SUBS PC, LR, #4 (returns + restores CPSR)
    }

    pub fn flush_pipeline(&mut self) {
        self.prefetched_valid = false;
        self.branched = true;
    }

    /// Invalidate the prefetch slot so the next `step` re-fetches from r[15].
    /// Called after a savestate restore (the pipeline is part of the snapshot
    /// only via r[15]; the prefetch buffer must not survive a load).
    pub fn clear_prefetch(&mut self) {
        self.prefetched_valid = false;
    }

    // Single dispatch — fetch from r[15] (= next decode addr), temporarily
    // raise r[15] to the architectural visible PC for execute, then advance
    // to the next decode address if execute didn't branch.
    pub fn step(&mut self, bus: &mut dyn Bus) -> u32 {
        if self.state.halted {
            if self.irq_line && (self.state.cpsr & FLAG_I) == 0 {
                self.state.halted = false;
            }
            self.cycles += 1;
            return 1;
        }
        if self.irq_line && (self.state.cpsr & FLAG_I) == 0 {
            self.take_irq(bus);
        }

        let is_thumb = (self.state.cpsr & FLAG_T) != 0;
        let insn_size: u32 = if is_thumb { 2 } else { 4 };
        let prefetch_off: u32 = if is_thumb { 4 } else { 8 };
        let decode = self.state.r[15] & (if is_thumb { !1 } else { !3 });
        let instr = if is_thumb {
            bus.read16(decode)
        } else {
            bus.read32(decode)
        };
        // Track the last decode PC (one relaxed atomic store/instruction) so
        // the IWRAM write-watch (LinkPanel debug tool) can attribute a store
        // to the instruction that issued it.
        LAST_PC.store(decode, std::sync::atomic::Ordering::Relaxed);
        self.state.r[15] = decode.wrapping_add(prefetch_off);
        self.branched = false;

        if is_thumb {
            crate::thumb::thumb_execute(self, bus, instr);
        } else {
            crate::arm::arm_execute(self, bus, instr);
        }

        // Auto-advance to the next decode if execute didn't flush the pipeline.
        if !self.branched {
            self.state.r[15] = decode.wrapping_add(insn_size);
        }
        self.cycles += 1;
        1
    }

    // Trigger exception entry — called from arm/thumb dispatch.
    // NOTE(orchestrator): BIOS HLE SWI interception is applied by Gba before
    // this real-exception fallback.
    pub fn software_interrupt(&mut self, comment: u32, bus: &mut dyn Bus) {
        // BIOS HLE first: the orchestrator's `Gba` overrides `try_hle_swi` to
        // service the SWI in high-level emulation. If handled, no real
        // exception is taken (matches TS `if (bios.handleSwi(c)) return`).
        if bus.try_hle_swi(self, comment) {
            return;
        }
        let s = &mut self.state;
        let in_thumb = (s.cpsr & FLAG_T) != 0;
        let ret = if in_thumb {
            s.r[15].wrapping_sub(2)
        } else {
            s.r[15].wrapping_sub(4)
        };
        s.enter_exception(mode::SVC, 0x08, ret, false);
        self.flush_pipeline();
    }

    // Take an IRQ exception. r[15] at entry is the next decode address.
    // BIOS uses SUBS PC, LR, #4 to return, so LR = next_decode + 4 lands
    // PC back at next_decode after restore.
    pub fn take_irq(&mut self, bus: &mut dyn Bus) {
        let _ = bus;
        let ret = self.state.r[15].wrapping_add(4);
        self.state.enter_exception(mode::IRQ, 0x18, ret, false);
        self.flush_pipeline();
    }

    // Halt — handled by HALTCNT BIOS HLE.
    pub fn halt(&mut self) {
        self.state.halted = true;
    }
}

// Decode PC of the instruction currently executing (set every step). The
// IWRAM write-watch (LinkPanel debug tool) reads this to attribute a store to
// the instruction that issued it.
pub static LAST_PC: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(test)]
mod tests {
    //! CPU-level vectors ported from the (deleted) TypeScript `src/test/cpu.test.ts`:
    //! IRQ entry/return, mode switching + banking, halt+IRQ wakeup (boot-stall
    //! pattern), the BIOS IRQ dispatcher round-trip, IO-register behavior, and
    //! memory mirrors/rotations. Harness mirrors the TS `makeCpu()`/`setupRunArm`.

    use crate::bus::Bus;
    use crate::irq::IRQ_VBLANK;
    use crate::state::{mode, FLAG_I, FLAG_T};
    use crate::Gba;

    fn step(g: &mut Gba) {
        let mut cpu = std::mem::take(&mut g.cpu);
        cpu.step(g);
        g.cpu = cpu;
    }

    // SYS / ARM, code in IWRAM (mirrors cpu.test.ts setupRunArm + makeCpu).
    fn setup(insns: &[u32]) -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        g.cpu.state.cpsr = mode::SYS;
        g.cpu.state.r[15] = 0x0300_0000;
        g.cpu.state.r[13] = 0x0300_7F00;
        g.cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            Bus::write32(&mut g, 0x0300_0000 + (i as u32) * 4, insn);
        }
        g
    }

    fn take_irq(g: &mut Gba) {
        let mut cpu = std::mem::take(&mut g.cpu);
        cpu.take_irq(g);
        g.cpu = cpu;
    }

    // ---- IRQ entry and return ----

    #[test]
    fn take_irq_enters_irq_mode_with_lr() {
        let mut g = setup(&[0xE320F000]);
        g.cpu.state.r[15] = 0x08000100; // next decode
        g.cpu.state.cpsr = mode::SYS;
        take_irq(&mut g);
        assert_eq!(g.cpu.state.mode(), mode::IRQ);
        assert_eq!(g.cpu.state.r[14], 0x08000104); // saved PC + 4
        assert_eq!(g.cpu.state.r[15], 0x18); // IRQ vector
        assert_eq!(g.cpu.state.cpsr & FLAG_T, 0); // T cleared
        assert_eq!(g.cpu.state.cpsr & 0x80, 0x80); // I set
    }

    #[test]
    fn subs_pc_lr_restores_cpsr_from_spsr() {
        // Enter IRQ then execute SUBS PC, LR, #4 at 0x03000000.
        let mut g = setup(&[0xE25EF004]); // SUBS PC, LR, #4
        g.cpu.state.cpsr = mode::SYS | FLAG_T;
        g.cpu.state.r[15] = 0x08000100;
        take_irq(&mut g);
        // Redirect to our SUBS at IWRAM.
        Bus::write32(&mut g, 0x03000000, 0xE25EF004);
        g.cpu.state.r[15] = 0x03000000;
        g.cpu.state.r[14] = 0x08000104; // saved LR
        step(&mut g);
        assert_eq!(g.cpu.state.mode(), mode::SYS); // back to SYS
        assert_eq!(g.cpu.state.cpsr & FLAG_T, FLAG_T); // T restored
        assert_eq!(g.cpu.state.r[15], 0x08000100); // PC restored
    }

    #[test]
    fn banked_sp_swap_on_mode_change() {
        let mut g = setup(&[0xE320F000]);
        g.cpu.state.cpsr = mode::SYS;
        g.cpu.state.r[13] = 0xDEAD_C0DE;
        g.cpu.state.switch_mode(mode::IRQ);
        assert_ne!(g.cpu.state.r[13], 0xDEAD_C0DE); // different banked SP
        g.cpu.state.switch_mode(mode::SYS);
        assert_eq!(g.cpu.state.r[13], 0xDEAD_C0DE); // restored
    }

    // ---- Halt + IRQ wakeup (the boot stall pattern) ----

    #[test]
    fn halted_wakes_on_irq() {
        let mut g = setup(&[]);
        g.cpu.state.halted = true;
        g.cpu.state.cpsr = mode::SYS; // I clear -> IRQ enabled
        g.irq.set_ime(1);
        g.irq.set_ie(IRQ_VBLANK);
        g.irq.raise(IRQ_VBLANK);
        g.cpu.irq_line = g.irq.pending();
        assert!(g.cpu.irq_line);
        step(&mut g); // un-halts but doesn't take IRQ
        assert!(!g.cpu.state.halted);
        step(&mut g); // takes IRQ; executes vector (B 0x128)
        assert_eq!(g.cpu.state.mode(), mode::IRQ);
        assert_eq!(g.cpu.state.r[15], 0x128);
    }

    #[test]
    fn halted_stays_when_i_masks() {
        let mut g = setup(&[]);
        g.cpu.state.halted = true;
        g.cpu.state.cpsr = mode::SYS | FLAG_I; // I set -> masked
        g.irq.set_ime(1);
        g.irq.set_ie(IRQ_VBLANK);
        g.irq.raise(IRQ_VBLANK);
        g.cpu.irq_line = g.irq.pending();
        step(&mut g);
        assert!(g.cpu.state.halted);
    }

    #[test]
    fn halted_stays_when_ime_zero() {
        let mut g = setup(&[]);
        g.cpu.state.halted = true;
        g.cpu.state.cpsr = mode::SYS;
        g.irq.set_ime(0); // master disable
        g.irq.set_ie(IRQ_VBLANK);
        g.irq.raise(IRQ_VBLANK);
        g.cpu.irq_line = g.irq.pending();
        assert!(!g.cpu.irq_line); // pending() respects IME
        step(&mut g);
        assert!(g.cpu.state.halted);
    }

    // ---- BIOS IRQ dispatcher round-trip ----

    #[test]
    fn bios_branch_at_0x18() {
        let mut g = setup(&[]);
        let insn = Bus::read32(&mut g, 0x18);
        assert_eq!(insn, 0xEA000042); // B 0x128
        let cond = insn >> 28;
        assert_eq!(cond, 0xE);
        let off = (insn & 0x00FFFFFF) << 2;
        let target = (0x18u32 + 8).wrapping_add(off);
        assert_eq!(target, 0x128);
    }

    #[test]
    fn bios_dispatcher_starts_with_stmfd() {
        let mut g = setup(&[]);
        assert_eq!(Bus::read32(&mut g, 0x128), 0xE92D500F);
    }

    #[test]
    fn bios_dispatcher_round_trip() {
        let mut g = setup(&[]);
        // Plant a minimal user IRQ handler at IWRAM 0x03002000.
        Bus::write32(&mut g, 0x03002000, 0xE3A00001); // MOV R0, #1
        Bus::write32(&mut g, 0x03002004, 0xE59F1004); // LDR R1, [PC, #4]
        Bus::write32(&mut g, 0x03002008, 0xE1C100B0); // STRH R0, [R1]
        Bus::write32(&mut g, 0x0300200C, 0xE12FFF1E); // BX LR
        Bus::write32(&mut g, 0x03002010, 0x04000202); // literal
        Bus::write32(&mut g, 0x03007FFC, 0x03002000); // handler pointer
        g.cpu.state.cpsr = mode::SYS;
        g.cpu.state.r[15] = 0x08000100;
        g.irq.set_ime(1);
        g.irq.set_ie(IRQ_VBLANK);
        g.irq.raise(IRQ_VBLANK);
        g.cpu.irq_line = true;

        let mut saw_handler = false;
        let mut saw_subs = false;
        let mut saw_return = false;
        let mut took_irq = false;
        for _ in 0..30 {
            g.cpu.irq_line = g.irq.pending();
            let pc_before = g.cpu.state.r[15];
            if pc_before == 0x03002000 {
                saw_handler = true;
            }
            if pc_before == 0x13C {
                saw_subs = true;
            }
            if g.cpu.state.mode() == mode::IRQ {
                took_irq = true;
            }
            if took_irq && g.cpu.state.mode() == mode::SYS && pc_before == 0x08000100 {
                saw_return = true;
                break;
            }
            step(&mut g);
        }
        assert!(saw_handler, "handler not reached");
        assert!(saw_subs, "SUBS PC,LR not reached");
        assert!(saw_return, "did not return to user code");
        assert_eq!(g.irq.iflag & IRQ_VBLANK, 0); // acked
        assert_eq!(g.cpu.state.mode(), mode::SYS);
    }

    // ---- IO register behavior ----

    #[test]
    fn write_dispcnt_reflects_in_ppu() {
        let mut g = setup(&[]);
        Bus::write16(&mut g, 0x04000000, 0x0100); // BG0 enable
        assert_eq!(g.ppu.dispcnt, 0x0100);
    }

    #[test]
    fn write_ie_updates_irq() {
        let mut g = setup(&[]);
        Bus::write16(&mut g, 0x04000200, 0x0001); // VBlank enable
        assert_eq!(g.irq.ie, 0x0001);
    }

    #[test]
    fn write_if_acks() {
        let mut g = setup(&[]);
        g.irq.raise(0x0001);
        Bus::write16(&mut g, 0x04000202, 0x0001);
        assert_eq!(g.irq.iflag, 0);
    }

    #[test]
    fn vcount_reads_ppu() {
        let mut g = setup(&[]);
        g.ppu.vcount = 42;
        assert_eq!(Bus::read16(&mut g, 0x04000006), 42);
    }

    #[test]
    fn byte_write_pram_mirrors_halfword() {
        let mut g = setup(&[]);
        Bus::write8(&mut g, 0x05000000, 0xAB);
        assert_eq!(Bus::read16(&mut g, 0x05000000), 0xABAB);
    }

    #[test]
    fn vram_byte_write_obj_dropped() {
        let mut g = setup(&[]);
        Bus::write8(&mut g, 0x06010000, 0xFF);
        assert_eq!(Bus::read16(&mut g, 0x06010000), 0);
    }

    #[test]
    fn oam_ignores_byte_writes() {
        let mut g = setup(&[]);
        Bus::write8(&mut g, 0x07000000, 0xFF);
        assert_eq!(Bus::read8(&mut g, 0x07000000), 0);
    }

    // ---- Memory mirrors and rotations ----

    #[test]
    fn iwram_mirrors_every_32k() {
        let mut g = setup(&[]);
        Bus::write32(&mut g, 0x03000000, 0xDEADBEEF);
        assert_eq!(Bus::read32(&mut g, 0x03008000), 0xDEADBEEF);
        Bus::write32(&mut g, 0x03007FFC, 0xCAFEBABE);
        assert_eq!(Bus::read32(&mut g, 0x03FFFFFC), 0xCAFEBABE); // mirror access
    }

    #[test]
    fn vram_mirror_fold() {
        let mut g = setup(&[]);
        Bus::write32(&mut g, 0x06010000, 0xAA55_AA55);
        assert_eq!(Bus::read32(&mut g, 0x06018000), 0xAA55_AA55);
    }
}
