// Headless boot smoke test — load the ROM from disk and run for a few
// frames, printing CPU state so we can see if it makes forward progress.
import { readFileSync } from 'node:fs';
import { Emulator } from '../emulator';

const path = process.argv[2] ?? 'public/firered.gba';
const rom = new Uint8Array(readFileSync(path));
if (process.env.TRACE_CPUSET) (globalThis as any).__traceCpuSet = true;
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
// Trace writes to wait flag 0x0300310C
let waitFlagWrites = 0;
const origBusWrite16 = emu.bus.write16.bind(emu.bus);
emu.bus.write16 = (addr: number, v: number) => {
  if ((addr >>> 0) === 0x0300310C && waitFlagWrites < 30) {
    console.log(`  [0x0300310C] <- 0x${v.toString(16).padStart(4,'0')}  pc=0x${emu.cpu.state.r[15].toString(16)}`);
    waitFlagWrites++;
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

// PC histogram during the next 10 frames.
console.log('\nPC histogram (next 10 frames):');
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
    if (executed % 1000 === 0) {
      const pcBucket = emu.cpu.state.r[15] & ~0xFF;
      pcCount.set(pcBucket, (pcCount.get(pcBucket) || 0) + 1);
    }
    if (emu.ppu.frameDone) { emu.ppu.frameDone = false; break; }
  }
  if (!(emu.ppu.dispstat & 0x10)) emu.irq.iflag &= ~0x2;
  emu.bus.iwram[0x310C] |= 0x01;
}
const sorted = Array.from(pcCount.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 20);
for (const [pc, count] of sorted) {
  console.log(`  pc 0x${pc.toString(16)}xx  ${count}`);
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

// Sample frame buffer pixels.
const f = emu.ppu.frame;
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
  for (let i = 0; i < 256; i += 4) {
    const v = (emu.bus.iwram[base+i] | (emu.bus.iwram[base+i+1]<<8) | (emu.bus.iwram[base+i+2]<<16) | (emu.bus.iwram[base+i+3]<<24)) >>> 0;
    console.log(`  ${(handlerAddr+i).toString(16)}: 0x${v.toString(16).padStart(8,'0')}`);
  }
}
