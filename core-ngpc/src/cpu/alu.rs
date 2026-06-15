//! TLCS-900/H ALU primitives + the second-opcode-byte operation handlers and
//! the condition-code table. Flag effects follow the Toshiba manual:
//!   ADD/ADC : S Z H V↕ N=0 C
//!   SUB/SBC/CP : S Z H V↕ N=1 C
//!   AND/OR/XOR : S Z H(AND=1 else 0) P N=0 C=0
//!   INC/DEC : S Z H V↕ N(0/1) — C UNCHANGED
//!   shifts/rotates : S Z H=0 P/V N=0 C=shifted bit

use crate::cpu::bus::Bus;
use crate::cpu::exec::Ea;
use crate::cpu::state::*;

impl Cpu {
    // ---------------------------------------------------------- flag helpers
    fn set_szp(&mut self, v: u32, size: Size) {
        self.set_flag(FLAG_S, v & size.sign_mask() != 0);
        self.set_flag(FLAG_Z, v & size.mask() == 0);
        let parity = (v & 0xFF).count_ones() & 1 == 0;
        self.set_flag(FLAG_V, parity);
    }
    fn set_sz(&mut self, v: u32, size: Size) {
        self.set_flag(FLAG_S, v & size.sign_mask() != 0);
        self.set_flag(FLAG_Z, v & size.mask() == 0);
    }

    pub(crate) fn alu_add(&mut self, a: u32, b: u32, carry: u32, size: Size) -> u32 {
        let m = size.mask();
        let res = (a & m).wrapping_add(b & m).wrapping_add(carry);
        let r = res & m;
        self.set_sz(r, size);
        self.set_flag(FLAG_H, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(FLAG_C, (res & !m) != 0);
        let sign = size.sign_mask();
        let ov = ((a ^ res) & (b ^ res) & sign) != 0;
        self.set_flag(FLAG_V, ov);
        self.set_flag(FLAG_N, false);
        r
    }

    pub(crate) fn alu_sub(&mut self, a: u32, b: u32, carry: u32, size: Size) -> u32 {
        let m = size.mask();
        let res = (a & m).wrapping_sub(b & m).wrapping_sub(carry);
        let r = res & m;
        self.set_sz(r, size);
        self.set_flag(FLAG_H, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(FLAG_C, (res & !m) != 0);
        let sign = size.sign_mask();
        let ov = ((a ^ b) & (a ^ res) & sign) != 0;
        self.set_flag(FLAG_V, ov);
        self.set_flag(FLAG_N, true);
        r
    }

    pub(crate) fn alu_and(&mut self, a: u32, b: u32, size: Size) -> u32 {
        let r = (a & b) & size.mask();
        self.set_szp(r, size);
        self.set_flag(FLAG_H, true);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, false);
        r
    }
    pub(crate) fn alu_or(&mut self, a: u32, b: u32, size: Size) -> u32 {
        let r = (a | b) & size.mask();
        self.set_szp(r, size);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, false);
        r
    }
    pub(crate) fn alu_xor(&mut self, a: u32, b: u32, size: Size) -> u32 {
        let r = (a ^ b) & size.mask();
        self.set_szp(r, size);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, false);
        r
    }

    /// INC #n — affects S Z H V N but NOT C.
    pub(crate) fn alu_inc(&mut self, a: u32, n: u32, size: Size) -> u32 {
        let m = size.mask();
        let res = (a & m).wrapping_add(n);
        let r = res & m;
        self.set_sz(r, size);
        self.set_flag(FLAG_H, ((a ^ n ^ res) & 0x10) != 0);
        let sign = size.sign_mask();
        self.set_flag(FLAG_V, ((a ^ res) & (n ^ res) & sign) != 0);
        self.set_flag(FLAG_N, false);
        r
    }
    /// DEC #n — affects S Z H V N but NOT C.
    pub(crate) fn alu_dec(&mut self, a: u32, n: u32, size: Size) -> u32 {
        let m = size.mask();
        let res = (a & m).wrapping_sub(n);
        let r = res & m;
        self.set_sz(r, size);
        self.set_flag(FLAG_H, ((a ^ n ^ res) & 0x10) != 0);
        let sign = size.sign_mask();
        self.set_flag(FLAG_V, ((a ^ n) & (a ^ res) & sign) != 0);
        self.set_flag(FLAG_N, true);
        r
    }

    // ---------------------------------------------------------- conditions
    /// Evaluate a 4-bit condition code (verified against the Mednafen table).
    pub(crate) fn cond(&self, cc: u8) -> bool {
        let s = self.flag(FLAG_S);
        let z = self.flag(FLAG_Z);
        let v = self.flag(FLAG_V);
        let c = self.flag(FLAG_C);
        match cc & 0x0F {
            0 => false,            // F
            1 => s ^ v,            // LT
            2 => z || (s ^ v),     // LE
            3 => c || z,           // ULE
            4 => v,                // OV / PE
            5 => s,                // MI
            6 => z,                // Z / EQ
            7 => c,                // C / ULT
            8 => true,             // T
            9 => !(s ^ v),         // GE
            10 => !(z || (s ^ v)), // GT
            11 => !(c || z),       // UGT
            12 => !v,              // NOV / PO
            13 => !s,              // PL
            14 => !z,              // NZ / NE
            _ => !c,               // NC / UGE
        }
    }

    // ---------------------------------------------------------- shifts
    /// One rotate/shift of `val` by `count`, op selected by `kind` (0..7):
    /// 0 RLC 1 RRC 2 RL 3 RR 4 SLA 5 SRA 6 SLL 7 SRL.
    pub(crate) fn do_shift(&mut self, kind: u8, mut val: u32, count: u32, size: Size) -> u32 {
        let bits = size.bytes() * 8;
        let sign = size.sign_mask();
        let m = size.mask();
        let mut carry = self.flag(FLAG_C);
        for _ in 0..count.max(1) {
            match kind & 0x07 {
                0 => {
                    // RLC
                    let b = (val & sign) != 0;
                    val = ((val << 1) | (b as u32)) & m;
                    carry = b;
                }
                1 => {
                    // RRC
                    let b = (val & 1) != 0;
                    val = ((val >> 1) | ((b as u32) << (bits - 1))) & m;
                    carry = b;
                }
                2 => {
                    // RL (through carry)
                    let b = (val & sign) != 0;
                    val = ((val << 1) | (carry as u32)) & m;
                    carry = b;
                }
                3 => {
                    // RR (through carry)
                    let b = (val & 1) != 0;
                    val = ((val >> 1) | ((carry as u32) << (bits - 1))) & m;
                    carry = b;
                }
                4 | 6 => {
                    // SLA / SLL
                    let b = (val & sign) != 0;
                    val = (val << 1) & m;
                    carry = b;
                }
                5 => {
                    // SRA (arithmetic)
                    let b = (val & 1) != 0;
                    let topset = val & sign;
                    val = ((val >> 1) | topset) & m;
                    carry = b;
                }
                _ => {
                    // SRL
                    let b = (val & 1) != 0;
                    val = (val >> 1) & m;
                    carry = b;
                }
            }
        }
        self.set_szp(val, size);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, carry);
        val
    }

    // ====================================================================
    // Second-opcode-byte handlers.
    //
    // For the SOURCE groups (0x80-0xAF, 0xC0/0xD0/0xE0): the operand is a value
    // read from memory/explicit-EA, and the second byte selects an operation
    // that combines it with a register (named by the second byte's low bits) or
    // performs LD reg<-operand. For the REGISTER groups (0xC8/0xD8/0xE8): the
    // operand IS a register. For the DESTINATION groups (0xB0-0xBF, 0xF0): the
    // operand is a memory address that is written.
    // ====================================================================

    /// SOURCE / REGISTER second-byte handler. `ea` is the resolved operand.
    pub(crate) fn exec_src(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size) -> u32 {
        let sub = self.fetch8(bus);
        self.exec_second(bus, ea, size, sub)
    }
    pub(crate) fn exec_reg(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size) -> u32 {
        let sub = self.fetch8(bus);
        self.exec_second(bus, ea, size, sub)
    }

    fn exec_second(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size, sub: u8) -> u32 {
        match sub {
            // LD R, src  (R = current-bank reg selected by low3 in a following
            // form). The canonical "LD r,(mem)" uses second byte 0x20-0x27.
            0x20..=0x27 => {
                let v = self.read_ea(bus, ea, size);
                let code = self.size_reg_code(sub & 0x07, size);
                self.write_reg(code, size, v);
                6
            }
            // LD src(mem), R : store register into the operand (when operand is
            // memory). Second byte 0x40-0x47.
            0x40..=0x47 => {
                let code = self.size_reg_code(sub & 0x07, size);
                let v = self.read_reg(code, size);
                self.write_ea(bus, ea, size, v);
                6
            }
            // EX (mem), R : 0x30-0x37.
            0x30..=0x37 => {
                let code = self.size_reg_code(sub & 0x07, size);
                let a = self.read_ea(bus, ea, size);
                let b = self.read_reg(code, size);
                self.write_ea(bus, ea, size, b);
                self.write_reg(code, size, a);
                8
            }
            // ADD/ADC/SUB/SBC/AND/XOR/OR/CP  R, src : 0x80-0xBF block, where the
            // op = (sub>>3)&7 and the register = low3.
            0x80..=0xBF => {
                let opn = (sub >> 3) & 0x07;
                let code = self.size_reg_code(sub & 0x07, size);
                let src = self.read_ea(bus, ea, size);
                let r = self.read_reg(code, size);
                let res = self.alu_op(opn, r, src, size);
                if opn != 7 {
                    // CP (opn 7) discards result
                    self.write_reg(code, size, res);
                }
                6
            }
            // ADD/.../CP  R, #imm forms with the operand as destination reg:
            // 0xC8-0xCF select op with immediate; handled in reg-form callers.
            // MUL/MULS/DIV/DIVS R, src : 0x08-0x0B (per second-byte table).
            0x08 => { self.do_mul(bus, ea, size, false); 18 }
            0x09 => { self.do_mul(bus, ea, size, true); 18 }
            0x0A => { self.do_div(bus, ea, size, false); 24 }
            0x0B => { self.do_div(bus, ea, size, true); 24 }
            // Rotate/shift on the operand: 0xE8-0xEF with count in A or imm.
            0xE8..=0xEF => {
                let kind = sub & 0x07;
                let cnt = self.read_reg(0xE1, Size::Byte); // W? use A's pair; approx
                let v = self.read_ea(bus, ea, size);
                let r = self.do_shift(kind, v, cnt & 0x0F, size);
                self.write_ea(bus, ea, size, r);
                8
            }
            // Unary on operand: CPL=0x06, NEG=0x07, EXTZ=0x12, EXTS=0x13,
            // INC/DEC handled via 0x60/0x68? We map common ones:
            0x06 => { let v = self.read_ea(bus, ea, size); let r = (!v) & size.mask(); self.write_ea(bus, ea, size, r); 4 } // CPL
            0x07 => { let v = self.read_ea(bus, ea, size); let r = self.alu_sub(0, v, 0, size); self.write_ea(bus, ea, size, r); 4 } // NEG
            0x12 => { let v = self.read_ea(bus, ea, size) & 0xFF; self.write_ea(bus, ea, size, v); 4 } // EXTZ (approx)
            0x13 => { let v = self.read_ea(bus, ea, size); let s = ((v & 0xFF) as i8) as i32 as u32 & size.mask(); self.write_ea(bus, ea, size, s); 4 } // EXTS (approx)
            0x04 => { let v = self.read_ea(bus, ea, size); self.push(bus, size, v); 6 } // PUSH operand
            0x05 => { let v = self.pop(bus, size); self.write_ea(bus, ea, size, v); 6 } // POP operand
            // INC #n / DEC #n : 0x60-0x67 INC, 0x68-0x6F DEC (n = low3, 0->8).
            0x60..=0x67 => { let n = (sub & 0x07) as u32; let n = if n == 0 { 8 } else { n }; let v = self.read_ea(bus, ea, size); let r = self.alu_inc(v, n, size); self.write_ea(bus, ea, size, r); 4 }
            0x68..=0x6F => { let n = (sub & 0x07) as u32; let n = if n == 0 { 8 } else { n }; let v = self.read_ea(bus, ea, size); let r = self.alu_dec(v, n, size); self.write_ea(bus, ea, size, r); 4 }
            // BIT/RES/SET/CHG b,operand : the manual encodes these as
            // 0xC8|b/0xD8|b/0xE8|b/0xF8|b etc. We handle BIT/RES/SET on the
            // 0xC8-0xFF span where the bit number is the low 3 bits.
            0xC8..=0xCF => { let bit = sub & 0x07; let v = self.read_ea(bus, ea, size); self.set_flag(FLAG_Z, (v >> bit) & 1 == 0); self.set_flag(FLAG_H, true); self.set_flag(FLAG_N, false); 4 } // BIT #b,operand
            0xD8..=0xDF => { let bit = sub & 0x07; let v = self.read_ea(bus, ea, size); let r = v & !(1 << bit); self.write_ea(bus, ea, size, r); 4 } // RES #b
            0xF8..=0xFF => { let bit = sub & 0x07; let v = self.read_ea(bus, ea, size); let r = v | (1 << bit); self.write_ea(bus, ea, size, r); 4 } // SET #b
            _ => { self.illegal = true; 4 }
        }
    }

    /// DESTINATION second-byte handler (0xB0-0xBF, 0xF0): operand is a memory
    /// address to write.
    pub(crate) fn exec_dst(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size) -> u32 {
        let addr = match ea {
            Ea::Mem(a) => a,
            Ea::Reg(_) => {
                self.illegal = true;
                return 4;
            }
        };
        let sub = self.fetch8(bus);
        match sub {
            // LD (mem), R : 0x40-0x47.
            0x40..=0x47 => {
                let code = self.size_reg_code(sub & 0x07, size);
                let v = self.read_reg(code, size);
                self.write_mem(bus, addr, size, v);
                6
            }
            // LD (mem), #imm : 0x00 (byte form) / 0x20? Use 0x00 = imm.
            0x00 => {
                let v = self.fetch_imm(bus, size);
                self.write_mem(bus, addr, size, v);
                6
            }
            // LDA R, addr : 0x20-0x27 (load the EFFECTIVE ADDRESS, not contents).
            0x20..=0x27 => {
                let code = self.long_reg_code_pub(sub & 0x07);
                self.write_reg(code, Size::Long, addr);
                6
            }
            // JP addr : 0x10? CALL addr : 0x11? Map common control:
            0x10 => { self.pc = addr; 8 } // JP (mem-address)
            0x11 => { let pc = self.pc; self.push(bus, Size::Long, pc); self.pc = addr; 12 } // CALL
            _ => { self.illegal = true; 4 }
        }
    }

    /// Apply ALU op number 0..7 (ADD,ADC,SUB,SBC,AND,XOR,OR,CP).
    fn alu_op(&mut self, opn: u8, a: u32, b: u32, size: Size) -> u32 {
        let c = self.flag(FLAG_C) as u32;
        match opn & 0x07 {
            0 => self.alu_add(a, b, 0, size),
            1 => self.alu_add(a, b, c, size),
            2 => self.alu_sub(a, b, 0, size),
            3 => self.alu_sub(a, b, c, size),
            4 => self.alu_and(a, b, size),
            5 => self.alu_xor(a, b, size),
            6 => self.alu_or(a, b, size),
            _ => self.alu_sub(a, b, 0, size), // CP
        }
    }

    fn do_mul(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size, _signed: bool) {
        // R := R * operand, result double-width. Approximate into XWA.
        let src = self.read_ea(bus, ea, size);
        let acc = self.xwa() & 0xFFFF;
        let prod = acc.wrapping_mul(src & 0xFFFF);
        let idx = self.rfp() * 4;
        self.regs[idx] = prod;
    }
    fn do_div(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size, _signed: bool) {
        let src = self.read_ea(bus, ea, size) & 0xFFFF;
        let acc = self.xwa();
        if src == 0 {
            self.set_flag(FLAG_V, true);
            return;
        }
        let q = acc / src;
        let r = acc % src;
        let idx = self.rfp() * 4;
        self.regs[idx] = (q & 0xFFFF) | ((r & 0xFFFF) << 16);
        self.set_flag(FLAG_V, q > 0xFFFF);
    }

    /// Map a 0..7 selector to a current-bank register code at the given size.
    fn size_reg_code(&self, n: u8, size: Size) -> u8 {
        match size {
            Size::Byte => Cpu::byte_reg_code(n),
            Size::Word => Cpu::word_reg_code(n),
            Size::Long => Cpu::long_reg_code(n),
        }
    }
    fn long_reg_code_pub(&self, n: u8) -> u8 {
        Cpu::long_reg_code(n)
    }
}
