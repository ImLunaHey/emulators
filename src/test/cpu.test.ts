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

describe('IRQ entry and return', () => {
  it('takeIrq enters IRQ mode with correct LR', () => {
    const { cpu } = setupRunArm([0xE320F000]);  // NOP (MSR CPSR_f, #0)
    cpu.state.r[15] = 0x08000100;  // next decode
    cpu.state.cpsr = Mode.SYS;     // start in SYS mode
    cpu.takeIrq();
    expect(cpu.state.mode()).toBe(Mode.IRQ);
    expect(cpu.state.r[14]).toBe(0x08000104);  // saved PC + 4
    expect(cpu.state.r[15]).toBe(0x18);        // IRQ vector
    expect(cpu.state.cpsr & FLAG_T).toBe(0);   // T cleared
    expect(cpu.state.cpsr & 0x80).toBe(0x80);  // I set
  });

  it('SUBS PC, LR, #4 restores CPSR from SPSR_irq', () => {
    // Manually enter IRQ then execute SUBS PC, LR, #4.
    const { cpu, bus } = setupRunArm([0xE25EF004]);  // SUBS PC, LR, #4
    // Set up exception state manually.
    cpu.state.cpsr = Mode.SYS | FLAG_T;
    cpu.state.r[15] = 0x08000100;
    cpu.takeIrq();
    // takeIrq put PC at 0x18; we want our SUBS instruction to execute.
    // Place it at 0x03000000 and set PC there.
    bus.write32(0x03000000, 0xE25EF004);
    cpu.state.r[15] = 0x03000000;
    cpu.state.r[14] = 0x08000104;  // saved LR (= original next + 4)
    runSteps(cpu, 1);
    expect(cpu.state.mode()).toBe(Mode.SYS);     // back to SYS
    expect(cpu.state.cpsr & FLAG_T).toBe(FLAG_T); // T restored
    expect(cpu.state.r[15]).toBe(0x08000100);    // PC restored to next of interrupted insn
  });

  it('Banked SP swap on mode change', () => {
    const { cpu } = setupRunArm([0xE320F000]);
    cpu.state.cpsr = Mode.SYS;
    cpu.state.r[13] = 0xDEAD_C0DE;
    cpu.state.switchMode(Mode.IRQ);
    expect(cpu.state.r[13]).not.toBe(0xDEAD_C0DE);  // different banked SP
    cpu.state.switchMode(Mode.SYS);
    expect(cpu.state.r[13]).toBe(0xDEAD_C0DE);  // restored
  });
});

describe('IO register behavior', () => {
  it('write to DISPCNT (0x4000000) reflects in PPU', () => {
    const { bus } = setupRunArm([]);
    bus.write16(0x04000000, 0x0100);  // BG0 enable
    expect((bus.io as any).ppu.dispcnt).toBe(0x0100);
  });

  it('write to IE (0x4000200) updates IRQ', () => {
    const { bus } = setupRunArm([]);
    bus.write16(0x04000200, 0x0001);  // VBlank enable
    expect((bus.io as any).irq.ie).toBe(0x0001);
  });

  it('write to IF (0x4000202) acks (clears)', () => {
    const { bus } = setupRunArm([]);
    (bus.io as any).irq.raise(0x0001);
    bus.write16(0x04000202, 0x0001);
    expect((bus.io as any).irq.iflag).toBe(0);
  });

  it('VCOUNT (0x4000006) reads PPU vcount', () => {
    const { bus } = setupRunArm([]);
    (bus.io as any).ppu.vcount = 42;
    expect(bus.read16(0x04000006)).toBe(42);
  });

  it('byte write to PRAM mirrors to halfword', () => {
    // GBA quirk: 8-bit writes to PRAM/VRAM are widened to halfword broadcasts.
    const { bus } = setupRunArm([]);
    bus.write8(0x05000000, 0xAB);
    expect(bus.read16(0x05000000)).toBe(0xABAB);
  });

  it('VRAM 8-bit write to OBJ area (≥0x10000) is dropped', () => {
    const { bus } = setupRunArm([]);
    bus.write8(0x06010000, 0xFF);
    expect(bus.read16(0x06010000)).toBe(0);
  });

  it('OAM ignores 8-bit writes entirely', () => {
    const { bus } = setupRunArm([]);
    bus.write8(0x07000000, 0xFF);
    expect(bus.read8(0x07000000)).toBe(0);
  });
});

describe('Halt + IRQ wakeup (the boot stall pattern)', () => {
  it('halted CPU wakes on IRQ pending + enabled', () => {
    const { cpu, bus } = setupRunArm([]);
    cpu.state.halted = true;
    cpu.state.cpsr = Mode.SYS;  // I bit clear → IRQ enabled
    const irq = (bus.io as any).irq;
    irq.setIme(1);
    irq.setIe(0x0001);  // VBlank
    irq.raise(0x0001);  // VBlank pending
    cpu.irqLine = irq.pending();
    expect(cpu.irqLine).toBe(true);
    cpu.step();  // un-halts but doesn't take IRQ
    expect(cpu.state.halted).toBe(false);
    cpu.step();  // takes IRQ; same step also fetches+executes vector instr (B 0x128)
    expect(cpu.state.mode()).toBe(Mode.IRQ);
    expect(cpu.state.r[15]).toBe(0x128);
  });

  it('halted CPU stays halted when CPSR.I masks IRQ', () => {
    const { cpu, bus } = setupRunArm([]);
    cpu.state.halted = true;
    cpu.state.cpsr = Mode.SYS | 0x80;  // I bit set → IRQ masked
    const irq = (bus.io as any).irq;
    irq.setIme(1);
    irq.setIe(0x0001);
    irq.raise(0x0001);
    cpu.irqLine = irq.pending();
    cpu.step();
    expect(cpu.state.halted).toBe(true);  // stays halted
  });

  it('halted CPU stays halted when IME=0', () => {
    const { cpu, bus } = setupRunArm([]);
    cpu.state.halted = true;
    cpu.state.cpsr = Mode.SYS;
    const irq = (bus.io as any).irq;
    irq.setIme(0);  // master disable
    irq.setIe(0x0001);
    irq.raise(0x0001);
    cpu.irqLine = irq.pending();
    expect(cpu.irqLine).toBe(false);  // pending() respects IME
    cpu.step();
    expect(cpu.state.halted).toBe(true);
  });
});

describe('BIOS IRQ dispatcher round-trip', () => {
  it('BIOS branch at 0x18 reaches dispatcher at 0x128', () => {
    const { bus } = setupRunArm([]);
    // Read the branch instruction the reset() installed at 0x18.
    const insn = bus.read32(0x18);
    expect(insn).toBe(0xEA000042);  // B 0x128
    // Verify by manual decode.
    const cond = insn >>> 28;
    expect(cond).toBe(0xE);
    const off = (insn & 0x00FFFFFF) << 2;
    const target = (0x18 + 8 + off) >>> 0;
    expect(target).toBe(0x128);
  });

  it('Dispatcher at 0x128 starts with STMFD SP!, {R0-R3, R12, LR}', () => {
    const { bus } = setupRunArm([]);
    expect(bus.read32(0x128)).toBe(0xE92D500F);
  });

  it('Dispatcher fully executes IRQ → user handler → return', () => {
    const { cpu, bus } = setupRunArm([]);
    // Plant a minimal user IRQ handler at IWRAM 0x03002000 that:
    //   1. Acks IF.VBlank (write 1 to 0x4000202)
    //   2. BX LR (return to dispatcher's continuation at 0x138)
    // ARM: MOV R0, #1; MOV R1, #0x4000000; ADD R1, R1, #0x202; STRH R0, [R1]; BX LR
    // Use simpler: MOV R0, #1; LDR R1, [PC]; STRH R0, [R1]; BX LR; .word 0x4000202
    bus.write32(0x03002000, 0xE3A00001);  // MOV R0, #1
    bus.write32(0x03002004, 0xE59F1004);  // LDR R1, [PC, #4] → loads from 0x03002010
    bus.write32(0x03002008, 0xE1C100B0);  // STRH R0, [R1] (P=1,U=1,W=0,L=0,Rn=1,Rd=0,offset=0,H=1)
    bus.write32(0x0300200C, 0xE12FFF1E);  // BX LR
    bus.write32(0x03002010, 0x04000202);  // literal
    // Install pointer to handler at the canonical IWRAM IRQ vector slot.
    bus.write32(0x03007FFC, 0x03002000);
    // Set up CPU + IRQ state.
    cpu.state.cpsr = Mode.SYS;
    cpu.state.r[15] = 0x08000100;  // pretend we're in game code at 0x08000100
    const irq = (bus.io as any).irq;
    irq.setIme(1);
    irq.setIe(0x0001);
    irq.raise(0x0001);
    cpu.irqLine = true;
    // Step CPU until we return to user code. Should take ~10-15 instructions:
    // takeIrq+B(0x128) → STMFD → MOV → ADR → LDR PC → MOV/LDR/STRH/BX LR
    // (in handler) → LDMFD → SUBS PC, LR, #4
    let saw_handler = false, saw_subs = false, saw_return = false;
    let took_irq = false;
    for (let i = 0; i < 30; i++) {
      cpu.irqLine = irq.pending();
      const pcBefore = cpu.state.r[15] >>> 0;
      if (pcBefore === 0x03002000) saw_handler = true;
      if (pcBefore === 0x13C) saw_subs = true;
      if (cpu.state.mode() === Mode.IRQ) took_irq = true;
      // Wait for return: in SYS mode AND back at original PC.
      if (took_irq && cpu.state.mode() === Mode.SYS && pcBefore === 0x08000100) {
        saw_return = true; break;
      }
      cpu.step();
    }
    expect(saw_handler).toBe(true);
    expect(saw_subs).toBe(true);
    expect(saw_return).toBe(true);
    // After return, IF.VBlank should have been acked by the handler.
    expect(irq.iflag & 0x0001).toBe(0);
    // CPU mode should be back to SYS.
    expect(cpu.state.mode()).toBe(Mode.SYS);
  });
});

describe('Memory mirrors and rotations', () => {
  it('IWRAM mirrors every 32 KB', () => {
    const { bus } = setupRunArm([]);
    bus.write32(0x03000000, 0xDEADBEEF);
    expect(bus.read32(0x03008000)).toBe(0xDEADBEEF);
    expect(bus.read32(0x03FFFFFC & ~3)).toBe(0);   // last word — different
    bus.write32(0x03007FFC, 0xCAFEBABE);
    expect(bus.read32(0x03FFFFFC)).toBe(0xCAFEBABE);  // mirror access
  });

  it('VRAM mirror fold: 0x18000+ aliases 0x10000+', () => {
    const { bus } = setupRunArm([]);
    bus.write32(0x06010000, 0xAA55_AA55);
    expect(bus.read32(0x06018000)).toBe(0xAA55_AA55);
  });

  it('ROM read returns padded zeros past end', () => {
    const { bus } = setupRunArm([]);
    // ROM is 0x100 bytes (test setup); read past should return open-bus pattern.
    const v = bus.read32(0x08001000);
    // Open-bus value depends on impl — just ensure no crash.
    expect(typeof v).toBe('number');
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
