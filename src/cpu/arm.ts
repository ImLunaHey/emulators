import { CpuState, FLAG_T, Mode } from './state';
import { immShift, regShift, rorImm32, applyCarry } from './shifter';
import type { Cpu } from './cpu';

// Add/sub flag helpers — return result and set N/Z/C/V on CPSR.
function addSetFlags(s: CpuState, a: number, b: number): number {
  const r = (a + b) >>> 0;
  s.setNZ(r);
  s.setC(r < a >>> 0);
  s.setV(((~(a ^ b) & (a ^ r)) & 0x80000000) !== 0);
  return r;
}
function adcSetFlags(s: CpuState, a: number, b: number, cIn: number): number {
  const sum = a + b + cIn;
  const r = sum >>> 0;
  s.setNZ(r);
  s.setC(sum > 0xFFFFFFFF);
  s.setV(((~(a ^ b) & (a ^ r)) & 0x80000000) !== 0);
  return r;
}
function subSetFlags(s: CpuState, a: number, b: number): number {
  const r = (a - b) >>> 0;
  s.setNZ(r);
  s.setC(a >>> 0 >= b >>> 0);
  s.setV((((a ^ b) & (a ^ r)) & 0x80000000) !== 0);
  return r;
}
function sbcSetFlags(s: CpuState, a: number, b: number, cIn: number): number {
  // ARM SBC: Rd = Rn - Rm - NOT(C)
  const notC = cIn ^ 1;
  const r = (a - b - notC) >>> 0;
  s.setNZ(r);
  s.setC((a >>> 0) >= ((b >>> 0) + notC));
  s.setV((((a ^ b) & (a ^ r)) & 0x80000000) !== 0);
  return r;
}

export function armExecute(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const cond = (instr >>> 28) & 0xF;
  if (cond !== 0xE && !s.checkCond(cond)) return;

  // Branch and Branch with Link: 101x cccc
  if ((instr & 0x0E000000) === 0x0A000000) {
    let offset = (instr & 0x00FFFFFF) << 2;
    if (offset & 0x02000000) offset |= 0xFC000000;
    if (instr & 0x01000000) s.r[14] = (s.r[15] - 4) >>> 0;  // BL: LR = pc+4 of next
    s.r[15] = (s.r[15] + offset) >>> 0;
    cpu.flushPipeline();
    return;
  }

  // BX: branch and exchange — 0001 0010 1111 1111 1111 0001 Rn
  if ((instr & 0x0FFFFFF0) === 0x012FFF10) {
    const rn = instr & 0xF;
    const tgt = s.r[rn];
    if (tgt & 1) {
      s.cpsr |= FLAG_T;
      s.r[15] = tgt & ~1;
    } else {
      s.cpsr &= ~FLAG_T;
      s.r[15] = tgt & ~3;
    }
    cpu.flushPipeline();
    return;
  }

  // SWI
  if ((instr & 0x0F000000) === 0x0F000000) {
    cpu.softwareInterrupt((instr & 0x00FFFFFF) >>> 16);
    return;
  }

  // Block data transfer LDM/STM: 100x
  if ((instr & 0x0E000000) === 0x08000000) {
    armBlockTransfer(cpu, instr);
    return;
  }

  // Single data transfer LDR/STR: 01xx
  if ((instr & 0x0C000000) === 0x04000000) {
    armSingleTransfer(cpu, instr);
    return;
  }

  // Half-word, signed transfer, multiply, swap — these share the 000x major
  // bits but with bit 4 + bit 7 set (the "extension space").
  if ((instr & 0x0E000090) === 0x00000090) {
    // Multiply / multiply long / swap / halfword
    const isHW = (instr & 0x60) !== 0; // any bits in 5/6 → halfword/signed
    if (isHW) {
      armHalfTransfer(cpu, instr);
      return;
    }
    // bit 24 distinguishes multiply (0) vs swap (1).
    if ((instr & 0x01000000) === 0) {
      armMultiply(cpu, instr);
      return;
    }
    armSwap(cpu, instr);
    return;
  }

  // PSR transfer: MRS / MSR. Pattern 0001 0?00 ... (with bit 25 cleared and
  // the specific encoding). Both immediate-form MSR and register-form MRS/MSR
  // fall through here.
  if ((instr & 0x0F900000) === 0x01000000 && (instr & 0x90) !== 0x90) {
    // bit 21 distinguishes MSR (1) from MRS (0).
    if (instr & 0x00200000) { armMsr(cpu, instr); return; }
    armMrs(cpu, instr); return;
  }
  if ((instr & 0x0FB00000) === 0x03200000) { // MSR immediate
    armMsrImm(cpu, instr); return;
  }

  // Data processing (immediate or register operand).
  armDataProcessing(cpu, instr);
}

// ---------------------------------------------------------------- data processing
function armDataProcessing(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const opcode = (instr >>> 21) & 0xF;
  const setFlags = (instr & 0x00100000) !== 0;
  const rn = (instr >>> 16) & 0xF;
  const rd = (instr >>> 12) & 0xF;
  let op1 = s.r[rn];
  let op2: number;
  let shifterCarry = s.c();

  if (instr & 0x02000000) {
    // Immediate operand: rotated 8-bit value.
    const imm = instr & 0xFF;
    const rot = ((instr >>> 8) & 0xF) << 1;
    op2 = rorImm32(imm, rot);
    if (rot !== 0 && setFlags) shifterCarry = (op2 >>> 31) & 1;
  } else {
    const rm = instr & 0xF;
    const shiftType = (instr >>> 5) & 3;
    let rmVal = s.r[rm];
    if (instr & 0x10) {
      // Register-specified shift amount — costs an extra cycle and R15 sees +12.
      const rs = (instr >>> 8) & 0xF;
      const amount = s.r[rs] & 0xFF;
      if (rn === 15) op1 = (op1 + 4) >>> 0;
      if (rm === 15) rmVal = (rmVal + 4) >>> 0;
      const r = regShift(shiftType, amount, rmVal, shifterCarry);
      op2 = r.value;
      shifterCarry = r.carry;
    } else {
      const imm = (instr >>> 7) & 0x1F;
      const r = immShift(shiftType, imm, rmVal, shifterCarry);
      op2 = r.value;
      shifterCarry = r.carry;
    }
  }

  let result = 0;
  let writeResult = true;
  const cIn = s.c();
  switch (opcode) {
    case 0x0: result = (op1 & op2) >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // AND
    case 0x1: result = (op1 ^ op2) >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // EOR
    case 0x2: result = setFlags ? subSetFlags(s, op1, op2) : (op1 - op2) >>> 0; break; // SUB
    case 0x3: result = setFlags ? subSetFlags(s, op2, op1) : (op2 - op1) >>> 0; break; // RSB
    case 0x4: result = setFlags ? addSetFlags(s, op1, op2) : (op1 + op2) >>> 0; break; // ADD
    case 0x5: result = setFlags ? adcSetFlags(s, op1, op2, cIn) : (op1 + op2 + cIn) >>> 0; break; // ADC
    case 0x6: result = setFlags ? sbcSetFlags(s, op1, op2, cIn) : (op1 - op2 - (cIn ^ 1)) >>> 0; break; // SBC
    case 0x7: result = setFlags ? sbcSetFlags(s, op2, op1, cIn) : (op2 - op1 - (cIn ^ 1)) >>> 0; break; // RSC
    case 0x8: writeResult = false; result = (op1 & op2) >>> 0; s.setNZ(result); applyCarry(s, shifterCarry); break; // TST
    case 0x9: writeResult = false; result = (op1 ^ op2) >>> 0; s.setNZ(result); applyCarry(s, shifterCarry); break; // TEQ
    case 0xA: writeResult = false; subSetFlags(s, op1, op2); break; // CMP
    case 0xB: writeResult = false; addSetFlags(s, op1, op2); break; // CMN
    case 0xC: result = (op1 | op2) >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // ORR
    case 0xD: result = op2 >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // MOV
    case 0xE: result = (op1 & ~op2) >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // BIC
    case 0xF: result = (~op2) >>> 0; if (setFlags) { s.setNZ(result); applyCarry(s, shifterCarry); } break; // MVN
  }

  if (writeResult) {
    if (rd === 15) {
      // Writing PC with S bit copies SPSR into CPSR (mode change).
      if (setFlags) {
        const spsr = s.getSpsr();
        s.switchMode(spsr & 0x1F);
        s.cpsr = spsr >>> 0;
      }
      const thumb = (s.cpsr & FLAG_T) !== 0;
      s.r[15] = thumb ? (result & ~1) : (result & ~3);
      cpu.flushPipeline();
    } else {
      s.r[rd] = result >>> 0;
    }
  }
}

// ---------------------------------------------------------------- MRS / MSR
function armMrs(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const rd = (instr >>> 12) & 0xF;
  s.r[rd] = (instr & 0x00400000) ? s.getSpsr() : s.cpsr >>> 0;
}
function armMsr(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const isSpsr = (instr & 0x00400000) !== 0;
  const val = s.r[instr & 0xF];
  applyMsr(s, isSpsr, instr, val);
}
function armMsrImm(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const isSpsr = (instr & 0x00400000) !== 0;
  const imm = instr & 0xFF;
  const rot = ((instr >>> 8) & 0xF) << 1;
  const val = rorImm32(imm, rot);
  applyMsr(s, isSpsr, instr, val);
}
function applyMsr(s: CpuState, isSpsr: boolean, instr: number, val: number): void {
  let mask = 0;
  if (instr & 0x00010000) mask |= 0x000000FF;   // control field — only in privileged modes
  if (instr & 0x00020000) mask |= 0x0000FF00;
  if (instr & 0x00040000) mask |= 0x00FF0000;
  if (instr & 0x00080000) mask |= 0xFF000000;
  if (isSpsr) {
    s.setSpsr((s.getSpsr() & ~mask) | (val & mask));
    return;
  }
  // Don't allow mode change from USR.
  if (s.mode() === Mode.USR) mask &= 0xFF000000;
  const newCpsr = (s.cpsr & ~mask) | (val & mask);
  const newMode = newCpsr & 0x1F;
  if ((newMode !== s.mode())) s.switchMode(newMode);
  s.cpsr = newCpsr >>> 0;
}

// ---------------------------------------------------------------- single transfer LDR/STR
function armSingleTransfer(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const I = (instr & 0x02000000) !== 0;
  const P = (instr & 0x01000000) !== 0;
  const U = (instr & 0x00800000) !== 0;
  const B = (instr & 0x00400000) !== 0;
  const W = (instr & 0x00200000) !== 0;
  const L = (instr & 0x00100000) !== 0;
  const rn = (instr >>> 16) & 0xF;
  const rd = (instr >>> 12) & 0xF;

  let base = s.r[rn];
  let offset: number;
  if (I) {
    const rm = instr & 0xF;
    const shiftType = (instr >>> 5) & 3;
    const imm = (instr >>> 7) & 0x1F;
    offset = immShift(shiftType, imm, s.r[rm], s.c()).value;
  } else {
    offset = instr & 0xFFF;
  }
  const eff = U ? (base + offset) >>> 0 : (base - offset) >>> 0;
  const addr = P ? eff : base;
  const writeback = !P || W;

  if (L) {
    let value: number;
    if (B) {
      value = cpu.bus.read8(addr) >>> 0;
    } else {
      // LDR with unaligned address: read aligned word then rotate.
      const aligned = cpu.bus.read32(addr & ~3) >>> 0;
      const rot = (addr & 3) << 3;
      value = rot ? ((aligned >>> rot) | (aligned << (32 - rot))) >>> 0 : aligned;
    }
    if (writeback && (!L || rn !== rd)) s.r[rn] = eff >>> 0;
    if (rd === 15) {
      s.r[15] = value & ~3;
      cpu.flushPipeline();
    } else {
      s.r[rd] = value >>> 0;
    }
  } else {
    let val = s.r[rd];
    if (rd === 15) val = (val + 4) >>> 0; // STR Rd=PC stores pc+12 of original instr
    if (B) cpu.bus.write8(addr, val & 0xFF);
    else   cpu.bus.write32(addr & ~3, val >>> 0);
    if (writeback) s.r[rn] = eff >>> 0;
  }
}

// ---------------------------------------------------------------- halfword / signed transfer
function armHalfTransfer(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const P = (instr & 0x01000000) !== 0;
  const U = (instr & 0x00800000) !== 0;
  const I = (instr & 0x00400000) !== 0; // immediate offset variant
  const W = (instr & 0x00200000) !== 0;
  const L = (instr & 0x00100000) !== 0;
  const rn = (instr >>> 16) & 0xF;
  const rd = (instr >>> 12) & 0xF;
  const sh = (instr >>> 5) & 3;          // 01 = H, 10 = SB, 11 = SH

  const base = s.r[rn];
  let offset: number;
  if (I) offset = ((instr >>> 4) & 0xF0) | (instr & 0xF);
  else   offset = s.r[instr & 0xF];

  const eff = U ? (base + offset) >>> 0 : (base - offset) >>> 0;
  const addr = P ? eff : base;
  const writeback = !P || W;

  if (L) {
    let value = 0;
    switch (sh) {
      case 1: { // LDRH — unaligned reads rotate
        const aligned = cpu.bus.read16(addr & ~1);
        value = (addr & 1) ? ((aligned >>> 8) | (aligned << 24)) >>> 0 : aligned;
        break;
      }
      case 2: { // LDRSB
        const b = cpu.bus.read8(addr);
        value = (b & 0x80) ? (b | 0xFFFFFF00) >>> 0 : b;
        break;
      }
      case 3: { // LDRSH — unaligned drops low byte → LDRSB
        if (addr & 1) {
          const b = cpu.bus.read8(addr);
          value = (b & 0x80) ? (b | 0xFFFFFF00) >>> 0 : b;
        } else {
          const h = cpu.bus.read16(addr & ~1);
          value = (h & 0x8000) ? (h | 0xFFFF0000) >>> 0 : h;
        }
        break;
      }
    }
    if (writeback && rn !== rd) s.r[rn] = eff >>> 0;
    if (rd === 15) { s.r[15] = value & ~3; cpu.flushPipeline(); }
    else s.r[rd] = value >>> 0;
  } else {
    if (sh === 1) cpu.bus.write16(addr & ~1, s.r[rd] & 0xFFFF);
    if (writeback) s.r[rn] = eff >>> 0;
  }
}

// ---------------------------------------------------------------- multiply
function armMultiply(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const isLong = (instr & 0x00800000) !== 0;
  const setFlags = (instr & 0x00100000) !== 0;
  const accumulate = (instr & 0x00200000) !== 0;
  const rd = (instr >>> 16) & 0xF;
  const rn = (instr >>> 12) & 0xF;
  const rs = (instr >>> 8) & 0xF;
  const rm = instr & 0xF;

  if (!isLong) {
    let r = Math.imul(s.r[rm], s.r[rs]) >>> 0;
    if (accumulate) r = (r + s.r[rn]) >>> 0;
    s.r[rd] = r;
    if (setFlags) s.setNZ(r);
    return;
  }

  const signed = (instr & 0x00400000) !== 0;
  const a = s.r[rm];
  const b = s.r[rs];
  let hi: number, lo: number;
  if (signed) {
    // Signed 64-bit multiply via splitting.
    const a32 = a | 0, b32 = b | 0;
    const big = BigInt(a32) * BigInt(b32);
    lo = Number(big & 0xFFFFFFFFn) >>> 0;
    hi = Number((big >> 32n) & 0xFFFFFFFFn) >>> 0;
  } else {
    const big = BigInt(a >>> 0) * BigInt(b >>> 0);
    lo = Number(big & 0xFFFFFFFFn) >>> 0;
    hi = Number((big >> 32n) & 0xFFFFFFFFn) >>> 0;
  }
  if (accumulate) {
    const accLo = s.r[rn];     // RdLo
    const accHi = s.r[rd];     // RdHi
    const sumLo = (lo + accLo) >>> 0;
    let carry = sumLo < lo >>> 0 ? 1 : 0;
    const sumHi = (hi + accHi + carry) >>> 0;
    lo = sumLo; hi = sumHi;
  }
  s.r[rn] = lo;
  s.r[rd] = hi;
  if (setFlags) s.setNZ64Hi(hi, lo);
}

function armSwap(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const B = (instr & 0x00400000) !== 0;
  const rn = (instr >>> 16) & 0xF;
  const rd = (instr >>> 12) & 0xF;
  const rm = instr & 0xF;
  const addr = s.r[rn];
  if (B) {
    const tmp = cpu.bus.read8(addr);
    cpu.bus.write8(addr, s.r[rm] & 0xFF);
    s.r[rd] = tmp >>> 0;
  } else {
    const aligned = cpu.bus.read32(addr & ~3);
    const rot = (addr & 3) << 3;
    const tmp = rot ? ((aligned >>> rot) | (aligned << (32 - rot))) >>> 0 : aligned;
    cpu.bus.write32(addr & ~3, s.r[rm] >>> 0);
    s.r[rd] = tmp >>> 0;
  }
}

// ---------------------------------------------------------------- block transfer LDM/STM
function armBlockTransfer(cpu: Cpu, instr: number): void {
  const s = cpu.state;
  const P = (instr & 0x01000000) !== 0;
  const U = (instr & 0x00800000) !== 0;
  const Sbit = (instr & 0x00400000) !== 0;
  const W = (instr & 0x00200000) !== 0;
  const L = (instr & 0x00100000) !== 0;
  const rn = (instr >>> 16) & 0xF;
  const list = instr & 0xFFFF;

  let count = 0;
  for (let i = 0; i < 16; i++) if (list & (1 << i)) count++;
  if (count === 0) {
    // Empty list — ARM7TDMI quirk: loads/stores PC, increments by 0x40.
    if (L) { s.r[15] = cpu.bus.read32(s.r[rn] & ~3); cpu.flushPipeline(); }
    else   { cpu.bus.write32(s.r[rn] & ~3, s.r[15]); }
    if (W) s.r[rn] = U ? (s.r[rn] + 0x40) >>> 0 : (s.r[rn] - 0x40) >>> 0;
    return;
  }
  let base = s.r[rn];
  let addr = U ? base : (base - (count << 2)) >>> 0;
  if (U && P) addr = (addr + 4) >>> 0;
  if (!U && !P) addr = (addr + 4) >>> 0;
  const writebackAddr = U ? (base + (count << 2)) >>> 0 : (base - (count << 2)) >>> 0;

  // S bit + R15 in list: load CPSR from SPSR (LDM with PC).
  // S bit without R15: user-mode register bank.
  const userBank = Sbit && !(list & 0x8000);
  const savedMode = s.mode();
  if (userBank) s.switchMode(Mode.USR);

  if (L) {
    let pcLoaded = false;
    for (let i = 0; i < 16; i++) {
      if (!(list & (1 << i))) continue;
      const v = cpu.bus.read32(addr & ~3);
      addr = (addr + 4) >>> 0;
      if (i === 15) {
        if (Sbit) {
          const spsr = s.getSpsr();
          s.switchMode(spsr & 0x1F);
          s.cpsr = spsr >>> 0;
        }
        const thumb = (s.cpsr & FLAG_T) !== 0;
        s.r[15] = thumb ? (v & ~1) : (v & ~3);
        pcLoaded = true;
      } else {
        s.r[i] = v >>> 0;
      }
    }
    if (pcLoaded) cpu.flushPipeline();
  } else {
    // STM with base register in list: ARM7 writes original base if first, new base otherwise.
    let firstStored = false;
    for (let i = 0; i < 16; i++) {
      if (!(list & (1 << i))) continue;
      let v = s.r[i];
      if (i === 15) v = (v + 4) >>> 0;
      if (i === rn && firstStored) v = writebackAddr;
      cpu.bus.write32(addr & ~3, v >>> 0);
      addr = (addr + 4) >>> 0;
      firstStored = true;
    }
  }

  if (userBank) s.switchMode(savedMode);
  if (W) s.r[rn] = writebackAddr;
}
