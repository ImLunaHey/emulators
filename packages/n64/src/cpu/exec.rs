//! VR4300 instruction interpreter — the MIPS III integer instruction set with
//! branch delay slots, MULT/DIV with HI/LO, the 64-bit doubleword ops, COP0
//! (MFC0/MTC0/ERET/TLB), and a partial COP1 (FPU load/store/move/arith).
//!
//! Built from the VR4300 user manual instruction reference. The interpreter is
//! a free function over a `&mut Cpu` and a `&mut dyn Bus` so the god-struct can
//! drive it with the `mem::take` pattern (the bus *is* the rest of the machine,
//! borrowed mutably while the CPU is taken out).
//!
//! Decoding follows the standard MIPS layout: bits 31..26 the primary opcode,
//! SPECIAL (0) dispatches on the funct field (bits 5..0), REGIMM (1) on the rt
//! field, COP0/COP1 (16/17) on the rs field.

use super::cop0::{self, Exception};
use super::state::Cpu;
use crate::bus::Bus;
use crate::regions::virt_to_phys;

/// Decode the standard fields of a MIPS instruction word.
#[inline]
fn op(i: u32) -> u32 {
    i >> 26
}
#[inline]
fn rs(i: u32) -> u32 {
    (i >> 21) & 0x1F
}
#[inline]
fn rt(i: u32) -> u32 {
    (i >> 16) & 0x1F
}
#[inline]
fn rd(i: u32) -> u32 {
    (i >> 11) & 0x1F
}
#[inline]
fn sa(i: u32) -> u32 {
    (i >> 6) & 0x1F
}
#[inline]
fn funct(i: u32) -> u32 {
    i & 0x3F
}
#[inline]
fn imm16(i: u32) -> u32 {
    i & 0xFFFF
}
/// Sign-extended 16-bit immediate as a 64-bit value.
#[inline]
fn imm_se(i: u32) -> u64 {
    (i & 0xFFFF) as i16 as i64 as u64
}
#[inline]
fn target26(i: u32) -> u32 {
    i & 0x03FF_FFFF
}

/// Execute exactly one instruction. Commits the branch-delay PC pair, fetches
/// at `pc`, decodes and runs it. The host's step loop calls this; it samples
/// interrupts before invoking us.
pub fn step(cpu: &mut Cpu, bus: &mut dyn Bus) {
    cpu.exception_pending = false;

    // Advance the branch-delay machinery: the instruction we are about to run
    // sits in a delay slot iff the *previous* one took a branch.
    cpu.current_pc = cpu.pc;
    cpu.in_delay_slot = cpu.branch_taken;
    cpu.branch_taken = false;

    // Fetch (PC is virtual; fold to physical for the bus). A misaligned PC is
    // an instruction address error.
    if cpu.pc & 3 != 0 {
        cpu.raise_address_error(cpu.pc, false);
        return;
    }
    let phys = virt_to_phys(cpu.pc as u32);
    let instr = bus.fetch32(phys);

    // Step the PC pair forward; a branch will rewrite next_pc.
    cpu.pc = cpu.next_pc;
    cpu.next_pc = cpu.next_pc.wrapping_add(4);

    execute(cpu, bus, instr);

    cpu.cycles = cpu.cycles.wrapping_add(1);
}

/// Take a relative branch: rewrite `next_pc` to (delay-slot PC) + offset.
#[inline]
fn branch(cpu: &mut Cpu, offset: u64) {
    // At this point cpu.pc already points at the delay slot (step advanced it).
    cpu.next_pc = cpu.pc.wrapping_add(offset);
    cpu.branch_taken = true;
}

/// A "likely" branch that is NOT taken nullifies its delay slot: skip the next
/// instruction by advancing the PC pair one extra word.
#[inline]
fn nullify_delay_slot(cpu: &mut Cpu) {
    cpu.pc = cpu.next_pc;
    cpu.next_pc = cpu.next_pc.wrapping_add(4);
}

fn execute(cpu: &mut Cpu, bus: &mut dyn Bus, i: u32) {
    match op(i) {
        0x00 => special(cpu, bus, i),
        0x01 => regimm(cpu, i),
        0x02 => {
            // J target
            let tgt = (cpu.pc & 0xFFFF_FFFF_F000_0000) | ((target26(i) as u64) << 2);
            cpu.next_pc = tgt;
            cpu.branch_taken = true;
        }
        0x03 => {
            // JAL target
            cpu.set_reg(31, cpu.next_pc);
            let tgt = (cpu.pc & 0xFFFF_FFFF_F000_0000) | ((target26(i) as u64) << 2);
            cpu.next_pc = tgt;
            cpu.branch_taken = true;
        }
        0x04 => {
            // BEQ
            if cpu.reg(rs(i)) == cpu.reg(rt(i)) {
                branch(cpu, imm_se(i) << 2);
            }
        }
        0x05 => {
            // BNE
            if cpu.reg(rs(i)) != cpu.reg(rt(i)) {
                branch(cpu, imm_se(i) << 2);
            }
        }
        0x06 => {
            // BLEZ
            if (cpu.reg(rs(i)) as i64) <= 0 {
                branch(cpu, imm_se(i) << 2);
            }
        }
        0x07 => {
            // BGTZ
            if (cpu.reg(rs(i)) as i64) > 0 {
                branch(cpu, imm_se(i) << 2);
            }
        }
        0x08 => {
            // ADDI (with overflow trap)
            let a = cpu.reg(rs(i)) as i32;
            match a.checked_add(imm_se(i) as i32) {
                Some(r) => cpu.set_reg32(rt(i), r as u32),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x09 => {
            // ADDIU (no trap)
            let r = (cpu.reg(rs(i)) as i32).wrapping_add(imm_se(i) as i32);
            cpu.set_reg32(rt(i), r as u32);
        }
        0x0A => {
            // SLTI (signed)
            let r = ((cpu.reg(rs(i)) as i64) < (imm_se(i) as i64)) as u64;
            cpu.set_reg(rt(i), r);
        }
        0x0B => {
            // SLTIU (unsigned)
            let r = (cpu.reg(rs(i)) < imm_se(i)) as u64;
            cpu.set_reg(rt(i), r);
        }
        0x0C => {
            // ANDI
            cpu.set_reg(rt(i), cpu.reg(rs(i)) & imm16(i) as u64);
        }
        0x0D => {
            // ORI
            cpu.set_reg(rt(i), cpu.reg(rs(i)) | imm16(i) as u64);
        }
        0x0E => {
            // XORI
            cpu.set_reg(rt(i), cpu.reg(rs(i)) ^ imm16(i) as u64);
        }
        0x0F => {
            // LUI: load upper immediate, sign-extended (32-bit value << 16)
            cpu.set_reg32(rt(i), imm16(i) << 16);
        }
        0x10 => cop0_op(cpu, i),
        0x11 => cop1_op(cpu, bus, i),
        0x14 => {
            // BEQL (likely)
            if cpu.reg(rs(i)) == cpu.reg(rt(i)) {
                branch(cpu, imm_se(i) << 2);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x15 => {
            // BNEL
            if cpu.reg(rs(i)) != cpu.reg(rt(i)) {
                branch(cpu, imm_se(i) << 2);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x16 => {
            // BLEZL
            if (cpu.reg(rs(i)) as i64) <= 0 {
                branch(cpu, imm_se(i) << 2);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x17 => {
            // BGTZL
            if (cpu.reg(rs(i)) as i64) > 0 {
                branch(cpu, imm_se(i) << 2);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x18 => {
            // DADDI (64-bit add with overflow trap)
            let a = cpu.reg(rs(i)) as i64;
            match a.checked_add(imm_se(i) as i64) {
                Some(r) => cpu.set_reg(rt(i), r as u64),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x19 => {
            // DADDIU
            let r = (cpu.reg(rs(i)) as i64).wrapping_add(imm_se(i) as i64);
            cpu.set_reg(rt(i), r as u64);
        }
        0x1A => load_left_right_d(cpu, bus, i, true),  // LDL
        0x1B => load_left_right_d(cpu, bus, i, false), // LDR
        0x20 => {
            // LB (sign-extended byte)
            if let Some(a) = addr(cpu, bus, i, 1, false) {
                let v = bus.read8(a) as i8 as i64 as u64;
                cpu.set_reg(rt(i), v);
            }
        }
        0x21 => {
            // LH (sign-extended halfword)
            if let Some(a) = addr(cpu, bus, i, 2, false) {
                let v = bus.read16(a) as i16 as i64 as u64;
                cpu.set_reg(rt(i), v);
            }
        }
        0x22 => load_left_right_w(cpu, bus, i, true),  // LWL
        0x23 => {
            // LW (sign-extended word)
            if let Some(a) = addr(cpu, bus, i, 4, false) {
                cpu.set_reg32(rt(i), bus.read32(a));
            }
        }
        0x24 => {
            // LBU
            if let Some(a) = addr(cpu, bus, i, 1, false) {
                cpu.set_reg(rt(i), bus.read8(a) as u64);
            }
        }
        0x25 => {
            // LHU
            if let Some(a) = addr(cpu, bus, i, 2, false) {
                cpu.set_reg(rt(i), bus.read16(a) as u64);
            }
        }
        0x26 => load_left_right_w(cpu, bus, i, false), // LWR
        0x27 => {
            // LWU (zero-extended word, MIPS III)
            if let Some(a) = addr(cpu, bus, i, 4, false) {
                cpu.set_reg(rt(i), bus.read32(a) as u64);
            }
        }
        0x28 => {
            // SB
            if let Some(a) = addr(cpu, bus, i, 1, true) {
                bus.write8(a, cpu.reg(rt(i)) as u8);
            }
        }
        0x29 => {
            // SH
            if let Some(a) = addr(cpu, bus, i, 2, true) {
                bus.write16(a, cpu.reg(rt(i)) as u16);
            }
        }
        0x2A => store_left_right_w(cpu, bus, i, true), // SWL
        0x2B => {
            // SW
            if let Some(a) = addr(cpu, bus, i, 4, true) {
                bus.write32(a, cpu.reg(rt(i)) as u32);
            }
        }
        0x2C => store_left_right_d(cpu, bus, i, true),  // SDL
        0x2D => store_left_right_d(cpu, bus, i, false), // SDR
        0x2E => store_left_right_w(cpu, bus, i, false), // SWR
        0x2F => { /* CACHE — no-op for the foundation */ }
        0x30 => {
            // LL (load linked)
            if let Some(a) = addr(cpu, bus, i, 4, false) {
                cpu.set_reg32(rt(i), bus.read32(a));
                cpu.ll_bit = true;
                cpu.cop0.reg[cop0::R_LLADDR] = (a >> 4) as u64;
            }
        }
        0x31 => {
            // LWC1
            if let Some(a) = addr(cpu, bus, i, 4, false) {
                let v = bus.read32(a);
                cpu.cop1.write_w(rt(i) as usize, v);
            }
        }
        0x34 => {
            // LLD (doubleword)
            if let Some(a) = addr(cpu, bus, i, 8, false) {
                cpu.set_reg(rt(i), bus.read64(a));
                cpu.ll_bit = true;
            }
        }
        0x35 => {
            // LDC1
            if let Some(a) = addr(cpu, bus, i, 8, false) {
                let v = bus.read64(a);
                cpu.cop1.write_dw(rt(i) as usize, v);
            }
        }
        0x37 => {
            // LD (doubleword)
            if let Some(a) = addr(cpu, bus, i, 8, false) {
                cpu.set_reg(rt(i), bus.read64(a));
            }
        }
        0x38 => {
            // SC (store conditional)
            if let Some(a) = addr(cpu, bus, i, 4, true) {
                if cpu.ll_bit {
                    bus.write32(a, cpu.reg(rt(i)) as u32);
                    cpu.set_reg(rt(i), 1);
                } else {
                    cpu.set_reg(rt(i), 0);
                }
            }
        }
        0x39 => {
            // SWC1
            if let Some(a) = addr(cpu, bus, i, 4, true) {
                bus.write32(a, cpu.cop1.read_w(rt(i) as usize));
            }
        }
        0x3C => {
            // SCD
            if let Some(a) = addr(cpu, bus, i, 8, true) {
                if cpu.ll_bit {
                    bus.write64(a, cpu.reg(rt(i)));
                    cpu.set_reg(rt(i), 1);
                } else {
                    cpu.set_reg(rt(i), 0);
                }
            }
        }
        0x3D => {
            // SDC1
            if let Some(a) = addr(cpu, bus, i, 8, true) {
                bus.write64(a, cpu.cop1.read_dw(rt(i) as usize));
            }
        }
        0x3F => {
            // SD (doubleword)
            if let Some(a) = addr(cpu, bus, i, 8, true) {
                bus.write64(a, cpu.reg(rt(i)));
            }
        }
        _ => cpu.raise(Exception::ReservedInstruction, 0),
    }
}

/// Compute and validate a load/store effective address: rs + sign-extended
/// immediate, folded to physical. Raises an address-error exception on a
/// misaligned access (and returns `None`). `size` is 1/2/4/8.
#[inline]
fn addr(cpu: &mut Cpu, _bus: &mut dyn Bus, i: u32, size: u32, store: bool) -> Option<u32> {
    let vaddr = cpu.reg(rs(i)).wrapping_add(imm_se(i));
    let align = (size - 1) as u64;
    if vaddr & align != 0 {
        cpu.raise_address_error(vaddr, store);
        return None;
    }
    Some(virt_to_phys(vaddr as u32))
}

fn special(cpu: &mut Cpu, _bus: &mut dyn Bus, i: u32) {
    match funct(i) {
        0x00 => {
            // SLL (also NOP when i == 0)
            cpu.set_reg32(rd(i), (cpu.reg32(rt(i))) << sa(i));
        }
        0x02 => {
            // SRL
            cpu.set_reg32(rd(i), cpu.reg32(rt(i)) >> sa(i));
        }
        0x03 => {
            // SRA (arithmetic)
            cpu.set_reg32(rd(i), ((cpu.reg32(rt(i)) as i32) >> sa(i)) as u32);
        }
        0x04 => {
            // SLLV
            let s = cpu.reg(rs(i)) & 0x1F;
            cpu.set_reg32(rd(i), cpu.reg32(rt(i)) << s);
        }
        0x06 => {
            // SRLV
            let s = cpu.reg(rs(i)) & 0x1F;
            cpu.set_reg32(rd(i), cpu.reg32(rt(i)) >> s);
        }
        0x07 => {
            // SRAV
            let s = cpu.reg(rs(i)) & 0x1F;
            cpu.set_reg32(rd(i), ((cpu.reg32(rt(i)) as i32) >> s) as u32);
        }
        0x08 => {
            // JR
            cpu.next_pc = cpu.reg(rs(i));
            cpu.branch_taken = true;
        }
        0x09 => {
            // JALR
            let ret = cpu.next_pc;
            cpu.next_pc = cpu.reg(rs(i));
            cpu.set_reg(rd(i), ret);
            cpu.branch_taken = true;
        }
        0x0C => cpu.raise(Exception::Syscall, 0),
        0x0D => cpu.raise(Exception::Breakpoint, 0),
        0x0F => { /* SYNC — no-op */ }
        0x10 => cpu.set_reg(rd(i), cpu.hi), // MFHI
        0x11 => cpu.hi = cpu.reg(rs(i)),    // MTHI
        0x12 => cpu.set_reg(rd(i), cpu.lo), // MFLO
        0x13 => cpu.lo = cpu.reg(rs(i)),    // MTLO
        0x14 => {
            // DSLLV
            let s = cpu.reg(rs(i)) & 0x3F;
            cpu.set_reg(rd(i), cpu.reg(rt(i)) << s);
        }
        0x16 => {
            // DSRLV
            let s = cpu.reg(rs(i)) & 0x3F;
            cpu.set_reg(rd(i), cpu.reg(rt(i)) >> s);
        }
        0x17 => {
            // DSRAV
            let s = cpu.reg(rs(i)) & 0x3F;
            cpu.set_reg(rd(i), ((cpu.reg(rt(i)) as i64) >> s) as u64);
        }
        0x18 => {
            // MULT (signed 32x32 -> 64)
            let p = (cpu.reg32(rs(i)) as i32 as i64) * (cpu.reg32(rt(i)) as i32 as i64);
            cpu.lo = p as i32 as i64 as u64;
            cpu.hi = (p >> 32) as i32 as i64 as u64;
        }
        0x19 => {
            // MULTU (unsigned 32x32 -> 64)
            let p = (cpu.reg32(rs(i)) as u64) * (cpu.reg32(rt(i)) as u64);
            cpu.lo = p as i32 as i64 as u64;
            cpu.hi = (p >> 32) as i32 as i64 as u64;
        }
        0x1A => {
            // DIV (signed)
            let n = cpu.reg32(rs(i)) as i32;
            let d = cpu.reg32(rt(i)) as i32;
            if d == 0 {
                // MIPS leaves architecturally-defined results on /0.
                cpu.lo = if n >= 0 { u64::MAX } else { 1 };
                cpu.hi = n as i64 as u64;
            } else if n == i32::MIN && d == -1 {
                cpu.lo = i32::MIN as i64 as u64;
                cpu.hi = 0;
            } else {
                cpu.lo = (n / d) as i64 as u64;
                cpu.hi = (n % d) as i64 as u64;
            }
        }
        0x1B => {
            // DIVU (unsigned)
            let n = cpu.reg32(rs(i));
            let d = cpu.reg32(rt(i));
            if d == 0 {
                cpu.lo = u64::MAX;
                cpu.hi = n as i32 as i64 as u64;
            } else {
                cpu.lo = (n / d) as i32 as i64 as u64;
                cpu.hi = (n % d) as i32 as i64 as u64;
            }
        }
        0x1C => {
            // DMULT (signed 64x64 -> 128)
            let p = (cpu.reg(rs(i)) as i64 as i128) * (cpu.reg(rt(i)) as i64 as i128);
            cpu.lo = p as u64;
            cpu.hi = (p >> 64) as u64;
        }
        0x1D => {
            // DMULTU
            let p = (cpu.reg(rs(i)) as u128) * (cpu.reg(rt(i)) as u128);
            cpu.lo = p as u64;
            cpu.hi = (p >> 64) as u64;
        }
        0x1E => {
            // DDIV (signed 64-bit)
            let n = cpu.reg(rs(i)) as i64;
            let d = cpu.reg(rt(i)) as i64;
            if d == 0 {
                cpu.lo = if n >= 0 { u64::MAX } else { 1 };
                cpu.hi = n as u64;
            } else if n == i64::MIN && d == -1 {
                cpu.lo = i64::MIN as u64;
                cpu.hi = 0;
            } else {
                cpu.lo = (n / d) as u64;
                cpu.hi = (n % d) as u64;
            }
        }
        0x1F => {
            // DDIVU
            let n = cpu.reg(rs(i));
            let d = cpu.reg(rt(i));
            if d == 0 {
                cpu.lo = u64::MAX;
                cpu.hi = n;
            } else {
                cpu.lo = n / d;
                cpu.hi = n % d;
            }
        }
        0x20 => {
            // ADD (with overflow trap)
            let a = cpu.reg32(rs(i)) as i32;
            let b = cpu.reg32(rt(i)) as i32;
            match a.checked_add(b) {
                Some(r) => cpu.set_reg32(rd(i), r as u32),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x21 => {
            // ADDU
            let r = cpu.reg32(rs(i)).wrapping_add(cpu.reg32(rt(i)));
            cpu.set_reg32(rd(i), r);
        }
        0x22 => {
            // SUB (with overflow trap)
            let a = cpu.reg32(rs(i)) as i32;
            let b = cpu.reg32(rt(i)) as i32;
            match a.checked_sub(b) {
                Some(r) => cpu.set_reg32(rd(i), r as u32),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x23 => {
            // SUBU
            let r = cpu.reg32(rs(i)).wrapping_sub(cpu.reg32(rt(i)));
            cpu.set_reg32(rd(i), r);
        }
        0x24 => cpu.set_reg(rd(i), cpu.reg(rs(i)) & cpu.reg(rt(i))), // AND
        0x25 => cpu.set_reg(rd(i), cpu.reg(rs(i)) | cpu.reg(rt(i))), // OR
        0x26 => cpu.set_reg(rd(i), cpu.reg(rs(i)) ^ cpu.reg(rt(i))), // XOR
        0x27 => cpu.set_reg(rd(i), !(cpu.reg(rs(i)) | cpu.reg(rt(i)))), // NOR
        0x2A => {
            // SLT (signed)
            let r = ((cpu.reg(rs(i)) as i64) < (cpu.reg(rt(i)) as i64)) as u64;
            cpu.set_reg(rd(i), r);
        }
        0x2B => {
            // SLTU
            let r = (cpu.reg(rs(i)) < cpu.reg(rt(i))) as u64;
            cpu.set_reg(rd(i), r);
        }
        0x2C => {
            // DADD (with overflow trap)
            let a = cpu.reg(rs(i)) as i64;
            let b = cpu.reg(rt(i)) as i64;
            match a.checked_add(b) {
                Some(r) => cpu.set_reg(rd(i), r as u64),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x2D => {
            // DADDU
            cpu.set_reg(rd(i), cpu.reg(rs(i)).wrapping_add(cpu.reg(rt(i))));
        }
        0x2E => {
            // DSUB (with overflow trap)
            let a = cpu.reg(rs(i)) as i64;
            let b = cpu.reg(rt(i)) as i64;
            match a.checked_sub(b) {
                Some(r) => cpu.set_reg(rd(i), r as u64),
                None => cpu.raise(Exception::Overflow, 0),
            }
        }
        0x2F => {
            // DSUBU
            cpu.set_reg(rd(i), cpu.reg(rs(i)).wrapping_sub(cpu.reg(rt(i))));
        }
        0x30..=0x36 => trap_op(cpu, i), // TGE/TGEU/TLT/TLTU/TEQ/.../TNE
        0x38 => {
            // DSLL
            cpu.set_reg(rd(i), cpu.reg(rt(i)) << sa(i));
        }
        0x3A => {
            // DSRL
            cpu.set_reg(rd(i), cpu.reg(rt(i)) >> sa(i));
        }
        0x3B => {
            // DSRA
            cpu.set_reg(rd(i), ((cpu.reg(rt(i)) as i64) >> sa(i)) as u64);
        }
        0x3C => {
            // DSLL32
            cpu.set_reg(rd(i), cpu.reg(rt(i)) << (sa(i) + 32));
        }
        0x3E => {
            // DSRL32
            cpu.set_reg(rd(i), cpu.reg(rt(i)) >> (sa(i) + 32));
        }
        0x3F => {
            // DSRA32
            cpu.set_reg(rd(i), ((cpu.reg(rt(i)) as i64) >> (sa(i) + 32)) as u64);
        }
        _ => cpu.raise(Exception::ReservedInstruction, 0),
    }
}

/// SPECIAL trap instructions (TGE/TGEU/TLT/TLTU/TEQ/TNE), funct 0x30..0x36.
fn trap_op(cpu: &mut Cpu, i: u32) {
    let a = cpu.reg(rs(i));
    let b = cpu.reg(rt(i));
    let cond = match funct(i) {
        0x30 => (a as i64) >= (b as i64), // TGE
        0x31 => a >= b,                    // TGEU
        0x32 => (a as i64) < (b as i64),  // TLT
        0x33 => a < b,                     // TLTU
        0x34 => a == b,                    // TEQ
        0x36 => a != b,                    // TNE
        _ => false,
    };
    if cond {
        cpu.raise(Exception::Trap, 0);
    }
}

fn regimm(cpu: &mut Cpu, i: u32) {
    let offset = imm_se(i) << 2;
    match rt(i) {
        0x00 => {
            // BLTZ
            if (cpu.reg(rs(i)) as i64) < 0 {
                branch(cpu, offset);
            }
        }
        0x01 => {
            // BGEZ
            if (cpu.reg(rs(i)) as i64) >= 0 {
                branch(cpu, offset);
            }
        }
        0x02 => {
            // BLTZL
            if (cpu.reg(rs(i)) as i64) < 0 {
                branch(cpu, offset);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x03 => {
            // BGEZL
            if (cpu.reg(rs(i)) as i64) >= 0 {
                branch(cpu, offset);
            } else {
                nullify_delay_slot(cpu);
            }
        }
        0x10 => {
            // BLTZAL
            cpu.set_reg(31, cpu.next_pc);
            if (cpu.reg(rs(i)) as i64) < 0 {
                branch(cpu, offset);
            }
        }
        0x11 => {
            // BGEZAL
            cpu.set_reg(31, cpu.next_pc);
            if (cpu.reg(rs(i)) as i64) >= 0 {
                branch(cpu, offset);
            }
        }
        _ => cpu.raise(Exception::ReservedInstruction, 0),
    }
}

fn cop0_op(cpu: &mut Cpu, i: u32) {
    match rs(i) {
        0x00 => {
            // MFC0 (32-bit, sign-extended)
            let v = cpu.cop0.read(rd(i) as usize) as u32;
            cpu.set_reg32(rt(i), v);
        }
        0x01 => {
            // DMFC0 (64-bit)
            cpu.set_reg(rt(i), cpu.cop0.read(rd(i) as usize));
        }
        0x04 => {
            // MTC0
            cpu.cop0.write(rd(i) as usize, cpu.reg32(rt(i)) as u64);
        }
        0x05 => {
            // DMTC0
            cpu.cop0.write(rd(i) as usize, cpu.reg(rt(i)));
        }
        0x10..=0x1F => {
            // CP0 function (TLB/ERET) — dispatch on funct.
            match funct(i) {
                0x01 => { /* TLBR — read indexed entry; foundation no-op-ish */ }
                0x02 | 0x06 => {
                    // TLBWI / TLBWR — write the EntryHi/EntryLo* into the
                    // indexed (or Random) TLB slot.
                    let idx = if funct(i) == 0x02 {
                        (cpu.cop0.reg[cop0::R_INDEX] & 0x3F) as usize
                    } else {
                        (cpu.cop0.reg[cop0::R_RANDOM] & 0x3F) as usize
                    } & 31;
                    cpu.cop0.tlb[idx] = super::cop0::TlbEntry {
                        entry_hi: cpu.cop0.reg[cop0::R_ENTRYHI],
                        entry_lo0: cpu.cop0.reg[cop0::R_ENTRYLO0],
                        entry_lo1: cpu.cop0.reg[cop0::R_ENTRYLO1],
                        page_mask: cpu.cop0.reg[cop0::R_PAGEMASK] as u32,
                    };
                }
                0x08 => { /* TLBP — probe; foundation leaves Index unchanged */ }
                0x18 => {
                    // ERET
                    let resume = cpu.cop0.eret();
                    cpu.pc = resume;
                    cpu.next_pc = resume.wrapping_add(4);
                    cpu.ll_bit = false;
                    cpu.branch_taken = false;
                    cpu.exception_pending = true; // skip normal PC advance
                }
                _ => {}
            }
        }
        _ => cpu.raise(Exception::ReservedInstruction, 0),
    }
}

/// COP1 (FPU) — load/store are handled in the primary opcode table; this
/// dispatches the COP1 register moves, branches, and arithmetic (fmt in rs).
fn cop1_op(cpu: &mut Cpu, _bus: &mut dyn Bus, i: u32) {
    match rs(i) {
        0x00 => {
            // MFC1 (move word from FPU, sign-extended)
            let v = cpu.cop1.read_w(rd(i) as usize);
            cpu.set_reg32(rt(i), v);
        }
        0x01 => {
            // DMFC1
            cpu.set_reg(rt(i), cpu.cop1.read_dw(rd(i) as usize));
        }
        0x02 => {
            // CFC1
            let v = cpu.cop1.read_ctrl(rd(i));
            cpu.set_reg32(rt(i), v);
        }
        0x04 => {
            // MTC1
            cpu.cop1.write_w(rd(i) as usize, cpu.reg32(rt(i)));
        }
        0x05 => {
            // DMTC1
            cpu.cop1.write_dw(rd(i) as usize, cpu.reg(rt(i)));
        }
        0x06 => {
            // CTC1
            cpu.cop1.write_ctrl(rd(i), cpu.reg32(rt(i)));
        }
        0x08 => {
            // BC1 — branch on FP condition (rt selects T/F and likely)
            let cond = cpu.cop1.condition();
            let take = match rt(i) {
                0x00 => !cond, // BC1F
                0x01 => cond,  // BC1T
                0x02 => !cond, // BC1FL
                0x03 => cond,  // BC1TL
                _ => false,
            };
            let likely = rt(i) & 0x02 != 0;
            if take {
                branch(cpu, imm_se(i) << 2);
            } else if likely {
                nullify_delay_slot(cpu);
            }
        }
        0x10 => fpu_arith(cpu, i, false), // single
        0x11 => fpu_arith(cpu, i, true),  // double
        0x14 | 0x15 => {
            // CVT.S/D.W and CVT.S/D.L: integer source -> float dest.
            let src = if rs(i) == 0x14 {
                cpu.cop1.read_w(rt(i) as usize) as i32 as f64
            } else {
                cpu.cop1.read_dw(rt(i) as usize) as i64 as f64
            };
            // funct picks the destination format.
            match funct(i) {
                0x20 => cpu.cop1.write_s(rd(i) as usize, src as f32),
                0x21 => cpu.cop1.write_d(rd(i) as usize, src),
                _ => {}
            }
        }
        _ => cpu.raise(Exception::ReservedInstruction, 0),
    }
}

/// COP1 arithmetic (single = !double). Implements the common ops; precise
/// rounding/exception flags are simplified (round-to-nearest via host float).
fn fpu_arith(cpu: &mut Cpu, i: u32, double: bool) {
    let f = &mut cpu.cop1;
    let (s, t, d) = (rt(i) as usize, rd(i) as usize, sa(i) as usize);
    // For the standard 3-operand FP format: fs = rd, ft = rt, fd = sa.
    macro_rules! bin {
        ($op:tt) => {{
            if double {
                let r = f.read_d(t) $op f.read_d(s);
                f.write_d(d, r);
            } else {
                let r = f.read_s(t) $op f.read_s(s);
                f.write_s(d, r);
            }
        }};
    }
    match funct(i) {
        0x00 => bin!(+), // ADD
        0x01 => bin!(-), // SUB
        0x02 => bin!(*), // MUL
        0x03 => bin!(/), // DIV
        0x04 => {
            // SQRT
            if double {
                let r = f.read_d(t).sqrt();
                f.write_d(d, r);
            } else {
                let r = f.read_s(t).sqrt();
                f.write_s(d, r);
            }
        }
        0x05 => {
            // ABS
            if double {
                let r = f.read_d(t).abs();
                f.write_d(d, r);
            } else {
                let r = f.read_s(t).abs();
                f.write_s(d, r);
            }
        }
        0x06 => {
            // MOV
            if double {
                let r = f.read_d(t);
                f.write_d(d, r);
            } else {
                let r = f.read_s(t);
                f.write_s(d, r);
            }
        }
        0x07 => {
            // NEG
            if double {
                let r = -f.read_d(t);
                f.write_d(d, r);
            } else {
                let r = -f.read_s(t);
                f.write_s(d, r);
            }
        }
        0x20 => {
            // CVT.S (from double, when fmt=double)
            if double {
                let r = f.read_d(t) as f32;
                f.write_s(d, r);
            }
        }
        0x21 => {
            // CVT.D (from single, when fmt=single)
            if !double {
                let r = f.read_s(t) as f64;
                f.write_d(d, r);
            }
        }
        0x30..=0x3F => {
            // C.cond.fmt — compare, set the FCR31 condition bit. We implement
            // the ordered comparisons used in practice (EQ / LT / LE) and the
            // "unordered" variants treat NaN as setting the condition false.
            let (a, b) = if double {
                (f.read_d(s), f.read_d(t))
            } else {
                (f.read_s(s) as f64, f.read_s(t) as f64)
            };
            let cond = match funct(i) & 0x0F {
                0x2 | 0xA => a == b,        // C.EQ / C.UEQ (NaN handling simplified)
                0x4 | 0xC => a < b,         // C.OLT / C.LT
                0x6 | 0xE => a <= b,        // C.OLE / C.LE
                _ => false,
            };
            f.set_condition(cond);
        }
        _ => {}
    }
}

// ---- unaligned load/store helpers (LWL/LWR/SWL/SWR/LDL/LDR/SDL/SDR) ----
// These access the *aligned* word/doubleword containing the address and merge
// the partial result. Built straight from the MIPS III definitions.

// The left/right helpers are expressed with explicit big-endian byte indices
// (the N64 is big-endian) rather than clever shift arithmetic, so they are
// obviously correct. LWL loads the bytes from the addressed byte through the
// most-significant end of the aligned word into the high bytes of the
// register; LWR loads from the least-significant end up to the addressed byte
// into the low bytes. Stores are the symmetric memory-side operation.

fn load_left_right_w(cpu: &mut Cpu, bus: &mut dyn Bus, i: u32, left: bool) {
    let vaddr = cpu.reg(rs(i)).wrapping_add(imm_se(i)) as u32;
    let aligned = virt_to_phys(vaddr & !3);
    let word = bus.read32(aligned).to_be_bytes();
    let off = (vaddr & 3) as usize; // 0..=3, big-endian byte index
    let mut reg = cpu.reg32(rt(i)).to_be_bytes();
    if left {
        // Bytes off..4 of memory fill bytes 0..(4-off) of the register.
        for k in 0..(4 - off) {
            reg[k] = word[off + k];
        }
    } else {
        // Bytes 0..=off of memory fill bytes (3-off)..4 of the register.
        for k in 0..=off {
            reg[3 - off + k] = word[k];
        }
    }
    cpu.set_reg32(rt(i), u32::from_be_bytes(reg));
}

fn store_left_right_w(cpu: &mut Cpu, bus: &mut dyn Bus, i: u32, left: bool) {
    let vaddr = cpu.reg(rs(i)).wrapping_add(imm_se(i)) as u32;
    let aligned = virt_to_phys(vaddr & !3);
    let mut word = bus.read32(aligned).to_be_bytes();
    let off = (vaddr & 3) as usize;
    let reg = cpu.reg32(rt(i)).to_be_bytes();
    if left {
        for k in 0..(4 - off) {
            word[off + k] = reg[k];
        }
    } else {
        for k in 0..=off {
            word[k] = reg[3 - off + k];
        }
    }
    bus.write32(aligned, u32::from_be_bytes(word));
}

fn load_left_right_d(cpu: &mut Cpu, bus: &mut dyn Bus, i: u32, left: bool) {
    let vaddr = cpu.reg(rs(i)).wrapping_add(imm_se(i)) as u32;
    let aligned = virt_to_phys(vaddr & !7);
    let dword = bus.read64(aligned).to_be_bytes();
    let off = (vaddr & 7) as usize;
    let mut reg = cpu.reg(rt(i)).to_be_bytes();
    if left {
        for k in 0..(8 - off) {
            reg[k] = dword[off + k];
        }
    } else {
        for k in 0..=off {
            reg[7 - off + k] = dword[k];
        }
    }
    cpu.set_reg(rt(i), u64::from_be_bytes(reg));
}

fn store_left_right_d(cpu: &mut Cpu, bus: &mut dyn Bus, i: u32, left: bool) {
    let vaddr = cpu.reg(rs(i)).wrapping_add(imm_se(i)) as u32;
    let aligned = virt_to_phys(vaddr & !7);
    let mut dword = bus.read64(aligned).to_be_bytes();
    let off = (vaddr & 7) as usize;
    let reg = cpu.reg(rt(i)).to_be_bytes();
    if left {
        for k in 0..(8 - off) {
            dword[off + k] = reg[k];
        }
    } else {
        for k in 0..=off {
            dword[k] = reg[7 - off + k];
        }
    }
    bus.write64(aligned, u64::from_be_bytes(dword));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TestBus;
    use crate::cpu::cop0::{ST_BEV, ST_ERL};

    /// Build a CPU at a clean PC in cached RAM (BEV/ERL cleared so exceptions
    /// take the RAM vector — not that these tests reach it) and a flat bus.
    fn setup(prog: &[u32]) -> (Cpu, TestBus) {
        let mut cpu = Cpu::new();
        cpu.pc = 0;
        cpu.next_pc = 4;
        cpu.cop0.set_status(cpu.cop0.status() & !(ST_BEV | ST_ERL));
        let mut bus = TestBus::new(0x1000);
        for (n, w) in prog.iter().enumerate() {
            bus.write32((n * 4) as u32, *w);
        }
        (cpu, bus)
    }

    /// Encode helpers.
    fn r_type(funct: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | funct
    }
    fn i_type(op: u32, rs: u32, rt: u32, imm: u16) -> u32 {
        (op << 26) | (rs << 21) | (rt << 16) | imm as u32
    }

    #[test]
    fn addiu_and_addu() {
        // ADDIU r1, r0, 5 ; ADDIU r2, r0, 7 ; ADDU r3, r1, r2
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 5),
            i_type(0x09, 0, 2, 7),
            r_type(0x21, 1, 2, 3, 0),
        ]);
        for _ in 0..3 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(3), 12);
    }

    #[test]
    fn lui_ori_builds_word() {
        // LUI r1, 0x1234 ; ORI r1, r1, 0x5678
        let (mut cpu, mut bus) = setup(&[i_type(0x0F, 0, 1, 0x1234), i_type(0x0D, 1, 1, 0x5678)]);
        step(&mut cpu, &mut bus);
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.reg(1), 0x1234_5678);
    }

    #[test]
    fn add_overflow_traps() {
        // r1 = 0x7FFFFFFF, ADD r2,r1,r1 -> overflow
        let (mut cpu, mut bus) = setup(&[
            i_type(0x0F, 0, 1, 0x7FFF), // LUI r1, 0x7FFF
            i_type(0x0D, 1, 1, 0xFFFF), // ORI r1, r1, 0xFFFF
            r_type(0x20, 1, 1, 2, 0),   // ADD r2, r1, r1
        ]);
        step(&mut cpu, &mut bus);
        step(&mut cpu, &mut bus);
        let before = cpu.cop0.exceptions;
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.cop0.exceptions, before + 1);
    }

    #[test]
    fn beq_delay_slot_executes() {
        // BEQ r0,r0,+2 ; ADDIU r1,r0,1 (delay slot, runs) ; ... (skipped)
        // MIPS branch target = (addr of delay slot) + (offset<<2) = 4 + 8 = 12,
        // i.e. instruction index 3. The delay slot (index 1) still executes; the
        // instruction at index 2 is skipped.
        let (mut cpu, mut bus) = setup(&[
            i_type(0x04, 0, 0, 2),      // BEQ r0,r0,+2
            i_type(0x09, 0, 1, 1),      // delay slot: r1 = 1
            i_type(0x09, 0, 2, 2),      // skipped: r2 = 2
            i_type(0x09, 0, 3, 3),      // branch target: r3 = 3
            i_type(0x09, 0, 4, 4),      // (not reached this step)
        ]);
        step(&mut cpu, &mut bus); // BEQ
        step(&mut cpu, &mut bus); // delay slot
        step(&mut cpu, &mut bus); // target
        assert_eq!(cpu.reg(1), 1); // delay slot ran
        assert_eq!(cpu.reg(2), 0); // skipped
        assert_eq!(cpu.reg(3), 3); // landed on target
    }

    #[test]
    fn beql_not_taken_nullifies_delay_slot() {
        // BEQL r0,r1 (r1!=0 so NOT equal? r0==0,r1==0 -> equal). Use BNEL with
        // equal regs so it's not taken and nullifies.
        let (mut cpu, mut bus) = setup(&[
            i_type(0x15, 0, 0, 5),  // BNEL r0,r0,+5 -> not taken
            i_type(0x09, 0, 1, 1),  // delay slot: nullified, r1 stays 0
            i_type(0x09, 0, 2, 2),  // next real instr: r2 = 2
        ]);
        step(&mut cpu, &mut bus); // BNEL (not taken)
        step(&mut cpu, &mut bus); // should be the instr AFTER the nullified slot
        assert_eq!(cpu.reg(1), 0); // delay slot was nullified
        assert_eq!(cpu.reg(2), 2);
    }

    #[test]
    fn jal_sets_ra_and_jumps() {
        // JAL to word index 4 (addr 0x10). target26 = addr>>2.
        let (mut cpu, mut bus) = setup(&[
            (0x03 << 26) | (0x10 >> 2), // JAL 0x10
            i_type(0x09, 0, 1, 1),      // delay slot
            0,
            0,
            i_type(0x09, 0, 2, 2),      // target
        ]);
        step(&mut cpu, &mut bus); // JAL
        step(&mut cpu, &mut bus); // delay slot
        step(&mut cpu, &mut bus); // target
        assert_eq!(cpu.reg(31), 8); // return addr = JAL+8
        assert_eq!(cpu.reg(1), 1);
        assert_eq!(cpu.reg(2), 2);
    }

    #[test]
    fn mult_sets_hi_lo() {
        // r1 = 0x10000, r2 = 0x10000, MULT -> 0x1_0000_0000 -> hi=1, lo=0
        let (mut cpu, mut bus) = setup(&[
            i_type(0x0F, 0, 1, 1),    // LUI r1, 1 -> 0x10000
            i_type(0x0F, 0, 2, 1),    // LUI r2, 1 -> 0x10000
            r_type(0x18, 1, 2, 0, 0), // MULT r1, r2
            r_type(0x10, 0, 0, 3, 0), // MFHI r3
            r_type(0x12, 0, 0, 4, 0), // MFLO r4
        ]);
        for _ in 0..5 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(3), 1); // hi
        assert_eq!(cpu.reg(4), 0); // lo
    }

    #[test]
    fn div_quotient_and_remainder() {
        // 17 / 5 = 3 r 2
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 17),
            i_type(0x09, 0, 2, 5),
            r_type(0x1A, 1, 2, 0, 0), // DIV
            r_type(0x12, 0, 0, 3, 0), // MFLO -> quotient
            r_type(0x10, 0, 0, 4, 0), // MFHI -> remainder
        ]);
        for _ in 0..5 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(3), 3);
        assert_eq!(cpu.reg(4), 2);
    }

    #[test]
    fn load_store_word_big_endian() {
        // SW then LW the value back. r1 = 0xCAFEBABE? build with LUI+ORI.
        let (mut cpu, mut bus) = setup(&[
            i_type(0x0F, 0, 1, 0xCAFE), // LUI
            i_type(0x0D, 1, 1, 0xBABE), // ORI -> r1 = 0xCAFEBABE
            i_type(0x09, 0, 5, 0x100),  // r5 = 0x100 (address)
            i_type(0x2B, 5, 1, 0),      // SW r1, 0(r5)
            i_type(0x23, 5, 2, 0),      // LW r2, 0(r5)
        ]);
        for _ in 0..5 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(2) as u32, 0xCAFE_BABE);
        // Verify big-endian byte order in the backing store.
        assert_eq!(bus.ram[0x100], 0xCA);
        assert_eq!(bus.ram[0x103], 0xBE);
    }

    #[test]
    fn sd_ld_doubleword() {
        // Build a 64-bit value, store, reload.
        let (mut cpu, mut bus) = setup(&[
            i_type(0x0F, 0, 1, 0x1122),  // LUI
            i_type(0x0D, 1, 1, 0x3344),  // ORI -> r1 = 0x11223344 (sign-extended)
            i_type(0x09, 0, 5, 0x200),   // address
            i_type(0x3F, 5, 1, 0),       // SD
            i_type(0x37, 5, 2, 0),       // LD
        ]);
        for _ in 0..5 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(2), 0x0000_0000_1122_3344);
    }

    #[test]
    fn slt_signed_vs_unsigned() {
        // r1 = -1 (0xFFFFFFFF), r2 = 1. SLT r3 = (-1 < 1) = 1 ; SLTU r4 = (big < 1) = 0
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 0xFFFF), // ADDIU r1, r0, -1
            i_type(0x09, 0, 2, 1),      // r2 = 1
            r_type(0x2A, 1, 2, 3, 0),   // SLT r3, r1, r2
            r_type(0x2B, 1, 2, 4, 0),   // SLTU r4, r1, r2
        ]);
        for _ in 0..4 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(3), 1); // signed: -1 < 1
        assert_eq!(cpu.reg(4), 0); // unsigned: huge >= 1
    }

    #[test]
    fn dsll32_shifts_into_high_half() {
        // r1 = 1 ; DSLL32 r2, r1, 0 -> r2 = 1 << 32
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 1),
            r_type(0x3C, 0, 1, 2, 0), // DSLL32 r2, r1, 0
        ]);
        step(&mut cpu, &mut bus);
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.reg(2), 1u64 << 32);
    }

    #[test]
    fn jr_returns_to_address() {
        // r1 = 0x10 ; JR r1 ; (delay) ; ... ; target at 0x10
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 0x10),   // r1 = 0x10
            r_type(0x08, 1, 0, 0, 0),   // JR r1
            i_type(0x09, 0, 2, 2),      // delay slot
            0,
            i_type(0x09, 0, 3, 3),      // target (0x10)
        ]);
        step(&mut cpu, &mut bus); // set r1
        step(&mut cpu, &mut bus); // JR
        step(&mut cpu, &mut bus); // delay slot
        step(&mut cpu, &mut bus); // target
        assert_eq!(cpu.reg(2), 2);
        assert_eq!(cpu.reg(3), 3);
    }

    #[test]
    fn syscall_raises_exception_and_vectors() {
        let (mut cpu, mut bus) = setup(&[r_type(0x0C, 0, 0, 0, 0)]); // SYSCALL
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.cop0.exceptions, 1);
        assert_eq!(
            (cpu.cop0.cause() & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::Syscall.code()
        );
    }

    #[test]
    fn reserved_instruction_traps() {
        // Opcode 0x1F is unassigned -> reserved instruction.
        let (mut cpu, mut bus) = setup(&[0x1F << 26]);
        step(&mut cpu, &mut bus);
        assert_eq!(
            (cpu.cop0.cause() & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::ReservedInstruction.code()
        );
    }

    #[test]
    fn mtc0_mfc0_roundtrip() {
        // MTC0 sets Compare; MFC0 reads it back.
        let (mut cpu, mut bus) = setup(&[
            i_type(0x09, 0, 1, 0x55),                 // r1 = 0x55
            (0x10 << 26) | (0x04 << 21) | (1 << 16) | (cop0::R_COMPARE as u32) << 11, // MTC0 r1, Compare
            (0x10 << 26) | (0x00 << 21) | (2 << 16) | (cop0::R_COMPARE as u32) << 11, // MFC0 r2, Compare
        ]);
        for _ in 0..3 {
            step(&mut cpu, &mut bus);
        }
        assert_eq!(cpu.reg(2), 0x55);
    }
}
