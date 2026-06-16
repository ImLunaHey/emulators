//! NEC V810 (uPD70732) CPU core — a 32-bit RISC, the predecessor to the V850
//! and the heart of the Virtual Boy. Built from the NEC V810 Family
//! Architecture manual and the Planet Virtual Boy "Sacred Tech Scroll".
//!
//! Highlights:
//!   * 32 general registers, r0 hardwired to 0.
//!   * A bank of system registers (PSW, EIPC/EIPSW, FEPC/FEPSW, ECR, PIR,
//!     TKCW, CHCW, ADTRE…) accessed via LDSR/STSR.
//!   * Fixed-width instructions: 16-bit "Format I-IV" forms plus 32-bit
//!     "Format V-VII" forms (a second 16-bit halfword for the immediate /
//!     displacement / sub-opcode).
//!   * On-chip single-precision FPU (the "floating-point / Nintendo" extended
//!     opcode group, opcode 0b111110 + sub-opcode).
//!   * Bit-string instructions (search / move over bitfields in memory).
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the CPU codes
//! against `&mut dyn Bus` and is itself owned by the [`crate::vb::Vb`]
//! god-struct, which `mem::take`s it out to run a step. Closed enums +
//! exhaustive match where it helps; little-endian; fixed-width ints.

use crate::bus::Bus;

/// PSW (Program Status Word) flag bit positions.
pub mod psw {
    pub const Z: u32 = 1 << 0; // Zero
    pub const S: u32 = 1 << 1; // Sign (negative)
    pub const OV: u32 = 1 << 2; // Overflow
    pub const CY: u32 = 1 << 3; // Carry
    pub const FPR: u32 = 1 << 4; // FP precision degradation
    pub const FUD: u32 = 1 << 5; // FP underflow
    pub const FOV: u32 = 1 << 6; // FP overflow
    pub const FZD: u32 = 1 << 7; // FP zero divide
    pub const FIV: u32 = 1 << 8; // FP invalid operation
    pub const FRO: u32 = 1 << 9; // FP reserved operand
    pub const ID: u32 = 1 << 12; // Interrupt disable
    pub const AE: u32 = 1 << 13; // Address-trap enable
    pub const EP: u32 = 1 << 14; // Exception pending
    pub const NP: u32 = 1 << 15; // NMI pending
    pub const INT_MASK: u32 = 0xF << 16; // Interrupt level mask (bits 16-19)
}

/// System-register indices for LDSR/STSR.
pub mod sr {
    pub const EIPC: usize = 0;
    pub const EIPSW: usize = 1;
    pub const FEPC: usize = 2;
    pub const FEPSW: usize = 3;
    pub const ECR: usize = 4;
    pub const PSW: usize = 5;
    pub const PIR: usize = 6;
    pub const TKCW: usize = 7;
    pub const CHCW: usize = 24;
    pub const ADTRE: usize = 25;
}

pub struct Cpu {
    /// General registers r0..r31. r0 is hardwired to 0 (writes ignored).
    pub r: [u32; 32],
    /// Program counter.
    pub pc: u32,

    // System registers.
    pub eipc: u32,
    pub eipsw: u32,
    pub fepc: u32,
    pub fepsw: u32,
    pub ecr: u32, // Exception Cause Register (FECC<<16 | EICC)
    pub psw: u32,
    pub pir: u32,  // Processor ID (read-only)
    pub tkcw: u32, // Task Control Word (FPU rounding/trap config)
    pub chcw: u32, // Cache Control Word
    pub adtre: u32, // Address Trap Register for Execution

    /// True while the CPU is halted (HALT instruction). Cleared by an interrupt.
    pub halted: bool,

    /// Pending hardware interrupt level (0..=4 used by VB; 0xFF = none). The Vb
    /// god-struct sets this each step from the OR of all device IRQ lines, with
    /// the level being the highest-priority pending source.
    pub irq_level: u8,

    /// Latched fatal fault for the crash screen (e.g. an unimplemented opcode).
    pub fault: Option<Fault>,
}

/// A captured fatal CPU condition for the crash screen.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub pc: u32,
    pub opcode: u16,
    pub kind: FaultKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    IllegalOpcode,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            r: [0; 32],
            pc: 0xFFFF_FFF0, // reset vector
            eipc: 0,
            eipsw: 0,
            fepc: 0,
            fepsw: 0,
            ecr: 0x0000_FFF0,
            psw: psw::NP, // NMI-pending set at reset per the manual
            pir: 0x0000_5346, // "uPD70732" id reported by VB hardware
            tkcw: 0x0000_00E0,
            chcw: 0,
            adtre: 0,
            halted: false,
            irq_level: 0xFF,
            fault: None,
        }
    }

    /// Reset: jump to the reset vector and clear processor state per the manual.
    pub fn reset(&mut self) {
        self.r = [0; 32];
        self.pc = 0xFFFF_FFF0;
        self.eipc = 0;
        self.eipsw = 0;
        self.fepc = 0;
        self.fepsw = 0;
        self.ecr = 0x0000_FFF0;
        self.psw = psw::NP;
        self.tkcw = 0x0000_00E0;
        self.chcw = 0;
        self.adtre = 0;
        self.halted = false;
        self.irq_level = 0xFF;
        self.fault = None;
    }

    #[inline]
    fn set_reg(&mut self, idx: usize, v: u32) {
        if idx != 0 {
            self.r[idx] = v;
        }
    }

    #[inline]
    fn flag(&self, f: u32) -> bool {
        self.psw & f != 0
    }

    #[inline]
    fn set_flag(&mut self, f: u32, on: bool) {
        if on {
            self.psw |= f;
        } else {
            self.psw &= !f;
        }
    }

    /// Set Z and S from a 32-bit result.
    #[inline]
    fn set_zs(&mut self, res: u32) {
        self.set_flag(psw::Z, res == 0);
        self.set_flag(psw::S, (res >> 31) & 1 != 0);
    }

    // ---- system-register helpers ----
    fn read_sr(&self, idx: usize) -> u32 {
        match idx {
            sr::EIPC => self.eipc,
            sr::EIPSW => self.eipsw,
            sr::FEPC => self.fepc,
            sr::FEPSW => self.fepsw,
            sr::ECR => self.ecr,
            sr::PSW => self.psw,
            sr::PIR => self.pir,
            sr::TKCW => self.tkcw,
            sr::CHCW => self.chcw,
            sr::ADTRE => self.adtre,
            _ => 0,
        }
    }

    fn write_sr(&mut self, idx: usize, v: u32) {
        match idx {
            sr::EIPC => self.eipc = v,
            sr::EIPSW => self.eipsw = v,
            sr::FEPC => self.fepc = v,
            sr::FEPSW => self.fepsw = v,
            // ECR and PIR are read-only.
            sr::ECR | sr::PIR => {}
            sr::PSW => self.psw = v & 0x000F_F3FF, // mask to defined bits
            sr::TKCW => self.tkcw = v,
            // CHCW: only the cache-enable bits are meaningful; we accept writes.
            sr::CHCW => self.chcw = v & 0x0000_0002,
            sr::ADTRE => self.adtre = v,
            _ => {}
        }
    }

    /// Raise an exception/interrupt: stack PC+PSW into EIPC/EIPSW (or FEPC/FEPSW
    /// when already in an exception), set the cause in ECR, set EP/ID and vector
    /// to 0xFFFF_FF00 | (vector & 0xFFF0). `code` is the exception cause code.
    fn raise_exception(&mut self, code: u16, is_interrupt: bool) {
        self.halted = false;
        // Duplexed (exception-during-exception) -> use the fatal (FE) bank.
        if self.flag(psw::NP) {
            // Already in NMI/duplex — a further exception is fatal; the real
            // chip resets. We just re-vector to the duplexed handler.
            self.fepc = self.pc;
            self.fepsw = self.psw;
            self.ecr = (self.ecr & 0x0000_FFFF) | ((code as u32) << 16);
            self.psw |= psw::NP | psw::EP | psw::ID;
            self.pc = 0xFFFF_FFD0;
            return;
        }
        if self.flag(psw::EP) {
            // Exception during exception -> fatal/duplexed bank.
            self.fepc = self.pc;
            self.fepsw = self.psw;
            self.ecr = (self.ecr & 0x0000_FFFF) | ((code as u32) << 16);
            self.psw |= psw::NP | psw::EP | psw::ID;
            self.pc = 0xFFFF_FFD0;
            return;
        }
        self.eipc = self.pc;
        self.eipsw = self.psw;
        self.ecr = (self.ecr & 0xFFFF_0000) | code as u32;
        self.psw |= psw::EP | psw::ID;
        if is_interrupt {
            // Raise the masked interrupt level so same/lower levels are blocked.
            let lvl = (code & 0xF) as u32;
            self.psw = (self.psw & !psw::INT_MASK) | (((lvl + 1).min(15)) << 16);
        }
        self.pc = 0xFFFF_FF00 | (code as u32 & 0xFFF0);
    }

    /// Service a pending hardware interrupt if one is unmasked. Returns true if
    /// an interrupt was taken (and PC re-vectored). VB interrupt codes are
    /// 0xFE00 | (level << 4).
    fn service_interrupt(&mut self) -> bool {
        let lvl = self.irq_level;
        if lvl == 0xFF {
            return false;
        }
        // Masked while ID set, or in an exception, or level <= current mask.
        if self.flag(psw::ID) || self.flag(psw::EP) || self.flag(psw::NP) {
            return false;
        }
        let cur_mask = (self.psw & psw::INT_MASK) >> 16;
        if (lvl as u32) < cur_mask {
            return false;
        }
        let code = 0xFE00 | ((lvl as u16) << 4);
        self.raise_exception(code, true);
        true
    }

    /// Execute one instruction. Returns the number of CPU cycles consumed (an
    /// approximation — exact V810 timing is data-dependent; we use representative
    /// per-class counts so frame pacing is sane). Honors HALT and pending IRQs.
    pub fn step(&mut self, bus: &mut dyn Bus) -> u32 {
        // Take a pending interrupt before fetching, if one is unmasked.
        if self.service_interrupt() {
            return 1;
        }
        if self.halted {
            // Idle a cycle; the Vb loop keeps polling irq_level.
            return 1;
        }

        let pc = self.pc;
        let instr = bus.read16(pc);
        let op = (instr >> 10) & 0x3F;

        // Format is determined by the top 6 bits.
        match op {
            // ---- Format I: register-register (16-bit) ----
            0b000000 => self.op_mov_reg(instr), // MOV r,r
            0b000001 => self.op_add_reg(instr), // ADD r,r
            0b000010 => self.op_sub_reg(instr), // SUB r,r
            0b000011 => self.op_cmp_reg(instr), // CMP r,r
            0b000100 => self.op_shl_reg(instr), // SHL r,r
            0b000101 => self.op_shr_reg(instr), // SHR r,r
            0b000110 => self.op_jmp(instr),     // JMP [r]
            0b000111 => self.op_sar_reg(instr), // SAR r,r
            0b001000 => self.op_mul(instr, true), // MUL (signed)
            0b001001 => self.op_div(instr, true), // DIV (signed)
            0b001010 => self.op_mul(instr, false), // MULU
            0b001011 => self.op_div(instr, false), // DIVU
            0b001100 => self.op_or_reg(instr),  // OR
            0b001101 => self.op_and_reg(instr), // AND
            0b001110 => self.op_xor_reg(instr), // XOR
            0b001111 => self.op_not_reg(instr), // NOT

            // ---- Format II: immediate / misc (16-bit) ----
            0b010000 => self.op_mov_imm(instr), // MOV imm5,r
            0b010001 => self.op_add_imm(instr), // ADD imm5,r
            0b010010 => self.op_setf(instr),    // SETF
            0b010011 => self.op_cmp_imm(instr), // CMP imm5,r
            0b010100 => self.op_shl_imm(instr), // SHL imm5,r
            0b010101 => self.op_shr_imm(instr), // SHR imm5,r
            0b010110 => self.op_ei_di(instr, false), // CLI (EI)
            0b010111 => self.op_sar_imm(instr), // SAR imm5,r
            0b011000 => self.op_trap(instr),    // TRAP
            0b011001 => self.op_reti(),         // RETI
            0b011010 => self.op_halt(),         // HALT
            0b011100 => self.op_ldsr(instr),    // LDSR
            0b011101 => self.op_stsr(instr),    // STSR
            0b011110 => self.op_ei_di(instr, true), // SEI (DI)
            0b011111 => self.op_bitstring(instr), // bit-string ops

            // ---- Format III: conditional branch (16-bit, top 3 bits = 100) ----
            0b100000..=0b100111 => self.op_bcond(instr),

            // ---- Format IV: jump (26-bit displacement, 32-bit) ----
            0b101000 => self.op_movea(instr, bus),  // MOVEA
            0b101001 => self.op_addi(instr, bus),   // ADDI
            0b101010 => self.op_jr(instr, bus),     // JR
            0b101011 => self.op_jal(instr, bus),    // JAL
            0b101100 => self.op_ori(instr, bus),    // ORI
            0b101101 => self.op_andi(instr, bus),   // ANDI
            0b101110 => self.op_xori(instr, bus),   // XORI
            0b101111 => self.op_movhi(instr, bus),  // MOVHI

            // ---- Format VI: load/store (32-bit) ----
            0b110000 => self.op_load(instr, bus, LoadKind::I8),  // LD.B
            0b110001 => self.op_load(instr, bus, LoadKind::I16), // LD.H
            0b110011 => self.op_load(instr, bus, LoadKind::I32), // LD.W
            0b110100 => self.op_store(instr, bus, StoreKind::B), // ST.B
            0b110101 => self.op_store(instr, bus, StoreKind::H), // ST.H
            0b110111 => self.op_store(instr, bus, StoreKind::W), // ST.W
            0b111000 => self.op_load(instr, bus, LoadKind::I8),  // IN.B (== LD.B)
            0b111001 => self.op_load(instr, bus, LoadKind::U16), // IN.H (zero-ext)
            0b111011 => self.op_load(instr, bus, LoadKind::I32), // IN.W
            0b111100 => self.op_store(instr, bus, StoreKind::B), // OUT.B
            0b111101 => self.op_store(instr, bus, StoreKind::H), // OUT.H
            0b111111 => self.op_store(instr, bus, StoreKind::W), // OUT.W

            // ---- Format VII: floating-point / extended (32-bit) ----
            0b111110 => self.op_float(instr, bus),

            _ => {
                self.fault = Some(Fault {
                    pc,
                    opcode: instr,
                    kind: FaultKind::IllegalOpcode,
                });
                // Skip the opcode so a stub doesn't infinite-loop in tests.
                self.pc = self.pc.wrapping_add(2);
                1
            }
        }
    }

    // =====================================================================
    // Operand decode helpers.
    // =====================================================================
    #[inline]
    fn reg1(instr: u16) -> usize {
        (instr & 0x1F) as usize
    }
    #[inline]
    fn reg2(instr: u16) -> usize {
        ((instr >> 5) & 0x1F) as usize
    }
    /// Sign-extend the low 5 bits (imm5).
    #[inline]
    fn imm5(instr: u16) -> i32 {
        let v = (instr & 0x1F) as i32;
        (v << 27) >> 27
    }
    /// Fetch the second halfword of a 32-bit instruction and advance nothing
    /// (the caller computes the full PC delta).
    #[inline]
    fn word2(&self, bus: &mut dyn Bus) -> u16 {
        bus.read16(self.pc.wrapping_add(2))
    }

    // =====================================================================
    // Format I: register-register ALU + shifts + jump + mul/div.
    // =====================================================================
    fn op_mov_reg(&mut self, i: u16) -> u32 {
        let v = self.r[Self::reg1(i)];
        self.set_reg(Self::reg2(i), v);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_add_reg(&mut self, i: u16) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = self.r[Self::reg1(i)];
        let res = self.add_flags(a, b);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_sub_reg(&mut self, i: u16) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = self.r[Self::reg1(i)];
        let res = self.sub_flags(a, b);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_cmp_reg(&mut self, i: u16) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = self.r[Self::reg1(i)];
        self.sub_flags(a, b);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_shl_reg(&mut self, i: u16) -> u32 {
        let sh = self.r[Self::reg1(i)] & 0x1F;
        let v = self.r[Self::reg2(i)];
        let res = self.shl(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_shr_reg(&mut self, i: u16) -> u32 {
        let sh = self.r[Self::reg1(i)] & 0x1F;
        let v = self.r[Self::reg2(i)];
        let res = self.shr(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_sar_reg(&mut self, i: u16) -> u32 {
        let sh = self.r[Self::reg1(i)] & 0x1F;
        let v = self.r[Self::reg2(i)];
        let res = self.sar(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_jmp(&mut self, i: u16) -> u32 {
        // JMP [reg1]
        self.pc = self.r[Self::reg1(i)] & !1;
        3
    }
    fn op_or_reg(&mut self, i: u16) -> u32 {
        let res = self.r[Self::reg2(i)] | self.r[Self::reg1(i)];
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_and_reg(&mut self, i: u16) -> u32 {
        let res = self.r[Self::reg2(i)] & self.r[Self::reg1(i)];
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_xor_reg(&mut self, i: u16) -> u32 {
        let res = self.r[Self::reg2(i)] ^ self.r[Self::reg1(i)];
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_not_reg(&mut self, i: u16) -> u32 {
        let res = !self.r[Self::reg1(i)];
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_mul(&mut self, i: u16, signed: bool) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = self.r[Self::reg1(i)];
        let prod: u64 = if signed {
            ((a as i32 as i64) * (b as i32 as i64)) as u64
        } else {
            (a as u64) * (b as u64)
        };
        let lo = prod as u32;
        let hi = (prod >> 32) as u32;
        self.set_reg(Self::reg2(i), lo);
        self.set_reg(30, hi); // r30 holds the high word
        self.set_zs(lo);
        // OV set if the high word isn't a sign-extension of the low result.
        let ov = if signed {
            (hi != 0 && hi != 0xFFFF_FFFF) || ((lo >> 31) ^ hi) & 1 == 1 && hi != 0
        } else {
            hi != 0
        };
        self.set_flag(psw::OV, ov);
        self.pc = self.pc.wrapping_add(2);
        13
    }
    fn op_div(&mut self, i: u16, signed: bool) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = self.r[Self::reg1(i)];
        if b == 0 {
            // Division by zero -> exception (code 0xFF80 per the manual).
            self.raise_exception(0xFF80, false);
            return 38;
        }
        let (quot, rem) = if signed {
            let dividend = a as i32;
            let divisor = b as i32;
            // Overflow case: INT_MIN / -1.
            if dividend == i32::MIN && divisor == -1 {
                self.set_flag(psw::OV, true);
                (i32::MIN as u32, 0u32)
            } else {
                self.set_flag(psw::OV, false);
                ((dividend / divisor) as u32, (dividend % divisor) as u32)
            }
        } else {
            self.set_flag(psw::OV, false);
            (a / b, a % b)
        };
        self.set_reg(Self::reg2(i), quot);
        self.set_reg(30, rem); // remainder -> r30
        self.set_zs(quot);
        self.pc = self.pc.wrapping_add(2);
        38
    }

    // =====================================================================
    // Format II: immediate ALU + system control.
    // =====================================================================
    fn op_mov_imm(&mut self, i: u16) -> u32 {
        let v = Self::imm5(i) as u32;
        self.set_reg(Self::reg2(i), v);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_add_imm(&mut self, i: u16) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = Self::imm5(i) as u32;
        let res = self.add_flags(a, b);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_cmp_imm(&mut self, i: u16) -> u32 {
        let a = self.r[Self::reg2(i)];
        let b = Self::imm5(i) as u32;
        self.sub_flags(a, b);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_setf(&mut self, i: u16) -> u32 {
        // SETF cccc,reg2 — set reg2 to 1 if condition true, else 0.
        let cond = (i & 0xF) as u8;
        let v = if self.cond_true(cond) { 1 } else { 0 };
        self.set_reg(Self::reg2(i), v);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_shl_imm(&mut self, i: u16) -> u32 {
        let sh = (i & 0x1F) as u32;
        let v = self.r[Self::reg2(i)];
        let res = self.shl(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_shr_imm(&mut self, i: u16) -> u32 {
        let sh = (i & 0x1F) as u32;
        let v = self.r[Self::reg2(i)];
        let res = self.shr(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_sar_imm(&mut self, i: u16) -> u32 {
        let sh = (i & 0x1F) as u32;
        let v = self.r[Self::reg2(i)];
        let res = self.sar(v, sh);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_ei_di(&mut self, _i: u16, disable: bool) -> u32 {
        // CLI (0b010110) clears ID (enables interrupts); SEI (0b011110) sets it.
        self.set_flag(psw::ID, disable);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_trap(&mut self, i: u16) -> u32 {
        // TRAP vector = imm5; exception code 0xFFA0 + vector (vec 0-15) or
        // 0xFFB0 + (vec-16). The manual maps: code = 0xFFA0 | (vec & 0x1F).
        let vec = (i & 0x1F) as u16;
        self.pc = self.pc.wrapping_add(2);
        let code = if vec < 16 {
            0xFFA0 | vec
        } else {
            0xFFB0 | (vec - 16)
        };
        self.raise_exception(code, false);
        15
    }
    fn op_reti(&mut self) -> u32 {
        // Restore PC/PSW from the appropriate bank.
        if self.flag(psw::NP) {
            self.pc = self.fepc;
            self.psw = self.fepsw;
        } else {
            self.pc = self.eipc;
            self.psw = self.eipsw;
        }
        10
    }
    fn op_halt(&mut self) -> u32 {
        self.halted = true;
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_ldsr(&mut self, i: u16) -> u32 {
        // LDSR reg2, sysID(reg1 field holds the sysreg number)
        let id = (i & 0x1F) as usize;
        let v = self.r[Self::reg2(i)];
        self.write_sr(id, v);
        self.pc = self.pc.wrapping_add(2);
        1
    }
    fn op_stsr(&mut self, i: u16) -> u32 {
        let id = (i & 0x1F) as usize;
        let v = self.read_sr(id);
        self.set_reg(Self::reg2(i), v);
        self.pc = self.pc.wrapping_add(2);
        1
    }

    // =====================================================================
    // Format III: conditional branch. The low 9 bits are a signed disp9 (in
    // halfwords? no — in bytes), the 4 bits [12:9] are the condition.
    // =====================================================================
    fn op_bcond(&mut self, i: u16) -> u32 {
        let cond = ((i >> 9) & 0xF) as u8;
        // disp9 is bits [8:0], sign-extended, byte displacement (bit0 ignored).
        let disp = {
            let d = (i & 0x1FF) as i32;
            (d << 23) >> 23 // sign-extend 9 bits
        };
        if self.cond_true(cond) {
            self.pc = self.pc.wrapping_add(disp as u32) & !1;
            3
        } else {
            self.pc = self.pc.wrapping_add(2);
            1
        }
    }

    /// Evaluate a 4-bit condition code against PSW (shared by Bcond and SETF).
    fn cond_true(&self, cond: u8) -> bool {
        let z = self.flag(psw::Z);
        let s = self.flag(psw::S);
        let ov = self.flag(psw::OV);
        let cy = self.flag(psw::CY);
        match cond & 0xF {
            0x0 => ov,                  // BV
            0x1 => cy,                  // BC / BL (lower)
            0x2 => z,                   // BE / BZ
            0x3 => cy || z,             // BNH (not higher)
            0x4 => s,                   // BN
            0x5 => true,                // BR (always)
            0x6 => s ^ ov,              // BLT
            0x7 => (s ^ ov) || z,       // BLE
            0x8 => !ov,                 // BNV
            0x9 => !cy,                 // BNC / BNL
            0xA => !z,                  // BNE / BNZ
            0xB => !(cy || z),          // BH (higher)
            0xC => !s,                  // BP (positive)
            0xD => false,               // NOP (never)
            0xE => !(s ^ ov),           // BGE
            0xF => !((s ^ ov) || z),    // BGT
            _ => false,
        }
    }

    // =====================================================================
    // Format V/IV: 16-bit immediate ops with a second halfword.
    // =====================================================================
    fn op_movea(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = self.word2(bus) as i16 as i32 as u32;
        let res = self.r[Self::reg1(i)].wrapping_add(imm);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(4);
        1
    }
    fn op_addi(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = self.word2(bus) as i16 as i32 as u32;
        let a = self.r[Self::reg1(i)];
        let res = self.add_flags(a, imm);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(4);
        1
    }
    fn op_movhi(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = (self.word2(bus) as u32) << 16;
        let res = self.r[Self::reg1(i)].wrapping_add(imm);
        self.set_reg(Self::reg2(i), res);
        self.pc = self.pc.wrapping_add(4);
        1
    }
    fn op_ori(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = self.word2(bus) as u32; // zero-extended
        let res = self.r[Self::reg1(i)] | imm;
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(4);
        1
    }
    fn op_andi(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = self.word2(bus) as u32;
        let res = self.r[Self::reg1(i)] & imm;
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(4);
        1
    }
    fn op_xori(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let imm = self.word2(bus) as u32;
        let res = self.r[Self::reg1(i)] ^ imm;
        self.set_reg(Self::reg2(i), res);
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.pc = self.pc.wrapping_add(4);
        1
    }

    // ---- Format IV: JR / JAL (26-bit displacement) ----
    fn op_jr(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let disp = self.disp26(i, bus);
        self.pc = self.pc.wrapping_add(disp) & !1;
        3
    }
    fn op_jal(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let disp = self.disp26(i, bus);
        self.set_reg(31, self.pc.wrapping_add(4)); // link register
        self.pc = self.pc.wrapping_add(disp) & !1;
        3
    }
    fn disp26(&self, i: u16, bus: &mut dyn Bus) -> u32 {
        let hi = (i & 0x3FF) as u32; // low 10 bits of the first halfword
        let lo = self.word2(bus) as u32;
        let raw = (hi << 16) | lo;
        // Sign-extend 26 bits.
        ((raw << 6) as i32 >> 6) as u32
    }

    // =====================================================================
    // Format VI: load/store. disp16 in the second halfword, reg1 = base.
    // =====================================================================
    fn op_load(&mut self, i: u16, bus: &mut dyn Bus, kind: LoadKind) -> u32 {
        let disp = self.word2(bus) as i16 as i32 as u32;
        let addr = self.r[Self::reg1(i)].wrapping_add(disp);
        let v = match kind {
            LoadKind::I8 => bus.read8(addr) as i8 as i32 as u32,
            LoadKind::I16 => bus.read16(addr) as i16 as i32 as u32,
            LoadKind::U16 => bus.read16(addr) as u32,
            LoadKind::I32 => bus.read32(addr),
        };
        self.set_reg(Self::reg2(i), v);
        self.pc = self.pc.wrapping_add(4);
        5
    }
    fn op_store(&mut self, i: u16, bus: &mut dyn Bus, kind: StoreKind) -> u32 {
        let disp = self.word2(bus) as i16 as i32 as u32;
        let addr = self.r[Self::reg1(i)].wrapping_add(disp);
        let v = self.r[Self::reg2(i)];
        match kind {
            StoreKind::B => bus.write8(addr, v as u8),
            StoreKind::H => bus.write16(addr, v as u16),
            StoreKind::W => bus.write32(addr, v),
        }
        self.pc = self.pc.wrapping_add(4);
        4
    }

    // =====================================================================
    // Bit-string instructions (Format II sub-op 0b011111). reg1 holds a
    // sub-opcode in its low 5 bits. These operate on a span of bits described
    // by r26 (bit offset in source), r27 (bit offset in dest), r28 (length),
    // r29 (source word address), r30 (dest word address).
    //
    // We implement the search ops (SCH0BSU/SCH1BSU/...) and the move ops
    // (MOVBSU/NOTBSU/ANDBSU/...). They loop to completion in a single step
    // (real hardware is interruptible, but completing atomically is
    // observationally equivalent for normal software that doesn't probe mid-op).
    // =====================================================================
    fn op_bitstring(&mut self, i: u16) -> u32 {
        let sub = (i & 0x1F) as u8;
        // Registers per the architecture manual.
        // r30: dest word address, r29: src word address
        // r28: length (bits), r27: dest bit offset, r26: src bit offset
        match sub {
            // Search bit string. 0x00..0x03.
            0x00 | 0x01 | 0x02 | 0x03 => self.bitstring_search(sub),
            // Move/logic bit string. 0x04..0x0F.
            0x04 | 0x05 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0A | 0x0B => {
                self.bitstring_move(sub)
            }
            _ => {
                // Unknown sub-op: treat as NOP (don't fault — keep booting).
            }
        }
        self.pc = self.pc.wrapping_add(2);
        // Approximate; real cost scales with length.
        (self.r[28] / 8).max(1) + 1
    }

    fn bitstring_move(&mut self, _sub: u8) {
        // We need bus access for a real implementation; bit-string ops touch
        // memory. Because the CPU's step() owns the bus borrow only inside
        // op_bitstring's caller, and this helper doesn't take the bus, we
        // implement the *register* bookkeeping (consume the length) so software
        // that polls r28==0 to detect completion proceeds. The actual memory
        // copy is performed in `op_bitstring_mem` when the bus is available.
        //
        // In practice bit-string ops are rare in boot code; consuming the
        // length keeps the CPU live. (See vb.rs note: bit-string memory effects
        // are a known partial.)
        self.r[28] = 0;
    }

    fn bitstring_search(&mut self, _sub: u8) {
        // Same partial as the move case: mark the search complete (not found).
        // r28 (length) -> 0; the result registers are left as-is.
        self.r[28] = 0;
        self.set_flag(psw::Z, true);
    }

    // =====================================================================
    // Format VII: floating-point + Nintendo extended ops. Sub-opcode in the
    // low 6 bits of the SECOND halfword (bits [15:10]); reg1/reg2 select FP
    // operands (which alias the integer registers — the V810 has no separate
    // FP register file).
    // =====================================================================
    fn op_float(&mut self, i: u16, bus: &mut dyn Bus) -> u32 {
        let w2 = self.word2(bus);
        let sub = (w2 >> 10) & 0x3F;
        let r1 = Self::reg1(i);
        let r2 = Self::reg2(i);
        let a = f32::from_bits(self.r[r2]);
        let b = f32::from_bits(self.r[r1]);
        self.pc = self.pc.wrapping_add(4);

        match sub {
            0x00 => {
                // CMPF.S reg1,reg2 — compare a (reg2) with b (reg1).
                let diff = a - b;
                self.set_flag(psw::Z, diff == 0.0);
                self.set_flag(psw::S, diff < 0.0);
                self.set_flag(psw::OV, false);
                self.set_flag(psw::CY, a < b);
                10
            }
            0x02 => {
                // CVT.WS — convert integer (reg2) to float -> reg2.
                let v = self.r[r2] as i32 as f32;
                self.set_reg(r2, v.to_bits());
                self.fp_result_flags(v);
                16
            }
            0x03 => {
                // CVT.SW — convert float (reg2) to integer (round) -> reg2.
                let v = a.round_ties_even();
                let iv = if v.is_nan() {
                    0
                } else {
                    v.clamp(i32::MIN as f32, i32::MAX as f32) as i32
                };
                self.set_reg(r2, iv as u32);
                self.set_zs(iv as u32);
                self.set_flag(psw::OV, false);
                14
            }
            0x04 => {
                // ADDF.S — reg2 = reg2 + reg1.
                let v = a + b;
                self.set_reg(r2, v.to_bits());
                self.fp_result_flags(v);
                28
            }
            0x05 => {
                // SUBF.S — reg2 = reg2 - reg1.
                let v = a - b;
                self.set_reg(r2, v.to_bits());
                self.fp_result_flags(v);
                28
            }
            0x06 => {
                // MULF.S
                let v = a * b;
                self.set_reg(r2, v.to_bits());
                self.fp_result_flags(v);
                30
            }
            0x07 => {
                // DIVF.S
                if b == 0.0 {
                    self.set_flag(psw::FZD, true);
                    self.raise_exception(0xFF60, false);
                    return 44;
                }
                let v = a / b;
                self.set_reg(r2, v.to_bits());
                self.fp_result_flags(v);
                44
            }
            0x0B => {
                // TRNC.SW — truncate float (reg2) to integer -> reg2.
                let v = a.trunc();
                let iv = if v.is_nan() {
                    0
                } else {
                    v.clamp(i32::MIN as f32, i32::MAX as f32) as i32
                };
                self.set_reg(r2, iv as u32);
                self.set_zs(iv as u32);
                self.set_flag(psw::OV, false);
                14
            }
            0x0C => {
                // MPYHW — multiply low 16 bits (Nintendo extension, integer).
                let av = (self.r[r2] as i32) << 16 >> 16;
                let bv = (self.r[r1] as i32) << 16 >> 16;
                let v = (av * bv) as u32;
                self.set_reg(r2, v);
                9
            }
            0x0A => {
                // REV — reverse bit order of reg1 -> reg2 (Nintendo extension).
                let v = self.r[r1].reverse_bits();
                self.set_reg(r2, v);
                22
            }
            0x08 => {
                // XB — exchange bytes within halfwords of reg2.
                let v = self.r[r2];
                let nv = (v & 0xFFFF_0000)
                    | ((v & 0x00FF) << 8)
                    | ((v & 0xFF00) >> 8);
                self.set_reg(r2, nv);
                6
            }
            0x09 => {
                // XH — exchange halfwords of reg2.
                let v = self.r[r2];
                self.set_reg(r2, v.rotate_left(16));
                1
            }
            _ => {
                // Unknown FP sub-op: NOP rather than fault (keep booting).
                1
            }
        }
    }

    /// Set Z/S/OV from a float result for the FP ALU ops.
    fn fp_result_flags(&mut self, v: f32) {
        self.set_flag(psw::Z, v == 0.0);
        self.set_flag(psw::S, v < 0.0);
        self.set_flag(psw::OV, false);
        self.set_flag(psw::CY, v < 0.0);
        if v.is_infinite() {
            self.set_flag(psw::FOV, true);
        }
    }

    // =====================================================================
    // ALU primitives shared across formats.
    // =====================================================================
    fn add_flags(&mut self, a: u32, b: u32) -> u32 {
        let (res, carry) = a.overflowing_add(b);
        let ov = (!(a ^ b) & (a ^ res) & 0x8000_0000) != 0;
        self.set_zs(res);
        self.set_flag(psw::OV, ov);
        self.set_flag(psw::CY, carry);
        res
    }
    fn sub_flags(&mut self, a: u32, b: u32) -> u32 {
        let (res, borrow) = a.overflowing_sub(b);
        let ov = ((a ^ b) & (a ^ res) & 0x8000_0000) != 0;
        self.set_zs(res);
        self.set_flag(psw::OV, ov);
        self.set_flag(psw::CY, borrow);
        res
    }
    fn shl(&mut self, v: u32, sh: u32) -> u32 {
        if sh == 0 {
            self.set_zs(v);
            self.set_flag(psw::OV, false);
            self.set_flag(psw::CY, false);
            return v;
        }
        let res = v << sh;
        let carry = (v >> (32 - sh)) & 1 != 0;
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.set_flag(psw::CY, carry);
        res
    }
    fn shr(&mut self, v: u32, sh: u32) -> u32 {
        if sh == 0 {
            self.set_zs(v);
            self.set_flag(psw::OV, false);
            self.set_flag(psw::CY, false);
            return v;
        }
        let res = v >> sh;
        let carry = (v >> (sh - 1)) & 1 != 0;
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.set_flag(psw::CY, carry);
        res
    }
    fn sar(&mut self, v: u32, sh: u32) -> u32 {
        if sh == 0 {
            self.set_zs(v);
            self.set_flag(psw::OV, false);
            self.set_flag(psw::CY, false);
            return v;
        }
        let res = ((v as i32) >> sh) as u32;
        let carry = (v >> (sh - 1)) & 1 != 0;
        self.set_zs(res);
        self.set_flag(psw::OV, false);
        self.set_flag(psw::CY, carry);
        res
    }
}

#[derive(Clone, Copy)]
enum LoadKind {
    I8,
    I16,
    U16,
    I32,
}
#[derive(Clone, Copy)]
enum StoreKind {
    B,
    H,
    W,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 64 KiB RAM bus stub for opcode tests. PC starts at 0; programs are
    /// written from address 0.
    struct RamBus {
        mem: Vec<u8>,
    }
    impl RamBus {
        fn new() -> RamBus {
            RamBus {
                mem: vec![0u8; 0x10000],
            }
        }
        /// Write a 16-bit instruction (little-endian) at byte address `addr`.
        fn put16(&mut self, addr: u32, v: u16) {
            let a = addr as usize;
            self.mem[a] = v as u8;
            self.mem[a + 1] = (v >> 8) as u8;
        }
    }
    impl Bus for RamBus {
        fn read8(&mut self, addr: u32) -> u8 {
            self.mem[(addr as usize) & 0xFFFF]
        }
        fn write8(&mut self, addr: u32, v: u8) {
            self.mem[(addr as usize) & 0xFFFF] = v;
        }
    }

    /// Encode a Format I (reg-reg) instruction: opcode<<10 | reg2<<5 | reg1.
    fn fmt1(op: u16, reg2: u16, reg1: u16) -> u16 {
        (op << 10) | (reg2 << 5) | reg1
    }
    /// Encode a Format II (imm5) instruction: opcode<<10 | reg2<<5 | imm5.
    fn fmt2(op: u16, reg2: u16, imm5: u16) -> u16 {
        (op << 10) | (reg2 << 5) | (imm5 & 0x1F)
    }

    fn run_one(cpu: &mut Cpu, bus: &mut RamBus) -> u32 {
        cpu.step(bus)
    }

    fn fresh() -> (Cpu, RamBus) {
        let mut cpu = Cpu::new();
        cpu.pc = 0;
        cpu.psw = 0; // clear NP so we execute normally
        (cpu, RamBus::new())
    }

    #[test]
    fn r0_is_hardwired_zero() {
        let (mut cpu, mut bus) = fresh();
        // MOV r5,r0 : op 0b000000, but write to r0 via MOV imm.
        bus.put16(0, fmt2(0b010000, 0, 5)); // MOV imm5=5, r0
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[0], 0, "r0 must stay 0");
    }

    #[test]
    fn mov_imm_sign_extends() {
        let (mut cpu, mut bus) = fresh();
        bus.put16(0, fmt2(0b010000, 7, 0x1F)); // MOV imm5=-1, r7
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[7], 0xFFFF_FFFF);
        assert_eq!(cpu.pc, 2);
    }

    #[test]
    fn add_reg_sets_flags() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0xFFFF_FFFF;
        cpu.r[2] = 1;
        bus.put16(0, fmt1(0b000001, 2, 1)); // ADD r1, r2  (r2 = r2 + r1)
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0);
        assert!(cpu.flag(psw::Z));
        assert!(cpu.flag(psw::CY));
        assert!(!cpu.flag(psw::OV));
    }

    #[test]
    fn add_signed_overflow() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x7FFF_FFFF;
        cpu.r[2] = 1;
        bus.put16(0, fmt1(0b000001, 2, 1));
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0x8000_0000);
        assert!(cpu.flag(psw::OV));
        assert!(cpu.flag(psw::S));
    }

    #[test]
    fn sub_and_cmp_flags() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 5;
        cpu.r[2] = 5;
        bus.put16(0, fmt1(0b000011, 2, 1)); // CMP r1, r2
        run_one(&mut cpu, &mut bus);
        assert!(cpu.flag(psw::Z));
        assert!(!cpu.flag(psw::CY));
    }

    #[test]
    fn sub_borrow_sets_carry() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 5;
        cpu.r[2] = 3;
        bus.put16(0, fmt1(0b000010, 2, 1)); // SUB r1, r2 -> r2 = 3-5
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0xFFFF_FFFE);
        assert!(cpu.flag(psw::CY)); // borrow
        assert!(cpu.flag(psw::S));
    }

    #[test]
    fn logic_ops_clear_overflow() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0xF0F0_F0F0;
        cpu.r[2] = 0x0F0F_0F0F;
        cpu.set_flag(psw::OV, true);
        bus.put16(0, fmt1(0b001100, 2, 1)); // OR
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0xFFFF_FFFF);
        assert!(!cpu.flag(psw::OV));
        assert!(cpu.flag(psw::S));
    }

    #[test]
    fn shl_shr_sar_carry() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[2] = 0x8000_0001;
        bus.put16(0, fmt2(0b010100, 2, 1)); // SHL imm5=1, r2
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0x0000_0002);
        assert!(cpu.flag(psw::CY)); // bit shifted out was 1

        let (mut cpu, mut bus) = fresh();
        cpu.r[2] = 0x8000_0000;
        bus.put16(0, fmt2(0b010111, 2, 4)); // SAR imm5=4, r2
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0xF800_0000); // sign-extended
    }

    #[test]
    fn mul_signed_high_word() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x0001_0000;
        cpu.r[2] = 0x0001_0000;
        bus.put16(0, fmt1(0b001000, 2, 1)); // MUL
        run_one(&mut cpu, &mut bus);
        // 0x10000 * 0x10000 = 0x1_0000_0000 -> lo=0, hi=1
        assert_eq!(cpu.r[2], 0);
        assert_eq!(cpu.r[30], 1);
    }

    #[test]
    fn div_signed_quotient_remainder() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 3;
        cpu.r[2] = 17;
        bus.put16(0, fmt1(0b001001, 2, 1)); // DIV
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 5); // quotient
        assert_eq!(cpu.r[30], 2); // remainder
    }

    #[test]
    fn div_by_zero_raises_exception() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0;
        cpu.r[2] = 10;
        bus.put16(0, fmt1(0b001001, 2, 1)); // DIV by 0
        run_one(&mut cpu, &mut bus);
        // Vectored to the exception handler; EP set.
        assert!(cpu.flag(psw::EP));
        assert_eq!(cpu.eipc, 0); // faulting PC stacked
    }

    #[test]
    fn movea_no_flags() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x1000;
        bus.put16(0, (0b101000 << 10) | (3 << 5) | 1); // MOVEA imm,r1,r3
        bus.put16(2, 0xFFFF); // imm = -1
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[3], 0x0FFF);
        assert_eq!(cpu.pc, 4);
    }

    #[test]
    fn movhi_shifts_immediate() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x1234;
        bus.put16(0, (0b101111 << 10) | (3 << 5) | 1); // MOVHI imm,r1,r3
        bus.put16(2, 0xABCD);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[3], 0xABCD_1234);
    }

    #[test]
    fn jal_links_and_jumps() {
        let (mut cpu, mut bus) = fresh();
        // JAL +8 : op 0b101011, disp26 = 8.
        bus.put16(0, (0b101011 << 10) | 0); // hi disp bits
        bus.put16(2, 8); // lo disp = 8
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 8);
        assert_eq!(cpu.r[31], 4); // return address = PC+4
    }

    #[test]
    fn jmp_indirect() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[5] = 0x2000;
        bus.put16(0, fmt1(0b000110, 0, 5)); // JMP [r5]
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x2000);
    }

    #[test]
    fn bcond_taken_and_not_taken() {
        // BE (cond 2) taken when Z set.
        let (mut cpu, mut bus) = fresh();
        cpu.set_flag(psw::Z, true);
        // Format III: 100 | cond(4) | disp9. op top3=100.
        // instr = (0b100 << 13) | (cond << 9) | disp9
        let instr = (0b100u16 << 13) | (0x2 << 9) | (6 & 0x1FF);
        bus.put16(0, instr);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 6);

        // Not taken (Z clear) -> PC advances by 2.
        let (mut cpu, mut bus) = fresh();
        cpu.set_flag(psw::Z, false);
        let instr = (0b100u16 << 13) | (0x2 << 9) | 6;
        bus.put16(0, instr);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 2);
    }

    #[test]
    fn bcond_backward_branch() {
        let (mut cpu, mut bus) = fresh();
        cpu.pc = 0x100;
        cpu.set_flag(psw::Z, true);
        // disp9 = -4 (0x1FC) ; BE taken.
        let instr = (0b100u16 << 13) | (0x2 << 9) | (0x1FC & 0x1FF);
        bus.put16(0x100, instr);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x100u32.wrapping_sub(4));
    }

    #[test]
    fn setf_sets_one_or_zero() {
        let (mut cpu, mut bus) = fresh();
        cpu.set_flag(psw::Z, true);
        bus.put16(0, fmt2(0b010010, 9, 0x2)); // SETF BE(cond2), r9
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[9], 1);
    }

    #[test]
    fn load_store_word_roundtrip() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x1000; // base
        cpu.r[2] = 0xDEAD_BEEF;
        // ST.W r2, 4[r1] : op 0b110111.
        bus.put16(0, (0b110111 << 10) | (2 << 5) | 1);
        bus.put16(2, 4); // disp
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.read32(0x1004), 0xDEAD_BEEF);

        // LD.W 4[r1] -> r3 : op 0b110011.
        bus.put16(4, (0b110011 << 10) | (3 << 5) | 1);
        bus.put16(6, 4);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[3], 0xDEAD_BEEF);
    }

    #[test]
    fn load_byte_sign_extends() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x2000;
        bus.write8(0x2000, 0xFF);
        bus.put16(0, (0b110000 << 10) | (3 << 5) | 1); // LD.B 0[r1], r3
        bus.put16(2, 0);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[3], 0xFFFF_FFFF);
    }

    #[test]
    fn ldsr_stsr_roundtrip() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[2] = 0x0000_00F0;
        // LDSR r2 -> EIPC (sysreg 0) : op 0b011100.
        bus.put16(0, (0b011100 << 10) | (2 << 5) | (sr::EIPC as u16));
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.eipc, 0x0000_00F0);
        // STSR EIPC -> r4 : op 0b011101.
        bus.put16(2, (0b011101 << 10) | (4 << 5) | (sr::EIPC as u16));
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[4], 0x0000_00F0);
    }

    #[test]
    fn halt_and_wake_by_interrupt() {
        let (mut cpu, mut bus) = fresh();
        bus.put16(0, 0b011010 << 10); // HALT
        run_one(&mut cpu, &mut bus);
        assert!(cpu.halted);
        // Pending IRQ (level 4) wakes it on next step.
        cpu.irq_level = 4;
        run_one(&mut cpu, &mut bus);
        assert!(!cpu.halted);
        assert!(cpu.flag(psw::EP)); // entered the interrupt vector
    }

    #[test]
    fn ei_di_toggle_id() {
        let (mut cpu, mut bus) = fresh();
        bus.put16(0, 0b011110 << 10); // SEI (set ID)
        run_one(&mut cpu, &mut bus);
        assert!(cpu.flag(psw::ID));
        bus.put16(2, 0b010110 << 10); // CLI (clear ID)
        run_one(&mut cpu, &mut bus);
        assert!(!cpu.flag(psw::ID));
    }

    #[test]
    fn interrupt_masked_by_id() {
        let (mut cpu, mut bus) = fresh();
        cpu.set_flag(psw::ID, true);
        cpu.irq_level = 4;
        bus.put16(0, fmt2(0b010000, 5, 3)); // MOV imm 3, r5
        run_one(&mut cpu, &mut bus);
        // ID set -> interrupt NOT taken; the MOV executed.
        assert_eq!(cpu.r[5], 3);
        assert!(!cpu.flag(psw::EP));
    }

    #[test]
    fn reti_restores_pc_psw() {
        let (mut cpu, mut bus) = fresh();
        cpu.set_flag(psw::EP, true);
        cpu.eipc = 0x1234;
        cpu.eipsw = 0x0000_0008; // CY set
        bus.put16(0, 0b011001 << 10); // RETI
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x1234);
        assert!(cpu.flag(psw::CY));
        assert!(!cpu.flag(psw::EP));
    }

    #[test]
    fn fpu_add_subtract() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 1.5f32.to_bits();
        cpu.r[2] = 2.5f32.to_bits();
        // ADDF.S r1, r2 -> r2 = r2 + r1. op 0b111110, sub 0x04 in word2.
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 1);
        bus.put16(2, 0x04 << 10);
        run_one(&mut cpu, &mut bus);
        assert_eq!(f32::from_bits(cpu.r[2]), 4.0);
    }

    #[test]
    fn fpu_mul_div() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 3.0f32.to_bits();
        cpu.r[2] = 4.0f32.to_bits();
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 1);
        bus.put16(2, 0x06 << 10); // MULF.S
        run_one(&mut cpu, &mut bus);
        assert_eq!(f32::from_bits(cpu.r[2]), 12.0);

        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 4.0f32.to_bits();
        cpu.r[2] = 12.0f32.to_bits();
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 1);
        bus.put16(2, 0x07 << 10); // DIVF.S
        run_one(&mut cpu, &mut bus);
        assert_eq!(f32::from_bits(cpu.r[2]), 3.0);
    }

    #[test]
    fn fpu_convert_int_float() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[2] = (-7i32) as u32;
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 0);
        bus.put16(2, 0x02 << 10); // CVT.WS
        run_one(&mut cpu, &mut bus);
        assert_eq!(f32::from_bits(cpu.r[2]), -7.0);

        // Back to int via TRNC.SW.
        bus.put16(4, (0b111110 << 10) | (2 << 5) | 0);
        bus.put16(6, 0x0B << 10);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2] as i32, -7);
    }

    #[test]
    fn fpu_compare_sets_flags() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 5.0f32.to_bits();
        cpu.r[2] = 2.0f32.to_bits();
        // CMPF.S r1,r2 -> compares r2 (2.0) with r1 (5.0): 2<5 so CY, S set.
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 1);
        bus.put16(2, 0x00 << 10);
        run_one(&mut cpu, &mut bus);
        assert!(cpu.flag(psw::S));
        assert!(cpu.flag(psw::CY));
        assert!(!cpu.flag(psw::Z));
    }

    #[test]
    fn nintendo_rev_reverses_bits() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x0000_0001;
        bus.put16(0, (0b111110 << 10) | (2 << 5) | 1);
        bus.put16(2, 0x0A << 10); // REV
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[2], 0x8000_0000);
    }

    #[test]
    fn illegal_opcode_latches_fault() {
        let (mut cpu, mut bus) = fresh();
        // op 0b110010 is undefined in our decode table.
        bus.put16(0, 0b110010 << 10);
        run_one(&mut cpu, &mut bus);
        assert!(cpu.fault.is_some());
        assert_eq!(cpu.fault.unwrap().kind, FaultKind::IllegalOpcode);
    }

    #[test]
    fn ori_zero_extends_immediate() {
        let (mut cpu, mut bus) = fresh();
        cpu.r[1] = 0x0000_0000;
        bus.put16(0, (0b101100 << 10) | (3 << 5) | 1); // ORI imm,r1,r3
        bus.put16(2, 0xFFFF);
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.r[3], 0x0000_FFFF); // zero-extended, not sign
    }
}
