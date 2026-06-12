// Recompiler smoke test. Loads a known THUMB instruction sequence into
// IWRAM, enables the JIT, forces compile (no profile threshold), then
// runs the block and checks that the CPU state matches what the
// interpreter would have produced.

import { describe, it, expect } from 'vitest';
import { Emulator } from '../emulator';

function placeInsns(emu: Emulator, addr: number, insns: number[]) {
  for (let i = 0; i < insns.length; i++) {
    emu.bus.write16(addr + i * 2, insns[i] & 0xFFFF);
  }
  // Terminate the block with an instruction the JIT can't compile
  // (PUSH {R0, LR}, Format 14). Zero-filled memory used to do this
  // implicitly, but 0x0000 decodes to LSL R0, R0, #0 — a valid Format
  // 1 shift the JIT now supports — so without a sentinel the compiler
  // would run straight past the test sequence up to MAX_BLOCK_INSNS.
  // Never executed: the interpreter steps exactly insns.length times
  // and compiled blocks stop just before it.
  emu.bus.write16(addr + insns.length * 2, 0xB501);
}

function initThumb(emu: Emulator, startPc: number) {
  emu.cpu.state.cpsr = 0x1F | 0x20;          // SYS mode + THUMB
  emu.cpu.state.r[15] = startPc | 1;           // THUMB low bit
  emu.cpu.state.r[13] = 0x03007F00;            // SP
}

// Differential harness: run `insns` through a JIT-forced emulator and
// a pure-interpreter emulator, then compare ALL of r0..r15 + cpsr.
// `setup` runs on both instances before execution (set registers etc.).
// `verify` runs after the register compare with both instances — use it
// to assert that stores landed where the interpreter put them.
function runDiff(
  insns: number[],
  setup?: (emu: Emulator) => void,
  verify?: (emu: Emulator, ref: Emulator) => void,
) {
  const pc = 0x03002000;

  const ref = new Emulator();
  ref.loadRom(new Uint8Array(0x100));
  placeInsns(ref, pc, insns);
  initThumb(ref, pc);
  setup?.(ref);
  for (let i = 0; i < insns.length; i++) ref.cpu.step();

  const emu = new Emulator();
  emu.loadRom(new Uint8Array(0x100));
  emu.recomp.enabled = true;
  placeInsns(emu, pc, insns);
  initThumb(emu, pc);
  setup?.(emu);
  (emu.recomp as any).hits.set(pc, 1000);
  expect(emu.recomp.tryDispatch(), 'block must compile fully').toBe(insns.length);

  for (let i = 0; i < 16; i++) {
    expect(emu.cpu.state.r[i] >>> 0, `r${i}`).toBe(ref.cpu.state.r[i] >>> 0);
  }
  expect(emu.cpu.state.cpsr >>> 0, 'cpsr').toBe(ref.cpu.state.cpsr >>> 0);
  verify?.(emu, ref);
}

describe('Recompiler (JIT)', () => {
  it('compiles Format 3 MOV/ADD/SUB sequence and matches interpreter', () => {
    const interpResult = (() => {
      const emu = new Emulator();
      emu.loadRom(new Uint8Array(0x100));
      const pc = 0x03002000;
      // MOV R0, #10 ; ADD R0, #5 ; SUB R0, #3
      placeInsns(emu, pc, [0x200A, 0x3005, 0x3803]);
      initThumb(emu, pc);
      for (let i = 0; i < 3; i++) emu.cpu.step();
      return emu.cpu.state.r[0];
    })();
    expect(interpResult).toBe(12);

    // Now do the same through the recompiler.
    const emu = new Emulator();
    emu.loadRom(new Uint8Array(0x100));
    emu.recomp.enabled = true;
    const pc = 0x03002000;
    placeInsns(emu, pc, [0x200A, 0x3005, 0x3803]);
    initThumb(emu, pc);
    // Force-compile by inserting a fake hit count past threshold.
    (emu.recomp as any).hits.set(pc, 1000);
    // tryDispatch returns the number of insns it executed (3 here).
    const ran = emu.recomp.tryDispatch();
    expect(ran).toBe(3);
    expect(emu.cpu.state.r[0]).toBe(12);
    // After the block, r[15] should be advanced past the 3 insns.
    expect(emu.cpu.state.r[15] & ~1).toBe(pc + 6);
  });

  it('compiles a conditional branch and lands on the taken edge', () => {
    const emu = new Emulator();
    emu.loadRom(new Uint8Array(0x100));
    emu.recomp.enabled = true;
    const pc = 0x03002000;
    // MOV R0, #5 ; CMP R0, #5 ; BEQ +4 (skip the next NOP-ish insn)
    placeInsns(emu, pc, [0x2005, 0x2805, 0xD000 /* BEQ #0 -> taken=pc+4+0=pc+4 */]);
    initThumb(emu, pc);
    (emu.recomp as any).hits.set(pc, 1000);
    expect(emu.recomp.tryDispatch()).toBeGreaterThan(0);
    // taken target is pc + 4 + (0 << 1) = pc + 4 (= the next-next insn)
    // But our BEQ encoding with off=0 actually means target = pc + 4 +
    // (0 << 1) = pc + 4 RELATIVE to the branch PC (which is pc + 4 in the
    // sequence). So: target = (pc + 4) + 4 + 0 = pc + 8.
    // The block hits the branch as insn 3 (pc index 4), at decode
    // address pc + 4. Resolved: pc + 4 + 4 = pc + 8.
    expect(emu.cpu.state.r[15] & ~1).toBe(pc + 8);
  });

  it('compiles a Format 9 LDR/STR pair', () => {
    const emu = new Emulator();
    emu.loadRom(new Uint8Array(0x100));
    emu.recomp.enabled = true;
    const pc = 0x03002000;
    // STR R0, [R1, #0]  (encoding: 0110_0_00000_001_000 = 0x6008)
    // LDR R2, [R1, #0]  (encoding: 0110_1_00000_001_010 = 0x680A)
    placeInsns(emu, pc, [0x6008, 0x680A]);
    initThumb(emu, pc);
    emu.cpu.state.r[0] = 0xCAFEBABE;
    emu.cpu.state.r[1] = 0x03004000;
    (emu.recomp as any).hits.set(pc, 1000);
    expect(emu.recomp.tryDispatch()).toBeGreaterThan(0);
    expect(emu.cpu.state.r[2]).toBe(0xCAFEBABE);
  });

  it('rotates unaligned word LDR like the interpreter (misalignment 1/2/3)', () => {
    const base = 0x03004000;
    for (const mis of [1, 2, 3]) {
      // Ground truth: same sequence through the interpreter only.
      const ref = new Emulator();
      ref.loadRom(new Uint8Array(0x100));
      const pc = 0x03002000;
      // LDR R0, [R1, #0]  (encoding: 0110_1_00000_001_000 = 0x6808)
      placeInsns(ref, pc, [0x6808]);
      initThumb(ref, pc);
      ref.bus.write32(base, 0x11223344);
      ref.cpu.state.r[1] = base + mis;
      ref.cpu.step();

      // Same thing through the recompiler.
      const emu = new Emulator();
      emu.loadRom(new Uint8Array(0x100));
      emu.recomp.enabled = true;
      placeInsns(emu, pc, [0x6808]);
      initThumb(emu, pc);
      emu.bus.write32(base, 0x11223344);
      emu.cpu.state.r[1] = base + mis;
      (emu.recomp as any).hits.set(pc, 1000);
      expect(emu.recomp.tryDispatch()).toBe(1);

      for (let i = 0; i < 16; i++) {
        expect(emu.cpu.state.r[i] >>> 0).toBe(ref.cpu.state.r[i] >>> 0);
      }
      expect(emu.cpu.state.cpsr >>> 0).toBe(ref.cpu.state.cpsr >>> 0);
    }
  });

  it('masks unaligned word STR to the aligned word like the interpreter', () => {
    const base = 0x03004000;
    const pc = 0x03002000;
    // Ground truth: interpreter only.
    const ref = new Emulator();
    ref.loadRom(new Uint8Array(0x100));
    // STR R0, [R1, #0]  (encoding: 0110_0_00000_001_000 = 0x6008)
    placeInsns(ref, pc, [0x6008]);
    initThumb(ref, pc);
    ref.cpu.state.r[0] = 0xDEADBEEF;
    ref.cpu.state.r[1] = base + 2;               // misaligned by 2
    ref.cpu.step();
    expect(ref.bus.read32(base) >>> 0).toBe(0xDEADBEEF);

    // Same thing through the recompiler.
    const emu = new Emulator();
    emu.loadRom(new Uint8Array(0x100));
    emu.recomp.enabled = true;
    placeInsns(emu, pc, [0x6008]);
    initThumb(emu, pc);
    emu.cpu.state.r[0] = 0xDEADBEEF;
    emu.cpu.state.r[1] = base + 2;
    (emu.recomp as any).hits.set(pc, 1000);
    expect(emu.recomp.tryDispatch()).toBe(1);
    // The write must land at the aligned word, not base+2 / base+4.
    expect(emu.bus.read32(base) >>> 0).toBe(0xDEADBEEF);
    expect(emu.bus.read32(base + 4) >>> 0).toBe(ref.bus.read32(base + 4) >>> 0);
    for (let i = 0; i < 16; i++) {
      expect(emu.cpu.state.r[i] >>> 0).toBe(ref.cpu.state.r[i] >>> 0);
    }
    expect(emu.cpu.state.cpsr >>> 0).toBe(ref.cpu.state.cpsr >>> 0);
  });

  it('Format 2 ADD/SUB register matches interpreter at carry/overflow edges', () => {
    // ADD R2, R0, R1 = 0x1842 ; SUB R2, R0, R1 = 0x1A42
    const pairs: [number, number][] = [
      [10, 3],
      [0xFFFFFFFF, 1],          // ADD: carry + zero; SUB: no borrow
      [0x7FFFFFFF, 1],          // ADD: signed overflow
      [0, 1],                   // SUB: borrow
      [0x80000000, 1],          // SUB: signed overflow
      [0xFFFFFFFF, 0xFFFFFFFF], // SUB: a==b → zero, carry
    ];
    for (const [a, b] of pairs) {
      for (const insn of [0x1842, 0x1A42]) {
        runDiff([insn], (e) => { e.cpu.state.r[0] = a; e.cpu.state.r[1] = b; });
      }
    }
  });

  it('Format 2 ADD/SUB imm3 matches interpreter at carry/overflow edges', () => {
    // ADD R2, R0, #3 = 0x1CC2 ; SUB R2, R0, #7 = 0x1FC2
    for (const a of [0, 5, 0xFFFFFFFF, 0xFFFFFFFD, 0x7FFFFFFF, 0x7FFFFFFD, 0x80000002, 7, 3]) {
      runDiff([0x1CC2], (e) => { e.cpu.state.r[0] = a; });
      runDiff([0x1FC2], (e) => { e.cpu.state.r[0] = a; });
    }
  });

  it('Format 4 ADC matches interpreter with C in both states', () => {
    // CMP R2, #0 (0x2A00) with r2=0 sets C; CMP R2, #1 (0x2A01) clears C.
    // ADC R0, R1 = 0x4148.
    const pairs: [number, number][] = [
      [10, 3],
      [0xFFFFFFFE, 1],          // a+b = 0xFFFFFFFF: cIn=1 wraps to 0 (carry-chain edge)
      [0xFFFFFFFF, 0],          // same edge, asymmetric operands
      [0xFFFFFFFF, 0xFFFFFFFF], // carry from the first add alone
      [0x7FFFFFFF, 0],          // cIn=1 → signed overflow
      [0x7FFFFFFE, 1],
      [0, 0],
    ];
    for (const [a, b] of pairs) {
      for (const setC of [0x2A00, 0x2A01]) {
        runDiff([setC, 0x4148], (e) => {
          e.cpu.state.r[0] = a; e.cpu.state.r[1] = b; e.cpu.state.r[2] = 0;
        });
      }
    }
  });

  it('Format 4 SBC matches interpreter with C in both states', () => {
    // SBC R0, R1 = 0x4188.
    const pairs: [number, number][] = [
      [10, 3],
      [5, 5],                   // a==b: cIn=1 → 0/C=1; cIn=0 → -1/C=0
      [0, 0],
      [3, 10],                  // borrow
      [0, 0xFFFFFFFF],          // b+notC can exceed 32 bits
      [0xFFFFFFFF, 0xFFFFFFFF],
      [0x80000000, 1],          // signed overflow
    ];
    for (const [a, b] of pairs) {
      for (const setC of [0x2A00, 0x2A01]) {
        runDiff([setC, 0x4188], (e) => {
          e.cpu.state.r[0] = a; e.cpu.state.r[1] = b; e.cpu.state.r[2] = 0;
        });
      }
    }
  });

  it('Format 4 TST/NEG/CMN/MUL match interpreter flags', () => {
    // TST R0, R1 = 0x4208 ; NEG R0, R1 = 0x4248
    // CMN R0, R1 = 0x42C8 ; MUL R0, R1 = 0x4348
    const pairs: [number, number][] = [
      [0xF0F0F0F0, 0x0F0F0F0F], // TST → zero
      [0x80000000, 0x80000000], // TST → negative; CMN carry+overflow
      [0, 0],                   // NEG of 0 → zero/carry
      [0, 0x80000000],          // NEG of INT_MIN → overflow
      [0, 1],                   // NEG → negative w/ borrow
      [0xFFFFFFFF, 1],          // CMN → zero + carry
      [0x7FFFFFFF, 1],          // CMN → signed overflow
      [0x10000, 0x10000],       // MUL → wraps to 0
      [0xFFFFFFFF, 3],          // MUL → negative result
      [1234, 5678],
    ];
    for (const [a, b] of pairs) {
      for (const insn of [0x4208, 0x4248, 0x42C8, 0x4348]) {
        runDiff([insn], (e) => { e.cpu.state.r[0] = a; e.cpu.state.r[1] = b; });
      }
    }
  });

  it('Format 1 LSL/LSR/ASR immediate matches interpreter incl. carry', () => {
    // LSL R0, R1, #imm = 0x0008 | imm<<6
    // LSR R0, R1, #imm = 0x0808 | imm<<6   (imm=0 encodes #32)
    // ASR R0, R1, #imm = 0x1008 | imm<<6   (imm=0 encodes #32)
    // Each op at imm 0/1/31; values chosen so the carry-out differs
    // between high and low bits. C is seeded both ways via CMP R2,#0
    // (r2=0 → C set) / CMP R2,#1 (→ C clear) so LSL #0's
    // carry-unchanged path is actually exercised in both states.
    const insns: number[] = [];
    for (const base of [0x0008, 0x0808, 0x1008]) {
      for (const imm of [0, 1, 31]) insns.push(base | (imm << 6));
    }
    const values = [0, 1, 2, 0x80000000, 0x80000001, 0xC0000001, 0x40000000, 0x7FFFFFFF, 0xFFFFFFFF];
    for (const v of values) {
      for (const insn of insns) {
        for (const seedC of [0x2A00, 0x2A01]) {
          runDiff([seedC, insn], (e) => { e.cpu.state.r[1] = v; e.cpu.state.r[2] = 0; });
        }
      }
    }
  });

  it('Format 4 LSL/LSR/ASR/ROR by register matches interpreter incl. carry', () => {
    // LSL R0, R1 = 0x4088 ; LSR R0, R1 = 0x40C8
    // ASR R0, R1 = 0x4108 ; ROR R0, R1 = 0x41C8
    // Amounts hit every regShift range: 0 (value+carry unchanged),
    // 1/31 (<32), 32 (boundary), 33/64/255 (>32; 64 is ROR's nonzero
    // multiple of 32), 0x100 (masks to 0 via & 0xFF). Values include
    // both bit-31 states so ASR ≥32 and ROR-at-32 carry are covered,
    // and C is seeded both ways via CMP so unchanged-carry cases diff.
    const insns = [0x4088, 0x40C8, 0x4108, 0x41C8];
    const values = [0, 1, 0x80000001, 0x7FFFFFFF, 0xC0000001];
    const amounts = [0, 1, 2, 31, 32, 33, 64, 255, 0x100];
    for (const insn of insns) {
      for (const v of values) {
        for (const amt of amounts) {
          for (const seedC of [0x2A00, 0x2A01]) {
            runDiff([seedC, insn], (e) => {
              e.cpu.state.r[0] = v; e.cpu.state.r[1] = amt; e.cpu.state.r[2] = 0;
            });
          }
        }
      }
    }
  });

  it('Format 12 load address matches interpreter (PC and SP variants)', () => {
    // ADD R0, PC, #20 = 0xA005 ; ADD R1, SP, #32 = 0xA908
    runDiff([0xA005]);
    runDiff([0xA908]);
    // Both in one block — the PC variant's constant must track the
    // per-instruction decode address, not the block start.
    runDiff([0xA908, 0xA005, 0xA1FF]);  // ADD R1,PC,#1020 as the 3rd insn
    // SP variant with a funky (misaligned) SP value.
    runDiff([0xA908], (e) => { e.cpu.state.r[13] = 0x03007F02; });
  });

  it('Format 13 ADD/SUB SP matches interpreter', () => {
    // ADD SP, #40 = 0xB00A ; SUB SP, #40 = 0xB08A
    runDiff([0xB00A]);
    runDiff([0xB08A]);
    runDiff([0xB07F]);                  // max positive: +508
    runDiff([0xB0FF]);                  // max negative: -508
    runDiff([0xB00A, 0xB08A]);          // net zero round-trip
    // Wrap-around edge.
    runDiff([0xB08A], (e) => { e.cpu.state.r[13] = 4; });
  });

  it('Format 6 LDR PC-relative matches interpreter', () => {
    const pc = 0x03002000;
    // LDR R0, [PC, #4] = 0x4801 → addr = ((pc+4) & ~3) + 4 = pc + 8.
    runDiff([0x4801], (e) => { e.bus.write32(pc + 8, 0xDEADBEEF); });
    // imm8 = 0: addr = (pc+4) & ~3 = pc + 4 (just past the code).
    runDiff([0x4800], (e) => { e.bus.write32(pc + 4, 0xCAFEF00D); });
    // As the 2nd insn: decode addr pc+2 → arch PC pc+6 → (&~3) pc+4 →
    // +4 → pc+8. The constant must track the per-insn decode address.
    runDiff([0x2100, 0x4801], (e) => { e.bus.write32(pc + 8, 0x12345678); });
  });

  it('Format 7 LDR/STR register offset (word + byte) matches interpreter', () => {
    const base = 0x03004000;
    // STR R0, [R1, R2] = 0x5088 ; LDR R3, [R1, R2] = 0x588B
    runDiff([0x5088, 0x588B], (e) => {
      e.cpu.state.r[0] = 0xCAFEBABE; e.cpu.state.r[1] = base; e.cpu.state.r[2] = 8;
    }, (emu, ref) => {
      expect(emu.bus.read32(base + 8) >>> 0).toBe(ref.bus.read32(base + 8) >>> 0);
      expect(emu.bus.read32(base + 8) >>> 0).toBe(0xCAFEBABE);
    });
    // Unaligned word load: rotation comes from rb+ro, not an immediate.
    for (const mis of [1, 2, 3]) {
      runDiff([0x588B], (e) => {
        e.bus.write32(base, 0x11223344);
        e.cpu.state.r[1] = base; e.cpu.state.r[2] = mis;
      });
    }
    // Unaligned word store masks down to the aligned word.
    runDiff([0x5088], (e) => {
      e.cpu.state.r[0] = 0xDEADBEEF; e.cpu.state.r[1] = base; e.cpu.state.r[2] = 2;
    }, (emu, ref) => {
      expect(emu.bus.read32(base) >>> 0).toBe(ref.bus.read32(base) >>> 0);
      expect(emu.bus.read32(base) >>> 0).toBe(0xDEADBEEF);
      expect(emu.bus.read32(base + 4) >>> 0).toBe(0);
    });
    // STRB R0, [R1, R2] = 0x5488 ; LDRB R3, [R1, R2] = 0x5C8B
    runDiff([0x5488, 0x5C8B], (e) => {
      e.cpu.state.r[0] = 0x1234ABCD;       // only 0xCD must land
      e.cpu.state.r[1] = base; e.cpu.state.r[2] = 5;
    }, (emu, ref) => {
      expect(emu.bus.read8(base + 5)).toBe(ref.bus.read8(base + 5));
      expect(emu.bus.read8(base + 5)).toBe(0xCD);
      expect(emu.bus.read8(base + 4)).toBe(0);   // neighbours untouched
      expect(emu.bus.read8(base + 6)).toBe(0);
    });
    // LDRB of a high-bit byte must NOT sign-extend.
    runDiff([0x5C8B], (e) => {
      e.bus.write8(base + 1, 0xFE);
      e.cpu.state.r[1] = base; e.cpu.state.r[2] = 1;
    });
  });

  it('Format 8 STRH/LDSB/LDRH/LDSH matches interpreter', () => {
    const base = 0x03004000;
    // STRH R0, [R1, R2] = 0x5288 ; LDSB R3, [R1, R2] = 0x568B
    // LDRH R3, [R1, R2] = 0x5A8B ; LDSH R3, [R1, R2] = 0x5E8B
    // STRH stores the low 16 bits; an odd address masks to the aligned
    // halfword.
    runDiff([0x5288], (e) => {
      e.cpu.state.r[0] = 0x9876FEDC; e.cpu.state.r[1] = base; e.cpu.state.r[2] = 5;
    }, (emu, ref) => {
      expect(emu.bus.read16(base + 4)).toBe(ref.bus.read16(base + 4));
      expect(emu.bus.read16(base + 4)).toBe(0xFEDC);
      expect(emu.bus.read16(base + 6)).toBe(0);
    });
    // LDRH aligned with the high bit set (no sign extension).
    runDiff([0x5A8B], (e) => {
      e.bus.write16(base + 2, 0xBEEF);
      e.cpu.state.r[1] = base; e.cpu.state.r[2] = 2;
    });
    // LDRH at an ODD address: rotated right by 8, not sign-extended.
    runDiff([0x5A8B], (e) => {
      e.bus.write16(base, 0xBEEF);
      e.cpu.state.r[1] = base; e.cpu.state.r[2] = 1;
    });
    // LDSB negative + positive bytes.
    for (const v of [0x80, 0x7F, 0xFF, 0x00]) {
      runDiff([0x568B], (e) => {
        e.bus.write8(base + 3, v);
        e.cpu.state.r[1] = base; e.cpu.state.r[2] = 3;
      });
    }
    // LDSH even: sign-extends 16→32 (negative and positive).
    for (const v of [0x8000, 0x7FFF, 0xFFFF, 0x1234]) {
      runDiff([0x5E8B], (e) => {
        e.bus.write16(base + 2, v);
        e.cpu.state.r[1] = base; e.cpu.state.r[2] = 2;
      });
    }
    // LDSH at an ODD address — the ARM7TDMI byte quirk: reads the BYTE
    // at addr (the halfword's high byte here) sign-extended 8→32.
    for (const v of [0x80FF, 0x7F00, 0xFF7F]) {
      runDiff([0x5E8B], (e) => {
        e.bus.write16(base, v);
        e.cpu.state.r[1] = base; e.cpu.state.r[2] = 1;
      });
    }
  });

  it('Format 10 STRH/LDRH immediate matches interpreter', () => {
    const base = 0x03004000;
    // STRH R0, [R1, #4] = 0x8088 ; LDRH R2, [R1, #4] = 0x888A
    runDiff([0x8088, 0x888A], (e) => {
      e.cpu.state.r[0] = 0xABCD1234; e.cpu.state.r[1] = base;
    }, (emu, ref) => {
      expect(emu.bus.read16(base + 4)).toBe(ref.bus.read16(base + 4));
      expect(emu.bus.read16(base + 4)).toBe(0x1234);
    });
    // Odd effective address (rb odd, imm even): LDRH rotates...
    runDiff([0x888A], (e) => {
      e.bus.write16(base + 4, 0xBEEF);
      e.cpu.state.r[1] = base + 1;
    });
    // ...and STRH masks down to the aligned halfword.
    runDiff([0x8088], (e) => {
      e.cpu.state.r[0] = 0xFEDC; e.cpu.state.r[1] = base + 1;
    }, (emu, ref) => {
      expect(emu.bus.read16(base + 4)).toBe(ref.bus.read16(base + 4));
      expect(emu.bus.read16(base + 4)).toBe(0xFEDC);
      expect(emu.bus.read16(base + 6)).toBe(0);
    });
  });

  it('Format 11 SP-relative LDR/STR matches interpreter', () => {
    const sp = 0x03004100;
    // STR R0, [SP, #8] = 0x9002 ; LDR R1, [SP, #8] = 0x9902
    runDiff([0x9002, 0x9902], (e) => {
      e.cpu.state.r[13] = sp; e.cpu.state.r[0] = 0xFEEDFACE;
    }, (emu, ref) => {
      expect(emu.bus.read32(sp + 8) >>> 0).toBe(ref.bus.read32(sp + 8) >>> 0);
      expect(emu.bus.read32(sp + 8) >>> 0).toBe(0xFEEDFACE);
    });
    // Misaligned SP: the load rotates and the store masks, exactly like
    // the interpreter.
    for (const mis of [1, 2, 3]) {
      runDiff([0x9902], (e) => {
        e.cpu.state.r[13] = sp + mis;
        e.bus.write32(sp + 8, 0x11223344);
      });
      runDiff([0x9002], (e) => {
        e.cpu.state.r[13] = sp + mis;
        e.cpu.state.r[0] = 0xDEADBEEF;
      }, (emu, ref) => {
        expect(emu.bus.read32(sp + 8) >>> 0).toBe(ref.bus.read32(sp + 8) >>> 0);
        expect(emu.bus.read32(sp + 8) >>> 0).toBe(0xDEADBEEF);
        expect(emu.bus.read32(sp + 12) >>> 0).toBe(0);
      });
    }
  });

  it('bails out and returns false on unsupported instruction at start', () => {
    const emu = new Emulator();
    emu.loadRom(new Uint8Array(0x100));
    emu.recomp.enabled = true;
    const pc = 0x03002000;
    // PUSH {R0, LR} — Format 14, not supported in this build
    placeInsns(emu, pc, [0xB501]);
    initThumb(emu, pc);
    (emu.recomp as any).hits.set(pc, 1000);
    expect(emu.recomp.tryDispatch()).toBe(0);
    // Now-cached as null so a second call doesn't bother re-trying.
    expect(emu.recomp.tryDispatch()).toBe(0);
  });
});
