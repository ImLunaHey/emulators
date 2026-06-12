// JIT lockstep divergence harness.
//
// Runs TWO emulators over the same ROM: A with the recompiler enabled,
// B interpreter-only (ground truth). A's dispatch loop is driven
// manually; every time A's recomp executes a block of N instructions,
// B is stepped exactly N times, then r0..r15 + CPSR are compared. On
// the first divergence we dump the block's start PC, its 16-bit
// opcodes, and both register files, then exit non-zero.
//
// IRQ skew is avoided by construction: the JIT only samples the IRQ
// line at block boundaries, so B's irqLine is asserted only on the
// FIRST interpreter step of each lockstep iteration — both cores then
// take IRQs at exactly the same instruction boundary.
//
// Usage:
//   npx tsx src/test/jit-lockstep.ts <rom> [frames]
//   npx tsx src/test/jit-lockstep.ts public/emerald.gba 600
import { readFileSync } from 'node:fs';
import { Emulator } from '../emulator';

const romPath = process.argv[2] ?? 'public/emerald.gba';
const frames = parseInt(process.argv[3] ?? '600', 10);
const rom = new Uint8Array(readFileSync(romPath));

const A = new Emulator();   // JIT
const B = new Emulator();   // interpreter ground truth
A.loadRom(rom);
B.loadRom(rom);
A.recomp.enabled = true;
B.recomp.enabled = false;

// Pin both RTCs to one boot-time wallclock read so a second-boundary
// tick between the two cores can't masquerade as a JIT bug.
const fixedNow = new Date();
for (const emu of [A, B]) {
  (emu.rtc as unknown as { now?: () => Date }).now = () => fixedNow;
}

const CYCLES_PER_FRAME = 280896;
let totalInsns = 0;
let jitBlocks = 0;
let lastBlockPc = -1;
let lastBlockN = 0;

function dumpState(tag: string, emu: Emulator) {
  const s = emu.cpu.state;
  const r = (i: number) => (s.r[i] >>> 0).toString(16).padStart(8, '0');
  console.log(`  ${tag}: r0-3  ${r(0)} ${r(1)} ${r(2)} ${r(3)}`);
  console.log(`  ${tag}: r4-7  ${r(4)} ${r(5)} ${r(6)} ${r(7)}`);
  console.log(`  ${tag}: r8-11 ${r(8)} ${r(9)} ${r(10)} ${r(11)}`);
  console.log(`  ${tag}: r12   ${r(12)} sp=${r(13)} lr=${r(14)} pc=${r(15)}`);
  console.log(`  ${tag}: cpsr  ${(s.cpsr >>> 0).toString(16).padStart(8, '0')}  halted=${s.halted}`);
}

function compare(): boolean {
  const sa = A.cpu.state, sb = B.cpu.state;
  for (let i = 0; i < 16; i++) {
    if ((sa.r[i] >>> 0) !== (sb.r[i] >>> 0)) return false;
  }
  if ((sa.cpsr >>> 0) !== (sb.cpsr >>> 0)) return false;
  if (sa.halted !== sb.halted) return false;
  return true;
}

function reportDivergence(frame: number): never {
  console.log(`\nDIVERGENCE at frame ${frame}, after ${totalInsns} insns, ${jitBlocks} jit blocks`);
  if (lastBlockPc >= 0) {
    console.log(`Last JIT block: startPc=0x${lastBlockPc.toString(16)}  insns=${lastBlockN}`);
    console.log(`Block opcodes:`);
    for (let i = 0; i < lastBlockN; i++) {
      const addr = (lastBlockPc + i * 2) >>> 0;
      const op = B.bus.read16(addr);
      console.log(`  0x${addr.toString(16).padStart(8, '0')}: 0x${op.toString(16).padStart(4, '0')}`);
    }
  } else {
    console.log(`Last step was interpreter-on-both (no JIT block).`);
  }
  dumpState('A(jit)', A);
  dumpState('B(int)', B);
  const sa = A.cpu.state, sb = B.cpu.state;
  for (let i = 0; i < 16; i++) {
    if ((sa.r[i] >>> 0) !== (sb.r[i] >>> 0)) {
      console.log(`  r${i}: A=0x${(sa.r[i] >>> 0).toString(16)}  B=0x${(sb.r[i] >>> 0).toString(16)}`);
    }
  }
  if ((sa.cpsr >>> 0) !== (sb.cpsr >>> 0)) {
    console.log(`  cpsr: A=0x${(sa.cpsr >>> 0).toString(16)}  B=0x${(sb.cpsr >>> 0).toString(16)}`);
  }
  process.exit(1);
}

// Step a single emulator's peripherals by n executed cycles.
function stepPeriph(emu: Emulator, n: number) {
  emu.ppu.step(n);
  emu.timers.step(n);
  emu.io.sio.step(n);
}

outer:
for (let frame = 0; frame < frames; frame++) {
  let executed = 0;
  while (executed < CYCLES_PER_FRAME) {
    // Both cores sample the IRQ line at this (block) boundary only.
    A.cpu.irqLine = A.irq.cachedPending;
    B.cpu.irqLine = B.irq.cachedPending;

    let n: number;
    const jitN = A.recomp.tryDispatch();
    if (jitN > 0) {
      lastBlockPc = (B.cpu.state.r[15] & ~1) >>> 0;
      lastBlockN = jitN;
      jitBlocks++;
      n = jitN;
      // B replays the same N instructions; IRQ only on the first.
      for (let k = 0; k < jitN; k++) {
        B.cpu.step();
        if (k === 0) B.cpu.irqLine = false;
      }
    } else {
      lastBlockPc = -1;
      A.cpu.step();
      B.cpu.step();
      n = 1;
    }
    totalInsns += n;

    if (!compare()) reportDivergence(frame);

    stepPeriph(A, n);
    stepPeriph(B, n);
    executed += n;
    if (A.ppu.frameDone !== B.ppu.frameDone) {
      console.log(`frameDone skew at frame ${frame}`);
      reportDivergence(frame);
    }
    if (A.ppu.frameDone) { A.ppu.frameDone = false; B.ppu.frameDone = false; break; }

    // Both halted: burn cycles in lockstep until an IRQ wakes them.
    // (cpu.step already handles halted as a 1-cycle no-op.)
  }
  A.bus.iwram[0x7FF8] |= 0x01;
  B.bus.iwram[0x7FF8] |= 0x01;
  if (frame % 60 === 0) {
    console.log(`frame ${frame}  insns=${totalInsns}  jitBlocks=${jitBlocks}  pc=0x${(A.cpu.state.r[15] >>> 0).toString(16)}`);
  }
}

console.log(`\nCLEAN: ${frames} frames, ${totalInsns} insns, ${jitBlocks} jit blocks — no divergence.`);
