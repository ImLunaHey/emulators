//! WDC 65C816 CPU core (the processor inside the Ricoh 5A22).
//!
//! Spec sources: the WDC 65C816 datasheet, the "Programming the 65816" book's
//! opcode/addressing tables, anomie's "65816 opcodes" reference, and fullsnes.
//!
//! The 65816 is a 16-bit superset of the 6502. Key features implemented here:
//! - **Emulation mode (E=1)** behaves like a 6502 (8-bit A/X/Y, page-1 stack);
//!   **native mode (E=0)** unlocks 16-bit registers and 24-bit addressing.
//! - The **M flag** selects 8- vs 16-bit accumulator/memory; the **X flag**
//!   selects 8- vs 16-bit index registers (X/Y). Both only apply in native mode;
//!   E=1 forces M=X=1.
//! - 24-bit address space via the **DBR** (data bank) and **PBR** (program bank)
//!   registers and the 16-bit **D** (direct page) register.
//! - Full documented instruction set, all addressing modes, decimal mode (ADC/
//!   SBC), and the RESET/NMI/IRQ/ABORT/BRK/COP vectors.
//!
//! Timing: `step()` returns an approximate cycle count derived from the base
//! cycle table plus the usual penalties (16-bit access, page cross, branch
//! taken, direct-page low byte nonzero, native-mode interrupt). The orchestrator
//! uses this to pace the PPU/APU. It is not master-clock exact.
//!
//! The CPU drives memory through `&mut dyn Bus` (see `crate::bus::Bus`).

use crate::bus::Bus;

// Processor status flags (P register).
pub const FLAG_C: u8 = 1 << 0; // carry
pub const FLAG_Z: u8 = 1 << 1; // zero
pub const FLAG_I: u8 = 1 << 2; // IRQ disable
pub const FLAG_D: u8 = 1 << 3; // decimal
pub const FLAG_X: u8 = 1 << 4; // index width (1 = 8-bit) — "B" break flag in emulation
pub const FLAG_M: u8 = 1 << 5; // accumulator/memory width (1 = 8-bit)
pub const FLAG_V: u8 = 1 << 6; // overflow
pub const FLAG_N: u8 = 1 << 7; // negative

const NATIVE_COP_VEC: u32 = 0xFFE4;
const NATIVE_BRK_VEC: u32 = 0xFFE6;
#[allow(dead_code)] // ABORT is not driven by any SNES hardware line; kept for the map.
const NATIVE_ABORT_VEC: u32 = 0xFFE8;
const NATIVE_NMI_VEC: u32 = 0xFFEA;
const NATIVE_IRQ_VEC: u32 = 0xFFEE;

const EMU_COP_VEC: u32 = 0xFFF4;
#[allow(dead_code)]
const EMU_ABORT_VEC: u32 = 0xFFF8;
const EMU_NMI_VEC: u32 = 0xFFFA;
const RESET_VEC: u32 = 0xFFFC;
const EMU_IRQ_BRK_VEC: u32 = 0xFFFE;

pub struct Cpu {
    pub a: u16,  // accumulator (C = full 16-bit; A = low 8)
    pub x: u16,  // index X
    pub y: u16,  // index Y
    pub sp: u16, // stack pointer
    pub d: u16,  // direct page register
    pub pc: u16, // program counter (within bank PBR)
    pub pbr: u8, // program bank
    pub dbr: u8, // data bank
    pub p: u8,   // processor status
    pub e: bool, // emulation mode flag

    /// Interrupt lines. NMI is edge (set by the orchestrator); IRQ is level.
    pub nmi_pending: bool,
    pub irq_line: bool,

    /// True while the CPU is stopped (STP) or waiting (WAI). STP can only be
    /// cleared by RESET; WAI by an interrupt.
    pub stopped: bool,
    pub waiting: bool,

    /// Latched fault for the crash screen: STP executed (the CPU is dead).
    pub fault: Option<(u8, u16)>,

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
            sp: 0x01FF,
            d: 0,
            pc: 0,
            pbr: 0,
            dbr: 0,
            p: FLAG_M | FLAG_X | FLAG_I,
            e: true,
            nmi_pending: false,
            irq_line: false,
            stopped: false,
            waiting: false,
            fault: None,
            cycles: 0,
        }
    }

    // ---- flag width helpers ----
    #[inline]
    pub fn m8(&self) -> bool {
        self.e || self.p & FLAG_M != 0
    }
    #[inline]
    pub fn x8(&self) -> bool {
        self.e || self.p & FLAG_X != 0
    }

    #[inline]
    fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.p |= f;
        } else {
            self.p &= !f;
        }
    }

    /// Set N/Z from an 8- or 16-bit value.
    #[inline]
    fn set_nz(&mut self, v: u16, eight: bool) {
        if eight {
            self.set_flag(FLAG_Z, v as u8 == 0);
            self.set_flag(FLAG_N, v & 0x80 != 0);
        } else {
            self.set_flag(FLAG_Z, v == 0);
            self.set_flag(FLAG_N, v & 0x8000 != 0);
        }
    }

    /// RESET: load PC from $00FFFC, force emulation mode and the boot flags.
    pub fn reset(&mut self, bus: &mut dyn Bus) {
        self.e = true;
        self.p = FLAG_M | FLAG_X | FLAG_I;
        self.d = 0;
        self.dbr = 0;
        self.pbr = 0;
        self.sp = 0x01FF;
        self.x &= 0xFF;
        self.y &= 0xFF;
        self.stopped = false;
        self.waiting = false;
        self.fault = None;
        let lo = bus.read8(RESET_VEC) as u16;
        let hi = bus.read8(RESET_VEC + 1) as u16;
        self.pc = (hi << 8) | lo;
    }

    // ---- low-level memory access (banked) ----
    #[inline]
    fn full(bank: u8, addr: u16) -> u32 {
        ((bank as u32) << 16) | addr as u32
    }

    /// Fetch the next opcode/operand byte from the program bank, advancing PC.
    #[inline]
    fn fetch8(&mut self, bus: &mut dyn Bus) -> u8 {
        let a = Self::full(self.pbr, self.pc);
        let v = bus.read8(a);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    #[inline]
    fn fetch16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        (hi << 8) | lo
    }

    // Data reads/writes honoring bank wrapping for 16-bit operands.
    #[inline]
    fn read8_at(&mut self, bus: &mut dyn Bus, bank: u8, addr: u16) -> u8 {
        bus.read8(Self::full(bank, addr))
    }
    #[inline]
    fn write8_at(&mut self, bus: &mut dyn Bus, bank: u8, addr: u16, v: u8) {
        bus.write8(Self::full(bank, addr), v);
    }

    // ---- stack ----
    #[inline]
    fn push8(&mut self, bus: &mut dyn Bus, v: u8) {
        bus.write8(self.sp as u32, v);
        if self.e {
            // Stack confined to page 1 in emulation mode.
            self.sp = 0x0100 | (self.sp.wrapping_sub(1) & 0xFF);
        } else {
            self.sp = self.sp.wrapping_sub(1);
        }
    }
    #[inline]
    fn pull8(&mut self, bus: &mut dyn Bus) -> u8 {
        if self.e {
            self.sp = 0x0100 | (self.sp.wrapping_add(1) & 0xFF);
        } else {
            self.sp = self.sp.wrapping_add(1);
        }
        bus.read8(self.sp as u32)
    }
    #[inline]
    fn push16(&mut self, bus: &mut dyn Bus, v: u16) {
        self.push8(bus, (v >> 8) as u8);
        self.push8(bus, (v & 0xFF) as u8);
    }
    #[inline]
    fn pull16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.pull8(bus) as u16;
        let hi = self.pull8(bus) as u16;
        (hi << 8) | lo
    }

    /// Execute one instruction (or service a pending interrupt). Returns the
    /// approximate number of CPU cycles consumed.
    pub fn step(&mut self, bus: &mut dyn Bus) -> u32 {
        if self.stopped {
            return 1;
        }

        // Interrupt sampling.
        if self.nmi_pending {
            self.nmi_pending = false;
            self.waiting = false;
            return self.interrupt(bus, false);
        }
        if self.irq_line && self.p & FLAG_I == 0 {
            self.waiting = false;
            return self.interrupt(bus, true);
        }
        if self.waiting {
            // WAI with interrupts masked but a line asserted still wakes; but if
            // no line is pending we idle one cycle.
            if self.irq_line || self.nmi_pending {
                self.waiting = false;
            } else {
                return 1;
            }
        }

        let start = self.cycles;
        let op = self.fetch8(bus);
        self.execute(bus, op);
        // execute() bumps self.cycles; return the delta (min 1).
        (self.cycles - start).max(1) as u32
    }

    fn interrupt(&mut self, bus: &mut dyn Bus, irq: bool) -> u32 {
        let vec = if self.e {
            self.push16(bus, self.pc);
            // Push P with B clear (hardware interrupt).
            self.push8(bus, self.p & !FLAG_X);
            if irq {
                EMU_IRQ_BRK_VEC
            } else {
                EMU_NMI_VEC
            }
        } else {
            self.push8(bus, self.pbr);
            self.push16(bus, self.pc);
            self.push8(bus, self.p);
            if irq {
                NATIVE_IRQ_VEC
            } else {
                NATIVE_NMI_VEC
            }
        };
        self.p |= FLAG_I;
        self.p &= !FLAG_D;
        self.pbr = 0;
        let lo = bus.read8(vec) as u16;
        let hi = bus.read8(vec + 1) as u16;
        self.pc = (hi << 8) | lo;
        self.cycles += 7;
        7
    }

    // =========================================================================
    // Addressing modes. Each returns the effective (bank, addr) for the operand.
    // Cycle penalties for page-cross / direct-page-low are added by the caller
    // via `add_cycles`-style bumps inside these helpers where appropriate.
    // =========================================================================

    /// Direct page: D + dp. Low-byte penalty handled by caller (D low != 0).
    fn am_direct(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let dp = self.fetch8(bus) as u16;
        if self.e && (self.d & 0xFF) == 0 {
            // 6502-style: stays within page set by D high byte.
            (0, (self.d & 0xFF00) | (dp & 0xFF))
        } else {
            (0, self.d.wrapping_add(dp))
        }
    }
    fn am_direct_x(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let dp = self.fetch8(bus) as u16;
        if self.e && (self.d & 0xFF) == 0 {
            (0, (self.d & 0xFF00) | (dp.wrapping_add(self.x) & 0xFF))
        } else {
            (0, self.d.wrapping_add(dp).wrapping_add(self.x))
        }
    }
    fn am_direct_y(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let dp = self.fetch8(bus) as u16;
        if self.e && (self.d & 0xFF) == 0 {
            (0, (self.d & 0xFF00) | (dp.wrapping_add(self.y) & 0xFF))
        } else {
            (0, self.d.wrapping_add(dp).wrapping_add(self.y))
        }
    }
    /// (dp) -> 16-bit pointer in DBR.
    fn am_indirect(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let (_, ptr) = self.am_direct(bus);
        let lo = self.read8_at(bus, 0, ptr) as u16;
        let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
        (self.dbr, (hi << 8) | lo)
    }
    /// [dp] -> 24-bit pointer.
    fn am_indirect_long(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let (_, ptr) = self.am_direct(bus);
        let lo = self.read8_at(bus, 0, ptr) as u16;
        let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
        let bank = self.read8_at(bus, 0, ptr.wrapping_add(2));
        (bank, (hi << 8) | lo)
    }
    /// (dp,X) -> pointer in DBR.
    fn am_indirect_x(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let (_, ptr) = self.am_direct_x(bus);
        let lo = self.read8_at(bus, 0, ptr) as u16;
        let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
        (self.dbr, (hi << 8) | lo)
    }
    /// (dp),Y -> DBR pointer + Y (page-cross penalty when applicable).
    fn am_indirect_y(&mut self, bus: &mut dyn Bus, pen: &mut u32) -> (u8, u16) {
        let (_, ptr) = self.am_direct(bus);
        let lo = self.read8_at(bus, 0, ptr) as u16;
        let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
        let base = (hi << 8) | lo;
        let eff = base.wrapping_add(self.y);
        if (base & 0xFF00) != (eff & 0xFF00) {
            *pen += 1;
        }
        // crossing into next bank.
        let bank = if eff < base { self.dbr.wrapping_add(1) } else { self.dbr };
        (bank, eff)
    }
    /// [dp],Y -> 24-bit pointer + Y.
    fn am_indirect_long_y(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let (bank, base) = self.am_indirect_long(bus);
        let eff = base.wrapping_add(self.y);
        let bank = if eff < base { bank.wrapping_add(1) } else { bank };
        (bank, eff)
    }
    /// Absolute -> DBR:addr.
    fn am_absolute(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let a = self.fetch16(bus);
        (self.dbr, a)
    }
    fn am_absolute_x(&mut self, bus: &mut dyn Bus, pen: &mut u32) -> (u8, u16) {
        let base = self.fetch16(bus);
        let eff = base.wrapping_add(self.x);
        if (base & 0xFF00) != (eff & 0xFF00) {
            *pen += 1;
        }
        let bank = if eff < base { self.dbr.wrapping_add(1) } else { self.dbr };
        (bank, eff)
    }
    fn am_absolute_y(&mut self, bus: &mut dyn Bus, pen: &mut u32) -> (u8, u16) {
        let base = self.fetch16(bus);
        let eff = base.wrapping_add(self.y);
        if (base & 0xFF00) != (eff & 0xFF00) {
            *pen += 1;
        }
        let bank = if eff < base { self.dbr.wrapping_add(1) } else { self.dbr };
        (bank, eff)
    }
    /// Absolute long -> bank:addr.
    fn am_long(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let a = self.fetch16(bus);
        let bank = self.fetch8(bus);
        (bank, a)
    }
    fn am_long_x(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let base = self.fetch16(bus);
        let bank = self.fetch8(bus);
        let eff = base.wrapping_add(self.x);
        let bank = if eff < base { bank.wrapping_add(1) } else { bank };
        (bank, eff)
    }
    /// Stack relative: sp + offset.
    fn am_stack_rel(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let off = self.fetch8(bus) as u16;
        (0, self.sp.wrapping_add(off))
    }
    /// (sr,S),Y.
    fn am_stack_rel_y(&mut self, bus: &mut dyn Bus) -> (u8, u16) {
        let off = self.fetch8(bus) as u16;
        let ptr = self.sp.wrapping_add(off);
        let lo = self.read8_at(bus, 0, ptr) as u16;
        let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
        let base = (hi << 8) | lo;
        let eff = base.wrapping_add(self.y);
        let bank = if eff < base { self.dbr.wrapping_add(1) } else { self.dbr };
        (bank, eff)
    }

    // ---- operand value read/write honoring M/X width ----
    fn read_m(&mut self, bus: &mut dyn Bus, ea: (u8, u16)) -> u16 {
        if self.m8() {
            self.read8_at(bus, ea.0, ea.1) as u16
        } else {
            let lo = self.read8_at(bus, ea.0, ea.1) as u16;
            let hi = self.read8_at(bus, ea.0, ea.1.wrapping_add(1)) as u16;
            self.cycles += 1;
            (hi << 8) | lo
        }
    }
    fn write_m(&mut self, bus: &mut dyn Bus, ea: (u8, u16), v: u16) {
        if self.m8() {
            self.write8_at(bus, ea.0, ea.1, v as u8);
        } else {
            self.write8_at(bus, ea.0, ea.1, v as u8);
            self.write8_at(bus, ea.0, ea.1.wrapping_add(1), (v >> 8) as u8);
            self.cycles += 1;
        }
    }

    // direct-page low-byte nonzero penalty.
    #[inline]
    fn dp_penalty(&self) -> u32 {
        if self.d & 0xFF != 0 {
            1
        } else {
            0
        }
    }
}

// =============================================================================
// Instruction execution. The 65816 opcode map is dense and regular; we dispatch
// on the opcode byte and use the addressing helpers above. Cycle counts start
// from a base table and accumulate width/page penalties.
// =============================================================================
impl Cpu {
    fn execute(&mut self, bus: &mut dyn Bus, op: u8) {
        let mut pen: u32 = 0;
        // Base cycle count for this opcode (approximate; penalties added below).
        let base = BASE_CYCLES[op as usize] as u32;

        match op {
            // ---- LDA ----
            0xA9 => { let v = self.imm_m(bus); self.lda(v); }
            0xA5 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xB5 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xAD => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.lda(v); }
            0xBD => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.lda(v); }
            0xB9 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.lda(v); }
            0xAF => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.lda(v); }
            0xBF => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.lda(v); }
            0xA1 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xB1 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xB2 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xA7 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xB7 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.lda(v); }
            0xA3 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.lda(v); }
            0xB3 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.lda(v); }

            // ---- STA ----
            0x85 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x95 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x8D => { let ea = self.am_absolute(bus); let a = self.a; self.write_m(bus, ea, a); }
            0x9D => { let ea = self.am_absolute_x(bus, &mut pen); let a = self.a; self.write_m(bus, ea, a); }
            0x99 => { let ea = self.am_absolute_y(bus, &mut pen); let a = self.a; self.write_m(bus, ea, a); }
            0x8F => { let ea = self.am_long(bus); let a = self.a; self.write_m(bus, ea, a); }
            0x9F => { let ea = self.am_long_x(bus); let a = self.a; self.write_m(bus, ea, a); }
            0x81 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x91 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x92 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x87 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x97 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let a = self.a; self.write_m(bus, ea, a); }
            0x83 => { let ea = self.am_stack_rel(bus); let a = self.a; self.write_m(bus, ea, a); }
            0x93 => { let ea = self.am_stack_rel_y(bus); let a = self.a; self.write_m(bus, ea, a); }

            // ---- LDX / LDY ----
            0xA2 => { let v = self.imm_x(bus); self.ldx(v); }
            0xA6 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.ldx(v); }
            0xB6 => { let ea = self.am_direct_y(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.ldx(v); }
            0xAE => { let ea = self.am_absolute(bus); let v = self.read_x(bus, ea); self.ldx(v); }
            0xBE => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_x(bus, ea); self.ldx(v); }
            0xA0 => { let v = self.imm_x(bus); self.ldy(v); }
            0xA4 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.ldy(v); }
            0xB4 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.ldy(v); }
            0xAC => { let ea = self.am_absolute(bus); let v = self.read_x(bus, ea); self.ldy(v); }
            0xBC => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_x(bus, ea); self.ldy(v); }

            // ---- STX / STY / STZ ----
            0x86 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let x = self.x; self.write_x(bus, ea, x); }
            0x96 => { let ea = self.am_direct_y(bus); pen += self.dp_penalty(); let x = self.x; self.write_x(bus, ea, x); }
            0x8E => { let ea = self.am_absolute(bus); let x = self.x; self.write_x(bus, ea, x); }
            0x84 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let y = self.y; self.write_x(bus, ea, y); }
            0x94 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let y = self.y; self.write_x(bus, ea, y); }
            0x8C => { let ea = self.am_absolute(bus); let y = self.y; self.write_x(bus, ea, y); }
            0x64 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.write_m(bus, ea, 0); }
            0x74 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.write_m(bus, ea, 0); }
            0x9C => { let ea = self.am_absolute(bus); self.write_m(bus, ea, 0); }
            0x9E => { let ea = self.am_absolute_x(bus, &mut pen); self.write_m(bus, ea, 0); }

            // ---- ADC ----
            0x69 => { let v = self.imm_m(bus); self.adc(v); }
            0x65 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x75 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x6D => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.adc(v); }
            0x7D => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.adc(v); }
            0x79 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.adc(v); }
            0x6F => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.adc(v); }
            0x7F => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.adc(v); }
            0x61 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x71 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x72 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x67 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x77 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.adc(v); }
            0x63 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.adc(v); }
            0x73 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.adc(v); }

            // ---- SBC ----
            0xE9 => { let v = self.imm_m(bus); self.sbc(v); }
            0xE5 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF5 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xED => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.sbc(v); }
            0xFD => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF9 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.sbc(v); }
            0xEF => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.sbc(v); }
            0xFF => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.sbc(v); }
            0xE1 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF1 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF2 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xE7 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF7 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.sbc(v); }
            0xE3 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.sbc(v); }
            0xF3 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.sbc(v); }

            // ---- AND ----
            0x29 => { let v = self.imm_m(bus); self.and(v); }
            0x25 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x35 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x2D => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.and(v); }
            0x3D => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.and(v); }
            0x39 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.and(v); }
            0x2F => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.and(v); }
            0x3F => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.and(v); }
            0x21 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x31 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x32 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x27 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x37 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.and(v); }
            0x23 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.and(v); }
            0x33 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.and(v); }

            // ---- ORA ----
            0x09 => { let v = self.imm_m(bus); self.ora(v); }
            0x05 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x15 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x0D => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.ora(v); }
            0x1D => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.ora(v); }
            0x19 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.ora(v); }
            0x0F => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.ora(v); }
            0x1F => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.ora(v); }
            0x01 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x11 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x12 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x07 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x17 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.ora(v); }
            0x03 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.ora(v); }
            0x13 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.ora(v); }

            // ---- EOR ----
            0x49 => { let v = self.imm_m(bus); self.eor(v); }
            0x45 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x55 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x4D => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.eor(v); }
            0x5D => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.eor(v); }
            0x59 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.eor(v); }
            0x4F => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.eor(v); }
            0x5F => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.eor(v); }
            0x41 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x51 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x52 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x47 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x57 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.eor(v); }
            0x43 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.eor(v); }
            0x53 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.eor(v); }

            // ---- CMP ----
            0xC9 => { let v = self.imm_m(bus); self.cmp(self.a, v, self.m8()); }
            0xC5 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD5 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xCD => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xDD => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD9 => { let ea = self.am_absolute_y(bus, &mut pen); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xCF => { let ea = self.am_long(bus); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xDF => { let ea = self.am_long_x(bus); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xC1 => { let ea = self.am_indirect_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD1 => { let ea = self.am_indirect_y(bus, &mut pen); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD2 => { let ea = self.am_indirect(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xC7 => { let ea = self.am_indirect_long(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD7 => { let ea = self.am_indirect_long_y(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xC3 => { let ea = self.am_stack_rel(bus); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }
            0xD3 => { let ea = self.am_stack_rel_y(bus); let v = self.read_m(bus, ea); self.cmp(self.a, v, self.m8()); }

            // ---- CPX / CPY ----
            0xE0 => { let v = self.imm_x(bus); self.cmp(self.x, v, self.x8()); }
            0xE4 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.cmp(self.x, v, self.x8()); }
            0xEC => { let ea = self.am_absolute(bus); let v = self.read_x(bus, ea); self.cmp(self.x, v, self.x8()); }
            0xC0 => { let v = self.imm_x(bus); self.cmp(self.y, v, self.x8()); }
            0xC4 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_x(bus, ea); self.cmp(self.y, v, self.x8()); }
            0xCC => { let ea = self.am_absolute(bus); let v = self.read_x(bus, ea); self.cmp(self.y, v, self.x8()); }

            // ---- BIT ----
            0x89 => { let v = self.imm_m(bus); let r = self.a & v; self.set_nz(r, self.m8()); /* imm only sets Z */ }
            0x24 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.bit(v); }
            0x34 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); let v = self.read_m(bus, ea); self.bit(v); }
            0x2C => { let ea = self.am_absolute(bus); let v = self.read_m(bus, ea); self.bit(v); }
            0x3C => { let ea = self.am_absolute_x(bus, &mut pen); let v = self.read_m(bus, ea); self.bit(v); }

            // ---- INC / DEC (memory + accumulator) ----
            0x1A => { self.a = self.inc_v(self.a, self.m8()); }      // INC A
            0x3A => { self.a = self.dec_v(self.a, self.m8()); }      // DEC A
            0xE6 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.inc_v(v, c.m8())); }
            0xF6 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.inc_v(v, c.m8())); }
            0xEE => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.inc_v(v, c.m8())); }
            0xFE => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.inc_v(v, c.m8())); }
            0xC6 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.dec_v(v, c.m8())); }
            0xD6 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.dec_v(v, c.m8())); }
            0xCE => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.dec_v(v, c.m8())); }
            0xDE => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.dec_v(v, c.m8())); }

            // ---- INX/INY/DEX/DEY ----
            0xE8 => { let v = self.inc_v(self.x, self.x8()); self.x = self.maskx(v); self.set_nz(self.x, self.x8()); }
            0xC8 => { let v = self.inc_v(self.y, self.x8()); self.y = self.maskx(v); self.set_nz(self.y, self.x8()); }
            0xCA => { let v = self.dec_v(self.x, self.x8()); self.x = self.maskx(v); self.set_nz(self.x, self.x8()); }
            0x88 => { let v = self.dec_v(self.y, self.x8()); self.y = self.maskx(v); self.set_nz(self.y, self.x8()); }

            // ---- shifts/rotates ----
            0x0A => { self.a = self.asl_v(self.a, self.m8()); }
            0x06 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.asl_v(v, c.m8())); }
            0x16 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.asl_v(v, c.m8())); }
            0x0E => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.asl_v(v, c.m8())); }
            0x1E => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.asl_v(v, c.m8())); }
            0x4A => { self.a = self.lsr_v(self.a, self.m8()); }
            0x46 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.lsr_v(v, c.m8())); }
            0x56 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.lsr_v(v, c.m8())); }
            0x4E => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.lsr_v(v, c.m8())); }
            0x5E => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.lsr_v(v, c.m8())); }
            0x2A => { self.a = self.rol_v(self.a, self.m8()); }
            0x26 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.rol_v(v, c.m8())); }
            0x36 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.rol_v(v, c.m8())); }
            0x2E => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.rol_v(v, c.m8())); }
            0x3E => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.rol_v(v, c.m8())); }
            0x6A => { self.a = self.ror_v(self.a, self.m8()); }
            0x66 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.ror_v(v, c.m8())); }
            0x76 => { let ea = self.am_direct_x(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.ror_v(v, c.m8())); }
            0x6E => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.ror_v(v, c.m8())); }
            0x7E => { let ea = self.am_absolute_x(bus, &mut pen); self.rmw(bus, ea, |c,v| c.ror_v(v, c.m8())); }

            // ---- TSB / TRB ----
            0x04 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.tsb(v)); }
            0x0C => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.tsb(v)); }
            0x14 => { let ea = self.am_direct(bus); pen += self.dp_penalty(); self.rmw(bus, ea, |c,v| c.trb(v)); }
            0x1C => { let ea = self.am_absolute(bus); self.rmw(bus, ea, |c,v| c.trb(v)); }

            // ---- branches ----
            0x90 => { let t = self.p & FLAG_C == 0; self.branch(bus, t, &mut pen); } // BCC
            0xB0 => { let t = self.p & FLAG_C != 0; self.branch(bus, t, &mut pen); } // BCS
            0xD0 => { let t = self.p & FLAG_Z == 0; self.branch(bus, t, &mut pen); } // BNE
            0xF0 => { let t = self.p & FLAG_Z != 0; self.branch(bus, t, &mut pen); } // BEQ
            0x10 => { let t = self.p & FLAG_N == 0; self.branch(bus, t, &mut pen); } // BPL
            0x30 => { let t = self.p & FLAG_N != 0; self.branch(bus, t, &mut pen); } // BMI
            0x50 => { let t = self.p & FLAG_V == 0; self.branch(bus, t, &mut pen); } // BVC
            0x70 => { let t = self.p & FLAG_V != 0; self.branch(bus, t, &mut pen); } // BVS
            0x80 => { self.branch(bus, true, &mut pen); } // BRA
            0x82 => { // BRL (16-bit relative)
                let off = self.fetch16(bus) as i16;
                self.pc = self.pc.wrapping_add(off as u16);
            }

            // ---- jumps / calls ----
            0x4C => { let a = self.fetch16(bus); self.pc = a; } // JMP abs
            0x6C => { // JMP (abs)
                let ptr = self.fetch16(bus);
                let lo = self.read8_at(bus, 0, ptr) as u16;
                let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x7C => { // JMP (abs,X)
                let ptr = self.fetch16(bus).wrapping_add(self.x);
                let lo = self.read8_at(bus, self.pbr, ptr) as u16;
                let hi = self.read8_at(bus, self.pbr, ptr.wrapping_add(1)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x5C => { let a = self.fetch16(bus); let b = self.fetch8(bus); self.pc = a; self.pbr = b; } // JML long
            0xDC => { // JML [abs]
                let ptr = self.fetch16(bus);
                let lo = self.read8_at(bus, 0, ptr) as u16;
                let hi = self.read8_at(bus, 0, ptr.wrapping_add(1)) as u16;
                let bank = self.read8_at(bus, 0, ptr.wrapping_add(2));
                self.pc = (hi << 8) | lo; self.pbr = bank;
            }
            0x20 => { let a = self.fetch16(bus); let ret = self.pc.wrapping_sub(1); self.push16(bus, ret); self.pc = a; } // JSR
            0xFC => { // JSR (abs,X)
                let a = self.fetch16(bus);
                let ret = self.pc.wrapping_sub(1);
                self.push16(bus, ret);
                let ptr = a.wrapping_add(self.x);
                let lo = self.read8_at(bus, self.pbr, ptr) as u16;
                let hi = self.read8_at(bus, self.pbr, ptr.wrapping_add(1)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x22 => { // JSL long
                let a = self.fetch16(bus); let bank = self.fetch8(bus);
                let ret = self.pc.wrapping_sub(1);
                self.push8(bus, self.pbr);
                self.push16(bus, ret);
                self.pc = a; self.pbr = bank;
            }
            0x60 => { let r = self.pull16(bus); self.pc = r.wrapping_add(1); } // RTS
            0x6B => { let r = self.pull16(bus); let bank = self.pull8(bus); self.pc = r.wrapping_add(1); self.pbr = bank; } // RTL
            0x40 => { self.rti(bus); } // RTI

            // ---- BRK / COP ----
            0x00 => { self.brk(bus, false); }
            0x02 => { self.brk(bus, true); }

            // ---- stack push/pull ----
            0x48 => { if self.m8() { let a = self.a as u8; self.push8(bus, a); } else { let a = self.a; self.push16(bus, a); } } // PHA
            0x68 => { let v = if self.m8() { self.pull8(bus) as u16 } else { self.pull16(bus) }; self.a = if self.m8() { (self.a & 0xFF00) | (v & 0xFF) } else { v }; self.set_nz(self.a, self.m8()); } // PLA
            0xDA => { if self.x8() { let x = self.x as u8; self.push8(bus, x); } else { let x = self.x; self.push16(bus, x); } } // PHX
            0xFA => { let v = if self.x8() { self.pull8(bus) as u16 } else { self.pull16(bus) }; self.x = self.maskx(v); self.set_nz(self.x, self.x8()); } // PLX
            0x5A => { if self.x8() { let y = self.y as u8; self.push8(bus, y); } else { let y = self.y; self.push16(bus, y); } } // PHY
            0x7A => { let v = if self.x8() { self.pull8(bus) as u16 } else { self.pull16(bus) }; self.y = self.maskx(v); self.set_nz(self.y, self.x8()); } // PLY
            0x08 => { self.push8(bus, self.p); } // PHP
            0x28 => { let p = self.pull8(bus); self.set_p(p); } // PLP
            0x8B => { self.push8(bus, self.dbr); } // PHB
            0xAB => { let v = self.pull8(bus); self.dbr = v; self.set_nz(v as u16, true); } // PLB
            0x4B => { self.push8(bus, self.pbr); } // PHK
            0x0B => { let d = self.d; self.push16(bus, d); } // PHD
            0x2B => { let v = self.pull16(bus); self.d = v; self.set_nz(v, false); } // PLD
            0xF4 => { let v = self.fetch16(bus); self.push16(bus, v); } // PEA
            0xD4 => { let (_, ptr) = self.am_direct(bus); let lo = self.read8_at(bus,0,ptr) as u16; let hi = self.read8_at(bus,0,ptr.wrapping_add(1)) as u16; self.push16(bus, (hi<<8)|lo); } // PEI
            0x62 => { let off = self.fetch16(bus) as i16; let v = self.pc.wrapping_add(off as u16); self.push16(bus, v); } // PER

            // ---- transfers ----
            0xAA => { let v = self.a; self.x = self.maskx(v); self.set_nz(self.x, self.x8()); } // TAX
            0xA8 => { let v = self.a; self.y = self.maskx(v); self.set_nz(self.y, self.x8()); } // TAY
            0x8A => { self.a = self.txa_val(self.x); self.set_nz(self.a, self.m8()); } // TXA
            0x98 => { self.a = self.txa_val(self.y); self.set_nz(self.a, self.m8()); } // TYA
            0x9A => { if self.e { self.sp = 0x0100 | (self.x & 0xFF); } else { self.sp = self.x; } } // TXS
            0xBA => { let v = self.sp; self.x = self.maskx(v); self.set_nz(self.x, self.x8()); } // TSX
            0x9B => { let v = self.x; self.y = self.maskx(v); self.set_nz(self.y, self.x8()); } // TXY
            0xBB => { let v = self.y; self.x = self.maskx(v); self.set_nz(self.x, self.x8()); } // TYX
            0x5B => { self.d = self.a; self.set_nz(self.d, false); } // TCD
            0x7B => { self.a = self.d; self.set_nz(self.a, false); } // TDC
            0x1B => { if self.e { self.sp = 0x0100 | (self.a & 0xFF); } else { self.sp = self.a; } } // TCS
            0x3B => { self.a = self.sp; self.set_nz(self.a, false); } // TSC

            // ---- flag ops ----
            0x18 => { self.p &= !FLAG_C; } // CLC
            0x38 => { self.p |= FLAG_C; }  // SEC
            0x58 => { self.p &= !FLAG_I; } // CLI
            0x78 => { self.p |= FLAG_I; }  // SEI
            0xB8 => { self.p &= !FLAG_V; } // CLV
            0xD8 => { self.p &= !FLAG_D; } // CLD
            0xF8 => { self.p |= FLAG_D; }  // SED
            0xC2 => { let m = self.fetch8(bus); self.set_p(self.p & !m); if self.e { self.p |= FLAG_M | FLAG_X; } self.normalize_widths(); } // REP
            0xE2 => { let m = self.fetch8(bus); self.set_p(self.p | m); self.normalize_widths(); } // SEP
            0xFB => { // XCE: swap C and E
                let new_e = self.p & FLAG_C != 0;
                self.set_flag(FLAG_C, self.e);
                self.e = new_e;
                if self.e { self.p |= FLAG_M | FLAG_X; self.sp = 0x0100 | (self.sp & 0xFF); }
                self.normalize_widths();
            }
            0xEB => { // XBA: swap A bytes
                let lo = self.a & 0xFF; let hi = (self.a >> 8) & 0xFF;
                self.a = (lo << 8) | hi;
                self.set_nz(hi, true);
            }

            // ---- block moves ----
            0x54 => { self.mvn(bus); } // MVN
            0x44 => { self.mvp(bus); } // MVP

            // ---- misc ----
            0xEA => {} // NOP
            0x42 => { let _ = self.fetch8(bus); } // WDM (reserved, consumes a byte)
            0xCB => { self.waiting = true; } // WAI
            0xDB => { self.stopped = true; self.fault = Some((0xDB, self.pc.wrapping_sub(1))); } // STP

            // The 65816 opcode map is fully populated, so every byte is handled
            // above; this arm is unreachable but kept as a panic-free safety net.
            #[allow(unreachable_patterns)]
            _ => {}
        }

        self.cycles += (base + pen) as u64;
    }
}

// =============================================================================
// ALU / helper operations + immediate operand reads + the base cycle table.
// =============================================================================
impl Cpu {
    /// Read an immediate operand sized by the M flag (1 or 2 bytes).
    fn imm_m(&mut self, bus: &mut dyn Bus) -> u16 {
        if self.m8() {
            self.fetch8(bus) as u16
        } else {
            self.cycles += 1;
            self.fetch16(bus)
        }
    }
    /// Read an immediate operand sized by the X flag.
    fn imm_x(&mut self, bus: &mut dyn Bus) -> u16 {
        if self.x8() {
            self.fetch8(bus) as u16
        } else {
            self.cycles += 1;
            self.fetch16(bus)
        }
    }

    /// Read a memory operand sized by the X flag (for LDX/LDY/CPX/CPY).
    fn read_x(&mut self, bus: &mut dyn Bus, ea: (u8, u16)) -> u16 {
        if self.x8() {
            self.read8_at(bus, ea.0, ea.1) as u16
        } else {
            let lo = self.read8_at(bus, ea.0, ea.1) as u16;
            let hi = self.read8_at(bus, ea.0, ea.1.wrapping_add(1)) as u16;
            self.cycles += 1;
            (hi << 8) | lo
        }
    }
    fn write_x(&mut self, bus: &mut dyn Bus, ea: (u8, u16), v: u16) {
        if self.x8() {
            self.write8_at(bus, ea.0, ea.1, v as u8);
        } else {
            self.write8_at(bus, ea.0, ea.1, v as u8);
            self.write8_at(bus, ea.0, ea.1.wrapping_add(1), (v >> 8) as u8);
            self.cycles += 1;
        }
    }

    /// Mask an index value to the current X width.
    #[inline]
    fn maskx(&self, v: u16) -> u16 {
        if self.x8() {
            v & 0xFF
        } else {
            v
        }
    }

    /// Compute the new accumulator for TXA/TYA honoring the M width: an 8-bit
    /// accumulator keeps its high byte.
    fn txa_val(&self, src: u16) -> u16 {
        if self.m8() {
            (self.a & 0xFF00) | (src & 0xFF)
        } else {
            src
        }
    }

    fn lda(&mut self, v: u16) {
        if self.m8() {
            self.a = (self.a & 0xFF00) | (v & 0xFF);
        } else {
            self.a = v;
        }
        self.set_nz(self.a, self.m8());
    }
    fn ldx(&mut self, v: u16) {
        self.x = self.maskx(v);
        self.set_nz(self.x, self.x8());
    }
    fn ldy(&mut self, v: u16) {
        self.y = self.maskx(v);
        self.set_nz(self.y, self.x8());
    }

    fn and(&mut self, v: u16) {
        if self.m8() {
            let r = (self.a as u8) & (v as u8);
            self.a = (self.a & 0xFF00) | r as u16;
        } else {
            self.a &= v;
        }
        self.set_nz(self.a, self.m8());
    }
    fn ora(&mut self, v: u16) {
        if self.m8() {
            let r = (self.a as u8) | (v as u8);
            self.a = (self.a & 0xFF00) | r as u16;
        } else {
            self.a |= v;
        }
        self.set_nz(self.a, self.m8());
    }
    fn eor(&mut self, v: u16) {
        if self.m8() {
            let r = (self.a as u8) ^ (v as u8);
            self.a = (self.a & 0xFF00) | r as u16;
        } else {
            self.a ^= v;
        }
        self.set_nz(self.a, self.m8());
    }

    fn adc(&mut self, v: u16) {
        let eight = self.m8();
        let c = (self.p & FLAG_C != 0) as u32;
        if self.p & FLAG_D != 0 {
            // Decimal mode (BCD).
            self.adc_bcd(v, eight, c);
            return;
        }
        if eight {
            let a = self.a as u8 as u32;
            let m = v as u8 as u32;
            let sum = a + m + c;
            let res = sum as u8;
            self.set_flag(FLAG_C, sum > 0xFF);
            self.set_flag(FLAG_V, ((a ^ sum) & (m ^ sum) & 0x80) != 0);
            self.a = (self.a & 0xFF00) | res as u16;
            self.set_nz(res as u16, true);
        } else {
            let a = self.a as u32;
            let m = v as u32;
            let sum = a + m + c;
            let res = sum as u16;
            self.set_flag(FLAG_C, sum > 0xFFFF);
            self.set_flag(FLAG_V, ((a ^ sum) & (m ^ sum) & 0x8000) != 0);
            self.a = res;
            self.set_nz(res, false);
        }
    }

    fn adc_bcd(&mut self, v: u16, eight: bool, c: u32) {
        if eight {
            let a = self.a as u8 as u32;
            let m = v as u8 as u32;
            let mut lo = (a & 0x0F) + (m & 0x0F) + c;
            if lo > 0x09 {
                lo += 0x06;
            }
            let mut hi = (a >> 4) + (m >> 4) + (lo > 0x0F) as u32;
            let bin = a + m + c;
            self.set_flag(FLAG_V, ((a ^ bin) & (m ^ bin) & 0x80) != 0);
            if hi > 0x09 {
                hi += 0x06;
            }
            self.set_flag(FLAG_C, hi > 0x0F);
            let res = (((hi << 4) | (lo & 0x0F)) & 0xFF) as u8;
            self.a = (self.a & 0xFF00) | res as u16;
            self.set_nz(res as u16, true);
        } else {
            let a = self.a as u32;
            let m = v as u32;
            let mut r0 = (a & 0x0F) + (m & 0x0F) + c;
            if r0 > 0x09 { r0 += 0x06; }
            let mut r1 = (a >> 4 & 0x0F) + (m >> 4 & 0x0F) + (r0 > 0x0F) as u32;
            if r1 > 0x09 { r1 += 0x06; }
            let mut r2 = (a >> 8 & 0x0F) + (m >> 8 & 0x0F) + (r1 > 0x0F) as u32;
            if r2 > 0x09 { r2 += 0x06; }
            let mut r3 = (a >> 12 & 0x0F) + (m >> 12 & 0x0F) + (r2 > 0x0F) as u32;
            let bin = a + m + c;
            self.set_flag(FLAG_V, ((a ^ bin) & (m ^ bin) & 0x8000) != 0);
            if r3 > 0x09 { r3 += 0x06; }
            self.set_flag(FLAG_C, r3 > 0x0F);
            let res = ((r3 << 12) | ((r2 & 0x0F) << 8) | ((r1 & 0x0F) << 4) | (r0 & 0x0F)) as u16;
            self.a = res;
            self.set_nz(res, false);
        }
    }

    fn sbc(&mut self, v: u16) {
        let eight = self.m8();
        let c = (self.p & FLAG_C != 0) as u32;
        if self.p & FLAG_D != 0 {
            self.sbc_bcd(v, eight, c);
            return;
        }
        if eight {
            let a = self.a as u8 as i32;
            let m = v as u8 as i32;
            let diff = a - m - (1 - c as i32);
            let res = diff as u8;
            self.set_flag(FLAG_C, diff >= 0);
            self.set_flag(FLAG_V, ((a ^ m) & (a ^ diff) & 0x80) != 0);
            self.a = (self.a & 0xFF00) | res as u16;
            self.set_nz(res as u16, true);
        } else {
            let a = self.a as i32;
            let m = v as i32;
            let diff = a - m - (1 - c as i32);
            let res = diff as u16;
            self.set_flag(FLAG_C, diff >= 0);
            self.set_flag(FLAG_V, ((a ^ m) & (a ^ diff) & 0x8000) != 0);
            self.a = res;
            self.set_nz(res, false);
        }
    }

    fn sbc_bcd(&mut self, v: u16, eight: bool, c: u32) {
        // Compute binary diff for V/C, then BCD-adjust.
        if eight {
            let a = self.a as u8 as i32;
            let m = v as u8 as i32;
            let bin = a - m - (1 - c as i32);
            let mut lo = (a & 0x0F) - (m & 0x0F) - (1 - c as i32);
            let mut hi = (a >> 4) - (m >> 4);
            if lo < 0 { lo += 0x10; hi -= 1; }
            if hi < 0 { hi += 0x10; }
            // adjust
            let mut lo2 = lo; let mut hi2 = hi;
            if (a & 0x0F) - (1 - c as i32) < (m & 0x0F) { lo2 = (lo2 - 6) & 0x0F; }
            if bin < 0 { hi2 = (hi2 - 6) & 0x0F; }
            self.set_flag(FLAG_C, bin >= 0);
            self.set_flag(FLAG_V, ((a ^ m) & (a ^ bin) & 0x80) != 0);
            let res = (((hi2 << 4) | (lo2 & 0x0F)) & 0xFF) as u8;
            self.a = (self.a & 0xFF00) | res as u16;
            self.set_nz(res as u16, true);
        } else {
            // 16-bit BCD subtract: do it digit-by-digit.
            let a = self.a as i32;
            let m = v as i32;
            let bin = a - m - (1 - c as i32);
            let mut borrow = 1 - c as i32;
            let mut res = 0u16;
            for d in 0..4 {
                let da = (a >> (d * 4)) & 0x0F;
                let dm = (m >> (d * 4)) & 0x0F;
                let mut dr = da - dm - borrow;
                if dr < 0 { dr += 10; borrow = 1; } else { borrow = 0; }
                res |= ((dr & 0x0F) as u16) << (d * 4);
            }
            self.set_flag(FLAG_C, bin >= 0);
            self.set_flag(FLAG_V, ((a ^ m) & (a ^ bin) & 0x8000) != 0);
            self.a = res;
            self.set_nz(res, false);
        }
    }

    fn cmp(&mut self, reg: u16, v: u16, eight: bool) {
        if eight {
            let r = (reg as u8).wrapping_sub(v as u8);
            self.set_flag(FLAG_C, (reg as u8) >= (v as u8));
            self.set_nz(r as u16, true);
        } else {
            let r = reg.wrapping_sub(v);
            self.set_flag(FLAG_C, reg >= v);
            self.set_nz(r, false);
        }
    }

    fn bit(&mut self, v: u16) {
        let eight = self.m8();
        let r = if eight { (self.a as u8 & v as u8) as u16 } else { self.a & v };
        self.set_flag(FLAG_Z, if eight { r as u8 == 0 } else { r == 0 });
        if eight {
            self.set_flag(FLAG_N, v & 0x80 != 0);
            self.set_flag(FLAG_V, v & 0x40 != 0);
        } else {
            self.set_flag(FLAG_N, v & 0x8000 != 0);
            self.set_flag(FLAG_V, v & 0x4000 != 0);
        }
    }

    fn inc_v(&mut self, v: u16, eight: bool) -> u16 {
        let r = if eight { (v as u8).wrapping_add(1) as u16 | (v & 0xFF00) } else { v.wrapping_add(1) };
        self.set_nz(r, eight);
        if eight { (v & 0xFF00) | ((v as u8).wrapping_add(1) as u16) } else { r }
    }
    fn dec_v(&mut self, v: u16, eight: bool) -> u16 {
        let r = if eight { (v & 0xFF00) | ((v as u8).wrapping_sub(1) as u16) } else { v.wrapping_sub(1) };
        self.set_nz(r, eight);
        r
    }

    fn asl_v(&mut self, v: u16, eight: bool) -> u16 {
        if eight {
            self.set_flag(FLAG_C, v & 0x80 != 0);
            let r = ((v as u8) << 1) as u16;
            self.set_nz(r, true);
            (v & 0xFF00) | (r & 0xFF)
        } else {
            self.set_flag(FLAG_C, v & 0x8000 != 0);
            let r = v << 1;
            self.set_nz(r, false);
            r
        }
    }
    fn lsr_v(&mut self, v: u16, eight: bool) -> u16 {
        if eight {
            self.set_flag(FLAG_C, v & 1 != 0);
            let r = ((v as u8) >> 1) as u16;
            self.set_nz(r, true);
            (v & 0xFF00) | (r & 0xFF)
        } else {
            self.set_flag(FLAG_C, v & 1 != 0);
            let r = v >> 1;
            self.set_nz(r, false);
            r
        }
    }
    fn rol_v(&mut self, v: u16, eight: bool) -> u16 {
        let cin = (self.p & FLAG_C != 0) as u16;
        if eight {
            self.set_flag(FLAG_C, v & 0x80 != 0);
            let r = (((v as u8) << 1) as u16) | cin;
            self.set_nz(r, true);
            (v & 0xFF00) | (r & 0xFF)
        } else {
            self.set_flag(FLAG_C, v & 0x8000 != 0);
            let r = (v << 1) | cin;
            self.set_nz(r, false);
            r
        }
    }
    fn ror_v(&mut self, v: u16, eight: bool) -> u16 {
        let cin = (self.p & FLAG_C != 0) as u16;
        if eight {
            self.set_flag(FLAG_C, v & 1 != 0);
            let r = (((v as u8) >> 1) as u16) | (cin << 7);
            self.set_nz(r, true);
            (v & 0xFF00) | (r & 0xFF)
        } else {
            self.set_flag(FLAG_C, v & 1 != 0);
            let r = (v >> 1) | (cin << 15);
            self.set_nz(r, false);
            r
        }
    }

    fn tsb(&mut self, v: u16) -> u16 {
        let eight = self.m8();
        let a = if eight { self.a & 0xFF } else { self.a };
        self.set_flag(FLAG_Z, (a & v) == 0);
        v | a
    }
    fn trb(&mut self, v: u16) -> u16 {
        let eight = self.m8();
        let a = if eight { self.a & 0xFF } else { self.a };
        self.set_flag(FLAG_Z, (a & v) == 0);
        v & !a
    }

    /// Read-modify-write helper honoring the M width.
    fn rmw(&mut self, bus: &mut dyn Bus, ea: (u8, u16), f: impl Fn(&mut Cpu, u16) -> u16) {
        let v = self.read_m(bus, ea);
        let r = f(self, v);
        self.write_m(bus, ea, r);
        self.cycles += 2; // RMW penalty (extra internal cycles)
    }

    fn branch(&mut self, bus: &mut dyn Bus, taken: bool, pen: &mut u32) {
        let off = self.fetch8(bus) as i8 as i16;
        if taken {
            let old = self.pc;
            self.pc = self.pc.wrapping_add(off as u16);
            *pen += 1;
            if self.e && (old & 0xFF00) != (self.pc & 0xFF00) {
                *pen += 1;
            }
        }
    }

    fn brk(&mut self, bus: &mut dyn Bus, cop: bool) {
        let _signature = self.fetch8(bus); // BRK/COP consume a signature byte
        let vec = if self.e {
            self.push16(bus, self.pc);
            self.push8(bus, self.p | FLAG_X); // B set
            if cop { EMU_COP_VEC } else { EMU_IRQ_BRK_VEC }
        } else {
            self.push8(bus, self.pbr);
            self.push16(bus, self.pc);
            self.push8(bus, self.p);
            if cop { NATIVE_COP_VEC } else { NATIVE_BRK_VEC }
        };
        self.p |= FLAG_I;
        self.p &= !FLAG_D;
        self.pbr = 0;
        let lo = bus.read8(vec) as u16;
        let hi = bus.read8(vec + 1) as u16;
        self.pc = (hi << 8) | lo;
    }

    fn rti(&mut self, bus: &mut dyn Bus) {
        if self.e {
            let p = self.pull8(bus);
            self.set_p(p);
            self.pc = self.pull16(bus);
        } else {
            let p = self.pull8(bus);
            self.set_p(p);
            self.pc = self.pull16(bus);
            self.pbr = self.pull8(bus);
        }
    }

    /// Set P from a pulled/REP/SEP value, normalizing register widths.
    fn set_p(&mut self, v: u8) {
        self.p = v;
        if self.e {
            self.p |= FLAG_M | FLAG_X;
        }
        self.normalize_widths();
    }

    /// After an M/X-flag change, mask the index registers down to 8 bits if X=1.
    fn normalize_widths(&mut self) {
        if self.x8() {
            self.x &= 0xFF;
            self.y &= 0xFF;
        }
    }

    fn mvn(&mut self, bus: &mut dyn Bus) {
        let dst = self.fetch8(bus);
        let src = self.fetch8(bus);
        self.dbr = dst;
        let b = self.read8_at(bus, src, self.x);
        self.write8_at(bus, dst, self.y, b);
        self.x = self.x.wrapping_add(1);
        self.y = self.y.wrapping_add(1);
        if self.x8() { self.x &= 0xFF; self.y &= 0xFF; }
        self.a = self.a.wrapping_sub(1);
        if self.a != 0xFFFF {
            // repeat: rewind PC to the opcode.
            self.pc = self.pc.wrapping_sub(3);
        }
        self.cycles += 5;
    }
    fn mvp(&mut self, bus: &mut dyn Bus) {
        let dst = self.fetch8(bus);
        let src = self.fetch8(bus);
        self.dbr = dst;
        let b = self.read8_at(bus, src, self.x);
        self.write8_at(bus, dst, self.y, b);
        self.x = self.x.wrapping_sub(1);
        self.y = self.y.wrapping_sub(1);
        if self.x8() { self.x &= 0xFF; self.y &= 0xFF; }
        self.a = self.a.wrapping_sub(1);
        if self.a != 0xFFFF {
            self.pc = self.pc.wrapping_sub(3);
        }
        self.cycles += 5;
    }
}

/// Base cycle counts indexed by opcode. These are the 65816 minimum cycle
/// counts (native-mode, 8-bit, no penalties); the executor adds width/page/
/// branch/RMW penalties on top. Derived from the WDC datasheet timing table.
#[rustfmt::skip]
static BASE_CYCLES: [u8; 256] = [
    7,6,7,4,5,3,5,6, 3,2,2,4,6,4,6,5, // 00
    2,5,5,7,5,4,6,6, 2,4,2,2,6,4,7,5, // 10
    6,6,8,4,3,3,5,6, 4,2,2,5,4,4,6,5, // 20
    2,5,5,7,4,4,6,6, 2,4,2,2,4,4,7,5, // 30
    6,6,2,4,7,3,5,6, 3,2,2,3,3,4,6,5, // 40
    2,5,5,7,7,4,6,6, 2,4,3,2,4,4,7,5, // 50
    6,6,6,4,3,3,5,6, 4,2,2,6,5,4,6,5, // 60
    2,5,5,7,4,4,6,6, 2,4,4,2,6,4,7,5, // 70
    2,6,3,4,3,3,3,6, 2,2,2,3,4,4,4,5, // 80
    2,6,5,7,4,4,4,6, 2,5,2,2,4,5,5,5, // 90
    2,6,2,4,3,3,3,6, 2,2,2,4,4,4,4,5, // A0
    2,5,5,7,4,4,4,6, 2,4,2,2,4,4,4,5, // B0
    2,6,3,4,3,3,5,6, 2,2,2,3,4,4,6,5, // C0
    2,5,5,7,6,4,6,6, 2,4,3,3,6,4,7,5, // D0
    2,6,3,4,3,3,5,6, 2,2,2,3,4,4,6,5, // E0
    2,5,5,7,5,4,6,6, 2,4,4,2,6,4,7,5, // F0
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 16 MiB RAM bus for CPU unit tests.
    struct FlatBus {
        mem: Vec<u8>,
    }
    impl FlatBus {
        fn new() -> FlatBus {
            FlatBus { mem: vec![0u8; 0x1000000] }
        }
    }
    impl Bus for FlatBus {
        fn read8(&mut self, addr: u32) -> u8 {
            self.mem[(addr & 0xFFFFFF) as usize]
        }
        fn write8(&mut self, addr: u32, v: u8) {
            self.mem[(addr & 0xFFFFFF) as usize] = v;
        }
    }

    /// Set up a CPU in native mode with 16-bit A/X (M=X=0), PC at $8000.
    fn native16() -> (Cpu, FlatBus) {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p &= !(FLAG_M | FLAG_X);
        cpu.pc = 0x8000;
        cpu.pbr = 0;
        (cpu, FlatBus::new())
    }

    fn run(cpu: &mut Cpu, bus: &mut FlatBus, prog: &[u8]) {
        for (i, &b) in prog.iter().enumerate() {
            bus.mem[0x8000 + i] = b;
        }
        let end = 0x8000 + prog.len() as u16;
        let mut guard = 0;
        while cpu.pc < end && guard < 1000 {
            cpu.step(bus);
            guard += 1;
        }
    }

    #[test]
    fn reset_loads_vector() {
        let mut bus = FlatBus::new();
        bus.mem[0xFFFC] = 0x34;
        bus.mem[0xFFFD] = 0x12;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        assert_eq!(cpu.pc, 0x1234);
        assert!(cpu.e);
    }

    #[test]
    fn lda_imm_16bit() {
        let (mut cpu, mut bus) = native16();
        // LDA #$1234
        run(&mut cpu, &mut bus, &[0xA9, 0x34, 0x12]);
        assert_eq!(cpu.a, 0x1234);
        assert_eq!(cpu.p & FLAG_Z, 0);
        assert_eq!(cpu.p & FLAG_N, 0);
    }

    #[test]
    fn lda_imm_8bit_keeps_high() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_M; // 8-bit A
        cpu.p &= !FLAG_X;
        cpu.pc = 0x8000;
        cpu.a = 0xAA00;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0xA9, 0x55]); // LDA #$55
        assert_eq!(cpu.a, 0xAA55); // high byte preserved
    }

    #[test]
    fn adc_16bit_carry_and_overflow() {
        let (mut cpu, mut bus) = native16();
        cpu.a = 0x7FFF;
        run(&mut cpu, &mut bus, &[0xA9, 0x01, 0x00, 0x18, 0x69, 0x01, 0x00]);
        // Actually do explicit: clear carry, ADC #$0001 to 0x7FFF.
        let (mut cpu, mut bus) = native16();
        cpu.a = 0x7FFF;
        run(&mut cpu, &mut bus, &[0x18, 0x69, 0x01, 0x00]); // CLC; ADC #$0001
        assert_eq!(cpu.a, 0x8000);
        assert_ne!(cpu.p & FLAG_V, 0); // signed overflow
        assert_ne!(cpu.p & FLAG_N, 0);
    }

    #[test]
    fn sbc_8bit() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_M | FLAG_X;
        cpu.pc = 0x8000;
        cpu.a = 0x50;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0x38, 0xE9, 0x30]); // SEC; SBC #$30
        assert_eq!(cpu.a & 0xFF, 0x20);
        assert_ne!(cpu.p & FLAG_C, 0); // no borrow
    }

    #[test]
    fn adc_decimal_8bit() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_M | FLAG_X;
        cpu.pc = 0x8000;
        cpu.a = 0x25;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0xF8, 0x18, 0x69, 0x48]); // SED; CLC; ADC #$48
        // 25 + 48 = 73 in BCD.
        assert_eq!(cpu.a & 0xFF, 0x73);
    }

    #[test]
    fn inx_wraps_at_width() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_X | FLAG_M;
        cpu.pc = 0x8000;
        cpu.x = 0x00FF;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0xE8]); // INX (8-bit)
        assert_eq!(cpu.x, 0x00); // wraps to 0
        assert_ne!(cpu.p & FLAG_Z, 0);
    }

    #[test]
    fn sta_lda_direct_page() {
        let (mut cpu, mut bus) = native16();
        cpu.d = 0x0000;
        // LDA #$ABCD; STA $10; LDA #$0000; LDA $10
        run(&mut cpu, &mut bus, &[
            0xA9, 0xCD, 0xAB, // LDA #$ABCD
            0x85, 0x10,       // STA $10
        ]);
        assert_eq!(bus.mem[0x10], 0xCD);
        assert_eq!(bus.mem[0x11], 0xAB);
    }

    #[test]
    fn jsr_rts_roundtrip() {
        let (mut cpu, mut bus) = native16();
        cpu.sp = 0x1FF;
        // JSR $9000 ; (at $9000) RTS
        bus.mem[0x8000] = 0x20;
        bus.mem[0x8001] = 0x00;
        bus.mem[0x8002] = 0x90;
        bus.mem[0x9000] = 0x60; // RTS
        cpu.step(&mut bus); // JSR
        assert_eq!(cpu.pc, 0x9000);
        cpu.step(&mut bus); // RTS
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn xce_switches_native() {
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        assert!(cpu.e);
        let mut bus = FlatBus::new();
        // CLC; XCE -> native mode.
        run(&mut cpu, &mut bus, &[0x18, 0xFB]);
        assert!(!cpu.e);
    }

    #[test]
    fn rep_sep_change_widths() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_M | FLAG_X;
        cpu.pc = 0x8000;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0xC2, 0x30]); // REP #$30 -> clear M,X
        assert!(!cpu.m8());
        assert!(!cpu.x8());
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p &= !(FLAG_M | FLAG_X);
        cpu.pc = 0x8000;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0xE2, 0x30]); // SEP #$30 -> set M,X
        assert!(cpu.m8());
        assert!(cpu.x8());
    }

    #[test]
    fn branch_taken_and_not() {
        let (mut cpu, mut bus) = native16();
        // LDA #$0000 sets Z; BEQ +2 should branch.
        bus.mem[0x8000] = 0xA9; bus.mem[0x8001] = 0x00; bus.mem[0x8002] = 0x00; // LDA #0
        bus.mem[0x8003] = 0xF0; bus.mem[0x8004] = 0x02; // BEQ +2
        cpu.step(&mut bus); // LDA
        cpu.step(&mut bus); // BEQ
        assert_eq!(cpu.pc, 0x8007); // 0x8005 + 2
    }

    #[test]
    fn cmp_sets_carry() {
        let (mut cpu, mut bus) = native16();
        cpu.a = 0x0005;
        run(&mut cpu, &mut bus, &[0xC9, 0x03, 0x00]); // CMP #$0003
        assert_ne!(cpu.p & FLAG_C, 0); // A >= operand
        assert_eq!(cpu.p & FLAG_Z, 0);
    }

    #[test]
    fn asl_carry_out() {
        let mut cpu = Cpu::new();
        cpu.e = false;
        cpu.p |= FLAG_M | FLAG_X;
        cpu.pc = 0x8000;
        cpu.a = 0x80;
        let mut bus = FlatBus::new();
        run(&mut cpu, &mut bus, &[0x0A]); // ASL A
        assert_eq!(cpu.a & 0xFF, 0x00);
        assert_ne!(cpu.p & FLAG_C, 0);
        assert_ne!(cpu.p & FLAG_Z, 0);
    }

    #[test]
    fn absolute_long_addressing() {
        let (mut cpu, mut bus) = native16();
        bus.mem[0x123456] = 0x99;
        bus.mem[0x123457] = 0x88;
        // LDA $123456
        run(&mut cpu, &mut bus, &[0xAF, 0x56, 0x34, 0x12]);
        assert_eq!(cpu.a, 0x8899);
    }

    #[test]
    fn stp_sets_fault() {
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        let mut bus = FlatBus::new();
        bus.mem[0x8000] = 0xDB; // STP
        cpu.step(&mut bus);
        assert!(cpu.stopped);
        assert!(cpu.fault.is_some());
    }
}
