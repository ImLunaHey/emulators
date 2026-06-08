import { Bus } from './memory/bus';
import { Flash128K } from './memory/flash';
import { Rtc } from './memory/rtc';
import { Cpu } from './cpu/cpu';
import { Ppu } from './ppu/ppu';
import { Io } from './io/io';
import { Dma } from './io/dma';
import { Timers } from './io/timers';
import { Irq } from './io/irq';
import { Keypad } from './io/keypad';
import { BiosHle } from './bios/hle';
import { Recompiler } from './recomp/compiler';

const CYCLES_PER_FRAME = 280896;

export class Emulator {
  bus = new Bus();
  flash = new Flash128K();
  rtc = new Rtc();
  irq = new Irq();
  keypad = new Keypad();
  ppu: Ppu;
  dma: Dma;
  timers: Timers;
  cpu: Cpu;
  io: Io;
  bios: BiosHle;
  recomp: Recompiler;
  // Cumulative cycle budget. We over- or under-run by up to one scanline.
  cycleCarry = 0;

  constructor() {
    this.dma = new Dma(this.bus, this.irq);
    this.timers = new Timers(this.irq);
    this.ppu = new Ppu(this.bus, this.irq, this.dma);
    this.cpu = new Cpu(this.bus);
    this.io = new Io(this.bus, this.ppu, this.dma, this.timers, this.irq, this.keypad, this.cpu);
    this.bios = new BiosHle(this.cpu, this.bus);
    this.cpu.bios = this.bios;
    this.recomp = new Recompiler(this.cpu);
    this.bus.attachIo(this.io);
    this.bus.attachSave({
      read:  (a) => this.flash.read(a),
      write: (a, v) => this.flash.write(a, v),
    });
    this.installRtcInterposer();
  }

  // Route reads/writes at the cart-GPIO range (0x080000C4/C6/C8) to the
  // on-board RTC instead of letting them hit raw ROM. Pokemon
  // Ruby/Sapphire/Emerald and FireRed/LeafGreen use this for berry
  // growth, dewford trends, etc. The interposer used to live in App.tsx
  // so headless boot wasn't routing GPIO at all (= zero RTC activity in
  // CLI tests).
  private installRtcInterposer(): void {
    const bus = this.bus;
    const rtc = this.rtc;
    const inRange = (a: number) => (a & 0xFFFFFFFE) === 0x080000C4 ||
                                    (a & 0xFFFFFFFE) === 0x080000C6 ||
                                    (a & 0xFFFFFFFE) === 0x080000C8;
    const oR16 = bus.read16.bind(bus);
    const oW16 = bus.write16.bind(bus);
    const oR8 = bus.read8.bind(bus);
    const oW8 = bus.write8.bind(bus);
    bus.read16 = (a) => inRange(a) ? rtc.read(a & 0xFF) : oR16(a);
    bus.write16 = (a, v) => { if (inRange(a)) rtc.write(a & 0xFF, v); else oW16(a, v); };
    bus.read8 = (a) => inRange(a) ? rtc.read(a & 0xFF) : oR8(a);
    bus.write8 = (a, v) => { if (inRange(a)) rtc.write(a & 0xFF, v); else oW8(a, v); };
  }

  loadRom(bytes: Uint8Array): void {
    this.bus.loadRom(bytes);
    this.cpu.reset();
    // Cartridge-bypass boot leaves DISPSTAT in a state the real BIOS would
    // have already touched — enable VBlank/HBlank/VCount IRQ defaults so
    // games that don't explicitly write DISPSTAT can still receive IRQs.
    this.ppu.dispstat = 0x38;
    // Apply the BIOS's affine-register defaults (PA=PD=0x100, PB=PC=0).
    // Pokemon FireRed's Oak intro and other Mode 1/2 scenes enable BG2
    // affine and never write PA/PD, expecting identity sampling.
    this.bios.resetAffineDefaults();
  }

  // Run a full GBA frame worth of cycles (~280896). Returns insn counts
  // for the UI's stat readout.
  runFrame(): { interp: number; jit: number; frames: number } {
    let executed = 0;
    const jitStart = this.recomp.jitInsns;
    const intStart = this.recomp.intInsns;
    const cpu = this.cpu;
    const ppu = this.ppu;
    const timers = this.timers;
    const irq = this.irq;
    while (executed < CYCLES_PER_FRAME) {
      // Batch CPU steps before touching PPU/Timer. Bound by min(remaining
      // frame budget, cycles until next scanline, 64) so PPU never lags a
      // scanline boundary by more than a batch.
      const lineRemaining = 1232 - ppu.cyclesAccum;
      let batch = lineRemaining < 64 ? lineRemaining : 64;
      if (batch > CYCLES_PER_FRAME - executed) batch = CYCLES_PER_FRAME - executed;
      if (batch <= 0) batch = 1;
      let i = 0;
      while (i < batch) {
        cpu.irqLine = irq.cachedPending;
        if (this.recomp.tryDispatch()) i++;
        else { cpu.step(); i++; this.recomp.intInsns++; }
        if (cpu.state.halted) { i = batch; break; }
      }
      ppu.step(i);
      timers.step(i);
      executed += i;
      if (ppu.frameDone) { ppu.frameDone = false; break; }
    }
    // BIOS-side IntrCheck flag: the BIOS sets bit 0 of *(u16*)0x03007FF8 on
    // VBlank IRQ. Our HLE doesn't drive this through a real BIOS handler,
    // so we set it directly each frame.
    this.bus.iwram[0x7FF8] |= 0x01;
    return {
      interp: this.recomp.intInsns - intStart,
      jit: this.recomp.jitInsns - jitStart,
      frames: this.ppu.frameCount,
    };
  }
}
