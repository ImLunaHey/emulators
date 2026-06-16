//! LR35902 instruction decode + execute.
//!
//! Spec: Pan Docs — CPU Instruction Set + the opcode tables
//! (gbdev.io/pandocs, gbdev.io/gb-opcodes). This is a straightforward
//! interpreter: `step` fetches one opcode, dispatches it, and returns the
//! number of **T-cycles** consumed. M-cycles are 4 T-cycles each; the returned
//! counts match the canonical tables.
//!
//! Memory accesses go through `&mut dyn Bus` so the CPU never needs to know
//! which device backs an address. Interrupt servicing (the IME gate + push/jump)
//! lives in `Cpu::service_interrupt`; HALT/STOP wake-up and the EI delay are
//! handled here in `step`.

use super::state::{Cpu, Power, FLAG_C, FLAG_H, FLAG_N, FLAG_Z};
use crate::bus::Bus;
use crate::interrupts::Irq;

impl Cpu {
    // ---- byte/word fetch at PC ----
    #[inline]
    fn fetch8(&mut self, bus: &mut dyn Bus) -> u8 {
        let b = bus.read8(self.pc);
        // The HALT bug: PC fails to advance for one fetch.
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.pc = self.pc.wrapping_add(1);
        }
        b
    }
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        lo | (hi << 8)
    }

    // ---- stack ----
    #[inline]
    fn push16(&mut self, bus: &mut dyn Bus, v: u16) {
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, (v >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, v as u8);
    }
    #[inline]
    fn pop16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = bus.read8(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read8(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        lo | (hi << 8)
    }

    /// Drive the CPU one step: service interrupts / handle HALT, then execute
    /// one instruction. Returns the number of T-cycles consumed.
    pub fn step(&mut self, bus: &mut dyn Bus, irq: &mut Irq) -> u32 {
        // The EI delay: IME becomes true after the instruction following EI.
        let ime_was_pending = self.ime_pending;

        // Try to service an interrupt (only when IME set + pending).
        if self.service_interrupt(bus, irq).is_some() {
            // Interrupt dispatch takes 20 T-cycles (5 M-cycles).
            return 20;
        }

        // Apply the deferred EI now (after a non-interrupt-servicing step).
        if ime_was_pending {
            self.ime = true;
            self.ime_pending = false;
        }

        match self.power {
            Power::Halted => {
                // Stay halted until an interrupt is pending (IE & IF), even if
                // IME is clear. The wake itself is handled above / here.
                if irq.pending() != 0 {
                    self.power = Power::Running;
                    // Fall through to execute next instruction next step.
                }
                return 4;
            }
            Power::Stopped => {
                // STOP is exited by a joypad line going low. We treat any
                // pending interrupt as the wake condition; the double-speed
                // switch is handled where STOP is decoded.
                if irq.pending() != 0 {
                    self.power = Power::Running;
                }
                return 4;
            }
            Power::Running => {}
        }

        // PC of the instruction about to be fetched, so an illegal opcode can
        // report where it locked up.
        let instr_pc = self.pc;
        let had_illegal = self.illegal_op.is_some();
        let opcode = self.fetch8(bus);
        let cycles = self.execute(opcode, bus);
        // If `execute` just newly raised the illegal-opcode signal, tag it with
        // this instruction's PC for the crash readout (don't clobber an earlier
        // one on later instructions).
        if !had_illegal {
            if let Some((op, _)) = self.illegal_op {
                self.illegal_op = Some((op, instr_pc));
            }
        }
        cycles
    }

    // ---- flag helpers ----
    #[inline]
    fn set_zn(&mut self, z: bool, n: bool) {
        self.set_flag(FLAG_Z, z);
        self.set_flag(FLAG_N, n);
    }

    // ---- 8-bit ALU ----
    fn alu_add(&mut self, v: u8, carry: bool) {
        let c = if carry && self.flag(FLAG_C) { 1u16 } else { 0 };
        let a = self.a as u16;
        let r = a + v as u16 + c;
        let half = (self.a & 0xF) + (v & 0xF) + c as u8;
        self.a = r as u8;
        self.set_zn(self.a == 0, false);
        self.set_flag(FLAG_H, half > 0xF);
        self.set_flag(FLAG_C, r > 0xFF);
    }
    fn alu_sub(&mut self, v: u8, carry: bool) -> u8 {
        let c = if carry && self.flag(FLAG_C) { 1i16 } else { 0 };
        let a = self.a as i16;
        let r = a - v as i16 - c;
        let half = (self.a & 0xF) as i16 - (v & 0xF) as i16 - c;
        let res = r as u8;
        self.set_zn(res == 0, true);
        self.set_flag(FLAG_H, half < 0);
        self.set_flag(FLAG_C, r < 0);
        res
    }
    fn alu_and(&mut self, v: u8) {
        self.a &= v;
        self.set_zn(self.a == 0, false);
        self.set_flag(FLAG_H, true);
        self.set_flag(FLAG_C, false);
    }
    fn alu_or(&mut self, v: u8) {
        self.a |= v;
        self.set_zn(self.a == 0, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, false);
    }
    fn alu_xor(&mut self, v: u8) {
        self.a ^= v;
        self.set_zn(self.a == 0, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, false);
    }
    fn alu_cp(&mut self, v: u8) {
        let a = self.a;
        self.alu_sub(v, false);
        self.a = a; // CP discards the result
    }
    fn alu_inc(&mut self, v: u8) -> u8 {
        let r = v.wrapping_add(1);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (v & 0xF) + 1 > 0xF);
        r
    }
    fn alu_dec(&mut self, v: u8) -> u8 {
        let r = v.wrapping_sub(1);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_H, (v & 0xF) == 0);
        r
    }

    fn add_hl(&mut self, v: u16) {
        let hl = self.hl();
        let r = hl as u32 + v as u32;
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (hl & 0xFFF) + (v & 0xFFF) > 0xFFF);
        self.set_flag(FLAG_C, r > 0xFFFF);
        self.set_hl(r as u16);
    }

    /// SP + signed-immediate (used by 0xE8 and 0xF8). Flags from the low byte.
    fn add_sp_imm(&mut self, bus: &mut dyn Bus) -> u16 {
        let e = self.fetch8(bus) as i8 as i16 as u16;
        let sp = self.sp;
        let r = sp.wrapping_add(e);
        self.set_flag(FLAG_Z, false);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, (sp & 0xF) + (e & 0xF) > 0xF);
        self.set_flag(FLAG_C, (sp & 0xFF) + (e & 0xFF) > 0xFF);
        r
    }

    fn daa(&mut self) {
        let mut a = self.a as u16;
        if !self.flag(FLAG_N) {
            if self.flag(FLAG_C) || a > 0x99 {
                a += 0x60;
                self.set_flag(FLAG_C, true);
            }
            if self.flag(FLAG_H) || (a & 0x0F) > 0x09 {
                a += 0x06;
            }
        } else {
            if self.flag(FLAG_C) {
                a = a.wrapping_sub(0x60);
            }
            if self.flag(FLAG_H) {
                a = a.wrapping_sub(0x06);
            }
        }
        self.a = a as u8;
        self.set_flag(FLAG_Z, self.a == 0);
        self.set_flag(FLAG_H, false);
    }

    // ---- 8-bit register get/set by index (B C D E H L (HL) A) ----
    fn reg_get(&mut self, idx: u8, bus: &mut dyn Bus) -> u8 {
        match idx {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => bus.read8(self.hl()),
            7 => self.a,
            _ => unreachable!(),
        }
    }
    fn reg_set(&mut self, idx: u8, v: u8, bus: &mut dyn Bus) {
        match idx {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => bus.write8(self.hl(), v),
            7 => self.a = v,
            _ => unreachable!(),
        }
    }

    /// Decode + execute a single (already-fetched) opcode; returns T-cycles.
    fn execute(&mut self, op: u8, bus: &mut dyn Bus) -> u32 {
        match op {
            // ---- 0x00 NOP, misc control ----
            0x00 => 4,
            0x10 => {
                // STOP — on CGB, with KEY1 bit0 armed this performs the speed
                // switch (toggling KEY1 bit 7). The byte after STOP is consumed.
                let _ = self.fetch8(bus);
                let key1 = bus.read8(0xFF4D);
                if key1 & 0x01 != 0 {
                    // Perform the CGB speed switch. The bus interprets a KEY1
                    // write with both bit 7 and bit 0 set as "execute switch"
                    // (only STOP issues this); it toggles the current-speed bit
                    // and clears the armed bit. CPU continues running.
                    bus.write8(0xFF4D, 0x81);
                } else {
                    self.power = Power::Stopped;
                }
                4
            }
            0x76 => {
                // HALT. The HALT bug: IME=0 and a pending interrupt → no halt,
                // PC fails to increment on next fetch.
                if !self.ime && bus.read8(0xFF0F) & bus.read8(0xFFFF) & 0x1F != 0 {
                    self.halt_bug = true;
                } else {
                    self.power = Power::Halted;
                }
                4
            }
            0xF3 => {
                self.ime = false;
                self.ime_pending = false;
                4
            }
            0xFB => {
                self.ime_pending = true;
                4
            }

            // ---- 8-bit loads LD r,r' (0x40-0x7F except 0x76) ----
            0x40..=0x7F => {
                let dst = (op >> 3) & 0x07;
                let src = op & 0x07;
                let v = self.reg_get(src, bus);
                self.reg_set(dst, v, bus);
                // (HL) source or dest costs an extra M-cycle.
                if dst == 6 || src == 6 { 8 } else { 4 }
            }

            // ---- LD r,d8 ----
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let dst = (op >> 3) & 0x07;
                let v = self.fetch8(bus);
                self.reg_set(dst, v, bus);
                if dst == 6 { 12 } else { 8 }
            }

            // ---- LD rr,d16 ----
            0x01 => { let v = self.fetch16(bus); self.set_bc(v); 12 }
            0x11 => { let v = self.fetch16(bus); self.set_de(v); 12 }
            0x21 => { let v = self.fetch16(bus); self.set_hl(v); 12 }
            0x31 => { self.sp = self.fetch16(bus); 12 }

            // ---- LD (rr),A / LD A,(rr) ----
            0x02 => { bus.write8(self.bc(), self.a); 8 }
            0x12 => { bus.write8(self.de(), self.a); 8 }
            0x0A => { self.a = bus.read8(self.bc()); 8 }
            0x1A => { self.a = bus.read8(self.de()); 8 }
            0x22 => { let hl = self.hl(); bus.write8(hl, self.a); self.set_hl(hl.wrapping_add(1)); 8 }
            0x32 => { let hl = self.hl(); bus.write8(hl, self.a); self.set_hl(hl.wrapping_sub(1)); 8 }
            0x2A => { let hl = self.hl(); self.a = bus.read8(hl); self.set_hl(hl.wrapping_add(1)); 8 }
            0x3A => { let hl = self.hl(); self.a = bus.read8(hl); self.set_hl(hl.wrapping_sub(1)); 8 }

            // ---- LD (a16),SP ----
            0x08 => {
                let addr = self.fetch16(bus);
                bus.write8(addr, self.sp as u8);
                bus.write8(addr.wrapping_add(1), (self.sp >> 8) as u8);
                20
            }

            // ---- LDH / LD (C),A etc ----
            0xE0 => { let n = self.fetch8(bus); bus.write8(0xFF00 + n as u16, self.a); 12 }
            0xF0 => { let n = self.fetch8(bus); self.a = bus.read8(0xFF00 + n as u16); 12 }
            0xE2 => { bus.write8(0xFF00 + self.c as u16, self.a); 8 }
            0xF2 => { self.a = bus.read8(0xFF00 + self.c as u16); 8 }
            0xEA => { let addr = self.fetch16(bus); bus.write8(addr, self.a); 16 }
            0xFA => { let addr = self.fetch16(bus); self.a = bus.read8(addr); 16 }

            // ---- 16-bit loads / SP ----
            0xF9 => { self.sp = self.hl(); 8 }
            0xF8 => { let v = self.add_sp_imm(bus); self.set_hl(v); 12 }
            0xE8 => { self.sp = self.add_sp_imm(bus); 16 }

            // ---- PUSH / POP ----
            0xC5 => { let v = self.bc(); self.push16(bus, v); 16 }
            0xD5 => { let v = self.de(); self.push16(bus, v); 16 }
            0xE5 => { let v = self.hl(); self.push16(bus, v); 16 }
            0xF5 => { let v = self.af(); self.push16(bus, v); 16 }
            0xC1 => { let v = self.pop16(bus); self.set_bc(v); 12 }
            0xD1 => { let v = self.pop16(bus); self.set_de(v); 12 }
            0xE1 => { let v = self.pop16(bus); self.set_hl(v); 12 }
            0xF1 => { let v = self.pop16(bus); self.set_af(v); 12 }

            // ---- INC/DEC 16-bit ----
            0x03 => { let v = self.bc().wrapping_add(1); self.set_bc(v); 8 }
            0x13 => { let v = self.de().wrapping_add(1); self.set_de(v); 8 }
            0x23 => { let v = self.hl().wrapping_add(1); self.set_hl(v); 8 }
            0x33 => { self.sp = self.sp.wrapping_add(1); 8 }
            0x0B => { let v = self.bc().wrapping_sub(1); self.set_bc(v); 8 }
            0x1B => { let v = self.de().wrapping_sub(1); self.set_de(v); 8 }
            0x2B => { let v = self.hl().wrapping_sub(1); self.set_hl(v); 8 }
            0x3B => { self.sp = self.sp.wrapping_sub(1); 8 }

            // ---- INC/DEC 8-bit ----
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let r = (op >> 3) & 0x07;
                let v = self.reg_get(r, bus);
                let nv = self.alu_inc(v);
                self.reg_set(r, nv, bus);
                if r == 6 { 12 } else { 4 }
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let r = (op >> 3) & 0x07;
                let v = self.reg_get(r, bus);
                let nv = self.alu_dec(v);
                self.reg_set(r, nv, bus);
                if r == 6 { 12 } else { 4 }
            }

            // ---- ADD HL,rr ----
            0x09 => { let v = self.bc(); self.add_hl(v); 8 }
            0x19 => { let v = self.de(); self.add_hl(v); 8 }
            0x29 => { let v = self.hl(); self.add_hl(v); 8 }
            0x39 => { self.add_hl(self.sp); 8 }

            // ---- 8-bit ALU A,r (0x80-0xBF) ----
            0x80..=0xBF => {
                let src = op & 0x07;
                let v = self.reg_get(src, bus);
                match (op >> 3) & 0x07 {
                    0 => self.alu_add(v, false),
                    1 => self.alu_add(v, true),
                    2 => { let r = self.alu_sub(v, false); self.a = r; }
                    3 => { let r = self.alu_sub(v, true); self.a = r; }
                    4 => self.alu_and(v),
                    5 => self.alu_xor(v),
                    6 => self.alu_or(v),
                    7 => self.alu_cp(v),
                    _ => unreachable!(),
                }
                if src == 6 { 8 } else { 4 }
            }

            // ---- 8-bit ALU A,d8 ----
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let v = self.fetch8(bus);
                match (op >> 3) & 0x07 {
                    0 => self.alu_add(v, false),
                    1 => self.alu_add(v, true),
                    2 => { let r = self.alu_sub(v, false); self.a = r; }
                    3 => { let r = self.alu_sub(v, true); self.a = r; }
                    4 => self.alu_and(v),
                    5 => self.alu_xor(v),
                    6 => self.alu_or(v),
                    7 => self.alu_cp(v),
                    _ => unreachable!(),
                }
                8
            }

            // ---- rotates on A (fast, non-prefixed) ----
            0x07 => { self.a = self.rlc(self.a); self.set_flag(FLAG_Z, false); 4 }
            0x17 => { self.a = self.rl(self.a); self.set_flag(FLAG_Z, false); 4 }
            0x0F => { self.a = self.rrc(self.a); self.set_flag(FLAG_Z, false); 4 }
            0x1F => { self.a = self.rr(self.a); self.set_flag(FLAG_Z, false); 4 }

            // ---- misc accumulator ops ----
            0x27 => { self.daa(); 4 }
            0x2F => {
                self.a = !self.a;
                self.set_flag(FLAG_N, true);
                self.set_flag(FLAG_H, true);
                4
            }
            0x37 => {
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_C, true);
                4
            }
            0x3F => {
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                let c = self.flag(FLAG_C);
                self.set_flag(FLAG_C, !c);
                4
            }

            // ---- jumps ----
            0xC3 => { self.pc = self.fetch16(bus); 16 }
            0xE9 => { self.pc = self.hl(); 4 }
            0xC2 | 0xCA | 0xD2 | 0xDA => {
                let addr = self.fetch16(bus);
                if self.cond((op >> 3) & 0x03) { self.pc = addr; 16 } else { 12 }
            }
            0x18 => {
                let e = self.fetch8(bus) as i8;
                self.pc = self.pc.wrapping_add(e as u16);
                12
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                let e = self.fetch8(bus) as i8;
                if self.cond((op >> 3) & 0x03) {
                    self.pc = self.pc.wrapping_add(e as u16);
                    12
                } else {
                    8
                }
            }

            // ---- calls / returns / rst ----
            0xCD => { let addr = self.fetch16(bus); let pc = self.pc; self.push16(bus, pc); self.pc = addr; 24 }
            0xC4 | 0xCC | 0xD4 | 0xDC => {
                let addr = self.fetch16(bus);
                if self.cond((op >> 3) & 0x03) {
                    let pc = self.pc;
                    self.push16(bus, pc);
                    self.pc = addr;
                    24
                } else {
                    12
                }
            }
            0xC9 => { self.pc = self.pop16(bus); 16 }
            0xD9 => { self.pc = self.pop16(bus); self.ime = true; 16 }
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                if self.cond((op >> 3) & 0x03) {
                    self.pc = self.pop16(bus);
                    20
                } else {
                    8
                }
            }
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                let pc = self.pc;
                self.push16(bus, pc);
                self.pc = (op & 0x38) as u16;
                16
            }

            // ---- CB prefix ----
            0xCB => self.execute_cb(bus),

            // ---- illegal opcodes (hard-lock real hardware) ----
            // These have no defined behavior and freeze the SM83. Signal a
            // fault for the orchestrator to surface on the crash screen. The
            // instruction PC is filled in by `step`.
            0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD => {
                self.illegal_op = Some((op, 0));
                4
            }
        }
    }

    /// Condition code for the 2-bit cc field: NZ, Z, NC, C.
    #[inline]
    fn cond(&self, cc: u8) -> bool {
        match cc {
            0 => !self.flag(FLAG_Z),
            1 => self.flag(FLAG_Z),
            2 => !self.flag(FLAG_C),
            3 => self.flag(FLAG_C),
            _ => unreachable!(),
        }
    }

    // ---- rotate/shift primitives (set flags; Z handled by caller for A-ops) ----
    fn rlc(&mut self, v: u8) -> u8 {
        let c = v >> 7;
        let r = (v << 1) | c;
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn rrc(&mut self, v: u8) -> u8 {
        let c = v & 1;
        let r = (v >> 1) | (c << 7);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn rl(&mut self, v: u8) -> u8 {
        let old_c = self.flag(FLAG_C) as u8;
        let c = v >> 7;
        let r = (v << 1) | old_c;
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn rr(&mut self, v: u8) -> u8 {
        let old_c = self.flag(FLAG_C) as u8;
        let c = v & 1;
        let r = (v >> 1) | (old_c << 7);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn sla(&mut self, v: u8) -> u8 {
        let c = v >> 7;
        let r = v << 1;
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn sra(&mut self, v: u8) -> u8 {
        let c = v & 1;
        let r = (v >> 1) | (v & 0x80);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn srl(&mut self, v: u8) -> u8 {
        let c = v & 1;
        let r = v >> 1;
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, c != 0);
        r
    }
    fn swap(&mut self, v: u8) -> u8 {
        let r = v.rotate_left(4);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_N, false);
        self.set_flag(FLAG_H, false);
        self.set_flag(FLAG_C, false);
        r
    }

    /// CB-prefixed ops: rotate/shift/swap (0x00-0x3F), BIT (0x40-0x7F),
    /// RES (0x80-0xBF), SET (0xC0-0xFF). Returns T-cycles.
    fn execute_cb(&mut self, bus: &mut dyn Bus) -> u32 {
        let op = self.fetch8(bus);
        let reg = op & 0x07;
        let is_hl = reg == 6;
        let v = self.reg_get(reg, bus);

        let (result, writeback): (u8, bool) = match op >> 3 {
            0x00 => (self.rlc(v), true),
            0x01 => (self.rrc(v), true),
            0x02 => (self.rl(v), true),
            0x03 => (self.rr(v), true),
            0x04 => (self.sla(v), true),
            0x05 => (self.sra(v), true),
            0x06 => (self.swap(v), true),
            0x07 => (self.srl(v), true),
            // BIT b,r (0x40-0x7F)
            0x08..=0x0F => {
                let bit = (op >> 3) & 0x07;
                self.set_flag(FLAG_Z, v & (1 << bit) == 0);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, true);
                (v, false)
            }
            // RES b,r (0x80-0xBF)
            0x10..=0x17 => {
                let bit = (op >> 3) & 0x07;
                (v & !(1 << bit), true)
            }
            // SET b,r (0xC0-0xFF)
            0x18..=0x1F => {
                let bit = (op >> 3) & 0x07;
                (v | (1 << bit), true)
            }
            _ => unreachable!(),
        };

        if writeback {
            self.reg_set(reg, result, bus);
        }

        // Timing: 8 for reg ops, 16 for (HL) writes, 12 for BIT (HL).
        match (is_hl, writeback) {
            (true, true) => 16,
            (true, false) => 12, // BIT b,(HL)
            (false, _) => 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::Gbc;

    /// Run a tiny program loaded into WRAM (0xC000) with PC pointed there.
    fn run(prog: &[u8]) -> Gbc {
        let mut gbc = Gbc::new();
        for (i, b) in prog.iter().enumerate() {
            gbc.write8(0xC000 + i as u16, *b);
        }
        gbc.cpu.pc = 0xC000;
        gbc
    }

    #[test]
    fn ld_and_add() {
        // LD A,0x10 ; LD B,0x05 ; ADD A,B
        let mut gbc = run(&[0x3E, 0x10, 0x06, 0x05, 0x80]);
        gbc.step();
        gbc.step();
        gbc.step();
        assert_eq!(gbc.cpu.a, 0x15);
        assert!(!gbc.cpu.flag(FLAG_Z));
    }

    #[test]
    fn add_sets_half_and_carry() {
        // LD A,0xFF ; ADD A,0x01
        let mut gbc = run(&[0x3E, 0xFF, 0xC6, 0x01]);
        gbc.step();
        gbc.step();
        assert_eq!(gbc.cpu.a, 0x00);
        assert!(gbc.cpu.flag(FLAG_Z));
        assert!(gbc.cpu.flag(FLAG_C));
        assert!(gbc.cpu.flag(FLAG_H));
    }

    #[test]
    fn jr_loop_decrements() {
        // LD B,3 ; (loop) DEC B ; JR NZ,-3
        let mut gbc = run(&[0x06, 0x03, 0x05, 0x20, 0xFD]);
        gbc.step(); // LD B,3
        for _ in 0..10 {
            if gbc.cpu.b == 0 {
                break;
            }
            gbc.step(); // DEC B
            gbc.step(); // JR NZ
        }
        assert_eq!(gbc.cpu.b, 0);
    }

    #[test]
    fn push_pop_roundtrip() {
        // LD BC,0x1234 ; PUSH BC ; POP DE
        let mut gbc = run(&[0x01, 0x34, 0x12, 0xC5, 0xD1]);
        gbc.cpu.sp = 0xDFFF;
        gbc.step();
        gbc.step();
        gbc.step();
        assert_eq!(gbc.cpu.de(), 0x1234);
    }

    #[test]
    fn cb_swap_nibbles() {
        // LD A,0xAB ; SWAP A (CB 37)
        let mut gbc = run(&[0x3E, 0xAB, 0xCB, 0x37]);
        gbc.step();
        gbc.step();
        assert_eq!(gbc.cpu.a, 0xBA);
    }

    #[test]
    fn cb_bit_sets_zero_flag() {
        // LD A,0x00 ; BIT 7,A (CB 7F)
        let mut gbc = run(&[0x3E, 0x00, 0xCB, 0x7F]);
        gbc.step();
        gbc.step();
        assert!(gbc.cpu.flag(FLAG_Z));
        assert!(gbc.cpu.flag(FLAG_H));
    }

    #[test]
    fn call_and_ret() {
        // CALL 0xC010 ; (at C010) RET
        let mut gbc = Gbc::new();
        gbc.cpu.sp = 0xDFFF;
        gbc.cpu.pc = 0xC000;
        gbc.write8(0xC000, 0xCD);
        gbc.write8(0xC001, 0x10);
        gbc.write8(0xC002, 0xC0);
        gbc.write8(0xC010, 0xC9); // RET
        gbc.step(); // CALL
        assert_eq!(gbc.cpu.pc, 0xC010);
        gbc.step(); // RET
        assert_eq!(gbc.cpu.pc, 0xC003);
    }

    #[test]
    fn timing_nop_is_4_cycles() {
        let mut gbc = run(&[0x00]);
        assert_eq!(gbc.step(), 4);
    }

    #[test]
    fn interrupt_dispatch_jumps_to_vector() {
        let mut gbc = run(&[0x00]);
        gbc.cpu.ime = true;
        gbc.cpu.sp = 0xDFFF;
        gbc.irq.write_ie(0x01); // enable VBlank
        gbc.request_interrupt(crate::interrupts::Interrupt::VBlank);
        let cycles = gbc.step();
        assert_eq!(cycles, 20);
        assert_eq!(gbc.cpu.pc, 0x40);
        assert!(!gbc.cpu.ime);
    }
}
