//! THUMB (16-bit) instruction interpreter for the DS, covering BOTH cores:
//! the ARM7 (ARMv4T) and the ARM9 (ARMv5TE). The base is adapted from the GBA
//! core's tested `thumb.rs` (the DS ARM7 *is* the same ARM7TDMI); the ARMv5T
//! deltas — gated on `cpu.is_arm9()` — are cross-checked against
//! ../../ds-recomp/src/cpu/thumb.ts:
//!
//!   * Format 5 op 3: BLX(2) — the high register `H1` bit turns the BX into a
//!     branch-with-link (`LR = (PC-2)|1`) before the BX-style interwork. On the
//!     ARM7 that bit is reserved and the op stays a plain BX.
//!   * Format 14 POP {…, PC}: on the ARM9 the loaded PC interworks (bit 0 → T),
//!     like a BX; on the ARM7 bit 0 is masked and the core stays in THUMB
//!     (the only THUMB→ARM transition there is BX).
//!   * Format 19 `H == 0b01`: BLX(1) immediate (THUMB→ARM long-branch-with-link)
//!     — ARM9 only; clears T and word-aligns the target.
//!
//! Memory goes through the per-core `Cpu::{read,write}{8,16,32}` shims so this
//! file never names a specific core's bus accessor.

use crate::cpu::exec::Cpu;
use crate::nds::Nds;
use crate::state::{CpuState, FLAG_T};

// ── Add/sub flag helpers — identical to the ARM set. ─────────────────────────
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
    let not_c = c_in ^ 1;
    let r = a.wrapping_sub(b).wrapping_sub(not_c);
    s.set_nz(r);
    s.set_c((a as u64) >= (b as u64 + not_c as u64));
    s.set_v(((a ^ b) & (a ^ r) & 0x8000_0000) != 0);
    r
}

pub fn thumb_execute(cpu: &mut Cpu, nds: &mut Nds, instr: u32) {
    let top = instr >> 13;

    match top {
        0b000 => {
            // Format 1 (LSL/LSR/ASR imm) or Format 2 (add/sub reg/imm3).
            let op = (instr >> 11) & 3;
            if op == 3 {
                // Format 2.
                let i = (instr & 0x0400) != 0;
                let sub = (instr & 0x0200) != 0;
                let rn_rm = ((instr >> 6) & 7) as usize;
                let rs = ((instr >> 3) & 7) as usize;
                let rd = (instr & 7) as usize;
                let b = if i { rn_rm as u32 } else { cpu.state.r[rn_rm] };
                let a = cpu.state.r[rs];
                cpu.state.r[rd] = if sub {
                    sub_set_flags(&mut cpu.state, a, b)
                } else {
                    add_set_flags(&mut cpu.state, a, b)
                };
                return;
            }
            // Format 1.
            let offset = (instr >> 6) & 0x1F;
            let rs = ((instr >> 3) & 7) as usize;
            let rd = (instr & 7) as usize;
            let r = crate::cpu::shifter::imm_shift(op, offset, cpu.state.r[rs], cpu.state.c());
            cpu.state.r[rd] = r.value;
            cpu.state.set_nz(r.value);
            crate::cpu::shifter::apply_carry(&mut cpu.state, r.carry);
        }
        0b001 => {
            // Format 3: mov/cmp/add/sub immediate.
            let op = (instr >> 11) & 3;
            let rd = ((instr >> 8) & 7) as usize;
            let imm = instr & 0xFF;
            match op {
                0 => {
                    cpu.state.r[rd] = imm;
                    cpu.state.set_nz(imm);
                }
                1 => {
                    let a = cpu.state.r[rd];
                    sub_set_flags(&mut cpu.state, a, imm);
                }
                2 => {
                    let a = cpu.state.r[rd];
                    cpu.state.r[rd] = add_set_flags(&mut cpu.state, a, imm);
                }
                3 => {
                    let a = cpu.state.r[rd];
                    cpu.state.r[rd] = sub_set_flags(&mut cpu.state, a, imm);
                }
                _ => {}
            }
        }
        0b010 => {
            let high4 = instr >> 12;
            if high4 == 0b0101 {
                // Format 7 (LDR/STR reg offset) or Format 8 (sign-extended).
                let bit9 = (instr >> 9) & 1;
                let ro = ((instr >> 6) & 7) as usize;
                let rb = ((instr >> 3) & 7) as usize;
                let rd = (instr & 7) as usize;
                let addr = cpu.state.r[rb].wrapping_add(cpu.state.r[ro]);
                if bit9 == 0 {
                    // Format 7: LDR/STR with byte/word toggle.
                    let l = (instr & 0x0800) != 0;
                    let b = (instr & 0x0400) != 0;
                    if l {
                        if b {
                            cpu.state.r[rd] = cpu.read8(nds, addr);
                        } else {
                            let aligned = cpu.read32(nds, addr & !3);
                            let rot = (addr & 3) << 3;
                            cpu.state.r[rd] = if rot != 0 {
                                (aligned >> rot) | (aligned << (32 - rot))
                            } else {
                                aligned
                            };
                        }
                    } else if b {
                        cpu.write8(nds, addr, cpu.state.r[rd] & 0xFF);
                    } else {
                        cpu.write32(nds, addr & !3, cpu.state.r[rd]);
                    }
                } else {
                    // Format 8: H + S bits.
                    let h = (instr & 0x0800) != 0;
                    let sgn = (instr & 0x0400) != 0;
                    if !h && !sgn {
                        // STRH
                        cpu.write16(nds, addr & !1, cpu.state.r[rd] & 0xFFFF);
                    } else if !h && sgn {
                        // LDRSB
                        let b = cpu.read8(nds, addr);
                        cpu.state.r[rd] = if b & 0x80 != 0 { b | 0xFFFF_FF00 } else { b };
                    } else if h && !sgn {
                        // LDRH (unaligned rotated)
                        let aligned = cpu.read16(nds, addr & !1);
                        cpu.state.r[rd] = if addr & 1 != 0 {
                            (aligned >> 8) | (aligned << 24)
                        } else {
                            aligned
                        };
                    } else {
                        // LDRSH — unaligned: read byte sign-extended.
                        if addr & 1 != 0 {
                            let b = cpu.read8(nds, addr);
                            cpu.state.r[rd] = if b & 0x80 != 0 { b | 0xFFFF_FF00 } else { b };
                        } else {
                            let h = cpu.read16(nds, addr);
                            cpu.state.r[rd] = if h & 0x8000 != 0 { h | 0xFFFF_0000 } else { h };
                        }
                    }
                }
                return;
            }
            if ((instr >> 10) & 7) == 0b000 {
                // Format 4: ALU ops.
                let op = (instr >> 6) & 0xF;
                let rs = ((instr >> 3) & 7) as usize;
                let rd = (instr & 7) as usize;
                let a = cpu.state.r[rd];
                let b = cpu.state.r[rs];
                let c_in = cpu.state.c();
                use crate::cpu::shifter::{apply_carry, reg_shift};
                match op {
                    0x0 => {
                        let v = a & b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // AND
                    0x1 => {
                        let v = a ^ b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // EOR
                    0x2 => {
                        let r = reg_shift(0, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    } // LSL
                    0x3 => {
                        let r = reg_shift(1, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    } // LSR
                    0x4 => {
                        let r = reg_shift(2, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    } // ASR
                    0x5 => {
                        cpu.state.r[rd] = adc_set_flags(&mut cpu.state, a, b, c_in);
                    } // ADC
                    0x6 => {
                        cpu.state.r[rd] = sbc_set_flags(&mut cpu.state, a, b, c_in);
                    } // SBC
                    0x7 => {
                        let r = reg_shift(3, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    } // ROR
                    0x8 => {
                        let v = a & b;
                        cpu.state.set_nz(v);
                    } // TST
                    0x9 => {
                        cpu.state.r[rd] = sub_set_flags(&mut cpu.state, 0, b);
                    } // NEG
                    0xA => {
                        sub_set_flags(&mut cpu.state, a, b);
                    } // CMP
                    0xB => {
                        add_set_flags(&mut cpu.state, a, b);
                    } // CMN
                    0xC => {
                        let v = a | b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // ORR
                    0xD => {
                        let v = a.wrapping_mul(b);
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // MUL
                    0xE => {
                        let v = a & !b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // BIC
                    0xF => {
                        let v = !b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    } // MVN
                    _ => {}
                }
                return;
            }
            if ((instr >> 10) & 7) == 0b001 {
                // Format 5: hi-register ops + BX/BLX.
                let op = (instr >> 8) & 3;
                let h1 = (instr & 0x80) != 0;
                let h2 = (instr & 0x40) != 0;
                let rs = (((instr >> 3) & 7) | if h2 { 8 } else { 0 }) as usize;
                let rd = ((instr & 7) | if h1 { 8 } else { 0 }) as usize;
                let mut a = cpu.state.r[rd];
                let mut b = cpu.state.r[rs];
                // PC in THUMB hi ops reads as aligned (low bit cleared).
                if rd == 15 {
                    a &= !1;
                }
                if rs == 15 {
                    b &= !1;
                }
                match op {
                    0 => {
                        // ADD (no flags)
                        let v = a.wrapping_add(b);
                        if rd == 15 {
                            cpu.state.r[15] = v & !1;
                            cpu.flush_pipeline();
                        } else {
                            cpu.state.r[rd] = v;
                        }
                    }
                    1 => {
                        // CMP (sets flags)
                        sub_set_flags(&mut cpu.state, a, b);
                    }
                    2 => {
                        // MOV (no flags)
                        if rd == 15 {
                            cpu.state.r[15] = b & !1;
                            cpu.flush_pipeline();
                        } else {
                            cpu.state.r[rd] = b;
                        }
                    }
                    3 => {
                        // BX (H1 == 0) or BLX(2) (H1 == 1, ARMv5/ARM9 only).
                        if cpu.is_arm9() && h1 {
                            cpu.state.r[14] = (cpu.state.r[15].wrapping_sub(2)) | 1;
                        }
                        if b & 1 != 0 {
                            cpu.state.cpsr |= FLAG_T;
                            cpu.state.r[15] = b & !1;
                        } else {
                            cpu.state.cpsr &= !FLAG_T;
                            cpu.state.r[15] = b & !3;
                        }
                        cpu.flush_pipeline();
                    }
                    _ => {}
                }
                return;
            }
            // Format 6: PC-relative load.
            let rd = ((instr >> 8) & 7) as usize;
            let imm = (instr & 0xFF) << 2;
            let addr = (cpu.state.r[15] & !3).wrapping_add(imm);
            cpu.state.r[rd] = cpu.read32(nds, addr);
        }
        0b011 => {
            // Format 9: load/store with immediate offset.
            let b = (instr & 0x1000) != 0;
            let l = (instr & 0x0800) != 0;
            let imm = (instr >> 6) & 0x1F;
            let rb = ((instr >> 3) & 7) as usize;
            let rd = (instr & 7) as usize;
            let addr = if b {
                cpu.state.r[rb].wrapping_add(imm)
            } else {
                cpu.state.r[rb].wrapping_add(imm << 2)
            };
            if l {
                if b {
                    cpu.state.r[rd] = cpu.read8(nds, addr);
                } else {
                    let aligned = cpu.read32(nds, addr & !3);
                    let rot = (addr & 3) << 3;
                    cpu.state.r[rd] = if rot != 0 {
                        (aligned >> rot) | (aligned << (32 - rot))
                    } else {
                        aligned
                    };
                }
            } else if b {
                cpu.write8(nds, addr, cpu.state.r[rd] & 0xFF);
            } else {
                cpu.write32(nds, addr & !3, cpu.state.r[rd]);
            }
        }
        0b100 => {
            if (instr & 0x1000) == 0 {
                // Format 10: load/store halfword.
                let l = (instr & 0x0800) != 0;
                let imm = ((instr >> 6) & 0x1F) << 1;
                let rb = ((instr >> 3) & 7) as usize;
                let rd = (instr & 7) as usize;
                let addr = cpu.state.r[rb].wrapping_add(imm);
                if l {
                    let aligned = cpu.read16(nds, addr & !1);
                    cpu.state.r[rd] = if addr & 1 != 0 {
                        (aligned >> 8) | (aligned << 24)
                    } else {
                        aligned
                    };
                } else {
                    cpu.write16(nds, addr & !1, cpu.state.r[rd] & 0xFFFF);
                }
                return;
            }
            // Format 11: SP-relative load/store.
            let l = (instr & 0x0800) != 0;
            let rd = ((instr >> 8) & 7) as usize;
            let imm = (instr & 0xFF) << 2;
            let addr = cpu.state.r[13].wrapping_add(imm);
            if l {
                let aligned = cpu.read32(nds, addr & !3);
                let rot = (addr & 3) << 3;
                cpu.state.r[rd] = if rot != 0 {
                    (aligned >> rot) | (aligned << (32 - rot))
                } else {
                    aligned
                };
            } else {
                cpu.write32(nds, addr & !3, cpu.state.r[rd]);
            }
        }
        0b101 => {
            if (instr & 0x1000) == 0 {
                // Format 12: load address (PC- or SP-relative).
                let sp = (instr & 0x0800) != 0;
                let rd = ((instr >> 8) & 7) as usize;
                let imm = (instr & 0xFF) << 2;
                if sp {
                    cpu.state.r[rd] = cpu.state.r[13].wrapping_add(imm);
                } else {
                    cpu.state.r[rd] = (cpu.state.r[15] & !3).wrapping_add(imm);
                }
                return;
            }
            if (instr & 0x0F00) == 0x0000 {
                // Format 13: add signed offset to SP.
                let imm = (instr & 0x7F) << 2;
                cpu.state.r[13] = if instr & 0x80 != 0 {
                    cpu.state.r[13].wrapping_sub(imm)
                } else {
                    cpu.state.r[13].wrapping_add(imm)
                };
                return;
            }
            if (instr & 0x0600) == 0x0400 {
                // Format 14: push/pop.
                let l = (instr & 0x0800) != 0;
                let r_bit = (instr & 0x0100) != 0;
                let list = instr & 0xFF;
                if l {
                    // POP { ..., PC? }
                    let mut sp = cpu.state.r[13];
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            cpu.state.r[i] = cpu.read32(nds, sp & !3);
                            sp = sp.wrapping_add(4);
                        }
                    }
                    if r_bit {
                        let v = cpu.read32(nds, sp & !3);
                        sp = sp.wrapping_add(4);
                        // ARMv5 (ARM9): POP {PC} interworks via bit 0 — like BX.
                        // ARMv4T (ARM7): bit 0 is masked and T is unchanged; the
                        // only THUMB→ARM transition there is BX.
                        if cpu.is_arm9() {
                            if v & 1 != 0 {
                                cpu.state.cpsr |= FLAG_T;
                                cpu.state.r[15] = v & !1;
                            } else {
                                cpu.state.cpsr &= !FLAG_T;
                                cpu.state.r[15] = v & !3;
                            }
                        } else {
                            cpu.state.r[15] = v & !1;
                        }
                        cpu.flush_pipeline();
                    }
                    cpu.state.r[13] = sp;
                } else {
                    // PUSH { ..., LR? } — store low register to low address.
                    let mut count = 0u32;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            count += 1;
                        }
                    }
                    if r_bit {
                        count += 1;
                    }
                    let start = cpu.state.r[13].wrapping_sub(count << 2);
                    let mut sp = start;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            cpu.write32(nds, sp & !3, cpu.state.r[i]);
                            sp = sp.wrapping_add(4);
                        }
                    }
                    if r_bit {
                        cpu.write32(nds, sp & !3, cpu.state.r[14]);
                    }
                    cpu.state.r[13] = start;
                }
                return;
            }
        }
        0b110 => {
            if (instr & 0x1000) == 0 {
                // Format 15: multiple load/store (LDMIA/STMIA).
                let l = (instr & 0x0800) != 0;
                let rb = ((instr >> 8) & 7) as usize;
                let list = instr & 0xFF;
                let mut addr = cpu.state.r[rb];
                if list == 0 {
                    // Empty list quirk: load/store PC, increment base by 0x40.
                    if l {
                        cpu.state.r[15] = cpu.read32(nds, addr & !3);
                        cpu.flush_pipeline();
                    } else {
                        cpu.write32(nds, addr & !3, cpu.state.r[15]);
                    }
                    cpu.state.r[rb] = addr.wrapping_add(0x40);
                    return;
                }
                let base_in_list = (list & (1 << rb)) != 0;
                let base_first = base_in_list && (list & ((1 << rb) - 1)) == 0;
                let start_addr = addr;
                for i in 0..8 {
                    if list & (1 << i) == 0 {
                        continue;
                    }
                    if l {
                        cpu.state.r[i] = cpu.read32(nds, addr & !3);
                    } else if i == rb && !base_first {
                        // STM with base in list (not first): store new base.
                        let count = (list as u32).count_ones();
                        cpu.write32(nds, addr & !3, start_addr.wrapping_add(count << 2));
                    } else {
                        cpu.write32(nds, addr & !3, cpu.state.r[i]);
                    }
                    addr = addr.wrapping_add(4);
                }
                if !l || !base_in_list {
                    cpu.state.r[rb] = addr;
                }
                return;
            }
            // Format 16/17: conditional branch / SWI.
            let cond = (instr >> 8) & 0xF;
            if cond == 0xF {
                // SWI
                cpu.software_interrupt(instr & 0xFF);
                return;
            }
            if cond == 0xE {
                return; // BKPT/undefined slot in THUMB cond space — NOP floor.
            }
            if !cpu.state.check_cond(cond) {
                return;
            }
            let mut off = (instr & 0xFF) << 1;
            if off & 0x100 != 0 {
                off |= 0xFFFF_FE00;
            }
            cpu.state.r[15] = cpu.state.r[15].wrapping_add(off);
            cpu.flush_pipeline();
        }
        0b111 => {
            if (instr & 0x1800) == 0x0000 {
                // Format 18: unconditional branch.
                let mut off = (instr & 0x07FF) << 1;
                if off & 0x0800 != 0 {
                    off |= 0xFFFF_F000;
                }
                cpu.state.r[15] = cpu.state.r[15].wrapping_add(off);
                cpu.flush_pipeline();
                return;
            }
            // Format 19: long branch with link (BL) / BLX immediate, two halves.
            let h = (instr >> 11) & 3;
            if h == 0b10 {
                // High half: LR = PC + signExt(offset << 12). Shared by BL/BLX.
                let mut off = (instr & 0x7FF) << 12;
                if off & 0x0040_0000 != 0 {
                    off |= 0xFF80_0000;
                }
                cpu.state.r[14] = cpu.state.r[15].wrapping_add(off);
                return;
            }
            if h == 0b11 {
                // BL low half: PC = LR + (offset << 1); LR = (oldPC-2) | 1.
                let new_pc = cpu.state.r[14].wrapping_add((instr & 0x7FF) << 1);
                let new_lr = cpu.state.r[15].wrapping_sub(2) | 1;
                cpu.state.r[15] = new_pc & !1;
                cpu.state.r[14] = new_lr;
                cpu.flush_pipeline();
                return;
            }
            if h == 0b01 && cpu.is_arm9() {
                // BLX(1) immediate low half (THUMB→ARM): like BL low half but
                // clears T and word-aligns the target. ARM9-only (ARMv5).
                let new_pc = cpu.state.r[14].wrapping_add((instr & 0x7FF) << 1) & !3;
                let new_lr = cpu.state.r[15].wrapping_sub(2) | 1;
                cpu.state.cpsr &= !FLAG_T;
                cpu.state.r[15] = new_pc;
                cpu.state.r[14] = new_lr;
                cpu.flush_pipeline();
            }
            // h == 0b01 on ARM7: undefined — NOP floor.
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nds::Core;
    use crate::state::{mode, FLAG_Z};

    // SYS + THUMB, code in ARM9 main RAM. Halfword stride.
    fn setup9(insns: &[u16]) -> (Cpu, Nds) {
        let mut cpu = Cpu::new(Core::Arm9);
        let mut nds = Nds::new();
        cpu.state.cpsr = mode::SYS | FLAG_T;
        cpu.state.r[15] = 0x0200_0000;
        cpu.state.r[13] = 0x0200_8000;
        cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            nds.write16_arm9(0x0200_0000 + (i as u32) * 2, insn as u32);
        }
        (cpu, nds)
    }

    fn setup7(insns: &[u16]) -> (Cpu, Nds) {
        let mut cpu = Cpu::new(Core::Arm7);
        let mut nds = Nds::new();
        cpu.state.cpsr = mode::SYS | FLAG_T;
        cpu.state.r[15] = 0x0200_0000;
        cpu.state.r[13] = 0x0200_8000;
        cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            nds.write16_arm7(0x0200_0000 + (i as u32) * 2, insn as u32);
        }
        (cpu, nds)
    }

    fn step(cpu: &mut Cpu, nds: &mut Nds) {
        cpu.step(nds);
    }

    #[test]
    fn lsl_imm() {
        let (mut cpu, mut nds) = setup9(&[0x0141]); // LSL R1, R0, #5
        cpu.state.r[0] = 3;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[1], 0x60);
    }

    #[test]
    fn add_reg() {
        let (mut cpu, mut nds) = setup9(&[0x1888]); // ADD R0, R1, R2
        cpu.state.r[1] = 5;
        cpu.state.r[2] = 7;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 12);
    }

    #[test]
    fn mov_imm() {
        let (mut cpu, mut nds) = setup9(&[0x20FF]); // MOV R0, #0xFF
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFF);
    }

    #[test]
    fn cmp_imm_equal_sets_z() {
        let (mut cpu, mut nds) = setup9(&[0x2805]); // CMP R0, #5
        cpu.state.r[0] = 5;
        step(&mut cpu, &mut nds);
        assert!(cpu.state.cpsr & FLAG_Z != 0);
    }

    #[test]
    fn mul_reg() {
        let (mut cpu, mut nds) = setup9(&[0x4348]); // MUL R0, R1
        cpu.state.r[0] = 7;
        cpu.state.r[1] = 6;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 42);
    }

    #[test]
    fn ldr_pc_relative() {
        let (mut cpu, mut nds) = setup9(&[0x4804]); // LDR R0, [PC, #16]
        nds.write32_arm9(0x0200_0014, 0xCAFE_BABE);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xCAFE_BABE);
    }

    #[test]
    fn ldrsh_reg_offset_sign_ext() {
        let (mut cpu, mut nds) = setup9(&[0x5E88]); // LDRSH R0, [R1, R2]
        nds.write16_arm9(0x0200_1000, 0xFFF0);
        cpu.state.r[1] = 0x0200_1000;
        cpu.state.r[2] = 0;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFFFF_FFF0);
    }

    #[test]
    fn ldr_imm_offset() {
        let (mut cpu, mut nds) = setup9(&[0x6848]); // LDR R0, [R1, #4]
        nds.write32_arm9(0x0200_1004, 0xDEAD_BEEF);
        cpu.state.r[1] = 0x0200_1000;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xDEAD_BEEF);
    }

    #[test]
    fn push_pop_pc() {
        // PUSH {R0,LR}=0xB501; POP {R0,PC}=0xBD01.
        let (mut cpu, mut nds) = setup9(&[0xB501, 0xBD01]);
        cpu.state.r[0] = 0x1234_5678;
        cpu.state.r[13] = 0x0200_2000;
        cpu.state.r[14] = 0x0200_0041; // bit0 set → stays THUMB on interwork
        step(&mut cpu, &mut nds); // PUSH
        assert_eq!(cpu.state.r[13], 0x0200_2000 - 8);
        step(&mut cpu, &mut nds); // POP
        assert_eq!(cpu.state.r[0], 0x1234_5678);
        assert_eq!(cpu.state.r[15], 0x0200_0040);
        assert_eq!(cpu.state.cpsr & FLAG_T, FLAG_T);
    }

    // ── ARMv5 THUMB deltas ──

    #[test]
    fn pop_pc_interworks_to_arm_on_arm9() {
        // POP {PC} with bit0 clear → switches to ARM on the ARM9.
        let (mut cpu, mut nds) = setup9(&[0xBD00]);
        cpu.state.r[13] = 0x0200_2000;
        nds.write32_arm9(0x0200_2000, 0x0200_3000); // bit0 clear
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_3000);
        assert_eq!(cpu.state.cpsr & FLAG_T, 0); // switched to ARM
    }

    #[test]
    fn pop_pc_stays_thumb_on_arm7() {
        // POP {PC} with bit0 clear → ARM7 masks bit0 and STAYS in THUMB.
        let (mut cpu, mut nds) = setup7(&[0xBD00]);
        cpu.state.r[13] = 0x0200_2000;
        nds.write32_arm7(0x0200_2000, 0x0200_3000);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_3000);
        assert_eq!(cpu.state.cpsr & FLAG_T, FLAG_T); // still THUMB
    }

    #[test]
    fn blx2_links_and_switches_to_arm_on_arm9() {
        // BLX R0 (Format 5, op 3, H1 set): 0x4780 | (rs<<3). BLX R1 = 0x4788.
        let (mut cpu, mut nds) = setup9(&[0x4788]);
        cpu.state.r[1] = 0x0200_3000; // bit0 clear → ARM target
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_3000);
        assert_eq!(cpu.state.cpsr & FLAG_T, 0); // switched to ARM
        assert_eq!(cpu.state.r[14], (0x0200_0002) | 1); // LR = (PC-2)|1
    }

    #[test]
    fn blx2_is_plain_bx_on_arm7() {
        // Same encoding on the ARM7: no link (LR untouched), BX semantics.
        let (mut cpu, mut nds) = setup7(&[0x4788]);
        cpu.state.r[1] = 0x0200_3000;
        cpu.state.r[14] = 0xDEAD_BEEF;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_3000);
        assert_eq!(cpu.state.r[14], 0xDEAD_BEEF); // LR untouched
    }

    #[test]
    fn blx_imm_thumb_to_arm_on_arm9() {
        // BL/BLX prefix (H=10) then BLX low half (H=01). Target switches to ARM
        // and word-aligns. High: 0xF000 (off=0). Low: 0xE800 | off.
        // Use off such that PC = LR + (off<<1), with LR = PC+0.
        // High half LR = decode_pc(=0x02000004) + 0 = 0x02000004.
        // Low half off=2 → PC = (0x02000004 + 4) & ~3 = 0x02000008.
        let (mut cpu, mut nds) = setup9(&[0xF000, 0xE802]);
        step(&mut cpu, &mut nds); // high half
        step(&mut cpu, &mut nds); // BLX low half
        assert_eq!(cpu.state.cpsr & FLAG_T, 0); // switched to ARM
        assert_eq!(cpu.state.r[15] & 3, 0); // word-aligned
        assert_eq!(cpu.state.r[14] & 1, 1); // LR bit0 set
    }

    #[test]
    fn bl_forward() {
        // BL +0x100: high 0xF000, low 0xF880.
        let (mut cpu, mut nds) = setup9(&[0xF000, 0xF880]);
        step(&mut cpu, &mut nds);
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15] & !1, 0x0200_0104);
        assert_eq!(cpu.state.r[14], 0x0200_0004 | 1);
        assert_eq!(cpu.state.cpsr & FLAG_T, FLAG_T); // BL stays THUMB
    }

    #[test]
    fn bx_to_arm() {
        let (mut cpu, mut nds) = setup9(&[0x4700]); // BX R0
        cpu.state.r[0] = 0x0200_3000; // bit0 clear
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15], 0x0200_3000);
        assert_eq!(cpu.state.cpsr & FLAG_T, 0);
    }

    #[test]
    fn beq_taken() {
        let (mut cpu, mut nds) = setup9(&[0xD002]); // BEQ +4
        cpu.state.cpsr |= FLAG_Z;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[15] & !1, 0x0200_0008);
    }

    #[test]
    fn stmia_writeback() {
        // STMIA R0!, {R1,R2} = 0xC006.
        let (mut cpu, mut nds) = setup9(&[0xC006]);
        cpu.state.r[0] = 0x0200_1000;
        cpu.state.r[1] = 0xAA;
        cpu.state.r[2] = 0xBB;
        step(&mut cpu, &mut nds);
        assert_eq!(nds.read32_arm9(0x0200_1000), 0xAA);
        assert_eq!(nds.read32_arm9(0x0200_1004), 0xBB);
        assert_eq!(cpu.state.r[0], 0x0200_1008);
    }

    #[test]
    fn add_sp_negative() {
        let (mut cpu, mut nds) = setup9(&[0xB084]); // ADD SP, #-16
        let sp = cpu.state.r[13];
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[13], sp.wrapping_sub(16));
    }

    #[test]
    fn neg_reg() {
        let (mut cpu, mut nds) = setup9(&[0x4248]); // NEG R0, R1
        cpu.state.r[1] = 5;
        step(&mut cpu, &mut nds);
        assert_eq!(cpu.state.r[0], 0xFFFF_FFFB);
    }

}
