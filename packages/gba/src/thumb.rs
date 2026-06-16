//! THUMB (16-bit) instruction interpreter. Ported 1:1 from src/cpu/thumb.ts.

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::shifter::{apply_carry, imm_shift, reg_shift};
use crate::state::{CpuState, FLAG_T};

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
    let not_c = c_in ^ 1;
    let r = a.wrapping_sub(b).wrapping_sub(not_c);
    s.set_nz(r);
    s.set_c((a as u64) >= (b as u64 + not_c as u64));
    s.set_v(((a ^ b) & (a ^ r) & 0x80000000) != 0);
    r
}

pub fn thumb_execute<B: Bus + ?Sized>(cpu: &mut Cpu, bus: &mut B, instr: u32) {
    let top = instr >> 13;

    match top {
        0b000 => {
            // Format 1 or 2.
            let op = (instr >> 11) & 3;
            if op == 3 {
                // Format 2: add/sub register or imm3.
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
            // Format 1: LSL/LSR/ASR immediate.
            let offset = (instr >> 6) & 0x1F;
            let rs = ((instr >> 3) & 7) as usize;
            let rd = (instr & 7) as usize;
            let r = imm_shift(op, offset, cpu.state.r[rs], cpu.state.c());
            cpu.state.r[rd] = r.value;
            cpu.state.set_nz(r.value);
            apply_carry(&mut cpu.state, r.carry);
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
                // Format 7 (LDR/STR reg offset) or Format 8 (load/store sign-extended).
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
                            cpu.state.r[rd] = bus.read8(addr);
                        } else {
                            let aligned = bus.read32(addr & !3);
                            let rot = (addr & 3) << 3;
                            cpu.state.r[rd] = if rot != 0 {
                                (aligned >> rot) | (aligned << (32 - rot))
                            } else {
                                aligned
                            };
                        }
                    } else if b {
                        bus.write8(addr, cpu.state.r[rd] & 0xFF);
                    } else {
                        bus.write32(addr & !3, cpu.state.r[rd]);
                    }
                } else {
                    // Format 8: H + S bits.
                    let h = (instr & 0x0800) != 0;
                    let sgn = (instr & 0x0400) != 0;
                    if !h && !sgn {
                        // STRH
                        bus.write16(addr & !1, cpu.state.r[rd] & 0xFFFF);
                    } else if !h && sgn {
                        // LDSB
                        let b = bus.read8(addr);
                        cpu.state.r[rd] = if b & 0x80 != 0 { b | 0xFFFFFF00 } else { b };
                    } else if h && !sgn {
                        // LDRH (unaligned rotated)
                        let aligned = bus.read16(addr & !1);
                        cpu.state.r[rd] = if addr & 1 != 0 {
                            (aligned >> 8) | (aligned << 24)
                        } else {
                            aligned
                        };
                    } else {
                        // LDSH — unaligned: read byte sign-extended.
                        if addr & 1 != 0 {
                            let b = bus.read8(addr);
                            cpu.state.r[rd] = if b & 0x80 != 0 { b | 0xFFFFFF00 } else { b };
                        } else {
                            let h = bus.read16(addr);
                            cpu.state.r[rd] = if h & 0x8000 != 0 { h | 0xFFFF0000 } else { h };
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
                match op {
                    0x0 => {
                        let v = a & b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    0x1 => {
                        let v = a ^ b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    0x2 => {
                        let r = reg_shift(0, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    }
                    0x3 => {
                        let r = reg_shift(1, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    }
                    0x4 => {
                        let r = reg_shift(2, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    }
                    0x5 => {
                        cpu.state.r[rd] = adc_set_flags(&mut cpu.state, a, b, c_in);
                    }
                    0x6 => {
                        cpu.state.r[rd] = sbc_set_flags(&mut cpu.state, a, b, c_in);
                    }
                    0x7 => {
                        let r = reg_shift(3, b & 0xFF, a, c_in);
                        cpu.state.r[rd] = r.value;
                        cpu.state.set_nz(r.value);
                        apply_carry(&mut cpu.state, r.carry);
                    }
                    0x8 => {
                        // TST
                        let v = a & b;
                        cpu.state.set_nz(v);
                    }
                    0x9 => {
                        // NEG
                        cpu.state.r[rd] = sub_set_flags(&mut cpu.state, 0, b);
                    }
                    0xA => {
                        // CMP
                        sub_set_flags(&mut cpu.state, a, b);
                    }
                    0xB => {
                        // CMN
                        add_set_flags(&mut cpu.state, a, b);
                    }
                    0xC => {
                        // ORR
                        let v = a | b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    0xD => {
                        // MUL
                        let v = a.wrapping_mul(b);
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    0xE => {
                        // BIC
                        let v = a & !b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    0xF => {
                        // MVN
                        let v = !b;
                        cpu.state.r[rd] = v;
                        cpu.state.set_nz(v);
                    }
                    _ => {}
                }
                return;
            }
            if ((instr >> 10) & 7) == 0b001 {
                // Format 5: hi reg ops / BX.
                let op = (instr >> 8) & 3;
                let h1 = (instr & 0x80) != 0;
                let h2 = (instr & 0x40) != 0;
                let rs = (((instr >> 3) & 7) | if h2 { 8 } else { 0 }) as usize;
                let rd = ((instr & 7) | if h1 { 8 } else { 0 }) as usize;
                let mut a = cpu.state.r[rd];
                let mut b = cpu.state.r[rs];
                // PC in THUMB hi ops reads as aligned +4.
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
                        // BX
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
            // 01001 Rd[10:8] imm8 — load word at ((PC & ~3) + (imm8<<2)).
            let rd = ((instr >> 8) & 7) as usize;
            let imm = (instr & 0xFF) << 2;
            let addr = (cpu.state.r[15] & !3).wrapping_add(imm);
            cpu.state.r[rd] = bus.read32(addr);
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
                    cpu.state.r[rd] = bus.read8(addr);
                } else {
                    let aligned = bus.read32(addr & !3);
                    let rot = (addr & 3) << 3;
                    cpu.state.r[rd] = if rot != 0 {
                        (aligned >> rot) | (aligned << (32 - rot))
                    } else {
                        aligned
                    };
                }
            } else if b {
                bus.write8(addr, cpu.state.r[rd] & 0xFF);
            } else {
                bus.write32(addr & !3, cpu.state.r[rd]);
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
                    let aligned = bus.read16(addr & !1);
                    cpu.state.r[rd] = if addr & 1 != 0 {
                        (aligned >> 8) | (aligned << 24)
                    } else {
                        aligned
                    };
                } else {
                    bus.write16(addr & !1, cpu.state.r[rd] & 0xFFFF);
                }
                return;
            }
            // Format 11: SP-relative load/store.
            let l = (instr & 0x0800) != 0;
            let rd = ((instr >> 8) & 7) as usize;
            let imm = (instr & 0xFF) << 2;
            let addr = cpu.state.r[13].wrapping_add(imm);
            if l {
                let aligned = bus.read32(addr & !3);
                let rot = (addr & 3) << 3;
                cpu.state.r[rd] = if rot != 0 {
                    (aligned >> rot) | (aligned << (32 - rot))
                } else {
                    aligned
                };
            } else {
                bus.write32(addr & !3, cpu.state.r[rd]);
            }
        }
        0b101 => {
            if (instr & 0x1000) == 0 {
                // Format 12: load address.
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
            // Format 13/14.
            if (instr & 0x0F00) == 0x0000 {
                // Format 13: add offset to SP.
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
                            cpu.state.r[i] = bus.read32(sp & !3);
                            sp = sp.wrapping_add(4);
                        }
                    }
                    if r_bit {
                        let v = bus.read32(sp & !3);
                        sp = sp.wrapping_add(4);
                        // ARMv4T (GBA): POP {PC} does NOT interwork. Bit 0 of the
                        // loaded value is masked out and the T flag stays THUMB —
                        // the only way to switch to ARM from THUMB is via BX.
                        // ARMv5T+ flips T from bit 0 here, but on the GBA's
                        // ARM7TDMI doing that lets compilers' THUMB-internal
                        // returns accidentally hop to ARM whenever a stored LR
                        // happens to have bit 0 clear, which is exactly Doom II's
                        // wild-ARM-jump symptom.
                        cpu.state.r[15] = v & !1;
                        cpu.flush_pipeline();
                    }
                    cpu.state.r[13] = sp;
                } else {
                    // PUSH { ..., LR? } — store low to high.
                    let mut count = 0;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            count += 1;
                        }
                    }
                    if r_bit {
                        count += 1;
                    }
                    let mut sp = cpu.state.r[13].wrapping_sub(count << 2);
                    let start = sp;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            bus.write32(sp & !3, cpu.state.r[i]);
                            sp = sp.wrapping_add(4);
                        }
                    }
                    if r_bit {
                        bus.write32(sp & !3, cpu.state.r[14]);
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
                    // Empty list quirk: load/store PC, increment by 0x40.
                    if l {
                        cpu.state.r[15] = bus.read32(addr & !3);
                        cpu.flush_pipeline();
                    } else {
                        bus.write32(addr & !3, cpu.state.r[15]);
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
                        cpu.state.r[i] = bus.read32(addr & !3);
                    } else if i == rb && !base_first {
                        // Writeback of new base if not first.
                        let mut count = 0;
                        for j in 0..8 {
                            if list & (1 << j) != 0 {
                                count += 1;
                            }
                        }
                        bus.write32(addr & !3, start_addr.wrapping_add(count << 2));
                    } else {
                        bus.write32(addr & !3, cpu.state.r[i]);
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
                cpu.software_interrupt(instr & 0xFF, bus);
                return;
            }
            if cond == 0xE {
                // Undefined: the real ARM7TDMI takes the undefined-instruction
                // exception (vector 0x04). Bump the counter so the frame loop
                // can detect a fault loop.
                cpu.undefined_instruction("UNDEF INSTR");
                return;
            }
            if !cpu.state.check_cond(cond) {
                return;
            }
            let mut off = (instr & 0xFF) << 1;
            if off & 0x100 != 0 {
                off |= 0xFFFFFE00;
            }
            cpu.state.r[15] = cpu.state.r[15].wrapping_add(off);
            cpu.flush_pipeline();
        }
        0b111 => {
            if (instr & 0x1800) == 0x0000 {
                // Format 18: unconditional branch.
                let mut off = (instr & 0x07FF) << 1;
                if off & 0x0800 != 0 {
                    off |= 0xFFFFF000;
                }
                cpu.state.r[15] = cpu.state.r[15].wrapping_add(off);
                cpu.flush_pipeline();
                return;
            }
            // Format 19: long branch with link, two halfwords.
            let h = (instr >> 11) & 3;
            if h == 0b10 {
                // High half: LR = PC + (offset << 12).
                let mut off = (instr & 0x7FF) << 12;
                if off & 0x00400000 != 0 {
                    off |= 0xFF800000;
                }
                cpu.state.r[14] = cpu.state.r[15].wrapping_add(off);
                return;
            }
            if h == 0b11 {
                // Low half: PC = LR + (offset << 1); LR = (oldPC+2) | 1.
                let new_pc = cpu.state.r[14].wrapping_add((instr & 0x7FF) << 1);
                let new_lr = cpu.state.r[15].wrapping_sub(2) | 1;
                let tg = new_pc & !1;
                cpu.state.r[15] = tg;
                cpu.state.r[14] = new_lr;
                cpu.flush_pipeline();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    //! THUMB instruction-level vectors ported from the (deleted) TypeScript
    //! `src/test/thumb.test.ts` + the THUMB-flavored cases of `cpu.test.ts`.
    //! Harness mirrors the TS `makeCpu()` (SYS + T, code+SP in IWRAM) and
    //! `load(cpu, bus, insns)` (halfwords).

    use crate::bus::Bus;
    use crate::state::{FLAG_C, FLAG_N, FLAG_T, FLAG_V, FLAG_Z};
    use crate::Gba;

    fn step(g: &mut Gba) {
        let mut cpu = std::mem::take(&mut g.cpu);
        cpu.step(g);
        g.cpu = cpu;
    }

    // THUMB variant of setup: set FLAG_T, write16 + 2-byte stride.
    fn setup(insns: &[u16]) -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        g.cpu.state.cpsr = 0x1F | FLAG_T; // SYS + T
        g.cpu.state.r[15] = 0x0300_0000;
        g.cpu.state.r[13] = 0x0300_7F00;
        g.cpu.branched = false;
        for (i, &insn) in insns.iter().enumerate() {
            Bus::write16(&mut g, 0x0300_0000 + (i as u32) * 2, insn as u32);
        }
        g
    }

    // ---- Format 1: LSL/LSR/ASR imm ----

    #[test]
    fn lsl_imm() {
        let mut g = setup(&[0x0141]); // LSL R1, R0, #5
        g.cpu.state.r[0] = 0x00000003;
        step(&mut g);
        assert_eq!(g.cpu.state.r[1], 0x00000060);
    }

    #[test]
    fn lsr_imm_carry() {
        let mut g = setup(&[0x110A]); // LSR R2, R1, #4
        g.cpu.state.r[1] = 0xFF;
        step(&mut g);
        assert_eq!(g.cpu.state.r[2], 0x0F);
        assert!(g.cpu.state.cpsr & FLAG_C != 0);
    }

    #[test]
    fn asr_imm_negative() {
        let mut g = setup(&[0x1053]); // ASR R3, R2, #1
        g.cpu.state.r[2] = 0x80000000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[3], 0xC0000000);
    }

    // ---- Format 2: ADD/SUB reg/imm3 ----

    #[test]
    fn add_reg() {
        let mut g = setup(&[0x1888]); // ADD R0, R1, R2
        g.cpu.state.r[1] = 5;
        g.cpu.state.r[2] = 7;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 12);
    }

    #[test]
    fn sub_imm3() {
        let mut g = setup(&[0x1EC8]); // SUB R0, R1, #3
        g.cpu.state.r[1] = 10;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 7);
    }

    // ---- Format 3: MOV/CMP/ADD/SUB imm ----

    #[test]
    fn mov_imm() {
        let mut g = setup(&[0x20FF]); // MOV R0, #0xFF
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFF);
    }

    #[test]
    fn cmp_imm_equal_sets_z() {
        let mut g = setup(&[0x2805]); // CMP R0, #5
        g.cpu.state.r[0] = 5;
        step(&mut g);
        assert!(g.cpu.state.cpsr & FLAG_Z != 0);
    }

    #[test]
    fn mov_imm_5() {
        // cpu.test.ts
        let mut g = setup(&[0x2005]);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 5);
    }

    // ---- Format 4: ALU register operations ----

    #[test]
    fn and_reg() {
        let mut g = setup(&[0x4008]); // AND R0, R1
        g.cpu.state.r[0] = 0xFF;
        g.cpu.state.r[1] = 0x0F;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x0F);
    }

    #[test]
    fn eor_reg() {
        let mut g = setup(&[0x4048]); // EOR R0, R1
        g.cpu.state.r[0] = 0xFF;
        g.cpu.state.r[1] = 0x0F;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xF0);
    }

    #[test]
    fn neg_reg() {
        let mut g = setup(&[0x4248]); // NEG R0, R1
        g.cpu.state.r[1] = 5;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFFB);
    }

    #[test]
    fn mul_reg() {
        let mut g = setup(&[0x4348]); // MUL R0, R1
        g.cpu.state.r[0] = 7;
        g.cpu.state.r[1] = 6;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 42);
    }

    #[test]
    fn ror_reg() {
        let mut g = setup(&[0x41C8]); // ROR R0, R1
        g.cpu.state.r[0] = 0x12345678;
        g.cpu.state.r[1] = 8;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x78123456);
    }

    #[test]
    fn neg_flags() {
        // cpu.test.ts: NEG R0,R0 (0x4240): 0-5 = -5, Nzcv
        let mut g = setup(&[0x4240]);
        g.cpu.state.r[0] = 5;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFFB);
        assert!(g.cpu.state.cpsr & FLAG_N != 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_Z, 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_C, 0);
        assert_eq!(g.cpu.state.cpsr & FLAG_V, 0);
    }

    #[test]
    fn lsl_r0_r0_1() {
        // cpu.test.ts: LSL R0, R0, #1
        let mut g = setup(&[0x0040]);
        g.cpu.state.r[0] = 0x55;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xAA);
    }

    // ---- Format 5: Hi-register ops + BX ----

    #[test]
    fn add_hi_reg() {
        let mut g = setup(&[0x4480]); // ADD R8, R0
        g.cpu.state.r[8] = 0x100;
        g.cpu.state.r[0] = 0x10;
        step(&mut g);
        assert_eq!(g.cpu.state.r[8], 0x110);
    }

    #[test]
    fn mov_hi_reg() {
        let mut g = setup(&[0x4680]); // MOV R8, R0
        g.cpu.state.r[0] = 0xDEADBEEF;
        step(&mut g);
        assert_eq!(g.cpu.state.r[8], 0xDEADBEEF);
    }

    #[test]
    fn add_hi_reg_r0_r9() {
        // cpu.test.ts: Format 5 hi-reg ADD R0, R9 (0x4448)
        let mut g = setup(&[0x4448]);
        g.cpu.state.r[0] = 10;
        g.cpu.state.r[9] = 5;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 15);
    }

    #[test]
    fn bx_stays_thumb() {
        let mut g = setup(&[0x4700]); // BX R0
        g.cpu.state.r[0] = 0x03000011; // bit 0 set
        step(&mut g);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000010);
        assert!(g.cpu.state.cpsr & FLAG_T != 0);
    }

    #[test]
    fn bx_switches_to_arm() {
        let mut g = setup(&[0x4700]); // BX R0
        g.cpu.state.r[0] = 0x03000020;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03000020);
        assert_eq!(g.cpu.state.cpsr & FLAG_T, 0);
    }

    #[test]
    fn bx_to_arm_clears_t() {
        // cpu.test.ts: THUMB BX R0 with R0 = 0x03000010
        let mut g = setup(&[0x4700]);
        g.cpu.state.r[0] = 0x03000010;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03000010);
        assert_eq!(g.cpu.state.cpsr & FLAG_T, 0);
    }

    // ---- Format 6: PC-relative load ----

    #[test]
    fn ldr_pc_relative() {
        let mut g = setup(&[0x4804]); // LDR R0, [PC, #16]
        Bus::write32(&mut g, 0x03000014, 0xCAFEBABE);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xCAFEBABE);
    }

    // ---- Format 7: load/store register offset ----

    #[test]
    fn str_ldr_reg_offset() {
        let mut g = setup(&[0x5088, 0x588B]); // STR R0,[R1,R2]; LDR R3,[R1,R2]
        g.cpu.state.r[0] = 0xABCDEF01;
        g.cpu.state.r[1] = 0x03001000;
        g.cpu.state.r[2] = 8;
        step(&mut g);
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, 0x03001008), 0xABCDEF01);
        assert_eq!(g.cpu.state.r[3], 0xABCDEF01);
    }

    #[test]
    fn strb_ldrb_reg_offset() {
        let mut g = setup(&[0x5488, 0x5C8B]);
        g.cpu.state.r[0] = 0xAB;
        g.cpu.state.r[1] = 0x03001000;
        g.cpu.state.r[2] = 4;
        step(&mut g);
        step(&mut g);
        assert_eq!(Bus::read8(&mut g, 0x03001004), 0xAB);
        assert_eq!(g.cpu.state.r[3], 0xAB);
    }

    #[test]
    fn str_reg_offset_cpu_test() {
        // cpu.test.ts: 0x5088 = STR R0,[R1,R2]
        let mut g = setup(&[0x5088]);
        g.cpu.state.r[0] = 0x12345678;
        g.cpu.state.r[1] = 0x03000100;
        g.cpu.state.r[2] = 0;
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, 0x03000100), 0x12345678);
    }

    #[test]
    fn ldrb_reg_offset_cpu_test() {
        // cpu.test.ts: 0x5C88 = LDRB R0,[R1,R2]
        let mut g = setup(&[0x5C88]);
        g.cpu.state.r[1] = 0x03000100;
        g.cpu.state.r[2] = 1;
        Bus::write8(&mut g, 0x03000101, 0xCD);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xCD);
    }

    // ---- Format 8: signed load/store halfword ----

    #[test]
    fn ldrsh_reg_offset_sign_ext() {
        let mut g = setup(&[0x5E88]); // LDRSH R0, [R1, R2]
        Bus::write16(&mut g, 0x03001000, 0xFFF0);
        g.cpu.state.r[1] = 0x03001000;
        g.cpu.state.r[2] = 0;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFF0);
    }

    #[test]
    fn ldrsb_reg_offset_sign_ext() {
        let mut g = setup(&[0x5688]); // LDRSB R0, [R1, R2]
        Bus::write8(&mut g, 0x03001000, 0x80);
        g.cpu.state.r[1] = 0x03001000;
        g.cpu.state.r[2] = 0;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFF80);
    }

    #[test]
    fn strh_reg_offset() {
        let mut g = setup(&[0x5288]); // STRH R0, [R1, R2]
        g.cpu.state.r[0] = 0x12345678;
        g.cpu.state.r[1] = 0x03001000;
        g.cpu.state.r[2] = 0;
        step(&mut g);
        assert_eq!(Bus::read16(&mut g, 0x03001000), 0x5678);
    }

    #[test]
    fn ldrh_reg_offset_cpu_test() {
        // cpu.test.ts: 0x5AC8 = LDRH R0,[R1,R3]
        let mut g = setup(&[0x5AC8]);
        g.cpu.state.r[1] = 0x03000100;
        g.cpu.state.r[3] = 2;
        Bus::write16(&mut g, 0x03000102, 0x1234);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x1234);
    }

    #[test]
    fn ldrsh_reg_offset_cpu_test() {
        // cpu.test.ts: 0x5EC8 = LDRSH R0,[R1,R3]
        let mut g = setup(&[0x5EC8]);
        g.cpu.state.r[1] = 0x03000100;
        g.cpu.state.r[3] = 2;
        Bus::write16(&mut g, 0x03000102, 0xFFAA);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xFFFFFFAA);
    }

    // ---- Format 9/10: imm-offset load/store ----

    #[test]
    fn ldr_imm_offset() {
        let mut g = setup(&[0x6848]); // LDR R0, [R1, #4]
        Bus::write32(&mut g, 0x03001004, 0xDEADBEEF);
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0xDEADBEEF);
    }

    #[test]
    fn strh_imm_offset() {
        let mut g = setup(&[0x80C8]); // STRH R0, [R1, #6]
        g.cpu.state.r[0] = 0xCAFE;
        g.cpu.state.r[1] = 0x03001000;
        step(&mut g);
        assert_eq!(Bus::read16(&mut g, 0x03001006), 0xCAFE);
    }

    // ---- Format 11: SP-relative load/store ----

    #[test]
    fn ldr_sp_relative() {
        let mut g = setup(&[0x9802]); // LDR R0, [SP, #8]
        let sp = g.cpu.state.r[13];
        Bus::write32(&mut g, sp + 8, 0x11223344);
        step(&mut g);
        assert_eq!(g.cpu.state.r[0], 0x11223344);
    }

    #[test]
    fn str_sp_relative() {
        let mut g = setup(&[0x9001]); // STR R0, [SP, #4]
        g.cpu.state.r[0] = 0x55667788;
        let sp = g.cpu.state.r[13];
        step(&mut g);
        assert_eq!(Bus::read32(&mut g, sp + 4), 0x55667788);
    }

    // ---- Format 13: ADD SP, #imm (signed) ----

    #[test]
    fn add_sp_positive() {
        let mut g = setup(&[0xB004]); // ADD SP, #16
        let sp = g.cpu.state.r[13];
        step(&mut g);
        assert_eq!(g.cpu.state.r[13], sp + 16);
    }

    #[test]
    fn add_sp_negative() {
        let mut g = setup(&[0xB084]); // ADD SP, #-16
        let sp = g.cpu.state.r[13];
        step(&mut g);
        assert_eq!(g.cpu.state.r[13], sp.wrapping_sub(16));
    }

    // ---- Format 14: PUSH/POP ----

    #[test]
    fn push_pop() {
        // PUSH {R0,R1,LR}=0xB503; POP {R2,R3,PC}=0xBD0C
        let mut g = setup(&[0xB503, 0xBD0C]);
        g.cpu.state.r[0] = 0xAAAA;
        g.cpu.state.r[1] = 0xBBBB;
        g.cpu.state.r[14] = 0x03000040 | 1;
        step(&mut g); // PUSH
        step(&mut g); // POP
        assert_eq!(g.cpu.state.r[2], 0xAAAA);
        assert_eq!(g.cpu.state.r[3], 0xBBBB);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000040);
    }

    #[test]
    fn push_pop_pc_cpu_test() {
        // cpu.test.ts: PUSH {R0,LR}=0xB501; POP {R0,PC}=0xBD01
        let mut g = setup(&[0xB501, 0xBD01]);
        g.cpu.state.r[0] = 0x12345678;
        g.cpu.state.r[13] = 0x03000200;
        g.cpu.state.r[14] = 0x03000041;
        step(&mut g); // PUSH
        assert_eq!(g.cpu.state.r[13], 0x03000200 - 8);
        step(&mut g); // POP
        assert_eq!(g.cpu.state.r[0], 0x12345678);
        assert_eq!(g.cpu.state.r[15], 0x03000040);
        assert_eq!(g.cpu.state.cpsr & FLAG_T, FLAG_T);
    }

    #[test]
    fn pop_pc_stays_thumb() {
        // cpu.test.ts: POP {PC} = 0xBD00 with bit0=1
        let mut g = setup(&[0xBD00]);
        g.cpu.state.r[13] = 0x03000200;
        Bus::write32(&mut g, 0x03000200, 0x03000011);
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03000010);
        assert_eq!(g.cpu.state.cpsr & FLAG_T, FLAG_T);
    }

    // ---- Format 16: conditional branch ----

    #[test]
    fn beq_taken() {
        let mut g = setup(&[0xD002]); // BEQ +4
        g.cpu.state.cpsr |= FLAG_Z;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000008);
    }

    #[test]
    fn bne_not_taken() {
        let mut g = setup(&[0xD102]); // BNE +4 (Z set -> not taken)
        g.cpu.state.cpsr |= FLAG_Z;
        step(&mut g);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000002);
    }

    // ---- Format 19: BL (long branch with link) ----

    #[test]
    fn bl_forward() {
        let mut g = setup(&[0xF000, 0xF880]); // BL +0x100
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000104);
        assert_eq!(g.cpu.state.r[14], 0x03000004 | 1);
    }

    #[test]
    fn bl_backward() {
        let mut g = setup(&[]);
        g.cpu.state.r[15] = 0x03000100;
        Bus::write16(&mut g, 0x03000100, 0xF7FF);
        Bus::write16(&mut g, 0x03000102, 0xFFFE);
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[15] & !1, 0x03000100);
    }

    #[test]
    fn bl_forward_cpu_test() {
        // cpu.test.ts: BL F000 F802 -> target 0x03000008, LR bit0=1
        let mut g = setup(&[0xF000, 0xF802, 0x0000, 0x0000, 0x2042, 0x4770]);
        step(&mut g);
        step(&mut g);
        assert_eq!(g.cpu.state.r[15], 0x03000008);
        assert_eq!(g.cpu.state.r[14] & 1, 1);
    }
}
