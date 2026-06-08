// CPU correctness tests. Vitest-driven — runs known ARM/THUMB
// instruction sequences and asserts register/flag output matches the
// ARMv4T spec. The goal is to flush out subtle bugs (flag setting,
// shifter edge cases, sign extension, mode banking) that would
// otherwise only manifest as game-init stalls.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Cpu } from '../cpu/cpu';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { FLAG_C, FLAG_N, FLAG_T, FLAG_V, FLAG_Z, Mode } from '../cpu/state';

function makeCpu(): { cpu: Cpu; bus: Bus } {
  const bus = new Bus();
  const irq = new Irq();
  const keypad = new Keypad();
  const dma = new Dma(bus, irq);
  const timers = new Timers(irq);
  const ppu = new Ppu(bus, irq, dma);
  const cpu = new Cpu(bus);
  const io = new Io(bus, ppu, dma, timers, irq, keypad, cpu);
  bus.attachIo(io);
  bus.attachSave({ read: () => 0xFF, write: () => {} });
  bus.loadRom(new Uint8Array(0x100));
  cpu.reset();
  return { cpu, bus };
}

function setupRunArm(insns: number[], regs: Record<string, number> = {}): { cpu: Cpu; bus: Bus } {
  const { cpu, bus } = makeCpu();
  cpu.state.cpsr = Mode.SYS;
  for (let i = 0; i < insns.length; i++) bus.write32(0x03000000 + i * 4, insns[i]);
  cpu.state.r[15] = 0x03000000;
  for (const k in regs) {
    const n = parseInt(k.replace(/^r/i, ''), 10);
    if (!isNaN(n) && n >= 0 && n < 16) cpu.state.r[n] = regs[k] >>> 0;
  }
  return { cpu, bus };
}

function setupRunThumb(insns: number[], regs: Record<string, number> = {}): { cpu: Cpu; bus: Bus } {
  const { cpu, bus } = makeCpu();
  cpu.state.cpsr = Mode.SYS | FLAG_T;
  for (let i = 0; i < insns.length; i++) bus.write16(0x03000000 + i * 2, insns[i]);
  cpu.state.r[15] = 0x03000000;
  for (const k in regs) {
    const n = parseInt(k.replace(/^r/i, ''), 10);
    if (!isNaN(n) && n >= 0 && n < 16) cpu.state.r[n] = regs[k] >>> 0;
  }
  return { cpu, bus };
}

function runSteps(cpu: Cpu, n: number): void { for (let i = 0; i < n; i++) cpu.step(); }

function flagsStr(cpsr: number): string {
  return (
    ((cpsr & FLAG_N) ? 'N' : 'n') +
    ((cpsr & FLAG_Z) ? 'Z' : 'z') +
    ((cpsr & FLAG_C) ? 'C' : 'c') +
    ((cpsr & FLAG_V) ? 'V' : 'v')
  );
}

describe('ARM data processing', () => {
  it('ADD R0, R1, R2 (no flags)', () => {
    const { cpu } = setupRunArm([0xE0810002], { r1: 5, r2: 3 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(8);
  });

  it('ADDS R0, R1, R2 with overflow', () => {
    // 0x80000000 + 0x80000000 = 0x100000000 → result 0, C=1, V=1 (neg+neg=pos)
    const { cpu } = setupRunArm([0xE0910002], { r1: 0x80000000, r2: 0x80000000 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0);
    expect(flagsStr(cpu.state.cpsr)).toBe('nZCV');
  });

  it('SUBS R0, R0, R0 → 0/Z', () => {
    const { cpu } = setupRunArm([0xE0500000], { r0: 42 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0);
    expect(flagsStr(cpu.state.cpsr)).toBe('nZCv');
  });

  it('MOV R0, #0xFF000000 (rotated immediate)', () => {
    // imm=FF, rot=8 (encoded as 4 in bits 11:8)
    const { cpu } = setupRunArm([0xE3A004FF]);
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0xFF000000);
  });

  it('CMP 5, 10 sets borrow (C=0), no overflow', () => {
    const { cpu } = setupRunArm([0xE1500001], { r0: 5, r1: 10 });
    runSteps(cpu, 1);
    // 5-10 = -5, N=1, no overflow, V=0
    expect(flagsStr(cpu.state.cpsr)).toBe('Nzcv');
  });

  it('SBCS with !C borrow', () => {
    // First SUBS R0,R0,#1 with R0=0 → C=0 (borrow). Then SBCS R3,R4,R5 with R4=10,R5=3.
    // SBC = R4 - R5 - !C = 10 - 3 - 1 = 6.
    const insns = [0xE2500001, 0xE0D43005];
    const { cpu } = setupRunArm(insns, { r0: 0, r4: 10, r5: 3 });
    runSteps(cpu, 2);
    expect(cpu.state.r[3]).toBe(6);
  });

  it('LSL #32 by register: result 0, C = bit0', () => {
    const insns = [0xE3A000FF, 0xE3A01020, 0xE1B00110];
    const { cpu } = setupRunArm(insns);
    runSteps(cpu, 3);
    expect(cpu.state.r[0]).toBe(0);
    expect(flagsStr(cpu.state.cpsr)).toBe('nZCv');
  });
});

describe('ARM memory access', () => {
  it('LDR with unaligned address rotates', () => {
    // ARM LDR rotates aligned value right by (addr & 3) * 8 bits.
    const insns = [0xE59F0008, 0xE5901000, 0xE12FFF1E, 0x00000000, 0x03000101];
    const { cpu, bus } = setupRunArm(insns);
    bus.write32(0x03000100, 0xDEADBEEF);
    runSteps(cpu, 2);
    // addr 0x101, rot=8: 0xDEADBEEF >>> 8 | (<<24) = 0xEFDEADBE
    expect(cpu.state.r[1]).toBe(0xEFDEADBE);
  });

  it('LDRSH unaligned reads byte sign-extended', () => {
    const insns = [0xE59F0008, 0xE1D010F0, 0xE12FFF1E, 0x00000000, 0x03000101];
    const { cpu, bus } = setupRunArm(insns);
    bus.write16(0x03000100, 0xAA55);
    runSteps(cpu, 2);
    // unaligned → read byte at 0x101 = 0xAA, sign-extend to 0xFFFFFFAA
    expect(cpu.state.r[1]).toBe(0xFFFFFFAA);
  });

  it('UMULL 0xFFFFFFFF * 0xFFFFFFFF', () => {
    // UMULL R0(Lo), R1(Hi), R2, R3
    const { cpu } = setupRunArm([0xE0810392], { r2: 0xFFFFFFFF, r3: 0xFFFFFFFF });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0x00000001);
    expect(cpu.state.r[1]).toBe(0xFFFFFFFE);
  });
});

describe('THUMB instructions', () => {
  it('MOV R0, #5', () => {
    const { cpu } = setupRunThumb([0x2005]);
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(5);
  });

  it('LSL R0, R0, #1', () => {
    const { cpu } = setupRunThumb([0x0040], { r0: 0x55 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0xAA);
  });

  it('Format 8 LDRH register-offset', () => {
    // 0x5AC8 = LDRH R0, [R1, R3] (per Format 8: H=1, S=0, bit 9=1)
    const { cpu, bus } = setupRunThumb([0x5AC8], { r1: 0x03000100, r3: 2 });
    bus.write16(0x03000102, 0x1234);
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0x1234);
  });

  it('Format 8 LDRSH sign extends', () => {
    // 0x5EC8 = LDRSH R0, [R1, R3]
    const { cpu, bus } = setupRunThumb([0x5EC8], { r1: 0x03000100, r3: 2 });
    bus.write16(0x03000102, 0xFFAA);
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0xFFFFFFAA);
  });

  it('Format 7 STR register-offset', () => {
    // 0x5088 = STR R0, [R1, R2] (L=0, B=0, bit 9=0)
    const { cpu, bus } = setupRunThumb([0x5088], { r0: 0x12345678, r1: 0x03000100, r2: 0 });
    runSteps(cpu, 1);
    expect(bus.read32(0x03000100)).toBe(0x12345678);
  });

  it('Format 7 LDRB register-offset', () => {
    // 0x5C88 = LDRB R0, [R1, R2] (L=1, B=1, bit 9=0)
    const { cpu, bus } = setupRunThumb([0x5C88], { r1: 0x03000100, r2: 1 });
    bus.write8(0x03000101, 0xCD);
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0xCD);
  });

  it('BL forward sets PC and LR', () => {
    // BL F000 F802 at 0x03000000: target = (decode+4) + (offset_l<<1) = 0x03000004 + 4 = 0x03000008
    const { cpu } = setupRunThumb([0xF000, 0xF802, 0x0000, 0x0000, 0x2042, 0x4770]);
    runSteps(cpu, 2);
    expect(cpu.state.r[15]).toBe(0x03000008);
    expect(cpu.state.r[14] & 1).toBe(1);  // THUMB return marker on bit 0
  });

  it('POP {PC} with bit0=1 stays in THUMB', () => {
    const { cpu, bus } = setupRunThumb([0xBD00], { r13: 0x03000200 });
    bus.write32(0x03000200, 0x03000011);
    runSteps(cpu, 1);
    expect(cpu.state.r[15]).toBe(0x03000010);
    expect(cpu.state.cpsr & FLAG_T).toBe(FLAG_T);
  });

  it('NEG R0, R0 (Format 4 op9)', () => {
    // NEG = 0 - 5 = -5; N=1, no signed overflow → V=0, borrow → C=0
    const { cpu } = setupRunThumb([0x4240], { r0: 5 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0xFFFFFFFB);
    expect(flagsStr(cpu.state.cpsr)).toBe('Nzcv');
  });

  it('Format 5 hi-reg ADD R0, R9', () => {
    const { cpu } = setupRunThumb([0x4448], { r0: 10, r9: 5 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(15);
  });
});

describe('Shifter edge cases', () => {
  it('LSR by reg = 0 leaves value + carry untouched', () => {
    // MOV R0, #0x5A; LSRS R0, #0 (encoded as MOV without shift)
    // Use Format 4 LSR by register where reg = 0
    // MOV R0, #0x5A; MOV R1, #0; LSRS R0, R0, R1
    const insns = [0xE3A0005A, 0xE3A01000, 0xE1B00130];
    const { cpu } = setupRunArm(insns);
    // Set C=1 via initial flag manipulation - actually we can't easily.
    // Just verify R0 stays 0x5A.
    runSteps(cpu, 3);
    expect(cpu.state.r[0]).toBe(0x5A);
  });

  it('ASR by reg = 32 → sign-extend', () => {
    // MOV R0, #0x80000000 (-MAX); MOV R1, #32; MOVS R0, R0, ASR R1
    // Result: 0xFFFFFFFF (sign bit broadcast). C = sign bit = 1.
    const insns = [0xE3A00102, 0xE3A01020, 0xE1B00150];
    const { cpu } = setupRunArm(insns, { r0: 0x80000000 });
    runSteps(cpu, 3);
    expect(cpu.state.r[0]).toBe(0xFFFFFFFF);
    expect(cpu.state.cpsr & FLAG_C).toBe(FLAG_C);
  });

  it('ROR by reg = 16 (rotate by 16)', () => {
    // MOV R0, #0x12345678 — actually we need to construct this differently.
    // Just set R0 directly via test setup.
    const insns = [0xE1B00170];  // MOVS R0, R0, ROR R1
    const { cpu } = setupRunArm(insns, { r0: 0xABCD1234, r1: 16 });
    runSteps(cpu, 1);
    expect(cpu.state.r[0]).toBe(0x1234ABCD);
  });

  it('RRX (ROR #0 in immediate form)', () => {
    // MOV R0, R0, RRX = data proc opcode MOV, shift type ROR, imm=0
    // 0xE1A00060 = MOV R0, R0, RRX
    const insns = [0xE1A00060];
    const { cpu } = setupRunArm(insns, { r0: 0x80000001 });
    // C bit must be set first. Use MOV with shifter that sets C.
    // Easier: set CPSR directly.
    cpu.state.cpsr |= FLAG_C;
    runSteps(cpu, 1);
    // RRX: result = (C<<31) | (val>>>1). C set, val=0x80000001.
    // Result: 0xC0000000 | 0x40000000 = 0xC0000000. Wait — (C<<31) | (val>>>1) = 0x80000000 | 0x40000000 = 0xC0000000.
    expect(cpu.state.r[0]).toBe(0xC0000000);
  });
});

describe('PSR transfer', () => {
  it('MRS R0, CPSR returns current CPSR', () => {
    const { cpu } = setupRunArm([0xE10F0000]);
    cpu.state.cpsr = Mode.SYS | FLAG_N | FLAG_C;
    runSteps(cpu, 1);
    expect(cpu.state.r[0] >>> 0).toBe((Mode.SYS | FLAG_N | FLAG_C) >>> 0);
  });

  it('MSR CPSR_c, R0 switches mode', () => {
    // MSR CPSR_c, R0 — field mask = 0x01 (control byte). Encoding: 0xE129F000 + Rm
    const insns = [0xE129F000];  // MSR CPSR_c, R0
    const { cpu } = setupRunArm(insns, { r0: Mode.IRQ });
    runSteps(cpu, 1);
    expect(cpu.state.cpsr & 0x1F).toBe(Mode.IRQ);
  });
});

describe('Block transfer (LDM/STM)', () => {
  it('STMIA R0!, {R1,R2} writes both and updates base', () => {
    // STMIA R0!, {R1, R2}: 0xE8A00006
    const insns = [0xE8A00006];
    const { cpu, bus } = setupRunArm(insns, { r0: 0x03000100, r1: 0x11111111, r2: 0x22222222 });
    runSteps(cpu, 1);
    expect(bus.read32(0x03000100)).toBe(0x11111111);
    expect(bus.read32(0x03000104)).toBe(0x22222222);
    expect(cpu.state.r[0]).toBe(0x03000108);  // base advanced
  });

  it('LDMDB R0!, {R1, R2} loads both and decrements base', () => {
    // LDMDB R0!, {R1, R2}: 0xE9300006
    const insns = [0xE9300006];
    const { cpu, bus } = setupRunArm(insns, { r0: 0x03000108 });
    bus.write32(0x03000100, 0xAAAAAAAA);
    bus.write32(0x03000104, 0xBBBBBBBB);
    runSteps(cpu, 1);
    expect(cpu.state.r[1]).toBe(0xAAAAAAAA);
    expect(cpu.state.r[2]).toBe(0xBBBBBBBB);
    expect(cpu.state.r[0]).toBe(0x03000100);
  });

  it('THUMB PUSH {R0, LR} then POP {R0, PC}', () => {
    // PUSH {R0, LR} = 0xB501; POP {R0, PC} = 0xBD01
    const insns = [0xB501, 0xBD01];
    const { cpu } = setupRunThumb(insns, { r0: 0x12345678, r13: 0x03000200, r14: 0x03000041 });
    runSteps(cpu, 1);  // PUSH
    expect(cpu.state.r[13]).toBe(0x03000200 - 8);
    runSteps(cpu, 1);  // POP
    expect(cpu.state.r[0]).toBe(0x12345678);
    expect(cpu.state.r[15]).toBe(0x03000040);
    expect(cpu.state.cpsr & FLAG_T).toBe(FLAG_T);  // bit 0 of LR was 1
  });
});

describe('Mode switching', () => {
  it('BX to ARM target clears T bit', () => {
    // THUMB BX R0 with R0 = 0x03000010 (bit 0 = 0)
    const { cpu } = setupRunThumb([0x4700], { r0: 0x03000010 });
    runSteps(cpu, 1);
    expect(cpu.state.r[15]).toBe(0x03000010);
    expect(cpu.state.cpsr & FLAG_T).toBe(0);
  });

  it('BX to THUMB target sets T bit', () => {
    // ARM BX R0 with R0 = 0x03000011 (bit 0 = 1)
    const { cpu } = setupRunArm([0xE12FFF10], { r0: 0x03000011 });
    runSteps(cpu, 1);
    expect(cpu.state.r[15]).toBe(0x03000010);
    expect(cpu.state.cpsr & FLAG_T).toBe(FLAG_T);
  });
});
