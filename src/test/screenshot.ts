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

// Trace timer-control writes to see if the game uses hardware timers
// for game-state pacing (which would run too fast/slow if our cycle
// counting is off relative to runFrame).
if (process.env.TRACE_TIMERS) {
  const origW = emu.io.write16.bind(emu.io);
  const seen = new Map<number, number>();
  emu.io.write16 = (addr: number, v: number) => {
    const off = addr & 0x3FF;
    // TM0CNT_L..TM3CNT_H at 0x100..0x10F
    if (off >= 0x100 && off <= 0x10F) {
      const key = (off << 16) | v;
      seen.set(key, (seen.get(key) ?? 0) + 1);
    }
    origW(addr, v);
  };
  process.on('exit', () => {
    console.log('Timer writes:');
    for (const [key, n] of Array.from(seen.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 16)) {
      const off = key >>> 16;
      const v = key & 0xFFFF;
      console.log(`  0x040000${off.toString(16).padStart(2,'0')} <- 0x${v.toString(16).padStart(4,'0')}  x${n}`);
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
