// Per-format THUMB instruction tests. The CPU bug list so far:
//   - PC-pipeline confusion in branches
//   - Branch detection via r[15] === visible
//   - Format 7/8 dispatch (LDR/STR reg-offset misrouted to PC-rel)
// These tests cover every encoding density we have so a future bug
// fails a named test instead of silently corrupting a game.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';

function makeCpu() {
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
  // Enter THUMB SYS mode at 0x03000000 so we have writable code memory.
  cpu.state.cpsr = 0x1F | 0x20;  // SYS + T
  cpu.state.r[15] = 0x03000000;
  cpu.state.r[13] = 0x03007F00;
  return { cpu, bus };
}

// Helper: write `insns` (halfwords) into IWRAM at 0x03000000 and step N times.
function load(cpu: ReturnType<typeof makeCpu>['cpu'], bus: Bus, insns: number[]) {
  for (let i = 0; i < insns.length; i++) {
    bus.write16(0x03000000 + i * 2, insns[i] & 0xFFFF);
  }
}

describe('THUMB Format 1: LSL/LSR/ASR imm', () => {
  it('LSL R1, R0, #5', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x00000003;
    load(cpu, bus, [0x0141]);  // LSL R1, R0, #5
    cpu.step();
    expect(cpu.state.r[1]).toBe(0x00000060);
  });
  it('LSR R2, R1, #4 with carry-out', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[1] = 0xFF;
    load(cpu, bus, [0x110A]);  // LSR R2, R1, #4
    cpu.step();
    expect(cpu.state.r[2]).toBe(0x0F);
    expect((cpu.state.cpsr & 0x20000000) !== 0).toBe(true);  // C set (bit 3 shifted out)
  });
  it('ASR R3, R2, #1 with negative', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[2] = 0x80000000;
    load(cpu, bus, [0x1053]);  // ASR R3, R2, #1
    cpu.step();
    expect(cpu.state.r[3]).toBe(0xC0000000);
  });
});

describe('THUMB Format 2: ADD/SUB reg/imm3', () => {
  it('ADD R0, R1, R2 (reg)', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[1] = 5; cpu.state.r[2] = 7;
    load(cpu, bus, [0x1888]);  // ADD R0, R1, R2
    cpu.step();
    expect(cpu.state.r[0]).toBe(12);
  });
  it('SUB R0, R1, #3', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[1] = 10;
    load(cpu, bus, [0x1EC8]);  // SUB R0, R1, #3
    cpu.step();
    expect(cpu.state.r[0]).toBe(7);
  });
});

describe('THUMB Format 3: MOV/CMP/ADD/SUB imm', () => {
  it('MOV R0, #0xFF', () => {
    const { cpu, bus } = makeCpu();
    load(cpu, bus, [0x20FF]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xFF);
  });
  it('CMP R0, #5 sets Z when equal', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 5;
    load(cpu, bus, [0x2805]);
    cpu.step();
    expect((cpu.state.cpsr & 0x40000000) !== 0).toBe(true);  // Z
  });
});

describe('THUMB Format 4: ALU register operations', () => {
  it('AND R0, R1', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xFF; cpu.state.r[1] = 0x0F;
    load(cpu, bus, [0x4008]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0x0F);
  });
  it('EOR R0, R1', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xFF; cpu.state.r[1] = 0x0F;
    load(cpu, bus, [0x4048]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xF0);
  });
  it('NEG R0, R1', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[1] = 5;
    load(cpu, bus, [0x4248]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xFFFFFFFB);
  });
  it('MUL R0, R1', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 7; cpu.state.r[1] = 6;
    load(cpu, bus, [0x4348]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(42);
  });
  it('ROR R0, R1 (rotate by R1)', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x12345678; cpu.state.r[1] = 8;
    load(cpu, bus, [0x41C8]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0x78123456);
  });
});

describe('THUMB Format 5: Hi-register ops + BX', () => {
  it('ADD R8, R0 (high register)', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[8] = 0x100; cpu.state.r[0] = 0x10;
    load(cpu, bus, [0x4480]);  // ADD R8, R0
    cpu.step();
    expect(cpu.state.r[8]).toBe(0x110);
  });
  it('MOV R8, R0', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xDEADBEEF;
    load(cpu, bus, [0x4680]);
    cpu.step();
    expect(cpu.state.r[8]).toBe(0xDEADBEEF);
  });
  it('BX R0 with bit-0 set → stays THUMB', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x03000011;  // THUMB target, bit 0 = 1
    load(cpu, bus, [0x4700]);
    cpu.step();
    expect(cpu.state.r[15] & ~1).toBe(0x03000010);
    expect((cpu.state.cpsr & 0x20) !== 0).toBe(true);
  });
  it('BX R0 with bit-0 clear → switches to ARM', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x03000020;
    load(cpu, bus, [0x4700]);
    cpu.step();
    expect(cpu.state.r[15]).toBe(0x03000020);
    expect((cpu.state.cpsr & 0x20) !== 0).toBe(false);
  });
});

describe('THUMB Format 6: PC-relative load', () => {
  it('LDR R0, [PC, #4]', () => {
    const { cpu, bus } = makeCpu();
    // PC at 0x03000000, visible PC = 0x03000004, offset 4*4 = 16 → load from 0x03000014.
    bus.write32(0x03000014, 0xCAFEBABE);
    load(cpu, bus, [0x4804]);  // LDR R0, [PC, #16] (offset = 4*4)
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xCAFEBABE);
  });
});

describe('THUMB Format 7: load/store register offset', () => {
  it('STR R0, [R1, R2] then LDR R3, [R1, R2]', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xABCDEF01; cpu.state.r[1] = 0x03001000; cpu.state.r[2] = 8;
    load(cpu, bus, [0x5088, 0x588B]);  // STR R0,[R1,R2]; LDR R3,[R1,R2]
    cpu.step(); cpu.step();
    expect(bus.read32(0x03001008)).toBe(0xABCDEF01);
    expect(cpu.state.r[3]).toBe(0xABCDEF01);
  });
  it('STRB R0, [R1, R2] then LDRB R3, [R1, R2]', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xAB; cpu.state.r[1] = 0x03001000; cpu.state.r[2] = 4;
    load(cpu, bus, [0x5488, 0x5C8B]);
    cpu.step(); cpu.step();
    expect(bus.read8(0x03001004)).toBe(0xAB);
    expect(cpu.state.r[3]).toBe(0xAB);
  });
});

describe('THUMB Format 8: signed load/store halfword', () => {
  it('LDRSH R0, [R1, R2] sign-extends', () => {
    const { cpu, bus } = makeCpu();
    bus.write16(0x03001000, 0xFFF0);
    cpu.state.r[1] = 0x03001000; cpu.state.r[2] = 0;
    load(cpu, bus, [0x5E88]);  // LDRSH R0, [R1, R2]
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xFFFFFFF0);
  });
  it('LDRSB R0, [R1, R2] sign-extends', () => {
    const { cpu, bus } = makeCpu();
    bus.write8(0x03001000, 0x80);
    cpu.state.r[1] = 0x03001000; cpu.state.r[2] = 0;
    load(cpu, bus, [0x5688]);  // LDRSB R0, [R1, R2]
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xFFFFFF80);
  });
  it('STRH R0, [R1, R2] writes halfword', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x12345678; cpu.state.r[1] = 0x03001000; cpu.state.r[2] = 0;
    load(cpu, bus, [0x5288]);  // STRH R0, [R1, R2]
    cpu.step();
    expect(bus.read16(0x03001000)).toBe(0x5678);
  });
});

describe('THUMB Format 9/10: imm-offset load/store', () => {
  it('LDR R0, [R1, #4]', () => {
    const { cpu, bus } = makeCpu();
    bus.write32(0x03001004, 0xDEADBEEF);
    cpu.state.r[1] = 0x03001000;
    load(cpu, bus, [0x6848]);
    cpu.step();
    expect(cpu.state.r[0]).toBe(0xDEADBEEF);
  });
  it('STRH R0, [R1, #6]', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xCAFE; cpu.state.r[1] = 0x03001000;
    load(cpu, bus, [0x80C8]);
    cpu.step();
    expect(bus.read16(0x03001006)).toBe(0xCAFE);
  });
});

describe('THUMB Format 11: SP-relative load/store', () => {
  it('LDR R0, [SP, #8]', () => {
    const { cpu, bus } = makeCpu();
    bus.write32(cpu.state.r[13] + 8, 0x11223344);
    load(cpu, bus, [0x9802]);  // LDR R0, [SP, #8]
    cpu.step();
    expect(cpu.state.r[0]).toBe(0x11223344);
  });
  it('STR R0, [SP, #4]', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0x55667788;
    load(cpu, bus, [0x9001]);
    cpu.step();
    expect(bus.read32(cpu.state.r[13] + 4)).toBe(0x55667788);
  });
});

describe('THUMB Format 13: ADD SP, #imm (signed)', () => {
  it('ADD SP, #16', () => {
    const { cpu, bus } = makeCpu();
    const sp = cpu.state.r[13];
    load(cpu, bus, [0xB004]);  // ADD SP, #16
    cpu.step();
    expect(cpu.state.r[13]).toBe(sp + 16);
  });
  it('ADD SP, #-16', () => {
    const { cpu, bus } = makeCpu();
    const sp = cpu.state.r[13];
    load(cpu, bus, [0xB084]);  // ADD SP, #-16 (sign bit set)
    cpu.step();
    expect(cpu.state.r[13]).toBe((sp - 16) >>> 0);
  });
});

describe('THUMB Format 14: PUSH/POP', () => {
  it('PUSH {R0, R1, LR} then POP {R2, R3, PC}', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[0] = 0xAAAA; cpu.state.r[1] = 0xBBBB; cpu.state.r[14] = 0x03000040 | 1;
    // PUSH {R0, R1, LR}  → 0xB503  (M=1 = LR, list = 0x03)
    // POP  {R2, R3, PC}  → 0xBD0C  (M=1 = PC, list = 0x0C)
    load(cpu, bus, [0xB503, 0xBD0C]);
    cpu.step();  // PUSH
    cpu.step();  // POP — PC restored to LR (with THUMB bit)
    expect(cpu.state.r[2]).toBe(0xAAAA);
    expect(cpu.state.r[3]).toBe(0xBBBB);
    expect(cpu.state.r[15] & ~1).toBe(0x03000040);
  });
});

describe('THUMB Format 16: conditional branch', () => {
  it('BEQ taken when Z set', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.cpsr |= 0x40000000;  // Z
    // BEQ +4 (offset 2 → target = pc+8 = 0x03000008)
    load(cpu, bus, [0xD002]);
    cpu.step();
    expect(cpu.state.r[15] & ~1).toBe(0x03000008);
  });
  it('BNE NOT taken when Z set', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.cpsr |= 0x40000000;
    load(cpu, bus, [0xD102]);
    cpu.step();
    expect(cpu.state.r[15] & ~1).toBe(0x03000002);
  });
});

describe('THUMB Format 19: BL (long branch with link)', () => {
  it('BL forward +0x100', () => {
    const { cpu, bus } = makeCpu();
    // BL forward: BL high (F000) + offset bits 11-22 of target_offset; BL low (F800) + offset bits 1-11.
    // Target = PC + 4 + (signed 23-bit << 1) = 0x03000004 + 0x100 = 0x03000104.
    // Offset = 0x100. High = 0x100 >> 12 = 0. Low = (0x100 >> 1) & 0x7FF = 0x80.
    load(cpu, bus, [0xF000, 0xF880]);
    cpu.step(); cpu.step();
    expect(cpu.state.r[15] & ~1).toBe(0x03000104);
    // LR should have THUMB bit + return addr.
    expect(cpu.state.r[14]).toBe(0x03000004 | 1);
  });

  it('BL backward', () => {
    const { cpu, bus } = makeCpu();
    cpu.state.r[15] = 0x03000100;
    load(cpu, bus, []);
    bus.write16(0x03000100, 0xF7FF);  // BL high: offset = 0xFFFFF, top 11 bits of -2
    bus.write16(0x03000102, 0xFFFE);  // BL low: bottom 11 bits of -2 → -4
    cpu.step(); cpu.step();
    // Target = 0x03000104 + (-4) = 0x03000100 (back to start).
    expect(cpu.state.r[15] & ~1).toBe(0x03000100);
  });
});
