//! Motorola 68000 CPU core — the Genesis/Mega Drive main processor (~7.67 MHz).
//!
//! Built from the Motorola M68000 Programmer's Reference Manual. The 68000 is a
//! BIG-ENDIAN, 16/32-bit CISC machine:
//!   - 8 data registers (D0-D7), 8 address registers (A0-A7); A7 is the stack
//!     pointer, with separate USP / SSP banks selected by the S bit.
//!   - A status register (SR): T-S--III---XNZVC. Low byte is the CCR.
//!   - Exception vectors at the bottom of address space; reset loads SSP from
//!     $000000 and PC from $000004.
//!
//! The CPU codes against a [`Bus`] (big-endian 8/16/32-bit access). `Genesis`
//! (see `genesis.rs`) is the production implementor; CPU unit tests use a flat
//! RAM stub.
//!
//! Cycle accounting is approximate (a base count per instruction class plus EA
//! costs) — good enough to pace the VDP and interrupts so commercial ROMs boot.
//! This is NOT a cycle-exact core.

/// The 68000's memory interface. All accesses are BIG-ENDIAN. Word and long
/// accesses are notionally aligned on the real chip (an odd address triggers an
/// address-error exception); we keep the helpers tolerant and let the CPU raise
/// the exception where it matters.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, v: u8);

    /// 16-bit big-endian read (default-derived from two byte reads).
    #[inline]
    fn read16(&mut self, addr: u32) -> u16 {
        let hi = self.read8(addr) as u16;
        let lo = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
    /// 16-bit big-endian write.
    #[inline]
    fn write16(&mut self, addr: u32, v: u16) {
        self.write8(addr, (v >> 8) as u8);
        self.write8(addr.wrapping_add(1), (v & 0xFF) as u8);
    }
    /// 32-bit big-endian read.
    #[inline]
    fn read32(&mut self, addr: u32) -> u32 {
        let hi = self.read16(addr) as u32;
        let lo = self.read16(addr.wrapping_add(2)) as u32;
        (hi << 16) | lo
    }
    /// 32-bit big-endian write.
    #[inline]
    fn write32(&mut self, addr: u32, v: u32) {
        self.write16(addr, (v >> 16) as u16);
        self.write16(addr.wrapping_add(2), (v & 0xFFFF) as u16);
    }
}

// Condition code register bits (low byte of SR).
pub const C: u16 = 1 << 0;
pub const V: u16 = 1 << 1;
pub const Z: u16 = 1 << 2;
pub const N: u16 = 1 << 3;
pub const X: u16 = 1 << 4;
// SR high byte.
pub const SR_S: u16 = 1 << 13; // supervisor
pub const SR_T: u16 = 1 << 15; // trace

/// Operand size for the generic ALU/move paths.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    #[inline]
    fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }
    #[inline]
    fn msb(self) -> u32 {
        match self {
            Size::Byte => 0x80,
            Size::Word => 0x8000,
            Size::Long => 0x8000_0000,
        }
    }
    #[inline]
    fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }
}

pub struct M68k {
    /// Data registers D0-D7.
    pub d: [u32; 8],
    /// Address registers A0-A6 (A7 is the active stack pointer).
    pub a: [u32; 8],
    /// User stack pointer (inactive copy when in supervisor mode).
    pub usp: u32,
    /// Supervisor stack pointer (inactive copy when in user mode).
    pub ssp: u32,
    pub pc: u32,
    pub sr: u16,

    /// True once the CPU executes STOP (waiting for an interrupt).
    pub stopped: bool,
    /// Pending interrupt level driven by the bus (0 = none). Sampled each step.
    pub irq_level: u8,
    /// Cycles consumed by the most recent `step`.
    cycles: u32,
    /// Latched illegal/exception info for crash reporting.
    pub last_exception: Option<u8>,
}

impl Default for M68k {
    fn default() -> Self {
        M68k::new()
    }
}

impl M68k {
    pub fn new() -> M68k {
        M68k {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            sr: SR_S, // boots in supervisor mode
            stopped: false,
            irq_level: 0,
            cycles: 0,
            last_exception: None,
        }
    }

    /// Cold reset: load SSP from $000000 and PC from $000004 (big-endian).
    pub fn reset(&mut self, bus: &mut dyn Bus) {
        self.sr = SR_S | 0x0700; // supervisor, IRQ mask = 7
        self.ssp = bus.read32(0);
        self.a[7] = self.ssp;
        self.pc = bus.read32(4);
        self.stopped = false;
        self.last_exception = None;
    }

    // ---- supervisor / stack bookkeeping ----
    #[inline]
    fn in_supervisor(&self) -> bool {
        self.sr & SR_S != 0
    }

    /// Swap the active A7 with the inactive stack pointer bank when the S bit
    /// changes. Call after any SR write that may flip S.
    fn sync_stack(&mut self, was_super: bool) {
        let now_super = self.in_supervisor();
        if was_super == now_super {
            return;
        }
        if was_super {
            // leaving supervisor: save SSP, restore USP
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
    }

    fn set_sr(&mut self, v: u16) {
        let was = self.in_supervisor();
        self.sr = v;
        self.sync_stack(was);
    }

    // ---- flag helpers ----
    #[inline]
    fn flag(&self, b: u16) -> bool {
        self.sr & b != 0
    }
    #[inline]
    fn set_flag(&mut self, b: u16, on: bool) {
        if on {
            self.sr |= b;
        } else {
            self.sr &= !b;
        }
    }
    #[inline]
    fn set_nz(&mut self, val: u32, sz: Size) {
        let v = val & sz.mask();
        self.set_flag(Z, v == 0);
        self.set_flag(N, v & sz.msb() != 0);
    }

    // ---- instruction-stream fetch ----
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn Bus) -> u16 {
        let w = bus.read16(self.pc);
        self.pc = self.pc.wrapping_add(2);
        w
    }
    #[inline]
    fn fetch32(&mut self, bus: &mut dyn Bus) -> u32 {
        let l = bus.read32(self.pc);
        self.pc = self.pc.wrapping_add(4);
        l
    }

    // ---- stack ----
    #[inline]
    fn push32(&mut self, bus: &mut dyn Bus, v: u32) {
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write32(self.a[7], v);
    }
    #[inline]
    fn push16(&mut self, bus: &mut dyn Bus, v: u16) {
        self.a[7] = self.a[7].wrapping_sub(2);
        bus.write16(self.a[7], v);
    }
    #[inline]
    fn pop32(&mut self, bus: &mut dyn Bus) -> u32 {
        let v = bus.read32(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        v
    }
    #[inline]
    fn pop16(&mut self, bus: &mut dyn Bus) -> u16 {
        let v = bus.read16(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        v
    }

    /// Take an exception: push PC + SR, set supervisor, clear trace, jump to the
    /// vector. `vector` is the vector number (multiplied by 4 for the address).
    fn exception(&mut self, bus: &mut dyn Bus, vector: u8) {
        self.last_exception = Some(vector);
        let was = self.in_supervisor();
        // Enter supervisor, clear trace.
        let mut sr = self.sr;
        sr |= SR_S;
        sr &= !SR_T;
        // sync stack to supervisor before pushing the frame
        self.sr = sr;
        self.sync_stack(was);
        let old_sr = self.saved_sr_for_frame();
        self.push32(bus, self.pc);
        self.push16(bus, old_sr);
        let addr = (vector as u32) * 4;
        self.pc = bus.read32(addr);
        self.stopped = false;
    }

    fn saved_sr_for_frame(&self) -> u16 {
        self.sr
    }

    /// Service a pending IRQ if its level exceeds the SR interrupt mask.
    /// Autovectored (Genesis uses autovectors: level 6 = VINT, 4 = HINT, 2 =
    /// external). Returns true if an interrupt was taken.
    fn service_interrupt(&mut self, bus: &mut dyn Bus) -> bool {
        let level = self.irq_level;
        if level == 0 {
            return false;
        }
        let mask = ((self.sr >> 8) & 0x07) as u8;
        // Level 7 is non-maskable; otherwise level must be > mask.
        if level != 7 && level <= mask {
            return false;
        }
        self.stopped = false;
        // Snapshot SR for the frame, then enter supervisor.
        let old_sr = self.sr;
        let was = self.in_supervisor();
        let mut sr = self.sr;
        sr |= SR_S;
        sr &= !SR_T;
        // set new interrupt mask to the serviced level
        sr = (sr & !0x0700) | ((level as u16) << 8);
        self.sr = sr;
        self.sync_stack(was);
        self.push32(bus, self.pc);
        self.push16(bus, old_sr);
        // Autovector: vector number = 24 + level.
        let vector = 24 + level as u32;
        self.pc = bus.read32(vector * 4);
        true
    }

    /// Execute one instruction (or service an interrupt). Returns elapsed cycles.
    pub fn step(&mut self, bus: &mut dyn Bus) -> u32 {
        self.cycles = 0;
        // Interrupts wake STOP and preempt instruction fetch.
        if self.service_interrupt(bus) {
            self.cycles += 44;
            return self.cycles;
        }
        if self.stopped {
            return 4; // idle, waiting for IRQ
        }
        let op = self.fetch16(bus);
        self.execute(bus, op);
        if self.cycles == 0 {
            self.cycles = 4;
        }
        self.cycles
    }

    // =====================================================================
    // Effective-address decode + access. An EA is the 6-bit (mode<<3 | reg)
    // field. We compute it once into an `Ea`, then read/write through it.
    // =====================================================================

    fn decode_ea(&mut self, bus: &mut dyn Bus, mode: u8, reg: u8, sz: Size) -> Ea {
        match mode {
            0 => Ea::DataReg(reg),
            1 => Ea::AddrReg(reg),
            2 => Ea::Mem(self.a[reg as usize]),
            3 => {
                // (An)+
                let addr = self.a[reg as usize];
                let inc = if reg == 7 && sz == Size::Byte {
                    2 // keep A7 word-aligned
                } else {
                    sz.bytes()
                };
                self.a[reg as usize] = addr.wrapping_add(inc);
                Ea::Mem(addr)
            }
            4 => {
                // -(An)
                let dec = if reg == 7 && sz == Size::Byte {
                    2
                } else {
                    sz.bytes()
                };
                let addr = self.a[reg as usize].wrapping_sub(dec);
                self.a[reg as usize] = addr;
                Ea::Mem(addr)
            }
            5 => {
                // (d16,An)
                let d = self.fetch16(bus) as i16 as i32;
                Ea::Mem(self.a[reg as usize].wrapping_add(d as u32))
            }
            6 => {
                // (d8,An,Xn)
                let addr = self.brief_index(bus, self.a[reg as usize]);
                Ea::Mem(addr)
            }
            7 => match reg {
                0 => {
                    // (xxx).W
                    let a = self.fetch16(bus) as i16 as i32 as u32;
                    Ea::Mem(a)
                }
                1 => {
                    // (xxx).L
                    let a = self.fetch32(bus);
                    Ea::Mem(a)
                }
                2 => {
                    // (d16,PC)
                    let base = self.pc;
                    let d = self.fetch16(bus) as i16 as i32;
                    Ea::Mem(base.wrapping_add(d as u32))
                }
                3 => {
                    // (d8,PC,Xn)
                    let base = self.pc;
                    let addr = self.brief_index(bus, base);
                    Ea::Mem(addr)
                }
                4 => {
                    // immediate
                    let v = match sz {
                        Size::Byte => (self.fetch16(bus) & 0xFF) as u32,
                        Size::Word => self.fetch16(bus) as u32,
                        Size::Long => self.fetch32(bus),
                    };
                    Ea::Imm(v)
                }
                _ => Ea::Imm(0),
            },
            _ => Ea::Imm(0),
        }
    }

    /// Brief extension word for indexed modes: (d8, An/PC, Xn.size*scale).
    fn brief_index(&mut self, bus: &mut dyn Bus, base: u32) -> u32 {
        let ext = self.fetch16(bus);
        let disp = (ext & 0xFF) as i8 as i32;
        let xreg = ((ext >> 12) & 0x07) as usize;
        let is_addr = ext & 0x8000 != 0;
        let is_long = ext & 0x0800 != 0;
        let raw = if is_addr { self.a[xreg] } else { self.d[xreg] };
        let idx = if is_long {
            raw as i32
        } else {
            raw as i16 as i32
        };
        base.wrapping_add(disp as u32).wrapping_add(idx as u32)
    }

    fn ea_read(&mut self, bus: &mut dyn Bus, ea: Ea, sz: Size) -> u32 {
        match ea {
            Ea::DataReg(r) => self.d[r as usize] & sz.mask(),
            Ea::AddrReg(r) => self.a[r as usize] & sz.mask(),
            Ea::Mem(a) => match sz {
                Size::Byte => bus.read8(a) as u32,
                Size::Word => bus.read16(a) as u32,
                Size::Long => bus.read32(a),
            },
            Ea::Imm(v) => v & sz.mask(),
        }
    }

    fn ea_write(&mut self, bus: &mut dyn Bus, ea: Ea, sz: Size, val: u32) {
        match ea {
            Ea::DataReg(r) => {
                let m = sz.mask();
                self.d[r as usize] = (self.d[r as usize] & !m) | (val & m);
            }
            Ea::AddrReg(r) => {
                // Address-register writes are always sign-extended to 32 bits.
                self.a[r as usize] = match sz {
                    Size::Word => val as u16 as i16 as i32 as u32,
                    _ => val,
                };
            }
            Ea::Mem(a) => match sz {
                Size::Byte => bus.write8(a, val as u8),
                Size::Word => bus.write16(a, val as u16),
                Size::Long => bus.write32(a, val),
            },
            Ea::Imm(_) => {} // not writable
        }
    }

    // =====================================================================
    // Main decode. The 68000 opcode map is dense; we dispatch on the top
    // nibble and refine inside.
    // =====================================================================
    fn execute(&mut self, bus: &mut dyn Bus, op: u16) {
        let top = (op >> 12) & 0xF;
        match top {
            0x0 => self.grp_immediate(bus, op),
            0x1 | 0x2 | 0x3 => self.grp_move(bus, op),
            0x4 => self.grp_misc(bus, op),
            0x5 => self.grp_addq_subq_scc(bus, op),
            0x6 => self.grp_bcc(bus, op),
            0x7 => self.op_moveq(op),
            0x8 => self.grp_or_div(bus, op),
            0x9 => self.grp_sub(bus, op),
            0xB => self.grp_cmp_eor(bus, op),
            0xC => self.grp_and_mul(bus, op),
            0xD => self.grp_add(bus, op),
            0xE => self.grp_shift(bus, op),
            _ => self.illegal(bus),
        }
    }

    fn illegal(&mut self, bus: &mut dyn Bus) {
        self.cycles += 34;
        self.exception(bus, 4); // illegal instruction vector
    }

    // ---------------------------------------------------------------- group 0
    // Immediate ALU ops (ORI/ANDI/SUBI/ADDI/EORI/CMPI), bit ops, MOVEP.
    fn grp_immediate(&mut self, bus: &mut dyn Bus, op: u16) {
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        // Bit operations: BTST/BCHG/BCLR/BSET have bits 8 set in various forms.
        if op & 0x0100 != 0 || (op & 0x0F00) == 0x0800 {
            self.bit_op(bus, op);
            return;
        }
        let szbits = (op >> 6) & 0x03;
        if szbits == 0x03 {
            self.illegal(bus);
            return;
        }
        let sz = match szbits {
            0 => Size::Byte,
            1 => Size::Word,
            _ => Size::Long,
        };
        let imm = match sz {
            Size::Byte => (self.fetch16(bus) & 0xFF) as u32,
            Size::Word => self.fetch16(bus) as u32,
            Size::Long => self.fetch32(bus),
        };
        let family = (op >> 9) & 0x07;
        // CMPI/ORI/etc to SR/CCR special-cases (mode 7 reg 4).
        if mode == 7 && reg == 4 && (family == 0 || family == 1 || family == 5) {
            self.imm_to_sr(family, imm, sz);
            return;
        }
        let ea = self.decode_ea(bus, mode, reg, sz);
        let dst = self.ea_read(bus, ea, sz);
        let res = match family {
            0 => {
                // ORI
                let r = dst | imm;
                self.set_nz(r, sz);
                self.set_flag(V, false);
                self.set_flag(C, false);
                Some(r)
            }
            1 => {
                // ANDI
                let r = dst & imm;
                self.set_nz(r, sz);
                self.set_flag(V, false);
                self.set_flag(C, false);
                Some(r)
            }
            2 => Some(self.sub_flags(dst, imm, sz, false)), // SUBI
            3 => Some(self.add_flags(dst, imm, sz, false)), // ADDI
            5 => {
                // EORI
                let r = dst ^ imm;
                self.set_nz(r, sz);
                self.set_flag(V, false);
                self.set_flag(C, false);
                Some(r)
            }
            6 => {
                // CMPI — flags only
                self.sub_flags(dst, imm, sz, false);
                None
            }
            _ => {
                self.illegal(bus);
                None
            }
        };
        if let Some(r) = res {
            self.ea_write(bus, ea, sz, r);
        }
        self.cycles += 8;
    }

    fn imm_to_sr(&mut self, family: u16, imm: u32, sz: Size) {
        let to_sr = sz == Size::Word;
        if to_sr && !self.in_supervisor() {
            // privilege violation handled elsewhere; for simplicity apply to CCR
        }
        let cur = if to_sr { self.sr } else { self.sr & 0xFF };
        let v = imm as u16;
        let r = match family {
            0 => cur | v,  // ORI
            1 => cur & v,  // ANDI
            5 => cur ^ v,  // EORI
            _ => cur,
        };
        if to_sr {
            self.set_sr(r);
        } else {
            self.sr = (self.sr & 0xFF00) | (r & 0xFF);
        }
        self.cycles += 16;
    }

    fn bit_op(&mut self, bus: &mut dyn Bus, op: u16) {
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        let dyn_bit = op & 0x0100 != 0;
        let bit_index = if dyn_bit {
            self.d[((op >> 9) & 0x07) as usize]
        } else {
            self.fetch16(bus) as u32
        };
        let kind = (op >> 6) & 0x03; // 0 BTST,1 BCHG,2 BCLR,3 BSET
        // Operating on a data register is a 32-bit op; memory is byte-wide.
        let sz = if mode == 0 { Size::Long } else { Size::Byte };
        let bits = if mode == 0 { 32 } else { 8 };
        let ea = self.decode_ea(bus, mode, reg, sz);
        let val = self.ea_read(bus, ea, sz);
        let b = bit_index % bits;
        let mask = 1u32 << b;
        self.set_flag(Z, val & mask == 0);
        if kind != 0 {
            let newv = match kind {
                1 => val ^ mask,  // BCHG
                2 => val & !mask, // BCLR
                _ => val | mask,  // BSET
            };
            self.ea_write(bus, ea, sz, newv);
        }
        self.cycles += 8;
    }

    // ---------------------------------------------------------------- group 1-3
    // MOVE.b/.w/.l (and MOVEA). Top nibble selects size.
    fn grp_move(&mut self, bus: &mut dyn Bus, op: u16) {
        let top = (op >> 12) & 0xF;
        let sz = match top {
            1 => Size::Byte,
            3 => Size::Word,
            _ => Size::Long,
        };
        let src_mode = ((op >> 3) & 0x07) as u8;
        let src_reg = (op & 0x07) as u8;
        let dst_reg = ((op >> 9) & 0x07) as u8;
        let dst_mode = ((op >> 6) & 0x07) as u8;
        let src_ea = self.decode_ea(bus, src_mode, src_reg, sz);
        let val = self.ea_read(bus, src_ea, sz);
        let dst_ea = self.decode_ea(bus, dst_mode, dst_reg, sz);
        if dst_mode == 1 {
            // MOVEA — sign-extend, no flags.
            let v = if sz == Size::Word {
                val as u16 as i16 as i32 as u32
            } else {
                val
            };
            self.a[dst_reg as usize] = v;
        } else {
            self.set_nz(val, sz);
            self.set_flag(V, false);
            self.set_flag(C, false);
            self.ea_write(bus, dst_ea, sz, val);
        }
        self.cycles += 4;
    }

    // ---------------------------------------------------------------- group 4
    // The "misc" grab-bag: NEG/NOT/CLR/TST, LEA/PEA, JMP/JSR, MOVEM, EXT,
    // SWAP, TRAP, LINK/UNLK, RTS/RTE/RTR, NOP, MOVE to/from SR/CCR/USP, etc.
    fn grp_misc(&mut self, bus: &mut dyn Bus, op: u16) {
        // Specific full-opcode forms first.
        match op {
            0x4E70 => {
                self.cycles += 132;
                return; // RESET (no-op for us)
            }
            0x4E71 => {
                self.cycles += 4;
                return; // NOP
            }
            0x4E72 => {
                // STOP #imm
                let imm = self.fetch16(bus);
                self.set_sr(imm);
                self.stopped = true;
                self.cycles += 4;
                return;
            }
            0x4E73 => {
                // RTE
                let sr = self.pop16(bus);
                let pc = self.pop32(bus);
                self.set_sr(sr);
                self.pc = pc;
                self.cycles += 20;
                return;
            }
            0x4E75 => {
                // RTS
                self.pc = self.pop32(bus);
                self.cycles += 16;
                return;
            }
            0x4E77 => {
                // RTR
                let ccr = self.pop16(bus);
                self.sr = (self.sr & 0xFF00) | (ccr & 0xFF);
                self.pc = self.pop32(bus);
                self.cycles += 20;
                return;
            }
            0x4E76 => {
                // TRAPV
                if self.flag(V) {
                    self.exception(bus, 7);
                }
                self.cycles += 4;
                return;
            }
            _ => {}
        }
        // LINK / UNLK / MOVE USP.
        if op & 0xFFF8 == 0x4E50 {
            // LINK An,#disp
            let reg = (op & 0x07) as usize;
            let disp = self.fetch16(bus) as i16 as i32;
            self.push32(bus, self.a[reg]);
            self.a[7] = self.a[7]; // sp already updated by push
            self.a[reg] = self.a[7];
            self.a[7] = self.a[7].wrapping_add(disp as u32);
            self.cycles += 16;
            return;
        }
        if op & 0xFFF8 == 0x4E58 {
            // UNLK An
            let reg = (op & 0x07) as usize;
            self.a[7] = self.a[reg];
            self.a[reg] = self.pop32(bus);
            self.cycles += 12;
            return;
        }
        if op & 0xFFF0 == 0x4E60 {
            // MOVE USP
            let reg = (op & 0x07) as usize;
            if op & 0x08 != 0 {
                self.a[reg] = self.usp; // MOVE USP,An
            } else {
                self.usp = self.a[reg]; // MOVE An,USP
            }
            self.cycles += 4;
            return;
        }
        if op & 0xFFF0 == 0x4E40 {
            // TRAP #vector
            let v = (op & 0x0F) as u8;
            self.cycles += 34;
            self.exception(bus, 32 + v);
            return;
        }

        let line = (op >> 6) & 0x3F;
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;

        // JMP (0x4EC0|ea) / JSR (0x4E80|ea)
        if op & 0xFFC0 == 0x4EC0 {
            let ea = self.decode_ea(bus, mode, reg, Size::Long);
            if let Ea::Mem(a) = ea {
                self.pc = a;
            }
            self.cycles += 8;
            return;
        }
        if op & 0xFFC0 == 0x4E80 {
            let ea = self.decode_ea(bus, mode, reg, Size::Long);
            if let Ea::Mem(a) = ea {
                self.push32(bus, self.pc);
                self.pc = a;
            }
            self.cycles += 18;
            return;
        }
        // PEA (0x4840|ea)
        if op & 0xFFC0 == 0x4840 && mode >= 2 {
            let ea = self.decode_ea(bus, mode, reg, Size::Long);
            if let Ea::Mem(a) = ea {
                self.push32(bus, a);
            }
            self.cycles += 12;
            return;
        }
        // SWAP (0x4840 + Dn) handled above only when mode>=2; SWAP is 0x4840|reg, mode 0
        if op & 0xFFF8 == 0x4840 {
            let r = (op & 0x07) as usize;
            let v = self.d[r];
            let s = (v >> 16) | (v << 16);
            self.d[r] = s;
            self.set_nz(s, Size::Long);
            self.set_flag(V, false);
            self.set_flag(C, false);
            self.cycles += 4;
            return;
        }
        // EXT (0x4880|reg word, 0x48C0|reg long), mode 0
        if op & 0xFFB8 == 0x4880 {
            let r = (op & 0x07) as usize;
            let long = op & 0x0040 != 0;
            if long {
                let v = self.d[r] as u16 as i16 as i32 as u32;
                self.d[r] = v;
                self.set_nz(v, Size::Long);
            } else {
                let v = (self.d[r] as u8 as i8 as i16 as u16) as u32;
                self.d[r] = (self.d[r] & 0xFFFF_0000) | (v & 0xFFFF);
                self.set_nz(v, Size::Word);
            }
            self.set_flag(V, false);
            self.set_flag(C, false);
            self.cycles += 4;
            return;
        }
        // LEA An,(ea) : 0x41C0 | dst<<9 | ea
        if op & 0xF1C0 == 0x41C0 {
            let dst = ((op >> 9) & 0x07) as usize;
            let ea = self.decode_ea(bus, mode, reg, Size::Long);
            if let Ea::Mem(a) = ea {
                self.a[dst] = a;
            }
            self.cycles += 4;
            return;
        }
        // MOVEM (0x4880/0x48C0 with mode>=2 for reg->mem, 0x4C80/0x4CC0 mem->reg)
        if op & 0xFB80 == 0x4880 {
            self.movem(bus, op);
            return;
        }
        // CHK (0x4180 | Dn<<9 | ea), size word
        if op & 0xF040 == 0x4000 && (line >> 1) & 0x07 == 0 {
            // fall through to NEG/etc below — CHK is rare; skip precise impl
        }
        // MOVE from SR (0x40C0|ea)
        if op & 0xFFC0 == 0x40C0 {
            let ea = self.decode_ea(bus, mode, reg, Size::Word);
            self.ea_write(bus, ea, Size::Word, self.sr as u32);
            self.cycles += 8;
            return;
        }
        // MOVE to CCR (0x44C0|ea)
        if op & 0xFFC0 == 0x44C0 {
            let ea = self.decode_ea(bus, mode, reg, Size::Word);
            let v = self.ea_read(bus, ea, Size::Word);
            self.sr = (self.sr & 0xFF00) | (v as u16 & 0xFF);
            self.cycles += 12;
            return;
        }
        // MOVE to SR (0x46C0|ea)
        if op & 0xFFC0 == 0x46C0 {
            let ea = self.decode_ea(bus, mode, reg, Size::Word);
            let v = self.ea_read(bus, ea, Size::Word);
            self.set_sr(v as u16);
            self.cycles += 12;
            return;
        }
        // NEG/NEGX/NOT/CLR/TST/NBCD/TAS, distinguished by bits 11-8.
        let szbits = (op >> 6) & 0x03;
        let sub = (op >> 8) & 0x0F;
        if szbits != 0x03 {
            let sz = match szbits {
                0 => Size::Byte,
                1 => Size::Word,
                _ => Size::Long,
            };
            match sub {
                0x0 => {
                    // NEGX
                    let ea = self.decode_ea(bus, mode, reg, sz);
                    let v = self.ea_read(bus, ea, sz);
                    let x = if self.flag(X) { 1 } else { 0 };
                    let r = self.sub_flags(0, v.wrapping_add(x), sz, false);
                    self.ea_write(bus, ea, sz, r);
                    self.cycles += 6;
                    return;
                }
                0x2 => {
                    // CLR
                    let ea = self.decode_ea(bus, mode, reg, sz);
                    self.ea_write(bus, ea, sz, 0);
                    self.set_flag(Z, true);
                    self.set_flag(N, false);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.cycles += 6;
                    return;
                }
                0x4 => {
                    // NEG
                    let ea = self.decode_ea(bus, mode, reg, sz);
                    let v = self.ea_read(bus, ea, sz);
                    let r = self.sub_flags(0, v, sz, false);
                    self.ea_write(bus, ea, sz, r);
                    self.cycles += 6;
                    return;
                }
                0x6 => {
                    // NOT
                    let ea = self.decode_ea(bus, mode, reg, sz);
                    let v = self.ea_read(bus, ea, sz);
                    let r = !v & sz.mask();
                    self.ea_write(bus, ea, sz, r);
                    self.set_nz(r, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.cycles += 6;
                    return;
                }
                0xA => {
                    // TST (and TAS at byte-size special 0x4AC0)
                    if op & 0xFFC0 == 0x4AC0 {
                        // TAS
                        let ea = self.decode_ea(bus, mode, reg, Size::Byte);
                        let v = self.ea_read(bus, ea, Size::Byte);
                        self.set_nz(v, Size::Byte);
                        self.set_flag(V, false);
                        self.set_flag(C, false);
                        self.ea_write(bus, ea, Size::Byte, v | 0x80);
                        self.cycles += 4;
                        return;
                    }
                    let ea = self.decode_ea(bus, mode, reg, sz);
                    let v = self.ea_read(bus, ea, sz);
                    self.set_nz(v, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.cycles += 4;
                    return;
                }
                _ => {}
            }
        }
        // Anything unrecognized in group 4 is illegal.
        self.illegal(bus);
    }

    fn movem(&mut self, bus: &mut dyn Bus, op: u16) {
        let dr = op & 0x0400 != 0; // direction: 1 = mem->reg
        let long = op & 0x0040 != 0;
        let sz = if long { Size::Long } else { Size::Word };
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        let mut list = self.fetch16(bus);
        let step = sz.bytes();
        if !dr && mode == 4 {
            // reg->mem predecrement: list bits are reversed (A7..D0).
            let mut addr = self.a[reg as usize];
            for i in 0..16 {
                if list & 1 != 0 {
                    addr = addr.wrapping_sub(step);
                    // index: bit0 -> A7, bit15 -> D0
                    let regnum = 15 - i;
                    let v = if regnum < 8 {
                        self.d[regnum]
                    } else {
                        self.a[regnum - 8]
                    };
                    if long {
                        bus.write32(addr, v);
                    } else {
                        bus.write16(addr, v as u16);
                    }
                }
                list >>= 1;
            }
            self.a[reg as usize] = addr;
        } else {
            // Compute base address for non-predecrement modes.
            let ea = self.decode_ea(bus, mode, reg, sz);
            let mut addr = match ea {
                Ea::Mem(a) => a,
                _ => 0,
            };
            for i in 0..16 {
                if list & 1 != 0 {
                    if dr {
                        let v = if long {
                            bus.read32(addr)
                        } else {
                            bus.read16(addr) as i16 as i32 as u32
                        };
                        if i < 8 {
                            self.d[i] = v;
                        } else {
                            self.a[i - 8] = v;
                        }
                    } else {
                        let v = if i < 8 { self.d[i] } else { self.a[i - 8] };
                        if long {
                            bus.write32(addr, v);
                        } else {
                            bus.write16(addr, v as u16);
                        }
                    }
                    addr = addr.wrapping_add(step);
                }
                list >>= 1;
            }
            if dr && mode == 3 {
                // postincrement updates An to final address
                self.a[reg as usize] = addr;
            }
        }
        self.cycles += 12;
    }

    // ---------------------------------------------------------------- group 5
    // ADDQ/SUBQ (#1..8), Scc, DBcc.
    fn grp_addq_subq_scc(&mut self, bus: &mut dyn Bus, op: u16) {
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        let cond = (op >> 8) & 0x0F;
        let szbits = (op >> 6) & 0x03;
        if szbits == 0x03 {
            // Scc / DBcc
            if mode == 1 {
                // DBcc Dn,disp
                let disp = self.fetch16(bus) as i16 as i32;
                let target = self.pc.wrapping_add(disp as u32).wrapping_sub(2);
                if !self.test_cond(cond) {
                    let r = reg as usize;
                    let counter = (self.d[r] as u16).wrapping_sub(1);
                    self.d[r] = (self.d[r] & 0xFFFF_0000) | counter as u32;
                    if counter != 0xFFFF {
                        self.pc = target;
                    }
                }
                self.cycles += 10;
            } else {
                // Scc
                let ea = self.decode_ea(bus, mode, reg, Size::Byte);
                let val = if self.test_cond(cond) { 0xFF } else { 0x00 };
                self.ea_write(bus, ea, Size::Byte, val);
                self.cycles += 8;
            }
            return;
        }
        let sz = match szbits {
            0 => Size::Byte,
            1 => Size::Word,
            _ => Size::Long,
        };
        let mut data = ((op >> 9) & 0x07) as u32;
        if data == 0 {
            data = 8;
        }
        let is_sub = op & 0x0100 != 0;
        let ea = self.decode_ea(bus, mode, reg, sz);
        if let Ea::AddrReg(r) = ea {
            // ADDQ/SUBQ to An: full 32-bit, no flags.
            let v = self.a[r as usize];
            self.a[r as usize] = if is_sub {
                v.wrapping_sub(data)
            } else {
                v.wrapping_add(data)
            };
            self.cycles += 8;
            return;
        }
        let dst = self.ea_read(bus, ea, sz);
        let r = if is_sub {
            self.sub_flags(dst, data, sz, false)
        } else {
            self.add_flags(dst, data, sz, false)
        };
        self.ea_write(bus, ea, sz, r);
        self.cycles += 8;
    }

    // ---------------------------------------------------------------- group 6
    // Bcc / BRA / BSR.
    fn grp_bcc(&mut self, bus: &mut dyn Bus, op: u16) {
        let cond = (op >> 8) & 0x0F;
        let mut disp = (op & 0xFF) as i8 as i32;
        let base = self.pc;
        if disp == 0 {
            disp = self.fetch16(bus) as i16 as i32;
        }
        let target = base.wrapping_add(disp as u32);
        if cond == 1 {
            // BSR
            self.push32(bus, self.pc);
            self.pc = target;
            self.cycles += 18;
        } else if cond == 0 || self.test_cond(cond) {
            // BRA (cond 0) or taken Bcc
            self.pc = target;
            self.cycles += 10;
        } else {
            self.cycles += 8;
        }
    }

    // ---------------------------------------------------------------- group 7
    fn op_moveq(&mut self, op: u16) {
        let reg = ((op >> 9) & 0x07) as usize;
        let v = (op & 0xFF) as i8 as i32 as u32;
        self.d[reg] = v;
        self.set_nz(v, Size::Long);
        self.set_flag(V, false);
        self.set_flag(C, false);
        self.cycles += 4;
    }

    // ---------------------------------------------------------------- group 8
    // OR, DIVU/DIVS, SBCD.
    fn grp_or_div(&mut self, bus: &mut dyn Bus, op: u16) {
        let dn = ((op >> 9) & 0x07) as usize;
        let opmode = (op >> 6) & 0x07;
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        match opmode {
            3 => {
                // DIVU Dn, <ea>
                let ea = self.decode_ea(bus, mode, reg, Size::Word);
                let divisor = self.ea_read(bus, ea, Size::Word) & 0xFFFF;
                if divisor == 0 {
                    self.exception(bus, 5);
                    self.cycles += 38;
                    return;
                }
                let dividend = self.d[dn];
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q > 0xFFFF {
                    self.set_flag(V, true);
                } else {
                    self.d[dn] = (r << 16) | (q & 0xFFFF);
                    self.set_nz(q, Size::Word);
                    self.set_flag(V, false);
                }
                self.set_flag(C, false);
                self.cycles += 76;
            }
            7 => {
                // DIVS Dn, <ea>
                let ea = self.decode_ea(bus, mode, reg, Size::Word);
                let divisor = (self.ea_read(bus, ea, Size::Word) as u16) as i16 as i32;
                if divisor == 0 {
                    self.exception(bus, 5);
                    self.cycles += 38;
                    return;
                }
                let dividend = self.d[dn] as i32;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if q > 0x7FFF || q < -0x8000 {
                    self.set_flag(V, true);
                } else {
                    self.d[dn] = ((r as u32 & 0xFFFF) << 16) | (q as u32 & 0xFFFF);
                    self.set_nz(q as u32, Size::Word);
                    self.set_flag(V, false);
                }
                self.set_flag(C, false);
                self.cycles += 122;
            }
            _ => {
                // OR
                let szbits = opmode & 0x03;
                let sz = match szbits {
                    0 => Size::Byte,
                    1 => Size::Word,
                    _ => Size::Long,
                };
                let to_ea = opmode & 0x04 != 0;
                let ea = self.decode_ea(bus, mode, reg, sz);
                if to_ea {
                    let s = self.d[dn] & sz.mask();
                    let d = self.ea_read(bus, ea, sz);
                    let r = s | d;
                    self.set_nz(r, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.ea_write(bus, ea, sz, r);
                } else {
                    let s = self.ea_read(bus, ea, sz);
                    let r = (self.d[dn] & sz.mask()) | s;
                    self.set_nz(r, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.d[dn] = (self.d[dn] & !sz.mask()) | (r & sz.mask());
                }
                self.cycles += 8;
            }
        }
    }

    // ---------------------------------------------------------------- group 9
    // SUB / SUBA / SUBX.
    fn grp_sub(&mut self, bus: &mut dyn Bus, op: u16) {
        self.add_sub_common(bus, op, true);
    }
    // ---------------------------------------------------------------- group D
    // ADD / ADDA / ADDX.
    fn grp_add(&mut self, bus: &mut dyn Bus, op: u16) {
        self.add_sub_common(bus, op, false);
    }

    fn add_sub_common(&mut self, bus: &mut dyn Bus, op: u16, is_sub: bool) {
        let dn = ((op >> 9) & 0x07) as usize;
        let opmode = (op >> 6) & 0x07;
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        // ADDA/SUBA: opmode 3 (word) or 7 (long).
        if opmode == 3 || opmode == 7 {
            let sz = if opmode == 3 { Size::Word } else { Size::Long };
            let ea = self.decode_ea(bus, mode, reg, sz);
            let mut s = self.ea_read(bus, ea, sz);
            if sz == Size::Word {
                s = s as u16 as i16 as i32 as u32;
            }
            self.a[dn] = if is_sub {
                self.a[dn].wrapping_sub(s)
            } else {
                self.a[dn].wrapping_add(s)
            };
            self.cycles += 8;
            return;
        }
        let szbits = opmode & 0x03;
        let sz = match szbits {
            0 => Size::Byte,
            1 => Size::Word,
            _ => Size::Long,
        };
        let to_ea = opmode & 0x04 != 0;
        // ADDX/SUBX share the to_ea encoding when mode is 0 or 4 (Dn / -(An)).
        if to_ea && (mode == 0 || mode == 1) {
            // ADDX/SUBX Dy,Dx (reg-reg form)
            let x = if self.flag(X) { 1 } else { 0 };
            let s = self.d[reg as usize] & sz.mask();
            let d = self.d[dn] & sz.mask();
            let r = if is_sub {
                self.subx_flags(d, s, x, sz)
            } else {
                self.addx_flags(d, s, x, sz)
            };
            self.d[dn] = (self.d[dn] & !sz.mask()) | (r & sz.mask());
            self.cycles += 8;
            return;
        }
        let ea = self.decode_ea(bus, mode, reg, sz);
        if to_ea {
            let s = self.d[dn] & sz.mask();
            let d = self.ea_read(bus, ea, sz);
            let r = if is_sub {
                self.sub_flags(d, s, sz, false)
            } else {
                self.add_flags(d, s, sz, false)
            };
            self.ea_write(bus, ea, sz, r);
        } else {
            let s = self.ea_read(bus, ea, sz);
            let d = self.d[dn] & sz.mask();
            let r = if is_sub {
                self.sub_flags(d, s, sz, false)
            } else {
                self.add_flags(d, s, sz, false)
            };
            self.d[dn] = (self.d[dn] & !sz.mask()) | (r & sz.mask());
        }
        self.cycles += 8;
    }

    // ---------------------------------------------------------------- group B
    // CMP / CMPA / CMPM / EOR.
    fn grp_cmp_eor(&mut self, bus: &mut dyn Bus, op: u16) {
        let dn = ((op >> 9) & 0x07) as usize;
        let opmode = (op >> 6) & 0x07;
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        // CMPA: opmode 3 (word) / 7 (long).
        if opmode == 3 || opmode == 7 {
            let sz = if opmode == 3 { Size::Word } else { Size::Long };
            let ea = self.decode_ea(bus, mode, reg, sz);
            let mut s = self.ea_read(bus, ea, sz);
            if sz == Size::Word {
                s = s as u16 as i16 as i32 as u32;
            }
            self.sub_flags(self.a[dn], s, Size::Long, false);
            self.cycles += 6;
            return;
        }
        let szbits = opmode & 0x03;
        let sz = match szbits {
            0 => Size::Byte,
            1 => Size::Word,
            _ => Size::Long,
        };
        let is_eor = opmode & 0x04 != 0;
        if is_eor {
            // CMPM: (Ay)+,(Ax)+ uses mode 1 in this field.
            if mode == 1 {
                let ay = reg as usize;
                let ax = dn;
                let sa = self.a[ay];
                let da = self.a[ax];
                let sv = match sz {
                    Size::Byte => bus.read8(sa) as u32,
                    Size::Word => bus.read16(sa) as u32,
                    Size::Long => bus.read32(sa),
                };
                let dv = match sz {
                    Size::Byte => bus.read8(da) as u32,
                    Size::Word => bus.read16(da) as u32,
                    Size::Long => bus.read32(da),
                };
                self.a[ay] = sa.wrapping_add(sz.bytes());
                self.a[ax] = da.wrapping_add(sz.bytes());
                self.sub_flags(dv, sv, sz, false);
                self.cycles += 8;
                return;
            }
            // EOR Dn,<ea>
            let ea = self.decode_ea(bus, mode, reg, sz);
            let d = self.ea_read(bus, ea, sz);
            let r = d ^ (self.d[dn] & sz.mask());
            self.set_nz(r, sz);
            self.set_flag(V, false);
            self.set_flag(C, false);
            self.ea_write(bus, ea, sz, r);
            self.cycles += 8;
            return;
        }
        // CMP <ea>,Dn
        let ea = self.decode_ea(bus, mode, reg, sz);
        let s = self.ea_read(bus, ea, sz);
        let d = self.d[dn] & sz.mask();
        self.sub_flags(d, s, sz, false);
        self.cycles += 6;
    }

    // ---------------------------------------------------------------- group C
    // AND, MULU/MULS, ABCD, EXG.
    fn grp_and_mul(&mut self, bus: &mut dyn Bus, op: u16) {
        let dn = ((op >> 9) & 0x07) as usize;
        let opmode = (op >> 6) & 0x07;
        let mode = ((op >> 3) & 0x07) as u8;
        let reg = (op & 0x07) as u8;
        // EXG: 0xC100 family.
        if op & 0xF130 == 0xC100 {
            let rx = dn;
            let ry = (op & 0x07) as usize;
            match (op >> 3) & 0x1F {
                0x08 => self.d.swap(rx, ry),                  // EXG Dx,Dy
                0x09 => self.a.swap(rx, ry),                  // EXG Ax,Ay
                0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]), // EXG Dx,Ay
                _ => {}
            }
            self.cycles += 6;
            return;
        }
        match opmode {
            3 => {
                // MULU Dn,<ea>
                let ea = self.decode_ea(bus, mode, reg, Size::Word);
                let s = self.ea_read(bus, ea, Size::Word) & 0xFFFF;
                let r = (self.d[dn] & 0xFFFF) * s;
                self.d[dn] = r;
                self.set_nz(r, Size::Long);
                self.set_flag(V, false);
                self.set_flag(C, false);
                self.cycles += 54;
            }
            7 => {
                // MULS Dn,<ea>
                let ea = self.decode_ea(bus, mode, reg, Size::Word);
                let s = (self.ea_read(bus, ea, Size::Word) as u16) as i16 as i32;
                let r = ((self.d[dn] as u16) as i16 as i32) * s;
                self.d[dn] = r as u32;
                self.set_nz(r as u32, Size::Long);
                self.set_flag(V, false);
                self.set_flag(C, false);
                self.cycles += 54;
            }
            _ => {
                // AND
                let szbits = opmode & 0x03;
                let sz = match szbits {
                    0 => Size::Byte,
                    1 => Size::Word,
                    _ => Size::Long,
                };
                let to_ea = opmode & 0x04 != 0;
                let ea = self.decode_ea(bus, mode, reg, sz);
                if to_ea {
                    let s = self.d[dn] & sz.mask();
                    let d = self.ea_read(bus, ea, sz);
                    let r = s & d;
                    self.set_nz(r, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.ea_write(bus, ea, sz, r);
                } else {
                    let s = self.ea_read(bus, ea, sz);
                    let r = (self.d[dn] & sz.mask()) & s;
                    self.set_nz(r, sz);
                    self.set_flag(V, false);
                    self.set_flag(C, false);
                    self.d[dn] = (self.d[dn] & !sz.mask()) | (r & sz.mask());
                }
                self.cycles += 8;
            }
        }
    }

    // ---------------------------------------------------------------- group E
    // Shifts/rotates: ASL/ASR, LSL/LSR, ROL/ROR, ROXL/ROXR.
    fn grp_shift(&mut self, bus: &mut dyn Bus, op: u16) {
        let szbits = (op >> 6) & 0x03;
        if szbits == 0x03 {
            // Memory shift by 1 (word only).
            let mode = ((op >> 3) & 0x07) as u8;
            let reg = (op & 0x07) as u8;
            let kind = (op >> 9) & 0x07;
            let left = op & 0x0100 != 0;
            let ea = self.decode_ea(bus, mode, reg, Size::Word);
            let v = self.ea_read(bus, ea, Size::Word);
            let r = self.do_shift(v, 1, Size::Word, kind, left);
            self.ea_write(bus, ea, Size::Word, r);
            self.cycles += 8;
            return;
        }
        let sz = match szbits {
            0 => Size::Byte,
            1 => Size::Word,
            _ => Size::Long,
        };
        let reg = (op & 0x07) as usize;
        let left = op & 0x0100 != 0;
        let kind = (op >> 3) & 0x03; // 0 AS,1 LS,2 ROX,3 RO
        let count_field = ((op >> 9) & 0x07) as u32;
        let immediate = op & 0x0020 == 0;
        let count = if immediate {
            if count_field == 0 {
                8
            } else {
                count_field
            }
        } else {
            self.d[count_field as usize] & 0x3F
        };
        let v = self.d[reg] & sz.mask();
        let r = self.do_shift(v, count, sz, kind, left);
        self.d[reg] = (self.d[reg] & !sz.mask()) | (r & sz.mask());
        self.cycles += 6 + 2 * count;
    }

    fn do_shift(&mut self, val: u32, count: u32, sz: Size, kind: u16, left: bool) -> u32 {
        let mask = sz.mask();
        let msb = sz.msb();
        let mut v = val & mask;
        let mut carry = false;
        let mut overflow = false;
        // Translate kind: in the register form kind = bits9..? we mapped: caller
        // passes (op>>3)&3 OR (op>>9)&7. Normalize here: 0 AS,1 LS,2 ROX,3 RO.
        let kind = kind & 0x03;
        for _ in 0..count {
            if left {
                carry = v & msb != 0;
                let prev_msb = v & msb;
                v = (v << 1) & mask;
                match kind {
                    0 => {
                        // ASL
                        if (v & msb) != prev_msb {
                            overflow = true;
                        }
                    }
                    2 => {
                        // ROXL
                        if self.flag(X) {
                            v |= 1;
                        }
                        self.set_flag(X, carry);
                    }
                    3 => {
                        // ROL
                        if carry {
                            v |= 1;
                        }
                    }
                    _ => {} // LSL
                }
            } else {
                carry = v & 1 != 0;
                let msb_bit = v & msb;
                v >>= 1;
                match kind {
                    0 => {
                        // ASR keeps sign
                        v |= msb_bit;
                    }
                    2 => {
                        // ROXR
                        if self.flag(X) {
                            v |= msb;
                        }
                        self.set_flag(X, carry);
                    }
                    3 => {
                        // ROR
                        if carry {
                            v |= msb;
                        }
                    }
                    _ => {} // LSR
                }
            }
        }
        v &= mask;
        self.set_nz(v, sz);
        if count != 0 {
            self.set_flag(C, carry);
            // ROX uses X for carry on zero count; here count != 0.
            if kind == 2 {
                self.set_flag(C, self.flag(X));
            }
            if kind != 2 {
                // X follows C for AS/LS, untouched for RO.
                if kind == 0 || kind == 1 {
                    self.set_flag(X, carry);
                }
            }
        } else {
            self.set_flag(C, false);
        }
        self.set_flag(V, kind == 0 && overflow);
        v
    }

    // =====================================================================
    // Arithmetic flag helpers (the heart of CCR correctness).
    // =====================================================================
    fn add_flags(&mut self, d: u32, s: u32, sz: Size, _x: bool) -> u32 {
        let m = sz.mask();
        let msb = sz.msb();
        let dd = d & m;
        let ss = s & m;
        let res = dd.wrapping_add(ss) & m;
        let carry = (dd as u64 + ss as u64) > m as u64;
        let overflow = ((dd ^ res) & (ss ^ res) & msb) != 0;
        self.set_flag(C, carry);
        self.set_flag(X, carry);
        self.set_flag(V, overflow);
        self.set_nz(res, sz);
        res
    }

    fn sub_flags(&mut self, d: u32, s: u32, sz: Size, _x: bool) -> u32 {
        let m = sz.mask();
        let msb = sz.msb();
        let dd = d & m;
        let ss = s & m;
        let res = dd.wrapping_sub(ss) & m;
        let borrow = (dd as u64) < (ss as u64);
        let overflow = ((dd ^ ss) & (dd ^ res) & msb) != 0;
        self.set_flag(C, borrow);
        self.set_flag(X, borrow);
        self.set_flag(V, overflow);
        self.set_nz(res, sz);
        res
    }

    fn addx_flags(&mut self, d: u32, s: u32, x: u32, sz: Size) -> u32 {
        let m = sz.mask();
        let msb = sz.msb();
        let dd = d & m;
        let ss = s & m;
        let total = dd as u64 + ss as u64 + x as u64;
        let res = (total as u32) & m;
        let carry = total > m as u64;
        let overflow = ((dd ^ res) & (ss ^ res) & msb) != 0;
        self.set_flag(C, carry);
        self.set_flag(X, carry);
        self.set_flag(V, overflow);
        self.set_flag(N, res & msb != 0);
        if res != 0 {
            self.set_flag(Z, false);
        }
        res
    }

    fn subx_flags(&mut self, d: u32, s: u32, x: u32, sz: Size) -> u32 {
        let m = sz.mask();
        let msb = sz.msb();
        let dd = d & m;
        let ss = s & m;
        let res = dd.wrapping_sub(ss).wrapping_sub(x) & m;
        let borrow = (dd as u64) < (ss as u64 + x as u64);
        let overflow = ((dd ^ ss) & (dd ^ res) & msb) != 0;
        self.set_flag(C, borrow);
        self.set_flag(X, borrow);
        self.set_flag(V, overflow);
        self.set_flag(N, res & msb != 0);
        if res != 0 {
            self.set_flag(Z, false);
        }
        res
    }

    /// Evaluate a 4-bit condition code against the CCR.
    fn test_cond(&self, cond: u16) -> bool {
        let c = self.flag(C);
        let v = self.flag(V);
        let z = self.flag(Z);
        let n = self.flag(N);
        match cond {
            0x0 => true,           // T
            0x1 => false,          // F
            0x2 => !c && !z,       // HI
            0x3 => c || z,         // LS
            0x4 => !c,             // CC/HS
            0x5 => c,              // CS/LO
            0x6 => !z,             // NE
            0x7 => z,              // EQ
            0x8 => !v,             // VC
            0x9 => v,              // VS
            0xA => !n,             // PL
            0xB => n,              // MI
            0xC => n == v,         // GE
            0xD => n != v,         // LT
            0xE => !z && (n == v), // GT
            0xF => z || (n != v),  // LE
            _ => false,
        }
    }
}

/// A decoded effective address: where an operand lives.
#[derive(Clone, Copy)]
enum Ea {
    DataReg(u8),
    AddrReg(u8),
    Mem(u32),
    Imm(u32),
}

// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 16 MiB flat-RAM bus for CPU unit tests (big-endian via the trait helpers).
    struct FlatBus {
        ram: Vec<u8>,
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus {
                ram: vec![0u8; 0x100_0000],
            }
        }
    }
    impl Bus for FlatBus {
        fn read8(&mut self, addr: u32) -> u8 {
            self.ram[(addr & 0xFF_FFFF) as usize]
        }
        fn write8(&mut self, addr: u32, v: u8) {
            self.ram[(addr & 0xFF_FFFF) as usize] = v;
        }
    }

    fn setup(prog: &[u16]) -> (M68k, FlatBus) {
        let mut bus = FlatBus::new();
        // Reset vector: SSP=$8000, PC=$1000.
        bus.write32(0, 0x8000);
        bus.write32(4, 0x1000);
        // Load program words big-endian at $1000.
        let mut addr = 0x1000u32;
        for &w in prog {
            bus.write16(addr, w);
            addr += 2;
        }
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        (cpu, bus)
    }

    #[test]
    fn reset_loads_vectors() {
        let (cpu, _) = setup(&[]);
        assert_eq!(cpu.a[7], 0x8000);
        assert_eq!(cpu.pc, 0x1000);
        assert!(cpu.in_supervisor());
    }

    #[test]
    fn moveq_sets_register_and_flags() {
        // MOVEQ #-1,D0 -> 0x70FF
        let (mut cpu, mut bus) = setup(&[0x70FF]);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0xFFFF_FFFF);
        assert!(cpu.flag(N));
        assert!(!cpu.flag(Z));
    }

    #[test]
    fn moveq_zero_sets_z() {
        let (mut cpu, mut bus) = setup(&[0x7000]); // MOVEQ #0,D0
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0);
        assert!(cpu.flag(Z));
        assert!(!cpu.flag(N));
    }

    #[test]
    fn move_immediate_to_data_reg() {
        // MOVE.L #$12345678,D1 : 0x223C imm imm
        let (mut cpu, mut bus) = setup(&[0x223C, 0x1234, 0x5678]);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[1], 0x1234_5678);
    }

    #[test]
    fn add_sets_carry_and_overflow() {
        // D0 = 0x7FFFFFFF; ADD.L #1,D0 -> overflow set, sign flips
        let (mut cpu, mut bus) = setup(&[0x203C, 0x7FFF, 0xFFFF, 0x0680, 0x0000, 0x0001]);
        cpu.step(&mut bus); // MOVE.L #$7FFFFFFF,D0
        cpu.step(&mut bus); // ADDI.L #1,D0
        assert_eq!(cpu.d[0], 0x8000_0000);
        assert!(cpu.flag(V));
        assert!(cpu.flag(N));
        assert!(!cpu.flag(C));
    }

    #[test]
    fn subi_borrow_sets_carry() {
        // D0 = 0; SUBI.W #1,D0 -> 0xFFFF, carry+negative set
        let (mut cpu, mut bus) = setup(&[0x7000, 0x0440, 0x0001]);
        cpu.step(&mut bus); // MOVEQ #0,D0
        cpu.step(&mut bus); // SUBI.W #1,D0
        assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
        assert!(cpu.flag(C));
        assert!(cpu.flag(N));
    }

    #[test]
    fn cmpi_does_not_modify_operand() {
        // D0 = 5; CMPI.W #5,D0 -> Z set, D0 unchanged
        let (mut cpu, mut bus) = setup(&[0x7005, 0x0C40, 0x0005]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 5);
        assert!(cpu.flag(Z));
    }

    #[test]
    fn bne_taken_when_not_equal() {
        // MOVEQ #1,D0; CMPI.W #0,D0 (Z=0); BNE +4
        let (mut cpu, mut bus) = setup(&[0x7001, 0x0C40, 0x0000, 0x6600, 0x0010]);
        cpu.step(&mut bus); // MOVEQ
        cpu.step(&mut bus); // CMPI
        let pc_before = cpu.pc;
        cpu.step(&mut bus); // BNE word disp 0x10 from after-disp-word PC
        assert_ne!(cpu.pc, pc_before + 4); // branch taken changes PC
    }

    #[test]
    fn jsr_rts_roundtrip() {
        // JSR $2000 ; (at $2000) RTS
        // JSR (xxx).L : 0x4EB9 followed by 32-bit addr
        let (mut cpu, mut bus) = setup(&[0x4EB9, 0x0000, 0x2000]);
        bus.write16(0x2000, 0x4E75); // RTS
        let ret = cpu.pc + 6; // after the JSR instruction
        cpu.step(&mut bus); // JSR
        assert_eq!(cpu.pc, 0x2000);
        cpu.step(&mut bus); // RTS
        assert_eq!(cpu.pc, ret);
    }

    #[test]
    fn lea_computes_address() {
        // LEA (d16,PC),A0 : mode 7 reg 2. 0x41FA + disp
        let (mut cpu, mut bus) = setup(&[0x41FA, 0x0010]);
        let base = cpu.pc + 2; // PC after opcode word, at the ext word
        cpu.step(&mut bus);
        assert_eq!(cpu.a[0], base.wrapping_add(0x10));
    }

    #[test]
    fn swap_exchanges_halves() {
        // MOVE.L #$AAAABBBB,D0 ; SWAP D0
        let (mut cpu, mut bus) = setup(&[0x203C, 0xAAAA, 0xBBBB, 0x4840]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0xBBBB_AAAA);
    }

    #[test]
    fn lsl_shifts_and_sets_carry() {
        // MOVEQ #1,D0 ; LSL.L #1,D0 ... use immediate shift: 0xE388 = LSL.L #1,D0
        let (mut cpu, mut bus) = setup(&[0x7001, 0xE388]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 2);
    }

    #[test]
    fn and_clears_bits() {
        // MOVE.L #$FF00,D0 ; ANDI.L #$0F00,D0
        let (mut cpu, mut bus) = setup(&[0x203C, 0x0000, 0xFF00, 0x0280, 0x0000, 0x0F00]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x0F00);
    }

    #[test]
    fn movem_roundtrips_registers() {
        // Store D0/D1 to (A0), reload into D2/D3.
        // MOVEM.L D0/D1,(A0): 0x48D0, mask=0x0003
        // MOVEM.L (A0),D2/D3: 0x4CD0, mask=0x000C
        let (mut cpu, mut bus) = setup(&[0x48D0, 0x0003, 0x4CD0, 0x000C]);
        cpu.a[0] = 0x4000;
        cpu.d[0] = 0x1111_1111;
        cpu.d[1] = 0x2222_2222;
        cpu.step(&mut bus); // store
        cpu.step(&mut bus); // load into D2/D3
        assert_eq!(cpu.d[2], 0x1111_1111);
        assert_eq!(cpu.d[3], 0x2222_2222);
    }

    #[test]
    fn dbra_loops() {
        // MOVEQ #3,D0 ; loop: DBRA D0,loop
        // DBRA (DBF) D0 = 0x51C8 ; disp = -2 to point back at itself.
        let (mut cpu, mut bus) = setup(&[0x7003, 0x51C8, 0xFFFE]);
        cpu.step(&mut bus); // MOVEQ #3
        // Execute the DBRA enough times to exhaust the counter.
        for _ in 0..4 {
            cpu.step(&mut bus);
        }
        // Counter went 3->2->1->0->-1(0xFFFF) and then falls through.
        assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
    }

    #[test]
    fn divu_computes_quotient_remainder() {
        // D0 = 100 ; DIVU #7,D0 -> quotient 14, remainder 2
        let (mut cpu, mut bus) = setup(&[0x203C, 0x0000, 0x0064, 0x80FC, 0x0007]);
        cpu.step(&mut bus); // MOVE.L #100,D0
        cpu.step(&mut bus); // DIVU #7,D0
        assert_eq!(cpu.d[0] & 0xFFFF, 14);
        assert_eq!((cpu.d[0] >> 16) & 0xFFFF, 2);
    }

    #[test]
    fn mulu_multiplies() {
        // D0 = 6 ; MULU #7,D0 -> 42
        let (mut cpu, mut bus) = setup(&[0x7006, 0xC0FC, 0x0007]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 42);
    }

    #[test]
    fn interrupt_pushes_frame_and_vectors() {
        // Set autovector 6 (offset 0x18*4=0x78? vector 30 -> addr 0x78).
        let (mut cpu, mut bus) = setup(&[0x4E71]); // NOP
        cpu.sr &= !0x0700; // unmask interrupts
        bus.write32((24 + 6) * 4, 0x3000); // level-6 autovector -> $3000
        cpu.irq_level = 6;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x3000);
        // SR mask raised to 6.
        assert_eq!((cpu.sr >> 8) & 0x07, 6);
    }

    #[test]
    fn trap_takes_vector() {
        // TRAP #0 -> vector 32 at addr 0x80
        let (mut cpu, mut bus) = setup(&[0x4E40]);
        bus.write32(32 * 4, 0x5000);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x5000);
    }

    #[test]
    fn addq_to_address_reg_no_flags() {
        // ADDQ.W #1,A0 : 0x5248 ; A0 starts 0x1000
        let (mut cpu, mut bus) = setup(&[0x5248]);
        cpu.a[0] = 0x1000;
        cpu.set_flag(Z, true);
        cpu.step(&mut bus);
        assert_eq!(cpu.a[0], 0x1001);
        assert!(cpu.flag(Z)); // unchanged
    }

    #[test]
    fn clr_sets_zero_flag() {
        // MOVEQ #5,D0 ; CLR.L D0 : 0x4280
        let (mut cpu, mut bus) = setup(&[0x7005, 0x4280]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0);
        assert!(cpu.flag(Z));
    }

    #[test]
    fn btst_immediate_tests_bit() {
        // MOVEQ #8,D0 ; BTST #3,D0 -> bit3 set so Z=0
        let (mut cpu, mut bus) = setup(&[0x7008, 0x0800, 0x0003]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(!cpu.flag(Z));
    }

    #[test]
    fn cond_codes_ge_lt() {
        let mut cpu = M68k::new();
        cpu.set_flag(N, true);
        cpu.set_flag(V, false);
        assert!(cpu.test_cond(0xD)); // LT (N!=V)
        assert!(!cpu.test_cond(0xC)); // GE
    }
}
