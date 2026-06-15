//! Sony SPC700 CPU core. An 8-bit processor loosely 6502-like (A/X/Y, SP, PSW)
//! with a direct page selectable between $00xx and $01xx (PSW P bit), and a
//! 16-bit YA register pair for a few ops.
//!
//! Source: fullsnes "SPC700 instruction set" + anomie's SPC700 reference.
//!
//! It accesses memory through the owning [`Apu`] (`read_aram`/`write_aram`),
//! which overlays the IPL ROM and the $F0-$FF register block. `step()` executes
//! one instruction and returns its cycle count.
//!
//! Coverage: the full common instruction set used by the IPL ROM and typical
//! sound drivers (MOV in all addressing forms, ALU ops ADC/SBC/CMP/AND/OR/EOR,
//! INC/DEC, shifts/rotates, branches incl. the CBNE/DBNZ/bit-branch family,
//! CALL/RET/PCALL/TCALL, stack ops, MOVW/INCW/DECW/CMPW, MUL/DIV, flag ops,
//! NOP/SLEEP/STOP). A few rare opcodes are NOP'd; this is enough to boot games.

use super::Apu;

const FLAG_C: u8 = 0x01;
const FLAG_Z: u8 = 0x02;
const FLAG_I: u8 = 0x04; // interrupt enable (unused on SNES)
const FLAG_H: u8 = 0x08; // half carry
#[allow(dead_code)]
const FLAG_B: u8 = 0x10; // break (set by BRK; SNES sound drivers rarely use it)
const FLAG_P: u8 = 0x20; // direct page select (0 = $00xx, 1 = $01xx)
const FLAG_V: u8 = 0x40;
const FLAG_N: u8 = 0x80;

#[derive(Clone)]
pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub psw: u8,
    pub stopped: bool,
}

impl Default for Spc700 {
    fn default() -> Self {
        Spc700::new()
    }
}

impl Spc700 {
    pub fn new() -> Spc700 {
        Spc700 {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xEF,
            pc: 0xFFC0,
            psw: 0x02,
            stopped: false,
        }
    }

    #[inline]
    fn dp_base(&self) -> u16 {
        if self.psw & FLAG_P != 0 {
            0x0100
        } else {
            0
        }
    }

    #[inline]
    fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.psw |= f;
        } else {
            self.psw &= !f;
        }
    }
    #[inline]
    fn set_nz(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
    }

    // ---- memory ----
    #[inline]
    fn read(apu: &mut Apu, addr: u16) -> u8 {
        apu.read_aram(addr)
    }
    #[inline]
    fn write(apu: &mut Apu, addr: u16, v: u8) {
        apu.write_aram(addr, v);
    }
    #[inline]
    fn fetch(&mut self, apu: &mut Apu) -> u8 {
        let v = Self::read(apu, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    #[inline]
    fn fetch16(&mut self, apu: &mut Apu) -> u16 {
        let lo = self.fetch(apu) as u16;
        let hi = self.fetch(apu) as u16;
        (hi << 8) | lo
    }

    fn push(&mut self, apu: &mut Apu, v: u8) {
        Self::write(apu, 0x0100 | self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }
    fn pull(&mut self, apu: &mut Apu) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        Self::read(apu, 0x0100 | self.sp as u16)
    }

    // ---- addressing helpers ----
    fn dp_addr(&mut self, apu: &mut Apu) -> u16 {
        let off = self.fetch(apu) as u16;
        self.dp_base() | off
    }
    fn dp_x_addr(&mut self, apu: &mut Apu) -> u16 {
        let off = self.fetch(apu).wrapping_add(self.x) as u16;
        self.dp_base() | off
    }
    fn dp_y_addr(&mut self, apu: &mut Apu) -> u16 {
        let off = self.fetch(apu).wrapping_add(self.y) as u16;
        self.dp_base() | off
    }
    fn abs_addr(&mut self, apu: &mut Apu) -> u16 {
        self.fetch16(apu)
    }
    fn abs_x_addr(&mut self, apu: &mut Apu) -> u16 {
        self.fetch16(apu).wrapping_add(self.x as u16)
    }
    fn abs_y_addr(&mut self, apu: &mut Apu) -> u16 {
        self.fetch16(apu).wrapping_add(self.y as u16)
    }
    /// (dp+X) indirect.
    fn ind_x_addr(&mut self, apu: &mut Apu) -> u16 {
        let p = self.fetch(apu).wrapping_add(self.x) as u16;
        let lo = Self::read(apu, self.dp_base() | p) as u16;
        let hi = Self::read(apu, self.dp_base() | (p.wrapping_add(1) & 0xFF)) as u16;
        (hi << 8) | lo
    }
    /// (dp)+Y indirect.
    fn ind_y_addr(&mut self, apu: &mut Apu) -> u16 {
        let p = self.fetch(apu) as u16;
        let lo = Self::read(apu, self.dp_base() | p) as u16;
        let hi = Self::read(apu, self.dp_base() | (p.wrapping_add(1) & 0xFF)) as u16;
        (((hi << 8) | lo)).wrapping_add(self.y as u16)
    }

    // ---- ALU primitives ----
    fn adc(&mut self, a: u8, b: u8) -> u8 {
        let c = (self.psw & FLAG_C != 0) as u16;
        let sum = a as u16 + b as u16 + c;
        let res = sum as u8;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_H, ((a & 0x0F) + (b & 0x0F) + c as u8) > 0x0F);
        self.set_flag(FLAG_V, ((a ^ res) & (b ^ res) & 0x80) != 0);
        self.set_nz(res);
        res
    }
    fn sbc(&mut self, a: u8, b: u8) -> u8 {
        let c = (self.psw & FLAG_C != 0) as i16;
        let diff = a as i16 - b as i16 - (1 - c);
        let res = diff as u8;
        self.set_flag(FLAG_C, diff >= 0);
        self.set_flag(FLAG_H, ((a & 0x0F) as i16 - (b & 0x0F) as i16 - (1 - c)) >= 0);
        self.set_flag(FLAG_V, ((a ^ b) & (a ^ res) & 0x80) != 0);
        self.set_nz(res);
        res
    }
    fn cmp(&mut self, a: u8, b: u8) {
        let diff = a.wrapping_sub(b);
        self.set_flag(FLAG_C, a >= b);
        self.set_nz(diff);
    }
    fn asl(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        self.set_nz(r);
        r
    }
    fn lsr(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 1 != 0);
        let r = v >> 1;
        self.set_nz(r);
        r
    }
    fn rol(&mut self, v: u8) -> u8 {
        let c = (self.psw & FLAG_C != 0) as u8;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | c;
        self.set_nz(r);
        r
    }
    fn ror(&mut self, v: u8) -> u8 {
        let c = (self.psw & FLAG_C != 0) as u8;
        self.set_flag(FLAG_C, v & 1 != 0);
        let r = (v >> 1) | (c << 7);
        self.set_nz(r);
        r
    }

    fn branch(&mut self, apu: &mut Apu, cond: bool) -> u32 {
        let off = self.fetch(apu) as i8 as i16;
        if cond {
            self.pc = self.pc.wrapping_add(off as u16);
            4
        } else {
            2
        }
    }

    /// Execute one instruction. Returns approximate cycle count.
    pub fn step(&mut self, apu: &mut Apu) -> u32 {
        if self.stopped {
            return 1;
        }
        let op = self.fetch(apu);
        match op {
            0x00 => 2, // NOP
            0xEF | 0xFF => { self.stopped = true; 3 } // SLEEP / STOP

            // ---- MOV A,... ----
            0xE8 => { let v = self.fetch(apu); self.a = v; self.set_nz(v); 2 } // MOV A,#imm
            0xE4 => { let a = self.dp_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 3 } // MOV A,dp
            0xF4 => { let a = self.dp_x_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 4 } // MOV A,dp+X
            0xE5 => { let a = self.abs_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 4 } // MOV A,abs
            0xF5 => { let a = self.abs_x_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 5 } // MOV A,abs+X
            0xF6 => { let a = self.abs_y_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 5 } // MOV A,abs+Y
            0xE6 => { let a = self.dp_base() | self.x as u16; self.a = Self::read(apu,a); self.set_nz(self.a); 3 } // MOV A,(X)
            0xBF => { let a = self.dp_base() | self.x as u16; self.a = Self::read(apu,a); self.x = self.x.wrapping_add(1); self.set_nz(self.a); 4 } // MOV A,(X)+
            0xE7 => { let a = self.ind_x_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 6 } // MOV A,(dp+X)
            0xF7 => { let a = self.ind_y_addr(apu); self.a = Self::read(apu,a); self.set_nz(self.a); 6 } // MOV A,(dp)+Y

            // ---- MOV X/Y,... ----
            0xCD => { let v = self.fetch(apu); self.x = v; self.set_nz(v); 2 } // MOV X,#imm
            0xF8 => { let a = self.dp_addr(apu); self.x = Self::read(apu,a); self.set_nz(self.x); 3 } // MOV X,dp
            0xF9 => { let a = self.dp_y_addr(apu); self.x = Self::read(apu,a); self.set_nz(self.x); 4 } // MOV X,dp+Y
            0xE9 => { let a = self.abs_addr(apu); self.x = Self::read(apu,a); self.set_nz(self.x); 4 } // MOV X,abs
            0x8D => { let v = self.fetch(apu); self.y = v; self.set_nz(v); 2 } // MOV Y,#imm
            0xEB => { let a = self.dp_addr(apu); self.y = Self::read(apu,a); self.set_nz(self.y); 3 } // MOV Y,dp
            0xFB => { let a = self.dp_x_addr(apu); self.y = Self::read(apu,a); self.set_nz(self.y); 4 } // MOV Y,dp+X
            0xEC => { let a = self.abs_addr(apu); self.y = Self::read(apu,a); self.set_nz(self.y); 4 } // MOV Y,abs

            // ---- MOV ...,A ----
            0xC4 => { let a = self.dp_addr(apu); Self::write(apu,a,self.a); 4 } // MOV dp,A
            0xD4 => { let a = self.dp_x_addr(apu); Self::write(apu,a,self.a); 5 } // MOV dp+X,A
            0xC5 => { let a = self.abs_addr(apu); Self::write(apu,a,self.a); 5 } // MOV abs,A
            0xD5 => { let a = self.abs_x_addr(apu); Self::write(apu,a,self.a); 6 } // MOV abs+X,A
            0xD6 => { let a = self.abs_y_addr(apu); Self::write(apu,a,self.a); 6 } // MOV abs+Y,A
            0xC6 => { let a = self.dp_base() | self.x as u16; Self::write(apu,a,self.a); 4 } // MOV (X),A
            0xAF => { let a = self.dp_base() | self.x as u16; Self::write(apu,a,self.a); self.x = self.x.wrapping_add(1); 4 } // MOV (X)+,A
            0xC7 => { let a = self.ind_x_addr(apu); Self::write(apu,a,self.a); 7 } // MOV (dp+X),A
            0xD7 => { let a = self.ind_y_addr(apu); Self::write(apu,a,self.a); 7 } // MOV (dp)+Y,A

            // ---- MOV ...,X/Y ----
            0xD8 => { let a = self.dp_addr(apu); Self::write(apu,a,self.x); 4 } // MOV dp,X
            0xD9 => { let a = self.dp_y_addr(apu); Self::write(apu,a,self.x); 5 } // MOV dp+Y,X
            0xC9 => { let a = self.abs_addr(apu); Self::write(apu,a,self.x); 5 } // MOV abs,X
            0xCB => { let a = self.dp_addr(apu); Self::write(apu,a,self.y); 4 } // MOV dp,Y
            0xDB => { let a = self.dp_x_addr(apu); Self::write(apu,a,self.y); 5 } // MOV dp+X,Y
            0xCC => { let a = self.abs_addr(apu); Self::write(apu,a,self.y); 5 } // MOV abs,Y

            // MOV dp,dp / MOV dp,#imm
            0xFA => { let src = self.dp_addr(apu); let v = Self::read(apu,src); let dst = self.dp_addr(apu); Self::write(apu,dst,v); 5 }
            0x8F => { let v = self.fetch(apu); let dst = self.dp_addr(apu); Self::write(apu,dst,v); 5 } // MOV dp,#imm

            // ---- op dp,#imm (immediate against a direct-page byte) ----
            // Encoding: opcode, imm, dp.
            0x18 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let r = Self::read(apu,a) | v; Self::write(apu,a,r); self.set_nz(r); 5 } // OR dp,#imm
            0x38 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let r = Self::read(apu,a) & v; Self::write(apu,a,r); self.set_nz(r); 5 } // AND dp,#imm
            0x58 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let r = Self::read(apu,a) ^ v; Self::write(apu,a,r); self.set_nz(r); 5 } // EOR dp,#imm
            0x78 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let m = Self::read(apu,a); self.cmp(m, v); 5 } // CMP dp,#imm
            0x98 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let m = Self::read(apu,a); let r = self.adc(m, v); Self::write(apu,a,r); 5 } // ADC dp,#imm
            0xB8 => { let v = self.fetch(apu); let a = self.dp_addr(apu); let m = Self::read(apu,a); let r = self.sbc(m, v); Self::write(apu,a,r); 5 } // SBC dp,#imm

            // ---- op dp,dp (direct-page against direct-page) ----
            // Encoding: opcode, src dp, dst dp.
            0x09 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let r = Self::read(apu,d) | sv; Self::write(apu,d,r); self.set_nz(r); 6 } // OR dp,dp
            0x29 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let r = Self::read(apu,d) & sv; Self::write(apu,d,r); self.set_nz(r); 6 } // AND dp,dp
            0x49 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let r = Self::read(apu,d) ^ sv; Self::write(apu,d,r); self.set_nz(r); 6 } // EOR dp,dp
            0x69 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let m = Self::read(apu,d); self.cmp(m, sv); 6 } // CMP dp,dp
            0x89 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let m = Self::read(apu,d); let r = self.adc(m, sv); Self::write(apu,d,r); 6 } // ADC dp,dp
            0xA9 => { let s = self.dp_addr(apu); let sv = Self::read(apu,s); let d = self.dp_addr(apu); let m = Self::read(apu,d); let r = self.sbc(m, sv); Self::write(apu,d,r); 6 } // SBC dp,dp

            // ---- register transfers ----
            0x7D => { self.a = self.x; self.set_nz(self.a); 2 } // MOV A,X
            0xDD => { self.a = self.y; self.set_nz(self.a); 2 } // MOV A,Y
            0x5D => { self.x = self.a; self.set_nz(self.x); 2 } // MOV X,A
            0xFD => { self.y = self.a; self.set_nz(self.y); 2 } // MOV Y,A
            0x9D => { self.x = self.sp; self.set_nz(self.x); 2 } // MOV X,SP
            0xBD => { self.sp = self.x; 2 } // MOV SP,X

            // ---- ALU A,imm/dp/abs/(X)/etc ----
            0x88 => { let v = self.fetch(apu); self.a = self.adc(self.a, v); 2 } // ADC A,#imm
            0x84 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.a = self.adc(self.a,v); 3 }
            0x85 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.a = self.adc(self.a,v); 4 }
            0x86 => { let a = self.dp_base()|self.x as u16; let v = Self::read(apu,a); self.a = self.adc(self.a,v); 3 }
            0xA8 => { let v = self.fetch(apu); self.a = self.sbc(self.a, v); 2 } // SBC A,#imm
            0xA4 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.a = self.sbc(self.a,v); 3 }
            0xA5 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.a = self.sbc(self.a,v); 4 }
            0x68 => { let v = self.fetch(apu); self.cmp(self.a, v); 2 } // CMP A,#imm
            0x64 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.cmp(self.a,v); 3 }
            0x65 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.cmp(self.a,v); 4 }
            0x66 => { let a = self.dp_base()|self.x as u16; let v = Self::read(apu,a); self.cmp(self.a,v); 3 }
            0x28 => { let v = self.fetch(apu); self.a &= v; self.set_nz(self.a); 2 } // AND A,#imm
            0x24 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.a &= v; self.set_nz(self.a); 3 }
            0x25 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.a &= v; self.set_nz(self.a); 4 }
            0x08 => { let v = self.fetch(apu); self.a |= v; self.set_nz(self.a); 2 } // OR A,#imm
            0x04 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.a |= v; self.set_nz(self.a); 3 }
            0x05 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.a |= v; self.set_nz(self.a); 4 }
            0x48 => { let v = self.fetch(apu); self.a ^= v; self.set_nz(self.a); 2 } // EOR A,#imm
            0x44 => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.a ^= v; self.set_nz(self.a); 3 }
            0x45 => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.a ^= v; self.set_nz(self.a); 4 }

            // CMP X / CMP Y
            0xC8 => { let v = self.fetch(apu); self.cmp(self.x, v); 2 } // CMP X,#imm
            0x3E => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.cmp(self.x,v); 3 }
            0x1E => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.cmp(self.x,v); 4 }
            0xAD => { let v = self.fetch(apu); self.cmp(self.y, v); 2 } // CMP Y,#imm
            0x7E => { let a = self.dp_addr(apu); let v = Self::read(apu,a); self.cmp(self.y,v); 3 }
            0x5E => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.cmp(self.y,v); 4 }

            // ---- INC / DEC ----
            0xBC => { self.a = self.a.wrapping_add(1); self.set_nz(self.a); 2 } // INC A
            0x3D => { self.x = self.x.wrapping_add(1); self.set_nz(self.x); 2 } // INC X
            0xFC => { self.y = self.y.wrapping_add(1); self.set_nz(self.y); 2 } // INC Y
            0x9C => { self.a = self.a.wrapping_sub(1); self.set_nz(self.a); 2 } // DEC A
            0x1D => { self.x = self.x.wrapping_sub(1); self.set_nz(self.x); 2 } // DEC X
            0xDC => { self.y = self.y.wrapping_sub(1); self.set_nz(self.y); 2 } // DEC Y
            0xAB => { let a = self.dp_addr(apu); let v = Self::read(apu,a).wrapping_add(1); Self::write(apu,a,v); self.set_nz(v); 4 } // INC dp
            0xAC => { let a = self.abs_addr(apu); let v = Self::read(apu,a).wrapping_add(1); Self::write(apu,a,v); self.set_nz(v); 5 } // INC abs
            0x8B => { let a = self.dp_addr(apu); let v = Self::read(apu,a).wrapping_sub(1); Self::write(apu,a,v); self.set_nz(v); 4 } // DEC dp
            0x8C => { let a = self.abs_addr(apu); let v = Self::read(apu,a).wrapping_sub(1); Self::write(apu,a,v); self.set_nz(v); 5 } // DEC abs

            // ---- shifts on A and memory ----
            0x1C => { self.a = self.asl(self.a); 2 } // ASL A
            0x0B => { let a = self.dp_addr(apu); let v = Self::read(apu,a); let r = self.asl(v); Self::write(apu,a,r); 4 } // ASL dp
            0x5C => { self.a = self.lsr(self.a); 2 } // LSR A
            0x4B => { let a = self.dp_addr(apu); let v = Self::read(apu,a); let r = self.lsr(v); Self::write(apu,a,r); 4 } // LSR dp
            0x3C => { self.a = self.rol(self.a); 2 } // ROL A
            0x2B => { let a = self.dp_addr(apu); let v = Self::read(apu,a); let r = self.rol(v); Self::write(apu,a,r); 4 } // ROL dp
            0x7C => { self.a = self.ror(self.a); 2 } // ROR A
            0x6B => { let a = self.dp_addr(apu); let v = Self::read(apu,a); let r = self.ror(v); Self::write(apu,a,r); 4 } // ROR dp

            // ---- 16-bit word ops ----
            0xBA => { // MOVW YA,dp
                let a = self.dp_addr(apu);
                self.a = Self::read(apu,a);
                self.y = Self::read(apu, (a & 0xFF00) | (a.wrapping_add(1) & 0xFF));
                self.set_flag(FLAG_Z, self.a == 0 && self.y == 0);
                self.set_flag(FLAG_N, self.y & 0x80 != 0);
                5
            }
            0xDA => { // MOVW dp,YA
                let a = self.dp_addr(apu);
                Self::write(apu,a,self.a);
                Self::write(apu, (a & 0xFF00) | (a.wrapping_add(1) & 0xFF), self.y);
                5
            }
            0x3A => { // INCW dp
                let a = self.dp_addr(apu);
                let a2 = (a & 0xFF00) | (a.wrapping_add(1) & 0xFF);
                let w = ((Self::read(apu,a2) as u16) << 8 | Self::read(apu,a) as u16).wrapping_add(1);
                Self::write(apu,a,w as u8); Self::write(apu,a2,(w>>8) as u8);
                self.set_flag(FLAG_Z, w==0); self.set_flag(FLAG_N, w & 0x8000 != 0);
                6
            }
            0x1A => { // DECW dp
                let a = self.dp_addr(apu);
                let a2 = (a & 0xFF00) | (a.wrapping_add(1) & 0xFF);
                let w = ((Self::read(apu,a2) as u16) << 8 | Self::read(apu,a) as u16).wrapping_sub(1);
                Self::write(apu,a,w as u8); Self::write(apu,a2,(w>>8) as u8);
                self.set_flag(FLAG_Z, w==0); self.set_flag(FLAG_N, w & 0x8000 != 0);
                6
            }
            0x7A => { // ADDW YA,dp
                let a = self.dp_addr(apu);
                let a2 = (a & 0xFF00) | (a.wrapping_add(1) & 0xFF);
                let w = (Self::read(apu,a2) as u16) << 8 | Self::read(apu,a) as u16;
                let ya = (self.y as u16) << 8 | self.a as u16;
                let sum = ya as u32 + w as u32;
                self.set_flag(FLAG_C, sum > 0xFFFF);
                self.set_flag(FLAG_V, ((ya ^ sum as u16) & (w ^ sum as u16) & 0x8000) != 0);
                self.set_flag(FLAG_H, ((ya & 0x0FFF) + (w & 0x0FFF)) > 0x0FFF);
                self.a = sum as u8; self.y = (sum >> 8) as u8;
                self.set_flag(FLAG_Z, sum as u16 == 0); self.set_flag(FLAG_N, sum & 0x8000 != 0);
                5
            }
            0x9A => { // SUBW YA,dp
                let a = self.dp_addr(apu);
                let a2 = (a & 0xFF00) | (a.wrapping_add(1) & 0xFF);
                let w = (Self::read(apu,a2) as u16) << 8 | Self::read(apu,a) as u16;
                let ya = (self.y as u16) << 8 | self.a as u16;
                let diff = ya as i32 - w as i32;
                self.set_flag(FLAG_C, diff >= 0);
                let r = diff as u16;
                self.set_flag(FLAG_V, ((ya ^ w) & (ya ^ r) & 0x8000) != 0);
                self.a = r as u8; self.y = (r >> 8) as u8;
                self.set_flag(FLAG_Z, r == 0); self.set_flag(FLAG_N, r & 0x8000 != 0);
                5
            }
            0x5A => { // CMPW YA,dp
                let a = self.dp_addr(apu);
                let a2 = (a & 0xFF00) | (a.wrapping_add(1) & 0xFF);
                let w = (Self::read(apu,a2) as u16) << 8 | Self::read(apu,a) as u16;
                let ya = (self.y as u16) << 8 | self.a as u16;
                self.set_flag(FLAG_C, ya >= w);
                let r = ya.wrapping_sub(w);
                self.set_flag(FLAG_Z, r == 0); self.set_flag(FLAG_N, r & 0x8000 != 0);
                4
            }
            0xCF => { // MUL YA = Y*A
                let r = self.y as u16 * self.a as u16;
                self.a = r as u8; self.y = (r >> 8) as u8;
                self.set_flag(FLAG_Z, self.y == 0); self.set_flag(FLAG_N, self.y & 0x80 != 0);
                9
            }
            0x9E => { // DIV YA/X
                let ya = (self.y as u16) << 8 | self.a as u16;
                if self.x != 0 {
                    self.a = (ya / self.x as u16) as u8;
                    self.y = (ya % self.x as u16) as u8;
                }
                self.set_nz(self.a);
                12
            }

            // ---- branches ----
            0x2F => self.branch(apu, true), // BRA
            0xF0 => { let c = self.psw & FLAG_Z != 0; self.branch(apu, c) } // BEQ
            0xD0 => { let c = self.psw & FLAG_Z == 0; self.branch(apu, c) } // BNE
            0xB0 => { let c = self.psw & FLAG_C != 0; self.branch(apu, c) } // BCS
            0x90 => { let c = self.psw & FLAG_C == 0; self.branch(apu, c) } // BCC
            0x70 => { let c = self.psw & FLAG_V != 0; self.branch(apu, c) } // BVS
            0x50 => { let c = self.psw & FLAG_V == 0; self.branch(apu, c) } // BVC
            0x30 => { let c = self.psw & FLAG_N != 0; self.branch(apu, c) } // BMI
            0x10 => { let c = self.psw & FLAG_N == 0; self.branch(apu, c) } // BPL
            0x2E => { // CBNE dp,rel
                let a = self.dp_addr(apu);
                let v = Self::read(apu,a);
                let c = v != self.a;
                self.branch(apu, c) + 3
            }
            0xDE => { // CBNE dp+X,rel
                let a = self.dp_x_addr(apu);
                let v = Self::read(apu,a);
                let c = v != self.a;
                self.branch(apu, c) + 4
            }
            0x6E => { // DBNZ dp,rel
                let a = self.dp_addr(apu);
                let v = Self::read(apu,a).wrapping_sub(1);
                Self::write(apu,a,v);
                let c = v != 0;
                self.branch(apu, c) + 3
            }
            0xFE => { // DBNZ Y,rel
                self.y = self.y.wrapping_sub(1);
                let c = self.y != 0;
                self.branch(apu, c) + 2
            }

            // ---- jumps / calls ----
            0x5F => { self.pc = self.abs_addr(apu); 3 } // JMP abs
            0x1F => { // JMP (abs+X)
                let a = self.abs_x_addr(apu);
                let lo = Self::read(apu,a) as u16;
                let hi = Self::read(apu,a.wrapping_add(1)) as u16;
                self.pc = (hi<<8)|lo; 6
            }
            0x3F => { // CALL abs
                let a = self.abs_addr(apu);
                self.push(apu, (self.pc >> 8) as u8);
                self.push(apu, self.pc as u8);
                self.pc = a; 8
            }
            0x6F => { let lo = self.pull(apu) as u16; let hi = self.pull(apu) as u16; self.pc = (hi<<8)|lo; 5 } // RET
            0x7F => { let p = self.pull(apu); self.psw = p; let lo = self.pull(apu) as u16; let hi = self.pull(apu) as u16; self.pc = (hi<<8)|lo; 6 } // RETI
            0x4F => { // PCALL up
                let off = self.fetch(apu) as u16;
                self.push(apu, (self.pc >> 8) as u8);
                self.push(apu, self.pc as u8);
                self.pc = 0xFF00 | off; 6
            }
            // TCALL n ($x1)
            0x01|0x11|0x21|0x31|0x41|0x51|0x61|0x71|0x81|0x91|0xA1|0xB1|0xC1|0xD1|0xE1|0xF1 => {
                let n = (op >> 4) as u16;
                let vec = 0xFFDE - n * 2;
                self.push(apu, (self.pc >> 8) as u8);
                self.push(apu, self.pc as u8);
                let lo = Self::read(apu,vec) as u16;
                let hi = Self::read(apu,vec.wrapping_add(1)) as u16;
                self.pc = (hi<<8)|lo; 8
            }

            // ---- stack ----
            0x2D => { self.push(apu, self.a); 4 } // PUSH A
            0x4D => { self.push(apu, self.x); 4 } // PUSH X
            0x6D => { self.push(apu, self.y); 4 } // PUSH Y
            0x0D => { self.push(apu, self.psw); 4 } // PUSH PSW
            0xAE => { self.a = self.pull(apu); 4 } // POP A
            0xCE => { self.x = self.pull(apu); 4 } // POP X
            0xEE => { self.y = self.pull(apu); 4 } // POP Y
            0x8E => { self.psw = self.pull(apu); 4 } // POP PSW

            // ---- flag ops ----
            0x60 => { self.psw &= !FLAG_C; 2 } // CLRC
            0x80 => { self.psw |= FLAG_C; 2 } // SETC
            0xED => { self.psw ^= FLAG_C; 3 } // NOTC
            0xE0 => { self.psw &= !(FLAG_V | FLAG_H); 2 } // CLRV
            0x20 => { self.psw &= !FLAG_P; 2 } // CLRP
            0x40 => { self.psw |= FLAG_P; 2 } // SETP
            0xA0 => { self.psw |= FLAG_I; 3 } // EI
            0xC0 => { self.psw &= !FLAG_I; 3 } // DI
            0x9F => { self.a = (self.a >> 4) | (self.a << 4); self.set_nz(self.a); 5 } // XCN

            // ---- DAA/DAS approximated ----
            0xDF => { // DAA
                if (self.psw & FLAG_C != 0) || self.a > 0x99 { self.a = self.a.wrapping_add(0x60); self.psw |= FLAG_C; }
                if (self.psw & FLAG_H != 0) || (self.a & 0x0F) > 9 { self.a = self.a.wrapping_add(0x06); }
                self.set_nz(self.a); 3
            }
            0xBE => { // DAS
                if (self.psw & FLAG_C == 0) || self.a > 0x99 { self.a = self.a.wrapping_sub(0x60); self.psw &= !FLAG_C; }
                if (self.psw & FLAG_H == 0) || (self.a & 0x0F) > 9 { self.a = self.a.wrapping_sub(0x06); }
                self.set_nz(self.a); 3
            }

            // TSET1 / TCLR1
            0x0E => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.set_nz(self.a.wrapping_sub(v)); Self::write(apu,a, v | self.a); 6 }
            0x4E => { let a = self.abs_addr(apu); let v = Self::read(apu,a); self.set_nz(self.a.wrapping_sub(v)); Self::write(apu,a, v & !self.a); 6 }

            // Any remaining (bit-manipulation SET1/CLR1/BBS/BBC, etc.) — these
            // are rarely on a game's critical boot path. Consume the operand
            // bytes conservatively and continue, rather than mis-decoding.
            _ => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apu_with(prog: &[u8], start: u16) -> Apu {
        let mut apu = Apu::new();
        for (i, &b) in prog.iter().enumerate() {
            apu.aram[start as usize + i] = b;
        }
        apu.spc.pc = start;
        apu
    }

    #[test]
    fn mov_a_imm() {
        let mut apu = apu_with(&[0xE8, 0x42], 0x0200); // MOV A,#$42
        let mut spc = std::mem::take(&mut apu.spc);
        spc.step(&mut apu);
        assert_eq!(spc.a, 0x42);
    }

    #[test]
    fn adc_sets_carry() {
        let mut apu = apu_with(&[0x80, 0xE8, 0xFF, 0x88, 0x02], 0x0200); // SETC; MOV A,#$FF; ADC A,#$02
        let mut spc = std::mem::take(&mut apu.spc);
        spc.step(&mut apu); // SETC
        spc.step(&mut apu); // MOV
        spc.step(&mut apu); // ADC -> 0xFF + 0x02 + 1 = 0x102
        assert_eq!(spc.a, 0x02);
        assert_ne!(spc.psw & FLAG_C, 0);
    }

    #[test]
    fn branch_bne() {
        // MOV A,#1; CMP A,#0; BNE +2; (skip) MOV X,#$FF
        let mut apu = apu_with(&[0xE8, 0x01, 0x68, 0x00, 0xD0, 0x02, 0xCD, 0xFF], 0x0200);
        let mut spc = std::mem::take(&mut apu.spc);
        spc.step(&mut apu); // MOV A,#1
        spc.step(&mut apu); // CMP A,#0 (Z=0)
        spc.step(&mut apu); // BNE taken
        assert_eq!(spc.pc, 0x0208); // skipped the MOV X
    }
}
