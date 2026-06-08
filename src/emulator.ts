import { Bus, SaveBridge } from './memory/bus';
import { Flash128K } from './memory/flash';
import { Sram32K } from './memory/sram';
import { detectSaveType, type SaveType } from './memory/saveDetect';
import { Rtc } from './memory/rtc';
import { Cpu } from './cpu/cpu';
import { Ppu } from './ppu/ppu';
import { Io } from './io/io';
import { Dma } from './io/dma';
import { Timers } from './io/timers';
import { Irq } from './io/irq';
import { Keypad } from './io/keypad';
import { Sound } from './io/sound';
import { applyCheats, type Cheat } from './io/cheats';
import { BiosHle } from './bios/hle';
import { Recompiler } from './recomp/compiler';

const CYCLES_PER_FRAME = 280896;

export class Emulator {
  bus = new Bus();
  flash = new Flash128K();
  sram = new Sram32K();
  rtc = new Rtc();
  saveType: SaveType = 'flash128';
  cheats: Cheat[] = [];
  // The currently-active save backend (= one of `flash` or `sram`,
  // picked by detectSaveType at loadRom time). Type widened to the
  // common SaveBridge so `Emulator.save` can be substituted freely
  // without callers needing to know which chip it is.
  save: SaveBridge & { data: Uint8Array; onChange: (() => void) | null; loadSave: (b: Uint8Array) => void } = this.flash;
  irq = new Irq();
  keypad = new Keypad();
  ppu: Ppu;
  dma: Dma;
  timers: Timers;
  cpu: Cpu;
  io: Io;
  sound: Sound;
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
    this.sound = new Sound(this.dma);
    this.sound.timers = this.timers;
    this.timers.sound = this.sound;
    this.io.sound = this.sound;
    this.bios = new BiosHle(this.cpu, this.bus);
    this.cpu.bios = this.bios;
    this.recomp = new Recompiler(this.cpu);
    this.bus.attachIo(this.io);
    // attachSave gets a thin closure so we can swap `this.save` per
    // ROM (Flash vs SRAM vs eventually EEPROM) without re-attaching.
    this.bus.attachSave({
      read:  (a) => this.save.read(a),
      write: (a, v) => this.save.write(a, v),
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
    // Pick the save backend from the ROM's embedded "SRAM_V" /
    // "FLASH_V" / "FLASH1M_V" / "EEPROM_V" signature. Default is
    // 128 KB Flash so unknown ROMs still get something workable.
    this.saveType = detectSaveType(bytes);
    switch (this.saveType) {
      case 'sram':
        this.save = this.sram;
        break;
      // EEPROM 512/8K not yet implemented; both fall back to Flash 128
      // until the bit-serial protocol lands.
      case 'flash64':
      case 'flash128':
      case 'eeprom512':
      case 'eeprom8k':
      case 'none':
      default:
        this.save = this.flash;
        break;
    }
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
        // tryDispatch returns the number of THUMB insns the JIT block
        // executed (0 when it didn't dispatch). We MUST advance `i` by
        // that count, not by 1 — otherwise a JIT block of 20 insns
        // counts as a single cycle, PPU/timers under-step by 20×, and
        // the game's main loop ticks ~20× per real-time VBlank.
        const jitN = this.recomp.tryDispatch();
        if (jitN > 0) i += jitN;
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
    // Re-apply any enabled cheats at the END of the frame, after the
    // game has had a chance to update RAM. This is the standard mGBA/
    // VBA approach — cheats fire once per VBlank, which is enough to
    // pin a value (HP, money, etc.) even if game code briefly
    // overwrites it during the next frame.
    if (this.cheats.length > 0) applyCheats(this.bus, this.cheats);
    return {
      interp: this.recomp.intInsns - intStart,
      jit: this.recomp.jitInsns - jitStart,
      frames: this.ppu.frameCount,
    };
  }
}
