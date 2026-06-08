// Boot every ROM in public/ for N frames, snapshot the framebuffer,
// emit a one-line summary per ROM. Used to quickly survey what works
// after a change. Set BATCH_FRAMES=N to override the 600-frame (10 s)
// default; ROMs are processed sequentially so a single crash doesn't
// kill the rest.

import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { Emulator } from '../emulator';

const dir = process.argv[2] ?? 'public';
const frames = parseInt(process.env.BATCH_FRAMES ?? '600', 10);
const outDir = process.argv[3] ?? '/tmp/batch';

const roms = readdirSync(dir)
  .filter((f) => f.toLowerCase().endsWith('.gba'))
  .sort();

console.log(`# Batch boot — ${roms.length} ROMs · ${frames} frames each (~${(frames/60).toFixed(1)}s emu time)`);
console.log();

interface Result {
  name: string;
  title: string;
  code: string;
  size: number;
  saveType: string;
  frames: number;
  ok: boolean;
  err?: string;
  lastPc: number;
  pcCount: number;
  dispcnt: number;
  mode: number;
  bgEnables: number;
  colors: number;
  wallMs: number;
}

const results: Result[] = [];

for (const f of roms) {
  const path = `${dir}/${f}`;
  const rom = new Uint8Array(readFileSync(path));
  const emu = new Emulator();
  let err: string | undefined;
  let ranFrames = 0;
  const pcs = new Set<number>();
  let lastPc = 0;
  const t0 = performance.now();
  try {
    emu.loadRom(rom);
    for (let i = 0; i < frames; i++) {
      emu.runFrame();
      ranFrames++;
      pcs.add(emu.cpu.state.r[15] & ~3);
      lastPc = emu.cpu.state.r[15];
    }
  } catch (e) {
    err = (e as Error).message;
  }
  const wallMs = performance.now() - t0;

  // Snapshot the framebuffer as a PPM in outDir.
  const W = 240, H = 160;
  const fb = emu.ppu.frame;
  const header = `P6\n${W} ${H}\n255\n`;
  const body = Buffer.alloc(W * H * 3);
  for (let i = 0; i < W * H; i++) {
    body[i * 3] = fb[i * 4];
    body[i * 3 + 1] = fb[i * 4 + 1];
    body[i * 3 + 2] = fb[i * 4 + 2];
  }
  const stem = f.replace(/\.gba$/i, '');
  const ppmPath = `${outDir}/${stem}.ppm`;
  try {
    writeFileSync(ppmPath, Buffer.concat([Buffer.from(header, 'ascii'), body]));
  } catch { /* outDir might not exist; caller pre-mkdirs */ }

  const colors = new Set<number>();
  for (let i = 0; i < W * H; i++) {
    colors.add((fb[i*4]<<16) | (fb[i*4+1]<<8) | fb[i*4+2]);
  }

  const title = new TextDecoder('ascii').decode(rom.subarray(0xA0, 0xAC)).replace(/[\0\x01-\x1F]/g, '').trim();
  const code = new TextDecoder('ascii').decode(rom.subarray(0xAC, 0xB0));

  results.push({
    name: f,
    title,
    code,
    size: rom.length,
    saveType: emu.saveType,
    frames: ranFrames,
    ok: !err,
    err,
    lastPc,
    pcCount: pcs.size,
    dispcnt: emu.ppu.dispcnt,
    mode: emu.ppu.dispcnt & 7,
    bgEnables: (emu.ppu.dispcnt >> 8) & 0x1F,
    colors: colors.size,
    wallMs,
  });
}

// Pretty table.
console.log('| ROM                          | Code | Size | Save     | Frames | PCs |  Mode | BGs    | Colors |  Wall | OK |');
console.log('|------------------------------|------|------|----------|--------|-----|-------|--------|--------|-------|----|');
for (const r of results) {
  const sizeMb = (r.size / (1024*1024)).toFixed(0) + 'M';
  const bgs = ['BG0', 'BG1', 'BG2', 'BG3', 'OBJ']
    .filter((_, i) => r.bgEnables & (1 << i))
    .join('+') || '—';
  console.log(`| ${r.name.slice(0, 28).padEnd(28)} | ${r.code} | ${sizeMb.padStart(4)} | ${r.saveType.padEnd(8)} | ${r.frames.toString().padStart(6)} | ${r.pcCount.toString().padStart(3)} | ${r.mode.toString().padStart(5)} | ${bgs.padEnd(6)} | ${r.colors.toString().padStart(6)} | ${(r.wallMs|0).toString().padStart(4)}ms | ${r.ok ? '✓' : '✗'} |`);
}

const failed = results.filter((r) => !r.ok);
if (failed.length) {
  console.log();
  console.log(`# ${failed.length} ROM${failed.length === 1 ? '' : 's'} threw during boot:`);
  for (const r of failed) console.log(`  ${r.name}: ${r.err}`);
}
