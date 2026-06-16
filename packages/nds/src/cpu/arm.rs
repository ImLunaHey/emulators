//! ARM (32-bit) instruction interpreter for the DS, covering BOTH cores:
//! the ARM7 (ARMv4T) and the ARM9 (ARMv5TE). The ARMv4 paths are adapted from
//! the GBA core's tested `arm.rs`; the ARMv5 deltas (BLX(1)/BLX(2), CLZ,
//! QADD/QSUB/QDADD/QDSUB saturating arithmetic, the DSP halfword multiplies,
//! LDRD/STRD, CP15 MCR/MRC, and the LDR/LDM-to-PC bit0→THUMB interworking) are
//! gated on `cpu.arch.is_v5()`, cross-checked against
//! ../../ds-recomp/src/cpu/arm.ts.

use crate::cpu::exec::Cpu;
use crate::cpu::shifter::{apply_carry, imm_shift, reg_shift, ror_imm32};
use crate::nds::Nds;
use crate::state::{mode, CpuState, FLAG_T};

/// Sticky saturation flag — CPSR bit 27 (Q). Set by the ARMv5E DSP ops.
const FLAG_Q: u32 = 0x0800_0000;

// ── Add/sub flag helpers — return the result and set N/Z/C/V. ───────────────
fn add_set_flags(s: &mut CpuState, a: u32, b: u32) -> u32 {
    let r = a.wrapping_add(b);
    s.set_nz(r);
    s.set_c(r < a);
    s.set_v((!(a ^ b) & (a ^ r) & 0x8000_0000) != 0);
    r
}
fn adc_set_flags(s: &mut CpuState, a: u32, b: u32, c_in: u32) -> u32 {
    let sum = a as u64 + b as u64 + c_in as u64;
    let r = sum as u32;
    s.set_nz(r);
    s.set_c(sum > 0xFFFF_FFFF);
    s.set_v((!(a ^ b) & (a ^ r) & 0x8000_0000) != 0);
    r
}
fn sub_set_flags(s: &mut CpuState, a: u32, b: u32) -> u32 {
    let r = a.wrapping_sub(b);
    s.set_nz(r);
    s.set_c(a >= b);
    s.set_v(((a ^ b) & (a ^ r) & 0x8000_0000) != 0);
    r
}
fn sbc_set_flags(s: &mut CpuState, a: u32, b: u32, c_in: u32) -> u32 {
    // ARM SBC: Rd = Rn - Rm - NOT(C).
    let not_c = c_in ^ 1;
    let r = a.wrapping_sub(b).wrapping_sub(not_c);
    s.set_nz(r);
    s.set_c((a as u64) >= (b as u64 + not_c as u64));
    s.set_v(((a ^ b) & (a ^ r) & 0x8000_0000) != 0);
    r
}

pub fn arm_execute(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let cond = (instr >> 28) & 0xF;

    // v5 unconditional space (cond == 0xF). On the ARM9 this is BLX(1) imm
    // (or PLD, which we treat as a NOP). On the ARM7 the whole space is
    // undefined — but PLD-shaped opcodes appear from shared toolchains, so a
    // NOP is the pragmatic floor.
    if cond == 0xF {
        if cpu.arch.is_v5() && ((instr >> 25) & 0b111) == 0b101 {
            // BLX(1): LR = next ARM PC, target = PC + signExt(off24)<<2 + H<<1,
            // and switch to THUMB.
            let mut off = instr & 0x00FF_FFFF;
            if off & 0x0080_0000 != 0 {
                off |= 0xFF00_0000;
            }
            let h = (instr >> 24) & 1;
            cpu.state.r[14] = cpu.state.r[15].wrapping_sub(4);
            let target = cpu
                .state
                .r[15]
                .wrapping_add(off << 2)
                .wrapping_add(h << 1);
            cpu.state.cpsr |= FLAG_T;
            cpu.state.r[15] = target & !1;
            cpu.flush_pipeline();
        }
        // PLD / any other unconditional code: NOP.
        return;
    }

    if cond != 0xE && !cpu.state.check_cond(cond) {
        return;
    }

    // Branch and Branch with Link: 101x cccc.
    if (instr & 0x0E00_0000) == 0x0A00_0000 {
        let mut offset = (instr & 0x00FF_FFFF) << 2;
        if offset & 0x0200_0000 != 0 {
            offset |= 0xFC00_0000;
        }
        if instr & 0x0100_0000 != 0 {
            cpu.state.r[14] = cpu.state.r[15].wrapping_sub(4); // BL: LR = next
        }
        cpu.state.r[15] = cpu.state.r[15].wrapping_add(offset);
        cpu.flush_pipeline();
        return;
    }

    // BX / BLX(2) — both share the prefix 0001 0010 1111 1111 1111 ...001.
    // (bit 5 = 1 for BLX(2), 0 for BX.)
    if (instr & 0x0FFF_FFD0) == 0x012F_FF10 {
        let link = (instr & 0x20) != 0;
        let rn = (instr & 0xF) as usize;
        let tgt = cpu.state.r[rn];
        if link && cpu.arch.is_v5() {
            // BLX(2): LR = address of the next ARM instruction. ARM7 has no
            // BLX(2) — there we fall through to a plain BX (pragmatic stub).
            cpu.state.r[14] = cpu.state.r[15].wrapping_sub(4);
        }
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

    // CLZ (ARMv5+): count leading zeros.
    if cpu.arch.is_v5() && (instr & 0x0FFF_0FF0) == 0x016F_0F10 {
        let rm = (instr & 0xF) as usize;
        let rd = ((instr >> 12) & 0xF) as usize;
        cpu.state.r[rd] = cpu.state.r[rm].leading_zeros();
        return;
    }

    // ARMv5E DSP saturating arithmetic (QADD / QSUB / QDADD / QDSUB):
    //   bits 27:24=0001, bit 23=0, bit 20=0, bits 7:4=0101.
    if cpu.arch.is_v5() && (instr & 0x0F90_00F0) == 0x0100_0050 {
        arm_saturation(cpu, instr);
        return;
    }
    // ARMv5E DSP halfword multiplies (SMLAxy / SMLAWy / SMULWy / SMLALxy /
    // SMULxy): bits 27:24=0001, bit 23=0, bit 20=0, bit 7=1, bit 4=0.
    if cpu.arch.is_v5() && (instr & 0x0F90_0090) == 0x0100_0080 {
        arm_dsp_multiply(cpu, instr);
        return;
    }

    // MCR / MRC — CP15 access on the ARM9. The ARM7 has no CP15; we accept
    // the decode (as a NOP) to avoid spurious undefined traps from shared
    // toolchains.
    if (instr & 0x0F00_0010) == 0x0E00_0010 {
        if cpu.arch.is_v5() {
            let is_read = (instr & 0x0010_0000) != 0;
            let cp_num = (instr >> 8) & 0xF;
            let opc1 = (instr >> 21) & 0x7;
            let opc2 = (instr >> 5) & 0x7;
            let crn = (instr >> 16) & 0xF;
            let crm = instr & 0xF;
            let rd = ((instr >> 12) & 0xF) as usize;
            if cp_num == 15 {
                if is_read {
                    cpu.state.r[rd] = nds.cp15_read(opc1, crn, crm, opc2);
                } else {
                    nds.cp15_write(opc1, crn, crm, opc2, cpu.state.r[rd]);
                }
            }
        }
        return;
    }

    // CDP + LDC/STC + MCRR/MRRC: any coprocessor besides CP15 is unimplemented
    // on the DS. Treat as NOP rather than risk mis-decoding into data
    // processing (Pokemon Platinum emits a CP6 CDP that, decoded as
    // ADC R15, would jump off the world).
    if (instr & 0x0F00_0010) == 0x0E00_0000 {
        return; // CDP
    }
    if (instr & 0x0E00_0000) == 0x0C00_0000 {
        return; // LDC/STC + MCRR/MRRC
    }

    // SWI.
    if (instr & 0x0F00_0000) == 0x0F00_0000 {
        cpu.swi(nds, instr & 0x00FF_FFFF);
        return;
    }

    // Permanently UNDEFINED encoding (ARMv5 "UDF" space): bits 27:20 = 0111_1111
    // and bits 7:4 = 1111. No toolchain emits this as a valid instruction, so a
    // game hitting it has jumped into garbage / corrupted its code — take the
    // real undefined-instruction exception (the fault-loop watcher counts it).
    if (instr & 0x0FF0_00F0) == 0x07F0_00F0 {
        cpu.undefined_instruction();
        return;
    }

    // Block data transfer LDM/STM: 100x.
    if (instr & 0x0E00_0000) == 0x0800_0000 {
        arm_block_transfer(cpu, nds, instr);
        return;
    }

    // Single data transfer LDR/STR: 01xx.
    if (instr & 0x0C00_0000) == 0x0400_0000 {
        arm_single_transfer(cpu, nds, instr);
        return;
    }

    // Halfword / signed transfer / multiply / swap / LDRD-STRD — the 000x
    // "extension space" (bit 4 + bit 7 set).
    if (instr & 0x0E00_0090) == 0x0000_0090 {
        let is_hw = (instr & 0x60) != 0; // any bits in 5/6 → halfword/signed
        if is_hw {
            // ARMv5: LDRD/STRD share this decode space (L bit = 0, op = 10/11).
            let op = (instr >> 5) & 3;
            let l = (instr & 0x0010_0000) != 0;
            if cpu.arch.is_v5() && !l && (op == 2 || op == 3) {
                arm_double_transfer(cpu, nds, instr, op == 3 /* store? */);
                return;
            }
            arm_half_transfer(cpu, nds, instr);
            return;
        }
        // bit 24 distinguishes multiply (0) vs swap (1).
        if (instr & 0x0100_0000) == 0 {
            arm_multiply(cpu, instr);
            return;
        }
        arm_swap(cpu, nds, instr);
        return;
    }

    // PSR transfer: MRS / MSR (register form).
    if (instr & 0x0F90_0000) == 0x0100_0000 && (instr & 0x90) != 0x90 {
        if instr & 0x0020_0000 != 0 {
            arm_msr(cpu, instr);
            return;
        }
        arm_mrs(cpu, instr);
        return;
    }
    if (instr & 0x0FB0_0000) == 0x0320_0000 {
        arm_msr_imm(cpu, instr);
        return;
    }

    // Data processing (immediate or register operand).
    arm_data_processing(cpu, instr);
}

// ──────────────────────────────────────────────────────────── data processing
fn arm_data_processing(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let opcode = (instr >> 21) & 0xF;
    let set_flags = (instr & 0x0010_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let mut op1 = s.r[rn];
    let op2: u32;
    let mut shifter_carry = s.c();

    if instr & 0x0200_0000 != 0 {
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
            // Register-specified shift amount: R15 reads as +12 (decode + 12).
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
            // Writing PC with the S bit copies SPSR→CPSR (mode/T change).
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

// ──────────────────────────────────────────────────────────────────── MRS/MSR
fn arm_mrs(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let rd = ((instr >> 12) & 0xF) as usize;
    s.r[rd] = if instr & 0x0040_0000 != 0 {
        s.get_spsr()
    } else {
        s.cpsr
    };
}
fn arm_msr(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_spsr = (instr & 0x0040_0000) != 0;
    let val = s.r[(instr & 0xF) as usize];
    apply_msr(s, is_spsr, instr, val);
}
fn arm_msr_imm(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_spsr = (instr & 0x0040_0000) != 0;
    let imm = instr & 0xFF;
    let rot = ((instr >> 8) & 0xF) << 1;
    let val = ror_imm32(imm, rot);
    apply_msr(s, is_spsr, instr, val);
}
fn apply_msr(s: &mut CpuState, is_spsr: bool, instr: u32, val: u32) {
    let mut mask: u32 = 0;
    if instr & 0x0001_0000 != 0 {
        mask |= 0x0000_00FF; // control field — privileged modes only
    }
    if instr & 0x0002_0000 != 0 {
        mask |= 0x0000_FF00;
    }
    if instr & 0x0004_0000 != 0 {
        mask |= 0x00FF_0000;
    }
    if instr & 0x0008_0000 != 0 {
        mask |= 0xFF00_0000;
    }
    if is_spsr {
        s.set_spsr((s.get_spsr() & !mask) | (val & mask));
        return;
    }
    // No mode change from USR.
    if s.mode() == mode::USR {
        mask &= 0xFF00_0000;
    }
    let new_cpsr = (s.cpsr & !mask) | (val & mask);
    let new_mode = new_cpsr & 0x1F;
    if new_mode != s.mode() {
        s.switch_mode(new_mode);
    }
    s.cpsr = new_cpsr;
}

// ────────────────────────────────────────────────── single transfer LDR/STR
fn arm_single_transfer(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let i_bit = (instr & 0x0200_0000) != 0;
    let p = (instr & 0x0100_0000) != 0;
    let u = (instr & 0x0080_0000) != 0;
    let b = (instr & 0x0040_0000) != 0;
    let w = (instr & 0x0020_0000) != 0;
    let l = (instr & 0x0010_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;

    let base = cpu.state.r[rn];
    let offset: u32 = if i_bit {
        let rm = (instr & 0xF) as usize;
        let shift_type = (instr >> 5) & 3;
        let imm = (instr >> 7) & 0x1F;
        imm_shift(shift_type, imm, cpu.state.r[rm], cpu.state.c()).value
    } else {
        instr & 0xFFF
    };
    let eff = if u {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if p { eff } else { base };
    let writeback = !p || w;

    if l {
        let value: u32 = if b {
            cpu.read8(nds, addr)
        } else {
            // LDR with an unaligned address: read the aligned word, rotate.
            let aligned = cpu.read32(nds, addr & !3);
            let rot = (addr & 3) << 3;
            if rot != 0 {
                (aligned >> rot) | (aligned << (32 - rot))
            } else {
                aligned
            }
        };
        if writeback && rn != rd {
            cpu.state.r[rn] = eff;
        }
        if rd == 15 {
            // ARMv5 LDR-to-PC interworking: bit 0 selects THUMB.
            if cpu.arch.is_v5() && (value & 1) != 0 {
                cpu.state.cpsr |= FLAG_T;
                cpu.state.r[15] = value & !1;
            } else {
                cpu.state.r[15] = value & !3;
            }
            cpu.flush_pipeline();
        } else {
            cpu.state.r[rd] = value;
        }
    } else {
        let mut val = cpu.state.r[rd];
        if rd == 15 {
            val = val.wrapping_add(4); // STR Rd=PC stores pc+12 of the instr
        }
        if b {
            cpu.write8(nds, addr, val & 0xFF);
        } else {
            cpu.write32(nds, addr & !3, val);
        }
        if writeback {
            cpu.state.r[rn] = eff;
        }
    }
}

// ───────────────────────────────────────────── halfword / signed transfer
fn arm_half_transfer(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let p = (instr & 0x0100_0000) != 0;
    let u = (instr & 0x0080_0000) != 0;
    let i_bit = (instr & 0x0040_0000) != 0; // immediate-offset variant
    let w = (instr & 0x0020_0000) != 0;
    let l = (instr & 0x0010_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let sh = (instr >> 5) & 3; // 01 = H, 10 = SB, 11 = SH

    let base = cpu.state.r[rn];
    let offset: u32 = if i_bit {
        ((instr >> 4) & 0xF0) | (instr & 0xF)
    } else {
        cpu.state.r[(instr & 0xF) as usize]
    };

    let eff = if u {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if p { eff } else { base };
    let writeback = !p || w;

    if l {
        let value: u32 = match sh {
            1 => {
                // LDRH — unaligned reads rotate.
                let aligned = cpu.read16(nds, addr & !1);
                if addr & 1 != 0 {
                    (aligned >> 8) | (aligned << 24)
                } else {
                    aligned
                }
            }
            2 => {
                // LDRSB.
                let byte = cpu.read8(nds, addr);
                if byte & 0x80 != 0 {
                    byte | 0xFFFF_FF00
                } else {
                    byte
                }
            }
            3 => {
                // LDRSH — unaligned drops to LDRSB.
                if addr & 1 != 0 {
                    let byte = cpu.read8(nds, addr);
                    if byte & 0x80 != 0 {
                        byte | 0xFFFF_FF00
                    } else {
                        byte
                    }
                } else {
                    let h = cpu.read16(nds, addr & !1);
                    if h & 0x8000 != 0 {
                        h | 0xFFFF_0000
                    } else {
                        h
                    }
                }
            }
            _ => 0,
        };
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
            cpu.write16(nds, addr & !1, cpu.state.r[rd] & 0xFFFF);
        }
        if writeback {
            cpu.state.r[rn] = eff;
        }
    }
}

// ───────────────────────────────────────── ARMv5 LDRD / STRD (paired transfer)
// Rd must be even; the pair is (Rd, Rd+1). `is_store` ⇒ STRD (op == 3),
// otherwise LDRD (op == 2).
fn arm_double_transfer(cpu: &mut Cpu, nds: &mut Nds, instr: u32, is_store: bool) {
    let p = (instr & 0x0100_0000) != 0;
    let u = (instr & 0x0080_0000) != 0;
    let i_bit = (instr & 0x0040_0000) != 0;
    let w = (instr & 0x0020_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;

    let base = cpu.state.r[rn];
    let offset: u32 = if i_bit {
        ((instr >> 4) & 0xF0) | (instr & 0xF)
    } else {
        cpu.state.r[(instr & 0xF) as usize]
    };
    let eff = if u {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if p { eff } else { base };
    let writeback = !p || w;

    if is_store {
        let lo = cpu.state.r[rd];
        let hi = cpu.state.r[(rd + 1) & 0xF];
        cpu.write32(nds, addr & !3, lo);
        cpu.write32(nds, addr.wrapping_add(4) & !3, hi);
    } else {
        let lo = cpu.read32(nds, addr & !3);
        let hi = cpu.read32(nds, addr.wrapping_add(4) & !3);
        cpu.state.r[rd] = lo;
        cpu.state.r[(rd + 1) & 0xF] = hi;
    }
    if writeback {
        cpu.state.r[rn] = eff;
    }
}

// ──────────────────────────────────────────────────────────────────── multiply
fn arm_multiply(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let is_long = (instr & 0x0080_0000) != 0;
    let set_flags = (instr & 0x0010_0000) != 0;
    let accumulate = (instr & 0x0020_0000) != 0;
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

    let signed = (instr & 0x0040_0000) != 0;
    let a = s.r[rm];
    let b = s.r[rs];
    let (mut hi, mut lo): (u32, u32);
    if signed {
        let big = (a as i32 as i64) * (b as i32 as i64);
        lo = (big as u64 & 0xFFFF_FFFF) as u32;
        hi = (((big as u64) >> 32) & 0xFFFF_FFFF) as u32;
    } else {
        let big = (a as u64) * (b as u64);
        lo = (big & 0xFFFF_FFFF) as u32;
        hi = ((big >> 32) & 0xFFFF_FFFF) as u32;
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

// ──────────────────────────────────────────────── ARMv5E saturating arithmetic
/// Signed 32-bit saturate; returns the clamped value + whether it saturated.
fn sat32(value: i64) -> (u32, bool) {
    if value > i32::MAX as i64 {
        (0x7FFF_FFFF, true)
    } else if value < i32::MIN as i64 {
        (0x8000_0000, true)
    } else {
        (value as i32 as u32, false)
    }
}

fn arm_saturation(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let op = (instr >> 21) & 0x3; // 00=QADD 01=QSUB 10=QDADD 11=QDSUB
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let rm = (instr & 0xF) as usize;
    let a = s.r[rm] as i32 as i64;
    let b = s.r[rn] as i32 as i64;
    let (result, saturated) = match op {
        0 => sat32(a + b),                     // QADD: sat(Rm + Rn)
        1 => sat32(a - b),                     // QSUB: sat(Rm - Rn)
        2 => {
            let (dbl, ds) = sat32(b * 2); // QDADD: sat(Rm + sat(2*Rn))
            let (r, rs) = sat32(a + dbl as i32 as i64);
            (r, rs || ds)
        }
        _ => {
            let (dbl, ds) = sat32(b * 2); // QDSUB: sat(Rm - sat(2*Rn))
            let (r, rs) = sat32(a - dbl as i32 as i64);
            (r, rs || ds)
        }
    };
    s.r[rd] = result;
    if saturated {
        s.cpsr |= FLAG_Q;
    }
}

// ─────────────────────────────────────────────────── ARMv5E DSP multiplies
fn arm_dsp_multiply(cpu: &mut Cpu, instr: u32) {
    let s = &mut cpu.state;
    let op = (instr >> 21) & 0x3; // 00=SMLAxy 01=SMLAW/SMULW 10=SMLALxy 11=SMULxy
    let rd_hi = ((instr >> 16) & 0xF) as usize; // also Rd for SMLA/SMUL/W
    let rn = ((instr >> 12) & 0xF) as usize; // accumulator low (SMLA/SMLAL)
    let rs = ((instr >> 8) & 0xF) as usize;
    let x = (instr >> 5) & 1; // Rm half select
    let y = (instr >> 6) & 1; // Rs half select

    let rm_val = s.r[(instr & 0xF) as usize];
    let rs_val = s.r[rs];
    // Sign-extended 16-bit halves.
    let rm_half: i32 = if x != 0 {
        (rm_val as i32) >> 16
    } else {
        (rm_val as i16) as i32
    };
    let rs_half: i32 = if y != 0 {
        (rs_val as i32) >> 16
    } else {
        (rs_val as i16) as i32
    };

    match op {
        0x0 => {
            // SMLAxy: Rd = Rm.x * Rs.y + Rn; Q on signed overflow.
            let product = rm_half.wrapping_mul(rs_half);
            let acc = s.r[rn] as i32;
            let sum = product.wrapping_add(acc);
            if (product ^ sum) & (acc ^ sum) < 0 {
                s.cpsr |= FLAG_Q;
            }
            s.r[rd_hi] = sum as u32;
        }
        0x1 => {
            // SMLAWy / SMULWy: (Rm[32] * Rs.y) >> 16.
            let big = (rm_val as i32 as i64) * (rs_half as i64);
            let product32 = (big >> 16) as i32;
            if x == 0 {
                // SMLAWy: + Rn, set Q on overflow.
                let acc = s.r[rn] as i32;
                let sum = product32.wrapping_add(acc);
                if (product32 ^ sum) & (acc ^ sum) < 0 {
                    s.cpsr |= FLAG_Q;
                }
                s.r[rd_hi] = sum as u32;
            } else {
                // SMULWy.
                s.r[rd_hi] = product32 as u32;
            }
        }
        0x2 => {
            // SMLALxy: 64-bit accumulate (RdHi:Rn += Rm.x * Rs.y).
            let product = (rm_half as i64) * (rs_half as i64);
            let acc = ((s.r[rd_hi] as i32 as i64) << 32) | (s.r[rn] as i64);
            let sum = acc.wrapping_add(product) as u64;
            s.r[rn] = (sum & 0xFFFF_FFFF) as u32;
            s.r[rd_hi] = ((sum >> 32) & 0xFFFF_FFFF) as u32;
        }
        _ => {
            // SMULxy: Rd = Rm.x * Rs.y.
            s.r[rd_hi] = rm_half.wrapping_mul(rs_half) as u32;
        }
    }
}

// ─────────────────────────────────────────────────────────────── SWP / SWPB
fn arm_swap(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let b = (instr & 0x0040_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let rd = ((instr >> 12) & 0xF) as usize;
    let rm = (instr & 0xF) as usize;
    let addr = cpu.state.r[rn];
    if b {
        let tmp = cpu.read8(nds, addr);
        cpu.write8(nds, addr, cpu.state.r[rm] & 0xFF);
        cpu.state.r[rd] = tmp;
    } else {
        let aligned = cpu.read32(nds, addr & !3);
        let rot = (addr & 3) << 3;
        let tmp = if rot != 0 {
            (aligned >> rot) | (aligned << (32 - rot))
        } else {
            aligned
        };
        cpu.write32(nds, addr & !3, cpu.state.r[rm]);
        cpu.state.r[rd] = tmp;
    }
}

// ────────────────────────────────────────────────── block transfer LDM/STM
fn arm_block_transfer(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let p = (instr & 0x0100_0000) != 0;
    let u = (instr & 0x0080_0000) != 0;
    let s_bit = (instr & 0x0040_0000) != 0;
    let w = (instr & 0x0020_0000) != 0;
    let l = (instr & 0x0010_0000) != 0;
    let rn = ((instr >> 16) & 0xF) as usize;
    let list = instr & 0xFFFF;

    let count = list.count_ones();
    if count == 0 {
        // Empty rlist. ARMv4T (ARM7) loads/stores PC and advances by 0x40.
        // ARMv5T (ARM9) transfers nothing but still bumps the base by 0x40.
        if !cpu.arch.is_v5() {
            if l {
                cpu.state.r[15] = cpu.read32(nds, cpu.state.r[rn] & !3);
                cpu.flush_pipeline();
            } else {
                let pc = cpu.state.r[15];
                cpu.write32(nds, cpu.state.r[rn] & !3, pc);
            }
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
    let mut addr = if u { base } else { base.wrapping_sub(count << 2) };
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

    // S bit + R15 in list ⇒ load CPSR from SPSR. S bit without R15 ⇒ user bank.
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
            let v = cpu.read32(nds, addr & !3);
            addr = addr.wrapping_add(4);
            if i == 15 {
                if s_bit {
                    let spsr = cpu.state.get_spsr();
                    cpu.state.switch_mode(spsr & 0x1F);
                    cpu.state.cpsr = spsr;
                }
                // ARMv5+ LDM-with-PC interworks like BX (bit 0 → THUMB);
                // ARMv4T just discards the low bits. Without the v5 path a
                // THUMB epilogue (POP {…, PC}) returns into ARM mode.
                if cpu.arch.is_v5() && !s_bit {
                    if v & 1 != 0 {
                        cpu.state.cpsr |= FLAG_T;
                        cpu.state.r[15] = v & !1;
                    } else {
                        cpu.state.cpsr &= !FLAG_T;
                        cpu.state.r[15] = v & !3;
                    }
                } else {
                    let thumb = (cpu.state.cpsr & FLAG_T) != 0;
                    cpu.state.r[15] = if thumb { v & !1 } else { v & !3 };
                }
                pc_loaded = true;
            } else {
                cpu.state.r[i as usize] = v;
            }
        }
        if pc_loaded {
            cpu.flush_pipeline();
        }
    } else {
        // STM base-in-list semantics differ between cores:
        //   ARM7 (v4T): if base is NOT the first stored, store post-writeback.
        //   ARM9 (v5T): always store the OLD base value, regardless of order.
        let mut first_stored = false;
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let mut v = cpu.state.r[i as usize];
            if i == 15 {
                v = v.wrapping_add(4);
            }
            if !cpu.arch.is_v5() && i as usize == rn && first_stored {
                v = writeback_addr;
            }
            cpu.write32(nds, addr & !3, v);
            addr = addr.wrapping_add(4);
            first_stored = true;
        }
    }

    if user_bank {
        cpu.state.switch_mode(saved_mode);
    }

    // LDM writeback when the base is in the rlist has observable v5 quirks:
    //   - count == 1 (only base): writeback wins.
    //   - count > 1, base is the highest-numbered register: load survives
    //     (writeback suppressed).
    //   - count > 1, base not highest: writeback wins.
    let mut suppress_writeback = false;
    if w && l && (list & (1 << rn)) != 0 && count > 1 {
        let highest = 15 - (list as u16).leading_zeros();
        if rn as u32 == highest {
            suppress_writeback = true;
        }
    }
    if w && !suppress_writeback {
        cpu.state.r[rn] = writeback_addr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nds::Core;
    use crate::state::{FLAG_C, FLAG_N, FLAG_V, FLAG_Z};

    // SYS/ARM, code in ARM9 main RAM. Returns (cpu, nds) ready to single-step.
    fn setup9(insns: &[u32]) -> (Cpu, Nds) {
        let mut cpu = Cpu::new(Core::Arm9);
        let mut nds = Nds::new();
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0000;
        cpu.state.r[13] = 0x0200_8000;
        cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            nds.write32_arm9(0x0200_0000 + (i as u32) * 4, insn);
        }
        (cpu, nds)
    }

    fn setup7(insns: &[u32]) -> (Cpu, Nds) {
        let mut cpu = Cpu::new(Core::Arm7);
        let mut nds = Nds::new();
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0000;
        cpu.state.r[13] = 0x0200_8000;
        cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            nds.write32_arm7(0x0200_0000 + (i as u32) * 4, insn);
        }
        (cpu, nds)
    }

    fn step(cpu: &mut Cpu, nds: &mut Nds) {
        cpu.step(nds);
    }

    // ── data processing ──

    #[test]
    fn mov_rotate_imm() {
        let (mut cpu, mut nds) = setup9(&[0xE3A0_04FF]); // MOV R0, #0xFF000000
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFF00_0000);
    }

    #[test]
    fn adds_overflow_nzcv() {
        let (mut cpu, mut nds) = setup9(&[0xE091_0002]); // ADDS R0, R1, R2
        cpu.state.r[1] = 0x8000_0000;
        cpu.state.r[2] = 0x8000_0000;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0);
        assert!(cpu.state.cpsr & FLAG_Z != 0);
        assert!(cpu.state.cpsr & FLAG_C != 0);
        assert!(cpu.state.cpsr & FLAG_V != 0);
        assert_eq!(cpu.state.cpsr & FLAG_N, 0);
    }

    #[test]
    fn cmp_borrow() {
        let (mut cpu, mut nds) = setup9(&[0xE150_0001]); // CMP R0, R1
        cpu.state.r[0] = 5;
        cpu.state.r[1] = 10;
        step(&mut cpu, &mut nds);
        assert!(cpu.state.cpsr & FLAG_N != 0);
        assert_eq!(cpu.state.cpsr & FLAG_C, 0);
    }

    // ── multiply ──

    #[test]
    fn mul() {
        let (mut cpu, mut nds) = setup9(&[0xE000_0291]); // MUL R0, R1, R2
        cpu.state.r[1] = 7;
        cpu.state.r[2] = 6;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 42);
    }

    #[test]
    fn smull_negative() {
        let (mut cpu, mut nds) = setup9(&[0xE0C1_0392]); // SMULL R0,R1,R2,R3
        cpu.state.r[2] = 0xFFFF_FFFE; // -2
        cpu.state.r[3] = 5;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFFFF_FFF6);
        assert_eq!(cpu.state.r[1], 0xFFFF_FFFF);
    }

    // ── LDR/STR ──

    #[test]
    fn ldr_pre_indexed_writeback() {
        let (mut cpu, mut nds) = setup9(&[0xE5B1_0008]); // LDR R0, [R1, #8]!
        nds.write32_arm9(0x0200_1008, 0xCAFE_BABE);
        cpu.state.r[1] = 0x0200_1000;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xCAFE_BABE);
        assert_eq!(cpu.state.r[1], 0x0200_1008);
    }

    #[test]
    fn ldrsh_sign_ext() {
        let (mut cpu, mut nds) = setup9(&[0xE1D1_00F0]); // LDRSH R0, [R1]
        nds.write16_arm9(0x0200_1000, 0x8000);
        cpu.state.r[1] = 0x0200_1000;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFFFF_8000);
    }

    // ── block transfer ──

    #[test]
    fn stmia_writeback() {
        let (mut cpu, mut nds) = setup9(&[0xE8A0_000E]); // STMIA R0!, {R1,R2,R3}
        cpu.state.r[0] = 0x0200_1000;
        cpu.state.r[1] = 0xAA;
        cpu.state.r[2] = 0xBB;
        cpu.state.r[3] = 0xCC;
        step(&mut cpu, &mut nds);
        assert_eq!(nds.read32_arm9(0x0200_1000), 0xAA);
        assert_eq!(nds.read32_arm9(0x0200_1008), 0xCC);
        assert_eq!(cpu.state.r[0], 0x0200_100C);
    }

    // ── SWP ──

    #[test]
    fn swp_word() {
        let (mut cpu, mut nds) = setup9(&[0xE102_0091]); // SWP R0, R1, [R2]
        cpu.state.r[1] = 0xAA;
        cpu.state.r[2] = 0x0200_1000;
        nds.write32_arm9(0x0200_1000, 0xBB);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xBB);
        assert_eq!(nds.read32_arm9(0x0200_1000), 0xAA);
    }

    // ── BX interworking ──

    #[test]
    fn bx_to_thumb() {
        let (mut cpu, mut nds) = setup9(&[0xE12F_FF10]); // BX R0
        cpu.state.r[0] = 0x0200_1001;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_1000);
        assert!(cpu.state.cpsr & FLAG_T != 0);
    }

    // ── ARMv5 deltas ──

    #[test]
    fn clz_arm9() {
        let (mut cpu, mut nds) = setup9(&[0xE16F_0F11]); // CLZ R0, R1
        cpu.state.r[1] = 0x0000_FFFF;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 16);
    }

    #[test]
    fn clz_zero_is_32() {
        let (mut cpu, mut nds) = setup9(&[0xE16F_0F11]);
        cpu.state.r[1] = 0;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 32);
    }

    #[test]
    fn clz_undefined_on_arm7() {
        // The CLZ encoding must NOT decode as CLZ on the ARM7. With bit 4 set
        // and bits 7:4 = 0001 it falls into the extension space; here we just
        // assert the ARM7 does not produce the CLZ result.
        let (mut cpu, mut nds) = setup7(&[0xE16F_0F11]);
        cpu.state.r[1] = 0x0000_FFFF;
        step(&mut cpu, &mut nds);
        assert_ne!(cpu.state.r[0], 16);
    }

    #[test]
    fn blx1_imm_arm9_switches_to_thumb() {
        // BLX #0: cond=0xF, target = PC(+8) + 0 + H<<1. With H=0, off=0 →
        // target = decode + 8. The H bit (bit 24) adds 2.
        // Encoding 0xFA000000 = cond 1111, 101 0, off 0.
        let (mut cpu, mut nds) = setup9(&[0xFA00_0000]);
        step(&mut cpu, &mut nds);
        assert!(cpu.state.cpsr & FLAG_T != 0); // switched to THUMB
        assert_eq!(cpu.state.r[14], 0x0200_0004); // LR = next ARM instr
    }

    #[test]
    fn blx1_undefined_on_arm7() {
        // cond=0xF space is wholly undefined (NOP-floored) on the ARM7 — must
        // not switch to THUMB.
        let (mut cpu, mut nds) = setup7(&[0xFA00_0000]);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.cpsr & FLAG_T, 0);
    }

    #[test]
    fn qadd_saturates_and_sets_q() {
        // QADD R0, R1, R2 → R0 = sat(Rm + Rn) = sat(R1 + R2).
        // Encoding: 0001 0000 Rn Rd 0000 0101 Rm = 0xE10_0_0_05_1.
        // op=00 (QADD): bits 27:24=0001,23..20=0000,7:4=0101.
        // QADD Rd, Rm, Rn: Rd=R0, Rm=R1, Rn=R2 → 0xE102_0051.
        let (mut cpu, mut nds) = setup9(&[0xE102_0051]);
        cpu.state.r[1] = 0x7FFF_FFFF; // Rm
        cpu.state.r[2] = 1; // Rn
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0x7FFF_FFFF);
        assert!(cpu.state.cpsr & FLAG_Q != 0);
    }

    #[test]
    fn ldrd_loads_pair() {
        // LDRD R0, [R2], with immediate offset 0 (post-index P=0,W=0).
        // Encoding bits: cond E, 000 PUIW0 Rn Rd HHHH 1101 LLLL.
        // op for LDRD = bits 6:5 = 10 → instr[6:5]=10 → 0b1101 in [7:4]=0xD.
        // Use pre-index P=1,U=1,I=1: 0xE1C2_00D0 → LDRD R0,[R2,#0].
        let (mut cpu, mut nds) = setup9(&[0xE1C2_00D0]);
        cpu.state.r[2] = 0x0200_1000;
        nds.write32_arm9(0x0200_1000, 0x1111_1111);
        nds.write32_arm9(0x0200_1004, 0x2222_2222);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0x1111_1111);
        assert_eq!(cpu.state.r[1], 0x2222_2222);
    }

    #[test]
    fn ldr_pc_interworks_on_arm9() {
        // LDR PC, [R1] with a value whose bit0=1 → THUMB on ARM9.
        let (mut cpu, mut nds) = setup9(&[0xE591_F000]); // LDR PC, [R1]
        cpu.state.r[1] = 0x0200_1000;
        nds.write32_arm9(0x0200_1000, 0x0200_2001); // bit0 set
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_2000);
        assert!(cpu.state.cpsr & FLAG_T != 0);
    }

    #[test]
    fn ldr_pc_no_interwork_on_arm7() {
        let (mut cpu, mut nds) = setup7(&[0xE591_F000]); // LDR PC, [R1]
        cpu.state.r[1] = 0x0200_1000;
        nds.write32_arm7(0x0200_1000, 0x0200_2001);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_2000);
        assert_eq!(cpu.state.cpsr & FLAG_T, 0); // stayed ARM
    }

    // ── Ported from ds-recomp/src/test/arm_dsp.test.ts ──
    // Q flag bit (CPSR[27]).
    const FLAG_Q: u32 = 0x0800_0000;
    // QADD Rd=0, Rm=1, Rn=2.
    const QADD: u32 = 0xE102_0051;
    const QSUB: u32 = 0xE122_0051;
    const QDADD: u32 = 0xE142_0051;

    #[test]
    fn qadd_straight_signed_sum() {
        let (mut cpu, mut nds) = setup9(&[QADD]);
        cpu.state.r[1] = 10;
        cpu.state.r[2] = 20;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 30);
    }

    #[test]
    fn qadd_saturates_negative_overflow() {
        let (mut cpu, mut nds) = setup9(&[QADD]);
        cpu.state.r[1] = 0x8000_0000;
        cpu.state.r[2] = 0xFFFF_FFFF; // -1
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0x8000_0000);
        assert!(cpu.state.cpsr & FLAG_Q != 0);
    }

    #[test]
    fn qsub_basic() {
        let (mut cpu, mut nds) = setup9(&[QSUB]);
        cpu.state.r[1] = 100;
        cpu.state.r[2] = 30;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 70);
    }

    #[test]
    fn qdadd_inner_saturation_sets_q() {
        // 0 + sat(2 * 2^30) → sat(2^31) = 2^31-1, Q set.
        let (mut cpu, mut nds) = setup9(&[QDADD]);
        cpu.state.r[1] = 0; // Rm
        cpu.state.r[2] = 0x4000_0000; // Rn
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0x7FFF_FFFF);
        assert!(cpu.state.cpsr & FLAG_Q != 0);
    }

    // SMULxy encoding: cond E, bits 27:20 = 0001 0110, Rd=Rn=0, Rs<<8 | 0x80 |
    // (y<<6) | (x<<5) | Rm.
    fn smulxy(rd: u32, rm: u32, rs: u32, x: u32, y: u32) -> u32 {
        (0xE << 28) | (0x16 << 20) | (rd << 16) | (rs << 8) | (0x80 | (y << 6) | (x << 5)) | rm
    }

    #[test]
    fn smulbb_5_times_3() {
        let (mut cpu, mut nds) = setup9(&[smulxy(0, 1, 2, 0, 0)]);
        cpu.state.r[1] = 5;
        cpu.state.r[2] = 3;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 15);
    }

    #[test]
    fn smulbb_negative_low_half() {
        let (mut cpu, mut nds) = setup9(&[smulxy(0, 1, 2, 0, 0)]);
        cpu.state.r[1] = 0x0000_FFFF; // low half = -1
        cpu.state.r[2] = 5;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0] as i32, -5);
    }

    #[test]
    fn smulbt_low_rm_high_rs() {
        let (mut cpu, mut nds) = setup9(&[smulxy(0, 1, 2, 0, 1)]);
        cpu.state.r[1] = 5;
        cpu.state.r[2] = 0x0003_0000; // high half = 3
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 15);
    }

    #[test]
    fn smultt_high_times_high() {
        let (mut cpu, mut nds) = setup9(&[smulxy(0, 1, 2, 1, 1)]);
        cpu.state.r[1] = 0x0007_0000;
        cpu.state.r[2] = 0x0004_0000;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 28);
    }

    // ── Banked-register / mode-switch spot-checks ──

    #[test]
    fn irq_banks_r13_r14_and_restores() {
        let (mut cpu, _nds) = setup9(&[]);
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[13] = 0xAAAA; // SYS sp
        cpu.state.bank_r13[2] = 0xBBBB; // IRQ sp bank
        cpu.state.r[15] = 0x0200_0100;
        cpu.take_irq();
        assert_eq!(cpu.state.mode(), mode::IRQ);
        assert_eq!(cpu.state.r[13], 0xBBBB); // IRQ-banked sp visible
        // SPSR holds the pre-exception CPSR (SYS).
        assert_eq!(cpu.state.get_spsr() & 0x1F, mode::SYS);
        // Return to SYS restores the SYS sp.
        cpu.state.switch_mode(mode::SYS);
        assert_eq!(cpu.state.r[13], 0xAAAA);
    }

    #[test]
    fn fiq_banks_r8_r12() {
        let (mut cpu, _nds) = setup9(&[]);
        cpu.state.cpsr = mode::SYS;
        for i in 8..=12 {
            cpu.state.r[i] = 0x1000 + i as u32;
        }
        cpu.state.switch_mode(mode::FIQ);
        // FIQ banks R8..R12 — they should now read the (zeroed) FIQ bank.
        for i in 8..=12 {
            assert_eq!(cpu.state.r[i], 0);
        }
        cpu.state.switch_mode(mode::SYS);
        // Back in SYS the user copies are restored.
        for i in 8..=12 {
            assert_eq!(cpu.state.r[i], 0x1000 + i as u32);
        }
    }

    // SUBS PC, LR, #4 with the S bit copies SPSR→CPSR (the IRQ return idiom).
    #[test]
    fn subs_pc_lr_restores_cpsr_from_spsr() {
        // First enter IRQ from a known PC, then execute SUBS PC, LR, #4.
        let (mut cpu, mut nds) = setup9(&[0xE25E_F004]); // at 0x02000000
        cpu.state.cpsr = mode::SYS;
        cpu.state.r[15] = 0x0200_0000;
        // Pretend an IRQ was taken returning to 0x02000010.
        cpu.state.bank_spsr[2] = mode::SYS; // IRQ bank SPSR = SYS
        cpu.state.switch_mode(mode::IRQ);
        cpu.state.r[14] = 0x0200_0014; // LR
        cpu.state.r[15] = 0x0200_0000; // re-point PC at our instruction
        // Reload the instruction at the IRQ-mode PC.
        nds.write32_arm9(0x0200_0000, 0xE25E_F004);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_0010); // LR-4
        assert_eq!(cpu.state.mode(), mode::SYS); // CPSR restored from SPSR
    }
}
