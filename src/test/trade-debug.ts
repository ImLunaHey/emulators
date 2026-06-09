// Headless trade-error reproducer. Loads the same Pokemon Emerald
// savestate into two Emulator instances (frozen on the "Please wait"
// link-search screen), wires them via a MockLinkPair, runs both for
// a few seconds, and dumps every SIO transfer. The goal is to find
// which transfer Emerald rejects to produce "link error."
//
// Usage:  npx tsx src/test/trade-debug.ts /tmp/em.state 600
//
// Protocol notes (reverse-engineered against US Emerald):
//   IWRAM struct1 base   = 0x03003170     (phase 1 — handshake)
//   IWRAM struct2 base   = 0x03004140     (phase 2 — command exchange)
//   struct1[1]            = state byte (2=probing, 3/4=advance, 0/1=error)
//   struct1[14]           = phase marker (0=phase 1, 1=phase 2)
//   struct1[23]           = missed-packet counter (>=8 → abort)
//   SEND function         = ROM 0x0800bad0  (writes SIOMLT_SEND)
//     struct1[14]!=1 → write 0xB9A0  (probe magic)
//     struct1[14]==1 → write 0x8FFF  (phase 2 magic)
//   SEND wrapper          = ROM 0x0800ba38  (state machine)
//   Phase 2 entry         = ROM 0x0800cbcc, 0x0800cce4, 0x0800cdcc
//   Timeout-abort point   = ROM 0x0800bcce  (STRH 0 → 0x03000d70)
//
// Setting struct1[14]=1 on both sides before each frame is enough to
// get past phase 1 ("Please wait" → state advances 2 → 4). Phase 2
// then exchanges command codes (0x10/0x26/0x30/0x3d/0x41/0x42), each
// of which has its own sub-protocol. Failing any command-exchange
// step trips the link-error path. That's where remaining work lives.

import { readFileSync } from 'node:fs';
import { Emulator } from '../emulator';
import { loadState } from '../savestate';
import { LocalLoopback, type LinkTransport, type MultiplayResult } from '../io/sio';

const statePath = process.argv[2] ?? '/tmp/em.state';
const frames = parseInt(process.argv[3] ?? '600', 10);

const rom = new Uint8Array(readFileSync('public/emerald.gba'));
const state = new Uint8Array(readFileSync(statePath));

const a = new Emulator(); a.loadRom(rom); loadState(a, state);
const b = new Emulator(); b.loadRom(rom); loadState(b, state);

// Mock transport — synchronous lockstep with full trace. Captures
// every requestMultiplay round-trip with both sides' SIOMLT_SEND
// values, the resulting SIOMULTI snapshot, and the frame number.
interface TransferLog {
  frame: number;
  masterMlt: number;
  slaveMlt: number;
  result: MultiplayResult;
}
const trace: TransferLog[] = [];
let curFrame = 0;

class M implements LinkTransport {
  peer: M | null = null;
  constructor(public sio: { mltSend: number; multi: Uint16Array; applyRemoteMultiplay: (m0: number, m1: number, m2: number, m3: number, e: boolean) => void }, public master: boolean) {}
  isConnected(): boolean { return this.peer !== null; }
  isMaster(): boolean { return this.master; }
  multiplayExchange(d: number): MultiplayResult {
    return { d0: d & 0xFFFF, d1: (this.peer?.sio.mltSend ?? 0xFFFF) & 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: false };
  }
  normal32Exchange(): number { return 0xFFFFFFFF; }
  normal8Exchange(): number { return 0xFF; }
  requestMultiplay(d: number, cb: (r: MultiplayResult) => void): boolean {
    if (!this.master || !this.peer) return false;
    const slaveMlt = this.peer.sio.mltSend & 0xFFFF;
    const r: MultiplayResult = {
      d0: d & 0xFFFF, d1: slaveMlt, d2: 0xFFFF, d3: 0xFFFF, error: false,
    };
    this.peer.sio.applyRemoteMultiplay(r.d0, r.d1, r.d2, r.d3, false);
    cb(r);
    trace.push({ frame: curFrame, masterMlt: d & 0xFFFF, slaveMlt, result: r });
    return true;
  }
}

const transA = new M(a.io.sio as never, true);
const transB = new M(b.io.sio as never, false);
transA.peer = transB; transB.peer = transA;
// We want them connected from t=0 since the savestate is already on
// the "please wait" screen — the game IS actively probing right now.
a.io.sio.transport = transA;
b.io.sio.transport = transB;

// Capture the very first ~120 frames of any link activity, then thin
// the log to "first 200 transfers, then last 50" so we can see both
// the handshake start and the post-error tail.

// Also watch for EWRAM writes that look like the error-screen string
// trigger. We don't know the exact address, so just snapshot the
// framebuffer periodically and a hash of the visible scene.

function frameHash(emu: Emulator): string {
  const fb = emu.ppu.frame;
  // Sample 16 pixels across the screen to get a quick fingerprint.
  let h = 0;
  for (let i = 0; i < 16; i++) {
    const p = ((i * (fb.length / 4 / 16)) | 0) * 4;
    h = (h * 31 + fb[p] + fb[p+1] * 7 + fb[p+2] * 13) >>> 0;
  }
  return h.toString(16);
}

let prevHashA = frameHash(a);
let prevHashB = frameHash(b);
const sceneChanges: { frame: number; side: 'A'|'B'; oldHash: string; newHash: string }[] = [];

for (let f = 0; f < frames; f++) {
  curFrame = f;
  a.runFrame();
  b.runFrame();
  const ha = frameHash(a), hb = frameHash(b);
  if (ha !== prevHashA) { sceneChanges.push({ frame: f, side: 'A', oldHash: prevHashA, newHash: ha }); prevHashA = ha; }
  if (hb !== prevHashB) { sceneChanges.push({ frame: f, side: 'B', oldHash: prevHashB, newHash: hb }); prevHashB = hb; }
}

// Reporting.
console.log(`# Trade debug: ${frames} frames, ${trace.length} SIO transfers`);
console.log(`# A SIOCNT=0x${a.io.read16(0x4000128).toString(16)}  SEND=0x${a.io.read16(0x400012a).toString(16)}  M0=0x${a.io.read16(0x4000120).toString(16)} M1=0x${a.io.read16(0x4000122).toString(16)}`);
console.log(`# B SIOCNT=0x${b.io.read16(0x4000128).toString(16)}  SEND=0x${b.io.read16(0x400012a).toString(16)}  M0=0x${b.io.read16(0x4000120).toString(16)} M1=0x${b.io.read16(0x4000122).toString(16)}`);

console.log(`\n# scene changes (${sceneChanges.length}):`);
for (const sc of sceneChanges.slice(0, 60)) {
  console.log(`  f=${sc.frame.toString().padStart(4)} ${sc.side}: ${sc.oldHash} → ${sc.newHash}`);
}
if (sceneChanges.length > 60) console.log(`  … +${sceneChanges.length - 60} more`);

console.log(`\n# transfer log (first 50 + last 30):`);
const shown = trace.slice(0, 50).concat(trace.slice(-30));
for (const t of shown) {
  console.log(`  f=${t.frame.toString().padStart(4)} masterMlt=0x${t.masterMlt.toString(16).padStart(4,'0')} slaveMlt=0x${t.slaveMlt.toString(16).padStart(4,'0')} → multi=[0x${t.result.d0.toString(16).padStart(4,'0')}, 0x${t.result.d1.toString(16).padStart(4,'0')}, ${t.result.d2.toString(16)}, ${t.result.d3.toString(16)}]`);
}

// Dump screens at endpoints so we know what the game ended on.
import { writeFileSync } from 'node:fs';
for (const [emu, lbl] of [[a, 'A'], [b, 'B']] as const) {
  const fb = emu.ppu.frame;
  const w=240,h=160;
  const body = Buffer.alloc(w*h*3);
  for (let i=0;i<w*h;i++){ body[i*3]=fb[i*4]; body[i*3+1]=fb[i*4+1]; body[i*3+2]=fb[i*4+2]; }
  writeFileSync(`/tmp/trade-final-${lbl}.ppm`, Buffer.concat([Buffer.from(`P6\n${w} ${h}\n255\n`,'ascii'), body]));
}
console.log(`\n# Final screens: /tmp/trade-final-A.ppm /tmp/trade-final-B.ppm`);
