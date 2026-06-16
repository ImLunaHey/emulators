//! ARM (32-bit) instruction interpreter. Ported 1:1 from src/cpu/arm.ts.

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::shifter::{apply_carry, imm_shift, reg_shift, ror_imm32};
use crate::state::{mode, CpuState, FLAG_T};

// Add/sub flag helpers — return result and set N/Z/C/V on CPSR.
fn add_set_flags(s: &mut CpuState, a: u32, b: u32) -> u32 {
    let r = a.wrapping_add(b);
    s.set_nz(r);
    s.set_c(r < a);
    s.set_v((!(a ^ b) & (a ^ r) & 0x80000000) != 0);
    r
}
fn adc_set_flags(s: &mut CpuState, a: u32, b: u32, c_in: u32) -> u32 {
    let sum = a as u64 + b as u64 + c_in as u64;
    let r = sum as u32;
    s.set_nz(r);
    s.set_c(sum > 0xFFFFFFFF);
    s.set_v((!(a ^ b) & (a ^ r) & 0x80000000) != 0);
    r
}
fn sub_set_flags(s: &mut CpuState, a: u32, b: u32) -> u32 {
    let r = a.wrapping_sub(b);
    s.set_nz(r);
    s.set_c(a >= b);
    s.set_v(((a ^ b) & (a ^ r) & 0x80000000) != 0);
    r
}
fn sbc_set_flags(s: &mut CpuState, a: u32, b: u32, c_in: u32) -> u32 {
    // ARM SBC: Rd = Rn - Rm - NOT(C)
    let not_c = c_in ^ 1;
    let r = a.wrapping_sub(b).wrapping_sub(not_c);
    s.set_nz(r);
    s.set_c((a as u64) >= (b as u64 + not_c as u64));
    s.set_v(((a ^ b) & (a ^ r) & 0x80000000) != 0);
    r
}

pub fn arm_execute<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let cond = (instr >> 28) & 0xF;
    if cond != 0xE && !cpu.state.check_cond(cond) {
        return;
    }

    // Branch and Branch with Link: 101x cccc
    if (instr & 0x0E000000) == 0x0A000000 {
        let mut offset = (instr & 0x00FFFFFF) << 2;
        if offset & 0x02000000 != 0 {
            offset |= 0xFC000000;
        }
        if instr & 0x01000000 != 0 {
            cpu.state.r[14] = cpu.state.r[15].wrapping_sub(4); // BL: LR = pc+4 of next
        }
        cpu.state.r[15] = cpu.state.r[15].wrapping_add(offset);
        cpu.flush_pipeline();
        return;
    }

    // BX: branch and exchange — 0001 0010 1111 1111 1111 0001 Rn
    if (instr & 0x0FFFFFF0) == 0x012FFF10 {
        let rn = (instr & 0xF) as usize;
        let tgt = cpu.state.r[rn];
        if tgt & 1 != 0 {
            cpu.state.cpsr |= FLAG_T;
            cpu.state.r[15] = tgt & !1;
        } else {
            cpu.state.cpsr &= !FLAG_T;
            cpu.state.r[15] = tgt & !3;
        }
        cpu.flush_pipeline();
        return;
    }

    // SWI
    if (instr & 0x0F000000) == 0x0F000000 {
        cpu.software_interrupt((instr & 0x00FFFFFF) >> 16, bus);
        return;
    }

    // Block data transfer LDM/STM: 100x
    if (instr & 0x0E000000) == 0x08000000 {
        arm_block_transfer(cpu, bus, instr);
        return;
    }

    // Architecturally-UNDEFINED slot: 011x xxxx ... xxx1 (bits 27-25 = 011 and
    // bit 4 set). The real ARM7TDMI takes the undefined-instruction exception
    // here rather than treating it as an LDR/STR; we vector to 0x04 and bump the
    // exception counter so a fault loop is detectable.
    if (instr & 0x0E000010) == 0x06000010 {
        cpu.undefined_instruction("UNDEF INSTR");
        return;
    }

    // Single data transfer LDR/STR: 01xx
    if (instr & 0x0C000000) == 0x04000000 {
        arm_single_transfer(cpu, bus, instr);
        return;
    }

    // Half-word, signed transfer, multiply, swap — these share the 000x major
    // bits but with bit 4 + bit 7 set (the "extension space").
    if (instr & 0x0E000090) == 0x00000090 {
        // Multiply / multiply long / swap / halfword
        let is_hw = (instr & 0x60) != 0; // any bits in 5/6 → halfword/signed
        if is_hw {
            arm_half_transfer(cpu, bus, instr);
            return;
        }
        // bit 24 distinguishes multiply (0) vs swap (1).
        if (instr & 0x01000000) == 0 {
            arm_multiply(cpu, instr);
            return;
        }
        arm_swap(cpu, bus, instr);
        return;
    }

    // PSR transfer: MRS / MSR. Pattern 0001 0?00 ... (with bit 25 cleared and
    // the specific encoding). Both immediate-form MSR and register-form MRS/MSR
    // fall through here.
    if (instr & 0x0F900000) == 0x01000000 && (instr & 0x90) != 0x90 {
        // bit 21 distinguishes MSR (1) from MRS (0).
        if instr & 0x00200000 != 0 {
            arm_msr(cpu, instr);
            return;
        }
        arm_mrs(cpu, instr);
        return;
    }
    if (instr & 0x0FB00000) == 0x03200000 {
        // MSR immediate
        arm_msr_imm(cpu, instr);
        return;
    }

    // Data processing (immediate or register operand).
    arm_data_processing(cpu, instr);
}

// ---------------------------------------------------------------- data processing
fn arm_data_processing(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let opcode = (instr >> 21) & 0xF;
    let set_flags = (instr & 0x00100000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let mut op1 = s.r[rn];
    let op2: u32;
    let mut shifter_carry = s.c();

    if instr & 0x02000000 != 0 {
        // Immediate operand: rotated 8-bit value.
        let imm = instr & 0xFF;
        let rot = ((instr >> 8) & 0xF) << 1;
        op2 = ror_imm32(imm, rot);
        if rot != 0 && set_flags {
            shifter_carry = (op2 >> 31) & 1;
        }
    } else {
        let rm = (instr & 0xF) as usize;
        let shift_type = (instr >> 5) & 3;
        let mut rm_val = s.r[rm];
        if instr & 0x10 != 0 {
            // Register-specified shift amount — costs an extra cycle and R15 sees +12.
            let rs = ((instr >> 8) & 0xF) as usize;
            let amount = s.r[rs] & 0xFF;
            if rn == 15 {
                op1 = op1.wrapping_add(4);
            }
            if rm == 15 {
                rm_val = rm_val.wrapping_add(4);
            }
            let r = reg_shift(shift_type, amount, rm_val, shifter_carry);
            op2 = r.value;
            shifter_carry = r.carry;
        } else {
            let imm = (instr >> 7) & 0x1F;
            let r = imm_shift(shift_type, imm, rm_val, shifter_carry);
            op2 = r.value;
            shifter_carry = r.carry;
        }
    }

    let mut result: u32 = 0;
    let mut write_result = true;
    let c_in = s.c();
    match opcode {
        0x0 => {
            result = op1 & op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // AND
        0x1 => {
            result = op1 ^ op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // EOR
        0x2 => {
            result = if set_flags {
                sub_set_flags(s, op1, op2)
            } else {
                op1.wrapping_sub(op2)
            };
        } // SUB
        0x3 => {
            result = if set_flags {
                sub_set_flags(s, op2, op1)
            } else {
                op2.wrapping_sub(op1)
            };
        } // RSB
        0x4 => {
            result = if set_flags {
                add_set_flags(s, op1, op2)
            } else {
                op1.wrapping_add(op2)
            };
        } // ADD
        0x5 => {
            result = if set_flags {
                adc_set_flags(s, op1, op2, c_in)
            } else {
                op1.wrapping_add(op2).wrapping_add(c_in)
            };
        } // ADC
        0x6 => {
            result = if set_flags {
                sbc_set_flags(s, op1, op2, c_in)
            } else {
                op1.wrapping_sub(op2).wrapping_sub(c_in ^ 1)
            };
        } // SBC
        0x7 => {
            result = if set_flags {
                sbc_set_flags(s, op2, op1, c_in)
            } else {
                op2.wrapping_sub(op1).wrapping_sub(c_in ^ 1)
            };
        } // RSC
        0x8 => {
            write_result = false;
            result = op1 & op2;
            s.set_nz(result);
            apply_carry(s, shifter_carry);
        } // TST
        0x9 => {
            write_result = false;
            result = op1 ^ op2;
            s.set_nz(result);
            apply_carry(s, shifter_carry);
        } // TEQ
        0xA => {
            write_result = false;
            sub_set_flags(s, op1, op2);
        } // CMP
        0xB => {
            write_result = false;
            add_set_flags(s, op1, op2);
        } // CMN
        0xC => {
            result = op1 | op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // ORR
        0xD => {
            result = op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // MOV
        0xE => {
            result = op1 & !op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // BIC
        0xF => {
            result = !op2;
            if set_flags {
                s.set_nz(result);
                apply_carry(s, shifter_carry);
            }
        } // MVN
        _ => {}
    }

    if write_result {
        if rd == 15 {
            // Writing PC with S bit copies SPSR into CPSR (mode change).
            if set_flags {
                let spsr = s.get_spsr();
                s.switch_mode(spsr & 0x1F);
                s.cpsr = spsr;
            }
            let thumb = (s.cpsr & FLAG_T) != 0;
            s.r[15] = if thumb { result & !1 } else { result & !3 };
            cpu.flush_pipeline();
        } else {
            s.r[rd] = result;
        }
    }
}

// ---------------------------------------------------------------- MRS / MSR
fn arm_mrs(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let rd = ((instr >> 12) & 0xF) as usize;
    s.r[rd] = if instr & 0x00400000 != 0 {
        s.get_spsr()
    } else {
        s.cpsr
    };
}
fn arm_msr(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_spsr = (instr & 0x00400000) != 0;
    let val = s.r[(instr & 0xF) as usize];
    apply_msr(s, is_spsr, instr, val);
}
fn arm_msr_imm(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_spsr = (instr & 0x00400000) != 0;
    let imm = instr & 0xFF;
    let rot = ((instr >> 8) & 0xF) << 1;
    let val = ror_imm32(imm, rot);
    apply_msr(s, is_spsr, instr, val);
}
fn apply_msr(s: &mut CpuState, is_spsr: bool, instr: u32, val: u32) {
    let mut mask: u32 = 0;
    if instr & 0x00010000 != 0 {
        mask |= 0x000000FF; // control field — only in privileged modes
    }
    if instr & 0x00020000 != 0 {
        mask |= 0x0000FF00;
    }
    if instr & 0x00040000 != 0 {
        mask |= 0x00FF0000;
    }
    if instr & 0x00080000 != 0 {
        mask |= 0xFF000000;
    }
    if is_spsr {
        s.set_spsr((s.get_spsr() & !mask) | (val & mask));
        return;
    }
    // Don't allow mode change from USR.
    if s.mode() == mode::USR {
        mask &= 0xFF000000;
    }
    let new_cpsr = (s.cpsr & !mask) | (val & mask);
    let new_mode = new_cpsr & 0x1F;
    if new_mode != s.mode() {
        s.switch_mode(new_mode);
    }
    s.cpsr = new_cpsr;
}

// ---------------------------------------------------------------- single transfer LDR/STR
fn arm_single_transfer<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let i_bit = (instr & 0x02000000) != 0;
    let p = (instr & 0x01000000) != 0;
    let u = (instr & 0x00800000) != 0;
    let b = (instr & 0x00400000) != 0;
    let w = (instr & 0x00200000) != 0;
    let l = (instr & 0x00100000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;

    let base = cpu.state.r[rn];
    let offset: u32;
    if i_bit {
        let rm = (instr & 0xF) as usize;
        let shift_type = (instr >> 5) & 3;
        let imm = (instr >> 7) & 0x1F;
        offset = imm_shift(shift_type, imm, cpu.state.r[rm], cpu.state.c()).value;
    } else {
        offset = instr & 0xFFF;
    }
    let eff = if u {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if p { eff } else { base };
    let writeback = !p || w;

    if l {
        let value: u32;
        if b {
            value = bus.read8(addr);
        } else {
            // LDR with unaligned address: read aligned word then rotate.
            let aligned = bus.read32(addr & !3);
            let rot = (addr & 3) << 3;
            value = if rot != 0 {
                (aligned >> rot) | (aligned << (32 - rot))
            } else {
                aligned
            };
        }
        if writeback && (!l || rn != rd) {
            cpu.state.r[rn] = eff;
        }
        if rd == 15 {
            cpu.state.r[15] = value & !3;
            cpu.flush_pipeline();
        } else {
            cpu.state.r[rd] = value;
        }
    } else {
        let mut val = cpu.state.r[rd];
        if rd == 15 {
            val = val.wrapping_add(4); // STR Rd=PC stores pc+12 of original instr
        }
        if b {
            bus.write8(addr, val & 0xFF);
        } else {
            bus.write32(addr & !3, val);
        }
        if writeback {
            cpu.state.r[rn] = eff;
        }
    }
}

// ---------------------------------------------------------------- halfword / signed transfer
fn arm_half_transfer<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let p = (instr & 0x01000000) != 0;
    let u = (instr & 0x00800000) != 0;
    let i_bit = (instr & 0x00400000) != 0; // immediate offset variant
    let w = (instr & 0x00200000) != 0;
    let l = (instr & 0x00100000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let sh = (instr >> 5) & 3; // 01 = H, 10 = SB, 11 = SH

    let base = cpu.state.r[rn];
    let offset: u32;
    if i_bit {
        offset = ((instr >> 4) & 0xF0) | (instr & 0xF);
    } else {
        offset = cpu.state.r[(instr & 0xF) as usize];
    }

    let eff = if u {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if p { eff } else { base };
    let writeback = !p || w;

    if l {
        let mut value: u32 = 0;
        match sh {
            1 => {
                // LDRH — unaligned reads rotate
                let aligned = bus.read16(addr & !1);
                value = if addr & 1 != 0 {
                    (aligned >> 8) | (aligned << 24)
                } else {
                    aligned
                };
            }
            2 => {
                // LDRSB
                let b = bus.read8(addr);
                value = if b & 0x80 != 0 { b | 0xFFFFFF00 } else { b };
            }
            3 => {
                // LDRSH — unaligned drops low byte → LDRSB
                if addr & 1 != 0 {
                    let b = bus.read8(addr);
                    value = if b & 0x80 != 0 { b | 0xFFFFFF00 } else { b };
                } else {
                    let h = bus.read16(addr & !1);
                    value = if h & 0x8000 != 0 { h | 0xFFFF0000 } else { h };
                }
            }
            _ => {}
        }
        if writeback && rn != rd {
            cpu.state.r[rn] = eff;
        }
        if rd == 15 {
            cpu.state.r[15] = value & !3;
            cpu.flush_pipeline();
        } else {
            cpu.state.r[rd] = value;
        }
    } else {
        if sh == 1 {
            bus.write16(addr & !1, cpu.state.r[rd] & 0xFFFF);
        }
        if writeback {
            cpu.state.r[rn] = eff;
        }
    }
}

// ---------------------------------------------------------------- multiply
fn arm_multiply(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_long = (instr & 0x00800000) != 0;
    let set_flags = (instr & 0x00100000) != 0;
    let accumulate = (instr & 0x00200000) != 0;
    let rd = ((instr >> 16) & 0xF) as usize;
    let rn = ((instr >> 12) & 0xF) as usize;
    let rs = ((instr >> 8) & 0xF) as usize;
    let rm = (instr & 0xF) as usize;

    if !is_long {
        let mut r = s.r[rm].wrapping_mul(s.r[rs]);
        if accumulate {
            r = r.wrapping_add(s.r[rn]);
        }
        s.r[rd] = r;
        if set_flags {
            s.set_nz(r);
        }
        return;
    }

    let signed = (instr & 0x00400000) != 0;
    let a = s.r[rm];
    let b = s.r[rs];
    let mut hi: u32;
    let mut lo: u32;
    if signed {
        // Signed 64-bit multiply via splitting.
        let a32 = a as i32;
        let b32 = b as i32;
        let big = (a32 as i64) * (b32 as i64);
        lo = (big as u64 & 0xFFFFFFFF) as u32;
        hi = (((big as u64) >> 32) & 0xFFFFFFFF) as u32;
    } else {
        let big = (a as u64) * (b as u64);
        lo = (big & 0xFFFFFFFF) as u32;
        hi = ((big >> 32) & 0xFFFFFFFF) as u32;
    }
    if accumulate {
        let acc_lo = s.r[rn]; // RdLo
        let acc_hi = s.r[rd]; // RdHi
        let sum_lo = lo.wrapping_add(acc_lo);
        let carry = if sum_lo < lo { 1 } else { 0 };
        let sum_hi = hi.wrapping_add(acc_hi).wrapping_add(carry);
        lo = sum_lo;
        hi = sum_hi;
    }
    s.r[rn] = lo;
    s.r[rd] = hi;
    if set_flags {
        s.set_nz64_hi(hi, lo);
    }
}

fn arm_swap<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let b = (instr & 0x00400000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let rm = (instr & 0xF) as usize;
    let addr = cpu.state.r[rn];
    if b {
        let tmp = bus.read8(addr);
        bus.write8(addr, cpu.state.r[rm] & 0xFF);
        cpu.state.r[rd] = tmp;
    } else {
        let aligned = bus.read32(addr & !3);
        let rot = (addr & 3) << 3;
        let tmp = if rot != 0 {
            (aligned >> rot) | (aligned << (32 - rot))
        } else {
            aligned
        };
        bus.write32(addr & !3, cpu.state.r[rm]);
        cpu.state.r[rd] = tmp;
    }
}

// ---------------------------------------------------------------- block transfer LDM/STM
fn arm_block_transfer<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let p = (instr & 0x01000000) != 0;
    let u = (instr & 0x00800000) != 0;
    let s_bit = (instr & 0x00400000) != 0;
    let w = (instr & 0x00200000) != 0;
    let l = (instr & 0x00100000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let list = instr & 0xFFFF;

    let mut count: u32 = 0;
    for i in 0..16 {
        if list & (1 << i) != 0 {
            count += 1;
        }
    }
    if count == 0 {
        // Empty list — ARM7TDMI quirk: loads/stores PC, increments by 0x40.
        if l {
            cpu.state.r[15] = bus.read32(cpu.state.r[rn] & !3);
            cpu.flush_pipeline();
        } else {
            bus.write32(cpu.state.r[rn] & !3, cpu.state.r[15]);
        }
        if w {
            cpu.state.r[rn] = if u {
                cpu.state.r[rn].wrapping_add(0x40)
            } else {
                cpu.state.r[rn].wrapping_sub(0x40)
            };
        }
        return;
    }
    let base = cpu.state.r[rn];
    let mut addr = if u {
        base
    } else {
        base.wrapping_sub(count << 2)
    };
    if u && p {
        addr = addr.wrapping_add(4);
    }
    if !u && !p {
        addr = addr.wrapping_add(4);
    }
    let writeback_addr = if u {
        base.wrapping_add(count << 2)
    } else {
        base.wrapping_sub(count << 2)
    };

    // S bit + R15 in list: load CPSR from SPSR (LDM with PC).
    // S bit without R15: user-mode register bank.
    let user_bank = s_bit && (list & 0x8000) == 0;
    let saved_mode = cpu.state.mode();
    if user_bank {
        cpu.state.switch_mode(mode::USR);
    }

    if l {
        let mut pc_loaded = false;
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let v = bus.read32(addr & !3);
            addr = addr.wrapping_add(4);
            if i == 15 {
                if s_bit {
                    let spsr = cpu.state.get_spsr();
                    cpu.state.switch_mode(spsr & 0x1F);
                    cpu.state.cpsr = spsr;
                }
                let thumb = (cpu.state.cpsr & FLAG_T) != 0;
                cpu.state.r[15] = if thumb { v & !1 } else { v & !3 };
                pc_loaded = true;
            } else {
                cpu.state.r[i as usize] = v;
            }
        }
        if pc_loaded {
            cpu.flush_pipeline();
        }
    } else {
        // STM with base register in list: ARM7 writes original base if first, new base otherwise.
        let mut first_stored = false;
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let mut v = cpu.state.r[i as usize];
            if i == 15 {
                v = v.wrapping_add(4);
            }
            if i as usize == rn && first_stored {
                v = writeback_addr;
            }
            bus.write32(addr & !3, v);
            addr = addr.wrapping_add(4);
            first_stored = true;
        }
    }

    if user_bank {
        cpu.state.switch_mode(saved_mode);
    }
    if w {
        cpu.state.r[rn] = writeback_addr;
    }
}

#[cfg(test)]
mod tests {
    //! ARM instruction-level vectors ported from the (deleted) TypeScript
    //! `src/test/arm.test.ts` + the ARM-flavored cases of `cpu.test.ts`.
    //! The harness mirrors the TS `makeCpu()` (SYS mode, ARM state, code+SP in
    //! IWRAM) and `load(bus, insns)`.

    use crate::bus::Bus;
    use crate::state::{FLAG_C, FLAG_N, FLAG_T, FLAG_V, FLAG_Z};
    use crate::Gba;

    // Single-instruction step: the CPU is owned by Gba; take it out so `g` can
    // act as its bus, step once, put it back (mirrors Emulator's frame loop).
    fn step(g: &mut Gba) {
        let mut cpu = std::mem::take(&mut g.cpu);
        cpu.step(g);
        g.cpu = cpu;
    }

    // Mirrors TS makeCpu(): SYS mode, ARM state, code+SP in IWRAM.
    fn setup(insns: &[u32]) -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]); // resets CPU + installs BIOS stub
        g.cpu.state.cpsr = 0x1F; // SYS, ARM
        g.cpu.state.r[15] = 0x0300_0000;
        g.cpu.state.r[13] = 0x0300_7F00;
        g.cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            Bus::write32(&mut g, 0x0300_0000 + (i as u32) * 4, insn);
        }
        g
    }

    // ---- ARM data-proc: barrel shifter via immediate ----

    #[test]
    fn mov_rotate_imm() {
        // MOV R0, #0xFF rotated by 8 = 0xFF000000
        let mut g = setup(&[0xE3A004FF]);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFF000000);
    }

    #[test]
    fn and_lsl_imm() {
        let mut g = setup(&[0xE0010202]); // AND R0, R1, R2, LSL #4
        g.cpu.state.r[1] = 0xFFFFFFFF;
        g.cpu.state.r[2] = 0x0F;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xF0);
    }

    #[test]
    fn orr_lsr_imm() {
        let mut g = setup(&[0xE1810822]); // ORR R0, R1, R2, LSR #16
        g.cpu.state.r[1] = 0xAA;
        g.cpu.state.r[2] = 0xBB000000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xBBAA);
    }

    #[test]
    fn mvn_zero() {
        let mut g = setup(&[0xE3E00000]); // MVN R0, #0 -> 0xFFFFFFFF
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFFF);
    }

    // ---- ARM data-proc: flag setting ----

    #[test]
    fn adds_sets_v_and_n_on_overflow() {
        let mut g = setup(&[0xE0910002]); // ADDS R0, R1, R2
        g.cpu.state.r[1] = 0x7FFFFFFF;
        g.cpu.state.r[2] = 1;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x80000000);
        assert!(g.cpu.state.cpsr & FLAG_V != 0); // V (signed overflow)
        assert!(g.cpu.state.cpsr & FLAG_N != 0); // N
    }

    #[test]
    fn subs_sets_c_no_borrow() {
        let mut g = setup(&[0xE0510002]); // SUBS R0, R1, R2
        g.cpu.state.r[1] = 10;
        g.cpu.state.r[2] = 3;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 7);
        assert!(g.cpu.state.cpsr & FLAG_C != 0); // C set
    }

    #[test]
    fn subs_clears_c_on_borrow() {
        let mut g = setup(&[0xE0510002]);
        g.cpu.state.r[1] = 3;
        g.cpu.state.r[2] = 10;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], (3u32).wrapping_sub(10));
        assert!(g.cpu.state.cpsr & FLAG_C == 0); // C clear
    }

    // cpu.test.ts: ARM data processing

    #[test]
    fn add_no_flags() {
        let mut g = setup(&[0xE0810002]); // ADD R0, R1, R2
        g.cpu.state.r[1] = 5;
        g.cpu.state.r[2] = 3;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 8);
    }

    #[test]
    fn adds_with_overflow_nzcv() {
        // 0x80000000 + 0x80000000 = 0 with C=1, V=1
        let mut g = setup(&[0xE0910002]);
        g.cpu.state.r[1] = 0x80000000;
        g.cpu.state.r[2] = 0x80000000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_N, 0);
        assert!(g.cpu.state.cpsr & FLAG_Z != 0);
        assert!(g.cpu.state.cpsr & FLAG_C != 0);
        assert!(g.cpu.state.cpsr & FLAG_V != 0);
    }

    #[test]
    fn subs_self_zero() {
        // SUBS R0, R0, R0 -> 0, nZCv
        let mut g = setup(&[0xE0500000]);
        g.cpu.state.r[0] = 42;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_N, 0);
        assert!(g.cpu.state.cpsr & FLAG_Z != 0);
        assert!(g.cpu.state.cpsr & FLAG_C != 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_V, 0);
    }

    #[test]
    fn cmp_borrow() {
        // CMP R0, R1 with 5,10 -> Nzcv
        let mut g = setup(&[0xE1500001]);
        g.cpu.state.r[0] = 5;
        g.cpu.state.r[1] = 10;
        step(&mut g);
        assert!(g.cpu.state.cpsr & FLAG_N != 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_Z, 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_C, 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_V, 0);
    }

    #[test]
    fn sbcs_with_borrow() {
        // SUBS R0,R0,#1 with R0=0 -> C=0 (borrow). Then SBC R3,R4,R5 = 10-3-1 = 6.
        let mut g = setup(&[0xE2500001, 0xE0D43005]);
        g.cpu.state.r[0] = 0;
        g.cpu.state.r[4] = 10;
        g.cpu.state.r[5] = 3;
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[3], 6);
    }

    #[test]
    fn lsl_by_reg_32() {
        // MOV R0,#0xFF; MOV R1,#0x20; MOVS R0,R0,LSL R1 -> 0, C=bit0(0xFF)=1
        let mut g = setup(&[0xE3A000FF, 0xE3A01020, 0xE1B00110]);
        step(&mut g);
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_N, 0);
        assert!(g.cpu.state.cpsr & FLAG_Z != 0);
        assert!(g.cpu.state.cpsr & FLAG_C != 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_V, 0);
    }

    // ---- Shifter edge cases (cpu.test.ts) ----

    #[test]
    fn lsr_by_reg_zero_is_noop() {
        // MOV R0,#0x5A; MOV R1,#0; LSRS R0,R0,R1 -> R0 unchanged
        let mut g = setup(&[0xE3A0005A, 0xE3A01000, 0xE1B00130]);
        step(&mut g);
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x5A);
    }

    #[test]
    fn asr_by_reg_32_sign_extend() {
        // MOV R0,#0x80000000; MOV R1,#32; MOVS R0,R0,ASR R1 -> 0xFFFFFFFF, C=1
        let mut g = setup(&[0xE3A00102, 0xE3A01020, 0xE1B00150]);
        g.cpu.state.r[0] = 0x80000000;
        step(&mut g);
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFFF);
        assert_eq!(g.cpu.state.cpsr & FLAG_C, FLAG_C);
    }

    #[test]
    fn ror_by_reg_16() {
        // MOVS R0,R0,ROR R1 with R0=0xABCD1234, R1=16
        let mut g = setup(&[0xE1B00170]);
        g.cpu.state.r[0] = 0xABCD1234;
        g.cpu.state.r[1] = 16;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x1234ABCD);
    }

    #[test]
    fn rrx_imm() {
        // MOV R0,R0,RRX with C=1, val=0x80000001 -> (C<<31)|(val>>1) = 0xC0000000
        let mut g = setup(&[0xE1A00060]);
        g.cpu.state.r[0] = 0x80000001;
        g.cpu.state.cpsr |= FLAG_C;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xC0000000);
    }

    // ---- ARM multiply ----

    #[test]
    fn mul() {
        let mut g = setup(&[0xE0000291]); // MUL R0, R1, R2
        g.cpu.state.r[1] = 7;
        g.cpu.state.r[2] = 6;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 42);
    }

    #[test]
    fn mla() {
        let mut g = setup(&[0xE0203291]); // MLA R0, R1, R2, R3
        g.cpu.state.r[1] = 7;
        g.cpu.state.r[2] = 6;
        g.cpu.state.r[3] = 100;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 142);
    }

    #[test]
    fn smull_negative() {
        // SMULL R0, R1, R2, R3 : -2 * 5 = -10
        let mut g = setup(&[0xE0C10392]);
        g.cpu.state.r[2] = 0xFFFFFFFE; // -2
        g.cpu.state.r[3] = 5;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFF6);
        assert_eq!(g.cpu.state.r[1], 0xFFFFFFFF);
    }

    #[test]
    fn umull_max() {
        // UMULL R0, R1, R2, R3 : 0xFFFFFFFF^2 = 0xFFFFFFFE00000001
        let mut g = setup(&[0xE0810392]);
        g.cpu.state.r[2] = 0xFFFFFFFF;
        g.cpu.state.r[3] = 0xFFFFFFFF;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x00000001);
        assert_eq!(g.cpu.state.r[1], 0xFFFFFFFE);
    }

    // ---- ARM LDR/STR addressing modes ----

    #[test]
    fn ldr_pre_indexed() {
        let mut g = setup(&[0xE5910008]); // LDR R0, [R1, #8]
        Bus::write32(&mut g, 0x03001008, 0xDEADBEEF);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xDEADBEEF);
    }

    #[test]
    fn ldr_pre_indexed_writeback() {
        let mut g = setup(&[0xE5B10008]); // LDR R0, [R1, #8]!
        Bus::write32(&mut g, 0x03001008, 0xCAFEBABE);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xCAFEBABE);
        assert_eq!(g.cpu.state.r[1], 0x03001008);
    }

    #[test]
    fn ldr_post_indexed() {
        let mut g = setup(&[0xE4910004]); // LDR R0, [R1], #4
        Bus::write32(&mut g, 0x03001000, 0x11223344);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x11223344);
        assert_eq!(g.cpu.state.r[1], 0x03001004);
    }

    #[test]
    fn str_negative_reg_offset() {
        let mut g = setup(&[0xE7010002]); // STR R0, [R1, -R2]
        g.cpu.state.r[0] = 0xAA;
        g.cpu.state.r[1] = 0x03001100;
        g.cpu.state.r[2] = 0x100;
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, 0x03001000), 0xAA);
    }

    #[test]
    fn ldrh_imm() {
        let mut g = setup(&[0xE1D100B4]); // LDRH R0, [R1, #4]
        Bus::write16(&mut g, 0x03001004, 0xABCD);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xABCD);
    }

    #[test]
    fn ldrsb_sign_ext() {
        let mut g = setup(&[0xE1D100D0]); // LDRSB R0, [R1, #0]
        Bus::write8(&mut g, 0x03001000, 0xFE); // -2
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFFE);
    }

    #[test]
    fn ldrsh_sign_ext() {
        let mut g = setup(&[0xE1D100F0]); // LDRSH R0, [R1, #0]
        Bus::write16(&mut g, 0x03001000, 0x8000);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFF8000);
    }

    // cpu.test.ts: LDR unaligned rotate + LDRSH unaligned. These use a small
    // ARM sequence: LDR R0,[PC,#8]; <load>; BX LR; .word 0; .word addr.
    #[test]
    fn ldr_unaligned_rotates() {
        // LDR R0,[PC,#8]; LDR R1,[R0]; BX LR; 0; 0x03000101
        let mut g = setup(&[0xE59F0008, 0xE5901000, 0xE12FFF1E, 0x00000000, 0x03000101]);
        Bus::write32(&mut g, 0x03000100, 0xDEADBEEF);
        step(&mut g);
        step(&mut g);
        // addr 0x101, rot=8: 0xDEADBEEF ror 8 = 0xEFDEADBE
        assert_eq!(g.cpu.state.r[1], 0xEFDEADBE);
    }

    #[test]
    fn ldrsh_unaligned_reads_byte() {
        // LDR R0,[PC,#8]; LDRSH R1,[R0]; BX LR; 0; 0x03000101
        let mut g = setup(&[0xE59F0008, 0xE1D010F0, 0xE12FFF1E, 0x00000000, 0x03000101]);
        Bus::write16(&mut g, 0x03000100, 0xAA55);
        step(&mut g);
        step(&mut g);
        // unaligned -> read byte at 0x101 = 0xAA, sign-extend
        assert_eq!(g.cpu.state.r[1], 0xFFFFFFAA);
    }

    // ---- ARM block transfer (LDM/STM) ----

    #[test]
    fn stmia_writeback_ascending() {
        let mut g = setup(&[0xE8A0000E]); // STMIA R0!, {R1, R2, R3}
        g.cpu.state.r[0] = 0x03001000;
        g.cpu.state.r[1] = 0xAA;
        g.cpu.state.r[2] = 0xBB;
        g.cpu.state.r[3] = 0xCC;
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, 0x03001000), 0xAA);
        assert_eq!(Bus::read32(&mut g, 0x03001004), 0xBB);
        assert_eq!(Bus::read32(&mut g, 0x03001008), 0xCC);
        assert_eq!(g.cpu.state.r[0], 0x0300100C);
    }

    #[test]
    fn ldmdb_writeback() {
        let mut g = setup(&[0xE930000E]); // LDMDB R0!, {R1, R2, R3}
        g.cpu.state.r[0] = 0x0300100C;
        Bus::write32(&mut g, 0x03001000, 0xAA);
        Bus::write32(&mut g, 0x03001004, 0xBB);
        Bus::write32(&mut g, 0x03001008, 0xCC);
        step(&mut g);
        assert_eq!(g.cpu.state.r[1], 0xAA);
        assert_eq!(g.cpu.state.r[2], 0xBB);
        assert_eq!(g.cpu.state.r[3], 0xCC);
        assert_eq!(g.cpu.state.r[0], 0x03001000);
    }

    #[test]
    fn ldm_pc_in_list() {
        let mut g = setup(&[0xE8BD8000]); // LDMIA R13!, {PC}
        g.cpu.state.r[13] = 0x03001000;
        Bus::write32(&mut g, 0x03001000, 0x03002000);
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03002000);
    }

    #[test]
    fn stmia_two_regs_writeback() {
        let mut g = setup(&[0xE8A00006]); // STMIA R0!, {R1, R2}
        g.cpu.state.r[0] = 0x03000100;
        g.cpu.state.r[1] = 0x11111111;
        g.cpu.state.r[2] = 0x22222222;
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, 0x03000100), 0x11111111);
        assert_eq!(Bus::read32(&mut g, 0x03000104), 0x22222222);
        assert_eq!(g.cpu.state.r[0], 0x03000108);
    }

    #[test]
    fn ldmdb_two_regs_writeback() {
        let mut g = setup(&[0xE9300006]); // LDMDB R0!, {R1, R2}
        g.cpu.state.r[0] = 0x03000108;
        Bus::write32(&mut g, 0x03000100, 0xAAAAAAAA);
        Bus::write32(&mut g, 0x03000104, 0xBBBBBBBB);
        step(&mut g);
        assert_eq!(g.cpu.state.r[1], 0xAAAAAAAA);
        assert_eq!(g.cpu.state.r[2], 0xBBBBBBBB);
        assert_eq!(g.cpu.state.r[0], 0x03000100);
    }

    // ---- ARM PSR transfer (MRS/MSR) ----

    #[test]
    fn mrs_cpsr() {
        let mut g = setup(&[0xE10F0000]); // MRS R0, CPSR
        g.cpu.state.cpsr = 0x6000001F;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x6000001F);
    }

    #[test]
    fn msr_cpsr_c_changes_mode() {
        let mut g = setup(&[0xE129F000]); // MSR CPSR_c, R0
        g.cpu.state.r[0] = 0x13; // SVC
        step(&mut g);
        assert_eq!(g.cpu.state.cpsr & 0x1F, 0x13);
    }

    #[test]
    fn mrs_cpsr_with_flags() {
        let mut g = setup(&[0xE10F0000]);
        g.cpu.state.cpsr = 0x1F | FLAG_N | FLAG_C;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x1F | FLAG_N | FLAG_C);
    }

    #[test]
    fn msr_cpsr_c_to_irq() {
        let mut g = setup(&[0xE129F000]);
        g.cpu.state.r[0] = crate::state::mode::IRQ;
        step(&mut g);
        assert_eq!(g.cpu.state.cpsr & 0x1F, crate::state::mode::IRQ);
    }

    // ---- ARM SWP (atomic swap) ----

    #[test]
    fn swp_word() {
        let mut g = setup(&[0xE1020091]); // SWP R0, R1, [R2]
        g.cpu.state.r[1] = 0xAA;
        g.cpu.state.r[2] = 0x03001000;
        Bus::write32(&mut g, 0x03001000, 0xBB);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xBB);
        assert_eq!(Bus::read32(&mut g, 0x03001000), 0xAA);
    }

    #[test]
    fn swpb_byte() {
        let mut g = setup(&[0xE1420091]); // SWPB R0, R1, [R2]
        g.cpu.state.r[1] = 0xCC;
        g.cpu.state.r[2] = 0x03001000;
        Bus::write8(&mut g, 0x03001000, 0xDD);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xDD);
        assert_eq!(Bus::read8(&mut g, 0x03001000), 0xCC);
    }

    // ---- ARM BX (branch and exchange) ----

    #[test]
    fn bx_to_thumb() {
        let mut g = setup(&[0xE12FFF10]); // BX R0
        g.cpu.state.r[0] = 0x03001001;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03001000);
        assert!(g.cpu.state.cpsr & FLAG_T != 0);
    }

    #[test]
    fn bx_to_thumb_sets_t_clears_arm() {
        // cpu.test.ts: ARM BX R0 with R0 = 0x03000011
        let mut g = setup(&[0xE12FFF10]);
        g.cpu.state.r[0] = 0x03000011;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03000010);
        assert_eq!(g.cpu.state.cpsr & FLAG_T, FLAG_T);
    }
}
