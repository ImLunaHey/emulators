import type { Emulator } from './emulator';

// Whole-emulator state snapshot. Captures everything we need to
// resume execution from this exact byte, on this exact CPU
// instruction, with this exact PPU scanline progress and IO state.
//
// Format: magic + version + size-prefixed sections. Sections are
// independent so older blobs can grow new fields at the end with a
// version bump and the loader skips unknown ones.
//
// What's NOT captured:
//   - Audio output queue (Web Audio's playback ring) — gets re-filled
//     on the next runFrame; a one-second audio gap on resume is fine.
//   - Recompiler caches — rebuilt lazily; the saved CPU state alone
//     is enough to re-warm them.
//   - Link-cable transport — savestates are per-emulator; resume
//     drops back to LocalLoopback and the user re-connects.
//
// Cheats are not captured here; they're stored separately in the UI
// layer and re-applied per-frame regardless.

const MAGIC = 0x47424153; // 'GBAS' (little-endian)
const VERSION = 1;

// Section tags — each is a 4-byte LE u32 followed by a 4-byte LE u32
// length, then `length` bytes of payload. Unknown tags are skipped.
const TAG = {
  CPU: 0x01435055,    // 'UPC\x01'
  IWRAM: 0x4d574901,  // 'IWM\x01'
  EWRAM: 0x4d574501,  // 'EWM\x01'
  VRAM: 0x4d525601,   // 'VRM\x01'
  PRAM: 0x4d525001,   // 'PRM\x01'
  OAM: 0x4d414f01,    // 'OAM\x01'
  IO:   0x4f495001,   // 'PIO\x01'
  PPU:  0x55505001,   // 'PPU\x01'
  TIMERS: 0x4d495401, // 'TIM\x01'
  DMA:  0x414d4401,   // 'DMA\x01'
  IRQ:  0x51524901,   // 'IRQ\x01'
  SIO:  0x4f495301,   // 'SIO\x01'
  SAVE: 0x56415301,   // 'SAV\x01' — Flash/SRAM/EEPROM chip data
  CYCLES: 0x4c435901, // 'YCL\x01'
} as const;

class Writer {
  private chunks: Uint8Array[] = [];
  private size = 0;

  u32(v: number): void {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, v >>> 0, true);
    this.chunks.push(b);
    this.size += 4;
  }
  bytes(b: Uint8Array): void {
    this.chunks.push(new Uint8Array(b));
    this.size += b.length;
  }
  section(tag: number, body: (w: Writer) => void): void {
    const inner = new Writer();
    body(inner);
    const payload = inner.finish();
    this.u32(tag);
    this.u32(payload.length);
    this.bytes(payload);
  }
  finish(): Uint8Array {
    const out = new Uint8Array(this.size);
    let p = 0;
    for (const c of this.chunks) { out.set(c, p); p += c.length; }
    return out;
  }
}

class Reader {
  private dv: DataView;
  private p = 0;
  constructor(private buf: Uint8Array) {
    this.dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  }
  u32(): number { const v = this.dv.getUint32(this.p, true); this.p += 4; return v >>> 0; }
  bytes(n: number): Uint8Array { const out = this.buf.subarray(this.p, this.p + n); this.p += n; return out; }
  eof(): boolean { return this.p >= this.buf.length; }
  remaining(): number { return this.buf.length - this.p; }
}

export function saveState(emu: Emulator): Uint8Array {
  const w = new Writer();
  w.u32(MAGIC);
  w.u32(VERSION);

  // CPU registers + banks + CPSR + halt state.
  w.section(TAG.CPU, (s) => {
    const c = emu.cpu;
    for (let i = 0; i < 16; i++) s.u32(c.state.r[i]);
    for (let i = 0; i < 6; i++) s.u32(c.state.bank_r13[i]);
    for (let i = 0; i < 6; i++) s.u32(c.state.bank_r14[i]);
    for (let i = 0; i < 6; i++) s.u32(c.state.bank_spsr[i]);
    for (let i = 0; i < 5; i++) s.u32(c.state.fiq_r8_12[i]);
    for (let i = 0; i < 5; i++) s.u32(c.state.usr_r8_12[i]);
    s.u32(c.state.usr_r13);
    s.u32(c.state.usr_r14);
    s.u32(c.state.cpsr);
    s.u32(c.state.halted ? 1 : 0);
    s.u32(c.cycles >>> 0);
    s.u32(c.irqLine ? 1 : 0);
  });

  // Memory regions — straight byte copies. EWRAM is the biggest at
  // 256 KB; everything else is much smaller.
  w.section(TAG.IWRAM, (s) => s.bytes(emu.bus.iwram));
  w.section(TAG.EWRAM, (s) => s.bytes(emu.bus.ewram));
  w.section(TAG.VRAM, (s) => s.bytes(emu.bus.vram));
  w.section(TAG.PRAM, (s) => s.bytes(emu.bus.pram));
  w.section(TAG.OAM, (s) => s.bytes(emu.bus.oam));
  w.section(TAG.IO, (s) => s.bytes(emu.io.raw));

  // PPU — registers + the rolling scanline cycle counter so we resume
  // mid-frame on the same scanline.
  w.section(TAG.PPU, (s) => {
    const p = emu.ppu;
    s.u32(p.dispcnt);
    s.u32(p.dispstat);
    s.u32(p.vcount);
    for (let i = 0; i < 4; i++) s.u32(p.bgcnt[i]);
    for (let i = 0; i < 4; i++) s.u32(p.bgHOFS[i]);
    for (let i = 0; i < 4; i++) s.u32(p.bgVOFS[i]);
    for (let i = 0; i < 2; i++) s.u32(p.bgX[i] >>> 0);
    for (let i = 0; i < 2; i++) s.u32(p.bgY[i] >>> 0);
    for (let i = 0; i < 2; i++) s.u32(p.bgPA[i] & 0xFFFF);
    for (let i = 0; i < 2; i++) s.u32(p.bgPB[i] & 0xFFFF);
    for (let i = 0; i < 2; i++) s.u32(p.bgPC[i] & 0xFFFF);
    for (let i = 0; i < 2; i++) s.u32(p.bgPD[i] & 0xFFFF);
    s.u32(p.win0H); s.u32(p.win1H); s.u32(p.win0V); s.u32(p.win1V);
    s.u32(p.winIn); s.u32(p.winOut);
    s.u32(p.mosaic);
    s.u32(p.bldcnt); s.u32(p.bldalpha); s.u32(p.bldy);
    s.u32(p.cyclesAccum);
    s.u32(p.inHBlank ? 1 : 0);
    s.u32(p.frameCount);
  });

  // Timers — counter/reload/control + subCycles for each channel.
  w.section(TAG.TIMERS, (s) => {
    for (const ch of emu.timers.ch) {
      s.u32(ch.reload);
      s.u32(ch.counter);
      s.u32(ch.control);
      s.u32(ch.subCycles);
      s.u32(ch.enabled ? 1 : 0);
      s.u32(ch.countUp ? 1 : 0);
      s.u32(ch.irqEnable ? 1 : 0);
      s.u32(ch.prescale);
    }
  });

  // DMA — 4 channels of src/dst/count/control plus the internal
  // counters that track in-flight transfers.
  w.section(TAG.DMA, (s) => {
    for (const ch of emu.dma.ch) {
      s.u32(ch.src);
      s.u32(ch.dst);
      s.u32(ch.count);
      s.u32(ch.control);
      // Internal book-keeping fields the bus needs to resume an
      // in-flight DMA. Same names as in dma.ts.
      const anyCh = ch as unknown as Record<string, number>;
      s.u32(anyCh.internalSrc ?? 0);
      s.u32(anyCh.internalDst ?? 0);
      s.u32(anyCh.internalCount ?? 0);
    }
  });

  w.section(TAG.IRQ, (s) => {
    s.u32(emu.irq.ie);
    s.u32(emu.irq.iflag);
    s.u32(emu.irq.ime);
  });

  // SIO — register file + transferSeq so resume re-syncs with peer.
  // The transport pointer itself is not serialized; we always resume
  // with LocalLoopback so the user must explicitly reconnect.
  w.section(TAG.SIO, (s) => {
    const sio = emu.io.sio;
    for (let i = 0; i < 4; i++) s.u32(sio.multi[i]);
    s.u32(sio.siocnt);
    s.u32(sio.mltSend);
    s.u32(sio.rcnt);
    s.u32(sio.joycnt);
    s.u32(sio.joyRecv);
    s.u32(sio.joyTrans);
    s.u32(sio.joystat);
    s.u32(sio.transferSeq);
  });

  // Save chip data — Flash/SRAM/EEPROM all share the SaveBridge
  // surface; we capture the raw byte buffer so the game's save
  // memory survives the round-trip.
  w.section(TAG.SAVE, (s) => {
    s.u32(emu.save.data.length);
    s.bytes(emu.save.data);
  });

  return w.finish();
}

export function loadState(emu: Emulator, blob: Uint8Array): void {
  const r = new Reader(blob);
  const magic = r.u32();
  if (magic !== MAGIC) throw new Error(`bad magic 0x${magic.toString(16)}`);
  const version = r.u32();
  if (version > VERSION) throw new Error(`unsupported version ${version}`);

  while (!r.eof()) {
    const tag = r.u32();
    const len = r.u32();
    const body = r.bytes(len);
    applySection(emu, tag, body);
  }

  // Defensive cleanup after restore: clear any in-flight pipeline
  // prefetch and recompiler cache so the CPU re-fetches from r[15].
  const cpu = emu.cpu;
  // prefetchedValid lives on Cpu as a private field; resetting the
  // entire prefetch state is what reset() does, so just clear the
  // relevant bit via a stub method when available.
  (cpu as unknown as { prefetchedValid: boolean }).prefetchedValid = false;
  emu.recomp.invalidate();
  // PPU should re-render fresh — clear any half-rendered scanline.
  emu.ppu.frameDone = false;
}

function applySection(emu: Emulator, tag: number, body: Uint8Array): void {
  const r = new Reader(body);
  switch (tag) {
    case TAG.CPU: {
      const c = emu.cpu;
      for (let i = 0; i < 16; i++) c.state.r[i] = r.u32();
      for (let i = 0; i < 6; i++) c.state.bank_r13[i] = r.u32();
      for (let i = 0; i < 6; i++) c.state.bank_r14[i] = r.u32();
      for (let i = 0; i < 6; i++) c.state.bank_spsr[i] = r.u32();
      for (let i = 0; i < 5; i++) c.state.fiq_r8_12[i] = r.u32();
      for (let i = 0; i < 5; i++) c.state.usr_r8_12[i] = r.u32();
      c.state.usr_r13 = r.u32();
      c.state.usr_r14 = r.u32();
      c.state.cpsr = r.u32();
      c.state.halted = r.u32() !== 0;
      c.cycles = r.u32();
      c.irqLine = r.u32() !== 0;
      break;
    }
    case TAG.IWRAM: emu.bus.iwram.set(body); break;
    case TAG.EWRAM: emu.bus.ewram.set(body); break;
    case TAG.VRAM: emu.bus.vram.set(body); break;
    case TAG.PRAM: emu.bus.pram.set(body); break;
    case TAG.OAM: emu.bus.oam.set(body); break;
    case TAG.IO: emu.io.raw.set(body); break;
    case TAG.PPU: {
      const p = emu.ppu;
      p.dispcnt = r.u32();
      p.dispstat = r.u32();
      p.vcount = r.u32();
      for (let i = 0; i < 4; i++) p.bgcnt[i] = r.u32();
      for (let i = 0; i < 4; i++) p.bgHOFS[i] = r.u32();
      for (let i = 0; i < 4; i++) p.bgVOFS[i] = r.u32();
      for (let i = 0; i < 2; i++) p.bgX[i] = r.u32() | 0;
      for (let i = 0; i < 2; i++) p.bgY[i] = r.u32() | 0;
      for (let i = 0; i < 2; i++) p.bgPA[i] = r.u32() << 16 >> 16;
      for (let i = 0; i < 2; i++) p.bgPB[i] = r.u32() << 16 >> 16;
      for (let i = 0; i < 2; i++) p.bgPC[i] = r.u32() << 16 >> 16;
      for (let i = 0; i < 2; i++) p.bgPD[i] = r.u32() << 16 >> 16;
      p.win0H = r.u32(); p.win1H = r.u32(); p.win0V = r.u32(); p.win1V = r.u32();
      p.winIn = r.u32(); p.winOut = r.u32();
      p.mosaic = r.u32();
      p.bldcnt = r.u32(); p.bldalpha = r.u32(); p.bldy = r.u32();
      p.cyclesAccum = r.u32();
      p.inHBlank = r.u32() !== 0;
      p.frameCount = r.u32();
      break;
    }
    case TAG.TIMERS:
      for (const ch of emu.timers.ch) {
        ch.reload = r.u32();
        ch.counter = r.u32();
        ch.control = r.u32();
        ch.subCycles = r.u32();
        ch.enabled = r.u32() !== 0;
        ch.countUp = r.u32() !== 0;
        ch.irqEnable = r.u32() !== 0;
        ch.prescale = r.u32();
      }
      break;
    case TAG.DMA:
      for (const ch of emu.dma.ch) {
        ch.src = r.u32();
        ch.dst = r.u32();
        ch.count = r.u32();
        ch.control = r.u32();
        const anyCh = ch as unknown as Record<string, number>;
        anyCh.internalSrc = r.u32();
        anyCh.internalDst = r.u32();
        anyCh.internalCount = r.u32();
      }
      break;
    case TAG.IRQ:
      emu.irq.ie = r.u32();
      emu.irq.iflag = r.u32();
      emu.irq.ime = r.u32();
      // Recompute cachedPending so the CPU hot loop sees the
      // restored IRQ state immediately.
      emu.irq.cachedPending = (emu.irq.ime & 1) !== 0 && (emu.irq.ie & emu.irq.iflag) !== 0;
      break;
    case TAG.SIO: {
      const sio = emu.io.sio;
      for (let i = 0; i < 4; i++) sio.multi[i] = r.u32();
      sio.siocnt = r.u32();
      sio.mltSend = r.u32();
      sio.rcnt = r.u32();
      sio.joycnt = r.u32();
      sio.joyRecv = r.u32();
      sio.joyTrans = r.u32();
      sio.joystat = r.u32();
      sio.transferSeq = r.u32();
      break;
    }
    case TAG.SAVE: {
      const len = r.u32();
      emu.save.loadSave(r.bytes(len));
      break;
    }
    default:
      // Forward-compat: skip unknown sections silently.
      break;
  }
}
