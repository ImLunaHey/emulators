// Micro-benchmark comparing the THUMB interpreter against the JIT on a
// short hot block of Format-3 ALU + Format-9 store + Format-18 branch.
// The block branches back to its own start so once it's compiled the
// dispatcher reuses the same translation forever.
//
// Run: npx vitest run src/test/recomp.bench.ts

import { describe, it, expect } from 'vitest';
import { Emulator } from '../emulator';

function placeInsns(emu: Emulator, addr: number, insns: number[]) {
  for (let i = 0; i < insns.length; i++) {
    emu.bus.write16(addr + i * 2, insns[i] & 0xFFFF);
  }
}

function initThumb(emu: Emulator, startPc: number) {
  emu.cpu.state.cpsr = 0x1F | 0x20;
  emu.cpu.state.r[15] = startPc | 1;
  emu.cpu.state.r[13] = 0x03007F00;
}

describe('Recompiler micro-benchmark', () => {
  it('JIT beats interpreter on a hot ALU loop', () => {
    // Block of pure-ALU Format 3 instructions, looping. The branch at
    // the end takes us back to the start so the dispatcher keeps re-
    // entering the same compiled block.
    //   MOV R0, #0
    //   ADD R0, #1   (x 8)
    //   SUB R0, #1   (x 8)
    //   B  -36       (jump back to MOV)
    const pc = 0x03002000;
    const insns: number[] = [];
    insns.push(0x2000);                       // MOV R0, #0
    for (let i = 0; i < 8; i++) insns.push(0x3001); // ADD R0, #1
    for (let i = 0; i < 8; i++) insns.push(0x3801); // SUB R0, #1
    // B label — Format 18, 11-bit signed offset. We want target = pc.
    // Branch insn is at index 17, decode addr = pc + 34. PC+4 at decode
    // = pc + 38. We want target = pc. So off (signed, in halfwords) =
    // (pc - (pc+38)) / 2 = -19 = 0x7ED (two's complement 11-bit).
    insns.push(0xE000 | (0x7ED & 0x7FF));
    // -- interpreter run
    const interp = (() => {
      const emu = new Emulator();
      emu.loadRom(new Uint8Array(0x100));
      placeInsns(emu, pc, insns);
      initThumb(emu, pc);
      const N = 200_000;
      const t0 = performance.now();
      for (let i = 0; i < N; i++) emu.cpu.step();
      const dt = performance.now() - t0;
      return { ns: (dt * 1e6) / N, r0: emu.cpu.state.r[0] };
    })();
    // -- JIT run
    const jit = (() => {
      const emu = new Emulator();
      emu.loadRom(new Uint8Array(0x100));
      placeInsns(emu, pc, insns);
      initThumb(emu, pc);
      emu.recomp.enabled = true;
      // Prime the cache by force-compiling the start PC.
      (emu.recomp as any).hits.set(pc, 1000);
      const N = 200_000;
      let executed = 0;
      const t0 = performance.now();
      while (executed < N) {
        if (emu.recomp.tryDispatch()) {
          // tryDispatch executed one block (insns.length insns)
          executed += insns.length;
        } else {
          emu.cpu.step();
          executed++;
        }
      }
      const dt = performance.now() - t0;
      return { ns: (dt * 1e6) / executed, r0: emu.cpu.state.r[0] };
    })();
    /* eslint-disable no-console */
    console.log(`interpreter: ${interp.ns.toFixed(1)} ns/insn   r[0]=${interp.r0}`);
    console.log(`        jit: ${jit.ns.toFixed(1)} ns/insn   r[0]=${jit.r0}`);
    console.log(`speedup    : ${(interp.ns / jit.ns).toFixed(2)}×`);
    /* eslint-enable no-console */
    // recomp.test.ts already proves the two paths agree on register
    // state; we don't re-check it here because the interpreter stops
    // mid-iteration at exactly N insns while the JIT always finishes
    // the current basic block, so R0 legitimately differs at the
    // stop boundary. This test only measures wall-clock per insn.
    expect(jit.ns).toBeLessThan(interp.ns);
  });
});
