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
  }

  loadRom(bytes: Uint8Array): void {
    this.bus.loadRom(bytes);
    this.cpu.reset();
    // Cartridge-bypass boot leaves DISPSTAT in a state the real BIOS would
    // have already touched — enable VBlank/HBlank/VCount IRQ defaults so
    // games that don't explicitly write DISPSTAT can still receive IRQs.
    this.ppu.dispstat = 0x38;
  }

  // Run a full GBA frame worth of cycles (~280896). Returns insn counts
  // for the UI's stat readout.
  runFrame(): { interp: number; jit: number; frames: number } {
    let executed = 0;
    let jitStart = this.recomp.jitInsns;
    let intStart = this.recomp.intInsns;
    const cpu = this.cpu;
    while (executed < CYCLES_PER_FRAME) {
      cpu.irqLine = this.irq.pending();
      let cycles: number;
      if (this.recomp.tryDispatch()) {
        cycles = 1;
      } else {
        cycles = cpu.step();
        this.recomp.intInsns++;
      }
      this.ppu.step(cycles);
      this.timers.step(cycles);
      executed += cycles;
      if (this.ppu.frameDone) { this.ppu.frameDone = false; break; }
    }
    return {
      interp: this.recomp.intInsns - intStart,
      jit: this.recomp.jitInsns - jitStart,
      frames: this.ppu.frameCount,
    };
  }
}
