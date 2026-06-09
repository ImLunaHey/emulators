// Headless harness for debugging Mario Kart Super Circuit's cable
// detection. Boots two Emulator instances and wires them together via
// a direct in-memory MockLinkTransport pair (no WS, no Worker, no
// browser). Lets us instrument and iterate without involving the user.
//
// Usage:
//   npx tsx src/test/link-debug.ts public/mario-kart-super-circuit.gba 1800
//
// Prints, every N frames, both emulators' SIOCNT / SIOMULTI / IRQ state
// plus the contents of Mario Kart's cable-detect IWRAM struct at
// 0x03002af0 (size 16 bytes). Optional env vars at the top tweak what
// gets dumped.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { Emulator } from '../emulator';
import { Key } from '../io/keypad';
import {
  LocalLoopback,
  type LinkTransport,
  type MultiplayResult,
  type Sio,
} from '../io/sio';

// ---------------------------------------------------------- mock transport

// MockLinkPair wires two Sio instances together with zero network
// latency. The lockstep path (requestMultiplay) is synchronous: when
// master calls it, we read slave's current mltSend, apply the snapshot
// to slave's Sio, and invoke the callback immediately. That's the
// best-case scenario for the lockstep model — anything Mario Kart
// fails to do here, it would also fail in real B-2 over the wire.
class MockTransport implements LinkTransport {
  // Set by MockLinkPair.connect.
  peer: MockTransport | null = null;
  master: boolean;
  private _connected = false;
  constructor(private localSio: Sio, master: boolean) { this.master = master; }
  setConnected(v: boolean): void { this._connected = v; }

  isConnected(): boolean { return this._connected; }
  isMaster(): boolean { return this.master; }

  multiplayExchange(localData: number): MultiplayResult {
    if (!this._connected || !this.peer) {
      return { d0: localData & 0xFFFF, d1: 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: false };
    }
    return {
      d0: localData & 0xFFFF,
      d1: this.peer.localSio.mltSend & 0xFFFF,
      d2: 0xFFFF, d3: 0xFFFF, error: false,
    };
  }
  normal32Exchange(_localData: number): number {
    if (!this._connected || !this.peer) return 0xFFFFFFFF;
    const lo = this.peer.localSio.multi[0] & 0xFFFF;
    const hi = this.peer.localSio.multi[1] & 0xFFFF;
    return ((hi << 16) | lo) >>> 0;
  }
  normal8Exchange(_localData: number): number {
    if (!this._connected || !this.peer) return 0xFF;
    return this.peer.localSio.mltSend & 0xFF;
  }

  // Env-gated. When MOCK_LOCKSTEP=0, falls back to the cycle-based
  // sync path (B-1 semantics) so we can compare the two transports.
  requestMultiplay(localData: number, onComplete: (r: MultiplayResult) => void): boolean {
    if (process.env.MOCK_LOCKSTEP === '0') return false;
    if (!this.master || !this._connected || !this.peer) return false;
    const slaveData = this.peer.localSio.mltSend & 0xFFFF;
    const result: MultiplayResult = {
      d0: localData & 0xFFFF,
      d1: slaveData,
      d2: 0xFFFF, d3: 0xFFFF, error: false,
    };
    this.peer.localSio.applyRemoteMultiplay(result.d0, result.d1, result.d2, result.d3, false);
    onComplete(result);
    return true;
  }
}

function pair(emuA: Emulator, emuB: Emulator): { a: MockTransport; b: MockTransport } {
  const a = new MockTransport(emuA.io.sio, true);
  const b = new MockTransport(emuB.io.sio, false);
  a.peer = b; b.peer = a;
  a.setConnected(true); b.setConnected(true);
  emuA.io.sio.transport = a;
  emuB.io.sio.transport = b;
  return { a, b };
}

// ---------------------------------------------------------- main

const romPath = process.argv[2] ?? 'public/mario-kart-super-circuit.gba';
const totalFrames = parseInt(process.argv[3] ?? '600', 10);
const reportEvery = parseInt(process.env.REPORT_EVERY ?? '60', 10);
// We connect the link only AFTER both emulators are on the cable-
// check screen — connecting before that just sets SD high while the
// game is still on its title and doesn't exercise the handshake at
// all. Default lines up with the menu nav below: title (~360 f) +
// nav inputs (~120 f) ≈ frame 600.
const connectAtFrame = parseInt(process.env.CONNECT_AT ?? '720', 10);

// Frame-driven scripted input: hold each Key for the given window
// [pressAt, releaseAt). Applied to both emulators in lockstep so they
// land on the same screen together. Numbers tuned for Mario Kart
// Super Circuit's title sequence:
//   - Title fades in / accepts START around f≈300
//   - START accepted, lands on the main menu (single-player highlighted)
//   - DOWN once  → multiplayer
//   - A          → enter multiplayer submenu
//   - A          → select Multi-Pak (cable cartridge)
//   - Now on the "insert link cable" screen.
interface InputEvent { frame: number; key: Key; release?: number; }
const inputScript: InputEvent[] = [
  // Title screen → main menu. One START is enough; a second one ends
  // up selecting whatever's highlighted (Single Player by default).
  { frame: 320,  key: Key.START,  release: 326  },
  // Main menu shows "SINGLE PLAYER" highlighted. Move down to
  // Multi-Player and enter.
  { frame: 540,  key: Key.DOWN,   release: 548  },
  { frame: 600,  key: Key.A,      release: 608  },
  // Multi-Player submenu — first option is Multi-Pak (real cable).
  { frame: 660,  key: Key.A,      release: 668  },
];

const rom = new Uint8Array(readFileSync(romPath));

const emuA = new Emulator();
const emuB = new Emulator();
emuA.loadRom(rom);
emuB.loadRom(rom);
// Start disconnected — the LocalLoopback default is in place from the
// Emulator constructor. We swap in the MockLinkPair only at
// `connectAtFrame`, mirroring the real "boot the game, then press a
// button to enter multiplayer, then link cable gets connected" flow.
emuA.io.sio.transport = new LocalLoopback();
emuB.io.sio.transport = new LocalLoopback();

console.log(`# Mario Kart link debug — ${totalFrames} frames; connect at frame ${connectAtFrame}`);
console.log(`# A = master (room creator) · B = slave (joiner)`);

// Dump shape of Mario Kart's cable-detect IWRAM struct + key SIO regs.
function snapshot(label: string, frame: number, emu: Emulator, isMaster: boolean): void {
  const iw = emu.bus.iwram;
  const base = 0x2af0;          // 0x03002af0 - 0x03000000
  const struct: string[] = [];
  for (let i = 0; i < 16; i++) struct.push(iw[base + i].toString(16).padStart(2, '0'));
  const siocnt = emu.io.read16(0x4000128);
  const mlt = emu.io.read16(0x400012A);
  const m0 = emu.io.read16(0x4000120);
  const m1 = emu.io.read16(0x4000122);
  const ie = emu.irq.ie;
  const iflag = emu.irq.iflag;
  const ime = emu.irq.ime;
  console.log(
    `[f${frame.toString().padStart(5, ' ')}] ${label}` +
    ` SIOCNT=0x${siocnt.toString(16).padStart(4, '0')}` +
    ` SD=${(siocnt>>3)&1}` +
    ` SI=${(siocnt>>2)&1}` +
    ` ID=${(siocnt>>4)&3}` +
    ` IRQ=${(siocnt>>14)&1}` +
    ` SEND=0x${mlt.toString(16).padStart(4, '0')}` +
    ` M0=0x${m0.toString(16).padStart(4, '0')}` +
    ` M1=0x${m1.toString(16).padStart(4, '0')}` +
    ` IE.sio=${(ie>>7)&1}` +
    ` IF.sio=${(iflag>>7)&1}` +
    ` IME=${ime&1}` +
    `  struct[0x2af0]=${struct.join(' ')}` +
    `${isMaster ? '  [master]' : '  [slave]'}`,
  );
}

// Instrument bus.write{8,16,32} on the master so we capture every
// write touching the cable-detect IWRAM struct. We dedup consecutive
// identical (pc, addr, val, size) triples since polling loops would
// otherwise blow the buffer.
interface BusWriteEntry { pc: number; addr: number; size: number; val: number; n: number; }
const watchLo = 0x03002af0;
const watchHi = 0x03002b20;
function instrumentBus(emu: Emulator, label: string): BusWriteEntry[] {
  const log: BusWriteEntry[] = [];
  const bus = emu.bus;
  const cpu = emu.cpu;
  const inRange = (a: number, size: number) => {
    const lo = a >>> 0;
    const hi = (a >>> 0) + size - 1;
    return hi >= watchLo && lo <= watchHi;
  };
  const push = (pc: number, addr: number, size: number, val: number) => {
    const last = log[log.length - 1];
    if (last && last.pc === pc && last.addr === addr && last.size === size && last.val === val) {
      last.n++; return;
    }
    log.push({ pc, addr, size, val, n: 1 });
  };
  const orig8 = bus.write8.bind(bus);
  const orig16 = bus.write16.bind(bus);
  const orig32 = bus.write32.bind(bus);
  bus.write8 = (a, v) => { if (inRange(a, 1)) push(cpu.state.r[15], a, 1, v & 0xFF); orig8(a, v); };
  bus.write16 = (a, v) => { if (inRange(a, 2)) push(cpu.state.r[15], a, 2, v & 0xFFFF); orig16(a, v); };
  bus.write32 = (a, v) => { if (inRange(a, 4)) push(cpu.state.r[15], a, 4, v >>> 0); orig32(a, v); };
  void label;
  return log;
}
const writesA = instrumentBus(emuA, 'A');
const writesB = instrumentBus(emuB, 'B');

// Count IRQ-vector dispatches on each side. cpu.takeIrq is the inner
// path that actually saves state and jumps to the user handler; if
// it doesn't fire on slave, slave's SIO IRQ handler can't be running
// and that's why struct[3] never sees bit 0 set.
interface IrqCount { total: number; sio: number; vblank: number; }
function instrumentIrq(emu: Emulator): IrqCount {
  const c: IrqCount = { total: 0, sio: 0, vblank: 0 };
  const cpu = emu.cpu;
  const origTakeIrq = cpu.takeIrq.bind(cpu);
  cpu.takeIrq = (() => {
    c.total++;
    const flag = emu.irq.iflag;
    if (flag & 0x80) c.sio++;
    if (flag & 0x01) c.vblank++;
    origTakeIrq();
  });
  return c;
}
const irqA = instrumentIrq(emuA);
const irqB = instrumentIrq(emuB);

let connected = false;
for (let f = 0; f < totalFrames; f++) {
  // Apply scripted keypad inputs to BOTH emulators so they navigate
  // the menu in lockstep.
  for (const ev of inputScript) {
    if (ev.frame === f) {
      emuA.keypad.press(ev.key); emuB.keypad.press(ev.key);
      console.log(`[f${f.toString().padStart(5, ' ')}] press key=${Key[ev.key]}`);
    }
    if (ev.release !== undefined && ev.release === f) {
      emuA.keypad.release(ev.key); emuB.keypad.release(ev.key);
    }
  }
  if (!connected && f >= connectAtFrame) {
    pair(emuA, emuB);
    connected = true;
    console.log(`# connected at frame ${f}`);
  }
  emuA.runFrame();
  emuB.runFrame();
  if (f % reportEvery === 0 || f === totalFrames - 1) {
    snapshot('A', f, emuA, true);
    snapshot('B', f, emuB, false);
    if (process.env.DUMP_PPM) {
      try { mkdirSync('/tmp/link', { recursive: true }); } catch { /* */ }
      const w = 240, h = 160;
      const dumpFrame = (emu: Emulator, suffix: string) => {
        const fb = emu.ppu.frame;
        const body = Buffer.alloc(w * h * 3);
        for (let i = 0; i < w * h; i++) {
          body[i*3] = fb[i*4]; body[i*3+1] = fb[i*4+1]; body[i*3+2] = fb[i*4+2];
        }
        const hdr = `P6\n${w} ${h}\n255\n`;
        writeFileSync(`/tmp/link/f${f.toString().padStart(5,'0')}-${suffix}.ppm`,
          Buffer.concat([Buffer.from(hdr, 'ascii'), body]));
      };
      dumpFrame(emuA, 'A');
      dumpFrame(emuB, 'B');
    }
  }
}

// Final summary — what does each side's struct[3] look like? That's
// the event-flags byte Mario Kart's phase-2 loop polls (bit 0 = the
// elusive "cable event" we're trying to set).
const aFlags = emuA.bus.iwram[0x2af3];
const bFlags = emuB.bus.iwram[0x2af3];
console.log(`\n# final struct[3] (the event-flags byte mario kart waits on):`);
console.log(`#   A (master) = 0x${aFlags.toString(16).padStart(2, '0')}  bit0=${aFlags & 1}`);
console.log(`#   B (slave)  = 0x${bFlags.toString(16).padStart(2, '0')}  bit0=${bFlags & 1}`);

// Dump every byte write that touched the cable-detect IWRAM struct,
// for both master and slave. Each row is one (pc, addr, val, size)
// pattern; n is how many times that exact pattern repeated.
function dumpWrites(label: string, log: BusWriteEntry[]): void {
  console.log(`\n# ${label} writes to 0x${watchLo.toString(16)}..0x${watchHi.toString(16)} (${log.length} unique):`);
  for (const e of log) {
    console.log(
      `  pc=0x${(e.pc>>>0).toString(16).padStart(8,'0')}` +
      `  addr=0x${e.addr.toString(16).padStart(8,'0')}` +
      `  ${e.size===1?'B':e.size===2?'H':'W'}` +
      `  val=0x${e.val.toString(16).padStart(e.size*2,'0')}` +
      `  n=${e.n}`,
    );
  }
}
// IRQ stats + the user IRQ handler installed in IWRAM. The handler
// address lives at 0x03007FFC; whatever that points at is what runs
// every time the GBA dispatches to its IRQ vector.
function userHandlerOf(emu: Emulator): number {
  const o = 0x7FFC;
  const iw = emu.bus.iwram;
  return (iw[o] | (iw[o+1] << 8) | (iw[o+2] << 16) | (iw[o+3] << 24)) >>> 0;
}
console.log(`\n# IRQ dispatch counts:`);
console.log(`#   A (master): total=${irqA.total}  sio=${irqA.sio}  vblank=${irqA.vblank}  handler=0x${userHandlerOf(emuA).toString(16)}`);
console.log(`#   B (slave):  total=${irqB.total}  sio=${irqB.sio}  vblank=${irqB.vblank}  handler=0x${userHandlerOf(emuB).toString(16)}`);

dumpWrites('master (A)', writesA);
dumpWrites('slave  (B)', writesB);
