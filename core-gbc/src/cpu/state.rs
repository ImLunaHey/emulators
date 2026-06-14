//! LR35902 (Sharp GB CPU) register state + interrupt dispatch.
//!
//! Spec: Pan Docs — CPU Registers and Flags (gbdev.io/pandocs/CPU_Registers_and_Flags.html)
//! and Interrupts. The CPU is an 8-bit core with eight 8-bit registers that
//! pair into 16-bit views: AF, BC, DE, HL, plus a 16-bit SP and PC. The F
//! register holds four flags in its high nibble; its low nibble is always 0.
//!
//! This module ports register state + the interrupt scaffold only — instruction
//! decode/execute lives in `cpu::exec` (stubbed in this phase).

use crate::bus::Bus;
use crate::interrupts::{Interrupt, Irq};

// ---- F register flag bits (high nibble; low nibble always reads 0) ----
pub const FLAG_Z: u8 = 0b1000_0000; // bit 7 — Zero
pub const FLAG_N: u8 = 0b0100_0000; // bit 6 — Subtract (BCD)
pub const FLAG_H: u8 = 0b0010_0000; // bit 5 — Half-carry (BCD)
pub const FLAG_C: u8 = 0b0001_0000; // bit 4 — Carry

/// HALT/STOP low-power state. The CPU resumes from HALT when any enabled
/// interrupt is requested (IE & IF); STOP is exited by a joypad input and is
/// also how CGB games initiate a double-speed switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Power {
    /// Normal execution.
    #[default]
    Running,
    /// HALT — clock stopped until an interrupt is pending.
    Halted,
    /// STOP — very-low-power; on CGB also drives the KEY1 speed switch.
    Stopped,
}

/// The complete LR35902 register file + interrupt master enable + power state.
///
/// One owner (the `Gbc` god-struct); collaborators are passed `&mut`. The 8-bit
/// registers are stored individually; the 16-bit pair views are accessors.
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,

    /// Interrupt Master Enable. `EI` sets it (after a one-instruction delay
    /// handled in exec), `DI`/interrupt-service clears it.
    pub ime: bool,
    /// `EI` schedules IME to become true *after the next* instruction; this
    /// holds that one-instruction delay.
    pub ime_pending: bool,

    /// HALT/STOP power state.
    pub power: Power,

    /// The HALT bug: if HALT is entered with IME=0 and (IE & IF) != 0, the CPU
    /// fails to increment PC on the next fetch (reads the byte after HALT
    /// twice). Tracked here for `cpu::exec` to honor.
    pub halt_bug: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Post-BIOS register state for a CGB booting in CGB mode (A=0x11). Pan
    /// Docs — Power Up Sequence. `cpu::exec`/boot may override after a real or
    /// HLE boot ROM runs.
    pub fn new() -> Self {
        Cpu {
            a: 0x11,
            f: FLAG_Z,
            b: 0x00,
            c: 0x00,
            d: 0xFF,
            e: 0x56,
            h: 0x00,
            l: 0x0D,
            sp: 0xFFFE,
            pc: 0x0100,
            ime: false,
            ime_pending: false,
            power: Power::Running,
            halt_bug: false,
        }
    }

    // ---- 16-bit register-pair accessors ----
    #[inline]
    pub fn af(&self) -> u16 {
        ((self.a as u16) << 8) | (self.f as u16)
    }
    #[inline]
    pub fn bc(&self) -> u16 {
        ((self.b as u16) << 8) | (self.c as u16)
    }
    #[inline]
    pub fn de(&self) -> u16 {
        ((self.d as u16) << 8) | (self.e as u16)
    }
    #[inline]
    pub fn hl(&self) -> u16 {
        ((self.h as u16) << 8) | (self.l as u16)
    }

    #[inline]
    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        // The F register's low nibble is unused and always reads 0.
        self.f = (v as u8) & 0xF0;
    }
    #[inline]
    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    #[inline]
    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    #[inline]
    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }

    // ---- individual flag accessors ----
    #[inline]
    pub fn flag(&self, mask: u8) -> bool {
        self.f & mask != 0
    }
    #[inline]
    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.f |= mask;
        } else {
            self.f &= !mask;
        }
        // Keep the low nibble clear at all times.
        self.f &= 0xF0;
    }

    // ---- interrupt dispatch ----

    /// Service the highest-priority pending interrupt if IME is set and one is
    /// pending. Performs the hardware sequence: clear IME, clear the request
    /// bit, push PC (high then low), jump to the vector. Returns the serviced
    /// interrupt (so the caller can charge the ~20 cycles), or None.
    ///
    /// HALT is exited separately (on any `irq.pending()` regardless of IME);
    /// that wake-up is the CPU step's job, not this dispatch.
    pub fn service_interrupt(&mut self, bus: &mut dyn Bus, irq: &mut Irq) -> Option<Interrupt> {
        if !self.ime {
            return None;
        }
        let int = irq.highest_priority()?;
        self.ime = false;
        self.ime_pending = false;
        irq.acknowledge(int);
        // Push current PC onto the stack, high byte first (SP pre-decrements).
        let pc = self.pc;
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, (pc >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write8(self.sp, pc as u8);
        self.pc = int.vector();
        // Servicing an interrupt also wakes the CPU from HALT.
        if self.power == Power::Halted {
            self.power = Power::Running;
        }
        Some(int)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_accessors_roundtrip() {
        let mut cpu = Cpu::new();
        cpu.set_bc(0x1234);
        assert_eq!(cpu.b, 0x12);
        assert_eq!(cpu.c, 0x34);
        assert_eq!(cpu.bc(), 0x1234);
    }

    #[test]
    fn f_low_nibble_always_zero() {
        let mut cpu = Cpu::new();
        cpu.set_af(0xAA_FF);
        assert_eq!(cpu.f, 0xF0); // low nibble masked off
        cpu.set_flag(FLAG_C, true);
        assert_eq!(cpu.f & 0x0F, 0);
    }

    #[test]
    fn cgb_boot_register_defaults() {
        let cpu = Cpu::new();
        assert_eq!(cpu.a, 0x11); // CGB boot marker
        assert_eq!(cpu.pc, 0x0100);
        assert_eq!(cpu.sp, 0xFFFE);
    }
}
