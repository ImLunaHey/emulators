//! Standard NES controller — the $4016/$4017 strobe/shift protocol.
//!
//! Spec: NESdev wiki "Standard controller". Writing bit0 of $4016 strobes
//! both controllers; while strobe is high they continuously reload the
//! current button state. Reading $4016/$4017 returns the next button bit
//! (LSB first) in bit0 and shifts. After 8 reads the shift register returns 1s
//! (open bus) on real hardware.
//!
//! Button bit order in `set_keys`: bit0 A, bit1 B, bit2 Select, bit3 Start,
//! bit4 Up, bit5 Down, bit6 Left, bit7 Right.

pub const BTN_A: u8 = 1 << 0;
pub const BTN_B: u8 = 1 << 1;
pub const BTN_SELECT: u8 = 1 << 2;
pub const BTN_START: u8 = 1 << 3;
pub const BTN_UP: u8 = 1 << 4;
pub const BTN_DOWN: u8 = 1 << 5;
pub const BTN_LEFT: u8 = 1 << 6;
pub const BTN_RIGHT: u8 = 1 << 7;

#[derive(Default)]
pub struct Controllers {
    /// Latched button state for the two ports.
    state: [u8; 2],
    /// Shift register snapshot taken on strobe.
    shift: [u8; 2],
    strobe: bool,
}

impl Controllers {
    pub fn new() -> Controllers {
        Controllers::default()
    }

    /// Set the live button mask for a port (0 or 1).
    pub fn set_keys(&mut self, port: usize, buttons: u8) {
        if port < 2 {
            self.state[port] = buttons;
            if self.strobe {
                self.shift[port] = buttons;
            }
        }
    }

    /// Write to $4016 (the strobe latch lives in bit0).
    pub fn write_strobe(&mut self, v: u8) {
        self.strobe = v & 1 != 0;
        if self.strobe {
            self.shift = self.state;
        }
    }

    /// Read $4016 (port 0) or $4017 (port 1). Returns bit0 = next button.
    pub fn read(&mut self, port: usize) -> u8 {
        if port >= 2 {
            return 0;
        }
        if self.strobe {
            // While strobing, always return the A button.
            return self.state[port] & 1;
        }
        let bit = self.shift[port] & 1;
        self.shift[port] = (self.shift[port] >> 1) | 0x80; // shift in 1s
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_returns_buttons_in_order() {
        let mut c = Controllers::new();
        // A + Start pressed (bit0 and bit3).
        c.set_keys(0, BTN_A | BTN_START);
        c.write_strobe(1);
        c.write_strobe(0);
        let bits: Vec<u8> = (0..8).map(|_| c.read(0)).collect();
        assert_eq!(bits, vec![1, 0, 0, 1, 0, 0, 0, 0]);
        // 9th+ read returns 1 (open bus).
        assert_eq!(c.read(0), 1);
    }

    #[test]
    fn strobe_high_reads_a() {
        let mut c = Controllers::new();
        c.set_keys(0, BTN_A);
        c.write_strobe(1); // strobe stays high
        assert_eq!(c.read(0), 1);
        assert_eq!(c.read(0), 1);
    }
}
