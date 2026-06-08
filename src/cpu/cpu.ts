import { Bus } from '../memory/bus';
import { CpuState, FLAG_F, FLAG_I, FLAG_T, Mode } from './state';
import { armExecute } from './arm';
import { thumbExecute } from './thumb';
import type { BiosHle } from '../bios/hle';

// Cpu groups state, bus, pipeline tracking, exception vectors, and the
// per-step dispatch. The recompiler will plug in as an alternative path.

export class Cpu {
  state = new CpuState();
  bus: Bus;
  cycles = 0;
  // Pending IRQ line — IO sets this; CPU samples it between instructions.
  irqLine = false;
  bios: BiosHle | null = null;
  // Optional hot path: recompiler reads this to know where to dispatch.
  // Pipeline tracking — for THUMB we fetch the next halfword to be executed.
  // For ARM the next word. r[15] always carries the architectural PC = decoded+8/+4.
  // We keep a `nextOpcode` that we already fetched (the prefetch slot).
  // After a branch we flush and refetch.
  private prefetched = 0;
  private prefetchedValid = false;

  constructor(bus: Bus) { this.bus = bus; }

  reset(): void {
    this.state = new CpuState();
    this.state.cpsr = Mode.SVC | FLAG_F | FLAG_I;
    // Cartridge boot bypass: CPU starts in System mode with SP at the
    // canonical IWRAM stack, mirroring what the real BIOS post-init state
    // looks like to the game.
    this.state.switchMode(Mode.SYS);
    this.state.r[13] = 0x03007F00;
    this.state.bank_r13[2] = 0x03007FA0; // IRQ
    this.state.bank_r13[3] = 0x03007FE0; // SVC
    this.state.r[15] = 0x08000000;
    this.prefetchedValid = false;
    this.installBiosStub();
  }

  // Install a minimal BIOS stub:
  //   - 0x00: reset vector (cart bypass: just branches to ROM entry)
  //   - 0x18: IRQ vector → BIOS handler that calls user handler at
  //           [0x03007FFC] and returns with the canonical SUBS PC, LR, #4.
  // Without this, halted CPUs that get IRQ'd land in zero-filled BIOS and
  // wander off into open bus.
  private installBiosStub(): void {
    const bios = this.bus.bios;
    const wr32 = (off: number, v: number) => {
      bios[off]     = v & 0xFF;
      bios[off + 1] = (v >> 8) & 0xFF;
      bios[off + 2] = (v >> 16) & 0xFF;
      bios[off + 3] = (v >> 24) & 0xFF;
    };
    // 0x00 reset: branch to ROM entry via LDR PC, [PC, #0x140] (load from
    // literal in unused BIOS space). ARM B can't encode the ±32MB jump
    // from 0x0 to 0x08000000 directly.
    wr32(0x00, 0xE59FF140);  // LDR PC, [PC, #0x140] — addr = 0x00+8+0x140 = 0x148
    wr32(0x148, 0x08000000); // ROM entry literal
    // 0x04 undef
    wr32(0x04, 0xEAFFFFFE);
    // 0x08 swi:   the CPU only lands here on SWI when HLE refuses. We
    //              emulate the BIOS SWI handler in HLE, so loop forever.
    wr32(0x08, 0xEAFFFFFE);
    // 0x0C prefetch abort
    wr32(0x0C, 0xEAFFFFFE);
    // 0x10 data abort
    wr32(0x10, 0xEAFFFFFE);
    // 0x14 reserved
    wr32(0x14, 0xEAFFFFFE);
    // 0x18 IRQ: B 0x128 — jump to the dispatcher below.
    // offset24 = (0x128 - (0x18 + 8)) / 4 = 0x108/4 = 0x42
    wr32(0x18, 0xEA000042);
    // 0x1C FIQ
    wr32(0x1C, 0xEAFFFFFE);

    // IRQ dispatcher at 0x128 — calls user handler stored at 0x03007FFC.
    wr32(0x128, 0xE92D500F);  // STMFD SP!, {R0-R3, R12, LR}
    wr32(0x12C, 0xE3A00301);  // MOV R0, #0x4000000
    wr32(0x130, 0xE28FE000);  // ADR LR, 0x138
    wr32(0x134, 0xE510F004);  // LDR PC, [R0, #-4]    ; loads from 0x03FFFFFC
                              // — actually we use 0x03007FFC; the standard
                              //   trick is MOV R0,#0x4000000 + LDR [R0,#-4]
                              //   reads 0x03FFFFFC, which is mirrored from
                              //   0x03007FFC. We mirror IWRAM at the bus,
                              //   so this works.
    wr32(0x138, 0xE8BD500F);  // LDMFD SP!, {R0-R3, R12, LR}
    wr32(0x13C, 0xE25EF004);  // SUBS PC, LR, #4 (returns + restores CPSR)
  }

  flushPipeline(): void {
    this.prefetchedValid = false;
  }

  // Single dispatch — fetch from r[15] (= next decode addr), temporarily
  // raise r[15] to the architectural visible PC for execute, then advance
  // to the next decode address if execute didn't branch.
  step(): number {
    const s = this.state;
    if (s.halted) {
      if (this.irqLine && !(s.cpsr & FLAG_I)) s.halted = false;
      this.cycles += 1;
      return 1;
    }
    if (this.irqLine && !(s.cpsr & FLAG_I)) {
      this.takeIrq();
    }

    const isThumb = (s.cpsr & FLAG_T) !== 0;
    const insnSize = isThumb ? 2 : 4;
    const prefetchOff = isThumb ? 4 : 8;
    const decode = s.r[15] & (isThumb ? ~1 : ~3);
    const instr = isThumb ? this.bus.read16(decode) : this.bus.read32(decode);
    const visible = (decode + prefetchOff) >>> 0;
    s.r[15] = visible;

    if (isThumb) thumbExecute(this, instr);
    else         armExecute(this, instr);

    // No branch happened → advance to next decode.
    if (s.r[15] === visible) s.r[15] = (decode + insnSize) >>> 0;
    this.cycles += 1;
    return 1;
  }

  // Trigger exception entry — called from arm/thumb dispatch.
  softwareInterrupt(comment: number): void {
    if (this.bios && this.bios.handleSwi(comment)) {
      // HLE handled it — no real exception.
      return;
    }
    const s = this.state;
    const inThumb = (s.cpsr & FLAG_T) !== 0;
    const ret = inThumb ? (s.r[15] - 2) >>> 0 : (s.r[15] - 4) >>> 0;
    s.enterException(Mode.SVC, 0x08, ret, false);
    this.flushPipeline();
  }

  // Take an IRQ exception. r[15] at entry is the next decode address.
  // BIOS uses SUBS PC, LR, #4 to return, so LR = next_decode + 4 lands
  // PC back at next_decode after restore.
  takeIrq(): void {
    const s = this.state;
    const ret = (s.r[15] + 4) >>> 0;
    s.enterException(Mode.IRQ, 0x18, ret, false);
    this.flushPipeline();
  }

  // Halt — handled by HALTCNT BIOS HLE.
  halt(): void { this.state.halted = true; }
}
