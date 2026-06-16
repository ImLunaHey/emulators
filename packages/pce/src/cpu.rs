//! Hudson HuC6280 CPU core — a 65C02 (so: the NMOS 6502 instruction set with
//! the CMOS additions — BRA, PHX/PLX/PHY/PLY, STZ, TSB/TRB, RMB/SMB, BBR/BBS,
//! zero-page-indirect addressing, INC/DEC A, fixed JMP-indirect, working
//! decimal mode) PLUS the Hudson extensions:
//!
//!   - Banking MMU: eight Memory Page Registers (MPR0..7). TAM #imm copies A
//!     into the selected MPRs; TMA #imm reads the (single) selected MPR into A.
//!   - Block transfers: TII, TDD, TIA, TAI, TIN — a 7-byte src/dst/len copy.
//!   - ST0/ST1/ST2: write an immediate to the VDC address/data registers
//!     (mapped to the VDC at the I/O page) without disturbing the MMU.
//!   - CSL/CSH: low/high speed select (1.79 / 7.16 MHz).
//!   - SET (the "T" flag): the next ALU op operates memory-to-memory at the
//!     zero-page address in X (rarely used; modelled minimally).
//!   - TST: test memory bits against an immediate.
//!   - The HuC6280 always has decimal mode behave as 65C02 (Z/N valid).
//!
//! Spec: HuC6280 datasheet, Archaic Pixels "HuC6280", pcedev wiki "CPU".
//!
//! The CPU drives memory through `&mut dyn Bus` (see `crate::bus::Bus`). `step()`
//! executes one instruction and returns the cycle count it consumed; the
//! orchestrator advances the VDC/PSG/timer accordingly.

use crate::bus::Bus;

// Status flags. The HuC6280's "T" flag reuses bit 5 (the 6502's unused bit).
pub const FLAG_C: u8 = 1 << 0; // carry
pub const FLAG_Z: u8 = 1 << 1; // zero
pub const FLAG_I: u8 = 1 << 2; // interrupt disable
pub const FLAG_D: u8 = 1 << 3; // decimal
pub const FLAG_B: u8 = 1 << 4; // break (only meaningful on the stack copy)
pub const FLAG_T: u8 = 1 << 5; // memory-operation (SET); the 6502's "unused" bit
pub const FLAG_V: u8 = 1 << 6; // overflow
pub const FLAG_N: u8 = 1 << 7; // negative

const STACK_BASE: u16 = 0x2100; // HuC6280: zero page is bank-relative; stack is
                                // at logical $2100-$21FF (page 1 of the 2 KiB
                                // RAM that MPR1 maps). The CPU still indexes by
                                // SP within a 256-byte window.
const RESET_VECTOR: u16 = 0xFFFE; // reset/IRQ2/BRK live at the top of bank $FF
const IRQ2_VECTOR: u16 = 0xFFF6; // IRQ2 / BRK
const IRQ1_VECTOR: u16 = 0xFFF8; // IRQ1 (VDC)
const TIMER_VECTOR: u16 = 0xFFFA; // TIQ (timer)
const NMI_VECTOR: u16 = 0xFFFC; // NMI (unused on PCE but present)
const RESET_VEC: u16 = 0xFFFE; // RESET

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Imm,  // immediate
    Zp,   // zero page
    Zpx,  // zero page,X
    Zpy,  // zero page,Y
    Abs,  // absolute
    Abx,  // absolute,X
    Aby,  // absolute,Y
    Ind,  // (indirect)  — JMP, 65C02-fixed
    Izp,  // (zp)        — 65C02 zero-page indirect
    Izx,  // (zp,X)
    Izy,  // (zp),Y
    Iax,  // (abs,X)     — 65C02 JMP (abs,X)
    Rel,  // relative (branch)
}

pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub status: u8,

    /// Pending interrupt lines. IRQ1 (VDC) and IRQ2 (CD/BRK) are level; TIQ
    /// (timer) is level; NMI is edge. The orchestrator raises these; the CPU
    /// samples them between instructions. The interrupt-disable register
    /// ($1402) masks IRQ1/IRQ2/TIQ separately — that masking is applied by the
    /// orchestrator before raising these lines.
    pub irq1_line: bool,
    pub irq2_line: bool,
    pub tiq_line: bool,
    pub nmi_pending: bool,

    /// Low/high speed mode (CSL/CSH). Affects the cycle scaling the orchestrator
    /// applies; the CPU just records it.
    pub high_speed: bool,

    /// Latched on executing a recognised hard-halt condition (the 6502 KIL
    /// opcodes do not exist on the 65C02, which makes them NOPs — so the
    /// HuC6280 cannot truly JAM. We still expose the field for parity with the
    /// other cores; it is currently never set.)
    pub jam: Option<(u8, u16)>,

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
            sp: 0xFF,
            pc: 0,
            status: FLAG_I,
            irq1_line: false,
            irq2_line: false,
            tiq_line: false,
            nmi_pending: false,
            high_speed: false,
            jam: None,
            cycles: 0,
        }
    }

    /// Power-on / RESET: load PC from the reset vector, set I, clear decimal.
    pub fn reset(&mut self, bus: &mut dyn Bus) {
        let lo = bus.read8(RESET_VEC) as u16;
        let hi = bus.read8(RESET_VEC + 1) as u16;
        self.pc = (hi << 8) | lo;
        self.sp = 0xFF;
        self.status = FLAG_I;
        self.high_speed = false;
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
    pub fn flag(&self, f: u8) -> bool {
        self.status & f != 0
    }
    #[inline]
    fn set_zn(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
    }

    // ---- stack (lives in logical $2100-$21FF) ----
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

    /// Zero page on the HuC6280 is logical $2000-$20FF (the base of MPR1's RAM).
    #[inline]
    fn zp_addr(&self, off: u8) -> u16 {
        0x2000 | off as u16
    }

    /// Resolve an addressing mode to an effective address + page-cross flag.
    fn operand_addr(&mut self, bus: &mut dyn Bus, mode: Mode) -> (u16, bool) {
        match mode {
            Mode::Imm => {
                let a = self.pc;
                self.pc = self.pc.wrapping_add(1);
                (a, false)
            }
            Mode::Zp => {
                let o = self.fetch8(bus);
                (self.zp_addr(o), false)
            }
            Mode::Zpx => {
                let o = self.fetch8(bus).wrapping_add(self.x);
                (self.zp_addr(o), false)
            }
            Mode::Zpy => {
                let o = self.fetch8(bus).wrapping_add(self.y);
                (self.zp_addr(o), false)
            }
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
                // 65C02 fixed JMP (indirect): no page-wrap bug.
                let ptr = self.fetch16(bus);
                let lo = bus.read8(ptr) as u16;
                let hi = bus.read8(ptr.wrapping_add(1)) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Izp => {
                // 65C02 (zp): pointer read from zero page.
                let zp = self.fetch8(bus);
                let lo = bus.read8(self.zp_addr(zp)) as u16;
                let hi = bus.read8(self.zp_addr(zp.wrapping_add(1))) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Izx => {
                let zp = self.fetch8(bus).wrapping_add(self.x);
                let lo = bus.read8(self.zp_addr(zp)) as u16;
                let hi = bus.read8(self.zp_addr(zp.wrapping_add(1))) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Izy => {
                let zp = self.fetch8(bus);
                let lo = bus.read8(self.zp_addr(zp)) as u16;
                let hi = bus.read8(self.zp_addr(zp.wrapping_add(1))) as u16;
                let base = (hi << 8) | lo;
                let a = base.wrapping_add(self.y as u16);
                (a, page_crossed(base, a))
            }
            Mode::Iax => {
                // 65C02 JMP (abs,X).
                let base = self.fetch16(bus);
                let ptr = base.wrapping_add(self.x as u16);
                let lo = bus.read8(ptr) as u16;
                let hi = bus.read8(ptr.wrapping_add(1)) as u16;
                ((hi << 8) | lo, false)
            }
            Mode::Rel => {
                let off = self.fetch8(bus) as i8 as i16;
                let a = (self.pc as i16).wrapping_add(off) as u16;
                (a, false)
            }
        }
    }

    /// Service a pending interrupt if one is latched + unmasked. Returns extra
    /// cycles (0 if none taken). Priority: TIQ < IRQ1 < IRQ2 — actually
    /// hardware priority is IRQ2 highest? Per the datasheet the vector priority
    /// (highest first) is RESET, NMI, TIQ, IRQ1, IRQ2. We service NMI, then
    /// TIQ, then IRQ1, then IRQ2.
    fn poll_interrupts(&mut self, bus: &mut dyn Bus) -> u64 {
        if self.nmi_pending {
            self.nmi_pending = false;
            self.interrupt(bus, NMI_VECTOR, false);
            return 8;
        }
        if self.flag(FLAG_I) {
            return 0;
        }
        if self.tiq_line {
            self.interrupt(bus, TIMER_VECTOR, false);
            return 8;
        }
        if self.irq1_line {
            self.interrupt(bus, IRQ1_VECTOR, false);
            return 8;
        }
        if self.irq2_line {
            self.interrupt(bus, IRQ2_VECTOR, false);
            return 8;
        }
        let _ = (RESET_VECTOR, IRQ2_VECTOR);
        0
    }

    fn interrupt(&mut self, bus: &mut dyn Bus, vector: u16, brk: bool) {
        self.push16(bus, self.pc);
        let mut p = self.status;
        if brk {
            p |= FLAG_B;
        } else {
            p &= !FLAG_B;
        }
        self.push(bus, p);
        self.set_flag(FLAG_I, true);
        self.set_flag(FLAG_D, false); // 65C02 clears D on interrupt
        self.set_flag(FLAG_T, false);
        let lo = bus.read8(vector) as u16;
        let hi = bus.read8(vector + 1) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute one instruction (servicing interrupts first). Returns cycles
    /// consumed (in CPU master cycles at the current speed).
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
            0xA5 => self.ld(bus, Zp, Reg::A, 4),
            0xB5 => self.ld(bus, Zpx, Reg::A, 4),
            0xAD => self.ld(bus, Abs, Reg::A, 5),
            0xBD => self.ld(bus, Abx, Reg::A, 5),
            0xB9 => self.ld(bus, Aby, Reg::A, 5),
            0xA1 => self.ld(bus, Izx, Reg::A, 7),
            0xB1 => self.ld(bus, Izy, Reg::A, 7),
            0xB2 => self.ld(bus, Izp, Reg::A, 7), // LDA (zp)

            0xA2 => self.ld(bus, Imm, Reg::X, 2),
            0xA6 => self.ld(bus, Zp, Reg::X, 4),
            0xB6 => self.ld(bus, Zpy, Reg::X, 4),
            0xAE => self.ld(bus, Abs, Reg::X, 5),
            0xBE => self.ld(bus, Aby, Reg::X, 5),

            0xA0 => self.ld(bus, Imm, Reg::Y, 2),
            0xA4 => self.ld(bus, Zp, Reg::Y, 4),
            0xB4 => self.ld(bus, Zpx, Reg::Y, 4),
            0xAC => self.ld(bus, Abs, Reg::Y, 5),
            0xBC => self.ld(bus, Abx, Reg::Y, 5),

            // ---- Stores ----
            0x85 => self.st(bus, Zp, Reg::A, 4),
            0x95 => self.st(bus, Zpx, Reg::A, 4),
            0x8D => self.st(bus, Abs, Reg::A, 5),
            0x9D => self.st(bus, Abx, Reg::A, 5),
            0x99 => self.st(bus, Aby, Reg::A, 5),
            0x81 => self.st(bus, Izx, Reg::A, 7),
            0x91 => self.st(bus, Izy, Reg::A, 7),
            0x92 => self.st(bus, Izp, Reg::A, 7), // STA (zp)
            0x86 => self.st(bus, Zp, Reg::X, 4),
            0x96 => self.st(bus, Zpy, Reg::X, 4),
            0x8E => self.st(bus, Abs, Reg::X, 5),
            0x84 => self.st(bus, Zp, Reg::Y, 4),
            0x94 => self.st(bus, Zpx, Reg::Y, 4),
            0x8C => self.st(bus, Abs, Reg::Y, 5),

            // ---- STZ (65C02 store zero) ----
            0x64 => self.stz(bus, Zp, 4),
            0x74 => self.stz(bus, Zpx, 4),
            0x9C => self.stz(bus, Abs, 5),
            0x9E => self.stz(bus, Abx, 5),

            // ---- Transfers ----
            0xAA => { self.x = self.a; self.set_zn(self.x); 2 }
            0xA8 => { self.y = self.a; self.set_zn(self.y); 2 }
            0x8A => { self.a = self.x; self.set_zn(self.a); 2 }
            0x98 => { self.a = self.y; self.set_zn(self.a); 2 }
            0xBA => { self.x = self.sp; self.set_zn(self.x); 2 }
            0x9A => { self.sp = self.x; 2 }

            // ---- Stack (incl. 65C02 PHX/PLX/PHY/PLY) ----
            0x48 => { self.push(bus, self.a); 3 }
            0x68 => { let v = self.pop(bus); self.a = v; self.set_zn(v); 4 }
            0x08 => { self.push(bus, self.status | FLAG_B); 3 }
            0x28 => { let v = self.pop(bus); self.status = v & !FLAG_B; 4 }
            0xDA => { self.push(bus, self.x); 3 } // PHX
            0xFA => { let v = self.pop(bus); self.x = v; self.set_zn(v); 4 } // PLX
            0x5A => { self.push(bus, self.y); 3 } // PHY
            0x7A => { let v = self.pop(bus); self.y = v; self.set_zn(v); 4 } // PLY

            // ---- Logic ----
            0x29 => self.alu(bus, Imm, AluOp::And, 2),
            0x25 => self.alu(bus, Zp, AluOp::And, 4),
            0x35 => self.alu(bus, Zpx, AluOp::And, 4),
            0x2D => self.alu(bus, Abs, AluOp::And, 5),
            0x3D => self.alu(bus, Abx, AluOp::And, 5),
            0x39 => self.alu(bus, Aby, AluOp::And, 5),
            0x21 => self.alu(bus, Izx, AluOp::And, 7),
            0x31 => self.alu(bus, Izy, AluOp::And, 7),
            0x32 => self.alu(bus, Izp, AluOp::And, 7),

            0x09 => self.alu(bus, Imm, AluOp::Ora, 2),
            0x05 => self.alu(bus, Zp, AluOp::Ora, 4),
            0x15 => self.alu(bus, Zpx, AluOp::Ora, 4),
            0x0D => self.alu(bus, Abs, AluOp::Ora, 5),
            0x1D => self.alu(bus, Abx, AluOp::Ora, 5),
            0x19 => self.alu(bus, Aby, AluOp::Ora, 5),
            0x01 => self.alu(bus, Izx, AluOp::Ora, 7),
            0x11 => self.alu(bus, Izy, AluOp::Ora, 7),
            0x12 => self.alu(bus, Izp, AluOp::Ora, 7),

            0x49 => self.alu(bus, Imm, AluOp::Eor, 2),
            0x45 => self.alu(bus, Zp, AluOp::Eor, 4),
            0x55 => self.alu(bus, Zpx, AluOp::Eor, 4),
            0x4D => self.alu(bus, Abs, AluOp::Eor, 5),
            0x5D => self.alu(bus, Abx, AluOp::Eor, 5),
            0x59 => self.alu(bus, Aby, AluOp::Eor, 5),
            0x41 => self.alu(bus, Izx, AluOp::Eor, 7),
            0x51 => self.alu(bus, Izy, AluOp::Eor, 7),
            0x52 => self.alu(bus, Izp, AluOp::Eor, 7),

            // ---- Arithmetic ----
            0x69 => self.alu(bus, Imm, AluOp::Adc, 2),
            0x65 => self.alu(bus, Zp, AluOp::Adc, 4),
            0x75 => self.alu(bus, Zpx, AluOp::Adc, 4),
            0x6D => self.alu(bus, Abs, AluOp::Adc, 5),
            0x7D => self.alu(bus, Abx, AluOp::Adc, 5),
            0x79 => self.alu(bus, Aby, AluOp::Adc, 5),
            0x61 => self.alu(bus, Izx, AluOp::Adc, 7),
            0x71 => self.alu(bus, Izy, AluOp::Adc, 7),
            0x72 => self.alu(bus, Izp, AluOp::Adc, 7),

            0xE9 => self.alu(bus, Imm, AluOp::Sbc, 2),
            0xE5 => self.alu(bus, Zp, AluOp::Sbc, 4),
            0xF5 => self.alu(bus, Zpx, AluOp::Sbc, 4),
            0xED => self.alu(bus, Abs, AluOp::Sbc, 5),
            0xFD => self.alu(bus, Abx, AluOp::Sbc, 5),
            0xF9 => self.alu(bus, Aby, AluOp::Sbc, 5),
            0xE1 => self.alu(bus, Izx, AluOp::Sbc, 7),
            0xF1 => self.alu(bus, Izy, AluOp::Sbc, 7),
            0xF2 => self.alu(bus, Izp, AluOp::Sbc, 7),

            // ---- Compares ----
            0xC9 => self.cmp(bus, Imm, Reg::A, 2),
            0xC5 => self.cmp(bus, Zp, Reg::A, 4),
            0xD5 => self.cmp(bus, Zpx, Reg::A, 4),
            0xCD => self.cmp(bus, Abs, Reg::A, 5),
            0xDD => self.cmp(bus, Abx, Reg::A, 5),
            0xD9 => self.cmp(bus, Aby, Reg::A, 5),
            0xC1 => self.cmp(bus, Izx, Reg::A, 7),
            0xD1 => self.cmp(bus, Izy, Reg::A, 7),
            0xD2 => self.cmp(bus, Izp, Reg::A, 7),
            0xE0 => self.cmp(bus, Imm, Reg::X, 2),
            0xE4 => self.cmp(bus, Zp, Reg::X, 4),
            0xEC => self.cmp(bus, Abs, Reg::X, 5),
            0xC0 => self.cmp(bus, Imm, Reg::Y, 2),
            0xC4 => self.cmp(bus, Zp, Reg::Y, 4),
            0xCC => self.cmp(bus, Abs, Reg::Y, 5),

            // ---- BIT ----
            0x24 => self.bit(bus, Zp, 4),
            0x2C => self.bit(bus, Abs, 5),
            0x34 => self.bit(bus, Zpx, 4),   // 65C02
            0x3C => self.bit(bus, Abx, 5),   // 65C02
            0x89 => self.bit_imm(bus, 2),    // 65C02 BIT #imm (only Z affected)

            // ---- TST (HuC6280): test memory bits against immediate ----
            0x83 => self.tst(bus, Zp, 7),
            0xA3 => self.tst(bus, Zpx, 7),
            0x93 => self.tst(bus, Abs, 8),
            0xB3 => self.tst(bus, Abx, 8),

            // ---- Inc/Dec memory ----
            0xE6 => self.inc_dec_mem(bus, Zp, 1, 6),
            0xF6 => self.inc_dec_mem(bus, Zpx, 1, 6),
            0xEE => self.inc_dec_mem(bus, Abs, 1, 7),
            0xFE => self.inc_dec_mem(bus, Abx, 1, 7),
            0xC6 => self.inc_dec_mem(bus, Zp, -1, 6),
            0xD6 => self.inc_dec_mem(bus, Zpx, -1, 6),
            0xCE => self.inc_dec_mem(bus, Abs, -1, 7),
            0xDE => self.inc_dec_mem(bus, Abx, -1, 7),

            // ---- Inc/Dec register (incl. 65C02 INC A / DEC A) ----
            0xE8 => { self.x = self.x.wrapping_add(1); self.set_zn(self.x); 2 }
            0xCA => { self.x = self.x.wrapping_sub(1); self.set_zn(self.x); 2 }
            0xC8 => { self.y = self.y.wrapping_add(1); self.set_zn(self.y); 2 }
            0x88 => { self.y = self.y.wrapping_sub(1); self.set_zn(self.y); 2 }
            0x1A => { self.a = self.a.wrapping_add(1); self.set_zn(self.a); 2 } // INC A
            0x3A => { self.a = self.a.wrapping_sub(1); self.set_zn(self.a); 2 } // DEC A

            // ---- Shifts/rotates on accumulator ----
            0x0A => { self.a = self.asl(self.a); 2 }
            0x4A => { self.a = self.lsr(self.a); 2 }
            0x2A => { self.a = self.rol(self.a); 2 }
            0x6A => { self.a = self.ror(self.a); 2 }

            // ---- Shifts/rotates on memory ----
            0x06 => self.rmw(bus, Zp, ShiftOp::Asl, 6),
            0x16 => self.rmw(bus, Zpx, ShiftOp::Asl, 6),
            0x0E => self.rmw(bus, Abs, ShiftOp::Asl, 7),
            0x1E => self.rmw(bus, Abx, ShiftOp::Asl, 7),
            0x46 => self.rmw(bus, Zp, ShiftOp::Lsr, 6),
            0x56 => self.rmw(bus, Zpx, ShiftOp::Lsr, 6),
            0x4E => self.rmw(bus, Abs, ShiftOp::Lsr, 7),
            0x5E => self.rmw(bus, Abx, ShiftOp::Lsr, 7),
            0x26 => self.rmw(bus, Zp, ShiftOp::Rol, 6),
            0x36 => self.rmw(bus, Zpx, ShiftOp::Rol, 6),
            0x2E => self.rmw(bus, Abs, ShiftOp::Rol, 7),
            0x3E => self.rmw(bus, Abx, ShiftOp::Rol, 7),
            0x66 => self.rmw(bus, Zp, ShiftOp::Ror, 6),
            0x76 => self.rmw(bus, Zpx, ShiftOp::Ror, 6),
            0x6E => self.rmw(bus, Abs, ShiftOp::Ror, 7),
            0x7E => self.rmw(bus, Abx, ShiftOp::Ror, 7),

            // ---- TSB/TRB (65C02) ----
            0x04 => self.tsb_trb(bus, Zp, true, 6),
            0x0C => self.tsb_trb(bus, Abs, true, 7),
            0x14 => self.tsb_trb(bus, Zp, false, 6),
            0x1C => self.tsb_trb(bus, Abs, false, 7),

            // ---- RMB/SMB (65C02): reset/set memory bit b in zero page ----
            0x07 | 0x17 | 0x27 | 0x37 | 0x47 | 0x57 | 0x67 | 0x77 => {
                let bit = (op >> 4) & 7;
                self.rmb_smb(bus, bit, false, 7)
            }
            0x87 | 0x97 | 0xA7 | 0xB7 | 0xC7 | 0xD7 | 0xE7 | 0xF7 => {
                let bit = (op >> 4) & 7;
                self.rmb_smb(bus, bit, true, 7)
            }

            // ---- BBR/BBS (65C02): branch on zero-page bit b ----
            0x0F | 0x1F | 0x2F | 0x3F | 0x4F | 0x5F | 0x6F | 0x7F => {
                let bit = (op >> 4) & 7;
                self.bbr_bbs(bus, bit, false)
            }
            0x8F | 0x9F | 0xAF | 0xBF | 0xCF | 0xDF | 0xEF | 0xFF => {
                let bit = (op >> 4) & 7;
                self.bbr_bbs(bus, bit, true)
            }

            // ---- Jumps / calls ----
            0x4C => { let (a, _) = self.operand_addr(bus, Abs); self.pc = a; 4 }
            0x6C => { let (a, _) = self.operand_addr(bus, Ind); self.pc = a; 7 }
            0x7C => { let (a, _) = self.operand_addr(bus, Iax); self.pc = a; 7 }
            0x20 => {
                let a = self.fetch16(bus);
                self.push16(bus, self.pc.wrapping_sub(1));
                self.pc = a;
                7
            }
            0x60 => { let a = self.pop16(bus); self.pc = a.wrapping_add(1); 7 }
            0x40 => {
                let p = self.pop(bus);
                self.status = p & !FLAG_B;
                self.pc = self.pop16(bus);
                7
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
            0x80 => self.branch(bus, true), // BRA (65C02)

            // ---- Flag ops ----
            0x18 => { self.set_flag(FLAG_C, false); 2 }
            0x38 => { self.set_flag(FLAG_C, true); 2 }
            0x58 => { self.set_flag(FLAG_I, false); 2 }
            0x78 => { self.set_flag(FLAG_I, true); 2 }
            0xB8 => { self.set_flag(FLAG_V, false); 2 }
            0xD8 => { self.set_flag(FLAG_D, false); 2 }
            0xF8 => { self.set_flag(FLAG_D, true); 2 }
            0xF4 => { self.set_flag(FLAG_T, true); 2 } // SET (the T flag)

            // ---- BRK ----
            0x00 => {
                self.pc = self.pc.wrapping_add(1);
                self.interrupt(bus, IRQ2_VECTOR, true);
                8
            }
            0xEA => 2, // NOP

            // ================= HuC6280 extensions =================
            // CSL / CSH: low / high speed select.
            0x54 => { self.high_speed = false; 3 } // CSL
            0xD4 => { self.high_speed = true; 3 }  // CSH

            // TAM #imm: copy A into every MPR whose bit is set in the immediate.
            0x53 => {
                let mask = self.fetch8(bus);
                for n in 0..8 {
                    if mask & (1 << n) != 0 {
                        bus.set_mpr(n, self.a);
                    }
                }
                5
            }
            // TMA #imm: read the (lowest selected) MPR into A.
            0x43 => {
                let mask = self.fetch8(bus);
                for n in 0..8 {
                    if mask & (1 << n) != 0 {
                        self.a = bus.get_mpr(n);
                        break;
                    }
                }
                4
            }

            // ST0/ST1/ST2: write immediate to the VDC. ST0 -> AR (address reg),
            // ST1 -> data low, ST2 -> data high. We forward via reserved logical
            // I/O addresses the bus recognises ($0000/$0002/$0003 in the I/O
            // page convention): the Pce bus maps these directly to the VDC.
            0x03 => { let v = self.fetch8(bus); bus.write8(VDC_ST_AR, v); 4 }   // ST0
            0x13 => { let v = self.fetch8(bus); bus.write8(VDC_ST_LO, v); 4 }   // ST1
            0x23 => { let v = self.fetch8(bus); bus.write8(VDC_ST_HI, v); 4 }   // ST2

            // SXY / SAX / SAY: swap register pairs (HuC6280).
            0x02 => { std::mem::swap(&mut self.x, &mut self.y); 3 } // SXY
            0x22 => { std::mem::swap(&mut self.a, &mut self.x); 3 } // SAX
            0x42 => { std::mem::swap(&mut self.a, &mut self.y); 3 } // SAY

            // CLA / CLX / CLY: clear A/X/Y (HuC6280).
            0x62 => { self.a = 0; 2 } // CLA
            0x82 => { self.x = 0; 2 } // CLX
            0xC2 => { self.y = 0; 2 } // CLY

            // Block transfers (7 bytes each: opcode + src16 + dst16 + len16).
            0x73 => self.block_xfer(bus, BlockOp::Tii), // TII inc src, inc dst
            0xC3 => self.block_xfer(bus, BlockOp::Tdd), // TDD dec src, dec dst
            0xD3 => self.block_xfer(bus, BlockOp::Tin), // TIN inc src, dst fixed
            0xE3 => self.block_xfer(bus, BlockOp::Tia), // TIA inc src, dst alt
            0xF3 => self.block_xfer(bus, BlockOp::Tai), // TAI src alt, inc dst

            // Any unimplemented opcode: NOP (the 65C02 turns NMOS illegals into
            // NOPs of various lengths; treat as a 2-cycle NOP).
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

    fn stz(&mut self, bus: &mut dyn Bus, mode: Mode, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        bus.write8(a, 0);
        base
    }

    fn alu(&mut self, bus: &mut dyn Bus, mode: Mode, op: AluOp, base: u64) -> u64 {
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        // The "T" flag (SET) redirects the ALU accumulator to a zero-page cell
        // pointed by X. Best-effort: when T is set, op against memory[zp:X].
        if self.flag(FLAG_T) {
            let zp = self.zp_addr(self.x);
            let acc = bus.read8(zp);
            let r = self.alu_compute(op, acc, v);
            bus.write8(zp, r);
            self.set_flag(FLAG_T, false);
            return base + 3 + cross as u64;
        }
        match op {
            AluOp::And => { self.a &= v; self.set_zn(self.a); }
            AluOp::Ora => { self.a |= v; self.set_zn(self.a); }
            AluOp::Eor => { self.a ^= v; self.set_zn(self.a); }
            AluOp::Adc => self.adc(v),
            AluOp::Sbc => self.sbc(v),
        }
        base + cross as u64
    }

    fn alu_compute(&mut self, op: AluOp, acc: u8, v: u8) -> u8 {
        match op {
            AluOp::And => { let r = acc & v; self.set_zn(r); r }
            AluOp::Ora => { let r = acc | v; self.set_zn(r); r }
            AluOp::Eor => { let r = acc ^ v; self.set_zn(r); r }
            AluOp::Adc => { let saved = self.a; self.a = acc; self.adc(v); let r = self.a; self.a = saved; r }
            AluOp::Sbc => { let saved = self.a; self.a = acc; self.sbc(v); let r = self.a; self.a = saved; r }
        }
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
        let (a, cross) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        self.set_flag(FLAG_Z, self.a & v == 0);
        self.set_flag(FLAG_V, v & 0x40 != 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
        base + cross as u64
    }
    fn bit_imm(&mut self, bus: &mut dyn Bus, base: u64) -> u64 {
        // 65C02 BIT #imm only affects Z.
        let v = self.fetch8(bus);
        self.set_flag(FLAG_Z, self.a & v == 0);
        base
    }

    fn tst(&mut self, bus: &mut dyn Bus, mode: Mode, base: u64) -> u64 {
        // TST #imm, <mem>: immediate comes FIRST, then the memory operand.
        let imm = self.fetch8(bus);
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        self.set_flag(FLAG_Z, imm & v == 0);
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

    fn tsb_trb(&mut self, bus: &mut dyn Bus, mode: Mode, set: bool, base: u64) -> u64 {
        let (a, _) = self.operand_addr(bus, mode);
        let v = bus.read8(a);
        self.set_flag(FLAG_Z, self.a & v == 0);
        let nv = if set { v | self.a } else { v & !self.a };
        bus.write8(a, nv);
        base
    }

    fn rmb_smb(&mut self, bus: &mut dyn Bus, bit: u8, set: bool, base: u64) -> u64 {
        let zp = self.fetch8(bus);
        let a = self.zp_addr(zp);
        let v = bus.read8(a);
        let nv = if set { v | (1 << bit) } else { v & !(1 << bit) };
        bus.write8(a, nv);
        base
    }

    fn bbr_bbs(&mut self, bus: &mut dyn Bus, bit: u8, set: bool) -> u64 {
        let zp = self.fetch8(bus);
        let v = bus.read8(self.zp_addr(zp));
        let off = self.fetch8(bus) as i8 as i16;
        let isset = v & (1 << bit) != 0;
        let take = if set { isset } else { !isset };
        if take {
            self.pc = (self.pc as i16).wrapping_add(off) as u16;
            8
        } else {
            6
        }
    }

    fn branch(&mut self, bus: &mut dyn Bus, take: bool) -> u64 {
        let (target, _) = self.operand_addr(bus, Mode::Rel);
        if take {
            let cross = page_crossed(self.pc, target);
            self.pc = target;
            4 + cross as u64
        } else {
            2
        }
    }

    // ---- block transfers (TII/TDD/TIN/TIA/TAI) ----
    fn block_xfer(&mut self, bus: &mut dyn Bus, op: BlockOp) -> u64 {
        let mut src = self.fetch16(bus);
        let mut dst = self.fetch16(bus);
        let len = self.fetch16(bus);
        let count = if len == 0 { 0x10000u32 } else { len as u32 };
        // For TIA/TAI the destination/source "alternates" between dst and dst+1.
        let mut alt = false;
        for _ in 0..count {
            let v = bus.read8(src);
            bus.write8(dst, v);
            match op {
                BlockOp::Tii => { src = src.wrapping_add(1); dst = dst.wrapping_add(1); }
                BlockOp::Tdd => { src = src.wrapping_sub(1); dst = dst.wrapping_sub(1); }
                BlockOp::Tin => { src = src.wrapping_add(1); /* dst fixed */ }
                BlockOp::Tia => {
                    // src increments; dst alternates between two addresses.
                    src = src.wrapping_add(1);
                    dst = if alt { dst.wrapping_sub(1) } else { dst.wrapping_add(1) };
                    alt = !alt;
                }
                BlockOp::Tai => {
                    // dst increments; src alternates between two addresses.
                    dst = dst.wrapping_add(1);
                    src = if alt { src.wrapping_sub(1) } else { src.wrapping_add(1) };
                    alt = !alt;
                }
            }
        }
        // 17 base cycles + 6 per byte (approx per the datasheet).
        17 + 6 * count as u64
    }

    // ---- ALU primitives ----
    fn adc(&mut self, v: u8) {
        let c = self.flag(FLAG_C) as u16;
        if self.flag(FLAG_D) {
            // Decimal mode (the HuC6280 implements 65C02-style BCD with valid
            // Z/N flags).
            let mut lo = (self.a & 0x0F) as u16 + (v & 0x0F) as u16 + c;
            let mut hi = (self.a >> 4) as u16 + (v >> 4) as u16;
            if lo > 9 { lo += 6; hi += 1; }
            if hi > 9 { hi += 6; }
            let result = ((hi << 4) | (lo & 0x0F)) as u8;
            self.set_flag(FLAG_C, hi > 0x0F);
            self.set_zn(result);
            self.a = result;
        } else {
            let sum = self.a as u16 + v as u16 + c;
            let result = sum as u8;
            self.set_flag(FLAG_C, sum > 0xFF);
            self.set_flag(FLAG_V, (self.a ^ result) & (v ^ result) & 0x80 != 0);
            self.a = result;
            self.set_zn(result);
        }
    }
    fn sbc(&mut self, v: u8) {
        if self.flag(FLAG_D) {
            let c = self.flag(FLAG_C) as i16;
            let mut lo = (self.a & 0x0F) as i16 - (v & 0x0F) as i16 + c - 1;
            let mut hi = (self.a >> 4) as i16 - (v >> 4) as i16;
            if lo < 0 { lo += 10; hi -= 1; }
            if hi < 0 { hi += 10; }
            let result = (((hi as u16) << 4) | (lo as u16 & 0x0F)) as u8;
            let full = self.a as i16 - v as i16 + c - 1;
            self.set_flag(FLAG_C, full >= 0);
            self.set_zn(result);
            self.a = result;
        } else {
            self.adc(v ^ 0xFF);
        }
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
}

#[inline]
fn page_crossed(a: u16, b: u16) -> bool {
    (a & 0xFF00) != (b & 0xFF00)
}

// Logical I/O addresses the Pce bus maps to the VDC for the ST0/ST1/ST2
// instructions. The HuC6280's ST ops always target the VDC regardless of the
// MMU, so the bus recognises these specific logical addresses. We pick the
// canonical VDC port addresses (the I/O page is mapped at logical $0000 by
// hardware convention via MPR0 = bank $FF).
const VDC_ST_AR: u16 = 0x0000; // ST0 -> VDC address register
const VDC_ST_LO: u16 = 0x0002; // ST1 -> VDC data low
const VDC_ST_HI: u16 = 0x0003; // ST2 -> VDC data high

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
enum BlockOp {
    Tii,
    Tdd,
    Tin,
    Tia,
    Tai,
}

#[cfg(test)]
mod tests {
    use super::*;

    // A flat 64 KiB RAM bus with 8 MPRs for isolated CPU tests. Banking is a
    // no-op here (the logical address IS the physical address); MPRs are just
    // stored so TAM/TMA round-trips can be checked.
    struct FlatBus {
        ram: Vec<u8>,
        mpr: [u8; 8],
        // record ST writes for the ST0/1/2 tests.
        st: [u8; 4],
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus { ram: vec![0u8; 0x10000], mpr: [0; 8], st: [0; 4] }
        }
    }
    impl Bus for FlatBus {
        fn read8(&mut self, a: u16) -> u8 {
            self.ram[a as usize]
        }
        fn write8(&mut self, a: u16, v: u8) {
            match a {
                VDC_ST_AR => self.st[0] = v,
                VDC_ST_LO => self.st[2] = v,
                VDC_ST_HI => self.st[3] = v,
                _ => self.ram[a as usize] = v,
            }
        }
        fn set_mpr(&mut self, n: u8, v: u8) {
            self.mpr[n as usize] = v;
        }
        fn get_mpr(&self, n: u8) -> u8 {
            self.mpr[n as usize]
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
        assert!(cpu.flag(FLAG_V));
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_C));
    }

    #[test]
    fn sbc_basic() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0x50;
        cpu.set_flag(FLAG_C, true);
        bus.ram[0x8000] = 0xE9; // SBC #$10
        bus.ram[0x8001] = 0x10;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0x40);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn bra_always_taken() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0x80; // BRA +4
        bus.ram[0x8001] = 0x04;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x8006);
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
    fn fixed_indirect_jmp_no_page_bug() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0x6C; // JMP ($30FF)
        bus.ram[0x8001] = 0xFF;
        bus.ram[0x8002] = 0x30;
        bus.ram[0x30FF] = 0x40;
        bus.ram[0x3100] = 0x99; // 65C02: high byte from 0x3100 (no bug)
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x9940);
    }

    #[test]
    fn tam_tma_roundtrip() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0xAB;
        // TAM #$02 -> set MPR1 to 0xAB.
        bus.ram[0x8000] = 0x53;
        bus.ram[0x8001] = 0x02;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.get_mpr(1), 0xAB);
        // TMA #$02 -> read MPR1 back into A (clobber A first).
        cpu.a = 0;
        bus.ram[0x8002] = 0x43;
        bus.ram[0x8003] = 0x02;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0xAB);
    }

    #[test]
    fn tam_multi_bit_sets_all() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0x12;
        bus.ram[0x8000] = 0x53; // TAM #$FF (all 8 MPRs)
        bus.ram[0x8001] = 0xFF;
        run_one(&mut cpu, &mut bus);
        for n in 0..8 {
            assert_eq!(bus.get_mpr(n), 0x12);
        }
    }

    #[test]
    fn tii_block_copy() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        // TII $4000 -> $5000, len 4.
        bus.ram[0x8000] = 0x73;
        bus.ram[0x8001] = 0x00; bus.ram[0x8002] = 0x40; // src $4000
        bus.ram[0x8003] = 0x00; bus.ram[0x8004] = 0x50; // dst $5000
        bus.ram[0x8005] = 0x04; bus.ram[0x8006] = 0x00; // len 4
        bus.ram[0x4000] = 0xDE;
        bus.ram[0x4001] = 0xAD;
        bus.ram[0x4002] = 0xBE;
        bus.ram[0x4003] = 0xEF;
        run_one(&mut cpu, &mut bus);
        assert_eq!(&bus.ram[0x5000..0x5004], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn tdd_block_copy_descends() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        // TDD $4003 -> $5003, len 4 (copies descending).
        bus.ram[0x8000] = 0xC3;
        bus.ram[0x8001] = 0x03; bus.ram[0x8002] = 0x40;
        bus.ram[0x8003] = 0x03; bus.ram[0x8004] = 0x50;
        bus.ram[0x8005] = 0x04; bus.ram[0x8006] = 0x00;
        bus.ram[0x4000] = 1; bus.ram[0x4001] = 2;
        bus.ram[0x4002] = 3; bus.ram[0x4003] = 4;
        run_one(&mut cpu, &mut bus);
        assert_eq!(&bus.ram[0x5000..0x5004], &[1, 2, 3, 4]);
    }

    #[test]
    fn tin_dst_fixed() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        // TIN $4000 -> $5000, len 3 (dst stays put -> last byte wins).
        bus.ram[0x8000] = 0xD3;
        bus.ram[0x8001] = 0x00; bus.ram[0x8002] = 0x40;
        bus.ram[0x8003] = 0x00; bus.ram[0x8004] = 0x50;
        bus.ram[0x8005] = 0x03; bus.ram[0x8006] = 0x00;
        bus.ram[0x4000] = 0x11; bus.ram[0x4001] = 0x22; bus.ram[0x4002] = 0x33;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x5000], 0x33);
    }

    #[test]
    fn st0_writes_vdc_address_register() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0x03; // ST0 #$05
        bus.ram[0x8001] = 0x05;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.st[0], 0x05);
        // ST1 / ST2.
        bus.ram[0x8002] = 0x13; bus.ram[0x8003] = 0xAA; // ST1 #$AA
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.st[2], 0xAA);
        bus.ram[0x8004] = 0x23; bus.ram[0x8005] = 0xBB; // ST2 #$BB
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.st[3], 0xBB);
    }

    #[test]
    fn csl_csh_speed_switch() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x8000] = 0xD4; // CSH
        run_one(&mut cpu, &mut bus);
        assert!(cpu.high_speed);
        bus.ram[0x8001] = 0x54; // CSL
        run_one(&mut cpu, &mut bus);
        assert!(!cpu.high_speed);
    }

    #[test]
    fn smb_rmb_zero_page_bits() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        // SMB3 $10 -> set bit 3 at zp $10 (logical $2010).
        bus.ram[0x8000] = 0xB7; // SMB3
        bus.ram[0x8001] = 0x10;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x2010] & 0x08, 0x08);
        // RMB3 $10 -> clear it.
        bus.ram[0x8002] = 0x37; // RMB3
        bus.ram[0x8003] = 0x10;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x2010] & 0x08, 0x00);
    }

    #[test]
    fn bbs_branches_on_set_bit() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x2010] = 0x80; // bit 7 set
        bus.ram[0x8000] = 0xFF; // BBS7 $10, +4
        bus.ram[0x8001] = 0x10;
        bus.ram[0x8002] = 0x04;
        run_one(&mut cpu, &mut bus);
        // pc after fetching 3 bytes = 0x8003, +4 = 0x8007.
        assert_eq!(cpu.pc, 0x8007);
    }

    #[test]
    fn stz_clears_memory() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        bus.ram[0x4000] = 0xFF;
        bus.ram[0x8000] = 0x9C; // STZ $4000
        bus.ram[0x8001] = 0x00;
        bus.ram[0x8002] = 0x40;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x4000], 0);
    }

    #[test]
    fn tsb_trb_sets_and_clears() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.a = 0x0F;
        bus.ram[0x4000] = 0xF0;
        bus.ram[0x8000] = 0x0C; // TSB $4000
        bus.ram[0x8001] = 0x00;
        bus.ram[0x8002] = 0x40;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x4000], 0xFF);
        assert!(cpu.flag(FLAG_Z)); // 0x0F & 0xF0 == 0
        // TRB clears the bits.
        bus.ram[0x8003] = 0x1C; // TRB $4000
        bus.ram[0x8004] = 0x00;
        bus.ram[0x8005] = 0x40;
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.ram[0x4000], 0xF0);
    }

    #[test]
    fn swap_instructions() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.x = 1; cpu.y = 2;
        bus.ram[0x8000] = 0x02; // SXY
        run_one(&mut cpu, &mut bus);
        assert_eq!((cpu.x, cpu.y), (2, 1));
    }

    #[test]
    fn irq1_vectors_when_enabled() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.set_flag(FLAG_I, false);
        cpu.irq1_line = true;
        bus.ram[IRQ1_VECTOR as usize] = 0x00;
        bus.ram[IRQ1_VECTOR as usize + 1] = 0x90;
        let c = cpu.step(&mut bus);
        assert_eq!(c, 8);
        assert_eq!(cpu.pc, 0x9000);
        assert!(cpu.flag(FLAG_I));
    }

    #[test]
    fn irq_masked_by_i_flag() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.set_flag(FLAG_I, true);
        cpu.irq1_line = true;
        bus.ram[0x8000] = 0xEA; // NOP — IRQ must NOT fire
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x8001);
    }

    #[test]
    fn decimal_adc() {
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu.set_flag(FLAG_D, true);
        cpu.a = 0x09;
        bus.ram[0x8000] = 0x69; // ADC #$01 -> BCD 0x10
        bus.ram[0x8001] = 0x01;
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a, 0x10);
    }
}
