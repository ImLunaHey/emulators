// Headless screenshot harness — load a ROM, run N frames, dump the GBA
// framebuffer as a PPM that we can convert to PNG with `sips`.
//
// Usage:
//   npx tsx src/test/screenshot.ts public/starwars-droidarmy.gba 600 /tmp/sw.ppm
//
// PPM is the cheapest possible image container we can pipe to a real
// image tool; converting to PNG happens outside this script.
import { readFileSync, writeFileSync } from 'node:fs';
import { Emulator } from '../emulator';
import { Key } from '../io/keypad';

const romPath  = process.argv[2];
const frames   = parseInt(process.argv[3] ?? '600', 10);
const outPath  = process.argv[4] ?? '/tmp/gba-frame.ppm';
if (!romPath) {
  console.error('usage: screenshot.ts <rom> [frames] [out.ppm]');
  process.exit(2);
}

const rom = new Uint8Array(readFileSync(romPath));
const emu = new Emulator();
emu.loadRom(rom);
if (process.env.JIT) emu.recomp.enabled = true;

if (process.env.RRR_TRACE) {
  const orig = emu.cpu.softwareInterrupt.bind(emu.cpu);
  emu.cpu.softwareInterrupt = (n) => {
    if (n === 0x01) console.log(`SWI 0x01 RegisterRamReset mask=0x${(emu.cpu.state.r[0] & 0xFF).toString(16)} pc=0x${emu.cpu.state.r[15].toString(16)}`);
    orig(n);
  };
}

// Catalog distinct IO read addresses + how many times each.
// Catalog BLDY writes
if (process.env.BLDY_TRACE) {
  const orig = emu.io.write16.bind(emu.io);
  emu.io.write16 = (a, v) => {
    if ((a & 0x3FF) === 0x054) console.log(`BLDY <= 0x${v.toString(16)} @ pc=0x${emu.cpu.state.r[15].toString(16)}`);
    orig(a, v);
  };
}
// Catalog IRQ raises (which IRQs actually fire over the run).
if (process.env.IRQ_TRACE) {
  const orig = emu.irq.raise.bind(emu.irq);
  const counts = new Map<number, number>();
  emu.irq.raise = (bits: number) => {
    counts.set(bits, (counts.get(bits) ?? 0) + 1);
    orig(bits);
  };
  process.on('exit', () => {
    console.log('IRQ raises:');
    for (const [b, n] of Array.from(counts.entries()).sort((a,b)=>a[0]-b[0])) {
      console.log(`  0x${b.toString(16)}: ${n}`);
    }
  });
}

if (process.env.IO_READS) {
  const orig8 = emu.io.read8.bind(emu.io);
  const orig16 = emu.io.read16.bind(emu.io);
  const orig32 = emu.io.read32.bind(emu.io);
  const counts = new Map<number, number>();
  emu.io.read8 = (a) => { const off = a & 0x3FF; counts.set(off, (counts.get(off) ?? 0) + 1); return orig8(a); };
  emu.io.read16 = (a) => { const off = a & 0x3FF; counts.set(off | 0x10000, (counts.get(off | 0x10000) ?? 0) + 1); return orig16(a); };
  emu.io.read32 = (a) => { const off = a & 0x3FF; counts.set(off | 0x20000, (counts.get(off | 0x20000) ?? 0) + 1); return orig32(a); };
  process.on('exit', () => {
    const sorted = Array.from(counts.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 20);
    console.log('Top 20 IO reads (addr | width-tag : count):');
    for (const [k, n] of sorted) {
      const off = k & 0xFFFF;
      const w = (k >>> 16) === 0 ? '8b' : (k >>> 16) === 1 ? '16b' : '32b';
      console.log(`  0x040000${off.toString(16).padStart(2,'0')} ${w}: ${n}`);
    }
  });
}

// Sample PC at high frequency to find tight init loops.
// Look at VRAM/PRAM utilization to see if the game is actually drawing.
if (process.env.RAM_AUDIT_AT_END) {
  process.on('exit', () => {
    const vNonZero = Array.from(emu.bus.vram).reduce((a, b) => a + (b ? 1 : 0), 0);
    const pNonZero = Array.from(emu.bus.pram).reduce((a, b) => a + (b ? 1 : 0), 0);
    const oNonZero = Array.from(emu.bus.oam).reduce((a, b) => a + (b ? 1 : 0), 0);
    const eNonZero = Array.from(emu.bus.ewram).reduce((a, b) => a + (b ? 1 : 0), 0);
    const iNonZero = Array.from(emu.bus.iwram).reduce((a, b) => a + (b ? 1 : 0), 0);
    console.log(`Non-zero: VRAM=${vNonZero}/${emu.bus.vram.length} PRAM=${pNonZero}/${emu.bus.pram.length} OAM=${oNonZero}/${emu.bus.oam.length}  EWRAM=${eNonZero}/${emu.bus.ewram.length}  IWRAM=${iNonZero}/${emu.bus.iwram.length}`);
    // First palette entries
    const p16 = new Uint16Array(emu.bus.pram.buffer);
    console.log('PRAM[0..7]:', Array.from(p16.slice(0, 8)).map(x => '0x' + x.toString(16).padStart(4, '0')).join(' '));
  });
}

// Trace writes to PRAM and DMA channel 3 (palette transfers).
if (process.env.PRAM_TRACE) {
  const origW16 = emu.bus.write16.bind(emu.bus);
  const origW32 = emu.bus.write32.bind(emu.bus);
  let count = 0;
  emu.bus.write16 = (addr: number, v: number) => {
    if (count < 20 && (addr >>> 24) === 5) {
      console.log(`  PRAM[0x${(addr & 0x3FF).toString(16)}] <= 0x${(v & 0xFFFF).toString(16)} @pc=0x${(emu.cpu.state.r[15]).toString(16)}`);
      count++;
    }
    origW16(addr, v);
  };
  emu.bus.write32 = (addr: number, v: number) => {
    if (count < 20 && (addr >>> 24) === 5) {
      console.log(`  PRAM[0x${(addr & 0x3FF).toString(16)}] <= 0x${(v >>> 0).toString(16).padStart(8, '0')} @pc=0x${(emu.cpu.state.r[15]).toString(16)}`);
      count++;
    }
    origW32(addr, v);
  };
}

if (process.env.PC_HIST) {
  const counts = new Map<number, number>();
  const origStep = emu.cpu.step.bind(emu.cpu);
  emu.cpu.step = () => {
    const pc = emu.cpu.state.r[15] & ~3;
    counts.set(pc, (counts.get(pc) ?? 0) + 1);
    return origStep();
  };
  process.on('exit', () => {
    console.log('PC histogram (top 20):');
    const sorted = Array.from(counts.entries()).sort((a, b) => b[1] - a[1]).slice(0, 20);
    for (const [pc, n] of sorted) {
      console.log(`  0x${pc.toString(16).padStart(8, '0')}: ${n}`);
    }
  });
}

if (process.env.VCOUNT_TRACE) {
  const origR16 = emu.io.read16.bind(emu.io);
  const hist = new Map<number, number>();
  emu.io.read16 = (a: number) => {
    const v = origR16(a);
    if ((a & 0x3FF) === 0x006) hist.set(v, (hist.get(v) ?? 0) + 1);
    return v;
  };
  process.on('exit', () => {
    console.log('VCOUNT read histogram:');
    for (const [v, n] of Array.from(hist.entries()).sort((a,b)=>a[0]-b[0])) {
      console.log(`  vcount=${v.toString().padStart(3)}: ${n}`);
    }
  });
}

// Trace timer-control writes to see if the game uses hardware timers
// for game-state pacing (which would run too fast/slow if our cycle
// counting is off relative to runFrame).
// Snapshot FIFO_A occupancy at every 100k cycles. Builds a histogram.
if (process.env.FIFO_HIST) {
  const hist = new Array(34).fill(0);
  const origStep = emu.cpu.step.bind(emu.cpu);
  let sinceSnap = 0;
  emu.cpu.step = () => {
    const r = origStep();
    sinceSnap++;
    if (sinceSnap >= 100) {
      sinceSnap = 0;
      hist[Math.min(33, emu.sound.countA)]++;
    }
    return r;
  };
  process.on('exit', () => {
    console.log('FIFO_A occupancy histogram (sampled every 100 insns):');
    const total = hist.reduce((a, b) => a + b, 0) || 1;
    for (let i = 0; i < hist.length; i++) {
      if (hist[i] > 0) {
        const pct = (hist[i] * 100 / total).toFixed(1);
        console.log(`  count=${i.toString().padStart(2)}: ${hist[i]} (${pct}%)`);
      }
    }
  });
}

// Trace push spacing in CPU cycles.
if (process.env.PUSH_SPACING) {
  const origPush = emu.sound.pushA.bind(emu.sound);
  let lastCyc = 0;
  let pushes = 0;
  let burst = 0;
  let maxBurst = 0;
  const hist = new Map<number, number>();
  emu.sound.pushA = (b: number) => {
    const c = emu.cpu.cycles;
    const dt = c - lastCyc;
    if (dt < 4) burst++; else { if (burst > maxBurst) maxBurst = burst; burst = 1; }
    lastCyc = c;
    pushes++;
    // Histogram buckets: 0, 1-15, 16-127, 128-1023, 1024+
    let bucket = 0;
    if (dt >= 1024) bucket = 1024;
    else if (dt >= 128) bucket = 128;
    else if (dt >= 16) bucket = 16;
    else if (dt >= 1) bucket = 1;
    hist.set(bucket, (hist.get(bucket) ?? 0) + 1);
    origPush(b);
  };
  process.on('exit', () => {
    console.log(`pushA: total=${pushes}  maxBurst(≤4 cyc)=${maxBurst}`);
    for (const [b, n] of Array.from(hist.entries()).sort((a,b)=>a[0]-b[0])) {
      console.log(`  Δcycles bucket ≥${b}: ${n}`);
    }
  });
}

// Count Timer 0 overflows directly.
if (process.env.COUNT_OVERFLOW) {
  const orig = (emu.timers as any).overflow.bind(emu.timers);
  const counts = [0, 0, 0, 0];
  (emu.timers as any).overflow = (i: number) => {
    counts[i]++;
    orig(i);
  };
  process.on('exit', () => {
    console.log(`Timer overflows: T0=${counts[0]} T1=${counts[1]} T2=${counts[2]} T3=${counts[3]}`);
    // Approximate expected: total CPU cycles / cyclesPerOverflow.
    const totalCycles = emu.cpu.cycles;
    console.log(`CPU cycles: ${totalCycles}  T0 expected ~ ${(totalCycles/760).toFixed(0)} (760 cyc/overflow at reload=0xFD08, prescale=1)`);
  });
}

if (process.env.TRACE_TIMERS) {
  const origW = emu.io.write16.bind(emu.io);
  const totalByOff = new Map<number, number>();
  emu.io.write16 = (addr: number, v: number) => {
    const off = addr & 0x3FF;
    if ((off >= 0x100 && off <= 0x10F) || (off >= 0x080 && off <= 0x0AF)) {
      totalByOff.set(off, (totalByOff.get(off) ?? 0) + 1);
    }
    origW(addr, v);
  };
  process.on('exit', () => {
    console.log('Sound/Timer write counts (per address):');
    for (const [off, n] of Array.from(totalByOff.entries()).sort((a,b)=>b[1]-a[1])) {
      console.log(`  0x040000${off.toString(16).padStart(2,'0')}  x${n}`);
    }
  });
}

// Trace SWI invocations + which ones fail HLE.
if (process.env.TRACE_SWI) {
  const swiCounts = new Map<number, number>();
  const swiUnhandled = new Map<number, number>();
  const origSwi = emu.cpu.softwareInterrupt.bind(emu.cpu);
  emu.cpu.softwareInterrupt = (n: number) => {
    swiCounts.set(n, (swiCounts.get(n) ?? 0) + 1);
    const s = emu.cpu.state;
    const isT = (s.cpsr & 0x20) !== 0;
    const decodeAddr = (s.r[15] - (isT ? 4 : 8)) >>> 0;
    const insn = isT ? emu.bus.read16(decodeAddr) : emu.bus.read32(decodeAddr);
    if ((swiUnhandled.get(n) ?? 0) === 0) {
      console.log(`  SWI 0x${n.toString(16)} @ 0x${decodeAddr.toString(16)} insn=0x${insn.toString(16)} thumb=${isT}`);
    }
    const handled = emu.bios.handleSwi(n);
    if (!handled) swiUnhandled.set(n, (swiUnhandled.get(n) ?? 0) + 1);
    origSwi(n);
  };
  process.on('exit', () => {
    console.log('SWI counts:');
    for (const [n, c] of Array.from(swiCounts.entries()).sort((a,b)=>a[0]-b[0])) {
      const un = swiUnhandled.get(n) ?? 0;
      console.log(`  0x${n.toString(16).padStart(2,'0')}: ${c}${un ? `  (UNHANDLED ${un})` : ''}`);
    }
  });
}

console.log('Title :', new TextDecoder('ascii').decode(rom.subarray(0xA0, 0xAC)).replace(/\0/g, '').trim());
console.log('Code  :', new TextDecoder('ascii').decode(rom.subarray(0xAC, 0xB0)));
console.log('Save  :', emu.saveType);

// Optionally press a button at each step (helps games that gate boot on
// any key press, like the SEGA chime in some carts).
const pressEvery = process.env.PRESS_A_EVERY ? parseInt(process.env.PRESS_A_EVERY, 10) : 0;
const pressStartAt = process.env.PRESS_START_AT ? parseInt(process.env.PRESS_START_AT, 10) : -1;
const pressAOnceAt = process.env.PRESS_A_ONCE_AT ? parseInt(process.env.PRESS_A_ONCE_AT, 10) : -1;

const start = performance.now();
let pcSeen = new Set<number>();
let lastPc = 0;
// Drain sound output per frame (browser does this) so we can audit
// the actual production rate without the output buffer capping.
let totalSamplesProduced = 0;
// Optional: dump a snapshot every N frames.
const dumpEvery = process.env.DUMP_EVERY ? parseInt(process.env.DUMP_EVERY, 10) : 0;
for (let i = 0; i < frames; i++) {
  if (pressEvery && i % pressEvery === 0) emu.keypad.press(Key.A);
  if (pressEvery && i % pressEvery === pressEvery / 2) emu.keypad.release(Key.A);
  if (i === pressStartAt) emu.keypad.press(Key.START);
  if (i === pressStartAt + 4) emu.keypad.release(Key.START);
  if (i === pressAOnceAt) emu.keypad.press(Key.A);
  if (i === pressAOnceAt + 4) emu.keypad.release(Key.A);
  try {
    emu.runFrame();
  } catch (e) {
    console.error(`FAIL at frame ${i}:`, (e as Error).message);
    break;
  }
  // Match browser behavior: drain sound output after each frame so the
  // 2048-sample cap doesn't masquerade as a real rate measurement.
  if (i === 0) (globalThis as any).__cycStart = emu.cpu.cycles;
  totalSamplesProduced += emu.sound.outputLen;
  emu.sound.outputLen = 0;
  pcSeen.add(emu.cpu.state.r[15] & ~3);
  lastPc = emu.cpu.state.r[15];
  if (dumpEvery && i > 0 && i % dumpEvery === 0) {
    dumpPpu(`frame ${i.toString().padStart(5)}`);
    writePpm(outPath.replace(/\.ppm$/, `-f${i}.ppm`));
  }
}
const dt = performance.now() - start;
console.log(`Ran ${frames} frames in ${dt.toFixed(0)}ms (${(frames / (dt/1000)).toFixed(1)} fps emu)`);
console.log(`Final pc=0x${lastPc.toString(16)}  distinct top-of-frame pcs=${pcSeen.size}`);
console.log(`DISPCNT=0x${emu.ppu.dispcnt.toString(16)}  mode=${emu.ppu.dispcnt & 7}  bg-enables=${(emu.ppu.dispcnt >> 8) & 0x1F}`);

writePpm(outPath);
dumpPpu('final');
dumpSound();
if (process.env.AUDIO_AUDIT) {
  // Actual cycles per frame (emu's internal cycle counter advance).
  const cycStart = (globalThis as any).__cycStart ?? 0;
  const totalCycles = emu.cpu.cycles - cycStart;
  const cyclesPerFrame = totalCycles / frames;
  console.log(`  cycles per frame (actual) = ${cyclesPerFrame.toFixed(2)} (nominal 280896)`);
  // Verify the relationship between samples-produced-per-frame and the
  // reported sourceRate. If the sourceRate Web Audio plays them at
  // differs from the emulator's actual production rate, audio and
  // video drift apart over time.
  const samplesPerFrame = totalSamplesProduced / frames;
  const framesPerSec = 59.7275;
  const producedPerSec = samplesPerFrame * framesPerSec;
  const playRate = emu.sound.sampleRate;
  const drift = (producedPerSec - playRate) / playRate;
  console.log(`AUDIO AUDIT:`);
  console.log(`  frames run            = ${frames}`);
  console.log(`  samples produced total= ${totalSamplesProduced}`);
  console.log(`  samples per frame avg = ${samplesPerFrame.toFixed(2)}`);
  console.log(`  produced rate (Hz)    = ${producedPerSec.toFixed(2)}`);
  console.log(`  playback rate (Hz)    = ${playRate.toFixed(2)}`);
  console.log(`  drift (%/sec)         = ${(drift * 100).toFixed(4)}%`);
  console.log(`  drift over 219s video = ${(drift * 219).toFixed(3)} s`);
}
if (process.env.DUMP_LAYERS) dumpLayers();
if (process.env.DUMP_OAM_AT) dumpOam();

function dumpLayers() {
  // Render each enabled BG / OBJ in isolation by toggling DISPCNT bits
  // and re-rendering one scanline at a time. We don't have a public
  // "render this BG only" API, so instead: write screenshots with all
  // BGs visible (already done), then with just sprites, then with BG0
  // only, etc. This isolates whose pixels are wrong.
  const p = emu.ppu;
  const orig = p.dispcnt;
  const base = orig & ~0x1F00;
  const variants: [string, number][] = [
    ['bg0',  base | 0x0100],
    ['bg1',  base | 0x0200],
    ['bg2',  base | 0x0400],
    ['bg3',  base | 0x0800],
    ['obj',  base | 0x1000],
    ['none', base],
  ];
  for (const [name, dc] of variants) {
    p.dispcnt = dc;
    p.frameDone = false;
    emu.runFrame();
    writePpm(outPath.replace(/\.ppm$/, `-layer-${name}.ppm`));
  }
  p.dispcnt = orig;
}

function dumpOam() {
  const oam = emu.bus.oam;
  console.log(`OAM (visible only):`);
  let n = 0;
  for (let i = 0; i < 128; i++) {
    const o = i * 8;
    const a0 = oam[o] | (oam[o+1] << 8);
    const a1 = oam[o+2] | (oam[o+3] << 8);
    const a2 = oam[o+4] | (oam[o+5] << 8);
    const aff = (a0 & 0x100) !== 0;
    const disabled = !aff && (a0 & 0x200) !== 0;
    if (disabled) continue;
    const shape = (a0 >> 14) & 3;
    if (shape === 3) continue;
    const size = (a1 >> 14) & 3;
    let y = a0 & 0xFF; if (y >= 160) y -= 256;
    let x = a1 & 0x1FF; if (x >= 240) x -= 512;
    const mode = (a0 >> 10) & 3;
    const doubled = aff && (a0 & 0x200) !== 0;
    console.log(`  [${i.toString().padStart(3)}] a0=${a0.toString(16).padStart(4,'0')} a1=${a1.toString(16).padStart(4,'0')} a2=${a2.toString(16).padStart(4,'0')}  pos=(${x},${y}) shape=${shape}/${size} ${aff ? (doubled ? 'AFFx2' : 'AFF') : '   '}  mode=${['nrm','sa','win','---'][mode]}  tile=${a2 & 0x3FF} pal=${(a2>>12)&0xF} prio=${(a2>>10)&3}`);
    if (++n >= 16) { console.log(`  ... (${128 - i - 1} more entries)`); break; }
  }
}

function writePpm(path: string) {
  const W = 240, H = 160;
  const f = emu.ppu.frame;
  const header = `P6\n${W} ${H}\n255\n`;
  const body = Buffer.alloc(W * H * 3);
  for (let i = 0; i < W * H; i++) {
    body[i * 3 + 0] = f[i * 4 + 0];
    body[i * 3 + 1] = f[i * 4 + 1];
    body[i * 3 + 2] = f[i * 4 + 2];
  }
  writeFileSync(path, Buffer.concat([Buffer.from(header, 'ascii'), body]));
  const colors = new Set<number>();
  for (let i = 0; i < W * H; i++) {
    colors.add((f[i*4]<<16) | (f[i*4+1]<<8) | f[i*4+2]);
  }
  console.log(`  wrote ${path}  (${colors.size} colors)`);
}

function dumpSound() {
  const s = emu.sound;
  console.log(`SOUNDCNT_X=0x${s.soundcntX.toString(16)} (enable=${(s.soundcntX&0x80)?1:0}) SOUNDCNT_H=0x${s.soundcntH.toString(16)} A.timer=${(s.soundcntH>>10)&1} B.timer=${(s.soundcntH>>14)&1} sampleRate=${s.sampleRate.toFixed(1)} drained=${s.outputLen} fifoA.count=${s.countA} fifoB.count=${s.countB}`);
}

function dumpPpu(label: string) {
  const p = emu.ppu;
  const d = p.dispcnt;
  const mode = d & 7;
  const enables: string[] = [];
  if (d & 0x100) enables.push('BG0');
  if (d & 0x200) enables.push('BG1');
  if (d & 0x400) enables.push('BG2');
  if (d & 0x800) enables.push('BG3');
  if (d & 0x1000) enables.push('OBJ');
  const winStr = [
    (d & 0x2000) ? 'W0' : '',
    (d & 0x4000) ? 'W1' : '',
    (d & 0x8000) ? 'WO' : '',
  ].filter(Boolean).join('+') || '—';
  console.log(`[${label}] mode=${mode} enables=${enables.join('+') || 'none'} windows=${winStr} dispcnt=0x${d.toString(16).padStart(4,'0')} bldcnt=0x${p.bldcnt.toString(16)} bldy=0x${p.bldy.toString(16)} bldalpha=0x${p.bldalpha.toString(16)} mosaic=0x${p.mosaic.toString(16)}`);
  for (let b = 0; b < 4; b++) {
    const cnt = p.bgcnt[b];
    const prio = cnt & 3;
    const charBase = (cnt >> 2) & 3;
    const mosaicOn = !!(cnt & 0x40);
    const c8 = !!(cnt & 0x80);
    const screenBase = (cnt >> 8) & 0x1F;
    const wrap = !!(cnt & 0x2000);
    const size = (cnt >> 14) & 3;
    console.log(`  BG${b} cnt=0x${cnt.toString(16).padStart(4,'0')} prio=${prio} char=${charBase} screen=${screenBase} bpp=${c8?8:4} sz=${size} mosaic=${mosaicOn?1:0} wrap=${wrap?1:0} HOFS=${p.bgHOFS[b]} VOFS=${p.bgVOFS[b]}`);
  }
}
