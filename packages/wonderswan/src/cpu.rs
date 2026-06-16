//! NEC V30MZ CPU core — an 80186/8086-compatible 16-bit x86 processor in real
//! mode. This is the WonderSwan's CPU and the bulk of the core's work.
//!
//! Spec: the Intel 8086/80186 instruction reference and the NEC V30/V30MZ
//! datasheet. We implement real-mode segmented x86: the eight 16-bit general
//! registers (AX/CX/DX/BX/SP/BP/SI/DI) with their 8-bit halves, the four
//! segment registers (ES/CS/SS/DS), IP, and the FLAGS word; full ModR/M
//! addressing with segment-override and REP/REPNE/LOCK prefixes; and the
//! standard arithmetic / logic / string / control-transfer instruction set.
//!
//! The CPU drives memory through `&mut dyn V30Bus` (see `crate::bus::V30Bus`), so
//! it never knows which device backs an address. `step()` executes one
//! instruction and returns the (approximate) cycle count it consumed; the
//! orchestrator advances the video/audio by that many cycles.
//!
//! Implemented: the full 8086 base opcode map (mov/arith/logic/shift/string/
//! jump/call/loop/int/flags/stack) plus the 80186 additions used by WS games
//! (PUSHA/POPA, IMUL imm, ENTER/LEAVE, INS/OUTS, shift-by-imm, BOUND). Floating
//! point and protected mode do not exist on the V30MZ.

use crate::bus::V30Bus;

// ---- FLAGS bits (8086 layout) ----
pub const F_CF: u16 = 1 << 0; // carry
pub const F_PF: u16 = 1 << 2; // parity
pub const F_AF: u16 = 1 << 4; // auxiliary carry
pub const F_ZF: u16 = 1 << 6; // zero
pub const F_SF: u16 = 1 << 7; // sign
pub const F_TF: u16 = 1 << 8; // trap
pub const F_IF: u16 = 1 << 9; // interrupt enable
pub const F_DF: u16 = 1 << 10; // direction
pub const F_OF: u16 = 1 << 11; // overflow

/// Bits 1,12-15 are reserved; the 8086 reads bit1 as 1 and 12-15 as 1.
const FLAGS_RESERVED_ON: u16 = 0xF002;

// Register indices for the 16-bit general regs (encoding order).
pub const REG_AX: usize = 0;
pub const REG_CX: usize = 1;
pub const REG_DX: usize = 2;
pub const REG_BX: usize = 3;
pub const REG_SP: usize = 4;
pub const REG_BP: usize = 5;
pub const REG_SI: usize = 6;
pub const REG_DI: usize = 7;

// Segment register indices (encoding order for the seg reg field).
pub const SEG_ES: usize = 0;
pub const SEG_CS: usize = 1;
pub const SEG_SS: usize = 2;
pub const SEG_DS: usize = 3;

/// A pending segment override prefix selected by 0x26/0x2E/0x36/0x3E.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SegOverride {
    None,
    Seg(usize),
}

#[derive(Clone)]
pub struct Cpu {
    /// General registers in encoding order: AX,CX,DX,BX,SP,BP,SI,DI.
    pub r: [u16; 8],
    /// Segment registers in encoding order: ES,CS,SS,DS.
    pub seg: [u16; 4],
    pub ip: u16,
    pub flags: u16,

    /// HLT executed: CPU idles until an interrupt arrives.
    pub halted: bool,
    /// External maskable interrupt line state (the WS interrupt controller).
    pub irq_line: bool,
    /// IRQ vector number presented by the interrupt controller when `irq_line`
    /// is asserted and IF is set.
    pub irq_vector: u8,

    /// Latched undefined-opcode trap for the crash screen: `(opcode, cs, ip)`.
    pub fault: Option<(u8, u16, u16)>,

    /// The raw effective *offset* (pre-segment) of the most recently decoded
    /// memory ModR/M operand. Used by LEA, which wants the offset, not the
    /// physical (segment-applied) address.
    last_ea_offset: u16,

    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

/// Decoded ModR/M: either a register operand or a computed effective address.
#[derive(Clone, Copy)]
enum Operand {
    Reg(u8),       // register number (interpretation depends on operand size)
    Mem(u32),      // physical address already resolved (with segment applied)
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            r: [0; 8],
            // Power-on: the V30MZ, like the 8086, starts at CS=0xFFFF, IP=0x0000,
            // i.e. physical 0xFFFF0. The WonderSwan boot ROM / cart entry lives
            // near the top of the address space.
            seg: [0, 0xFFFF, 0, 0],
            ip: 0x0000,
            flags: FLAGS_RESERVED_ON,
            halted: false,
            irq_line: false,
            irq_vector: 0,
            fault: None,
            last_ea_offset: 0,
            cycles: 0,
        }
    }

    /// Reset to the power-on vector (physical 0xFFFF0).
    pub fn reset(&mut self) {
        self.r = [0; 8];
        self.seg = [0, 0xFFFF, 0, 0];
        self.ip = 0;
        self.flags = FLAGS_RESERVED_ON;
        self.halted = false;
        self.fault = None;
        self.last_ea_offset = 0;
        self.cycles = 0;
    }

    // ---- flag helpers ----
    #[inline]
    pub fn flag(&self, f: u16) -> bool {
        self.flags & f != 0
    }
    #[inline]
    fn set_flag(&mut self, f: u16, on: bool) {
        if on {
            self.flags |= f;
        } else {
            self.flags &= !f;
        }
    }

    // ---- 8-bit register access (AL,CL,DL,BL,AH,CH,DH,BH) ----
    #[inline]
    fn get_reg8(&self, n: u8) -> u8 {
        let idx = (n & 3) as usize;
        if n < 4 {
            (self.r[idx] & 0xFF) as u8 // low byte
        } else {
            (self.r[idx] >> 8) as u8 // high byte
        }
    }
    #[inline]
    fn set_reg8(&mut self, n: u8, v: u8) {
        let idx = (n & 3) as usize;
        if n < 4 {
            self.r[idx] = (self.r[idx] & 0xFF00) | v as u16;
        } else {
            self.r[idx] = (self.r[idx] & 0x00FF) | ((v as u16) << 8);
        }
    }

    #[inline]
    fn get_reg16(&self, n: u8) -> u16 {
        self.r[(n & 7) as usize]
    }
    #[inline]
    fn set_reg16(&mut self, n: u8, v: u16) {
        self.r[(n & 7) as usize] = v;
    }

    // ---- physical address from segment:offset ----
    #[inline]
    fn phys(seg: u16, off: u16) -> u32 {
        (((seg as u32) << 4) + off as u32) & crate::bus::ADDR_MASK
    }

    // ---- instruction stream fetch ----
    #[inline]
    fn fetch8(&mut self, bus: &mut dyn V30Bus) -> u8 {
        let a = Self::phys(self.seg[SEG_CS], self.ip);
        self.ip = self.ip.wrapping_add(1);
        bus.read8(a)
    }
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn V30Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }

    // ---- stack ----
    #[inline]
    fn push16(&mut self, bus: &mut dyn V30Bus, v: u16) {
        self.r[REG_SP] = self.r[REG_SP].wrapping_sub(2);
        let a = Self::phys(self.seg[SEG_SS], self.r[REG_SP]);
        bus.write16(a, v);
    }
    #[inline]
    fn pop16(&mut self, bus: &mut dyn V30Bus) -> u16 {
        let a = Self::phys(self.seg[SEG_SS], self.r[REG_SP]);
        let v = bus.read16(a);
        self.r[REG_SP] = self.r[REG_SP].wrapping_add(2);
        v
    }

    /// Effective segment for a data access, honoring an override prefix and the
    /// instruction's default segment (DS for most, SS for BP-based addressing).
    #[inline]
    fn data_seg(&self, ov: SegOverride, default: usize) -> u16 {
        match ov {
            SegOverride::Seg(s) => self.seg[s],
            SegOverride::None => self.seg[default],
        }
    }

    /// Decode the ModR/M byte. Returns (operand, reg_field). `ov` is any active
    /// segment override. Advances IP past displacement bytes.
    fn decode_modrm(
        &mut self,
        bus: &mut dyn V30Bus,
        modrm: u8,
        ov: SegOverride,
    ) -> (Operand, u8) {
        let md = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        if md == 3 {
            return (Operand::Reg(rm), reg);
        }
        // Compute the effective address. The default segment is SS when the base
        // register is BP (modes 2,3,6 with disp), DS otherwise.
        let (base, default_seg): (u16, usize) = match rm {
            0 => (self.r[REG_BX].wrapping_add(self.r[REG_SI]), SEG_DS),
            1 => (self.r[REG_BX].wrapping_add(self.r[REG_DI]), SEG_DS),
            2 => (self.r[REG_BP].wrapping_add(self.r[REG_SI]), SEG_SS),
            3 => (self.r[REG_BP].wrapping_add(self.r[REG_DI]), SEG_SS),
            4 => (self.r[REG_SI], SEG_DS),
            5 => (self.r[REG_DI], SEG_DS),
            6 => {
                if md == 0 {
                    // Special case: direct 16-bit displacement, segment DS.
                    let disp = self.fetch16(bus);
                    self.last_ea_offset = disp;
                    let seg = self.data_seg(ov, SEG_DS);
                    return (Operand::Mem(Self::phys(seg, disp)), reg);
                }
                (self.r[REG_BP], SEG_SS)
            }
            7 => (self.r[REG_BX], SEG_DS),
            _ => unreachable!(),
        };
        let disp: u16 = match md {
            0 => 0,
            1 => self.fetch8(bus) as i8 as i16 as u16, // sign-extended byte
            2 => self.fetch16(bus),
            _ => 0,
        };
        let off = base.wrapping_add(disp);
        self.last_ea_offset = off;
        let seg = self.data_seg(ov, default_seg);
        (Operand::Mem(Self::phys(seg, off)), reg)
    }

    // ---- operand read/write through a decoded Operand ----
    fn read_op8(&mut self, bus: &mut dyn V30Bus, op: Operand) -> u8 {
        match op {
            Operand::Reg(n) => self.get_reg8(n),
            Operand::Mem(a) => bus.read8(a),
        }
    }
    fn write_op8(&mut self, bus: &mut dyn V30Bus, op: Operand, v: u8) {
        match op {
            Operand::Reg(n) => self.set_reg8(n, v),
            Operand::Mem(a) => bus.write8(a, v),
        }
    }
    fn read_op16(&mut self, bus: &mut dyn V30Bus, op: Operand) -> u16 {
        match op {
            Operand::Reg(n) => self.get_reg16(n),
            Operand::Mem(a) => bus.read16(a),
        }
    }
    fn write_op16(&mut self, bus: &mut dyn V30Bus, op: Operand, v: u16) {
        match op {
            Operand::Reg(n) => self.set_reg16(n, v),
            Operand::Mem(a) => bus.write16(a, v),
        }
    }

    // =====================================================================
    // Flag-setting ALU primitives.
    // =====================================================================
    #[inline]
    fn set_pzs8(&mut self, v: u8) {
        self.set_flag(F_ZF, v == 0);
        self.set_flag(F_SF, v & 0x80 != 0);
        self.set_flag(F_PF, (v.count_ones() & 1) == 0);
    }
    #[inline]
    fn set_pzs16(&mut self, v: u16) {
        self.set_flag(F_ZF, v == 0);
        self.set_flag(F_SF, v & 0x8000 != 0);
        // Parity is computed over the low byte only on x86.
        self.set_flag(F_PF, ((v as u8).count_ones() & 1) == 0);
    }

    fn add8(&mut self, a: u8, b: u8, carry: u8) -> u8 {
        let r = a as u16 + b as u16 + carry as u16;
        let res = r as u8;
        self.set_flag(F_CF, r > 0xFF);
        self.set_flag(F_AF, ((a & 0xF) + (b & 0xF) + carry) > 0xF);
        self.set_flag(F_OF, ((a ^ res) & (b ^ res) & 0x80) != 0);
        self.set_pzs8(res);
        res
    }
    fn add16(&mut self, a: u16, b: u16, carry: u16) -> u16 {
        let r = a as u32 + b as u32 + carry as u32;
        let res = r as u16;
        self.set_flag(F_CF, r > 0xFFFF);
        self.set_flag(F_AF, ((a & 0xF) + (b & 0xF) + carry) > 0xF);
        self.set_flag(F_OF, ((a ^ res) & (b ^ res) & 0x8000) != 0);
        self.set_pzs16(res);
        res
    }
    fn sub8(&mut self, a: u8, b: u8, borrow: u8) -> u8 {
        let r = (a as i16) - (b as i16) - (borrow as i16);
        let res = r as u8;
        self.set_flag(F_CF, (a as u16) < (b as u16) + (borrow as u16));
        self.set_flag(F_AF, (a & 0xF) as i16 - (b & 0xF) as i16 - (borrow as i16) < 0);
        self.set_flag(F_OF, ((a ^ b) & (a ^ res) & 0x80) != 0);
        self.set_pzs8(res);
        res
    }
    fn sub16(&mut self, a: u16, b: u16, borrow: u16) -> u16 {
        let r = (a as i32) - (b as i32) - (borrow as i32);
        let res = r as u16;
        self.set_flag(F_CF, (a as u32) < (b as u32) + (borrow as u32));
        self.set_flag(F_AF, (a & 0xF) as i32 - (b & 0xF) as i32 - (borrow as i32) < 0);
        self.set_flag(F_OF, ((a ^ b) & (a ^ res) & 0x8000) != 0);
        self.set_pzs16(res);
        res
    }
    fn and8(&mut self, a: u8, b: u8) -> u8 {
        let r = a & b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs8(r);
        r
    }
    fn and16(&mut self, a: u16, b: u16) -> u16 {
        let r = a & b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs16(r);
        r
    }
    fn or8(&mut self, a: u8, b: u8) -> u8 {
        let r = a | b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs8(r);
        r
    }
    fn or16(&mut self, a: u16, b: u16) -> u16 {
        let r = a | b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs16(r);
        r
    }
    fn xor8(&mut self, a: u8, b: u8) -> u8 {
        let r = a ^ b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs8(r);
        r
    }
    fn xor16(&mut self, a: u16, b: u16) -> u16 {
        let r = a ^ b;
        self.set_flag(F_CF, false);
        self.set_flag(F_OF, false);
        self.set_flag(F_AF, false);
        self.set_pzs16(r);
        r
    }

    /// Dispatch one of the eight ALU group ops by index (ADD/OR/ADC/SBB/AND/SUB/
    /// XOR/CMP) for 8-bit; returns the result (CMP/the caller discards).
    fn alu8(&mut self, op: u8, a: u8, b: u8) -> u8 {
        let c = if self.flag(F_CF) { 1 } else { 0 };
        match op & 7 {
            0 => self.add8(a, b, 0),
            1 => self.or8(a, b),
            2 => self.add8(a, b, c),
            3 => self.sub8(a, b, c),
            4 => self.and8(a, b),
            5 => self.sub8(a, b, 0),
            6 => self.xor8(a, b),
            7 => {
                self.sub8(a, b, 0);
                a
            } // CMP: discard result
            _ => unreachable!(),
        }
    }
    fn alu16(&mut self, op: u8, a: u16, b: u16) -> u16 {
        let c = if self.flag(F_CF) { 1 } else { 0 };
        match op & 7 {
            0 => self.add16(a, b, 0),
            1 => self.or16(a, b),
            2 => self.add16(a, b, c),
            3 => self.sub16(a, b, c),
            4 => self.and16(a, b),
            5 => self.sub16(a, b, 0),
            6 => self.xor16(a, b),
            7 => {
                self.sub16(a, b, 0);
                a
            }
            _ => unreachable!(),
        }
    }

    // ---- INC/DEC: like ADD/SUB 1 but leave CF untouched ----
    fn inc8(&mut self, a: u8) -> u8 {
        let cf = self.flag(F_CF);
        let r = self.add8(a, 1, 0);
        self.set_flag(F_CF, cf);
        r
    }
    fn dec8(&mut self, a: u8) -> u8 {
        let cf = self.flag(F_CF);
        let r = self.sub8(a, 1, 0);
        self.set_flag(F_CF, cf);
        r
    }
    fn inc16(&mut self, a: u16) -> u16 {
        let cf = self.flag(F_CF);
        let r = self.add16(a, 1, 0);
        self.set_flag(F_CF, cf);
        r
    }
    fn dec16(&mut self, a: u16) -> u16 {
        let cf = self.flag(F_CF);
        let r = self.sub16(a, 1, 0);
        self.set_flag(F_CF, cf);
        r
    }

    // =====================================================================
    // Interrupt entry.
    // =====================================================================
    /// Software/hardware interrupt: push FLAGS,CS,IP; clear IF,TF; load the
    /// vector from the IVT at physical `vector*4`.
    fn interrupt(&mut self, bus: &mut dyn V30Bus, vector: u8) {
        let f = (self.flags & !FLAGS_RESERVED_ON) | FLAGS_RESERVED_ON;
        self.push16(bus, f);
        self.push16(bus, self.seg[SEG_CS]);
        self.push16(bus, self.ip);
        self.set_flag(F_IF, false);
        self.set_flag(F_TF, false);
        let base = (vector as u32) * 4;
        let new_ip = bus.read16(base);
        let new_cs = bus.read16(base + 2);
        self.ip = new_ip;
        self.seg[SEG_CS] = new_cs;
        self.halted = false;
    }

    /// Service a pending maskable hardware interrupt if IF is set. Called by the
    /// orchestrator before each instruction.
    pub fn poll_irq(&mut self, bus: &mut dyn V30Bus) {
        if self.irq_line && self.flag(F_IF) {
            let v = self.irq_vector;
            self.interrupt(bus, v);
        }
    }

    /// Execute exactly one instruction (consuming any prefixes) and return the
    /// approximate cycle count. If halted, returns a small idle cost.
    pub fn step(&mut self, bus: &mut dyn V30Bus) -> u32 {
        if self.halted {
            // Wake on a pending, enabled IRQ (vectoring consumes this step);
            // otherwise stay idle.
            if self.irq_line && self.flag(F_IF) {
                self.poll_irq(bus);
            }
            return 2;
        }
        self.poll_irq(bus);
        if self.halted {
            return 2;
        }

        // Consume prefixes.
        let mut seg_ov = SegOverride::None;
        let mut rep: u8 = 0; // 0=none, 0xF2=REPNE, 0xF3=REP/REPE
        loop {
            let op = self.fetch8(bus);
            match op {
                0x26 => seg_ov = SegOverride::Seg(SEG_ES),
                0x2E => seg_ov = SegOverride::Seg(SEG_CS),
                0x36 => seg_ov = SegOverride::Seg(SEG_SS),
                0x3E => seg_ov = SegOverride::Seg(SEG_DS),
                0xF0 => { /* LOCK — no-op on a single core */ }
                0xF2 => rep = 0xF2,
                0xF3 => rep = 0xF3,
                _ => return self.execute(bus, op, seg_ov, rep),
            }
        }
    }

    /// Decode + execute the opcode `op` with any active prefixes. Returns cycles.
    fn execute(&mut self, bus: &mut dyn V30Bus, op: u8, ov: SegOverride, rep: u8) -> u32 {
        match op {
            // ---- ALU r/m, reg and reg, r/m (the 00-3F grid) ----
            0x00 | 0x08 | 0x10 | 0x18 | 0x20 | 0x28 | 0x30 | 0x38 => {
                // op r/m8, r8
                let alu = op >> 3;
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op8(bus, rm);
                let b = self.get_reg8(reg);
                let r = self.alu8(alu, a, b);
                if alu != 7 {
                    self.write_op8(bus, rm, r);
                }
                16
            }
            0x01 | 0x09 | 0x11 | 0x19 | 0x21 | 0x29 | 0x31 | 0x39 => {
                let alu = op >> 3;
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op16(bus, rm);
                let b = self.get_reg16(reg);
                let r = self.alu16(alu, a, b);
                if alu != 7 {
                    self.write_op16(bus, rm, r);
                }
                16
            }
            0x02 | 0x0A | 0x12 | 0x1A | 0x22 | 0x2A | 0x32 | 0x3A => {
                let alu = op >> 3;
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.get_reg8(reg);
                let b = self.read_op8(bus, rm);
                let r = self.alu8(alu, a, b);
                if alu != 7 {
                    self.set_reg8(reg, r);
                }
                16
            }
            0x03 | 0x0B | 0x13 | 0x1B | 0x23 | 0x2B | 0x33 | 0x3B => {
                let alu = op >> 3;
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.get_reg16(reg);
                let b = self.read_op16(bus, rm);
                let r = self.alu16(alu, a, b);
                if alu != 7 {
                    self.set_reg16(reg, r);
                }
                16
            }
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                // op AL, imm8
                let alu = op >> 3;
                let imm = self.fetch8(bus);
                let a = self.get_reg8(0);
                let r = self.alu8(alu, a, imm);
                if alu != 7 {
                    self.set_reg8(0, r);
                }
                4
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                // op AX, imm16
                let alu = op >> 3;
                let imm = self.fetch16(bus);
                let a = self.get_reg16(REG_AX as u8);
                let r = self.alu16(alu, a, imm);
                if alu != 7 {
                    self.set_reg16(REG_AX as u8, r);
                }
                4
            }

            // ---- PUSH/POP segment regs ----
            0x06 => {
                self.push16(bus, self.seg[SEG_ES]);
                10
            }
            0x07 => {
                self.seg[SEG_ES] = self.pop16(bus);
                8
            }
            0x0E => {
                self.push16(bus, self.seg[SEG_CS]);
                10
            }
            0x16 => {
                self.push16(bus, self.seg[SEG_SS]);
                10
            }
            0x17 => {
                self.seg[SEG_SS] = self.pop16(bus);
                8
            }
            0x1E => {
                self.push16(bus, self.seg[SEG_DS]);
                10
            }
            0x1F => {
                self.seg[SEG_DS] = self.pop16(bus);
                8
            }

            // ---- segment-prefix bytes already consumed in step(); 0x0F is the
            // 80186/V30 extended escape, rare on WS — treat as NOP-ish ----
            0x0F => 2,

            // ---- DAA/DAS/AAA/AAS ----
            0x27 => {
                self.daa();
                4
            }
            0x2F => {
                self.das();
                4
            }
            0x37 => {
                self.aaa();
                4
            }
            0x3F => {
                self.aas();
                4
            }

            // ---- INC/DEC reg16 (0x40-0x4F) ----
            0x40..=0x47 => {
                let n = (op - 0x40) as u8;
                let v = self.get_reg16(n);
                let r = self.inc16(v);
                self.set_reg16(n, r);
                3
            }
            0x48..=0x4F => {
                let n = (op - 0x48) as u8;
                let v = self.get_reg16(n);
                let r = self.dec16(v);
                self.set_reg16(n, r);
                3
            }

            // ---- PUSH/POP reg16 (0x50-0x5F) ----
            0x50..=0x57 => {
                let n = (op - 0x50) as u8;
                let v = self.get_reg16(n);
                self.push16(bus, v);
                11
            }
            0x58..=0x5F => {
                let n = (op - 0x58) as u8;
                let v = self.pop16(bus);
                self.set_reg16(n, v);
                8
            }

            // ---- 80186: PUSHA/POPA ----
            0x60 => {
                let sp = self.r[REG_SP];
                self.push16(bus, self.r[REG_AX]);
                self.push16(bus, self.r[REG_CX]);
                self.push16(bus, self.r[REG_DX]);
                self.push16(bus, self.r[REG_BX]);
                self.push16(bus, sp);
                self.push16(bus, self.r[REG_BP]);
                self.push16(bus, self.r[REG_SI]);
                self.push16(bus, self.r[REG_DI]);
                36
            }
            0x61 => {
                self.r[REG_DI] = self.pop16(bus);
                self.r[REG_SI] = self.pop16(bus);
                self.r[REG_BP] = self.pop16(bus);
                let _ = self.pop16(bus); // discarded SP
                self.r[REG_BX] = self.pop16(bus);
                self.r[REG_DX] = self.pop16(bus);
                self.r[REG_CX] = self.pop16(bus);
                self.r[REG_AX] = self.pop16(bus);
                51
            }
            // 0x62 BOUND — check array bounds; we accept and ignore (no trap).
            0x62 => {
                let modrm = self.fetch8(bus);
                let _ = self.decode_modrm(bus, modrm, ov);
                13
            }

            // ---- PUSH imm16 / imm8 (80186) ----
            0x68 => {
                let imm = self.fetch16(bus);
                self.push16(bus, imm);
                3
            }
            0x6A => {
                let imm = self.fetch8(bus) as i8 as i16 as u16;
                self.push16(bus, imm);
                3
            }
            // IMUL r16, r/m16, imm16 (0x69) and imm8 (0x6B) — 80186
            0x69 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let src = self.read_op16(bus, rm) as i16 as i32;
                let imm = self.fetch16(bus) as i16 as i32;
                let prod = src * imm;
                self.set_reg16(reg, prod as u16);
                let ovf = prod != (prod as i16 as i32);
                self.set_flag(F_CF, ovf);
                self.set_flag(F_OF, ovf);
                25
            }
            0x6B => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let src = self.read_op16(bus, rm) as i16 as i32;
                let imm = self.fetch8(bus) as i8 as i32;
                let prod = src * imm;
                self.set_reg16(reg, prod as u16);
                let ovf = prod != (prod as i16 as i32);
                self.set_flag(F_CF, ovf);
                self.set_flag(F_OF, ovf);
                25
            }

            // ---- conditional jumps (0x70-0x7F) ----
            0x70..=0x7F => {
                let disp = self.fetch8(bus) as i8 as i16;
                if self.cond(op & 0x0F) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                    16
                } else {
                    4
                }
            }

            // ---- group1: op r/m, imm (0x80-0x83) ----
            0x80 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch8(bus);
                let a = self.read_op8(bus, rm);
                let r = self.alu8(reg, a, imm);
                if (reg & 7) != 7 {
                    self.write_op8(bus, rm, r);
                }
                17
            }
            0x81 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch16(bus);
                let a = self.read_op16(bus, rm);
                let r = self.alu16(reg, a, imm);
                if (reg & 7) != 7 {
                    self.write_op16(bus, rm, r);
                }
                17
            }
            0x82 => {
                // alias of 0x80 on 8086
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch8(bus);
                let a = self.read_op8(bus, rm);
                let r = self.alu8(reg, a, imm);
                if (reg & 7) != 7 {
                    self.write_op8(bus, rm, r);
                }
                17
            }
            0x83 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch8(bus) as i8 as i16 as u16; // sign-extend
                let a = self.read_op16(bus, rm);
                let r = self.alu16(reg, a, imm);
                if (reg & 7) != 7 {
                    self.write_op16(bus, rm, r);
                }
                17
            }

            // ---- TEST r/m, reg ----
            0x84 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op8(bus, rm);
                let b = self.get_reg8(reg);
                self.and8(a, b);
                9
            }
            0x85 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op16(bus, rm);
                let b = self.get_reg16(reg);
                self.and16(a, b);
                9
            }
            // ---- XCHG r/m, reg ----
            0x86 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op8(bus, rm);
                let b = self.get_reg8(reg);
                self.write_op8(bus, rm, b);
                self.set_reg8(reg, a);
                17
            }
            0x87 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op16(bus, rm);
                let b = self.get_reg16(reg);
                self.write_op16(bus, rm, b);
                self.set_reg16(reg, a);
                17
            }
            // ---- MOV r/m, reg and reg, r/m ----
            0x88 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let b = self.get_reg8(reg);
                self.write_op8(bus, rm, b);
                9
            }
            0x89 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let b = self.get_reg16(reg);
                self.write_op16(bus, rm, b);
                9
            }
            0x8A => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let v = self.read_op8(bus, rm);
                self.set_reg8(reg, v);
                8
            }
            0x8B => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let v = self.read_op16(bus, rm);
                self.set_reg16(reg, v);
                8
            }
            // MOV r/m16, sreg
            0x8C => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let v = self.seg[(reg & 3) as usize];
                self.write_op16(bus, rm, v);
                9
            }
            // LEA reg, m — load the effective *offset* (segment is irrelevant).
            0x8D => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                if let Operand::Mem(_) = rm {
                    self.set_reg16(reg, self.last_ea_offset);
                }
                6
            }
            // MOV sreg, r/m16
            0x8E => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let v = self.read_op16(bus, rm);
                self.seg[(reg & 3) as usize] = v;
                9
            }
            // POP r/m16 (group)
            0x8F => {
                let modrm = self.fetch8(bus);
                let (rm, _reg) = self.decode_modrm(bus, modrm, ov);
                let v = self.pop16(bus);
                self.write_op16(bus, rm, v);
                17
            }

            // ---- XCHG AX, reg (0x90-0x97); 0x90 = NOP ----
            0x90 => 3, // NOP
            0x91..=0x97 => {
                let n = (op - 0x90) as u8;
                let a = self.get_reg16(REG_AX as u8);
                let b = self.get_reg16(n);
                self.set_reg16(REG_AX as u8, b);
                self.set_reg16(n, a);
                3
            }
            // CBW
            0x98 => {
                let al = self.get_reg8(0) as i8 as i16 as u16;
                self.set_reg16(REG_AX as u8, al);
                2
            }
            // CWD
            0x99 => {
                let ax = self.get_reg16(REG_AX as u8);
                self.set_reg16(REG_DX as u8, if ax & 0x8000 != 0 { 0xFFFF } else { 0 });
                5
            }
            // CALL far ptr16:16
            0x9A => {
                let new_ip = self.fetch16(bus);
                let new_cs = self.fetch16(bus);
                self.push16(bus, self.seg[SEG_CS]);
                self.push16(bus, self.ip);
                self.seg[SEG_CS] = new_cs;
                self.ip = new_ip;
                28
            }
            // WAIT
            0x9B => 4,
            // PUSHF
            0x9C => {
                let f = self.flags | FLAGS_RESERVED_ON;
                self.push16(bus, f);
                10
            }
            // POPF
            0x9D => {
                let v = self.pop16(bus);
                self.flags = (v & !FLAGS_RESERVED_ON) | FLAGS_RESERVED_ON;
                8
            }
            // SAHF
            0x9E => {
                let ah = self.get_reg8(4);
                self.flags = (self.flags & 0xFF00)
                    | ((ah as u16) & (F_CF | F_PF | F_AF | F_ZF | F_SF))
                    | 0x02;
                4
            }
            // LAHF
            0x9F => {
                let lo = (self.flags & (F_CF | F_PF | F_AF | F_ZF | F_SF)) as u8 | 0x02;
                self.set_reg8(4, lo);
                4
            }

            // ---- MOV AL/AX, moffs and back (0xA0-0xA3) ----
            0xA0 => {
                let off = self.fetch16(bus);
                let seg = self.data_seg(ov, SEG_DS);
                let v = bus.read8(Self::phys(seg, off));
                self.set_reg8(0, v);
                10
            }
            0xA1 => {
                let off = self.fetch16(bus);
                let seg = self.data_seg(ov, SEG_DS);
                let v = bus.read16(Self::phys(seg, off));
                self.set_reg16(REG_AX as u8, v);
                10
            }
            0xA2 => {
                let off = self.fetch16(bus);
                let seg = self.data_seg(ov, SEG_DS);
                let v = self.get_reg8(0);
                bus.write8(Self::phys(seg, off), v);
                10
            }
            0xA3 => {
                let off = self.fetch16(bus);
                let seg = self.data_seg(ov, SEG_DS);
                let v = self.get_reg16(REG_AX as u8);
                bus.write16(Self::phys(seg, off), v);
                10
            }

            // ---- string ops (0xA4-0xAF) ----
            0xA4 => self.string_op(bus, ov, rep, StrOp::Movs, false),
            0xA5 => self.string_op(bus, ov, rep, StrOp::Movs, true),
            0xA6 => self.string_op(bus, ov, rep, StrOp::Cmps, false),
            0xA7 => self.string_op(bus, ov, rep, StrOp::Cmps, true),
            0xA8 => {
                let imm = self.fetch8(bus);
                let a = self.get_reg8(0);
                self.and8(a, imm);
                4
            }
            0xA9 => {
                let imm = self.fetch16(bus);
                let a = self.get_reg16(REG_AX as u8);
                self.and16(a, imm);
                4
            }
            0xAA => self.string_op(bus, ov, rep, StrOp::Stos, false),
            0xAB => self.string_op(bus, ov, rep, StrOp::Stos, true),
            0xAC => self.string_op(bus, ov, rep, StrOp::Lods, false),
            0xAD => self.string_op(bus, ov, rep, StrOp::Lods, true),
            0xAE => self.string_op(bus, ov, rep, StrOp::Scas, false),
            0xAF => self.string_op(bus, ov, rep, StrOp::Scas, true),

            // ---- MOV reg8, imm8 (0xB0-0xB7) ----
            0xB0..=0xB7 => {
                let imm = self.fetch8(bus);
                self.set_reg8((op - 0xB0) as u8, imm);
                4
            }
            // ---- MOV reg16, imm16 (0xB8-0xBF) ----
            0xB8..=0xBF => {
                let imm = self.fetch16(bus);
                self.set_reg16((op - 0xB8) as u8, imm);
                4
            }

            // ---- group2 shifts/rotates by imm8 (80186) and by 1 / by CL ----
            0xC0 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let cnt = self.fetch8(bus);
                let a = self.read_op8(bus, rm);
                let r = self.shift8(reg, a, cnt);
                self.write_op8(bus, rm, r);
                17
            }
            0xC1 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let cnt = self.fetch8(bus);
                let a = self.read_op16(bus, rm);
                let r = self.shift16(reg, a, cnt);
                self.write_op16(bus, rm, r);
                17
            }
            // ---- RET near (imm16 / no imm) ----
            0xC2 => {
                let n = self.fetch16(bus);
                self.ip = self.pop16(bus);
                self.r[REG_SP] = self.r[REG_SP].wrapping_add(n);
                20
            }
            0xC3 => {
                self.ip = self.pop16(bus);
                16
            }
            // LES / LDS reg16, m32
            0xC4 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                if let Operand::Mem(a) = rm {
                    let off = bus.read16(a);
                    let seg = bus.read16((a + 2) & crate::bus::ADDR_MASK);
                    self.set_reg16(reg, off);
                    self.seg[SEG_ES] = seg;
                }
                16
            }
            0xC5 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                if let Operand::Mem(a) = rm {
                    let off = bus.read16(a);
                    let seg = bus.read16((a + 2) & crate::bus::ADDR_MASK);
                    self.set_reg16(reg, off);
                    self.seg[SEG_DS] = seg;
                }
                16
            }
            // MOV r/m, imm
            0xC6 => {
                let modrm = self.fetch8(bus);
                let (rm, _reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch8(bus);
                self.write_op8(bus, rm, imm);
                10
            }
            0xC7 => {
                let modrm = self.fetch8(bus);
                let (rm, _reg) = self.decode_modrm(bus, modrm, ov);
                let imm = self.fetch16(bus);
                self.write_op16(bus, rm, imm);
                10
            }
            // ENTER imm16, imm8 (80186)
            0xC8 => {
                let alloc = self.fetch16(bus);
                let level = self.fetch8(bus) & 0x1F;
                self.push16(bus, self.r[REG_BP]);
                let frame = self.r[REG_SP];
                for _ in 1..level {
                    self.r[REG_BP] = self.r[REG_BP].wrapping_sub(2);
                    let v = bus.read16(Self::phys(self.seg[SEG_SS], self.r[REG_BP]));
                    self.push16(bus, v);
                }
                if level > 0 {
                    self.push16(bus, frame);
                }
                self.r[REG_BP] = frame;
                self.r[REG_SP] = self.r[REG_SP].wrapping_sub(alloc);
                15
            }
            // LEAVE (80186)
            0xC9 => {
                self.r[REG_SP] = self.r[REG_BP];
                self.r[REG_BP] = self.pop16(bus);
                8
            }
            // RET far
            0xCA => {
                let n = self.fetch16(bus);
                self.ip = self.pop16(bus);
                self.seg[SEG_CS] = self.pop16(bus);
                self.r[REG_SP] = self.r[REG_SP].wrapping_add(n);
                25
            }
            0xCB => {
                self.ip = self.pop16(bus);
                self.seg[SEG_CS] = self.pop16(bus);
                26
            }
            // INT3 / INT imm8 / INTO
            0xCC => {
                self.interrupt(bus, 3);
                52
            }
            0xCD => {
                let v = self.fetch8(bus);
                self.interrupt(bus, v);
                51
            }
            0xCE => {
                if self.flag(F_OF) {
                    self.interrupt(bus, 4);
                    53
                } else {
                    4
                }
            }
            // IRET
            0xCF => {
                self.ip = self.pop16(bus);
                self.seg[SEG_CS] = self.pop16(bus);
                let f = self.pop16(bus);
                self.flags = (f & !FLAGS_RESERVED_ON) | FLAGS_RESERVED_ON;
                32
            }

            // ---- group2 shifts by 1 / by CL ----
            0xD0 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op8(bus, rm);
                let r = self.shift8(reg, a, 1);
                self.write_op8(bus, rm, r);
                15
            }
            0xD1 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op16(bus, rm);
                let r = self.shift16(reg, a, 1);
                self.write_op16(bus, rm, r);
                15
            }
            0xD2 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let cnt = self.get_reg8(1); // CL
                let a = self.read_op8(bus, rm);
                let r = self.shift8(reg, a, cnt);
                self.write_op8(bus, rm, r);
                20
            }
            0xD3 => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let cnt = self.get_reg8(1);
                let a = self.read_op16(bus, rm);
                let r = self.shift16(reg, a, cnt);
                self.write_op16(bus, rm, r);
                20
            }
            // AAM / AAD
            0xD4 => {
                let base = self.fetch8(bus).max(1);
                let al = self.get_reg8(0);
                self.set_reg8(4, al / base);
                let rem = al % base;
                self.set_reg8(0, rem);
                self.set_pzs8(rem);
                83
            }
            0xD5 => {
                let base = self.fetch8(bus);
                let al = self.get_reg8(0);
                let ah = self.get_reg8(4);
                let r = al.wrapping_add(ah.wrapping_mul(base));
                self.set_reg8(0, r);
                self.set_reg8(4, 0);
                self.set_pzs8(r);
                60
            }
            // 0xD6 SALC (undocumented): AL = CF ? 0xFF : 0
            0xD6 => {
                self.set_reg8(0, if self.flag(F_CF) { 0xFF } else { 0 });
                4
            }
            // XLAT
            0xD7 => {
                let seg = self.data_seg(ov, SEG_DS);
                let off = self.r[REG_BX].wrapping_add(self.get_reg8(0) as u16);
                let v = bus.read8(Self::phys(seg, off));
                self.set_reg8(0, v);
                11
            }
            // 0xD8-0xDF: FPU (ESC) — no FPU on V30MZ; consume the modrm and ignore
            0xD8..=0xDF => {
                let modrm = self.fetch8(bus);
                let _ = self.decode_modrm(bus, modrm, ov);
                2
            }

            // ---- LOOP / LOOPE / LOOPNE / JCXZ ----
            0xE0 => {
                let disp = self.fetch8(bus) as i8 as i16;
                self.r[REG_CX] = self.r[REG_CX].wrapping_sub(1);
                if self.r[REG_CX] != 0 && !self.flag(F_ZF) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                    19
                } else {
                    5
                }
            }
            0xE1 => {
                let disp = self.fetch8(bus) as i8 as i16;
                self.r[REG_CX] = self.r[REG_CX].wrapping_sub(1);
                if self.r[REG_CX] != 0 && self.flag(F_ZF) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                    18
                } else {
                    6
                }
            }
            0xE2 => {
                let disp = self.fetch8(bus) as i8 as i16;
                self.r[REG_CX] = self.r[REG_CX].wrapping_sub(1);
                if self.r[REG_CX] != 0 {
                    self.ip = self.ip.wrapping_add(disp as u16);
                    17
                } else {
                    5
                }
            }
            0xE3 => {
                let disp = self.fetch8(bus) as i8 as i16;
                if self.r[REG_CX] == 0 {
                    self.ip = self.ip.wrapping_add(disp as u16);
                    18
                } else {
                    6
                }
            }
            // IN/OUT imm8
            0xE4 => {
                let port = self.fetch8(bus) as u16;
                let v = bus.port_in8(port);
                self.set_reg8(0, v);
                10
            }
            0xE5 => {
                let port = self.fetch8(bus) as u16;
                let v = bus.port_in16(port);
                self.set_reg16(REG_AX as u8, v);
                10
            }
            0xE6 => {
                let port = self.fetch8(bus) as u16;
                let v = self.get_reg8(0);
                bus.port_out8(port, v);
                10
            }
            0xE7 => {
                let port = self.fetch8(bus) as u16;
                let v = self.get_reg16(REG_AX as u8);
                bus.port_out16(port, v);
                10
            }
            // CALL near rel16
            0xE8 => {
                let disp = self.fetch16(bus) as i16;
                self.push16(bus, self.ip);
                self.ip = self.ip.wrapping_add(disp as u16);
                19
            }
            // JMP near rel16
            0xE9 => {
                let disp = self.fetch16(bus) as i16;
                self.ip = self.ip.wrapping_add(disp as u16);
                15
            }
            // JMP far ptr16:16
            0xEA => {
                let new_ip = self.fetch16(bus);
                let new_cs = self.fetch16(bus);
                self.ip = new_ip;
                self.seg[SEG_CS] = new_cs;
                15
            }
            // JMP short rel8
            0xEB => {
                let disp = self.fetch8(bus) as i8 as i16;
                self.ip = self.ip.wrapping_add(disp as u16);
                15
            }
            // IN/OUT DX
            0xEC => {
                let port = self.get_reg16(REG_DX as u8);
                let v = bus.port_in8(port);
                self.set_reg8(0, v);
                8
            }
            0xED => {
                let port = self.get_reg16(REG_DX as u8);
                let v = bus.port_in16(port);
                self.set_reg16(REG_AX as u8, v);
                8
            }
            0xEE => {
                let port = self.get_reg16(REG_DX as u8);
                let v = self.get_reg8(0);
                bus.port_out8(port, v);
                8
            }
            0xEF => {
                let port = self.get_reg16(REG_DX as u8);
                let v = self.get_reg16(REG_AX as u8);
                bus.port_out16(port, v);
                8
            }

            // 0xF1 — INT1 / undocumented; treat as NOP
            0xF1 => 2,
            // HLT
            0xF4 => {
                self.halted = true;
                2
            }
            // CMC
            0xF5 => {
                let cf = self.flag(F_CF);
                self.set_flag(F_CF, !cf);
                2
            }
            // group3: TEST/NOT/NEG/MUL/IMUL/DIV/IDIV r/m8
            0xF6 => self.group3_8(bus, ov),
            0xF7 => self.group3_16(bus, ov),
            // CLC/STC/CLI/STI/CLD/STD
            0xF8 => {
                self.set_flag(F_CF, false);
                2
            }
            0xF9 => {
                self.set_flag(F_CF, true);
                2
            }
            0xFA => {
                self.set_flag(F_IF, false);
                2
            }
            0xFB => {
                self.set_flag(F_IF, true);
                2
            }
            0xFC => {
                self.set_flag(F_DF, false);
                2
            }
            0xFD => {
                self.set_flag(F_DF, true);
                2
            }
            // group4: INC/DEC r/m8
            0xFE => {
                let modrm = self.fetch8(bus);
                let (rm, reg) = self.decode_modrm(bus, modrm, ov);
                let a = self.read_op8(bus, rm);
                let r = match reg & 7 {
                    0 => self.inc8(a),
                    1 => self.dec8(a),
                    _ => a,
                };
                self.write_op8(bus, rm, r);
                15
            }
            // group5: INC/DEC/CALL/CALLF/JMP/JMPF/PUSH r/m16
            0xFF => self.group5(bus, ov),

            // Any genuinely-unmapped opcode latches a fault for the crash screen.
            _ => {
                self.fault = Some((op, self.seg[SEG_CS], self.ip.wrapping_sub(1)));
                2
            }
        }
    }

    // ---- condition codes for Jcc / SETcc ----
    fn cond(&self, cc: u8) -> bool {
        let cf = self.flag(F_CF);
        let zf = self.flag(F_ZF);
        let sf = self.flag(F_SF);
        let of = self.flag(F_OF);
        let pf = self.flag(F_PF);
        match cc {
            0x0 => of,                 // JO
            0x1 => !of,                // JNO
            0x2 => cf,                 // JB/JC
            0x3 => !cf,                // JAE/JNC
            0x4 => zf,                 // JE/JZ
            0x5 => !zf,                // JNE/JNZ
            0x6 => cf || zf,           // JBE
            0x7 => !(cf || zf),        // JA
            0x8 => sf,                 // JS
            0x9 => !sf,                // JNS
            0xA => pf,                 // JP
            0xB => !pf,                // JNP
            0xC => sf != of,           // JL
            0xD => sf == of,           // JGE
            0xE => zf || (sf != of),   // JLE
            0xF => !zf && (sf == of),  // JG
            _ => false,
        }
    }

    // ---- shift/rotate group (reg field selects: ROL ROR RCL RCR SHL SHR SAL SAR)
    fn shift8(&mut self, reg: u8, val: u8, count: u8) -> u8 {
        let cnt = (count & 0x1F) % 9; // 8-bit rotate-through-carry cycles mod 9
        let n = count & 0x1F;
        if n == 0 {
            return val;
        }
        let mut v = val;
        match reg & 7 {
            0 => {
                // ROL
                let c = n & 7;
                v = (val << c) | (val >> ((8 - c) & 7));
                if c == 0 {
                    v = val;
                }
                self.set_flag(F_CF, v & 1 != 0);
                self.set_flag(F_OF, ((v >> 7) ^ (v & 1)) != 0);
            }
            1 => {
                // ROR
                let c = n & 7;
                v = (val >> c) | (val << ((8 - c) & 7));
                if c == 0 {
                    v = val;
                }
                self.set_flag(F_CF, v & 0x80 != 0);
                self.set_flag(F_OF, ((v >> 7) ^ ((v >> 6) & 1)) != 0);
            }
            2 => {
                // RCL through carry
                let mut carry = self.flag(F_CF) as u16;
                let mut x = val as u16;
                for _ in 0..cnt {
                    let newc = (x >> 7) & 1;
                    x = ((x << 1) | carry) & 0xFF;
                    carry = newc;
                }
                v = x as u8;
                self.set_flag(F_CF, carry != 0);
                self.set_flag(F_OF, (((v >> 7) as u16) ^ carry) != 0);
            }
            3 => {
                // RCR through carry
                let mut carry = self.flag(F_CF) as u16;
                let mut x = val as u16;
                for _ in 0..cnt {
                    let newc = x & 1;
                    x = (x >> 1) | (carry << 7);
                    carry = newc;
                }
                v = x as u8;
                self.set_flag(F_CF, carry != 0);
                self.set_flag(F_OF, (((v >> 7) ^ ((v >> 6) & 1)) & 1) != 0);
            }
            4 | 6 => {
                // SHL / SAL
                let res = (val as u16) << n;
                v = res as u8;
                self.set_flag(F_CF, res & 0x100 != 0);
                self.set_flag(F_OF, ((v >> 7) ^ (self.flag(F_CF) as u8)) != 0);
                self.set_pzs8(v);
            }
            5 => {
                // SHR
                let last = (val >> (n - 1)) & 1;
                v = val >> n;
                self.set_flag(F_CF, last != 0);
                self.set_flag(F_OF, val & 0x80 != 0);
                self.set_pzs8(v);
            }
            7 => {
                // SAR
                let sv = val as i8;
                let last = ((sv >> (n - 1)) & 1) as u8;
                v = (sv >> n.min(7)) as u8;
                self.set_flag(F_CF, last != 0);
                self.set_flag(F_OF, false);
                self.set_pzs8(v);
            }
            _ => {}
        }
        v
    }

    fn shift16(&mut self, reg: u8, val: u16, count: u8) -> u16 {
        let n = count & 0x1F;
        if n == 0 {
            return val;
        }
        let mut v = val;
        match reg & 7 {
            0 => {
                let c = n & 15;
                v = val.rotate_left(c as u32);
                self.set_flag(F_CF, v & 1 != 0);
                self.set_flag(F_OF, (((v >> 15) ^ (v & 1)) & 1) != 0);
            }
            1 => {
                let c = n & 15;
                v = val.rotate_right(c as u32);
                self.set_flag(F_CF, v & 0x8000 != 0);
                self.set_flag(F_OF, (((v >> 15) ^ ((v >> 14) & 1)) & 1) != 0);
            }
            2 => {
                let mut carry = self.flag(F_CF) as u32;
                let mut x = val as u32;
                for _ in 0..n {
                    let newc = (x >> 15) & 1;
                    x = ((x << 1) | carry) & 0xFFFF;
                    carry = newc;
                }
                v = x as u16;
                self.set_flag(F_CF, carry != 0);
                self.set_flag(F_OF, ((((v >> 15) as u32) ^ carry) & 1) != 0);
            }
            3 => {
                let mut carry = self.flag(F_CF) as u32;
                let mut x = val as u32;
                for _ in 0..n {
                    let newc = x & 1;
                    x = (x >> 1) | (carry << 15);
                    carry = newc;
                }
                v = x as u16;
                self.set_flag(F_CF, carry != 0);
                self.set_flag(F_OF, (((v >> 15) ^ ((v >> 14) & 1)) & 1) != 0);
            }
            4 | 6 => {
                let res = (val as u32) << n;
                v = res as u16;
                self.set_flag(F_CF, res & 0x10000 != 0);
                self.set_flag(F_OF, (((v >> 15) as u32 ^ (self.flag(F_CF) as u32)) & 1) != 0);
                self.set_pzs16(v);
            }
            5 => {
                let last = (val >> (n - 1)) & 1;
                v = val >> n;
                self.set_flag(F_CF, last != 0);
                self.set_flag(F_OF, val & 0x8000 != 0);
                self.set_pzs16(v);
            }
            7 => {
                let sv = val as i16;
                let last = ((sv >> (n - 1)) & 1) as u16;
                v = (sv >> n.min(15)) as u16;
                self.set_flag(F_CF, last != 0);
                self.set_flag(F_OF, false);
                self.set_pzs16(v);
            }
            _ => {}
        }
        v
    }

    // ---- group3 (0xF6 / 0xF7) ----
    fn group3_8(&mut self, bus: &mut dyn V30Bus, ov: SegOverride) -> u32 {
        let modrm = self.fetch8(bus);
        let (rm, reg) = self.decode_modrm(bus, modrm, ov);
        match reg & 7 {
            0 | 1 => {
                let imm = self.fetch8(bus);
                let a = self.read_op8(bus, rm);
                self.and8(a, imm);
                11
            }
            2 => {
                let a = self.read_op8(bus, rm);
                self.write_op8(bus, rm, !a);
                16
            }
            3 => {
                let a = self.read_op8(bus, rm);
                let r = self.sub8(0, a, 0);
                self.write_op8(bus, rm, r);
                16
            }
            4 => {
                // MUL
                let a = self.read_op8(bus, rm) as u16;
                let al = self.get_reg8(0) as u16;
                let r = a * al;
                self.set_reg16(REG_AX as u8, r);
                let hi = (r >> 8) != 0;
                self.set_flag(F_CF, hi);
                self.set_flag(F_OF, hi);
                self.set_pzs8(r as u8);
                70
            }
            5 => {
                // IMUL
                let a = self.read_op8(bus, rm) as i8 as i16;
                let al = self.get_reg8(0) as i8 as i16;
                let r = a * al;
                self.set_reg16(REG_AX as u8, r as u16);
                let ovf = r != (r as i8 as i16);
                self.set_flag(F_CF, ovf);
                self.set_flag(F_OF, ovf);
                80
            }
            6 => {
                // DIV
                let d = self.read_op8(bus, rm) as u16;
                if d == 0 {
                    self.interrupt(bus, 0);
                    return 80;
                }
                let ax = self.get_reg16(REG_AX as u8);
                let q = ax / d;
                let r = ax % d;
                if q > 0xFF {
                    self.interrupt(bus, 0);
                    return 80;
                }
                self.set_reg8(0, q as u8);
                self.set_reg8(4, r as u8);
                80
            }
            7 => {
                // IDIV
                let d = self.read_op8(bus, rm) as i8 as i16;
                if d == 0 {
                    self.interrupt(bus, 0);
                    return 101;
                }
                let ax = self.get_reg16(REG_AX as u8) as i16;
                let q = ax / d;
                let r = ax % d;
                if q > 127 || q < -128 {
                    self.interrupt(bus, 0);
                    return 101;
                }
                self.set_reg8(0, q as u8);
                self.set_reg8(4, r as u8);
                101
            }
            _ => unreachable!(),
        }
    }

    fn group3_16(&mut self, bus: &mut dyn V30Bus, ov: SegOverride) -> u32 {
        let modrm = self.fetch8(bus);
        let (rm, reg) = self.decode_modrm(bus, modrm, ov);
        match reg & 7 {
            0 | 1 => {
                let imm = self.fetch16(bus);
                let a = self.read_op16(bus, rm);
                self.and16(a, imm);
                11
            }
            2 => {
                let a = self.read_op16(bus, rm);
                self.write_op16(bus, rm, !a);
                16
            }
            3 => {
                let a = self.read_op16(bus, rm);
                let r = self.sub16(0, a, 0);
                self.write_op16(bus, rm, r);
                16
            }
            4 => {
                let a = self.read_op16(bus, rm) as u32;
                let ax = self.get_reg16(REG_AX as u8) as u32;
                let r = a * ax;
                self.set_reg16(REG_AX as u8, r as u16);
                self.set_reg16(REG_DX as u8, (r >> 16) as u16);
                let hi = (r >> 16) != 0;
                self.set_flag(F_CF, hi);
                self.set_flag(F_OF, hi);
                118
            }
            5 => {
                let a = self.read_op16(bus, rm) as i16 as i32;
                let ax = self.get_reg16(REG_AX as u8) as i16 as i32;
                let r = a * ax;
                self.set_reg16(REG_AX as u8, r as u16);
                self.set_reg16(REG_DX as u8, (r >> 16) as u16);
                let ovf = r != (r as i16 as i32);
                self.set_flag(F_CF, ovf);
                self.set_flag(F_OF, ovf);
                128
            }
            6 => {
                let d = self.read_op16(bus, rm) as u32;
                if d == 0 {
                    self.interrupt(bus, 0);
                    return 144;
                }
                let dxax = ((self.get_reg16(REG_DX as u8) as u32) << 16)
                    | self.get_reg16(REG_AX as u8) as u32;
                let q = dxax / d;
                let r = dxax % d;
                if q > 0xFFFF {
                    self.interrupt(bus, 0);
                    return 144;
                }
                self.set_reg16(REG_AX as u8, q as u16);
                self.set_reg16(REG_DX as u8, r as u16);
                144
            }
            7 => {
                let d = self.read_op16(bus, rm) as i16 as i32;
                if d == 0 {
                    self.interrupt(bus, 0);
                    return 165;
                }
                let dxax = (((self.get_reg16(REG_DX as u8) as u32) << 16)
                    | self.get_reg16(REG_AX as u8) as u32) as i32;
                let q = dxax / d;
                let r = dxax % d;
                if q > 32767 || q < -32768 {
                    self.interrupt(bus, 0);
                    return 165;
                }
                self.set_reg16(REG_AX as u8, q as u16);
                self.set_reg16(REG_DX as u8, r as u16);
                165
            }
            _ => unreachable!(),
        }
    }

    // ---- group5 (0xFF) ----
    fn group5(&mut self, bus: &mut dyn V30Bus, ov: SegOverride) -> u32 {
        let modrm = self.fetch8(bus);
        let (rm, reg) = self.decode_modrm(bus, modrm, ov);
        match reg & 7 {
            0 => {
                let a = self.read_op16(bus, rm);
                let r = self.inc16(a);
                self.write_op16(bus, rm, r);
                15
            }
            1 => {
                let a = self.read_op16(bus, rm);
                let r = self.dec16(a);
                self.write_op16(bus, rm, r);
                15
            }
            2 => {
                // CALL near r/m16
                let target = self.read_op16(bus, rm);
                self.push16(bus, self.ip);
                self.ip = target;
                21
            }
            3 => {
                // CALL far m16:16
                if let Operand::Mem(a) = rm {
                    let new_ip = bus.read16(a);
                    let new_cs = bus.read16((a + 2) & crate::bus::ADDR_MASK);
                    self.push16(bus, self.seg[SEG_CS]);
                    self.push16(bus, self.ip);
                    self.seg[SEG_CS] = new_cs;
                    self.ip = new_ip;
                }
                37
            }
            4 => {
                // JMP near r/m16
                let target = self.read_op16(bus, rm);
                self.ip = target;
                18
            }
            5 => {
                // JMP far m16:16
                if let Operand::Mem(a) = rm {
                    let new_ip = bus.read16(a);
                    let new_cs = bus.read16((a + 2) & crate::bus::ADDR_MASK);
                    self.seg[SEG_CS] = new_cs;
                    self.ip = new_ip;
                }
                24
            }
            6 => {
                // PUSH r/m16
                let v = self.read_op16(bus, rm);
                self.push16(bus, v);
                16
            }
            _ => 2,
        }
    }

    // ---- BCD adjust ----
    fn daa(&mut self) {
        let mut al = self.get_reg8(0);
        let old_cf = self.flag(F_CF);
        let mut cf = false;
        if (al & 0x0F) > 9 || self.flag(F_AF) {
            let (r, c) = al.overflowing_add(6);
            al = r;
            cf = old_cf || c;
            self.set_flag(F_AF, true);
        } else {
            self.set_flag(F_AF, false);
        }
        if al > 0x9F || old_cf {
            al = al.wrapping_add(0x60);
            cf = true;
        }
        self.set_flag(F_CF, cf);
        self.set_reg8(0, al);
        self.set_pzs8(al);
    }
    fn das(&mut self) {
        let mut al = self.get_reg8(0);
        let old_cf = self.flag(F_CF);
        let mut cf = false;
        if (al & 0x0F) > 9 || self.flag(F_AF) {
            let (r, b) = al.overflowing_sub(6);
            al = r;
            cf = old_cf || b;
            self.set_flag(F_AF, true);
        } else {
            self.set_flag(F_AF, false);
        }
        if al > 0x9F || old_cf {
            al = al.wrapping_sub(0x60);
            cf = true;
        }
        self.set_flag(F_CF, cf);
        self.set_reg8(0, al);
        self.set_pzs8(al);
    }
    fn aaa(&mut self) {
        let mut al = self.get_reg8(0);
        let mut ah = self.get_reg8(4);
        if (al & 0x0F) > 9 || self.flag(F_AF) {
            al = al.wrapping_add(6);
            ah = ah.wrapping_add(1);
            self.set_flag(F_AF, true);
            self.set_flag(F_CF, true);
        } else {
            self.set_flag(F_AF, false);
            self.set_flag(F_CF, false);
        }
        al &= 0x0F;
        self.set_reg8(0, al);
        self.set_reg8(4, ah);
    }
    fn aas(&mut self) {
        let mut al = self.get_reg8(0);
        let mut ah = self.get_reg8(4);
        if (al & 0x0F) > 9 || self.flag(F_AF) {
            al = al.wrapping_sub(6);
            ah = ah.wrapping_sub(1);
            self.set_flag(F_AF, true);
            self.set_flag(F_CF, true);
        } else {
            self.set_flag(F_AF, false);
            self.set_flag(F_CF, false);
        }
        al &= 0x0F;
        self.set_reg8(0, al);
        self.set_reg8(4, ah);
    }

    // ---- string operations with REP handling ----
    fn string_op(
        &mut self,
        bus: &mut dyn V30Bus,
        ov: SegOverride,
        rep: u8,
        kind: StrOp,
        word: bool,
    ) -> u32 {
        let step: u16 = if word { 2 } else { 1 };
        let delta = if self.flag(F_DF) {
            step.wrapping_neg()
        } else {
            step
        };
        // REP: iterate while CX != 0; for CMPS/SCAS also honor ZF condition.
        let uses_zf = matches!(kind, StrOp::Cmps | StrOp::Scas);
        let mut count = 1u32;
        let mut cycles = 0u32;
        loop {
            if rep != 0 {
                if self.r[REG_CX] == 0 {
                    break;
                }
            }
            self.one_string(bus, ov, kind, word, delta);
            cycles += 17;
            if rep != 0 {
                self.r[REG_CX] = self.r[REG_CX].wrapping_sub(1);
                if uses_zf {
                    let zf = self.flag(F_ZF);
                    // REP (0xF3) = REPE -> stop when ZF==0; REPNE (0xF2) -> stop when ZF==1
                    if (rep == 0xF3 && !zf) || (rep == 0xF2 && zf) {
                        break;
                    }
                }
                if self.r[REG_CX] == 0 {
                    break;
                }
            } else {
                break;
            }
            count += 1;
            if count > 0x20000 {
                break; // safety
            }
        }
        cycles.max(17)
    }

    fn one_string(
        &mut self,
        bus: &mut dyn V30Bus,
        ov: SegOverride,
        kind: StrOp,
        word: bool,
        delta: u16,
    ) {
        let dseg = self.data_seg(ov, SEG_DS);
        let eseg = self.seg[SEG_ES];
        match kind {
            StrOp::Movs => {
                let s = Self::phys(dseg, self.r[REG_SI]);
                let d = Self::phys(eseg, self.r[REG_DI]);
                if word {
                    let v = bus.read16(s);
                    bus.write16(d, v);
                } else {
                    let v = bus.read8(s);
                    bus.write8(d, v);
                }
                self.r[REG_SI] = self.r[REG_SI].wrapping_add(delta);
                self.r[REG_DI] = self.r[REG_DI].wrapping_add(delta);
            }
            StrOp::Cmps => {
                let s = Self::phys(dseg, self.r[REG_SI]);
                let d = Self::phys(eseg, self.r[REG_DI]);
                if word {
                    let a = bus.read16(s);
                    let b = bus.read16(d);
                    self.sub16(a, b, 0);
                } else {
                    let a = bus.read8(s);
                    let b = bus.read8(d);
                    self.sub8(a, b, 0);
                }
                self.r[REG_SI] = self.r[REG_SI].wrapping_add(delta);
                self.r[REG_DI] = self.r[REG_DI].wrapping_add(delta);
            }
            StrOp::Stos => {
                let d = Self::phys(eseg, self.r[REG_DI]);
                if word {
                    let v = self.get_reg16(REG_AX as u8);
                    bus.write16(d, v);
                } else {
                    let v = self.get_reg8(0);
                    bus.write8(d, v);
                }
                self.r[REG_DI] = self.r[REG_DI].wrapping_add(delta);
            }
            StrOp::Lods => {
                let s = Self::phys(dseg, self.r[REG_SI]);
                if word {
                    let v = bus.read16(s);
                    self.set_reg16(REG_AX as u8, v);
                } else {
                    let v = bus.read8(s);
                    self.set_reg8(0, v);
                }
                self.r[REG_SI] = self.r[REG_SI].wrapping_add(delta);
            }
            StrOp::Scas => {
                let d = Self::phys(eseg, self.r[REG_DI]);
                if word {
                    let a = self.get_reg16(REG_AX as u8);
                    let b = bus.read16(d);
                    self.sub16(a, b, 0);
                } else {
                    let a = self.get_reg8(0);
                    let b = bus.read8(d);
                    self.sub8(a, b, 0);
                }
                self.r[REG_DI] = self.r[REG_DI].wrapping_add(delta);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum StrOp {
    Movs,
    Cmps,
    Stos,
    Lods,
    Scas,
}

// =============================================================================
// Unit tests: a flat-RAM bus stub exercises opcode behavior, flags, ModR/M
// decoding, segmentation, and string ops.
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    struct FlatBus {
        mem: Vec<u8>,
        ports: [u8; 0x10000],
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus {
                mem: vec![0u8; 0x100000],
                ports: [0u8; 0x10000],
            }
        }
    }
    impl V30Bus for FlatBus {
        fn read8(&mut self, addr: u32) -> u8 {
            self.mem[(addr & 0xFFFFF) as usize]
        }
        fn write8(&mut self, addr: u32, v: u8) {
            self.mem[(addr & 0xFFFFF) as usize] = v;
        }
        fn port_in8(&mut self, port: u16) -> u8 {
            self.ports[port as usize]
        }
        fn port_out8(&mut self, port: u16, v: u8) {
            self.ports[port as usize] = v;
        }
    }

    /// Build a CPU whose CS=0, IP=0, and load a program at physical 0.
    fn run(prog: &[u8], setup: impl FnOnce(&mut Cpu)) -> (Cpu, FlatBus) {
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SP] = 0xFFF0;
        let mut bus = FlatBus::new();
        bus.mem[..prog.len()].copy_from_slice(prog);
        setup(&mut cpu);
        cpu
            .clone();
        // Execute instructions until we reach the end of the program.
        let end = prog.len() as u16;
        let mut guard = 0;
        while cpu.ip < end && guard < 10000 {
            cpu.step(&mut bus);
            guard += 1;
        }
        (cpu, bus)
    }

    #[test]
    fn mov_imm_to_reg16() {
        // MOV AX, 0x1234
        let (cpu, _) = run(&[0xB8, 0x34, 0x12], |_| {});
        assert_eq!(cpu.r[REG_AX], 0x1234);
    }

    #[test]
    fn mov_imm_to_reg8_halves() {
        // MOV AL,0xAA ; MOV AH,0xBB
        let (cpu, _) = run(&[0xB0, 0xAA, 0xB4, 0xBB], |_| {});
        assert_eq!(cpu.r[REG_AX], 0xBBAA);
    }

    #[test]
    fn add_sets_flags() {
        // MOV AL,0xFF ; ADD AL,1  -> 0, CF=1, ZF=1
        let (cpu, _) = run(&[0xB0, 0xFF, 0x04, 0x01], |_| {});
        assert_eq!(cpu.get_reg8(0), 0);
        assert!(cpu.flag(F_CF));
        assert!(cpu.flag(F_ZF));
        assert!(!cpu.flag(F_SF));
    }

    #[test]
    fn sub_overflow_flag() {
        // MOV AL,0x80 ; SUB AL,1 -> 0x7F, OF=1 (signed overflow), SF=0
        let (cpu, _) = run(&[0xB0, 0x80, 0x2C, 0x01], |_| {});
        assert_eq!(cpu.get_reg8(0), 0x7F);
        assert!(cpu.flag(F_OF));
        assert!(!cpu.flag(F_SF));
    }

    #[test]
    fn cmp_sets_zero_no_writeback() {
        // MOV AX,0x1234 ; CMP AX,0x1234 -> ZF=1, AX unchanged
        let (cpu, _) = run(&[0xB8, 0x34, 0x12, 0x3D, 0x34, 0x12], |_| {});
        assert_eq!(cpu.r[REG_AX], 0x1234);
        assert!(cpu.flag(F_ZF));
    }

    #[test]
    fn modrm_reg_to_reg() {
        // MOV BX,0xBEEF ; MOV AX,BX  (89 D8 = MOV r/m16=AX, reg16=BX)
        let (cpu, _) = run(&[0xBB, 0xEF, 0xBE, 0x89, 0xD8], |_| {});
        assert_eq!(cpu.r[REG_AX], 0xBEEF);
    }

    #[test]
    fn modrm_mem_store_load() {
        // MOV [0x0100], AX where AX=0xCAFE, then MOV BX,[0x0100]
        // B8 FE CA  MOV AX,0xCAFE
        // A3 00 01  MOV [0x0100],AX
        // 8B 1E 00 01  MOV BX,[0x0100]
        let prog = [0xB8, 0xFE, 0xCA, 0xA3, 0x00, 0x01, 0x8B, 0x1E, 0x00, 0x01];
        let (cpu, bus) = run(&prog, |_| {});
        assert_eq!(bus.mem[0x0100], 0xFE);
        assert_eq!(bus.mem[0x0101], 0xCA);
        assert_eq!(cpu.r[REG_BX], 0xCAFE);
    }

    #[test]
    fn segmentation_applies_base() {
        // DS=0x1000 so [0x0010] resolves to physical 0x10010.
        // MOV AX,0x5599 ; MOV [0x0010],AX
        let prog = [0xB8, 0x99, 0x55, 0xA3, 0x10, 0x00];
        let (_, bus) = run(&prog, |c| c.seg[SEG_DS] = 0x1000);
        assert_eq!(bus.mem[0x10010], 0x99);
        assert_eq!(bus.mem[0x10011], 0x55);
    }

    #[test]
    fn segment_override_prefix() {
        // ES=0x2000; ES: MOV [0x0004],AL with AL=0x7E -> phys 0x20004
        // B0 7E       MOV AL,0x7E
        // 26 A2 04 00 ES: MOV [0x0004],AL
        let prog = [0xB0, 0x7E, 0x26, 0xA2, 0x04, 0x00];
        let (_, bus) = run(&prog, |c| c.seg[SEG_ES] = 0x2000);
        assert_eq!(bus.mem[0x20004], 0x7E);
    }

    #[test]
    fn push_pop_roundtrip() {
        // MOV AX,0xABCD ; PUSH AX ; POP BX
        let prog = [0xB8, 0xCD, 0xAB, 0x50, 0x5B];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.r[REG_BX], 0xABCD);
    }

    #[test]
    fn conditional_jump_taken() {
        // MOV AL,0 ; CMP AL,0 (sets ZF) ; JZ +2 ; MOV BL,1 ; MOV CL,2
        // If JZ taken, BL stays 0, CL=2.
        // B0 00 ; 3C 00 ; 74 02 ; B3 01 ; B1 02
        let prog = [0xB0, 0x00, 0x3C, 0x00, 0x74, 0x02, 0xB3, 0x01, 0xB1, 0x02];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.get_reg8(3), 0); // BL untouched (jump skipped it)
        assert_eq!(cpu.get_reg8(1), 2); // CL=2
    }

    #[test]
    fn call_ret_near() {
        // CALL +1 (E8 01 00) lands past a filler; RET (C3) returns.
        // 0: E8 01 00  CALL 0x0004
        // 3: F4        HLT  (should be skipped)
        // 4: C3        RET
        let mut bus = FlatBus::new();
        let prog = [0xE8, 0x01, 0x00, 0xF4, 0xC3];
        bus.mem[..prog.len()].copy_from_slice(&prog);
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SP] = 0xFFF0;
        cpu.step(&mut bus); // CALL -> ip=4, pushed return 3
        assert_eq!(cpu.ip, 4);
        cpu.step(&mut bus); // RET -> ip=3
        assert_eq!(cpu.ip, 3);
        assert!(!cpu.halted);
    }

    #[test]
    fn loop_decrements_cx() {
        // MOV CX,3 ; (loop body: INC AL) LOOP back
        // B9 03 00     MOV CX,3
        // FE C0        INC AL
        // E2 FC        LOOP -4 (back to INC AL)
        let prog = [0xB9, 0x03, 0x00, 0xFE, 0xC0, 0xE2, 0xFC];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.r[REG_CX], 0);
        assert_eq!(cpu.get_reg8(0), 3); // incremented 3 times
    }

    #[test]
    fn rep_movsb_copies() {
        // Copy 4 bytes from DS:SI=0x0200 to ES:DI=0x0300.
        // setup data, then: MOV CX,4 ; REP MOVSB
        let mut bus = FlatBus::new();
        for i in 0..4 {
            bus.mem[0x0200 + i] = (0x10 + i) as u8;
        }
        let prog = [0xB9, 0x04, 0x00, 0xF3, 0xA4]; // MOV CX,4 ; REP MOVSB
        bus.mem[..prog.len()].copy_from_slice(&prog);
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SI] = 0x0200;
        cpu.r[REG_DI] = 0x0300;
        cpu.r[REG_SP] = 0xFFF0;
        let end = prog.len() as u16;
        while cpu.ip < end {
            cpu.step(&mut bus);
        }
        assert_eq!(&bus.mem[0x0300..0x0304], &[0x10, 0x11, 0x12, 0x13]);
        assert_eq!(cpu.r[REG_CX], 0);
    }

    #[test]
    fn shift_left_carry() {
        // MOV AL,0x80 ; SHL AL,1 -> 0x00, CF=1
        // B0 80 ; D0 E0  (D0 /4 = SHL r/m8,1)
        let prog = [0xB0, 0x80, 0xD0, 0xE0];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.get_reg8(0), 0);
        assert!(cpu.flag(F_CF));
    }

    #[test]
    fn mul_byte() {
        // MOV AL,0x10 ; MOV BL,0x10 ; MUL BL -> AX=0x100, CF=1
        // B0 10 ; B3 10 ; F6 E3 (F6 /4 = MUL r/m8, rm=BL)
        let prog = [0xB0, 0x10, 0xB3, 0x10, 0xF6, 0xE3];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.r[REG_AX], 0x0100);
        assert!(cpu.flag(F_CF));
    }

    #[test]
    fn div_word() {
        // DX:AX = 0x00010000 / BX=0x0002 -> AX=0x8000, DX=0
        // MOV DX,1 ; MOV AX,0 ; MOV BX,2 ; DIV BX (F7 F3)
        let prog = [
            0xBA, 0x01, 0x00, 0xB8, 0x00, 0x00, 0xBB, 0x02, 0x00, 0xF7, 0xF3,
        ];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.r[REG_AX], 0x8000);
        assert_eq!(cpu.r[REG_DX], 0);
    }

    #[test]
    fn inc_preserves_carry() {
        // STC ; MOV AL,5 ; INC AL -> CF still set (INC doesn't touch CF)
        // F9 ; B0 05 ; FE C0
        let prog = [0xF9, 0xB0, 0x05, 0xFE, 0xC0];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.get_reg8(0), 6);
        assert!(cpu.flag(F_CF));
    }

    #[test]
    fn flags_pushf_popf() {
        // STC ; PUSHF ; CLC ; POPF -> CF restored to 1
        // F9 ; 9C ; F8 ; 9D
        let prog = [0xF9, 0x9C, 0xF8, 0x9D];
        let (cpu, _) = run(&prog, |_| {});
        assert!(cpu.flag(F_CF));
    }

    #[test]
    fn io_ports() {
        // MOV AL,0x55 ; OUT 0x10,AL ; (clear AL) MOV AL,0 ; IN AL,0x10
        // B0 55 ; E6 10 ; B0 00 ; E4 10
        let prog = [0xB0, 0x55, 0xE6, 0x10, 0xB0, 0x00, 0xE4, 0x10];
        let (cpu, bus) = run(&prog, |_| {});
        assert_eq!(bus.ports[0x10], 0x55);
        assert_eq!(cpu.get_reg8(0), 0x55);
    }

    #[test]
    fn software_interrupt_and_iret() {
        // IVT[0x20] -> CS:IP = 0x0000:0x0010. Handler at 0x10: STC ; IRET.
        // Program: INT 0x20 (CD 20) at 0; then HLT at 2.
        let mut bus = FlatBus::new();
        bus.mem[0] = 0xCD;
        bus.mem[1] = 0x20;
        bus.mem[2] = 0xF4; // HLT after return
        // IVT entry 0x20 at phys 0x80: ip=0x10, cs=0
        bus.mem[0x80] = 0x10;
        bus.mem[0x81] = 0x00;
        bus.mem[0x82] = 0x00;
        bus.mem[0x83] = 0x00;
        // Handler
        bus.mem[0x10] = 0xF9; // STC
        bus.mem[0x11] = 0xCF; // IRET
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SP] = 0xFFF0;
        cpu.step(&mut bus); // INT 0x20
        assert_eq!(cpu.ip, 0x10);
        cpu.step(&mut bus); // STC
        cpu.step(&mut bus); // IRET -> back to ip=2
        assert_eq!(cpu.ip, 2);
        // IRET restores the FLAGS pushed at INT time (CF was 0), discarding the
        // handler's STC, so CF is now clear.
        assert!(!cpu.flag(F_CF));
    }

    #[test]
    fn xor_self_zeroes() {
        // MOV AX,0x1234 ; XOR AX,AX -> 0, ZF=1
        // B8 34 12 ; 31 C0
        let prog = [0xB8, 0x34, 0x12, 0x31, 0xC0];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.r[REG_AX], 0);
        assert!(cpu.flag(F_ZF));
    }

    #[test]
    fn neg_sets_carry() {
        // MOV AL,1 ; NEG AL (F6 D8) -> 0xFF, CF=1
        let prog = [0xB0, 0x01, 0xF6, 0xD8];
        let (cpu, _) = run(&prog, |_| {});
        assert_eq!(cpu.get_reg8(0), 0xFF);
        assert!(cpu.flag(F_CF));
    }

    #[test]
    fn group1_imm_to_mem() {
        // ADD byte [0x0100], 5 ; preset mem=10 -> 15
        // 80 06 00 01 05  (80 /0 mem disp16 imm8)
        let mut bus = FlatBus::new();
        bus.mem[0x100] = 10;
        let prog = [0x80, 0x06, 0x00, 0x01, 0x05];
        bus.mem[..prog.len()].copy_from_slice(&prog);
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SP] = 0xFFF0;
        cpu.step(&mut bus);
        assert_eq!(bus.mem[0x100], 15);
    }

    #[test]
    fn halt_then_irq_wakes() {
        // HLT, then assert IRQ with IF set -> CPU wakes and vectors.
        let mut bus = FlatBus::new();
        bus.mem[0] = 0xFB; // STI
        bus.mem[1] = 0xF4; // HLT
        // IVT[8] (vector 8) at phys 0x20 -> ip=0x40
        bus.mem[0x20] = 0x40;
        let mut cpu = Cpu::new();
        cpu.seg = [0, 0, 0, 0];
        cpu.ip = 0;
        cpu.r[REG_SP] = 0xFFF0;
        cpu.step(&mut bus); // STI
        cpu.step(&mut bus); // HLT
        assert!(cpu.halted);
        cpu.irq_line = true;
        cpu.irq_vector = 8;
        cpu.step(&mut bus); // wakes, vectors
        assert!(!cpu.halted);
        assert_eq!(cpu.ip, 0x40);
    }

    #[test]
    fn undefined_opcode_faults() {
        // 0x64 is unmapped (FS prefix doesn't exist on V30) -> fault latched.
        let prog = [0x64];
        let (cpu, _) = run(&prog, |_| {});
        assert!(cpu.fault.is_some());
    }
}
