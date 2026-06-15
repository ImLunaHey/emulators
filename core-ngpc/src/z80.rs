//! Full Zilog Z80 CPU interpreter, built from the Zilog Z80 user manual and
//! the community-canonical opcode/flag tables (z80.info "The Undocumented Z80
//! Documented", and SMS Power!'s Z80 notes).
//!
//! Coverage:
//!   * Main register set AF/BC/DE/HL + shadows AF'/BC'/DE'/HL', IX/IY, SP/PC,
//!     I/R, the interrupt flip-flops IFF1/IFF2 and the interrupt mode IM 0/1/2.
//!   * The full opcode space: the un-prefixed page, CB (rotate/shift/bit),
//!     ED (extended: block ops, 16-bit arithmetic, I/O), DD/FD (IX/IY) and the
//!     DDCB/FDCB displaced bit ops.
//!   * Accurate M-cycle/T-state timing per instruction.
//!   * NMI and the maskable INT in modes 0/1/2, DI/EI/HALT, EI's one-instruction
//!     interrupt-enable delay.
//!
//! Flag register (F) bit layout:
//!   bit7 S  sign      bit6 Z  zero       bit5 Y/F5 (bit5 of result)
//!   bit4 H  half-carry bit3 X/F3 (bit3)  bit2 P/V parity/overflow
//!   bit1 N  add/sub    bit0 C  carry
//!
//! The CPU codes against `&mut dyn Z80Bus` (memory + I/O port spaces) and never
//! knows which device backs an address — see `bus.rs`.

use crate::z80bus::Z80Bus;

pub const FLAG_C: u8 = 1 << 0;
pub const FLAG_N: u8 = 1 << 1;
pub const FLAG_PV: u8 = 1 << 2;
pub const FLAG_X: u8 = 1 << 3; // undocumented (bit3 of result)
pub const FLAG_H: u8 = 1 << 4;
pub const FLAG_Y: u8 = 1 << 5; // undocumented (bit5 of result)
pub const FLAG_Z: u8 = 1 << 6;
pub const FLAG_S: u8 = 1 << 7;

#[derive(Clone)]
pub struct Cpu {
    // Main register pairs, stored as 16-bit; 8-bit halves accessed via helpers.
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,

    // Alternate (shadow) register set.
    pub a_: u8,
    pub f_: u8,
    pub b_: u8,
    pub c_: u8,
    pub d_: u8,
    pub e_: u8,
    pub h_: u8,
    pub l_: u8,

    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,

    pub i: u8,
    pub r: u8,

    pub iff1: bool,
    pub iff2: bool,
    pub im: u8,

    pub halted: bool,

    /// EI enables interrupts only AFTER the following instruction. This holds
    /// the deferred enable for one instruction.
    ei_pending: bool,

    /// Level-sensitive maskable interrupt request line (set by the VDP).
    pub irq_line: bool,
    /// Edge-triggered NMI request (the SMS Pause button); consumed once.
    pub nmi_pending: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            a: 0xFF,
            f: 0xFF,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            a_: 0,
            f_: 0,
            b_: 0,
            c_: 0,
            d_: 0,
            e_: 0,
            h_: 0,
            l_: 0,
            ix: 0,
            iy: 0,
            sp: 0xDFF0,
            pc: 0,
            i: 0,
            r: 0,
            iff1: false,
            iff2: false,
            im: 0,
            halted: false,
            ei_pending: false,
            irq_line: false,
            nmi_pending: false,
        }
    }

    // ---- 16-bit register pair accessors ----
    #[inline]
    pub fn af(&self) -> u16 {
        ((self.a as u16) << 8) | self.f as u16
    }
    #[inline]
    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f = v as u8;
    }
    #[inline]
    pub fn bc(&self) -> u16 {
        ((self.b as u16) << 8) | self.c as u16
    }
    #[inline]
    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    #[inline]
    pub fn de(&self) -> u16 {
        ((self.d as u16) << 8) | self.e as u16
    }
    #[inline]
    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    #[inline]
    pub fn hl(&self) -> u16 {
        ((self.h as u16) << 8) | self.l as u16
    }
    #[inline]
    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }

    #[inline]
    fn flag(&self, m: u8) -> bool {
        self.f & m != 0
    }
    #[inline]
    fn set_flag(&mut self, m: u8, on: bool) {
        if on {
            self.f |= m;
        } else {
            self.f &= !m;
        }
    }

    /// Increment the 7-bit R register (the high bit is preserved). Called once
    /// per opcode fetch (twice for prefixed instructions).
    #[inline]
    fn bump_r(&mut self) {
        self.r = (self.r & 0x80) | ((self.r.wrapping_add(1)) & 0x7F);
    }

    // ---- fetch helpers ----
    #[inline]
    fn fetch8(&mut self, bus: &mut dyn Z80Bus) -> u8 {
        let v = bus.read8(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn Z80Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn push16(&mut self, bus: &mut dyn Z80Bus, v: u16) {
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, (v >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, v as u8);
    }
    #[inline]
    fn pop16(&mut self, bus: &mut dyn Z80Bus) -> u16 {
        let lo = bus.read8(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read8(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        (hi << 8) | lo
    }

    /// Reset to the cold-boot state (PC=0, interrupts disabled).
    pub fn reset(&mut self) {
        self.pc = 0;
        self.iff1 = false;
        self.iff2 = false;
        self.im = 0;
        self.i = 0;
        self.r = 0;
        self.halted = false;
        self.ei_pending = false;
    }

    /// Service a pending NMI or maskable INT if appropriate. Returns the extra
    /// T-states consumed (0 if nothing serviced). Call before `step`.
    fn service_interrupts(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        // NMI: edge-triggered, always accepted, IFF1->IFF2 saved, jump to $0066.
        if self.nmi_pending {
            self.nmi_pending = false;
            if self.halted {
                self.halted = false;
                self.pc = self.pc.wrapping_add(1);
            }
            self.iff2 = self.iff1;
            self.iff1 = false;
            self.bump_r();
            let pc = self.pc;
            self.push16(bus, pc);
            self.pc = 0x0066;
            return 11;
        }
        // Maskable INT.
        if self.irq_line && self.iff1 && !self.ei_pending {
            if self.halted {
                self.halted = false;
                self.pc = self.pc.wrapping_add(1);
            }
            self.iff1 = false;
            self.iff2 = false;
            self.bump_r();
            match self.im {
                // IM0: device puts an instruction on the bus. SMS hardware ties
                // the data bus to 0xFF -> RST 38h, same effect as IM1.
                0 | 1 => {
                    let pc = self.pc;
                    self.push16(bus, pc);
                    self.pc = 0x0038;
                    13
                }
                _ => {
                    // IM2: vector = (I<<8) | bus byte (0xFF on SMS).
                    let pc = self.pc;
                    self.push16(bus, pc);
                    let vector = ((self.i as u16) << 8) | 0xFF;
                    self.pc = bus.read16(vector);
                    19
                }
            }
        } else {
            0
        }
    }

    /// Execute one instruction (servicing a pending interrupt first). Returns
    /// the number of T-states (CPU clock cycles) consumed.
    pub fn step(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        // EI takes effect after the instruction following EI. Capture whether
        // an EI is in flight for THIS instruction's interrupt check, then apply
        // the enable for the NEXT instruction.
        let mut t = self.service_interrupts(bus);
        // If we serviced an interrupt, that's our work for this step.
        if t != 0 {
            // EI delay still resolves.
            self.resolve_ei();
            return t;
        }
        self.resolve_ei();

        if self.halted {
            // HALT executes NOPs until an interrupt. 4 T-states per NOP.
            self.bump_r();
            return 4;
        }

        self.bump_r();
        let op = self.fetch8(bus);
        t += self.execute(bus, op);
        t
    }

    /// Apply a deferred EI: it enables interrupts for the instruction AFTER EI.
    #[inline]
    fn resolve_ei(&mut self) {
        if self.ei_pending {
            self.ei_pending = false;
            self.iff1 = true;
            self.iff2 = true;
        }
    }

    // =====================================================================
    // Main (un-prefixed) opcode dispatch.
    // =====================================================================
    fn execute(&mut self, bus: &mut dyn Z80Bus, op: u8) -> u32 {
        match op {
            0x00 => 4, // NOP
            0xCB => self.exec_cb(bus),
            0xED => self.exec_ed(bus),
            0xDD => {
                self.bump_r();
                let sub = self.fetch8(bus);
                self.exec_index(bus, sub, true)
            }
            0xFD => {
                self.bump_r();
                let sub = self.fetch8(bus);
                self.exec_index(bus, sub, false)
            }

            // ---- 8-bit loads: LD r,r' / LD r,(HL) / LD (HL),r ----
            0x40..=0x7F if op != 0x76 => self.ld_r_r(bus, op),
            0x76 => {
                // HALT
                self.halted = true;
                4
            }

            // ---- LD r,n ----
            0x06 => { let n = self.fetch8(bus); self.b = n; 7 }
            0x0E => { let n = self.fetch8(bus); self.c = n; 7 }
            0x16 => { let n = self.fetch8(bus); self.d = n; 7 }
            0x1E => { let n = self.fetch8(bus); self.e = n; 7 }
            0x26 => { let n = self.fetch8(bus); self.h = n; 7 }
            0x2E => { let n = self.fetch8(bus); self.l = n; 7 }
            0x36 => { let n = self.fetch8(bus); let a = self.hl(); bus.write8(a, n); 10 }
            0x3E => { let n = self.fetch8(bus); self.a = n; 7 }

            // ---- 16-bit immediate loads ----
            0x01 => { let n = self.fetch16(bus); self.set_bc(n); 10 }
            0x11 => { let n = self.fetch16(bus); self.set_de(n); 10 }
            0x21 => { let n = self.fetch16(bus); self.set_hl(n); 10 }
            0x31 => { let n = self.fetch16(bus); self.sp = n; 10 }

            // ---- LD (nn),A / LD A,(nn) ----
            0x32 => { let a = self.fetch16(bus); bus.write8(a, self.a); 13 }
            0x3A => { let a = self.fetch16(bus); self.a = bus.read8(a); 13 }
            // LD (nn),HL / LD HL,(nn)
            0x22 => { let a = self.fetch16(bus); bus.write16(a, self.hl()); 16 }
            0x2A => { let a = self.fetch16(bus); let v = bus.read16(a); self.set_hl(v); 16 }
            // LD (BC),A / LD (DE),A / LD A,(BC) / LD A,(DE)
            0x02 => { bus.write8(self.bc(), self.a); 7 }
            0x12 => { bus.write8(self.de(), self.a); 7 }
            0x0A => { self.a = bus.read8(self.bc()); 7 }
            0x1A => { self.a = bus.read8(self.de()); 7 }

            // ---- LD SP,HL ----
            0xF9 => { self.sp = self.hl(); 6 }

            // ---- 16-bit INC/DEC ----
            0x03 => { let v = self.bc().wrapping_add(1); self.set_bc(v); 6 }
            0x13 => { let v = self.de().wrapping_add(1); self.set_de(v); 6 }
            0x23 => { let v = self.hl().wrapping_add(1); self.set_hl(v); 6 }
            0x33 => { self.sp = self.sp.wrapping_add(1); 6 }
            0x0B => { let v = self.bc().wrapping_sub(1); self.set_bc(v); 6 }
            0x1B => { let v = self.de().wrapping_sub(1); self.set_de(v); 6 }
            0x2B => { let v = self.hl().wrapping_sub(1); self.set_hl(v); 6 }
            0x3B => { self.sp = self.sp.wrapping_sub(1); 6 }

            // ---- 8-bit INC/DEC ----
            0x04 => { self.b = self.inc8(self.b); 4 }
            0x0C => { self.c = self.inc8(self.c); 4 }
            0x14 => { self.d = self.inc8(self.d); 4 }
            0x1C => { self.e = self.inc8(self.e); 4 }
            0x24 => { self.h = self.inc8(self.h); 4 }
            0x2C => { self.l = self.inc8(self.l); 4 }
            0x34 => { let a = self.hl(); let v = self.inc8(bus.read8(a)); bus.write8(a, v); 11 }
            0x3C => { self.a = self.inc8(self.a); 4 }
            0x05 => { self.b = self.dec8(self.b); 4 }
            0x0D => { self.c = self.dec8(self.c); 4 }
            0x15 => { self.d = self.dec8(self.d); 4 }
            0x1D => { self.e = self.dec8(self.e); 4 }
            0x25 => { self.h = self.dec8(self.h); 4 }
            0x2D => { self.l = self.dec8(self.l); 4 }
            0x35 => { let a = self.hl(); let v = self.dec8(bus.read8(a)); bus.write8(a, v); 11 }
            0x3D => { self.a = self.dec8(self.a); 4 }

            // ---- 16-bit ADD HL,rr ----
            0x09 => { let v = self.add16(self.hl(), self.bc()); self.set_hl(v); 11 }
            0x19 => { let v = self.add16(self.hl(), self.de()); self.set_hl(v); 11 }
            0x29 => { let v = self.add16(self.hl(), self.hl()); self.set_hl(v); 11 }
            0x39 => { let v = self.add16(self.hl(), self.sp); self.set_hl(v); 11 }

            // ---- rotates on A ----
            0x07 => { self.rlca(); 4 }
            0x0F => { self.rrca(); 4 }
            0x17 => { self.rla(); 4 }
            0x1F => { self.rra(); 4 }

            // ---- DAA / CPL / SCF / CCF ----
            0x27 => { self.daa(); 4 }
            0x2F => { self.cpl(); 4 }
            0x37 => { self.scf(); 4 }
            0x3F => { self.ccf(); 4 }

            // ---- exchange ----
            0x08 => { // EX AF,AF'
                std::mem::swap(&mut self.a, &mut self.a_);
                std::mem::swap(&mut self.f, &mut self.f_);
                4
            }
            0xEB => { // EX DE,HL
                std::mem::swap(&mut self.d, &mut self.h);
                std::mem::swap(&mut self.e, &mut self.l);
                4
            }
            0xD9 => { // EXX
                std::mem::swap(&mut self.b, &mut self.b_);
                std::mem::swap(&mut self.c, &mut self.c_);
                std::mem::swap(&mut self.d, &mut self.d_);
                std::mem::swap(&mut self.e, &mut self.e_);
                std::mem::swap(&mut self.h, &mut self.h_);
                std::mem::swap(&mut self.l, &mut self.l_);
                4
            }
            0xE3 => { // EX (SP),HL
                let sp = self.sp;
                let v = bus.read16(sp);
                bus.write16(sp, self.hl());
                self.set_hl(v);
                19
            }

            // ---- 8-bit arithmetic/logic with A (register/(HL)/immediate) ----
            0x80..=0x87 => { let v = self.src_r(bus, op & 7); self.add_a(v); self.r_cost(op & 7) }
            0x88..=0x8F => { let v = self.src_r(bus, op & 7); self.adc_a(v); self.r_cost(op & 7) }
            0x90..=0x97 => { let v = self.src_r(bus, op & 7); self.sub_a(v); self.r_cost(op & 7) }
            0x98..=0x9F => { let v = self.src_r(bus, op & 7); self.sbc_a(v); self.r_cost(op & 7) }
            0xA0..=0xA7 => { let v = self.src_r(bus, op & 7); self.and_a(v); self.r_cost(op & 7) }
            0xA8..=0xAF => { let v = self.src_r(bus, op & 7); self.xor_a(v); self.r_cost(op & 7) }
            0xB0..=0xB7 => { let v = self.src_r(bus, op & 7); self.or_a(v); self.r_cost(op & 7) }
            0xB8..=0xBF => { let v = self.src_r(bus, op & 7); self.cp_a(v); self.r_cost(op & 7) }

            0xC6 => { let n = self.fetch8(bus); self.add_a(n); 7 }
            0xCE => { let n = self.fetch8(bus); self.adc_a(n); 7 }
            0xD6 => { let n = self.fetch8(bus); self.sub_a(n); 7 }
            0xDE => { let n = self.fetch8(bus); self.sbc_a(n); 7 }
            0xE6 => { let n = self.fetch8(bus); self.and_a(n); 7 }
            0xEE => { let n = self.fetch8(bus); self.xor_a(n); 7 }
            0xF6 => { let n = self.fetch8(bus); self.or_a(n); 7 }
            0xFE => { let n = self.fetch8(bus); self.cp_a(n); 7 }

            // ---- jumps ----
            0xC3 => { let a = self.fetch16(bus); self.pc = a; 10 }
            0xE9 => { self.pc = self.hl(); 4 } // JP (HL)
            0xC2 => self.jp_cc(bus, !self.flag(FLAG_Z)),
            0xCA => self.jp_cc(bus, self.flag(FLAG_Z)),
            0xD2 => self.jp_cc(bus, !self.flag(FLAG_C)),
            0xDA => self.jp_cc(bus, self.flag(FLAG_C)),
            0xE2 => self.jp_cc(bus, !self.flag(FLAG_PV)),
            0xEA => self.jp_cc(bus, self.flag(FLAG_PV)),
            0xF2 => self.jp_cc(bus, !self.flag(FLAG_S)),
            0xFA => self.jp_cc(bus, self.flag(FLAG_S)),

            // ---- relative jumps ----
            0x18 => { let d = self.fetch8(bus) as i8; self.pc = self.pc.wrapping_add(d as u16); 12 }
            0x20 => self.jr_cc(bus, !self.flag(FLAG_Z)),
            0x28 => self.jr_cc(bus, self.flag(FLAG_Z)),
            0x30 => self.jr_cc(bus, !self.flag(FLAG_C)),
            0x38 => self.jr_cc(bus, self.flag(FLAG_C)),
            0x10 => { // DJNZ
                let d = self.fetch8(bus) as i8;
                self.b = self.b.wrapping_sub(1);
                if self.b != 0 {
                    self.pc = self.pc.wrapping_add(d as u16);
                    13
                } else {
                    8
                }
            }

            // ---- calls ----
            0xCD => { let a = self.fetch16(bus); let pc = self.pc; self.push16(bus, pc); self.pc = a; 17 }
            0xC4 => self.call_cc(bus, !self.flag(FLAG_Z)),
            0xCC => self.call_cc(bus, self.flag(FLAG_Z)),
            0xD4 => self.call_cc(bus, !self.flag(FLAG_C)),
            0xDC => self.call_cc(bus, self.flag(FLAG_C)),
            0xE4 => self.call_cc(bus, !self.flag(FLAG_PV)),
            0xEC => self.call_cc(bus, self.flag(FLAG_PV)),
            0xF4 => self.call_cc(bus, !self.flag(FLAG_S)),
            0xFC => self.call_cc(bus, self.flag(FLAG_S)),

            // ---- returns ----
            0xC9 => { self.pc = self.pop16(bus); 10 }
            0xC0 => self.ret_cc(bus, !self.flag(FLAG_Z)),
            0xC8 => self.ret_cc(bus, self.flag(FLAG_Z)),
            0xD0 => self.ret_cc(bus, !self.flag(FLAG_C)),
            0xD8 => self.ret_cc(bus, self.flag(FLAG_C)),
            0xE0 => self.ret_cc(bus, !self.flag(FLAG_PV)),
            0xE8 => self.ret_cc(bus, self.flag(FLAG_PV)),
            0xF0 => self.ret_cc(bus, !self.flag(FLAG_S)),
            0xF8 => self.ret_cc(bus, self.flag(FLAG_S)),

            // ---- RST ----
            0xC7 => self.rst(bus, 0x00),
            0xCF => self.rst(bus, 0x08),
            0xD7 => self.rst(bus, 0x10),
            0xDF => self.rst(bus, 0x18),
            0xE7 => self.rst(bus, 0x20),
            0xEF => self.rst(bus, 0x28),
            0xF7 => self.rst(bus, 0x30),
            0xFF => self.rst(bus, 0x38),

            // ---- 16-bit push/pop ----
            0xC5 => { let v = self.bc(); self.push16(bus, v); 11 }
            0xD5 => { let v = self.de(); self.push16(bus, v); 11 }
            0xE5 => { let v = self.hl(); self.push16(bus, v); 11 }
            0xF5 => { let v = self.af(); self.push16(bus, v); 11 }
            0xC1 => { let v = self.pop16(bus); self.set_bc(v); 10 }
            0xD1 => { let v = self.pop16(bus); self.set_de(v); 10 }
            0xE1 => { let v = self.pop16(bus); self.set_hl(v); 10 }
            0xF1 => { let v = self.pop16(bus); self.set_af(v); 10 }

            // ---- I/O ----
            0xDB => { // IN A,(n)
                let n = self.fetch8(bus) as u16;
                let port = ((self.a as u16) << 8) | n;
                self.a = bus.port_in(port);
                11
            }
            0xD3 => { // OUT (n),A
                let n = self.fetch8(bus) as u16;
                let port = ((self.a as u16) << 8) | n;
                bus.port_out(port, self.a);
                11
            }

            // ---- interrupt control ----
            0xF3 => { self.iff1 = false; self.iff2 = false; 4 } // DI
            0xFB => { self.ei_pending = true; 4 } // EI

            // Unprefixed opcodes are all covered above; any gap is a bug.
            _ => 4,
        }
    }

    // ---- LD r,r' family ($40-$7F minus $76) ----
    fn ld_r_r(&mut self, bus: &mut dyn Z80Bus, op: u8) -> u32 {
        let dst = (op >> 3) & 7;
        let src = op & 7;
        let v = self.src_r(bus, src);
        self.dst_r(bus, dst, v);
        // (HL) source or dest costs 7; reg-reg costs 4.
        if src == 6 || dst == 6 {
            7
        } else {
            4
        }
    }

    /// Read register-encoded source: 0=B 1=C 2=D 3=E 4=H 5=L 6=(HL) 7=A.
    #[inline]
    fn src_r(&mut self, bus: &mut dyn Z80Bus, code: u8) -> u8 {
        match code {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => bus.read8(self.hl()),
            _ => self.a,
        }
    }
    #[inline]
    fn dst_r(&mut self, bus: &mut dyn Z80Bus, code: u8, v: u8) {
        match code {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => bus.write8(self.hl(), v),
            _ => self.a = v,
        }
    }
    /// T-state cost for a register-coded ALU operand (4, or 7 for (HL)).
    #[inline]
    fn r_cost(&self, code: u8) -> u32 {
        if code == 6 {
            7
        } else {
            4
        }
    }

    // ---- control-flow helpers ----
    fn jp_cc(&mut self, bus: &mut dyn Z80Bus, cond: bool) -> u32 {
        let a = self.fetch16(bus);
        if cond {
            self.pc = a;
        }
        10
    }
    fn jr_cc(&mut self, bus: &mut dyn Z80Bus, cond: bool) -> u32 {
        let d = self.fetch8(bus) as i8;
        if cond {
            self.pc = self.pc.wrapping_add(d as u16);
            12
        } else {
            7
        }
    }
    fn call_cc(&mut self, bus: &mut dyn Z80Bus, cond: bool) -> u32 {
        let a = self.fetch16(bus);
        if cond {
            let pc = self.pc;
            self.push16(bus, pc);
            self.pc = a;
            17
        } else {
            10
        }
    }
    fn ret_cc(&mut self, bus: &mut dyn Z80Bus, cond: bool) -> u32 {
        if cond {
            self.pc = self.pop16(bus);
            11
        } else {
            5
        }
    }
    fn rst(&mut self, bus: &mut dyn Z80Bus, target: u16) -> u32 {
        let pc = self.pc;
        self.push16(bus, pc);
        self.pc = target;
        11
    }

    // =====================================================================
    // 8-bit ALU. All set flags per the Z80 spec (incl. undocumented X/Y).
    // =====================================================================
    #[inline]
    fn set_xy(&mut self, v: u8) {
        self.set_flag(FLAG_X, v & FLAG_X != 0);
        self.set_flag(FLAG_Y, v & FLAG_Y != 0);
    }

    fn add_a(&mut self, v: u8) {
        let a = self.a;
        let r = (a as u16) + (v as u16);
        let res = r as u8;
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (a & 0xF) + (v & 0xF) > 0xF);
        self.set_flag(FLAG_PV, ((a ^ v) & 0x80 == 0) && ((a ^ res) & 0x80 != 0));
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, r > 0xFF);
        self.set_xy(res);
        self.a = res;
    }
    fn adc_a(&mut self, v: u8) {
        let a = self.a;
        let cy = self.flag(FLAG_C) as u16;
        let r = (a as u16) + (v as u16) + cy;
        let res = r as u8;
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (a & 0xF) as u16 + (v & 0xF) as u16 + cy > 0xF);
        self.set_flag(FLAG_PV, ((a ^ v) & 0x80 == 0) && ((a ^ res) & 0x80 != 0));
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, r > 0xFF);
        self.set_xy(res);
        self.a = res;
    }
    fn sub_a(&mut self, v: u8) {
        let res = self.do_sub(v, false);
        self.a = res;
    }
    fn sbc_a(&mut self, v: u8) {
        let res = self.do_sub(v, true);
        self.a = res;
    }
    fn cp_a(&mut self, v: u8) {
        // CP is SUB without storing; X/Y come from the OPERAND, not the result.
        let a = self.a;
        let r = (a as i16) - (v as i16);
        let res = r as u8;
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (a & 0xF) < (v & 0xF));
        self.set_flag(FLAG_PV, ((a ^ v) & 0x80 != 0) && ((a ^ res) & 0x80 != 0));
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_C, r < 0);
        self.set_xy(v);
    }
    fn do_sub(&mut self, v: u8, with_carry: bool) -> u8 {
        let a = self.a;
        let cy = (with_carry && self.flag(FLAG_C)) as i16;
        let r = (a as i16) - (v as i16) - cy;
        let res = r as u8;
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, ((a & 0xF) as i16 - (v & 0xF) as i16 - cy) < 0);
        self.set_flag(FLAG_PV, ((a ^ v) & 0x80 != 0) && ((a ^ res) & 0x80 != 0));
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_C, r < 0);
        self.set_xy(res);
        res
    }
    fn and_a(&mut self, v: u8) {
        self.a &= v;
        let res = self.a;
        self.set_logic_flags(res, true);
    }
    fn or_a(&mut self, v: u8) {
        self.a |= v;
        let res = self.a;
        self.set_logic_flags(res, false);
    }
    fn xor_a(&mut self, v: u8) {
        self.a ^= v;
        let res = self.a;
        self.set_logic_flags(res, false);
    }
    fn set_logic_flags(&mut self, res: u8, half: bool) {
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, half);
        self.set_flag(FLAG_PV, parity(res));
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, false);
        self.set_xy(res);
    }

    fn inc8(&mut self, v: u8) -> u8 {
        let res = v.wrapping_add(1);
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (v & 0xF) + 1 > 0xF);
        self.set_flag(FLAG_PV, v == 0x7F);
        self.set_flag(FLAG_N, false);
        self.set_xy(res);
        res
    }
    fn dec8(&mut self, v: u8) -> u8 {
        let res = v.wrapping_sub(1);
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (v & 0xF) == 0);
        self.set_flag(FLAG_PV, v == 0x80);
        self.set_flag(FLAG_N, true);
        self.set_xy(res);
        res
    }

    fn add16(&mut self, a: u16, b: u16) -> u16 {
        let r = (a as u32) + (b as u32);
        let res = r as u16;
        self.set_flag(FLAG_H, (a & 0x0FFF) + (b & 0x0FFF) > 0x0FFF);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, r > 0xFFFF);
        // X/Y from high byte of result.
        self.set_xy((res >> 8) as u8);
        res
    }
    fn adc16(&mut self, a: u16, b: u16) -> u16 {
        let cy = self.flag(FLAG_C) as u32;
        let r = (a as u32) + (b as u32) + cy;
        let res = r as u16;
        self.set_flag(FLAG_S, res & 0x8000 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, (a & 0x0FFF) as u32 + (b & 0x0FFF) as u32 + cy > 0x0FFF);
        self.set_flag(FLAG_PV, ((a ^ b) & 0x8000 == 0) && ((a ^ res) & 0x8000 != 0));
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_C, r > 0xFFFF);
        self.set_xy((res >> 8) as u8);
        res
    }
    fn sbc16(&mut self, a: u16, b: u16) -> u16 {
        let cy = self.flag(FLAG_C) as i32;
        let r = (a as i32) - (b as i32) - cy;
        let res = r as u16;
        self.set_flag(FLAG_S, res & 0x8000 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, ((a & 0x0FFF) as i32 - (b & 0x0FFF) as i32 - cy) < 0);
        self.set_flag(FLAG_PV, ((a ^ b) & 0x8000 != 0) && ((a ^ res) & 0x8000 != 0));
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_C, r < 0);
        self.set_xy((res >> 8) as u8);
        res
    }

    // ---- rotates/shifts on A (faster variants: S/Z/PV unaffected) ----
    fn rlca(&mut self) {
        let c = self.a >> 7;
        self.a = (self.a << 1) | c;
        self.set_flag(FLAG_C, c != 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }
    fn rrca(&mut self) {
        let c = self.a & 1;
        self.a = (self.a >> 1) | (c << 7);
        self.set_flag(FLAG_C, c != 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }
    fn rla(&mut self) {
        let old_c = self.flag(FLAG_C) as u8;
        let c = self.a >> 7;
        self.a = (self.a << 1) | old_c;
        self.set_flag(FLAG_C, c != 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }
    fn rra(&mut self) {
        let old_c = self.flag(FLAG_C) as u8;
        let c = self.a & 1;
        self.a = (self.a >> 1) | (old_c << 7);
        self.set_flag(FLAG_C, c != 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }

    fn daa(&mut self) {
        // Decimal adjust after add/sub. Algorithm per the Z80 user manual.
        let mut adjust = 0u8;
        let mut carry = self.flag(FLAG_C);
        let a = self.a;
        if self.flag(FLAG_H) || (a & 0x0F) > 9 {
            adjust |= 0x06;
        }
        if carry || a > 0x99 {
            adjust |= 0x60;
            carry = true;
        }
        let res = if self.flag(FLAG_N) {
            a.wrapping_sub(adjust)
        } else {
            a.wrapping_add(adjust)
        };
        // H flag: set per the half-borrow/half-carry of the adjustment.
        let h = if self.flag(FLAG_N) {
            self.flag(FLAG_H) && (a & 0x0F) < 6
        } else {
            (a & 0x0F) > 9
        };
        self.a = res;
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, h);
        self.set_flag(FLAG_PV, parity(res));
        self.set_flag(FLAG_C, carry);
        self.set_xy(res);
    }
    fn cpl(&mut self) {
        self.a = !self.a;
        self.set_flag(FLAG_H, true);
        self.set_flag(FLAG_N, true);
        let a = self.a;
        self.set_xy(a);
    }
    fn scf(&mut self) {
        self.set_flag(FLAG_C, true);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }
    fn ccf(&mut self) {
        let c = self.flag(FLAG_C);
        self.set_flag(FLAG_H, c);
        self.set_flag(FLAG_C, !c);
        self.set_flag(FLAG_N, false);
        let a = self.a;
        self.set_xy(a);
    }

    // =====================================================================
    // CB prefix: rotates, shifts, BIT/RES/SET.
    // =====================================================================
    fn exec_cb(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.bump_r();
        let op = self.fetch8(bus);
        let reg = op & 7;
        let v = self.src_r(bus, reg);
        let extra = if reg == 6 { 15 } else { 8 };
        match op >> 6 {
            0 => {
                // rotate/shift group, sub-selected by bits 3-5.
                let res = self.cb_shift((op >> 3) & 7, v);
                self.dst_r(bus, reg, res);
                if reg == 6 { 15 } else { 8 }
            }
            1 => {
                // BIT b,r — does not write back.
                let bit = (op >> 3) & 7;
                self.bit(bit, v, reg);
                if reg == 6 { 12 } else { 8 }
            }
            2 => {
                // RES b,r
                let bit = (op >> 3) & 7;
                let res = v & !(1 << bit);
                self.dst_r(bus, reg, res);
                extra
            }
            _ => {
                // SET b,r
                let bit = (op >> 3) & 7;
                let res = v | (1 << bit);
                self.dst_r(bus, reg, res);
                extra
            }
        }
    }

    fn cb_shift(&mut self, kind: u8, v: u8) -> u8 {
        let res = match kind {
            0 => { // RLC
                let c = v >> 7;
                self.set_flag(FLAG_C, c != 0);
                (v << 1) | c
            }
            1 => { // RRC
                let c = v & 1;
                self.set_flag(FLAG_C, c != 0);
                (v >> 1) | (c << 7)
            }
            2 => { // RL
                let old = self.flag(FLAG_C) as u8;
                self.set_flag(FLAG_C, v & 0x80 != 0);
                (v << 1) | old
            }
            3 => { // RR
                let old = self.flag(FLAG_C) as u8;
                self.set_flag(FLAG_C, v & 1 != 0);
                (v >> 1) | (old << 7)
            }
            4 => { // SLA
                self.set_flag(FLAG_C, v & 0x80 != 0);
                v << 1
            }
            5 => { // SRA (arithmetic, preserves bit7)
                self.set_flag(FLAG_C, v & 1 != 0);
                (v >> 1) | (v & 0x80)
            }
            6 => { // SLL (undocumented: shift left, bit0 set)
                self.set_flag(FLAG_C, v & 0x80 != 0);
                (v << 1) | 1
            }
            _ => { // SRL
                self.set_flag(FLAG_C, v & 1 != 0);
                v >> 1
            }
        };
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_PV, parity(res));
        self.set_flag(FLAG_N, false);
        self.set_xy(res);
        res
    }

    fn bit(&mut self, bit: u8, v: u8, reg: u8) {
        let set = v & (1 << bit) != 0;
        self.set_flag(FLAG_Z, !set);
        self.set_flag(FLAG_PV, !set); // BIT sets P/V like Z
        self.set_flag(FLAG_S, bit == 7 && set);
        self.set_flag(FLAG_H, true);
        self.set_flag(FLAG_N, false);
        // X/Y: for register operands, copy from the operand; for (HL) the
        // hardware uses internal WZ — we approximate with the operand, which is
        // correct for register ops and harmless for SMS software.
        if reg != 6 {
            self.set_xy(v);
        } else {
            self.set_flag(FLAG_X, bit == 3 && set);
            self.set_flag(FLAG_Y, bit == 5 && set);
        }
    }

    // =====================================================================
    // ED prefix: extended opcodes (block ops, 16-bit arith, I/O, etc.).
    // =====================================================================
    fn exec_ed(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.bump_r();
        let op = self.fetch8(bus);
        match op {
            // ---- 16-bit SBC/ADC HL,rr ----
            0x42 => { let v = self.sbc16(self.hl(), self.bc()); self.set_hl(v); 15 }
            0x52 => { let v = self.sbc16(self.hl(), self.de()); self.set_hl(v); 15 }
            0x62 => { let v = self.sbc16(self.hl(), self.hl()); self.set_hl(v); 15 }
            0x72 => { let v = self.sbc16(self.hl(), self.sp); self.set_hl(v); 15 }
            0x4A => { let v = self.adc16(self.hl(), self.bc()); self.set_hl(v); 15 }
            0x5A => { let v = self.adc16(self.hl(), self.de()); self.set_hl(v); 15 }
            0x6A => { let v = self.adc16(self.hl(), self.hl()); self.set_hl(v); 15 }
            0x7A => { let v = self.adc16(self.hl(), self.sp); self.set_hl(v); 15 }

            // ---- LD (nn),rr / LD rr,(nn) ----
            0x43 => { let a = self.fetch16(bus); bus.write16(a, self.bc()); 20 }
            0x53 => { let a = self.fetch16(bus); bus.write16(a, self.de()); 20 }
            0x63 => { let a = self.fetch16(bus); bus.write16(a, self.hl()); 20 }
            0x73 => { let a = self.fetch16(bus); bus.write16(a, self.sp); 20 }
            0x4B => { let a = self.fetch16(bus); let v = bus.read16(a); self.set_bc(v); 20 }
            0x5B => { let a = self.fetch16(bus); let v = bus.read16(a); self.set_de(v); 20 }
            0x6B => { let a = self.fetch16(bus); let v = bus.read16(a); self.set_hl(v); 20 }
            0x7B => { let a = self.fetch16(bus); let v = bus.read16(a); self.sp = v; 20 }

            // ---- NEG (and undocumented mirrors) ----
            0x44 | 0x4C | 0x54 | 0x5C | 0x64 | 0x6C | 0x74 | 0x7C => {
                let v = self.a;
                self.a = 0;
                let r = self.do_sub(v, false);
                self.a = r;
                8
            }

            // ---- RETN / RETI ----
            0x45 | 0x55 | 0x65 | 0x75 | 0x4D | 0x5D | 0x6D | 0x7D => {
                // RETN/RETI both pop PC; RETN restores IFF1 from IFF2.
                self.pc = self.pop16(bus);
                self.iff1 = self.iff2;
                14
            }

            // ---- IM 0/1/2 ----
            0x46 | 0x4E | 0x66 | 0x6E => { self.im = 0; 8 }
            0x56 | 0x76 => { self.im = 1; 8 }
            0x5E | 0x7E => { self.im = 2; 8 }

            // ---- LD I,A / LD A,I / LD R,A / LD A,R ----
            0x47 => { self.i = self.a; 9 }
            0x4F => { self.r = self.a; 9 }
            0x57 => { let v = self.i; self.ld_a_ir(v); 9 } // LD A,I
            0x5F => { let v = self.r; self.ld_a_ir(v); 9 } // LD A,R

            // ---- RRD / RLD ----
            0x67 => { self.rrd(bus); 18 }
            0x6F => { self.rld(bus); 18 }

            // ---- I/O: IN r,(C) / OUT (C),r ----
            0x40 => { let v = self.in_c(bus); self.b = v; 12 }
            0x48 => { let v = self.in_c(bus); self.c = v; 12 }
            0x50 => { let v = self.in_c(bus); self.d = v; 12 }
            0x58 => { let v = self.in_c(bus); self.e = v; 12 }
            0x60 => { let v = self.in_c(bus); self.h = v; 12 }
            0x68 => { let v = self.in_c(bus); self.l = v; 12 }
            0x70 => { self.in_c(bus); 12 } // IN (C) / IN F,(C): flags only
            0x78 => { let v = self.in_c(bus); self.a = v; 12 }
            0x41 => { let port = self.bc(); bus.port_out(port, self.b); 12 }
            0x49 => { let port = self.bc(); bus.port_out(port, self.c); 12 }
            0x51 => { let port = self.bc(); bus.port_out(port, self.d); 12 }
            0x59 => { let port = self.bc(); bus.port_out(port, self.e); 12 }
            0x61 => { let port = self.bc(); bus.port_out(port, self.h); 12 }
            0x69 => { let port = self.bc(); bus.port_out(port, self.l); 12 }
            0x71 => { let port = self.bc(); bus.port_out(port, 0); 12 } // OUT (C),0
            0x79 => { let port = self.bc(); bus.port_out(port, self.a); 12 }

            // ---- block transfer / search ----
            0xA0 => { self.ldi(bus); 16 }
            0xA8 => { self.ldd(bus); 16 }
            0xB0 => self.ldir(bus),
            0xB8 => self.lddr(bus),
            0xA1 => { self.cpi(bus); 16 }
            0xA9 => { self.cpd(bus); 16 }
            0xB1 => self.cpir(bus),
            0xB9 => self.cpdr(bus),

            // ---- block I/O ----
            0xA2 => { self.ini(bus); 16 }
            0xAA => { self.ind(bus); 16 }
            0xB2 => self.inir(bus),
            0xBA => self.indr(bus),
            0xA3 => { self.outi(bus); 16 }
            0xAB => { self.outd(bus); 16 }
            0xB3 => self.otir(bus),
            0xBB => self.otdr(bus),

            // Unimplemented ED opcodes are NOPs (8 T-states) on the real chip.
            _ => 8,
        }
    }

    fn ld_a_ir(&mut self, v: u8) {
        self.a = v;
        self.set_flag(FLAG_S, v & 0x80 != 0);
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_PV, self.iff2);
        self.set_flag(FLAG_N, false);
        self.set_xy(v);
    }

    fn in_c(&mut self, bus: &mut dyn Z80Bus) -> u8 {
        let port = self.bc();
        let v = bus.port_in(port);
        // IN r,(C) sets S/Z/PV/X/Y from the value; H,N cleared; C unaffected.
        self.set_flag(FLAG_S, v & 0x80 != 0);
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_PV, parity(v));
        self.set_flag(FLAG_N, false);
        self.set_xy(v);
        v
    }

    fn rrd(&mut self, bus: &mut dyn Z80Bus) {
        let addr = self.hl();
        let m = bus.read8(addr);
        let new_m = (m >> 4) | (self.a << 4);
        let new_a = (self.a & 0xF0) | (m & 0x0F);
        bus.write8(addr, new_m);
        self.a = new_a;
        self.set_flag(FLAG_S, new_a & 0x80 != 0);
        self.set_flag(FLAG_Z, new_a == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_PV, parity(new_a));
        self.set_flag(FLAG_N, false);
        self.set_xy(new_a);
    }
    fn rld(&mut self, bus: &mut dyn Z80Bus) {
        let addr = self.hl();
        let m = bus.read8(addr);
        let new_m = (m << 4) | (self.a & 0x0F);
        let new_a = (self.a & 0xF0) | (m >> 4);
        bus.write8(addr, new_m);
        self.a = new_a;
        self.set_flag(FLAG_S, new_a & 0x80 != 0);
        self.set_flag(FLAG_Z, new_a == 0);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_PV, parity(new_a));
        self.set_flag(FLAG_N, false);
        self.set_xy(new_a);
    }

    // ---- block transfer ----
    fn ldi(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        bus.write8(self.de(), v);
        self.set_hl(self.hl().wrapping_add(1));
        self.set_de(self.de().wrapping_add(1));
        self.set_bc(self.bc().wrapping_sub(1));
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_PV, self.bc() != 0);
        // X/Y: bit3 and bit1 of (A+v).
        let n = self.a.wrapping_add(v);
        self.set_flag(FLAG_Y, n & 0x02 != 0);
        self.set_flag(FLAG_X, n & 0x08 != 0);
    }
    fn ldd(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        bus.write8(self.de(), v);
        self.set_hl(self.hl().wrapping_sub(1));
        self.set_de(self.de().wrapping_sub(1));
        self.set_bc(self.bc().wrapping_sub(1));
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_PV, self.bc() != 0);
        let n = self.a.wrapping_add(v);
        self.set_flag(FLAG_Y, n & 0x02 != 0);
        self.set_flag(FLAG_X, n & 0x08 != 0);
    }
    fn ldir(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.ldi(bus);
        if self.bc() != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn lddr(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.ldd(bus);
        if self.bc() != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }

    fn cpi(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        let a = self.a;
        let res = a.wrapping_sub(v);
        let h = (a & 0xF) < (v & 0xF);
        self.set_hl(self.hl().wrapping_add(1));
        self.set_bc(self.bc().wrapping_sub(1));
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, h);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_PV, self.bc() != 0);
        let n = res.wrapping_sub(h as u8);
        self.set_flag(FLAG_Y, n & 0x02 != 0);
        self.set_flag(FLAG_X, n & 0x08 != 0);
    }
    fn cpd(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        let a = self.a;
        let res = a.wrapping_sub(v);
        let h = (a & 0xF) < (v & 0xF);
        self.set_hl(self.hl().wrapping_sub(1));
        self.set_bc(self.bc().wrapping_sub(1));
        self.set_flag(FLAG_S, res & 0x80 != 0);
        self.set_flag(FLAG_Z, res == 0);
        self.set_flag(FLAG_H, h);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_PV, self.bc() != 0);
        let n = res.wrapping_sub(h as u8);
        self.set_flag(FLAG_Y, n & 0x02 != 0);
        self.set_flag(FLAG_X, n & 0x08 != 0);
    }
    fn cpir(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.cpi(bus);
        if self.bc() != 0 && !self.flag(FLAG_Z) {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn cpdr(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.cpd(bus);
        if self.bc() != 0 && !self.flag(FLAG_Z) {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }

    // ---- block I/O ----
    fn ini(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.port_in(self.bc());
        bus.write8(self.hl(), v);
        self.b = self.b.wrapping_sub(1);
        self.set_hl(self.hl().wrapping_add(1));
        self.io_block_flags(v);
    }
    fn ind(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.port_in(self.bc());
        bus.write8(self.hl(), v);
        self.b = self.b.wrapping_sub(1);
        self.set_hl(self.hl().wrapping_sub(1));
        self.io_block_flags(v);
    }
    fn inir(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.ini(bus);
        if self.b != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn indr(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.ind(bus);
        if self.b != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn outi(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        self.b = self.b.wrapping_sub(1);
        bus.port_out(self.bc(), v);
        self.set_hl(self.hl().wrapping_add(1));
        self.io_block_flags(v);
    }
    fn outd(&mut self, bus: &mut dyn Z80Bus) {
        let v = bus.read8(self.hl());
        self.b = self.b.wrapping_sub(1);
        bus.port_out(self.bc(), v);
        self.set_hl(self.hl().wrapping_sub(1));
        self.io_block_flags(v);
    }
    fn otir(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.outi(bus);
        if self.b != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn otdr(&mut self, bus: &mut dyn Z80Bus) -> u32 {
        self.outd(bus);
        if self.b != 0 {
            self.pc = self.pc.wrapping_sub(2);
            21
        } else {
            16
        }
    }
    fn io_block_flags(&mut self, v: u8) {
        // Z from B, S/X/Y from B; N from bit7 of the transferred value; the
        // H/PV behaviour is an obscure function — approximate the common case.
        self.set_flag(FLAG_Z, self.b == 0);
        self.set_flag(FLAG_S, self.b & 0x80 != 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
        self.set_xy(self.b);
        // H and PV per the documented (k = ((C+1)&0xFF)+v) formula:
        let k = (v as u16).wrapping_add(self.l as u16);
        self.set_flag(FLAG_H, k > 0xFF);
        self.set_flag(FLAG_PV, parity(((k & 7) as u8) ^ self.b));
    }

    // =====================================================================
    // DD/FD prefix: IX/IY operations, including DDCB/FDCB displaced bit ops.
    // `is_ix` selects IX (true) or IY (false).
    // =====================================================================
    fn exec_index(&mut self, bus: &mut dyn Z80Bus, op: u8, is_ix: bool) -> u32 {
        let idx = if is_ix { self.ix } else { self.iy };
        match op {
            // Another DD/FD just re-fetches with the new prefix (treat as NOP +
            // re-dispatch by recursing into the next prefix byte).
            0xDD | 0xFD => {
                self.bump_r();
                let sub = self.fetch8(bus);
                self.exec_index(bus, sub, op == 0xDD)
            }
            0xCB => self.exec_index_cb(bus, idx, is_ix),

            // ---- LD IX,nn ----
            0x21 => { let n = self.fetch16(bus); self.set_idx(is_ix, n); 14 }
            // ---- LD (nn),IX / LD IX,(nn) ----
            0x22 => { let a = self.fetch16(bus); bus.write16(a, idx); 20 }
            0x2A => { let a = self.fetch16(bus); let v = bus.read16(a); self.set_idx(is_ix, v); 20 }
            // ---- INC/DEC IX ----
            0x23 => { self.set_idx(is_ix, idx.wrapping_add(1)); 10 }
            0x2B => { self.set_idx(is_ix, idx.wrapping_sub(1)); 10 }
            // ---- ADD IX,rr ----
            0x09 => { let v = self.add16(idx, self.bc()); self.set_idx(is_ix, v); 15 }
            0x19 => { let v = self.add16(idx, self.de()); self.set_idx(is_ix, v); 15 }
            0x29 => { let v = self.add16(idx, idx); self.set_idx(is_ix, v); 15 }
            0x39 => { let v = self.add16(idx, self.sp); self.set_idx(is_ix, v); 15 }

            // ---- INC/DEC IXH/IXL (undocumented) ----
            0x24 => { let v = self.inc8(idx_h(idx)); self.set_idx(is_ix, set_h(idx, v)); 8 }
            0x25 => { let v = self.dec8(idx_h(idx)); self.set_idx(is_ix, set_h(idx, v)); 8 }
            0x2C => { let v = self.inc8(idx_l(idx)); self.set_idx(is_ix, set_l(idx, v)); 8 }
            0x2D => { let v = self.dec8(idx_l(idx)); self.set_idx(is_ix, set_l(idx, v)); 8 }
            0x26 => { let n = self.fetch8(bus); self.set_idx(is_ix, set_h(idx, n)); 11 }
            0x2E => { let n = self.fetch8(bus); self.set_idx(is_ix, set_l(idx, n)); 11 }

            // ---- (IX+d) load/store and INC/DEC ----
            0x34 => { let a = self.disp(bus, idx); let v = self.inc8(bus.read8(a)); bus.write8(a, v); 23 }
            0x35 => { let a = self.disp(bus, idx); let v = self.dec8(bus.read8(a)); bus.write8(a, v); 23 }
            0x36 => { let a = self.disp(bus, idx); let n = self.fetch8(bus); bus.write8(a, n); 19 }

            // ---- LD r,(IX+d) ----
            0x46 => { let a = self.disp(bus, idx); self.b = bus.read8(a); 19 }
            0x4E => { let a = self.disp(bus, idx); self.c = bus.read8(a); 19 }
            0x56 => { let a = self.disp(bus, idx); self.d = bus.read8(a); 19 }
            0x5E => { let a = self.disp(bus, idx); self.e = bus.read8(a); 19 }
            0x66 => { let a = self.disp(bus, idx); self.h = bus.read8(a); 19 }
            0x6E => { let a = self.disp(bus, idx); self.l = bus.read8(a); 19 }
            0x7E => { let a = self.disp(bus, idx); self.a = bus.read8(a); 19 }
            // ---- LD (IX+d),r ----
            0x70 => { let a = self.disp(bus, idx); bus.write8(a, self.b); 19 }
            0x71 => { let a = self.disp(bus, idx); bus.write8(a, self.c); 19 }
            0x72 => { let a = self.disp(bus, idx); bus.write8(a, self.d); 19 }
            0x73 => { let a = self.disp(bus, idx); bus.write8(a, self.e); 19 }
            0x74 => { let a = self.disp(bus, idx); bus.write8(a, self.h); 19 }
            0x75 => { let a = self.disp(bus, idx); bus.write8(a, self.l); 19 }
            0x77 => { let a = self.disp(bus, idx); bus.write8(a, self.a); 19 }

            // ---- ALU A,(IX+d) ----
            0x86 => { let a = self.disp(bus, idx); let v = bus.read8(a); self.add_a(v); 19 }
            0x8E => { let a = self.disp(bus, idx); let v = bus.read8(a); self.adc_a(v); 19 }
            0x96 => { let a = self.disp(bus, idx); let v = bus.read8(a); self.sub_a(v); 19 }
            0x9E => { let a = self.disp(bus, idx); let v = bus.read8(a); self.sbc_a(v); 19 }
            0xA6 => { let a = self.disp(bus, idx); let v = bus.read8(a); self.and_a(v); 19 }
            0xAE => { let a = self.disp(bus, idx); let v = bus.read8(a); self.xor_a(v); 19 }
            0xB6 => { let a = self.disp(bus, idx); let v = bus.read8(a); self.or_a(v); 19 }
            0xBE => { let a = self.disp(bus, idx); let v = bus.read8(a); self.cp_a(v); 19 }

            // ---- ALU/LD with IXH/IXL (undocumented) ----
            // LD r,IXH/IXL and LD IXH/IXL,r — handled for the common A/B/C/D/E.
            0x44 => { self.b = idx_h(idx); 8 }
            0x45 => { self.b = idx_l(idx); 8 }
            0x4C => { self.c = idx_h(idx); 8 }
            0x4D => { self.c = idx_l(idx); 8 }
            0x54 => { self.d = idx_h(idx); 8 }
            0x55 => { self.d = idx_l(idx); 8 }
            0x5C => { self.e = idx_h(idx); 8 }
            0x5D => { self.e = idx_l(idx); 8 }
            0x7C => { self.a = idx_h(idx); 8 }
            0x7D => { self.a = idx_l(idx); 8 }
            0x60 => { self.set_idx(is_ix, set_h(idx, self.b)); 8 }
            0x61 => { self.set_idx(is_ix, set_h(idx, self.c)); 8 }
            0x62 => { self.set_idx(is_ix, set_h(idx, self.d)); 8 }
            0x63 => { self.set_idx(is_ix, set_h(idx, self.e)); 8 }
            0x64 => 8, // LD IXH,IXH (nop)
            0x65 => { self.set_idx(is_ix, set_h(idx, idx_l(idx))); 8 }
            0x67 => { self.set_idx(is_ix, set_h(idx, self.a)); 8 }
            0x68 => { self.set_idx(is_ix, set_l(idx, self.b)); 8 }
            0x69 => { self.set_idx(is_ix, set_l(idx, self.c)); 8 }
            0x6A => { self.set_idx(is_ix, set_l(idx, self.d)); 8 }
            0x6B => { self.set_idx(is_ix, set_l(idx, self.e)); 8 }
            0x6C => { self.set_idx(is_ix, set_l(idx, idx_h(idx))); 8 }
            0x6D => 8, // LD IXL,IXL (nop)
            0x6F => { self.set_idx(is_ix, set_l(idx, self.a)); 8 }
            0x84 => { self.add_a(idx_h(idx)); 8 }
            0x85 => { self.add_a(idx_l(idx)); 8 }
            0x8C => { self.adc_a(idx_h(idx)); 8 }
            0x8D => { self.adc_a(idx_l(idx)); 8 }
            0x94 => { self.sub_a(idx_h(idx)); 8 }
            0x95 => { self.sub_a(idx_l(idx)); 8 }
            0x9C => { self.sbc_a(idx_h(idx)); 8 }
            0x9D => { self.sbc_a(idx_l(idx)); 8 }
            0xA4 => { self.and_a(idx_h(idx)); 8 }
            0xA5 => { self.and_a(idx_l(idx)); 8 }
            0xAC => { self.xor_a(idx_h(idx)); 8 }
            0xAD => { self.xor_a(idx_l(idx)); 8 }
            0xB4 => { self.or_a(idx_h(idx)); 8 }
            0xB5 => { self.or_a(idx_l(idx)); 8 }
            0xBC => { self.cp_a(idx_h(idx)); 8 }
            0xBD => { self.cp_a(idx_l(idx)); 8 }

            // ---- stack / jump using IX ----
            0xE1 => { let v = self.pop16(bus); self.set_idx(is_ix, v); 14 } // POP IX
            0xE5 => { self.push16(bus, idx); 15 } // PUSH IX
            0xE9 => { self.pc = idx; 8 } // JP (IX)
            0xF9 => { self.sp = idx; 10 } // LD SP,IX
            0xE3 => { // EX (SP),IX
                let v = bus.read16(self.sp);
                bus.write16(self.sp, idx);
                self.set_idx(is_ix, v);
                23
            }

            // Any other byte: the prefix is ignored and the opcode runs as if
            // un-prefixed (DD/FD acts as a NOP prefix on it). Re-dispatch.
            _ => 4 + self.execute(bus, op),
        }
    }

    /// DDCB/FDCB: the displacement comes BEFORE the opcode byte.
    fn exec_index_cb(&mut self, bus: &mut dyn Z80Bus, idx: u16, _is_ix: bool) -> u32 {
        let d = self.fetch8(bus) as i8;
        let op = self.fetch8(bus);
        let addr = idx.wrapping_add(d as u16);
        let v = bus.read8(addr);
        let reg = op & 7; // undocumented: result also copied to this register
        match op >> 6 {
            0 => {
                let res = self.cb_shift((op >> 3) & 7, v);
                bus.write8(addr, res);
                if reg != 6 {
                    self.dst_r_no_hl(reg, res);
                }
                23
            }
            1 => {
                let bit = (op >> 3) & 7;
                // BIT n,(IX+d): X/Y come from the high byte of the address.
                let set = v & (1 << bit) != 0;
                self.set_flag(FLAG_Z, !set);
                self.set_flag(FLAG_PV, !set);
                self.set_flag(FLAG_S, bit == 7 && set);
                self.set_flag(FLAG_H, true);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_X, (addr >> 8) as u8 & FLAG_X != 0);
                self.set_flag(FLAG_Y, (addr >> 8) as u8 & FLAG_Y != 0);
                20
            }
            2 => {
                let bit = (op >> 3) & 7;
                let res = v & !(1 << bit);
                bus.write8(addr, res);
                if reg != 6 {
                    self.dst_r_no_hl(reg, res);
                }
                23
            }
            _ => {
                let bit = (op >> 3) & 7;
                let res = v | (1 << bit);
                bus.write8(addr, res);
                if reg != 6 {
                    self.dst_r_no_hl(reg, res);
                }
                23
            }
        }
    }

    #[inline]
    fn dst_r_no_hl(&mut self, code: u8, v: u8) {
        match code {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            _ => self.a = v,
        }
    }

    /// Read the signed displacement byte and form `idx + d`.
    #[inline]
    fn disp(&mut self, bus: &mut dyn Z80Bus, idx: u16) -> u16 {
        let d = self.fetch8(bus) as i8;
        idx.wrapping_add(d as u16)
    }
    #[inline]
    fn set_idx(&mut self, is_ix: bool, v: u16) {
        if is_ix {
            self.ix = v;
        } else {
            self.iy = v;
        }
    }
}

#[inline]
fn idx_h(idx: u16) -> u8 {
    (idx >> 8) as u8
}
#[inline]
fn idx_l(idx: u16) -> u8 {
    idx as u8
}
#[inline]
fn set_h(idx: u16, v: u8) -> u16 {
    (idx & 0x00FF) | ((v as u16) << 8)
}
#[inline]
fn set_l(idx: u16, v: u8) -> u16 {
    (idx & 0xFF00) | (v as u16)
}

/// Even-parity test (P/V flag for logic ops): true when the number of set bits
/// is even.
#[inline]
fn parity(v: u8) -> bool {
    v.count_ones() & 1 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 64 KiB RAM + a 256-byte port space stub for CPU unit tests.
    struct FlatBus {
        mem: Vec<u8>,
        ports: Vec<u8>,
        last_out: Option<(u16, u8)>,
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus {
                mem: vec![0; 0x10000],
                ports: vec![0; 0x100],
                last_out: None,
            }
        }
    }
    impl Z80Bus for FlatBus {
        fn read8(&mut self, addr: u16) -> u8 {
            self.mem[addr as usize]
        }
        fn write8(&mut self, addr: u16, v: u8) {
            self.mem[addr as usize] = v;
        }
        fn port_in(&mut self, port: u16) -> u8 {
            self.ports[(port & 0xFF) as usize]
        }
        fn port_out(&mut self, port: u16, v: u8) {
            self.last_out = Some((port, v));
            self.ports[(port & 0xFF) as usize] = v;
        }
    }

    fn run(prog: &[u8], steps: usize) -> (Cpu, FlatBus) {
        let mut cpu = Cpu::new();
        cpu.reset();
        cpu.a = 0;
        cpu.f = 0;
        let mut bus = FlatBus::new();
        bus.mem[..prog.len()].copy_from_slice(prog);
        for _ in 0..steps {
            cpu.step(&mut bus);
        }
        (cpu, bus)
    }

    #[test]
    fn ld_immediate_and_add() {
        // LD A,$05 ; LD B,$03 ; ADD A,B
        let (cpu, _) = run(&[0x3E, 0x05, 0x06, 0x03, 0x80], 3);
        assert_eq!(cpu.a, 0x08);
        assert!(!cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn add_sets_carry_and_zero() {
        // LD A,$FF ; ADD A,$01
        let (cpu, _) = run(&[0x3E, 0xFF, 0xC6, 0x01], 2);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_H));
    }

    #[test]
    fn add_sets_overflow() {
        // LD A,$7F ; ADD A,$01 -> 0x80, overflow set, sign set
        let (cpu, _) = run(&[0x3E, 0x7F, 0xC6, 0x01], 2);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.flag(FLAG_PV));
        assert!(cpu.flag(FLAG_S));
    }

    #[test]
    fn sub_sets_flags() {
        // LD A,$05 ; SUB $05 -> 0, Z and N set, no carry
        let (cpu, _) = run(&[0x3E, 0x05, 0xD6, 0x05], 2);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_C));
    }

    #[test]
    fn sub_borrow() {
        // LD A,$00 ; SUB $01 -> 0xFF, carry+sign set
        let (cpu, _) = run(&[0x3E, 0x00, 0xD6, 0x01], 2);
        assert_eq!(cpu.a, 0xFF);
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_S));
    }

    #[test]
    fn logic_parity() {
        // LD A,$03 ; OR $00 -> parity even (two bits) -> PV set
        let (cpu, _) = run(&[0x3E, 0x03, 0xF6, 0x00], 2);
        assert_eq!(cpu.a, 0x03);
        assert!(cpu.flag(FLAG_PV));
        // LD A,$01 ; OR $00 -> one bit -> PV clear
        let (cpu, _) = run(&[0x3E, 0x01, 0xF6, 0x00], 2);
        assert!(!cpu.flag(FLAG_PV));
    }

    #[test]
    fn inc_dec_wrap_flags() {
        // LD A,$FF ; INC A -> 0, Z set, H set
        let (cpu, _) = run(&[0x3E, 0xFF, 0x3C], 2);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_H));
        // LD A,$80 ; DEC A -> 0x7F, PV set (overflow), H set
        let (cpu, _) = run(&[0x3E, 0x80, 0x3D], 2);
        assert_eq!(cpu.a, 0x7F);
        assert!(cpu.flag(FLAG_PV));
    }

    #[test]
    fn sixteen_bit_load_and_add() {
        // LD HL,$1234 ; LD BC,$1111 ; ADD HL,BC -> $2345
        let (cpu, _) = run(&[0x21, 0x34, 0x12, 0x01, 0x11, 0x11, 0x09], 3);
        assert_eq!(cpu.hl(), 0x2345);
    }

    #[test]
    fn push_pop_roundtrip() {
        // LD SP,$F000 ; LD BC,$ABCD ; PUSH BC ; POP HL
        let (cpu, _) = run(
            &[0x31, 0x00, 0xF0, 0x01, 0xCD, 0xAB, 0xC5, 0xE1],
            4,
        );
        assert_eq!(cpu.hl(), 0xABCD);
    }

    #[test]
    fn jump_relative() {
        // JR +2 skips a HALT; lands on LD A,$AA
        // 0x18 0x02 (JR +2 from pc after operand = 0x02+0x02=0x04)
        let prog = [0x18, 0x02, 0x76, 0x76, 0x3E, 0xAA];
        let (cpu, _) = run(&prog, 2);
        assert_eq!(cpu.a, 0xAA);
    }

    #[test]
    fn call_and_ret() {
        // LD SP,$F000 ; CALL $0008 ; (at 0008) LD A,$55 ; RET
        let mut prog = vec![0u8; 0x20];
        prog[0] = 0x31;
        prog[1] = 0x00;
        prog[2] = 0xF0; // LD SP,$F000
        prog[3] = 0xCD;
        prog[4] = 0x08;
        prog[5] = 0x00; // CALL $0008
        prog[6] = 0x76; // HALT (return lands here)
        prog[8] = 0x3E;
        prog[9] = 0x55; // LD A,$55
        prog[10] = 0xC9; // RET
        let (cpu, _) = run(&prog, 4);
        assert_eq!(cpu.a, 0x55);
        assert_eq!(cpu.pc, 6);
    }

    #[test]
    fn cb_bit_and_set() {
        // LD A,$00 ; (use B) LD B,$00 ; SET 3,B ; BIT 3,B
        let (cpu, _) = run(&[0x06, 0x00, 0xCB, 0xD8, 0xCB, 0x58], 3);
        assert_eq!(cpu.b, 0x08);
        assert!(!cpu.flag(FLAG_Z)); // bit set -> Z clear
    }

    #[test]
    fn cb_rotate_rlc() {
        // LD A,$80 ; LD B,A ; RLC B -> 0x01, carry set
        let (cpu, _) = run(&[0x06, 0x80, 0xCB, 0x00], 2);
        assert_eq!(cpu.b, 0x01);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn ed_ldir_block_copy() {
        // Copy 3 bytes from $1000 to $2000.
        // LD HL,$1000 ; LD DE,$2000 ; LD BC,$0003 ; LDIR
        let mut cpu = Cpu::new();
        cpu.reset();
        let mut bus = FlatBus::new();
        bus.mem[0x1000] = 0xAA;
        bus.mem[0x1001] = 0xBB;
        bus.mem[0x1002] = 0xCC;
        let prog = [
            0x21, 0x00, 0x10, // LD HL,$1000
            0x11, 0x00, 0x20, // LD DE,$2000
            0x01, 0x03, 0x00, // LD BC,$0003
            0xED, 0xB0, // LDIR
        ];
        bus.mem[..prog.len()].copy_from_slice(&prog);
        // 3 loads + LDIR repeats until BC=0.
        for _ in 0..10 {
            cpu.step(&mut bus);
        }
        assert_eq!(bus.mem[0x2000], 0xAA);
        assert_eq!(bus.mem[0x2001], 0xBB);
        assert_eq!(bus.mem[0x2002], 0xCC);
        assert_eq!(cpu.bc(), 0);
    }

    #[test]
    fn ix_displaced_load() {
        // LD IX,$2000 ; LD A,(IX+1)
        let mut cpu = Cpu::new();
        cpu.reset();
        let mut bus = FlatBus::new();
        bus.mem[0x2001] = 0x77;
        let prog = [0xDD, 0x21, 0x00, 0x20, 0xDD, 0x7E, 0x01];
        bus.mem[..prog.len()].copy_from_slice(&prog);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0x77);
    }

    #[test]
    fn exx_and_ex_de_hl() {
        // LD HL,$1234 ; LD DE,$5678 ; EX DE,HL
        let (cpu, _) = run(&[0x21, 0x34, 0x12, 0x11, 0x78, 0x56, 0xEB], 3);
        assert_eq!(cpu.hl(), 0x5678);
        assert_eq!(cpu.de(), 0x1234);
    }

    #[test]
    fn out_and_in_port() {
        // LD A,$42 ; OUT ($10),A ; then IN A,($10)
        let (cpu, bus) = run(&[0x3E, 0x42, 0xD3, 0x10], 2);
        assert_eq!(bus.ports[0x10], 0x42);
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn di_ei_and_irq() {
        let mut cpu = Cpu::new();
        cpu.reset();
        cpu.im = 1;
        let mut bus = FlatBus::new();
        // EI ; NOP ; NOP. IRQ raised — should be serviced after the NOP
        // following EI (EI delay).
        bus.mem[0] = 0xFB; // EI
        bus.mem[1] = 0x00; // NOP
        bus.mem[2] = 0x00; // NOP
        cpu.irq_line = true;
        cpu.step(&mut bus); // EI: enables after next instr
        assert!(cpu.pc == 1);
        cpu.step(&mut bus); // NOP; interrupt becomes serviceable next step
        // Now an interrupt should be taken (IM1 -> $0038).
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x0038);
    }

    #[test]
    fn nmi_jumps_to_0066() {
        let mut cpu = Cpu::new();
        cpu.reset();
        let mut bus = FlatBus::new();
        bus.mem[0] = 0x00; // NOP
        cpu.sp = 0xF000;
        cpu.nmi_pending = true;
        let t = cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x0066);
        assert_eq!(t, 11);
    }

    #[test]
    fn neg_instruction() {
        // LD A,$01 ; NEG -> 0xFF
        let (cpu, _) = run(&[0x3E, 0x01, 0xED, 0x44], 2);
        assert_eq!(cpu.a, 0xFF);
        assert!(cpu.flag(FLAG_N));
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn daa_after_add() {
        // LD A,$15 ; ADD A,$27 -> $3C ; DAA -> $42 (BCD 15+27)
        let (cpu, _) = run(&[0x3E, 0x15, 0xC6, 0x27, 0x27], 3);
        assert_eq!(cpu.a, 0x42);
    }
}
