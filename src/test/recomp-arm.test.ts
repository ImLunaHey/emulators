// ARM recompiler differential tests. Mirrors recomp.test.ts but in ARM
// mode: run an ARM instruction sequence through a JIT-forced emulator
// and a pure-interpreter emulator, then compare all of r0..r15 + cpsr.
// The lockstep harness (jit-lockstep.ts) is the broad real-ROM check;
// these pin down specific data-processing encodings as a regression net.

import { describe, it, expect } from 'vitest';
import { Emulator } from '../emulator';

function placeArm(emu: Emulator, addr: number, insns: number[]) {
  for (let i = 0; i < insns.length; i++) emu.bus.write32(addr + i * 4, insns[i] >>> 0);
  // Terminate with SWI (0xEF000000) — the JIT always bails on it, so a
  // compiled block stops exactly at insns.length.
  emu.bus.write32(addr + insns.length * 4, 0xEF000000);
}

function initArm(emu: Emulator, pc: number) {
  emu.cpu.state.cpsr = 0x1F;             // SYS mode, ARM (no T bit)
  emu.cpu.state.r[15] = pc;              // ARM PC (word-aligned, no THUMB bit)
  emu.cpu.state.r[13] = 0x03007F00;      // SP
}

// Run `insns` (ARM) through interpreter + JIT and assert identical state.
function runDiffArm(
  insns: number[],
  setup?: (emu: Emulator) => void,
  verify?: (emu: Emulator, ref: Emulator) => void,
) {
  const pc = 0x03002000;

  const ref = new Emulator();
  ref.loadRom(new Uint8Array(0x100));
  placeArm(ref, pc, insns);
  initArm(ref, pc);
  setup?.(ref);
  for (let i = 0; i < insns.length; i++) ref.cpu.step();

  const emu = new Emulator();
  emu.loadRom(new Uint8Array(0x100));
  emu.recomp.enabled = true;
  placeArm(emu, pc, insns);
  initArm(emu, pc);
  setup?.(emu);
  (emu.recomp as any).hits.set(pc, 1000);
  // Proves the ARM block actually JIT-compiled (and ran every insn).
  expect(emu.recomp.tryDispatch(), 'ARM block must compile fully').toBe(insns.length);

  for (let i = 0; i < 16; i++) {
    expect(emu.cpu.state.r[i] >>> 0, `r${i}`).toBe(ref.cpu.state.r[i] >>> 0);
  }
  expect(emu.cpu.state.cpsr >>> 0, 'cpsr').toBe(ref.cpu.state.cpsr >>> 0);
  verify?.(emu, ref);
}

describe('ARM recompiler (JIT)', () => {
  it('MOV/ADD/SUB immediate', () => {
    // MOV r0,#10 ; ADD r0,r0,#5 ; SUB r0,r0,#3  → r0 = 12
    runDiffArm([0xE3A0000A, 0xE2800005, 0xE2400003], undefined, (emu) => {
      expect(emu.cpu.state.r[0] >>> 0).toBe(12);
    });
  });

  it('register operand with immediate LSL shift', () => {
    // MOV r0,#12 ; MOV r1, r0, LSL #2  → r1 = 48
    runDiffArm([0xE3A0000C, 0xE1A01100], undefined, (emu) => {
      expect(emu.cpu.state.r[1] >>> 0).toBe(48);
    });
  });

  it('RSB / ORR / BIC / MVN', () => {
    // MOV r0,#5 ; RSB r1,r0,#0 (=-5) ; ORR r2,r0,#0x100 ; BIC r3,r0,#1 ; MVN r4,r0
    runDiffArm([0xE3A00005, 0xE2601000, 0xE3802C01, 0xE3C03001, 0xE1E04000]);
  });

  it('flag-setting MOVS/ADDS/SUBS (N/Z/C/V)', () => {
    // MOVS r0,#0 (Z) ; SUBS r1,r0,#1 (borrow) ; ADDS r2,r1,#1 (carry/Z)
    runDiffArm([0xE3B00000, 0xE2501001, 0xE2912001]);
  });

  it('ADCS / SBCS carry propagation', () => {
    // MOV r0,#0xFF000000 ; ADDS r0,r0,r0 (sets C) ; ADC r1,r0,#0 ; SBC r2,r0,#0
    runDiffArm([0xE3A004FF, 0xE0900000, 0xE2A01000, 0xE2C02000]);
  });

  it('logical shift carry into flags (ANDS with LSR)', () => {
    // MOV r0,#3 ; ANDS r1, r0, r0, LSR #1
    runDiffArm([0xE3A00003, 0xE0111020]);
  });

  it('conditional execution — taken and skipped', () => {
    // MOVS r0,#0 (Z=1) ; MOVEQ r1,#7 (runs) ; MOVNE r2,#9 (skipped)
    runDiffArm([0xE3B00000, 0x03A01007, 0x13A02009], undefined, (emu) => {
      expect(emu.cpu.state.r[1] >>> 0).toBe(7);
      expect(emu.cpu.state.r[2] >>> 0).toBe(0);
    });
  });

  it('conditional execution depends on in-block flags', () => {
    // MOV r0,#5 ; CMP r0,#5 (Z=1) ; ADDEQ r3,r0,#10 ; SUBNE r4,r0,#1
    runDiffArm([0xE3A00005, 0xE3500005, 0x0280300A, 0x12404001], undefined, (emu) => {
      expect(emu.cpu.state.r[3] >>> 0).toBe(15);  // ADDEQ ran
      expect(emu.cpu.state.r[4] >>> 0).toBe(0);   // SUBNE skipped
    });
  });

  it('conditional flag-setting op (CMP then ADCS-style) honours condition', () => {
    // MOV r0,#1 ; CMP r0,#0 (Z=0,C=1) ; MOVEQS r1,#0xFF (skipped, no flag change)
    runDiffArm([0xE3A00001, 0xE3500000, 0x03B010FF]);
  });

  it('B (unconditional, backward)', () => {
    // MOV r0,#5 ; B 0x03002000  → r15 lands on 0x03002000
    runDiffArm([0xE3A00005, 0xEAFFFFFD], undefined, (emu) => {
      expect(emu.cpu.state.r[15] & ~3).toBe(0x03002000);
      expect(emu.cpu.state.r[0] >>> 0).toBe(5);
    });
  });

  it('BL sets LR = next instruction', () => {
    // MOV r0,#5 ; BL 0x03002000
    runDiffArm([0xE3A00005, 0xEBFFFFFD], undefined, (emu) => {
      expect(emu.cpu.state.r[15] & ~3).toBe(0x03002000);
      expect(emu.cpu.state.r[14] >>> 0).toBe(0x03002008);
    });
  });

  it('Bcond taken vs not-taken', () => {
    // MOVS r0,#0 (Z=1) ; BEQ 0x03002000  → taken
    runDiffArm([0xE3B00000, 0x0AFFFFFD], undefined, (emu) => {
      expect(emu.cpu.state.r[15] & ~3).toBe(0x03002000);
    });
    // MOVS r0,#1 (Z=0) ; BEQ 0x03002000  → not taken, falls through
    runDiffArm([0xE3B00001, 0x0AFFFFFD], undefined, (emu) => {
      expect(emu.cpu.state.r[15] & ~3).toBe(0x03002008);
    });
  });

  it('BX switches to THUMB on bit0', () => {
    // BX r1 with r1 = 0x08000001 → THUMB, r15 = 0x08000000, T set
    runDiffArm([0xE12FFF11], (emu) => { emu.cpu.state.r[1] = 0x08000001; }, (emu) => {
      expect(emu.cpu.state.r[15] & ~1).toBe(0x08000000);
      expect(emu.cpu.state.cpsr & 0x20).toBe(0x20);   // T bit set
    });
  });

  it('register-specified shift operand (LSL/LSR/ASR/ROR by Rs)', () => {
    // r0=0x87654321, r1=4 ; MOV r2,r0,LSL r1 ; MOV r3,r0,LSR r1 ;
    //                       MOV r4,r0,ASR r1 ; MOVS r5,r0,ROR r1
    runDiffArm([0xE1A02110, 0xE1A03130, 0xE1A04150, 0xE1B05170],
      (emu) => { emu.cpu.state.r[0] = 0x87654321; emu.cpu.state.r[1] = 4; });
  });

  it('register-shift amount >= 32 edge cases', () => {
    // r0=0xF0F0F0F0, r1=32, r6=40 ; LSLS/LSRS by 32 ; LSLS by 40
    runDiffArm([0xE1B02011, 0xE1B03031, 0xE1B04611],
      (emu) => { emu.cpu.state.r[0] = 0xF0F0F0F0; emu.cpu.state.r[1] = 32; emu.cpu.state.r[6] = 40; });
  });

  it('R15 as operand (ADD rd, pc, #imm — ADR-style)', () => {
    // ADD r0, pc, #4  → r0 = (decode+8) + 4
    runDiffArm([0xE28F0004], undefined, (emu) => {
      expect(emu.cpu.state.r[0] >>> 0).toBe((0x03002000 + 8 + 4) >>> 0);
    });
  });

  it('MOV PC, LR (ALU branch / return)', () => {
    // r14 = 0x08000100 ; MOV pc, lr  → r15 = 0x08000100
    runDiffArm([0xE1A0F00E], (emu) => { emu.cpu.state.r[14] = 0x08000100; }, (emu) => {
      expect(emu.cpu.state.r[15] & ~3).toBe(0x08000100);
    });
  });

  it('STR/LDR word round-trip', () => {
    // r1=IWRAM, r0=0xDEADBEEF ; STR r0,[r1] ; LDR r2,[r1]
    runDiffArm([0xE5810000, 0xE5912000],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[0] = 0xDEADBEEF; },
      (emu) => { expect(emu.cpu.state.r[2] >>> 0).toBe(0xDEADBEEF); });
  });

  it('STRB/LDRB byte round-trip', () => {
    // STRB r0,[r1] ; LDRB r3,[r1]  with r0=0x123456AB → r3 = 0xAB
    runDiffArm([0xE5C10000, 0xE5D13000],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[0] = 0x123456AB; },
      (emu) => { expect(emu.cpu.state.r[3] >>> 0).toBe(0xAB); });
  });

  it('pre-indexed with writeback', () => {
    // LDR r4, [r1, #8]!  → r1 advances by 8
    runDiffArm([0xE5B14008], (emu) => { emu.cpu.state.r[1] = 0x03004000; },
      (emu) => { expect(emu.cpu.state.r[1] >>> 0).toBe(0x03004008); });
  });

  it('register-offset (shifted) store/load', () => {
    // STR r0,[r1,r6,LSL#2] ; LDR r5,[r1,r6,LSL#2]  (addr = r1 + 8)
    runDiffArm([0xE7810106, 0xE7915106],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[6] = 2; emu.cpu.state.r[0] = 0xCAFEF00D; },
      (emu) => { expect(emu.cpu.state.r[5] >>> 0).toBe(0xCAFEF00D); });
  });

  it('post-indexed always writes back', () => {
    // LDR r7, [r1], #4  → r1 += 4
    runDiffArm([0xE4917004], (emu) => { emu.cpu.state.r[1] = 0x03004000; },
      (emu) => { expect(emu.cpu.state.r[1] >>> 0).toBe(0x03004004); });
  });

  it('LDR PC (load into r15, block-ender)', () => {
    // mem[0x03004000] = 0x08000100 ; LDR pc, [r1]
    runDiffArm([0xE591F000],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.bus.write32(0x03004000, 0x08000100); },
      (emu) => { expect(emu.cpu.state.r[15] & ~3).toBe(0x08000100); });
  });

  it('STRH/LDRH/LDRSB/LDRSH round-trip + sign extension', () => {
    // r0=0x1234ABCD ; STRH r0,[r1] ; LDRH r2,[r1] ; LDRSB r3,[r1] ; LDRSH r4,[r1]
    runDiffArm([0xE1C100B0, 0xE1D120B0, 0xE1D130D0, 0xE1D140F0],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[0] = 0x1234ABCD; },
      (emu) => {
        expect(emu.cpu.state.r[2] >>> 0).toBe(0x0000ABCD);   // LDRH
        expect(emu.cpu.state.r[3] >>> 0).toBe(0xFFFFFFCD);   // LDRSB (0xCD sign-ext)
        expect(emu.cpu.state.r[4] >>> 0).toBe(0xFFFFABCD);   // LDRSH (0xABCD sign-ext)
      });
  });

  it('LDRSH at odd address degrades to LDRSB', () => {
    // mem8[0x03004001] = 0x80 ; LDRSH r5, [r1]  (r1 odd) → 0xFFFFFF80
    runDiffArm([0xE1D150F0],
      (emu) => { emu.cpu.state.r[1] = 0x03004001; emu.bus.write8(0x03004001, 0x80); },
      (emu) => { expect(emu.cpu.state.r[5] >>> 0).toBe(0xFFFFFF80); });
  });

  it('halfword register-offset store/load', () => {
    // r7=0 ; STRH r0,[r1,r7] ; LDRH r6,[r1,r7]
    runDiffArm([0xE18100B7, 0xE19160B7],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[7] = 0; emu.cpu.state.r[0] = 0x0000BEEF; },
      (emu) => { expect(emu.cpu.state.r[6] >>> 0).toBe(0xBEEF); });
  });

  it('STMIA / LDMIA round-trip', () => {
    // STMIA r1, {r0,r2,r3} ; LDMIA r1, {r5,r6,r7}
    runDiffArm([0xE881000D, 0xE89100E0],
      (emu) => {
        emu.cpu.state.r[1] = 0x03004000;
        emu.cpu.state.r[0] = 0x11111111; emu.cpu.state.r[2] = 0x22222222; emu.cpu.state.r[3] = 0x33333333;
      },
      (emu) => {
        expect(emu.cpu.state.r[5] >>> 0).toBe(0x11111111);
        expect(emu.cpu.state.r[6] >>> 0).toBe(0x22222222);
        expect(emu.cpu.state.r[7] >>> 0).toBe(0x33333333);
      });
  });

  it('PUSH / POP (STMDB sp! / LDMIA sp!)', () => {
    // PUSH {r0,r1} ; POP {r2,r3}  → r2=r0, r3=r1, sp restored
    runDiffArm([0xE92D0003, 0xE8BD000C],
      (emu) => { emu.cpu.state.r[0] = 0xAAAA0000; emu.cpu.state.r[1] = 0xBBBB0000; },
      (emu) => {
        expect(emu.cpu.state.r[2] >>> 0).toBe(0xAAAA0000);
        expect(emu.cpu.state.r[3] >>> 0).toBe(0xBBBB0000);
        expect(emu.cpu.state.r[13] >>> 0).toBe(0x03007F00);
      });
  });

  it('LDM with PC in list (block-ender)', () => {
    // mem[0x03004000]=0x08000200 ; LDMIA r1!, {pc}
    runDiffArm([0xE8B18000],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.bus.write32(0x03004000, 0x08000200); },
      (emu) => {
        expect(emu.cpu.state.r[15] & ~3).toBe(0x08000200);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0x03004004);   // writeback
      });
  });

  it('STM base-in-list: lowest stores original base', () => {
    // STMIA r1!, {r1,r2}  (r1 is lowest in list) → mem[base] == original base
    runDiffArm([0xE8A10006],
      (emu) => { emu.cpu.state.r[1] = 0x03004000; emu.cpu.state.r[2] = 0x22222222; },
      (emu, ref) => {
        expect(emu.bus.read32(0x03004000) >>> 0).toBe(0x03004000);
        expect(emu.bus.read32(0x03004000) >>> 0).toBe(ref.bus.read32(0x03004000) >>> 0);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0x03004008);   // writeback
      });
  });

  it('STM base-in-list: not-first stores writeback value', () => {
    // STMIA r1!, {r0,r1}  (r1 not lowest) → mem[base+4] == writeback addr
    runDiffArm([0xE8A10003],
      (emu) => { emu.cpu.state.r[0] = 0x0000AAAA; emu.cpu.state.r[1] = 0x03004000; },
      (emu, ref) => {
        expect(emu.bus.read32(0x03004004) >>> 0).toBe(0x03004008);
        expect(emu.bus.read32(0x03004004) >>> 0).toBe(ref.bus.read32(0x03004004) >>> 0);
      });
  });

  it('MUL / MLA', () => {
    // r1=7, r2=6, r4=100 ; MUL r0,r1,r2 ; MLA r3,r1,r2,r4
    runDiffArm([0xE0000291, 0xE0234291],
      (emu) => { emu.cpu.state.r[1] = 7; emu.cpu.state.r[2] = 6; emu.cpu.state.r[4] = 100; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(42);
        expect(emu.cpu.state.r[3] >>> 0).toBe(142);
      });
  });

  it('MULS sets N/Z from the low 32 bits (signed product)', () => {
    // r1=-3, r2=5 ; MULS r0,r1,r2 → r0 = -15 (0xFFFFFFF1), N set
    runDiffArm([0xE0100291],
      (emu) => { emu.cpu.state.r[1] = 0xFFFFFFFD; emu.cpu.state.r[2] = 5; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(0xFFFFFFF1);
        expect((emu.cpu.state.cpsr & 0x80000000) >>> 0).toBe(0x80000000);   // N
      });
  });

  it('UMULL (unsigned 64-bit)', () => {
    // r2=0xFFFFFFFF, r3=2 ; UMULL r0(lo),r1(hi),r2,r3 → 0x1_FFFFFFFE
    runDiffArm([0xE0810392],
      (emu) => { emu.cpu.state.r[2] = 0xFFFFFFFF; emu.cpu.state.r[3] = 2; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(0xFFFFFFFE);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0x1);
      });
  });

  it('SMULL (signed 64-bit)', () => {
    // r2=-1, r3=2 ; SMULL r0,r1,r2,r3 → -2 = 0xFFFFFFFF_FFFFFFFE
    runDiffArm([0xE0C10392],
      (emu) => { emu.cpu.state.r[2] = 0xFFFFFFFF; emu.cpu.state.r[3] = 2; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(0xFFFFFFFE);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0xFFFFFFFF);
      });
  });

  it('UMLAL (unsigned accumulate)', () => {
    // r0=10(lo), r1=0(hi), r2=5, r3=4 ; UMLAL → 20 + 10 = 30
    runDiffArm([0xE0A10392],
      (emu) => { emu.cpu.state.r[0] = 10; emu.cpu.state.r[1] = 0; emu.cpu.state.r[2] = 5; emu.cpu.state.r[3] = 4; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(30);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0);
      });
  });

  it('SMLAL (signed accumulate)', () => {
    // r0=0, r1=0, r2=-1, r3=3 ; SMLAL → -3 = 0xFFFFFFFF_FFFFFFFD
    runDiffArm([0xE0E10392],
      (emu) => { emu.cpu.state.r[0] = 0; emu.cpu.state.r[1] = 0; emu.cpu.state.r[2] = 0xFFFFFFFF; emu.cpu.state.r[3] = 3; },
      (emu) => {
        expect(emu.cpu.state.r[0] >>> 0).toBe(0xFFFFFFFD);
        expect(emu.cpu.state.r[1] >>> 0).toBe(0xFFFFFFFF);
      });
  });

  it('UMULLS sets N from hi sign bit', () => {
    // r2=r3=0xFFFFFFFF ; UMULLS → hi=0xFFFFFFFE (N set)
    runDiffArm([0xE0910392],
      (emu) => { emu.cpu.state.r[2] = 0xFFFFFFFF; emu.cpu.state.r[3] = 0xFFFFFFFF; },
      (emu) => {
        expect(emu.cpu.state.r[1] >>> 0).toBe(0xFFFFFFFE);
        expect((emu.cpu.state.cpsr & 0x80000000) >>> 0).toBe(0x80000000);   // N
      });
  });
});
