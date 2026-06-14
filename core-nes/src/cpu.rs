//! Ricoh 2A03 CPU core — a 6502 with decimal mode disabled.
//!
//! Spec: NESdev wiki "CPU", "6502 instruction reference", "CPU unofficial
//! opcodes". Implements every official opcode + addressing mode with correct
//! cycle counts (including page-cross and branch-taken penalties), the
//! NMI/IRQ/RESET sequences, and the common unofficial opcodes (LAX, SAX, DCP,
//! ISC, SLO, RLA, SRE, RRA, plus the NOP variants) that many test ROMs use.
//!
//! The CPU drives memory through `&mut dyn Bus` (see `crate::bus::Bus`), so it
//! never knows which device backs an address. `step()` executes one
//! instruction and returns the cycle count it consumed; the orchestrator
//! advances the PPU/APU by 3 dots per CPU cycle.

use crate::bus::Bus;

// Status flags.
pub const FLAG_C: u8 = 1 << 0; // carry
pub const FLAG_Z: u8 = 1 << 1; // zero
pub const FLAG_I: u8 = 1 << 2; // interrupt disable
pub const FLAG_D: u8 = 1 << 3; // decimal (no effect on 2A03)
pub const FLAG_B: u8 = 1 << 4; // break (only meaningful on the stack copy)
pub const FLAG_U: u8 = 1 << 5; // unused, always 1
pub const FLAG_V: u8 = 1 << 6; // overflow
pub const FLAG_N: u8 = 1 << 7; // negative

const STACK_BASE: u16 = 0x0100;
const NMI_VECTOR: u16 = 0xFFFA;
const RESET_VECTOR: u16 = 0xFFFC;
const IRQ_VECTOR: u16 = 0xFFFE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Imm,  // immediate
    Zp,   // zero page
    Zpx,  // zero page,X
    Zpy,  // zero page,Y
    Abs,  // absolute
    Abx,  // absolute,X
    Aby,  // absolute,Y
    Ind,  // indirect (JMP)
    Izx,  // (indirect,X)
    Izy,  // (indirect),Y
    Rel,  // relative (branch)
}

pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub status: u8,

    /// Pending interrupt lines (level-sensitive IRQ, edge NMI). The orchestrator
    /// raises these; the CPU samples them between instructions.
    pub nmi_pending: bool,
    pub irq_line: bool,

    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            status: FLAG_I | FLAG_U,
            nmi_pending: false,
            irq_line: false,
            cycles: 0,
        }
    }

    /// Power-on / RESET: load PC from the reset vector, set I, sp -= 3.
    pub fn reset(&mut self, bus: &mut dyn Bus) {
        let lo = bus.read8(RESET_VECTOR) as u16;
        let hi = bus.read8(RESET_VECTOR + 1) as u16;
        self.pc = (hi << 8) | lo;
        self.sp = 0xFD;
        self.status = FLAG_I | FLAG_U;
        self.cycles = 0;
    }

    // ---- flag helpers ----
    #[inline]
    fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.status |= f;
        } else {
            self.status &= !f;
        }
    }
    #[inline]
    fn flag(&self, f: u8) -> bool {
        self.status & f != 0
    }
    #[inline]
    fn set_zn(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
    }

    // ---- stack ----
    #[inline]
    fn push(&mut self, bus: &mut dyn Bus, v: u8) {
        bus.write8(STACK_BASE + self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }
    #[inline]
    fn pop(&mut self, bus: &mut dyn Bus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read8(STACK_BASE + self.sp as u16)
    }
    #[inline]
    fn push16(&mut self, bus: &mut dyn Bus, v: u16) {
        self.push(bus, (v >> 8) as u8);
        self.push(bus, (v & 0xFF) as u8);
    }
    #[inline]
    fn pop16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.pop(bus) as u16;
        let hi = self.pop(bus) as u16;
        (hi << 8) | lo
    }

    // ---- fetch ----
    #[inline]
    fn fetch8(&mut self, bus: &mut dyn Bus) -> u8 {
        let v = bus.read8(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }

    /// Resolve an addressing mode to an effective address. Returns the address
    /// and whether a page boundary was crossed (for the +1 cycle penalty on
    /// the read-only Abx/Aby/Izy forms).
    fn operand_addr(&mut self, bus: &mut dyn Bus, mode: Mode) -> (u16, bool) {
        match mode {
            Mode::Imm => {
                let a = self.pc;
                self.pc = self.pc.wrapping_add(1);
                (a, false)
            }
            Mode::Zp => (self.fetch8(bus) as u16, false),
            Mode::Zpx => (((self.fetch8(bus).wrapping_add(self.x)) as u16) & 0xFF, false),
            Mode::Zpy => (((self.fetch8(bus).wrapping_add(self.y)) as u16) & 0xFF, false),
            Mode::Abs => (self.fetch16(bus), false),
            Mode::Abx => {
                let base = self.fetch16(bus);
                let a = base.wrapping_add(self.x as u16);
                (a, page_crossed(base, a))
            }
            Mode::Aby => {
                let base = self.fetch16(bus);
                let a = base.wrapping_add(self.y as u16);
                (a, page_crossed(base, a))
            }
            Mode::Ind => {
                // JMP (indirect) with the 6502 page-wrap bug on the high byte.
                let ptr = self.fetch16(bus);
                let lo = bus.read8(ptr) as u16;
                let hi_addr = (ptr & 0xFF00) | ((ptr + 1) & 0x00FF);
                let hi = bus.read8(hi_addr) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Izx => {
                let zp = self.fetch8(bus).wrapping_add(self.x);
                let lo = bus.read8(zp as u16) as u16;
                let hi = bus.read8(zp.wrapping_add(1) as u16) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Izy => {
                let zp = self.fetch8(bus);
                let lo = bus.read8(zp as u16) as u16;
                let hi = bus.read8(zp.wrapping_add(1) as u16) as u16;
                let base = (hi << 8) | lo;
                let a = base.wrapping_add(self.y as u16);
                (a, page_crossed(base, a))
            }
            Mode::Rel => {
                let off = self.fetch8(bus) as i8 as i16;
                let a = (self.pc as i16).wrapping_add(off) as u16;
                (a, false)
            }
        }
    }

    /// Service a pending NMI or IRQ if one is latched. Returns extra cycles.
    fn poll_interrupts(&mut self, bus: &mut dyn Bus) -> u64 {
        if self.nmi_pending {
            self.nmi_pending = false;
            self.interrupt(bus, NMI_VECTOR, false);
            return 7;
        }
        if self.irq_line && !self.flag(FLAG_I) {
            self.interrupt(bus, IRQ_VECTOR, false);
            return 7;
        }
        0
    }

    fn interrupt(&mut self, bus: &mut dyn Bus, vector: u16, brk: bool) {
        self.push16(bus, self.pc);
        let mut p = self.status | FLAG_U;
        if brk {
            p |= FLAG_B;
        } else {
            p &= !FLAG_B;
        }
        self.push(bus, p);
        self.set_flag(FLAG_I, true);
        let lo = bus.read8(vector) as u16;
        let hi = bus.read8(vector + 1) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute one instruction (servicing interrupts first). Returns cycles
    /// consumed.
    pub fn step(&mut self, bus: &mut dyn Bus) -> u64 {
        let int_cycles = self.poll_interrupts(bus);
        if int_cycles > 0 {
            self.cycles += int_cycles;
            return int_cycles;
        }

        let opcode = self.fetch8(bus);
        let cyc = self.execute(bus, opcode);
        self.cycles += cyc;
        cyc
    }

    // The big decode. Returns base cycles + any page-cross/branch penalties.
    fn execute(&mut self, bus: &mut dyn Bus, op: u8) -> u64 {
        use Mode::*;
        match op {
            // ---- Loads ----
            0xA9 => self.ld(bus, Imm, Reg::A, 2),
            0xA5 => self.ld(bus, Zp, Reg::A, 3),
            0xB5 => self.ld(bus, Zpx, Reg::A, 4),
            0xAD => self.ld(bus, Abs, Reg::A, 4),
            0xBD => self.ld(bus, Abx, Reg::A, 4),
            0xB9 => self.ld(bus, Aby, Reg::A, 4),
            0xA1 => self.ld(bus, Izx, Reg::A, 6),
            0xB1 => self.ld(bus, Izy, Reg::A, 5),

            0xA2 => self.ld(bus, Imm, Reg::X, 2),
            0xA6 => self.ld(bus, Zp, Reg::X, 3),
            0xB6 => self.ld(bus, Zpy, Reg::X, 4),
            0xAE => self.ld(bus, Abs, Reg::X, 4),
            0xBE => self.ld(bus, Aby, Reg::X, 4),

            0xA0 => self.ld(bus, Imm, Reg::Y, 2),
            0xA4 => self.ld(bus, Zp, Reg::Y, 3),
            0xB4 => self.ld(bus, Zpx, Reg::Y, 4),
            0xAC => self.ld(bus, Abs, Reg::Y, 4),
            0xBC => self.ld(bus, Abx, Reg::Y, 4),

            // ---- Stores ----
            0x85 => self.st(bus, Zp, Reg::A, 3),
            0x95 => self.st(bus, Zpx, Reg::A, 4),
            0x8D => self.st(bus, Abs, Reg::A, 4),
            0x9D => self.st(bus, Abx, Reg::A, 5),
            0x99 => self.st(bus, Aby, Reg::A, 5),
            0x81 => self.st(bus, Izx, Reg::A, 6),
            0x91 => self.st(bus, Izy, Reg::A, 6),
            0x86 => self.st(bus, Zp, Reg::X, 3),
            0x96 => self.st(bus, Zpy, Reg::X, 4),
            0x8E => self.st(bus, Abs, Reg::X, 4),
            0x84 => self.st(bus, Zp, Reg::Y, 3),
            0x94 => self.st(bus, Zpx, Reg::Y, 4),
            0x8C => self.st(bus, Abs, Reg::Y, 4),

            // ---- Transfers ----
            0xAA => { self.x = self.a; self.set_zn(self.x); 2 }
            0xA8 => { self.y = self.a; self.set_zn(self.y); 2 }
            0x8A => { self.a = self.x; self.set_zn(self.a); 2 }
            0x98 => { self.a = self.y; self.set_zn(self.a); 2 }
            0xBA => { self.x = self.sp; self.set_zn(self.x); 2 }
            0x9A => { self.sp = self.x; 2 }

            // ---- Stack ----
            0x48 => { self.push(bus, self.a); 3 }
            0x68 => { let v = self.pop(bus); self.a = v; self.set_zn(v); 4 }
            0x08 => { self.push(bus, self.status | FLAG_U | FLAG_B); 3 }
            0x28 => {
                let v = self.pop(bus);
                self.status = (v & !FLAG_B) | FLAG_U;
                4
            }

            // ---- Logic ----
            0x29 => self.alu(bus, Imm, AluOp::And, 2),
            0x25 => self.alu(bus, Zp, AluOp::And, 3),
            0x35 => self.alu(bus, Zpx, AluOp::And, 4),
            0x2D => self.alu(bus, Abs, AluOp::And, 4),
            0x3D => self.alu(bus, Abx, AluOp::And, 4),
            0x39 => self.alu(bus, Aby, AluOp::And, 4),
            0x21 => self.alu(bus, Izx, AluOp::And, 6),
            0x31 => self.alu(bus, Izy, AluOp::And, 5),

            0x09 => self.alu(bus, Imm, AluOp::Ora, 2),
            0x05 => self.alu(bus, Zp, AluOp::Ora, 3),
            0x15 => self.alu(bus, Zpx, AluOp::Ora, 4),
            0x0D => self.alu(bus, Abs, AluOp::Ora, 4),
            0x1D => self.alu(bus, Abx, AluOp::Ora, 4),
            0x19 => self.alu(bus, Aby, AluOp::Ora, 4),
            0x01 => self.alu(bus, Izx, AluOp::Ora, 6),
            0x11 => self.alu(bus, Izy, AluOp::Ora, 5),

            0x49 => self.alu(bus, Imm, AluOp::Eor, 2),
            0x45 => self.alu(bus, Zp, AluOp::Eor, 3),
            0x55 => self.alu(bus, Zpx, AluOp::Eor, 4),
            0x4D => self.alu(bus, Abs, AluOp::Eor, 4),
            0x5D => self.alu(bus, Abx, AluOp::Eor, 4),
            0x59 => self.alu(bus, Aby, AluOp::Eor, 4),
            0x41 => self.alu(bus, Izx, AluOp::Eor, 6),
            0x51 => self.alu(bus, Izy, AluOp::Eor, 5),

            // ---- Arithmetic ----
            0x69 => self.alu(bus, Imm, AluOp::Adc, 2),
            0x65 => self.alu(bus, Zp, AluOp::Adc, 3),
            0x75 => self.alu(bus, Zpx, AluOp::Adc, 4),
            0x6D => self.alu(bus, Abs, AluOp::Adc, 4),
            0x7D => self.alu(bus, Abx, AluOp::Adc, 4),
            0x79 => self.alu(bus, Aby, AluOp::Adc, 4),
            0x61 => self.alu(bus, Izx, AluOp::Adc, 6),
            0x71 => self.alu(bus, Izy, AluOp::Adc, 5),

            0xE9 | 0xEB => self.alu(bus, Imm, AluOp::Sbc, 2),
            0xE5 => self.alu(bus, Zp, AluOp::Sbc, 3),
            0xF5 => self.alu(bus, Zpx, AluOp::Sbc, 4),
            0xED => self.alu(bus, Abs, AluOp::Sbc, 4),
            0xFD => self.alu(bus, Abx, AluOp::Sbc, 4),
            0xF9 => self.alu(bus, Aby, AluOp::Sbc, 4),
            0xE1 => self.alu(bus, Izx, AluOp::Sbc, 6),
            0xF1 => self.alu(bus, Izy, AluOp::Sbc, 5),

            // ---- Compares ----
            0xC9 => self.cmp(bus, Imm, Reg::A, 2),
            0xC5 => self.cmp(bus, Zp, Reg::A, 3),
            0xD5 => self.cmp(bus, Zpx, Reg::A, 4),
            0xCD => self.cmp(bus, Abs, Reg::A, 4),
            0xDD => self.cmp(bus, Abx, Reg::A, 4),
            0xD9 => self.cmp(bus, Aby, Reg::A, 4),
            0xC1 => self.cmp(bus, Izx, Reg::A, 6),
            0xD1 => self.cmp(bus, Izy, Reg::A, 5),
            0xE0 => self.cmp(bus, Imm, Reg::X, 2),
            0xE4 => self.cmp(bus, Zp, Reg::X, 3),
            0xEC => self.cmp(bus, Abs, Reg::X, 4),
            0xC0 => self.cmp(bus, Imm, Reg::Y, 2),
            0xC4 => self.cmp(bus, Zp, Reg::Y, 3),
            0xCC => self.cmp(bus, Abs, Reg::Y, 4),

            // ---- BIT ----
            0x24 => self.bit(bus, Zp, 3),
            0x2C => self.bit(bus, Abs, 4),

            // ---- Inc/Dec memory ----
            0xE6 => self.inc_dec_mem(bus, Zp, 1, 5),
            0xF6 => self.inc_dec_mem(bus, Zpx, 1, 6),
            0xEE => self.inc_dec_mem(bus, Abs, 1, 6),
            0xFE => self.inc_dec_mem(bus, Abx, 1, 7),
            0xC6 => self.inc_dec_mem(bus, Zp, -1, 5),
            0xD6 => self.inc_dec_mem(bus, Zpx, -1, 6),
            0xCE => self.inc_dec_mem(bus, Abs, -1, 6),
            0xDE => self.inc_dec_mem(bus, Abx, -1, 7),

            // ---- Inc/Dec register ----
            0xE8 => { self.x = self.x.wrapping_add(1); self.set_zn(self.x); 2 }
            0xCA => { self.x = self.x.wrapping_sub(1); self.set_zn(self.x); 2 }
            0xC8 => { self.y = self.y.wrapping_add(1); self.set_zn(self.y); 2 }
            0x88 => { self.y = self.y.wrapping_sub(1); self.set_zn(self.y); 2 }

            // ---- Shifts/rotates on accumulator ----
            0x0A => { self.a = self.asl(self.a); 2 }
            0x4A => { self.a = self.lsr(self.a); 2 }
            0x2A => { self.a = self.rol(self.a); 2 }
            0x6A => { self.a = self.ror(self.a); 2 }

            // ---- Shifts/rotates on memory ----
            0x06 => self.rmw(bus, Zp, ShiftOp::Asl, 5),
            0x16 => self.rmw(bus, Zpx, ShiftOp::Asl, 6),
            0x0E => self.rmw(bus, Abs, ShiftOp::Asl, 6),
            0x1E => self.rmw(bus, Abx, ShiftOp::Asl, 7),
            0x46 => self.rmw(bus, Zp, ShiftOp::Lsr, 5),
            0x56 => self.rmw(bus, Zpx, ShiftOp::Lsr, 6),
            0x4E => self.rmw(bus, Abs, ShiftOp::Lsr, 6),
            0x5E => self.rmw(bus, Abx, ShiftOp::Lsr, 7),
            0x26 => self.rmw(bus, Zp, ShiftOp::Rol, 5),
            0x36 => self.rmw(bus, Zpx, ShiftOp::Rol, 6),
            0x2E => self.rmw(bus, Abs, ShiftOp::Rol, 6),
            0x3E => self.rmw(bus, Abx, ShiftOp::Rol, 7),
            0x66 => self.rmw(bus, Zp, ShiftOp::Ror, 5),
            0x76 => self.rmw(bus, Zpx, ShiftOp::Ror, 6),
            0x6E => self.rmw(bus, Abs, ShiftOp::Ror, 6),
            0x7E => self.rmw(bus, Abx, ShiftOp::Ror, 7),

            // ---- Jumps / calls ----
            0x4C => { let (a, _) = self.operand_addr(bus, Abs); self.pc = a; 3 }
            0x6C => { let (a, _) = self.operand_addr(bus, Ind); self.pc = a; 5 }
            0x20 => {
                // JSR: push PC-1 of the next instruction.
                let a = self.fetch16(bus);
                self.push16(bus, self.pc.wrapping_sub(1));
                self.pc = a;
                6
            }
            0x60 => { let a = self.pop16(bus); self.pc = a.wrapping_add(1); 6 }
            0x40 => {
                // RTI: pull status then PC (no +1 on PC).
                let p = self.pop(bus);
                self.status = (p & !FLAG_B) | FLAG_U;
                self.pc = self.pop16(bus);
                6
            }

            // ---- Branches ----
            0x10 => self.branch(bus, !self.flag(FLAG_N)),
            0x30 => self.branch(bus, self.flag(FLAG_N)),
            0x50 => self.branch(bus, !self.flag(FLAG_V)),
            0x70 => self.branch(bus, self.flag(FLAG_V)),
            0x90 => self.branch(bus, !self.flag(FLAG_C)),
            0xB0 => self.branch(bus, self.flag(FLAG_C)),
            0xD0 => self.branch(bus, !self.flag(FLAG_Z)),
            0xF0 => self.branch(bus, self.flag(FLAG_Z)),

            // ---- Flag ops ----
            0x18 => { self.set_flag(FLAG_C, false); 2 }
            0x38 => { self.set_flag(FLAG_C, true); 2 }
            0x58 => { self.set_flag(FLAG_I, false); 2 }
            0x78 => { self.set_flag(FLAG_I, true); 2 }
            0xB8 => { self.set_flag(FLAG_V, false); 2 }
            0xD8 => { self.set_flag(FLAG_D, false); 2 }
            0xF8 => { self.set_flag(FLAG_D, true); 2 }

            // ---- BRK / NOP ----
            0x00 => {
                self.pc = self.pc.wrapping_add(1); // BRK has a padding byte
                self.interrupt(bus, IRQ_VECTOR, true);
                7
            }
            0xEA => 2,

            // ================= Unofficial opcodes =================
            // NOP variants (implied, immediate, zp, zpx, abs, abx).
            0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => 2,
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => { self.fetch8(bus); 2 }
            0x04 | 0x44 | 0x64 => { self.fetch8(bus); 3 }
            0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => { self.fetch8(bus); 4 }
            0x0C => { self.fetch16(bus); 4 }
            0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
                let (_, cross) = self.operand_addr(bus, Abx);
                4 + cross as u64
            }

            // LAX = LDA + LDX.
            0xA7 => self.lax(bus, Zp, 3),
            0xB7 => self.lax(bus, Zpy, 4),
            0xAF => self.lax(bus, Abs, 4),
            0xBF => self.lax(bus, Aby, 4),
            0xA3 => self.lax(bus, Izx, 6),
            0xB3 => self.lax(bus, Izy, 5),

            // SAX = store A & X.
            0x87 => self.sax(bus, Zp, 3),
            0x97 => self.sax(bus, Zpy, 4),
            0x8F => self.sax(bus, Abs, 4),
            0x83 => self.sax(bus, Izx, 6),

            // DCP = DEC + CMP.
            0xC7 => self.rmw_alu(bus, Zp, RmwAlu::Dcp, 5),
            0xD7 => self.rmw_alu(bus, Zpx, RmwAlu::Dcp, 6),
            0xCF => self.rmw_alu(bus, Abs, RmwAlu::Dcp, 6),
            0xDF => self.rmw_alu(bus, Abx, RmwAlu::Dcp, 7),
            0xDB => self.rmw_alu(bus, Aby, RmwAlu::Dcp, 7),
            0xC3 => self.rmw_alu(bus, Izx, RmwAlu::Dcp, 8),
            0xD3 => self.rmw_alu(bus, Izy, RmwAlu::Dcp, 8),

            // ISC/ISB = INC + SBC.
            0xE7 => self.rmw_alu(bus, Zp, RmwAlu::Isc, 5),
            0xF7 => self.rmw_alu(bus, Zpx, RmwAlu::Isc, 6),
            0xEF => self.rmw_alu(bus, Abs, RmwAlu::Isc, 6),
            0xFF => self.rmw_alu(bus, Abx, RmwAlu::Isc, 7),
            0xFB => self.rmw_alu(bus, Aby, RmwAlu::Isc, 7),
            0xE3 => self.rmw_alu(bus, Izx, RmwAlu::Isc, 8),
            0xF3 => self.rmw_alu(bus, Izy, RmwAlu::Isc, 8),

            // SLO = ASL + ORA.
            0x07 => self.rmw_alu(bus, Zp, RmwAlu::Slo, 5),
            0x17 => self.rmw_alu(bus, Zpx, RmwAlu::Slo, 6),
            0x0F => self.rmw_alu(bus, Abs, RmwAlu::Slo, 6),
            0x1F => self.rmw_alu(bus, Abx, RmwAlu::Slo, 7),
            0x1B => self.rmw_alu(bus, Aby, RmwAlu::Slo, 7),
            0x03 => self.rmw_alu(bus, Izx, RmwAlu::Slo, 8),
            0x13 => self.rmw_alu(bus, Izy, RmwAlu::Slo, 8),

            // RLA = ROL + AND.
            0x27 => self.rmw_alu(bus, Zp, RmwAlu::Rla, 5),
            0x37 => self.rmw_alu(bus, Zpx, RmwAlu::Rla, 6),
            0x2F => self.rmw_alu(bus, Abs, RmwAlu::Rla, 6),
            0x3F => self.rmw_alu(bus, Abx, RmwAlu::Rla, 7),
            0x3B => self.rmw_alu(bus, Aby, RmwAlu::Rla, 7),
            0x23 => self.rmw_alu(bus, Izx, RmwAlu::Rla, 8),
            0x33 => self.rmw_alu(bus, Izy, RmwAlu::Rla, 8),

            // SRE = LSR + EOR.
            0x47 => self.rmw_alu(bus, Zp, RmwAlu::Sre, 5),
            0x57 => self.rmw_alu(bus, Zpx, RmwAlu::Sre, 6),
            0x4F => self.rmw_alu(bus, Abs, RmwAlu::Sre, 6),
            0x5F => self.rmw_alu(bus, Abx, RmwAlu::Sre, 7),
            0x5B => self.rmw_alu(bus, Aby, RmwAlu::Sre, 7),
            0x43 => self.rmw_alu(bus, Izx, RmwAlu::Sre, 8),
            0x53 => self.rmw_alu(bus, Izy, RmwAlu::Sre, 8),

            // RRA = ROR + ADC.
            0x67 => self.rmw_alu(bus, Zp, RmwAlu::Rra, 5),
            0x77 => self.rmw_alu(bus, Zpx, RmwAlu::Rra, 6),
            0x6F => self.rmw_alu(bus, Abs, RmwAlu::Rra, 6),
            0x7F => self.rmw_alu(bus, Abx, RmwAlu::Rra, 7),
            0x7B => self.rmw_alu(bus, Aby, RmwAlu::Rra, 7),
            0x63 => self.rmw_alu(bus, Izx, RmwAlu::Rra, 8),
            0x73 => self.rmw_alu(bus, Izy, RmwAlu::Rra, 8),

            // Any remaining undocumented opcode: treat as a 2-cycle NOP. (KIL
            // opcodes would actually jam the CPU; test ROMs don't hit them.)
            _ => 2,
        }
    }

    // ---- instruction families ----

    fn ld(&mut self, bus: &mut dyn Bus, mode: Mode, reg: Reg, base: u64) -> u64 {
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        match reg {
            Reg::A => self.a = v,
            Reg::X => self.x = v,
            Reg::Y => self.y = v,
        }
        self.set_zn(v);
        base + cross as u64
    }

    fn st(&mut self, bus: &mut dyn Bus, mode: Mode, reg: Reg, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = match reg {
            Reg::A => self.a,
            Reg::X => self.x,
            Reg::Y => self.y,
        };
        bus.write8(a, v);
        base
    }

    fn alu(&mut self, bus: &mut dyn Bus, mode: Mode, op: AluOp, base: u64) -> u64 {
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        match op {
            AluOp::And => { self.a &= v; self.set_zn(self.a); }
            AluOp::Ora => { self.a |= v; self.set_zn(self.a); }
            AluOp::Eor => { self.a ^= v; self.set_zn(self.a); }
            AluOp::Adc => self.adc(v),
            AluOp::Sbc => self.sbc(v),
        }
        base + cross as u64
    }

    fn cmp(&mut self, bus: &mut dyn Bus, mode: Mode, reg: Reg, base: u64) -> u64 {
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        let r = match reg {
            Reg::A => self.a,
            Reg::X => self.x,
            Reg::Y => self.y,
        };
        let diff = r.wrapping_sub(v);
        self.set_flag(FLAG_C, r >= v);
        self.set_zn(diff);
        base + cross as u64
    }

    fn bit(&mut self, bus: &mut dyn Bus, mode: Mode, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        self.set_flag(FLAG_Z, self.a & v == 0);
        self.set_flag(FLAG_V, v & 0x40 != 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
        base
    }

    fn inc_dec_mem(&mut self, bus: &mut dyn Bus, mode: Mode, delta: i8, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        let nv = if delta > 0 { v.wrapping_add(1) } else { v.wrapping_sub(1) };
        bus.write8(a, nv);
        self.set_zn(nv);
        base
    }

    fn rmw(&mut self, bus: &mut dyn Bus, mode: Mode, op: ShiftOp, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        let nv = match op {
            ShiftOp::Asl => self.asl(v),
            ShiftOp::Lsr => self.lsr(v),
            ShiftOp::Rol => self.rol(v),
            ShiftOp::Ror => self.ror(v),
        };
        bus.write8(a, nv);
        base
    }

    fn branch(&mut self, bus: &mut dyn Bus, take: bool) -> u64 {
        let (target, _) = self.operand_addr(bus, Mode::Rel);
        if take {
            let cross = page_crossed(self.pc, target);
            self.pc = target;
            3 + cross as u64
        } else {
            2
        }
    }

    // ---- ALU primitives ----
    fn adc(&mut self, v: u8) {
        let c = self.flag(FLAG_C) as u16;
        let sum = self.a as u16 + v as u16 + c;
        let result = sum as u8;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_V, (self.a ^ result) & (v ^ result) & 0x80 != 0);
        self.a = result;
        self.set_zn(result);
    }
    fn sbc(&mut self, v: u8) {
        // SBC is ADC of the one's complement.
        self.adc(v ^ 0xFF);
    }
    fn asl(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        self.set_zn(r);
        r
    }
    fn lsr(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 1 != 0);
        let r = v >> 1;
        self.set_zn(r);
        r
    }
    fn rol(&mut self, v: u8) -> u8 {
        let c = self.flag(FLAG_C) as u8;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | c;
        self.set_zn(r);
        r
    }
    fn ror(&mut self, v: u8) -> u8 {
        let c = self.flag(FLAG_C) as u8;
        self.set_flag(FLAG_C, v & 1 != 0);
        let r = (v >> 1) | (c << 7);
        self.set_zn(r);
        r
    }

    // ---- unofficial helpers ----
    fn lax(&mut self, bus: &mut dyn Bus, mode: Mode, base: u64) -> u64 {
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        self.a = v;
        self.x = v;
        self.set_zn(v);
        base + cross as u64
    }
    fn sax(&mut self, bus: &mut dyn Bus, mode: Mode, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        bus.write8(a, self.a & self.x);
        base
    }
    fn rmw_alu(&mut self, bus: &mut dyn Bus, mode: Mode, op: RmwAlu, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        let nv = match op {
            RmwAlu::Dcp => {
                let d = v.wrapping_sub(1);
                bus.write8(a, d);
                let diff = self.a.wrapping_sub(d);
                self.set_flag(FLAG_C, self.a >= d);
                self.set_zn(diff);
                d
            }
            RmwAlu::Isc => {
                let d = v.wrapping_add(1);
                bus.write8(a, d);
                self.sbc(d);
                d
            }
            RmwAlu::Slo => {
                let d = self.asl(v);
                bus.write8(a, d);
                self.a |= d;
                self.set_zn(self.a);
                d
            }
            RmwAlu::Rla => {
                let d = self.rol(v);
                bus.write8(a, d);
                self.a &= d;
                self.set_zn(self.a);
                d
            }
            RmwAlu::Sre => {
                let d = self.lsr(v);
                bus.write8(a, d);
                self.a ^= d;
                self.set_zn(self.a);
                d
            }
            RmwAlu::Rra => {
                let d = self.ror(v);
                bus.write8(a, d);
                self.adc(d);
                d
            }
        };
        let _ = nv;
        base
    }
}

#[inline]
fn page_crossed(a: u16, b: u16) -> bool {
    (a & 0xFF00) != (b & 0xFF00)
}

#[derive(Clone, Copy)]
enum Reg {
    A,
    X,
    Y,
}
#[derive(Clone, Copy)]
enum AluOp {
    And,
    Ora,
    Eor,
    Adc,
    Sbc,
}
#[derive(Clone, Copy)]
enum ShiftOp {
    Asl,
    Lsr,
    Rol,
    Ror,
}
#[derive(Clone, Copy)]
enum RmwAlu {
    Dcp,
    Isc,
    Slo,
    Rla,
    Sre,
    Rra,
}

#[cfg(test)]
mod tests {
    use super::*;

    // A flat 64 KiB RAM bus for isolated CPU tests.
    struct FlatBus {
        ram: Vec<u8>,
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus { ram: vec![0u8; 0x10000] }
        }
    }
    impl Bus for FlatBus {
        fn read8(&mut self, a: u16) -> u8 {
            self.ram[a as usize]
        }
        fn write8(&mut self, a: u16, v: u8) {
            self.ram[a as usize] = v;
        }
    }

    fn run_one(cpu: &mut Cpu, bus: &mut FlatBus) -> u64 {
        cpu.step(bus)
    }

    #[test]
    fn lda_immediate_sets_flags() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0xA9; // LDA #$00
        bus.ram[0x8001] = 0x00;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0);
        assert!(cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn adc_overflow_and_carry() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0x50;
        bus.ram[0x8000] = 0x69; // ADC #$50
        bus.ram[0x8001] = 0x50;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0xA0);
        assert!(cpu.flag(FLAG_V)); // 80 + 80 -> overflow
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_C));
    }

    #[test]
    fn sbc_basic() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0x50;
        cpu.set_flag(FLAG_C, true); // no borrow
        bus.ram[0x8000] = 0xE9; // SBC #$10
        bus.ram[0x8001] = 0x10;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0x40);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn branch_taken_page_cross_cycles() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x80F0;
        cpu.set_flag(FLAG_Z, true);
        bus.ram[0x80F0] = 0xF0; // BEQ +0x20 -> crosses into 0x8112
        bus.ram[0x80F1] = 0x20;
        let c = run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x8112);
        assert_eq!(c, 4); // 3 base + 1 page cross
    }

    #[test]
    fn jsr_rts_roundtrip() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0x20; // JSR $9000
        bus.ram[0x8001] = 0x00;
        bus.ram[0x8002] = 0x90;
        bus.ram[0x9000] = 0x60; // RTS
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x9000);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn indirect_jmp_page_bug() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0x6C; // JMP ($30FF)
        bus.ram[0x8001] = 0xFF;
        bus.ram[0x8002] = 0x30;
        bus.ram[0x30FF] = 0x40;
        bus.ram[0x3000] = 0x80; // bug: high byte read from 0x3000, not 0x3100
        bus.ram[0x3100] = 0x99;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x8040);
    }

    #[test]
    fn nmi_pushes_and_jumps() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[NMI_VECTOR as usize] = 0x00;
        bus.ram[NMI_VECTOR as usize + 1] = 0x90;
        cpu.nmi_pending = true;
        let c = cpu.step(&mut bus);
        assert_eq!(c, 7);
        assert_eq!(cpu.pc, 0x9000);
        assert!(cpu.flag(FLAG_I));
    }

    #[test]
    fn lax_loads_a_and_x() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0xA7; // LAX $10
        bus.ram[0x8001] = 0x10;
        bus.ram[0x0010] = 0x42;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.x, 0x42);
    }
}
