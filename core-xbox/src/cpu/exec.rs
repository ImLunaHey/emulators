//! IA-32 (x86) instruction decode + execution — a starter interpreter.
//!
//! Built from scratch against the Intel IA-32 SDM Vol. 2 (instruction set). The
//! executor is a plain interpreter: each [`Cpu::step`] consumes any legacy
//! prefixes, fetches the opcode at CS:EIP, decodes the ModR/M + SIB + immediate
//! operands (16- and 32-bit addressing), executes, and advances EIP. x86 is a
//! variable-length, **little-endian** CISC ISA, so unlike the fixed-width
//! PowerPC/MIPS cores the fetch length is data-dependent.
//!
//! # Coverage (this foundation)
//!
//! A meaningful *starter* slice of the integer ISA, enough to single-step real
//! BIOS/boot code a fair way before it needs an unimplemented feature:
//!
//! * the full 8-op ALU group (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) in all six
//!   encodings + the `0x80/0x81/0x83` immediate group,
//! * MOV in every common form (r/m↔r, imm→r/m, imm→reg, moffs, **Sreg**, and
//!   `mov CRn` so boot code can flip into protected mode), plus MOVZX/MOVSX,
//! * INC/DEC/NEG/NOT/TEST, XCHG, LEA, PUSH/POP (reg, imm, r/m, segment),
//! * the shift/rotate group (SHL/SHR/SAR/ROL/ROR),
//! * JMP (short/near/far), Jcc (short + near), SETcc, CALL/RET (near), LOOP,
//!   the flag ops (CLI/STI/CLD/STD/CLC/STC/CMC, PUSHF/POPF, SAHF/LAHF),
//! * HLT, NOP, CPUID, RDTSC, and MUL/DIV (unsigned, with #DE on divide-by-zero).
//!
//! Everything else decodes to the documented [`Decoded::Unimplemented`] seam,
//! which raises an #UD (invalid-opcode) exception — **never** a silent no-op.
//! Protected-mode descriptor loads, paging, privilege checks, and string/REP
//! ops are explicit seams for later phases.

use super::state::*;
use crate::bus::Bus;

/// Legacy instruction prefixes gathered before the opcode.
#[derive(Default, Clone, Copy)]
struct Prefixes {
    /// 0x66 — operand-size override.
    opsize: bool,
    /// 0x67 — address-size override.
    addrsize: bool,
    /// Segment-override prefix (2E/36/3E/26/64/65), if any.
    seg: Option<usize>,
    /// 0xF2/0xF3 — REP/REPNE (recorded; string ops are a future seam).
    rep: u8,
}

/// A decoded ModR/M operand: either a register encoding or a resolved linear
/// memory address (plus the raw effective offset, which `LEA` wants).
#[derive(Clone, Copy)]
enum Ea {
    Reg(u8),
    Mem { lin: u32, off: u32 },
}

/// The eight ALU sub-operations selected by the high opcode bits / group-1 reg
/// field, in their x86 numeric order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Alu {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Cmp,
}

const ALU_BY_INDEX: [Alu; 8] = [
    Alu::Add,
    Alu::Or,
    Alu::Adc,
    Alu::Sbb,
    Alu::And,
    Alu::Sub,
    Alu::Xor,
    Alu::Cmp,
];

/// Marker for the outcome of dispatch — purely for documentation/tests of the
/// decode boundary. The interpreter itself executes inline; this names what a
/// byte decoded to (mirrors the GC core's `Decoded` seam enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Alu,
    Mov,
    Stack,
    IncDec,
    Shift,
    Branch,
    Flags,
    System,
    Nop,
    /// Any opcode not yet handled by this foundation (raises #UD).
    Unimplemented,
}

impl Cpu {
    // ============================ fetch ============================
    /// Linear address of a code offset within CS (real mode masks the offset to
    /// 16 bits — IP wraps inside the 64 KB segment).
    #[inline]
    fn code_linear(&self, off: u32) -> u32 {
        let off = if self.real_mode() { off & 0xFFFF } else { off };
        self.seg_base[CS].wrapping_add(off)
    }

    #[inline]
    fn fetch_u8(&mut self, bus: &mut impl Bus) -> u8 {
        let b = bus.fetch8(self.code_linear(self.eip));
        self.eip = self.eip.wrapping_add(1);
        if self.real_mode() {
            self.eip &= 0xFFFF;
        }
        b
    }
    #[inline]
    fn fetch_u16(&mut self, bus: &mut impl Bus) -> u32 {
        let lo = self.fetch_u8(bus) as u32;
        let hi = self.fetch_u8(bus) as u32;
        lo | (hi << 8)
    }
    #[inline]
    fn fetch_u32(&mut self, bus: &mut impl Bus) -> u32 {
        let b0 = self.fetch_u8(bus) as u32;
        let b1 = self.fetch_u8(bus) as u32;
        let b2 = self.fetch_u8(bus) as u32;
        let b3 = self.fetch_u8(bus) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }
    /// Fetch an 8-bit immediate, sign-extended to 32 bits.
    #[inline]
    fn fetch_i8(&mut self, bus: &mut impl Bus) -> u32 {
        self.fetch_u8(bus) as i8 as i32 as u32
    }
    /// Fetch an operand-size immediate (zero-extended).
    #[inline]
    fn fetch_imm(&mut self, bus: &mut impl Bus, size: u8) -> u32 {
        match size {
            1 => self.fetch_u8(bus) as u32,
            2 => self.fetch_u16(bus),
            _ => self.fetch_u32(bus),
        }
    }

    // ============================ memory ============================
    #[inline]
    fn read_mem(&mut self, bus: &mut impl Bus, lin: u32, size: u8) -> u32 {
        match size {
            1 => bus.read8(lin),
            2 => bus.read16(lin),
            _ => bus.read32(lin),
        }
    }
    #[inline]
    fn write_mem(&mut self, bus: &mut impl Bus, lin: u32, size: u8, v: u32) {
        match size {
            1 => bus.write8(lin, v),
            2 => bus.write16(lin, v),
            _ => bus.write32(lin, v),
        }
    }
    #[inline]
    fn read_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) -> u32 {
        match ea {
            Ea::Reg(r) => self.reg(r as usize, size),
            Ea::Mem { lin, .. } => self.read_mem(bus, lin, size),
        }
    }
    #[inline]
    fn write_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, v: u32) {
        match ea {
            Ea::Reg(r) => self.set_reg(r as usize, size, v),
            Ea::Mem { lin, .. } => self.write_mem(bus, lin, size, v),
        }
    }

    // ============================ ModR/M ============================
    /// Decode a ModR/M byte (and any SIB/displacement) into the reg field and an
    /// effective operand, honouring the address size and segment override.
    fn modrm(&mut self, bus: &mut impl Bus, p: &Prefixes, asize: u8) -> (u8, Ea) {
        let b = self.fetch_u8(bus);
        let md = b >> 6;
        let reg = (b >> 3) & 7;
        let rm = b & 7;
        if md == 3 {
            return (reg, Ea::Reg(rm));
        }
        let (off, seg_def) = if asize == 2 {
            self.ea16(bus, md, rm)
        } else {
            self.ea32(bus, md, rm)
        };
        let seg = p.seg.unwrap_or(seg_def);
        let lin = self.seg_base[seg].wrapping_add(off);
        (reg, Ea::Mem { lin, off })
    }

    /// 32-bit effective-address computation (with SIB). Returns (offset, default
    /// segment).
    fn ea32(&mut self, bus: &mut impl Bus, md: u8, rm: u8) -> (u32, usize) {
        let mut seg = DS;
        let off;
        if rm == 4 {
            let sib = self.fetch_u8(bus);
            let scale = sib >> 6;
            let index = (sib >> 3) & 7;
            let base = sib & 7;
            let mut addr = 0u32;
            if base == 5 && md == 0 {
                addr = addr.wrapping_add(self.fetch_u32(bus)); // disp32, no base
            } else {
                addr = addr.wrapping_add(self.reg32(base as usize));
                if base == 4 || base == 5 {
                    seg = SS; // ESP/EBP base defaults to the stack segment
                }
            }
            if index != 4 {
                addr = addr.wrapping_add(self.reg32(index as usize) << scale);
            }
            match md {
                1 => addr = addr.wrapping_add(self.fetch_i8(bus)),
                2 => addr = addr.wrapping_add(self.fetch_u32(bus)),
                _ => {}
            }
            off = addr;
        } else if rm == 5 && md == 0 {
            off = self.fetch_u32(bus); // disp32 absolute
        } else {
            let mut addr = self.reg32(rm as usize);
            if rm == 5 {
                seg = SS; // [EBP] defaults to SS
            }
            match md {
                1 => addr = addr.wrapping_add(self.fetch_i8(bus)),
                2 => addr = addr.wrapping_add(self.fetch_u32(bus)),
                _ => {}
            }
            off = addr;
        }
        (off, seg)
    }

    /// 16-bit effective-address computation (the classic [bx+si] table).
    fn ea16(&mut self, bus: &mut impl Bus, md: u8, rm: u8) -> (u32, usize) {
        let mut seg = DS;
        let mut off = match rm {
            0 => self.reg16(EBX).wrapping_add(self.reg16(ESI)),
            1 => self.reg16(EBX).wrapping_add(self.reg16(EDI)),
            2 => {
                seg = SS;
                self.reg16(EBP).wrapping_add(self.reg16(ESI))
            }
            3 => {
                seg = SS;
                self.reg16(EBP).wrapping_add(self.reg16(EDI))
            }
            4 => self.reg16(ESI),
            5 => self.reg16(EDI),
            6 => {
                if md == 0 {
                    0 // disp16 absolute (filled below)
                } else {
                    seg = SS;
                    self.reg16(EBP)
                }
            }
            _ => self.reg16(EBX),
        };
        if rm == 6 && md == 0 {
            off = self.fetch_u16(bus);
        } else {
            match md {
                1 => off = off.wrapping_add(self.fetch_i8(bus)),
                2 => off = off.wrapping_add(self.fetch_u16(bus)),
                _ => {}
            }
        }
        (off & 0xFFFF, seg)
    }

    // ============================ stack ============================
    fn push(&mut self, bus: &mut impl Bus, v: u32, size: u8) {
        if self.real_mode() {
            let sp = self.reg16(ESP).wrapping_sub(size as u32) & 0xFFFF;
            self.set_reg16(ESP, sp);
            let lin = self.seg_base[SS].wrapping_add(sp);
            self.write_mem(bus, lin, size, v);
        } else {
            let esp = self.reg32(ESP).wrapping_sub(size as u32);
            self.set_reg32(ESP, esp);
            let lin = self.seg_base[SS].wrapping_add(esp);
            self.write_mem(bus, lin, size, v);
        }
    }
    fn pop(&mut self, bus: &mut impl Bus, size: u8) -> u32 {
        if self.real_mode() {
            let sp = self.reg16(ESP);
            let lin = self.seg_base[SS].wrapping_add(sp);
            let v = self.read_mem(bus, lin, size);
            self.set_reg16(ESP, sp.wrapping_add(size as u32) & 0xFFFF);
            v
        } else {
            let esp = self.reg32(ESP);
            let lin = self.seg_base[SS].wrapping_add(esp);
            let v = self.read_mem(bus, lin, size);
            self.set_reg32(ESP, esp.wrapping_add(size as u32));
            v
        }
    }

    // ============================ branch helpers ============================
    /// Set EIP, masking to 16 bits in real mode.
    #[inline]
    fn set_eip(&mut self, v: u32) {
        self.eip = if self.real_mode() { v & 0xFFFF } else { v };
    }
    /// Take a relative branch (`disp` already sign-extended) from the current
    /// (post-instruction) EIP.
    #[inline]
    fn jump_rel(&mut self, disp: u32) {
        let t = self.eip.wrapping_add(disp);
        self.set_eip(t);
    }

    /// Evaluate an x86 condition code (the low nibble of a Jcc/SETcc opcode).
    fn cc(&self, c: u8) -> bool {
        let f = |m: u32| self.flag(m);
        match c & 0xF {
            0x0 => f(OF),
            0x1 => !f(OF),
            0x2 => f(CF),
            0x3 => !f(CF),
            0x4 => f(ZF),
            0x5 => !f(ZF),
            0x6 => f(CF) || f(ZF),
            0x7 => !f(CF) && !f(ZF),
            0x8 => f(SF),
            0x9 => !f(SF),
            0xA => f(PF),
            0xB => !f(PF),
            0xC => f(SF) != f(OF),
            0xD => f(SF) == f(OF),
            0xE => f(ZF) || (f(SF) != f(OF)),
            _ => !f(ZF) && (f(SF) == f(OF)),
        }
    }

    // ============================ ALU ============================
    /// Apply an ALU op, returning (result, should-write-back).
    fn alu(&mut self, op: Alu, a: u32, b: u32, size: u8) -> (u32, bool) {
        match op {
            Alu::Add => (self.flags_add(a, b, size), true),
            Alu::Sub => (self.flags_sub(a, b, size), true),
            Alu::Cmp => (self.flags_sub(a, b, size), false),
            Alu::And => (self.flags_logic(a & b, size), true),
            Alu::Or => (self.flags_logic(a | b, size), true),
            Alu::Xor => (self.flags_logic(a ^ b, size), true),
            Alu::Adc => (self.flags_adc(a, b, size), true),
            Alu::Sbb => (self.flags_sbb(a, b, size), true),
        }
    }

    /// ADD-with-carry flags (`a + b + CF`).
    fn flags_adc(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Cpu::size_mask(size);
        let cf = self.flag(CF) as u64;
        let sum = (a & m) as u64 + (b & m) as u64 + cf;
        let res = (sum as u32) & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, res == 0);
        self.set_flag(SF, res & sign != 0);
        self.set_flag(PF, (res as u8).count_ones() % 2 == 0);
        self.set_flag(CF, sum > m as u64);
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(OF, ((!(a ^ b)) & (a ^ res) & sign) != 0);
        res
    }
    /// SUB-with-borrow flags (`a - b - CF`).
    fn flags_sbb(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Cpu::size_mask(size);
        let cf = self.flag(CF) as i64;
        let diff = (a & m) as i64 - (b & m) as i64 - cf;
        let res = (diff as u32) & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, res == 0);
        self.set_flag(SF, res & sign != 0);
        self.set_flag(PF, (res as u8).count_ones() % 2 == 0);
        self.set_flag(CF, diff < 0);
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(OF, ((a ^ b) & (a ^ res) & sign) != 0);
        res
    }

    // ============================ step ============================
    /// Execute one instruction. Consume prefixes, fetch + decode the opcode,
    /// execute, and advance EIP. Faults/HLT freeze the core (checked first).
    pub fn step(&mut self, bus: &mut impl Bus) {
        if self.halted || self.fault.is_some() {
            return;
        }
        let start_eip = self.eip;

        // ---- legacy prefixes ----
        let mut p = Prefixes::default();
        let mut op;
        let mut guard = 0;
        loop {
            op = self.fetch_u8(bus);
            match op {
                0x66 => p.opsize = true,
                0x67 => p.addrsize = true,
                0x2E => p.seg = Some(CS),
                0x36 => p.seg = Some(SS),
                0x3E => p.seg = Some(DS),
                0x26 => p.seg = Some(ES),
                0x64 => p.seg = Some(FS),
                0x65 => p.seg = Some(GS),
                0xF0 => {} // LOCK — no-op for a single-threaded interpreter
                0xF2 | 0xF3 => p.rep = op,
                _ => break,
            }
            guard += 1;
            if guard > 15 {
                self.eip = start_eip;
                self.raise(Exception::GeneralProtection, 0, op);
                return;
            }
        }

        // Operand / address size after the override prefixes.
        let osize = match (self.default_opsize(), p.opsize) {
            (2, false) => 2,
            (2, true) => 4,
            (4, true) => 2,
            _ => 4,
        };
        let asize = match (self.default_opsize(), p.addrsize) {
            (2, false) => 2,
            (2, true) => 4,
            (4, true) => 2,
            _ => 4,
        };

        self.instret = self.instret.wrapping_add(1);

        // ---- ALU group (0x00..0x3F, low 3 bits < 6) ----
        if op < 0x40 && (op & 7) < 6 {
            let alu = ALU_BY_INDEX[(op >> 3) as usize];
            self.exec_alu_group(bus, op, alu, osize, asize, &p);
            return;
        }

        match op {
            // ---- INC/DEC reg (operand size) ----
            0x40..=0x47 => {
                let r = (op - 0x40) as usize;
                let v = self.flags_inc(self.reg(r, osize), osize);
                self.set_reg(r, osize, v);
            }
            0x48..=0x4F => {
                let r = (op - 0x48) as usize;
                let v = self.flags_dec(self.reg(r, osize), osize);
                self.set_reg(r, osize, v);
            }

            // ---- PUSH/POP reg ----
            0x50..=0x57 => {
                let v = self.reg((op - 0x50) as usize, osize);
                self.push(bus, v, osize);
            }
            0x58..=0x5F => {
                let v = self.pop(bus, osize);
                self.set_reg((op - 0x58) as usize, osize, v);
            }

            // ---- PUSH imm ----
            0x68 => {
                let v = self.fetch_imm(bus, osize);
                self.push(bus, v, osize);
            }
            0x6A => {
                let v = self.fetch_i8(bus);
                self.push(bus, v, osize);
            }

            // ---- PUSH/POP segment (one-byte forms) ----
            0x06 => {
                let v = self.seg_sel[ES] as u32;
                self.push(bus, v, osize);
            }
            0x0E => {
                let v = self.seg_sel[CS] as u32;
                self.push(bus, v, osize);
            }
            0x16 => {
                let v = self.seg_sel[SS] as u32;
                self.push(bus, v, osize);
            }
            0x1E => {
                let v = self.seg_sel[DS] as u32;
                self.push(bus, v, osize);
            }
            0x07 => {
                let v = self.pop(bus, osize);
                self.set_seg(ES, v as u16);
            }
            0x17 => {
                let v = self.pop(bus, osize);
                self.set_seg(SS, v as u16);
            }
            0x1F => {
                let v = self.pop(bus, osize);
                self.set_seg(DS, v as u16);
            }

            // ---- group 1: ALU r/m, imm (0x80/0x81/0x83) ----
            0x80 | 0x81 | 0x83 => {
                let size = if op == 0x80 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let imm = if op == 0x83 {
                    self.fetch_i8(bus) // sign-extended imm8
                } else {
                    self.fetch_imm(bus, size)
                };
                let a = self.read_ea(bus, ea, size);
                let (res, wr) = self.alu(ALU_BY_INDEX[reg as usize], a, imm, size);
                if wr {
                    self.write_ea(bus, ea, size, res);
                }
            }

            // ---- TEST r/m, r ----
            0x84 | 0x85 => {
                let size = if op == 0x84 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                self.flags_logic(a & b, size);
            }
            // ---- XCHG r/m, r ----
            0x86 | 0x87 => {
                let size = if op == 0x86 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                self.write_ea(bus, ea, size, b);
                self.set_reg(reg as usize, size, a);
            }

            // ---- MOV r/m, r and r, r/m ----
            0x88 | 0x89 => {
                let size = if op == 0x88 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.reg(reg as usize, size);
                self.write_ea(bus, ea, size, v);
            }
            0x8A | 0x8B => {
                let size = if op == 0x8A { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, size);
                self.set_reg(reg as usize, size, v);
            }
            // ---- MOV r/m16, Sreg  and  MOV Sreg, r/m16 ----
            0x8C => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.seg_sel[(reg & 7) as usize] as u32;
                self.write_ea(bus, ea, 2, v);
            }
            0x8E => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, 2);
                self.set_seg((reg & 7) as usize, v as u16);
            }
            // ---- LEA r, m ----
            0x8D => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                match ea {
                    Ea::Mem { off, .. } => self.set_reg(reg as usize, osize, off),
                    Ea::Reg(_) => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }
            // ---- POP r/m ----
            0x8F => {
                let (_reg, ea) = self.modrm(bus, &p, asize);
                let v = self.pop(bus, osize);
                self.write_ea(bus, ea, osize, v);
            }

            // ---- NOP / XCHG eAX, reg ----
            0x90 => { /* NOP (xchg eAX,eAX) */ }
            0x91..=0x97 => {
                let r = (op - 0x90) as usize;
                let a = self.reg(EAX, osize);
                let b = self.reg(r, osize);
                self.set_reg(EAX, osize, b);
                self.set_reg(r, osize, a);
            }
            // CBW / CWDE — sign-extend AL->AX (osize 2) or AX->EAX (osize 4).
            0x98 => {
                if osize == 2 {
                    self.set_reg16(EAX, self.reg8(EAX) as i8 as i16 as u16 as u32);
                } else {
                    self.set_reg32(EAX, self.reg16(EAX) as i16 as i32 as u32);
                }
            }
            // CWD / CDQ — sign-extend AX->DX:AX (osize 2) or EAX->EDX:EAX (osize 4).
            0x99 => {
                if osize == 2 {
                    let s = if self.reg16(EAX) & 0x8000 != 0 { 0xFFFF } else { 0 };
                    self.set_reg16(EDX, s);
                } else {
                    let s = if self.reg32(EAX) & 0x8000_0000 != 0 { 0xFFFF_FFFF } else { 0 };
                    self.set_reg32(EDX, s);
                }
            }

            // ---- MOV moffs (AL/eAX ↔ [disp]) ----
            0xA0 | 0xA1 | 0xA2 | 0xA3 => {
                let size = if op & 1 == 0 { 1 } else { osize };
                let off = if asize == 2 {
                    self.fetch_u16(bus)
                } else {
                    self.fetch_u32(bus)
                };
                let seg = p.seg.unwrap_or(DS);
                let lin = self.seg_base[seg].wrapping_add(off);
                if op <= 0xA1 {
                    let v = self.read_mem(bus, lin, size);
                    self.set_reg(EAX, size, v);
                } else {
                    let v = self.reg(EAX, size);
                    self.write_mem(bus, lin, size, v);
                }
            }
            // ---- TEST AL/eAX, imm ----
            0xA8 => {
                let imm = self.fetch_u8(bus) as u32;
                let a = self.reg8(EAX);
                self.flags_logic(a & imm, 1);
            }
            0xA9 => {
                let imm = self.fetch_imm(bus, osize);
                let a = self.reg(EAX, osize);
                self.flags_logic(a & imm, osize);
            }

            // ---- MOV r8/r, imm ----
            0xB0..=0xB7 => {
                let imm = self.fetch_u8(bus) as u32;
                self.set_reg8((op - 0xB0) as usize, imm);
            }
            0xB8..=0xBF => {
                let imm = self.fetch_imm(bus, osize);
                self.set_reg((op - 0xB8) as usize, osize, imm);
            }
            // ---- MOV r/m, imm ----
            0xC6 | 0xC7 => {
                let size = if op == 0xC6 { 1 } else { osize };
                let (_reg, ea) = self.modrm(bus, &p, asize);
                let imm = self.fetch_imm(bus, size);
                self.write_ea(bus, ea, size, imm);
            }

            // ---- shift/rotate group 2 ----
            0xC0 | 0xC1 | 0xD0 | 0xD1 | 0xD2 | 0xD3 => {
                let size = if op & 1 == 0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let count = match op {
                    0xC0 | 0xC1 => self.fetch_u8(bus) as u32,
                    0xD0 | 0xD1 => 1,
                    _ => self.reg8(ECX),
                };
                let v = self.read_ea(bus, ea, size);
                match self.do_shift(reg, v, count, size) {
                    Some(res) => self.write_ea(bus, ea, size, res),
                    None => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }

            // ---- RET near ----
            0xC3 => {
                let v = self.pop(bus, osize);
                self.set_eip(v);
            }
            0xC2 => {
                let n = self.fetch_u16(bus);
                let v = self.pop(bus, osize);
                self.set_eip(v);
                // pop the imm16 bytes off the caller's stack
                if self.real_mode() {
                    let sp = self.reg16(ESP).wrapping_add(n) & 0xFFFF;
                    self.set_reg16(ESP, sp);
                } else {
                    let esp = self.reg32(ESP).wrapping_add(n);
                    self.set_reg32(ESP, esp);
                }
            }

            // ---- group 3: TEST/NOT/NEG/MUL/DIV ----
            0xF6 | 0xF7 => {
                let size = if op == 0xF6 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                self.exec_group3(bus, reg, ea, size, op, start_eip);
            }
            // ---- group 4/5: INC/DEC/CALL/JMP/PUSH r/m ----
            0xFE => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, 1);
                match reg {
                    0 => {
                        let r = self.flags_inc(v, 1);
                        self.write_ea(bus, ea, 1, r);
                    }
                    1 => {
                        let r = self.flags_dec(v, 1);
                        self.write_ea(bus, ea, 1, r);
                    }
                    _ => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }
            0xFF => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                self.exec_group5(bus, reg, ea, osize, op, start_eip);
            }

            // ---- relative jumps / calls ----
            0xEB => {
                let d = self.fetch_i8(bus);
                self.jump_rel(d);
            }
            0xE9 => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                self.jump_rel(d);
            }
            0xEA => {
                // far JMP ptr16:16/32 — new EIP then new CS selector.
                let off = self.fetch_imm(bus, osize);
                let sel = self.fetch_u16(bus) as u16;
                self.set_seg(CS, sel);
                self.set_eip(off);
            }
            0x70..=0x7F => {
                let d = self.fetch_i8(bus);
                if self.cc(op - 0x70) {
                    self.jump_rel(d);
                }
            }
            0xE8 => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                let ret = self.eip;
                self.push(bus, ret, osize);
                self.jump_rel(d);
            }
            0xE3 => {
                // JCXZ / JECXZ
                let d = self.fetch_i8(bus);
                let cx = if asize == 2 { self.reg16(ECX) } else { self.reg32(ECX) };
                if cx == 0 {
                    self.jump_rel(d);
                }
            }
            0xE0 | 0xE1 | 0xE2 => {
                let d = self.fetch_i8(bus);
                let cx = if asize == 2 {
                    let c = self.reg16(ECX).wrapping_sub(1) & 0xFFFF;
                    self.set_reg16(ECX, c);
                    c
                } else {
                    let c = self.reg32(ECX).wrapping_sub(1);
                    self.set_reg32(ECX, c);
                    c
                };
                let take = match op {
                    0xE0 => cx != 0 && !self.flag(ZF), // LOOPNE
                    0xE1 => cx != 0 && self.flag(ZF),  // LOOPE
                    _ => cx != 0,                      // LOOP
                };
                if take {
                    self.jump_rel(d);
                }
            }

            // ---- flag ops ----
            0xF4 => self.halted = true, // HLT
            0xF5 => self.eflags ^= CF,  // CMC
            0xF8 => self.set_flag(CF, false),
            0xF9 => self.set_flag(CF, true),
            0xFA => self.set_flag(IF, false), // CLI
            0xFB => self.set_flag(IF, true),  // STI
            0xFC => self.set_flag(DF, false), // CLD
            0xFD => self.set_flag(DF, true),  // STD
            0x9C => {
                let v = self.eflags;
                self.push(bus, v, osize);
            }
            0x9D => {
                let v = self.pop(bus, osize);
                self.eflags = (v | EFLAGS_ALWAYS_ONE) & 0x003F_7FD5 | EFLAGS_ALWAYS_ONE;
            }
            0x9E => {
                // SAHF: AH -> low byte of EFLAGS (SF ZF xx AF xx PF xx CF).
                let ah = self.reg8(4);
                self.eflags = (self.eflags & 0xFFFF_FF00) | (ah & 0xD5) | EFLAGS_ALWAYS_ONE;
            }
            0x9F => {
                // LAHF: low byte of EFLAGS -> AH.
                let lo = (self.eflags & 0xD5) | 0x02;
                self.set_reg8(4, lo);
            }

            // ---- two-byte (0x0F) ----
            0x0F => self.exec_0f(bus, osize, asize, &p, start_eip),

            // ---- everything else: documented #UD seam ----
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// ALU group dispatch for the six register/immediate-accumulator encodings.
    fn exec_alu_group(
        &mut self,
        bus: &mut impl Bus,
        op: u8,
        alu: Alu,
        osize: u8,
        asize: u8,
        p: &Prefixes,
    ) {
        match op & 7 {
            0 | 1 => {
                let size = if op & 7 == 0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                let (res, wr) = self.alu(alu, a, b, size);
                if wr {
                    self.write_ea(bus, ea, size, res);
                }
            }
            2 | 3 => {
                let size = if op & 7 == 2 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let a = self.reg(reg as usize, size);
                let b = self.read_ea(bus, ea, size);
                let (res, wr) = self.alu(alu, a, b, size);
                if wr {
                    self.set_reg(reg as usize, size, res);
                }
            }
            4 => {
                let imm = self.fetch_u8(bus) as u32;
                let a = self.reg8(EAX);
                let (res, wr) = self.alu(alu, a, imm, 1);
                if wr {
                    self.set_reg8(EAX, res);
                }
            }
            _ => {
                let imm = self.fetch_imm(bus, osize);
                let a = self.reg(EAX, osize);
                let (res, wr) = self.alu(alu, a, imm, osize);
                if wr {
                    self.set_reg(EAX, osize, res);
                }
            }
        }
    }

    /// Group 3 (0xF6/0xF7): TEST imm / NOT / NEG / MUL / DIV (unsigned).
    /// IMUL/IDIV are documented seams (#UD).
    fn exec_group3(
        &mut self,
        bus: &mut impl Bus,
        reg: u8,
        ea: Ea,
        size: u8,
        op: u8,
        start_eip: u32,
    ) {
        match reg {
            0 | 1 => {
                let imm = self.fetch_imm(bus, size);
                let a = self.read_ea(bus, ea, size);
                self.flags_logic(a & imm, size);
            }
            2 => {
                let v = self.read_ea(bus, ea, size);
                self.write_ea(bus, ea, size, !v); // NOT — no flags
            }
            3 => {
                let v = self.read_ea(bus, ea, size);
                let res = self.flags_sub(0, v, size); // NEG = 0 - v
                self.write_ea(bus, ea, size, res);
            }
            4 => self.do_mul(bus, ea, size), // MUL (unsigned)
            5 => self.do_imul1(bus, ea, size), // IMUL (one-operand, signed)
            6 => self.do_div(bus, ea, size, start_eip), // DIV (unsigned)
            7 => self.do_idiv(bus, ea, size, start_eip), // IDIV (signed)
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// One-operand signed multiply (IMUL r/m): AL/AX/EAX * src into AX/DX:AX/
    /// EDX:EAX. CF=OF when the high half isn't the sign-extension of the low half.
    fn do_imul1(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) {
        let src = sign_ext(self.read_ea(bus, ea, size), size) as i32 as i64;
        match size {
            1 => {
                let r = (self.reg8(EAX) as i8 as i64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                let of = r as i8 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
            2 => {
                let r = (self.reg16(EAX) as i16 as i64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                self.set_reg16(EDX, (r as u32 >> 16) & 0xFFFF);
                let of = r as i16 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
            _ => {
                let r = (self.reg32(EAX) as i32 as i64) * src;
                self.set_reg32(EAX, r as u32);
                self.set_reg32(EDX, (r >> 32) as u32);
                let of = r as i32 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
        }
    }

    /// Signed divide (IDIV r/m); raises #DE on divide-by-zero or quotient
    /// overflow.
    fn do_idiv(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, start_eip: u32) {
        let d = sign_ext(self.read_ea(bus, ea, size), size) as i32 as i64;
        if d == 0 {
            self.eip = start_eip;
            self.raise(Exception::DivideError, 0, 0);
            return;
        }
        match size {
            1 => {
                let n = self.reg16(EAX) as i16 as i64;
                let (q, r) = (n / d, n % d);
                if !(-128..=127).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg8(EAX, q as u32);
                self.set_reg8(4, r as u32); // AH
            }
            2 => {
                let n = (((self.reg16(EDX)) << 16) | self.reg16(EAX)) as i32 as i64;
                let (q, r) = (n / d, n % d);
                if !(i16::MIN as i64..=i16::MAX as i64).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg16(EAX, q as u32);
                self.set_reg16(EDX, r as u32);
            }
            _ => {
                let n = (((self.reg32(EDX) as u64) << 32) | self.reg32(EAX) as u64) as i64;
                let (q, r) = (n / d, n % d);
                if !(i32::MIN as i64..=i32::MAX as i64).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg32(EAX, q as u32);
                self.set_reg32(EDX, r as u32);
            }
        }
    }

    /// Group 5 (0xFF): INC/DEC/CALL near/JMP near/PUSH r/m. Far call/jmp are
    /// documented seams (#UD).
    fn exec_group5(
        &mut self,
        bus: &mut impl Bus,
        reg: u8,
        ea: Ea,
        osize: u8,
        op: u8,
        start_eip: u32,
    ) {
        match reg {
            0 => {
                let v = self.read_ea(bus, ea, osize);
                let r = self.flags_inc(v, osize);
                self.write_ea(bus, ea, osize, r);
            }
            1 => {
                let v = self.read_ea(bus, ea, osize);
                let r = self.flags_dec(v, osize);
                self.write_ea(bus, ea, osize, r);
            }
            2 => {
                // CALL near indirect
                let target = self.read_ea(bus, ea, osize);
                let ret = self.eip;
                self.push(bus, ret, osize);
                self.set_eip(target);
            }
            4 => {
                // JMP near indirect
                let target = self.read_ea(bus, ea, osize);
                self.set_eip(target);
            }
            6 => {
                let v = self.read_ea(bus, ea, osize);
                self.push(bus, v, osize);
            }
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// Unsigned MUL: AX = AL*r/m8, DX:AX = AX*r/m16, EDX:EAX = EAX*r/m32. CF/OF
    /// set when the upper half is non-zero.
    fn do_mul(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) {
        let src = self.read_ea(bus, ea, size) as u64;
        match size {
            1 => {
                let r = (self.reg8(EAX) as u64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                let upper = (r >> 8) & 0xFF != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
            2 => {
                let r = (self.reg16(EAX) as u64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                self.set_reg16(EDX, (r >> 16) as u32 & 0xFFFF);
                let upper = (r >> 16) & 0xFFFF != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
            _ => {
                let r = (self.reg32(EAX) as u64) * src;
                self.set_reg32(EAX, r as u32);
                self.set_reg32(EDX, (r >> 32) as u32);
                let upper = (r >> 32) != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
        }
    }

    /// Unsigned DIV: raises #DE on divide-by-zero or quotient overflow.
    fn do_div(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, start_eip: u32) {
        let d = self.read_ea(bus, ea, size) as u64;
        if d == 0 {
            self.eip = start_eip;
            self.raise(Exception::DivideError, 0, 0);
            return;
        }
        match size {
            1 => {
                let n = self.reg16(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg8(EAX, q as u32); // AL
                self.set_reg8(4, r as u32); // AH
            }
            2 => {
                let n = ((self.reg16(EDX) as u64) << 16) | self.reg16(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFFFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg16(EAX, q as u32);
                self.set_reg16(EDX, r as u32);
            }
            _ => {
                let n = ((self.reg32(EDX) as u64) << 32) | self.reg32(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFFFF_FFFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg32(EAX, q as u32);
                self.set_reg32(EDX, r as u32);
            }
        }
    }

    /// Shift/rotate group-2 sub-op (`reg` field). Returns None for the
    /// not-yet-implemented through-carry rotates (RCL/RCR) so the caller raises
    /// #UD. SHL/SHR/SAR set SZP+CF (+OF for count 1); ROL/ROR set CF (+OF for
    /// count 1) but leave SZP.
    fn do_shift(&mut self, reg: u8, val: u32, count: u32, size: u8) -> Option<u32> {
        let bits = (size as u32) * 8;
        let count = count & 0x1F;
        let m = Cpu::size_mask(size);
        let sign = m ^ (m >> 1);
        let v = val & m;
        if count == 0 {
            return Some(v);
        }
        let res = match reg {
            4 | 6 => {
                // SHL / SAL
                let r = (v << count) & m;
                let cf = count <= bits && (v >> (bits - count)) & 1 != 0;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, (r & sign != 0) ^ cf);
                }
                r
            }
            5 => {
                // SHR
                let cf = (v >> (count - 1)) & 1 != 0;
                let r = v >> count;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, v & sign != 0);
                }
                r
            }
            7 => {
                // SAR (arithmetic — sign-extend then signed shift)
                let sv = sign_ext(v, size) as i32;
                let r = ((sv >> count.min(31)) as u32) & m;
                let cf = (sv >> (count - 1).min(31)) & 1 != 0;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, false);
                }
                r
            }
            0 => {
                // ROL
                let c = count % bits;
                let r = if c == 0 { v } else { ((v << c) | (v >> (bits - c))) & m };
                let cf = r & 1 != 0;
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, (r & sign != 0) ^ cf);
                }
                r
            }
            1 => {
                // ROR
                let c = count % bits;
                let r = if c == 0 { v } else { ((v >> c) | (v << (bits - c))) & m };
                let cf = r & sign != 0;
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, ((r >> (bits - 1)) ^ (r >> (bits - 2))) & 1 != 0);
                }
                r
            }
            // RCL (2) / RCR (3): through-carry rotates — future seam.
            _ => return None,
        };
        Some(res)
    }

    /// Two-byte (0x0F-prefixed) opcodes: a small system/utility slice.
    fn exec_0f(
        &mut self,
        bus: &mut impl Bus,
        osize: u8,
        asize: u8,
        p: &Prefixes,
        start_eip: u32,
    ) {
        let op2 = self.fetch_u8(bus);
        match op2 {
            // MOV r32, CRn  /  MOV CRn, r32
            0x20 => {
                let b = self.fetch_u8(bus);
                let cr = ((b >> 3) & 7) as usize;
                let rm = (b & 7) as usize;
                self.set_reg32(rm, self.cr.get(cr).copied().unwrap_or(0));
            }
            0x22 => {
                let b = self.fetch_u8(bus);
                let cr = ((b >> 3) & 7) as usize;
                let rm = (b & 7) as usize;
                if cr < self.cr.len() {
                    self.cr[cr] = self.reg32(rm);
                }
            }
            // RDTSC: EDX:EAX <- retired-instruction counter (our clock proxy).
            0x31 => {
                self.set_reg32(EAX, self.instret as u32);
                self.set_reg32(EDX, (self.instret >> 32) as u32);
            }
            // CPUID: a minimal, plausible Pentium III response.
            0xA2 => self.do_cpuid(),
            // Jcc near
            0x80..=0x8F => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                if self.cc(op2 - 0x80) {
                    self.jump_rel(d);
                }
            }
            // SETcc r/m8
            0x90..=0x9F => {
                let (_reg, ea) = self.modrm(bus, p, asize);
                let v = self.cc(op2 - 0x90) as u32;
                self.write_ea(bus, ea, 1, v);
            }
            // MOVZX
            0xB6 | 0xB7 => {
                let src = if op2 == 0xB6 { 1 } else { 2 };
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = self.read_ea(bus, ea, src);
                self.set_reg(reg as usize, osize, v);
            }
            // MOVSX
            0xBE | 0xBF => {
                let src = if op2 == 0xBE { 1 } else { 2 };
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = sign_ext(self.read_ea(bus, ea, src), src);
                self.set_reg(reg as usize, osize, v);
            }
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op2);
            }
        }
    }

    /// A minimal CPUID: leaf 0 returns the vendor string + max leaf; leaf 1
    /// returns the reset signature. Enough to satisfy a feature probe without
    /// pretending to a full feature set.
    fn do_cpuid(&mut self) {
        match self.reg32(EAX) {
            0 => {
                self.set_reg32(EAX, 1); // max standard leaf
                self.set_reg32(EBX, 0x756E_6547); // "Genu"
                self.set_reg32(EDX, 0x4969_6E65); // "ineI"
                self.set_reg32(ECX, 0x6C65_746E); // "ntel"
            }
            _ => {
                self.set_reg32(EAX, RESET_EDX); // family/model/stepping
                self.set_reg32(EBX, 0);
                self.set_reg32(ECX, 0);
                self.set_reg32(EDX, 0x0000_0001); // FPU present (token feature bit)
            }
        }
    }

    /// Public wrapper so [`do_shift`] can set SZP (the inherent helper is
    /// private to `state.rs`); recomputes the same SF/ZF/PF subset.
    fn set_szp_pub(&mut self, res: u32, size: u8) {
        let m = Cpu::size_mask(size);
        let r = res & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, r == 0);
        self.set_flag(SF, r & sign != 0);
        self.set_flag(PF, (r as u8).count_ones() % 2 == 0);
    }
}

/// Sign-extend a value of `size` bytes to a full 32-bit word.
#[inline]
fn sign_ext(v: u32, size: u8) -> u32 {
    match size {
        1 => v as u8 as i8 as i32 as u32,
        2 => v as u16 as i16 as i32 as u32,
        _ => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xbox::Xbox;

    /// A harness: an `Xbox` with a small program in RAM and the CPU pointed at
    /// it in flat 32-bit protected mode (so we exercise 32-bit decoding without
    /// modelling a GDT).
    fn harness(program: &[u8]) -> Xbox {
        let mut xb = Xbox::new();
        let base = 0x1_0000u32;
        for (i, &b) in program.iter().enumerate() {
            xb.mem.ram_write8(base + i as u32, b as u32);
        }
        // Flat protected mode: PE=1, all segment bases 0, CS:EIP -> program.
        xb.cpu.cr[0] |= CR0_PE;
        for s in 0..6 {
            xb.cpu.seg_base[s] = 0;
            xb.cpu.seg_sel[s] = 0x08;
        }
        xb.cpu.eip = base;
        xb.cpu.set_reg32(ESP, 0x2_0000);
        xb
    }

    fn run(xb: &mut Xbox, n: usize) {
        for _ in 0..n {
            let mut cpu = std::mem::take(&mut xb.cpu);
            cpu.step(xb);
            xb.cpu = cpu;
        }
    }

    #[test]
    fn mov_imm32_and_add() {
        // mov eax, 5 ; mov ebx, 9 ; add eax, ebx
        let mut xb = harness(&[
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
            0xBB, 0x09, 0x00, 0x00, 0x00, // mov ebx,9
            0x01, 0xD8, // add eax,ebx
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 14);
    }

    #[test]
    fn sub_sets_zero_flag() {
        // mov eax,7 ; sub eax,7
        let mut xb = harness(&[0xB8, 0x07, 0x00, 0x00, 0x00, 0x29, 0xC0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(ZF));
    }

    #[test]
    fn xor_self_clears_register() {
        // xor eax,eax
        let mut xb = harness(&[0xB8, 0xFF, 0x00, 0x00, 0x00, 0x31, 0xC0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(ZF));
    }

    #[test]
    fn push_pop_round_trips_through_stack() {
        // mov eax,0xCAFEBABE ; push eax ; pop ebx
        let mut xb = harness(&[
            0xB8, 0xBE, 0xBA, 0xFE, 0xCA, // mov eax,0xCAFEBABE
            0x50, // push eax
            0x5B, // pop ebx
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 0xCAFE_BABE);
        assert_eq!(xb.cpu.reg32(ESP), 0x2_0000, "stack balanced");
    }

    #[test]
    fn inc_dec_preserve_carry() {
        // stc ; mov eax,0 ; inc eax  — CF must survive INC.
        let mut xb = harness(&[0xF9, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x40]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 1);
        assert!(xb.cpu.flag(CF));
    }

    #[test]
    fn jmp_short_skips_instruction() {
        // jmp +2 (skip the mov) ; mov eax,0xAA ; mov eax,0xBB
        let mut xb = harness(&[
            0xEB, 0x05, // jmp over the 5-byte mov
            0xB8, 0xAA, 0x00, 0x00, 0x00, // mov eax,0xAA (skipped)
            0xB8, 0xBB, 0x00, 0x00, 0x00, // mov eax,0xBB
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0xBB);
    }

    #[test]
    fn conditional_branch_taken_on_zero() {
        // mov eax,0 ; test eax,eax ; jz +5 ; mov eax,1 ; (target) hlt
        let mut xb = harness(&[
            0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax,0
            0x85, 0xC0, // test eax,eax
            0x74, 0x05, // jz +5
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1 (skipped)
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 0, "jz taken, mov skipped");
    }

    #[test]
    fn call_and_ret_near() {
        // call +5 (to ret) ; (filler) hlt ; ret
        // layout: E8 disp32 (5 bytes) -> target at +5 which is the RET.
        let mut xb = harness(&[
            0xE8, 0x00, 0x00, 0x00, 0x00, // call +0 -> next instr (the ret)
            0xC3, // ret
        ]);
        let sp0 = xb.cpu.reg32(ESP);
        run(&mut xb, 2); // call, then ret
        assert_eq!(xb.cpu.reg32(ESP), sp0, "stack balanced after call/ret");
    }

    #[test]
    fn shift_left_sets_carry() {
        // mov eax,0x80000000 ; shl eax,1  -> 0, CF=1
        let mut xb = harness(&[0xB8, 0x00, 0x00, 0x00, 0x80, 0xD1, 0xE0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(CF));
    }

    #[test]
    fn unsigned_mul_and_div() {
        // mov eax,200 ; mov ebx,3 ; mul ebx (EAX*EBX) ; mov ebx,7 ; div ebx
        let mut xb = harness(&[
            0xB8, 0xC8, 0x00, 0x00, 0x00, // mov eax,200
            0xBB, 0x03, 0x00, 0x00, 0x00, // mov ebx,3
            0xF7, 0xE3, // mul ebx -> EDX:EAX = 600
            0xBB, 0x07, 0x00, 0x00, 0x00, // mov ebx,7
            0xF7, 0xF3, // div ebx -> 600/7
        ]);
        run(&mut xb, 5);
        assert_eq!(xb.cpu.reg32(EAX), 600 / 7);
        assert_eq!(xb.cpu.reg32(EDX), 600 % 7);
    }

    #[test]
    fn divide_by_zero_faults() {
        // mov eax,1 ; xor edx,edx ; xor ebx,ebx ; div ebx -> #DE
        let mut xb = harness(&[
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
            0x31, 0xD2, // xor edx,edx
            0x31, 0xDB, // xor ebx,ebx
            0xF7, 0xF3, // div ebx
        ]);
        run(&mut xb, 4);
        assert_eq!(xb.cpu.fault.unwrap().vector, 0, "#DE raised");
    }

    #[test]
    fn unimplemented_opcode_raises_ud() {
        // 0x82 is an invalid opcode in the modern ISA.
        let mut xb = harness(&[0x82]);
        run(&mut xb, 1);
        let f = xb.cpu.fault.unwrap();
        assert_eq!(f.vector, 6, "#UD");
        assert_eq!(f.opcode, 0x82);
    }

    #[test]
    fn mov_to_cr0_enables_protected_mode_bit() {
        // We start in flat protected mode already; verify mov CR0 round-trips.
        // mov eax, cr0 ; mov cr0, eax
        let mut xb = harness(&[0x0F, 0x20, 0xC0, 0x0F, 0x22, 0xC0]);
        run(&mut xb, 2);
        assert!(xb.cpu.cr[0] & CR0_PE != 0);
    }

    #[test]
    fn movzx_zero_extends_byte() {
        // mov eax,0xFFFFFFFF ; movzx ebx, al  -> 0xFF
        let mut xb = harness(&[
            0xB8, 0xFF, 0xFF, 0xFF, 0xFF, // mov eax,-1
            0x0F, 0xB6, 0xD8, // movzx ebx, al
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EBX), 0xFF);
    }

    #[test]
    fn memory_store_load_via_modrm() {
        // mov eax,0x12345678 ; mov [0x1000], eax ; mov ebx,[0x1000]
        let mut xb = harness(&[
            0xB8, 0x78, 0x56, 0x34, 0x12, // mov eax,0x12345678
            0xA3, 0x00, 0x10, 0x00, 0x00, // mov [0x1000], eax
            0x8B, 0x1D, 0x00, 0x10, 0x00, 0x00, // mov ebx, [0x1000]
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 0x1234_5678);
        // Verify little-endian byte order in RAM.
        assert_eq!(xb.mem.ram_read8(0x1000), 0x78);
        assert_eq!(xb.mem.ram_read8(0x1003), 0x12);
    }
}
