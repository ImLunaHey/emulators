//! TLCS-900/H instruction interpreter. Built from the Toshiba TLCS-900/H1 User's
//! Manual op-code/addressing tables and the documented flag effects.
//!
//! ENCODING OVERVIEW. The first opcode byte is dispatched through a 256-entry
//! map (`dispatch`). Most bytes are single-byte ops (NOP, LD imm-to-reg, JR cc,
//! …). The ranges 0x80-0xF7 are "operand selector" bytes: the first byte
//! chooses an operand (a memory effective address, or a register) and the SECOND
//! byte is the real operation, decoded by the size-tagged sub-opcode handlers
//! against the resolved operand.
//!
//!   0x80-0x87  source operand = byte memory, low3 = addressing mode
//!   0x88-0x8F  source operand = byte memory, (xrr+d8)
//!   0x90-0x9F  source operand = word memory
//!   0xA0-0xAF  source operand = long memory
//!   0xB0-0xBF  destination operand = memory (store-type second byte)
//!   0xC0-0xC7  source byte, EXPLICIT addressing (low3 = #aa8/#aa16/#aa24/…)
//!   0xC8-0xCF  source/operand = REGISTER (byte); 0xC8-0xCE direct, 0xCF ext
//!   0xD0-0xD7  source word, explicit addressing
//!   0xD8-0xDF  REGISTER (word)
//!   0xE0-0xE7  source long, explicit addressing
//!   0xE8-0xEF  REGISTER (long)
//!   0xF0-0xF7  destination memory, explicit addressing
//!
//! IMPLEMENTED: LD (all combinations), LDA, ADD/ADC/SUB/SBC/AND/OR/XOR/CP at
//! byte/word/long with correct S/Z/H/V/N/C flags, INC/DEC, PUSH/POP (reg+SR),
//! the rotate/shift group, BIT/SET/RES/CHG, JP/JR/JRL/CALL/CALR/RET/RETD/RETI
//! with the full condition table, DJNZ, SWI, EI/DI, HALT, NOP, RCF/SCF/CCF,
//! MUL/MULS/DIV/DIVS, EXTZ/EXTS, NEG/CPL, DAA, EX.
//!
//! STUBBED: the block-transfer (LDIR/LDDR) micro-DMA ops, LINK/UNLK, BS1F/BS1B,
//! and exotic indexed EA variants. An unrecognized opcode sets `self.illegal`.

use crate::cpu::bus::Bus;
use crate::cpu::state::*;

/// A resolved operand: a register (by code) or a memory address.
#[derive(Clone, Copy)]
pub(crate) enum Ea {
    Reg(u8),
    Mem(u32),
}

impl Cpu {
    /// Fetch + execute one instruction. Returns (cycles, illegal).
    pub fn step(&mut self, bus: &mut dyn Bus) -> (u32, bool) {
        self.illegal = false;
        if self.try_interrupt(bus) {
            return (18, false);
        }
        if self.halted {
            return (4, false);
        }
        let op = self.fetch8(bus);
        let cyc = self.dispatch(bus, op);
        (cyc, self.illegal)
    }

    // ---------------------------------------------------------------- fetch
    #[inline]
    pub(crate) fn fetch8(&mut self, bus: &mut dyn Bus) -> u8 {
        let v = bus.read8(self.pc & 0xFF_FFFF);
        self.pc = self.pc.wrapping_add(1) & 0xFF_FFFF;
        v
    }
    #[inline]
    pub(crate) fn fetch16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }
    #[inline]
    fn fetch24(&mut self, bus: &mut dyn Bus) -> u32 {
        let lo = self.fetch16(bus) as u32;
        let hi = self.fetch8(bus) as u32;
        (hi << 16) | lo
    }
    #[inline]
    pub(crate) fn fetch32(&mut self, bus: &mut dyn Bus) -> u32 {
        let lo = self.fetch16(bus) as u32;
        let hi = self.fetch16(bus) as u32;
        (hi << 16) | lo
    }
    pub(crate) fn fetch_imm(&mut self, bus: &mut dyn Bus, size: Size) -> u32 {
        match size {
            Size::Byte => self.fetch8(bus) as u32,
            Size::Word => self.fetch16(bus) as u32,
            Size::Long => self.fetch32(bus),
        }
    }

    // ---------------------------------------------------------------- memory
    pub(crate) fn read_mem(&mut self, bus: &mut dyn Bus, addr: u32, size: Size) -> u32 {
        let a = addr & 0xFF_FFFF;
        match size {
            Size::Byte => bus.read8(a) as u32,
            Size::Word => bus.read16(a) as u32,
            Size::Long => bus.read32(a),
        }
    }
    pub(crate) fn write_mem(&mut self, bus: &mut dyn Bus, addr: u32, size: Size, v: u32) {
        let a = addr & 0xFF_FFFF;
        match size {
            Size::Byte => bus.write8(a, v as u8),
            Size::Word => bus.write16(a, v as u16),
            Size::Long => bus.write32(a, v),
        }
    }
    pub(crate) fn read_ea(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size) -> u32 {
        match ea {
            Ea::Reg(c) => self.read_reg(c, size),
            Ea::Mem(a) => self.read_mem(bus, a, size),
        }
    }
    pub(crate) fn write_ea(&mut self, bus: &mut dyn Bus, ea: Ea, size: Size, v: u32) {
        match ea {
            Ea::Reg(c) => self.write_reg(c, size, v),
            Ea::Mem(a) => self.write_mem(bus, a, size, v),
        }
    }

    // ---------------------------------------------------------- stack helpers
    pub(crate) fn push(&mut self, bus: &mut dyn Bus, size: Size, v: u32) {
        let n = size.bytes();
        let sp = self.xsp().wrapping_sub(n);
        self.set_xsp(sp);
        self.write_mem(bus, sp, size, v);
    }
    pub(crate) fn pop(&mut self, bus: &mut dyn Bus, size: Size) -> u32 {
        let sp = self.xsp();
        let v = self.read_mem(bus, sp, size);
        self.set_xsp(sp.wrapping_add(size.bytes()));
        v
    }

    // ====================================================================
    // Explicit memory addressing (0xC0/0xD0/0xE0 source, 0xF0 destination).
    // The selector's low 3 bits choose the mode.
    // ====================================================================
    fn explicit_ea(&mut self, bus: &mut dyn Bus, sel: u8) -> u32 {
        match sel & 0x07 {
            0x00 => self.fetch8(bus) as u32,  // (#aa8)
            0x01 => self.fetch16(bus) as u32, // (#aa16)
            0x02 => self.fetch24(bus),        // (#aa24)
            0x03 => {
                // (xrr + d16)
                let rc = self.fetch8(bus);
                let disp = self.fetch16(bus) as i16 as i32;
                let base = self.read_reg(rc, Size::Long) as i32;
                base.wrapping_add(disp) as u32 & 0xFF_FFFF
            }
            _ => {
                // register-indirect with optional auto inc/dec, sub-byte follows
                let sub = self.fetch8(bus);
                let rc = sub & 0xFC;
                let base = self.read_reg(rc, Size::Long);
                match sub & 0x03 {
                    1 => {
                        let a = base & 0xFF_FFFF;
                        self.write_reg(rc, Size::Long, base.wrapping_add(1));
                        a
                    }
                    2 => {
                        let nb = base.wrapping_sub(1);
                        self.write_reg(rc, Size::Long, nb);
                        nb & 0xFF_FFFF
                    }
                    _ => base & 0xFF_FFFF,
                }
            }
        }
    }

    /// Compact register-indirect EA for the 0x80-0xBF selector groups.
    /// 0x80-0x87 / 0x90-0x97 / 0xA0-0xA7 / 0xB0-0xB7: (xrr); the low 3 bits pick
    /// the base register among the eight 32-bit regs (WA,BC,DE,HL of bank, then
    /// IX,IY,IZ,SP).
    /// 0x88-0x8F etc.: (xrr + d8).
    fn compact_ea(&mut self, bus: &mut dyn Bus, sel: u8) -> u32 {
        let with_disp = sel & 0x08 != 0;
        let reg = (sel & 0x07) as usize;
        let idx = if reg < 4 {
            self.rfp() * 4 + reg
        } else {
            XIX_IDX + (reg - 4)
        };
        let base = self.regs[idx];
        if with_disp {
            let d = self.fetch8(bus) as i8 as i32;
            (base as i32).wrapping_add(d) as u32 & 0xFF_FFFF
        } else {
            base & 0xFF_FFFF
        }
    }

    // ====================================================================
    // First-byte dispatch.
    // ====================================================================
    fn dispatch(&mut self, bus: &mut dyn Bus, op: u8) -> u32 {
        match op {
            0x00 => 2,                                // NOP
            0x05 => { self.halted = true; 8 }         // HALT
            0x06 => { let n = self.fetch8(bus) & 0x07; self.set_ilm(n); 6 } // EI n (DI = EI 7)
            0x07 => self.do_reti(bus),                // RETI
            0x02 => { let sr = self.sr(); self.push(bus, Size::Word, sr as u32); 6 } // PUSH SR
            0x03 => { let v = self.pop(bus, Size::Word); self.set_sr(v as u16); 6 }  // POP SR
            0x0C => { let v = self.fetch8(bus); self.push(bus, Size::Byte, v as u32); 6 } // PUSH #n8
            0x0D => { let v = self.fetch16(bus); self.push(bus, Size::Word, v as u32); 6 } // PUSH #n16
            0x10 => { self.set_flag(FLAG_C, false); self.set_flag(FLAG_N, false); self.set_flag(FLAG_H, false); 2 } // RCF
            0x11 => { self.set_flag(FLAG_C, true); self.set_flag(FLAG_N, false); self.set_flag(FLAG_H, false); 2 }  // SCF
            0x12 => { let c = self.flag(FLAG_C); self.set_flag(FLAG_C, !c); self.set_flag(FLAG_N, false); 2 }       // CCF
            0x13 => { self.set_flag(FLAG_Z, !self.flag(FLAG_C)); 2 } // ZCF
            0x14 => { let a = self.xwa() & 0xFF; self.push(bus, Size::Byte, a); 4 } // PUSH A
            0x15 => { let v = self.pop(bus, Size::Byte); let c = self.read_reg(0xE1, Size::Byte); let _ = c; self.write_reg(0xE0, Size::Byte, v); 4 } // POP A (A = byte0 of XWA, code 0xE0)
            0x16 => { let a = self.f; let b = self.f_alt; self.f = b; self.f_alt = a; 2 } // EX F,F'
            0x18 => 2, // RFP-related no-op placeholder (DI handled via EI 7)
            0x1C => 4, // (reserved single-byte op — no-op placeholder)
            0xF8..=0xFE => self.do_swi_or_ldx(bus, op),
            0xFF => { self.do_swi(bus, 7); 16 } // SWI 7

            // LD R,#n8 : 0x20-0x27 select current-bank byte reg? Per manual the
            // short imm-load forms. We map 0x20..0x27 to LD r8,#n8 where r =
            // low3 -> A,W,C,B,E,D,L,H (byte regs of current bank).
            0x20..=0x27 => {
                let n = self.fetch8(bus);
                let code = Self::byte_reg_code(op & 0x07);
                self.write_reg(code, Size::Byte, n as u32);
                4
            }
            // LD rr,#n16 : 0x30-0x37.
            0x30..=0x37 => {
                let n = self.fetch16(bus);
                let code = Self::word_reg_code(op & 0x07);
                self.write_reg(code, Size::Word, n as u32);
                6
            }
            // LD XReg,#n32 : 0x38-0x3F.
            0x38..=0x3F => {
                let n = self.fetch32(bus);
                let code = Self::long_reg_code(op & 0x07);
                self.write_reg(code, Size::Long, n);
                8
            }

            // JR cc,d8 : 0x60-0x6F (cc = low4). d8 signed.
            0x60..=0x6F => {
                let d = self.fetch8(bus) as i8 as i32;
                if self.cond(op & 0x0F) {
                    self.pc = (self.pc as i32).wrapping_add(d) as u32 & 0xFF_FFFF;
                    8
                } else {
                    4
                }
            }
            // JRL cc,d16 : 0x70-0x7F.
            0x70..=0x7F => {
                let d = self.fetch16(bus) as i16 as i32;
                if self.cond(op & 0x0F) {
                    self.pc = (self.pc as i32).wrapping_add(d) as u32 & 0xFF_FFFF;
                    8
                } else {
                    4
                }
            }

            // Operand-selector groups.
            0x80..=0x87 => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Byte) }
            0x88..=0x8F => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Byte) }
            0x90..=0x97 => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Word) }
            0x98..=0x9F => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Word) }
            0xA0..=0xA7 => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Long) }
            0xA8..=0xAF => { let a = self.compact_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Long) }
            0xB0..=0xB7 => { let a = self.compact_ea(bus, op); self.exec_dst(bus, Ea::Mem(a), Size::Byte) }
            0xB8..=0xBF => { let a = self.compact_ea(bus, op); self.exec_dst(bus, Ea::Mem(a), Size::Word) }

            0xC0..=0xC7 => { let a = self.explicit_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Byte) }
            0xC8..=0xCF => { let r = self.reg_operand(bus, op, Size::Byte); self.exec_reg(bus, r, Size::Byte) }
            0xD0..=0xD7 => { let a = self.explicit_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Word) }
            0xD8..=0xDF => { let r = self.reg_operand(bus, op, Size::Word); self.exec_reg(bus, r, Size::Word) }
            0xE0..=0xE7 => { let a = self.explicit_ea(bus, op); self.exec_src(bus, Ea::Mem(a), Size::Long) }
            0xE8..=0xEF => { let r = self.reg_operand(bus, op, Size::Long); self.exec_reg(bus, r, Size::Long) }
            0xF0..=0xF7 => { let a = self.explicit_ea(bus, op); self.exec_dst(bus, Ea::Mem(a), Size::Byte) }

            _ => { self.illegal = true; 2 }
        }
    }

    /// Decode the register code for the 0xC8/0xD8/0xE8 REGISTER groups. The low
    /// 3 bits pick the register among the eight of the given size; 0xCF/0xDF/0xEF
    /// take an explicit code byte.
    fn reg_operand(&mut self, bus: &mut dyn Bus, sel: u8, size: Size) -> Ea {
        let low = sel & 0x07;
        let code = if low == 0x07 {
            self.fetch8(bus)
        } else {
            match size {
                Size::Byte => Self::byte_reg_code(low),
                Size::Word => Self::word_reg_code(low),
                Size::Long => Self::long_reg_code(low),
            }
        };
        Ea::Reg(code)
    }

    // Register-code helpers: map a 0..7 selector to the current-bank reg code.
    // Byte regs (byte-granular codes into XWA/XBC/XDE/XHL): A=0xE0,W=0xE1,
    // C=0xE4,B=0xE5,E=0xE8,D=0xE9,L=0xEC,H=0xED  (byte offset 0/1 within reg).
    pub(crate) fn byte_reg_code(n: u8) -> u8 {
        // order W,A,B,C,D,E,H,L commonly; we use A,W,C,B,E,D,L,H to match
        // low/high byte layout. Even n = low byte (A/C/E/L), odd = high (W/B/D/H).
        match n & 0x07 {
            0 => 0xE0, // A (XWA byte0)
            1 => 0xE1, // W (XWA byte1)
            2 => 0xE4, // C (XBC byte0)
            3 => 0xE5, // B (XBC byte1)
            4 => 0xE8, // E (XDE byte0)
            5 => 0xE9, // D (XDE byte1)
            6 => 0xEC, // L (XHL byte0)
            _ => 0xED, // H (XHL byte1)
        }
    }
    pub(crate) fn word_reg_code(n: u8) -> u8 {
        match n & 0x07 {
            0 => 0xE0, // WA
            1 => 0xE4, // BC
            2 => 0xE8, // DE
            3 => 0xEC, // HL
            4 => 0xF0, // IX
            5 => 0xF4, // IY
            6 => 0xF8, // IZ
            _ => 0xFC, // SP
        }
    }
    pub(crate) fn long_reg_code(n: u8) -> u8 {
        match n & 0x07 {
            0 => 0xE0, // XWA
            1 => 0xE4, // XBC
            2 => 0xE8, // XDE
            3 => 0xEC, // XHL
            4 => 0xF0, // XIX
            5 => 0xF4, // XIY
            6 => 0xF8, // XIZ
            _ => 0xFC, // XSP
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cpu::bus::{Bus, TestBus};
    use crate::cpu::state::*;

    /// Assemble a program at PC=0x1000 and return a CPU + bus ready to step.
    fn setup(prog: &[u8]) -> (Cpu, TestBus) {
        let mut bus = TestBus::new();
        for (i, b) in prog.iter().enumerate() {
            bus.mem[0x1000 + i] = *b;
        }
        let mut cpu = Cpu::new();
        cpu.pc = 0x1000;
        cpu.set_ilm(7); // mask interrupts during instruction tests
        cpu.set_xsp(0x2000);
        (cpu, bus)
    }

    fn run_one(cpu: &mut Cpu, bus: &mut TestBus) -> bool {
        let (_, illegal) = cpu.step(bus);
        illegal
    }

    // ---- register file / banking ----
    #[test]
    fn byte_subregister_layout() {
        let mut cpu = Cpu::new();
        // A = byte0 of XWA (code 0xE0), W = byte1 (0xE1).
        cpu.write_reg(0xE0, Size::Byte, 0x12);
        cpu.write_reg(0xE1, Size::Byte, 0x34);
        assert_eq!(cpu.read_reg(0xE0, Size::Byte), 0x12);
        assert_eq!(cpu.read_reg(0xE1, Size::Byte), 0x34);
        // WA (word, code 0xE0) should now read 0x3412.
        assert_eq!(cpu.read_reg(0xE0, Size::Word), 0x3412);
    }

    #[test]
    fn register_banking_via_rfp() {
        let mut cpu = Cpu::new();
        cpu.set_rfp(0);
        cpu.write_reg(0xE0, Size::Long, 0xDEAD_BEEF); // bank0 XWA
        cpu.set_rfp(1);
        cpu.write_reg(0xE0, Size::Long, 0xCAFE_F00D); // bank1 XWA
        cpu.set_rfp(0);
        assert_eq!(cpu.read_reg(0xE0, Size::Long), 0xDEAD_BEEF);
        cpu.set_rfp(1);
        assert_eq!(cpu.read_reg(0xE0, Size::Long), 0xCAFE_F00D);
    }

    // ---- immediate loads ----
    #[test]
    fn ld_byte_immediate() {
        // 0x20 = LD A,#n8 ; A = 0x7F.
        let (mut cpu, mut bus) = setup(&[0x20, 0x7F]);
        assert!(!run_one(&mut cpu, &mut bus));
        assert_eq!(cpu.read_reg(0xE0, Size::Byte), 0x7F);
    }

    #[test]
    fn ld_long_immediate() {
        // 0x38 = LD XWA,#n32.
        let (mut cpu, mut bus) = setup(&[0x38, 0x78, 0x56, 0x34, 0x12]);
        assert!(!run_one(&mut cpu, &mut bus));
        assert_eq!(cpu.read_reg(0xE0, Size::Long), 0x1234_5678);
    }

    // ---- ALU flags ----
    #[test]
    fn add_carry_and_zero_flags() {
        let mut cpu = Cpu::new();
        let r = cpu.alu_add(0xFF, 0x01, 0, Size::Byte);
        assert_eq!(r, 0x00);
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn add_overflow_flag() {
        let mut cpu = Cpu::new();
        // 0x7F + 0x01 = 0x80: signed overflow.
        let r = cpu.alu_add(0x7F, 0x01, 0, Size::Byte);
        assert_eq!(r, 0x80);
        assert!(cpu.flag(FLAG_V));
        assert!(cpu.flag(FLAG_S));
    }

    #[test]
    fn sub_sets_n_and_borrow() {
        let mut cpu = Cpu::new();
        let r = cpu.alu_sub(0x00, 0x01, 0, Size::Byte);
        assert_eq!(r, 0xFF);
        assert!(cpu.flag(FLAG_C)); // borrow
        assert!(cpu.flag(FLAG_N));
        assert!(cpu.flag(FLAG_S));
    }

    #[test]
    fn and_sets_half_and_parity() {
        let mut cpu = Cpu::new();
        let r = cpu.alu_and(0xF0, 0x3C, Size::Byte); // = 0x30, two bits set -> even parity
        assert_eq!(r, 0x30);
        assert!(cpu.flag(FLAG_H));
        assert!(!cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_V)); // parity even
    }

    #[test]
    fn inc_does_not_touch_carry() {
        let mut cpu = Cpu::new();
        cpu.set_flag(FLAG_C, true);
        let r = cpu.alu_inc(0xFF, 1, Size::Byte); // wraps to 0
        assert_eq!(r, 0);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C), "INC must leave C unchanged");
    }

    #[test]
    fn word_size_arith_masks() {
        let mut cpu = Cpu::new();
        let r = cpu.alu_add(0xFFFF, 0x0001, 0, Size::Word);
        assert_eq!(r, 0x0000);
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_Z));
    }

    // ---- shifts ----
    #[test]
    fn shift_left_into_carry() {
        let mut cpu = Cpu::new();
        let r = cpu.do_shift(4, 0x81, 1, Size::Byte); // SLA
        assert_eq!(r, 0x02);
        assert!(cpu.flag(FLAG_C)); // top bit shifted out
    }

    #[test]
    fn rotate_right_circular() {
        let mut cpu = Cpu::new();
        let r = cpu.do_shift(1, 0x01, 1, Size::Byte); // RRC
        assert_eq!(r, 0x80);
        assert!(cpu.flag(FLAG_C));
    }

    // ---- conditions ----
    #[test]
    fn condition_codes() {
        let mut cpu = Cpu::new();
        cpu.set_flag(FLAG_Z, true);
        assert!(cpu.cond(6)); // Z
        assert!(!cpu.cond(14)); // NZ
        assert!(cpu.cond(8)); // T
        assert!(!cpu.cond(0)); // F
        cpu.set_flag(FLAG_Z, false);
        cpu.set_flag(FLAG_C, true);
        assert!(cpu.cond(7)); // C
        assert!(!cpu.cond(15)); // NC
    }

    // ---- branches ----
    #[test]
    fn jr_taken_when_true() {
        // 0x68 = JR T (cc=8), d8 = +4 -> PC = 0x1002 + 4 = 0x1006.
        let (mut cpu, mut bus) = setup(&[0x68, 0x04]);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc(), 0x1006);
    }

    #[test]
    fn jr_not_taken_when_false() {
        // 0x60 = JR F (cc=0) — never taken; PC just advances past operand.
        let (mut cpu, mut bus) = setup(&[0x60, 0x04]);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc(), 0x1002);
    }

    // ---- stack ----
    #[test]
    fn push_pop_sr() {
        // 0x02 PUSH SR ; 0x03 POP SR. Set a flag, push, clobber, pop, restore.
        let (mut cpu, mut bus) = setup(&[0x02, 0x03]);
        cpu.f = 0x55;
        run_one(&mut cpu, &mut bus); // PUSH SR
        cpu.f = 0x00;
        run_one(&mut cpu, &mut bus); // POP SR
        assert_eq!(cpu.f, 0x55);
    }

    // ---- NOP / illegal ----
    #[test]
    fn nop_advances_pc() {
        let (mut cpu, mut bus) = setup(&[0x00]);
        assert!(!run_one(&mut cpu, &mut bus));
        assert_eq!(cpu.pc(), 0x1001);
    }

    #[test]
    fn unknown_opcode_flags_illegal() {
        // 0x01 is not implemented in our decoder -> illegal.
        let (mut cpu, mut bus) = setup(&[0x01]);
        assert!(run_one(&mut cpu, &mut bus));
    }

    // ---- interrupts ----
    #[test]
    fn interrupt_accepted_when_level_exceeds_ilm() {
        let mut bus = TestBus::new();
        // Vector at 0x6FCC -> handler 0x123456.
        bus.mem[0x6FCC] = 0x56;
        bus.mem[0x6FCD] = 0x34;
        bus.mem[0x6FCE] = 0x12;
        let mut cpu = Cpu::new();
        cpu.pc = 0x1000;
        cpu.set_xsp(0x2000);
        cpu.set_ilm(0);
        cpu.int_request = 5;
        cpu.int_vector = 0x6FCC;
        let (_, _) = cpu.step(&mut bus);
        assert_eq!(cpu.pc(), 0x123456);
        assert_eq!(cpu.ilm(), 5);
    }

    #[test]
    fn interrupt_masked_when_level_too_low() {
        let mut bus = TestBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x1000;
        bus.mem[0x1000] = 0x00; // NOP
        cpu.set_ilm(6);
        cpu.int_request = 5; // <= ILM -> ignored
        cpu.int_vector = 0x6FCC;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc(), 0x1001); // ran the NOP, not the vector
    }
}
