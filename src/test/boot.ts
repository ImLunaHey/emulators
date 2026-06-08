// Headless boot smoke test — load the ROM from disk and run for a few
// frames, printing CPU state so we can see if it makes forward progress.
import { readFileSync } from 'node:fs';
import { Emulator } from '../emulator';

const path = process.argv[2] ?? 'public/firered.gba';
const rom = new Uint8Array(readFileSync(path));
if (process.env.TRACE_CPUSET) (globalThis as any).__traceCpuSet = true;
if (process.env.TRACE_BL) {
  const seen = new Map<string, number>();
  (globalThis as any).__traceBL = (from: number, to: number) => {
    const key = `${from.toString(16)}→${to.toString(16)}`;
    seen.set(key, (seen.get(key) || 0) + 1);
  };
  process.on('exit', () => {
    const sorted = Array.from(seen.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 100);
    for (const [key, n] of sorted) console.log(`  BL ${key}  ×${n}`);
  });
}
// Find all BL targets that match a watchlist.
if (process.env.WATCH_BL) {
  const watch = new Set<number>(process.env.WATCH_BL.split(',').map(s => parseInt(s, 16) >>> 0));
  let hits = 0;
  (globalThis as any).__traceBL = (from: number, to: number) => {
    if (watch.has(to >>> 0) && hits < 30) {
      console.log(`  WATCH BL 0x${from.toString(16)} -> 0x${to.toString(16)}`);
      hits++;
    }
  };
}
const emu = new Emulator();
emu.loadRom(rom);

// Trace PPU IO + DISPCNT/DISPSTAT writes.
const ppu = emu.ppu;
const origWriteReg = ppu.writeReg.bind(ppu);
ppu.writeReg = (a: number, v: number) => {
  if (a === 0x00 || a === 0x04) console.log(`  PPU[${a.toString(16).padStart(2,'0')}] <- 0x${v.toString(16).padStart(4,'0')}  pc=0x${emu.cpu.state.r[15].toString(16)}`);
  origWriteReg(a, v);
};
// Trace IO writes to DISPCNT/DISPSTAT/IE/IME only.
const origIoWrite16 = emu.io.write16.bind(emu.io);
emu.io.write16 = (addr: number, v: number) => {
  const off = addr & 0x3FF;
  if (off === 0x000 || off === 0x004 || off === 0x200 || off === 0x208) {
    // console.log(`  IO[0x${off.toString(16).padStart(3,'0')}] <- 0x${v.toString(16).padStart(4,'0')}  pc=0x${emu.cpu.state.r[15].toString(16)}`);
  }
  origIoWrite16(addr, v);
};
// Trace writes near state byte 0x03003FB1 (BootInitWindow state)
let stateWrites = 0;
const origBusWrite8 = emu.bus.write8.bind(emu.bus);
emu.bus.write8 = (addr: number, v: number) => {
  if ((addr >>> 0) === 0x03003FB1 && stateWrites < 30) {
    console.log(`  STATE[0x03003FB1] <- 0x${v.toString(16)}  pc=0x${emu.cpu.state.r[15].toString(16)}`);
    stateWrites++;
  }
  origBusWrite8(addr, v);
};
const origBusWrite16 = emu.bus.write16.bind(emu.bus);
emu.bus.write16 = (addr: number, v: number) => {
  if ((addr >>> 0) === 0x03003FB1 && stateWrites < 30) {
    console.log(`  STATE16[0x03003FB1] <- 0x${v.toString(16)}  pc=0x${emu.cpu.state.r[15].toString(16)}`);
    stateWrites++;
  }
  origBusWrite16(addr, v);
};
// Track vcount reads + IRQ raises.
let vcountReads = 0;
let lastVcountRead = -1;
const origIoRead16 = emu.io.read16.bind(emu.io);
emu.io.read16 = (addr: number) => {
  const v = origIoRead16(addr);
  if ((addr & 0x3FF) === 0x006) { vcountReads++; lastVcountRead = v; }
  return v;
};
let irqRaises = 0;
const origRaise = emu.irq.raise.bind(emu.irq);
emu.irq.raise = (bits: number) => { irqRaises++; origRaise(bits); };
let irqEntries = 0;
const irqIfHist = new Map<number, number>();
const origTakeIrq = emu.cpu.takeIrq.bind(emu.cpu);
emu.cpu.takeIrq = () => {
  irqEntries++;
  const ifv = emu.irq.iflag;
  irqIfHist.set(ifv, (irqIfHist.get(ifv) || 0) + 1);
  origTakeIrq();
};
const swiCounts = new Map<number, number>();
const origSwi = emu.cpu.softwareInterrupt.bind(emu.cpu);
emu.cpu.softwareInterrupt = (n: number) => {
  swiCounts.set(n, (swiCounts.get(n) || 0) + 1);
  origSwi(n);
};

console.log('Header title:', new TextDecoder('ascii').decode(rom.subarray(0xA0, 0xAC)).replace(/\0/g, ''));
console.log('Game code  :', new TextDecoder('ascii').decode(rom.subarray(0xAC, 0xB0)));
console.log('Maker code :', new TextDecoder('ascii').decode(rom.subarray(0xB0, 0xB2)));

const frames = parseInt(process.argv[3] ?? '60', 10);
console.log(`Running ${frames} frames…`);

// Optionally press a key for the duration to see if it advances.
import { Key } from '../io/keypad';
if (process.env.PRESS_START) emu.keypad.press(Key.START);
if (process.env.PRESS_A) emu.keypad.press(Key.A);

let lastPc = 0;
const start = performance.now();
for (let i = 0; i < frames; i++) {
  try {
    const r = emu.runFrame();
    if (i < 5 || i === frames - 1) {
      const s = emu.cpu.state;
      console.log(
        `frame ${i.toString().padStart(3)}  pc=${s.r[15].toString(16).padStart(8, '0')}  mode=${s.mode().toString(16)}  thumb=${(s.cpsr & 0x20) ? 1 : 0}  sp=${s.r[13].toString(16).padStart(8, '0')}  halted=${s.halted}  cpsr=${s.cpsr.toString(16)}  interp=${r.interp}  jit=${r.jit}`
      );
    }
    lastPc = emu.cpu.state.r[15];
  } catch (e) {
    console.error(`FAILED at frame ${i}:`, (e as Error).stack);
    process.exit(1);
  }
}
const dt = performance.now() - start;
console.log(`OK — ${frames} frames in ${dt.toFixed(0)}ms  (last pc=${lastPc.toString(16)})`);
console.log(`vcountReads=${vcountReads}  lastValue=${lastVcountRead}  irqRaises=${irqRaises}  irqEntries=${irqEntries}`);

// Step-by-step trace of the first N instructions (when TRACE_STEPS env is set).
if (process.env.TRACE_STEPS) {
  const steps = parseInt(process.env.TRACE_STEPS, 10) || 30;
  console.log(`\nStep-by-step trace (${steps} steps from start):`);
  emu.loadRom(rom);  // reset
  for (let i = 0; i < steps; i++) {
    const s = emu.cpu.state;
    const isT = (s.cpsr & 0x20) !== 0;
    const decode = s.r[15] & (isT ? ~1 : ~3);
    const insn = isT ? emu.bus.read16(decode) : emu.bus.read32(decode);
    console.log(`  ${i.toString().padStart(3)}  pc=0x${decode.toString(16).padStart(8,'0')}  ${isT ? 'T' : 'A'}  insn=0x${insn.toString(16).padStart(isT ? 4 : 8, '0')}  r0=${s.r[0].toString(16)} r1=${s.r[1].toString(16)} r12=${s.r[12].toString(16)} sp=${s.r[13].toString(16)}`);
    emu.cpu.step();
  }
  process.exit(0);
}
// PC histogram during the next 10 frames — finer bucket size.
console.log('\nPC histogram (next 10 frames, 16-byte buckets):');
const pcCount = new Map<number, number>();
for (let f = 0; f < 10; f++) {
  let executed = 0;
  while (executed < 280896) {
    emu.cpu.irqLine = emu.irq.pending();
    let cycles: number;
    if (emu.recomp.tryDispatch()) cycles = 1;
    else cycles = emu.cpu.step();
    emu.ppu.step(cycles);
    emu.timers.step(cycles);
    executed += cycles;
    if (executed % 200 === 0) {
      const pcBucket = emu.cpu.state.r[15] & ~0xF;
      pcCount.set(pcBucket, (pcCount.get(pcBucket) || 0) + 1);
    }
    if (emu.ppu.frameDone) { emu.ppu.frameDone = false; break; }
  }
  if (!(emu.ppu.dispstat & 0x10)) emu.irq.iflag &= ~0x2;
}
const sorted = Array.from(pcCount.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 30);
for (const [pc, count] of sorted) {
  console.log(`  pc 0x${pc.toString(16).padStart(8,'0')}  ${count}`);
}

// User IRQ handler pointer at 0x03007FFC (game writes its handler here).
const iwramAt = (off: number) =>
  emu.bus.iwram[off] | (emu.bus.iwram[off+1]<<8) | (emu.bus.iwram[off+2]<<16) | (emu.bus.iwram[off+3]<<24);
console.log(`User IRQ vector @ IWRAM[0x7FFC] = ${(iwramAt(0x7FFC) >>> 0).toString(16)}`);
console.log(`IntrCheck       @ IWRAM[0x7FF8] = ${(iwramAt(0x7FF8) >>> 0).toString(16)}`);
console.log(`HBLANK Vector   @ IWRAM[0x7FF4] = ${(iwramAt(0x7FF4) >>> 0).toString(16)}`);

// Sample a chunk of VRAM to see if anything was written.
const vramNonZero = Array.from(emu.bus.vram).reduce((a, b) => a + (b ? 1 : 0), 0);
const pramNonZero = Array.from(emu.bus.pram).reduce((a, b) => a + (b ? 1 : 0), 0);
const oamNonZero = Array.from(emu.bus.oam).reduce((a, b) => a + (b ? 1 : 0), 0);
const ewramNonZero = Array.from(emu.bus.ewram).reduce((a, b) => a + (b ? 1 : 0), 0);
console.log(`Non-zero bytes  VRAM=${vramNonZero}/${emu.bus.vram.length}  PRAM=${pramNonZero}  OAM=${oamNonZero}  EWRAM=${ewramNonZero}`);
console.log(`DISPCNT=${emu.ppu.dispcnt.toString(16)}  DISPSTAT=${emu.ppu.dispstat.toString(16)}  VCOUNT=${emu.ppu.vcount}  IE=${emu.irq.ie.toString(16)}  IF=${emu.irq.iflag.toString(16)}  IME=${emu.irq.ime}`);
console.log(`IWRAM[310C-310F] (wait flag) = ${[0x310C,0x310D,0x310E,0x310F].map(o=>emu.bus.iwram[o].toString(16)).join(' ')}`);
console.log(`SWI counts:`, Array.from(swiCounts.entries()).map(([k,v]) => `0x${k.toString(16)}=${v}`).join(' '));
console.log(`IRQ entry IF distribution:`, Array.from(irqIfHist.entries()).map(([k,v]) => `0x${k.toString(16)}=${v}`).join(' '));
// Dump anything non-zero in EWRAM beyond the bare state area.
let ewramRanges = '';
let inRange = false;
let rangeStart = 0;
for (let i = 0; i < emu.bus.ewram.length; i++) {
  if (emu.bus.ewram[i] !== 0) {
    if (!inRange) { rangeStart = i; inRange = true; }
  } else if (inRange) {
    inRange = false;
    if (i - rangeStart >= 4) ewramRanges += ` 0x${(0x02000000 + rangeStart).toString(16)}..0x${(0x02000000 + i - 1).toString(16)}`;
  }
}
console.log(`Non-zero EWRAM ranges (>=4 bytes):${ewramRanges || ' (none)'}`);
console.log(`EWRAM[0..0x10]: ${Array.from(emu.bus.ewram.slice(0, 16)).map(b => b.toString(16).padStart(2,'0')).join(' ')}`);
// Check first init function output: should set IWRAM[0x60..0xBF] to 0xFF.
const iwramAtRange = (s: number, e: number) =>
  Array.from(emu.bus.iwram.slice(s, e)).map(b => b.toString(16).padStart(2,'0')).join(' ');
console.log(`IWRAM[0x60..0x70]: ${iwramAtRange(0x60, 0x70)}`);
console.log(`IWRAM[0xB0..0xC0]: ${iwramAtRange(0xB0, 0xC0)}`);
console.log(`IWRAM[0xC0..0xD0]: ${iwramAtRange(0xC0, 0xD0)}`);

// Sample frame buffer pixels.
const f = emu.ppu.frame;
// Dump state at 0x03003528 (callback state byte)
console.log(`State byte @ 0x03003528 = 0x${emu.bus.iwram[0x3528].toString(16)}`);
console.log(`Init byte @ 0x03003F3C = 0x${emu.bus.iwram[0x3F3C].toString(16)}`);
console.log(`Init byte @ 0x03003F84 = 0x${emu.bus.iwram[0x3F84].toString(16)}`);
console.log(`Init byte @ 0x03003F64 = 0x${emu.bus.iwram[0x3F64].toString(16)}`);
console.log(`BootInit state @ 0x03003FB1 = 0x${emu.bus.iwram[0x3FB1].toString(16)}`);
console.log(`E14 state @ 0x03005E14 = 0x${emu.bus.iwram[0x5E14].toString(16)}`);
console.log(`E17 @ 0x03005E17 = 0x${emu.bus.iwram[0x5E17].toString(16)}`);
console.log(`Bytes 0x03003520..0x03003540: ${Array.from(emu.bus.iwram.slice(0x3520, 0x3540)).map(b => b.toString(16).padStart(2,'0')).join(' ')}`);
// Dump gMain struct at IWRAM 0x30F0..0x3120
console.log(`gMain @ 0x030030F0..0x03003130:`);
for (let i = 0; i < 8; i++) {
  const off = 0x30F0 + i * 8;
  const b = Array.from(emu.bus.iwram.slice(off, off + 8)).map(b => b.toString(16).padStart(2, '0')).join(' ');
  console.log(`  0x0300${off.toString(16).padStart(4,'0')}: ${b}`);
}
// Dump CPU registers + walk a likely return-address chain from SP.
const sr = emu.cpu.state;
console.log(`Regs: R0=${sr.r[0].toString(16)} R1=${sr.r[1].toString(16)} R2=${sr.r[2].toString(16)} R3=${sr.r[3].toString(16)}`);
console.log(`      R4=${sr.r[4].toString(16)} R5=${sr.r[5].toString(16)} R6=${sr.r[6].toString(16)} R7=${sr.r[7].toString(16)}`);
console.log(`      R8=${sr.r[8].toString(16)} R9=${sr.r[9].toString(16)} R10=${sr.r[10].toString(16)} R11=${sr.r[11].toString(16)}`);
console.log(`      R12=${sr.r[12].toString(16)} SP=${sr.r[13].toString(16)} LR=${sr.r[14].toString(16)} PC=${sr.r[15].toString(16)}`);
console.log(`Stack dump from SP:`);
const sp = sr.r[13];
for (let i = 0; i < 16; i++) {
  const a = sp + i * 4;
  const off = a & 0x7FFF;
  const v = (emu.bus.iwram[off] | (emu.bus.iwram[off+1]<<8) | (emu.bus.iwram[off+2]<<16) | (emu.bus.iwram[off+3]<<24)) >>> 0;
  const looksLikeCode = (v & 0xFF000000) === 0x08000000;
  console.log(`  [SP+${(i*4).toString(16).padStart(3,'0')}] = 0x${v.toString(16).padStart(8,'0')}${looksLikeCode ? ' ← ROM' : ''}`);
}
console.log(`PRAM[0..7] bytes: ${Array.from(emu.bus.pram.slice(0, 8)).map(b => b.toString(16).padStart(2,'0')).join(' ')}`);
console.log(`Frame buffer first 8 px (RGBA):`);
for (let i = 0; i < 8; i++) {
  console.log(`  px ${i}: r=${f[i*4]} g=${f[i*4+1]} b=${f[i*4+2]} a=${f[i*4+3]}`);
}
// Count distinct colors
const colors = new Set<number>();
for (let i = 0; i < 240*160; i++) {
  colors.add((f[i*4]<<24) | (f[i*4+1]<<16) | (f[i*4+2]<<8) | f[i*4+3]);
}
console.log(`Distinct frame colors: ${colors.size}`);
for (const c of Array.from(colors).slice(0, 8)) {
  console.log(`  color 0x${(c >>> 0).toString(16).padStart(8,'0')}`);
}

// Dump first 32 bytes of the user IRQ handler in IWRAM.
const handlerAddr = (emu.bus.iwram[0x7FFC] | (emu.bus.iwram[0x7FFD]<<8) | (emu.bus.iwram[0x7FFE]<<16) | (emu.bus.iwram[0x7FFF]<<24)) >>> 0;
console.log(`\nUser IRQ handler at 0x${handlerAddr.toString(16)}:`);
if ((handlerAddr & 0xFF000000) === 0x03000000) {
  const base = handlerAddr & 0x7FFF;
  for (let i = 0; i < 512; i += 4) {
    const v = (emu.bus.iwram[base+i] | (emu.bus.iwram[base+i+1]<<8) | (emu.bus.iwram[base+i+2]<<16) | (emu.bus.iwram[base+i+3]<<24)) >>> 0;
    console.log(`  ${(handlerAddr+i).toString(16)}: 0x${v.toString(16).padStart(8,'0')}`);
  }
}
// Dump IntrFunc table at 0x03007438
console.log(`\nIntrFunc table @ 0x03007438:`);
for (let i = 0; i < 14; i++) {
  const off = 0x7438 + i * 4;
  const v = (emu.bus.iwram[off] | (emu.bus.iwram[off+1]<<8) | (emu.bus.iwram[off+2]<<16) | (emu.bus.iwram[off+3]<<24)) >>> 0;
  console.log(`  [${i}] = 0x${v.toString(16).padStart(8,'0')}`);
}
// VBlank handler at IntrFunc[4] — dump its code in IWRAM
const vblankAddr = (emu.bus.iwram[0x7438 + 4*4] | (emu.bus.iwram[0x7438 + 4*4 + 1]<<8) | (emu.bus.iwram[0x7438 + 4*4 + 2]<<16) | (emu.bus.iwram[0x7438 + 4*4 + 3]<<24)) >>> 0;
console.log(`\nVBlank handler @ 0x${vblankAddr.toString(16)}:`);
if ((vblankAddr & 0xFF000000) === 0x03000000) {
  const base = vblankAddr & 0x7FFF;
  for (let i = 0; i < 64; i += 4) {
    const v = (emu.bus.iwram[base+i] | (emu.bus.iwram[base+i+1]<<8) | (emu.bus.iwram[base+i+2]<<16) | (emu.bus.iwram[base+i+3]<<24)) >>> 0;
    console.log(`  ${(vblankAddr+i).toString(16)}: 0x${v.toString(16).padStart(8,'0')}`);
  }
}
