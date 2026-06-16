//! The joypad / P1 (0xFF00) register + joypad interrupt.
//!
//! Spec: Pan Docs — Joypad Input (gbdev.io/pandocs/Joypad_Input.html).
//!
//! The eight buttons are read through a 2x4 matrix multiplexed by P1 bits 5-4:
//! bit 5 selects the action buttons (A/B/Select/Start), bit 4 selects the
//! direction buttons (Right/Left/Up/Down). The low nibble (bits 3-0) reads the
//! selected group, **active low** — a pressed button reads 0. A joypad
//! interrupt is requested on any high→low transition of a selected line.

use crate::interrupts::{Interrupt, Irq};

/// Internal pressed-state bit layout (1 = pressed). Mirrors the host-facing
/// `set_keys` order so the wasm/FFI bridge can pass a single byte.
pub mod button {
    pub const A: u8 = 1 << 0;
    pub const B: u8 = 1 << 1;
    pub const SELECT: u8 = 1 << 2;
    pub const START: u8 = 1 << 3;
    pub const RIGHT: u8 = 1 << 4;
    pub const LEFT: u8 = 1 << 5;
    pub const UP: u8 = 1 << 6;
    pub const DOWN: u8 = 1 << 7;
}

#[derive(Default, Clone)]
pub struct Joypad {
    /// Pressed buttons, 1 = pressed (see `button`).
    pressed: u8,
    /// The selection bits the CPU last wrote (P1 bits 5-4). 0 = group selected.
    select: u8,
}

impl Joypad {
    /// Set the full pressed-button state (host input). Bits per `button`.
    /// Raises the joypad interrupt on any newly-pressed selected line.
    pub fn set_keys(&mut self, bits: u8, irq: &mut Irq) {
        let before = self.read_lines();
        self.pressed = bits;
        let after = self.read_lines();
        // High→low transition on any selected line requests the interrupt.
        if (before & !after) != 0 {
            irq.request(Interrupt::Joypad);
        }
    }

    /// The current low nibble (bits 3-0) given the active selection, active low.
    #[inline]
    fn read_lines(&self) -> u8 {
        let mut lines = 0x0F;
        // bit 5 low → action buttons selected
        if self.select & 0x20 == 0 {
            lines &= !(self.pressed & 0x0F); // A,B,Select,Start in low nibble
        }
        // bit 4 low → direction buttons selected
        if self.select & 0x10 == 0 {
            lines &= !((self.pressed >> 4) & 0x0F); // Right,Left,Up,Down
        }
        lines
    }

    /// Read P1/JOYP (0xFF00). Bits 7-6 always read 1; bits 5-4 are the last
    /// written selection; bits 3-0 are the (active-low) button lines.
    pub fn read(&self) -> u8 {
        0xC0 | (self.select & 0x30) | self.read_lines()
    }

    /// Write P1/JOYP (0xFF00). Only bits 5-4 are writable (the selection).
    pub fn write(&mut self, v: u8) {
        self.select = v & 0x30;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_and_direction_groups_multiplex() {
        let mut jp = Joypad::default();
        let mut irq = Irq::new();
        jp.set_keys(button::A | button::DOWN, &mut irq);

        // Select action group (bit 5 low, bit 4 high).
        jp.write(0x10);
        // A is bit 0; pressed -> reads 0.
        assert_eq!(jp.read() & 0x0F, 0x0F & !button::A & 0x0F);

        // Select direction group (bit 4 low, bit 5 high).
        jp.write(0x20);
        // Down is bit 3 in the low nibble.
        assert_eq!(jp.read() & 0x0F, 0x0F & !(1 << 3));
    }

    #[test]
    fn press_requests_interrupt() {
        let mut jp = Joypad::default();
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        jp.write(0x10); // select action group
        jp.set_keys(button::START, &mut irq);
        assert_eq!(irq.pending() & Interrupt::Joypad.mask(), Interrupt::Joypad.mask());
    }

    #[test]
    fn unselected_groups_read_high() {
        let mut jp = Joypad::default();
        let mut irq = Irq::new();
        jp.set_keys(0xFF, &mut irq);
        jp.write(0x30); // neither group selected
        assert_eq!(jp.read() & 0x0F, 0x0F);
    }
}
