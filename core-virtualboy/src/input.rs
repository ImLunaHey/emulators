//! Virtual Boy controller. The VB pad has two D-pads (left + right), A/B, the
//! L/R shoulder triggers, and Start/Select. The hardware reads the 16-bit
//! button state through the serial controller registers (SDLR/SDHR at
//! 0x02000010 / 0x02000014); bit 1 (the low "battery low" / always-1 bit) and
//! bit 0 reads back as a fixed pattern.
//!
//! Hardware bit layout of the 16-bit read (SDHR:SDLR):
//!   bit  0  reserved (reads 0)
//!   bit  1  always 1 (signature)
//!   bit  2  R  trigger (right shoulder)
//!   bit  3  L  trigger (left shoulder)
//!   bit  4  R-pad Right
//!   bit  5  R-pad Left
//!   bit  6  R-pad Down
//!   bit  7  R-pad Up
//!   bit  8  Start
//!   bit  9  Select
//!   bit 10  B
//!   bit 11  A
//!   bit 12  L-pad Right
//!   bit 13  L-pad Left
//!   bit 14  L-pad Down
//!   bit 15  L-pad Up
//!
//! Unlike the SMS pads, VB buttons are active-HIGH in the hardware read.
//!
//! `set_keys` accepts a logical bitmask (see the `KEY_*` constants) and maps it
//! to the hardware bit positions.

// Logical key bits used by the host/wasm surface.
pub const KEY_LU: u32 = 1 << 0; // left D-pad up
pub const KEY_LD: u32 = 1 << 1;
pub const KEY_LL: u32 = 1 << 2;
pub const KEY_LR: u32 = 1 << 3;
pub const KEY_RU: u32 = 1 << 4; // right D-pad up
pub const KEY_RD: u32 = 1 << 5;
pub const KEY_RL: u32 = 1 << 6;
pub const KEY_RR: u32 = 1 << 7;
pub const KEY_A: u32 = 1 << 8;
pub const KEY_B: u32 = 1 << 9;
pub const KEY_L: u32 = 1 << 10; // left trigger
pub const KEY_R: u32 = 1 << 11; // right trigger
pub const KEY_START: u32 = 1 << 12;
pub const KEY_SELECT: u32 = 1 << 13;

pub struct Input {
    /// Logical key state (KEY_* bits).
    keys: u32,
}

impl Default for Input {
    fn default() -> Self {
        Input::new()
    }
}

impl Input {
    pub fn new() -> Input {
        Input { keys: 0 }
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.keys = bits;
    }

    /// Build the 16-bit hardware controller word (active-high, with the bit-1
    /// signature always set).
    pub fn hw_state(&self) -> u16 {
        let k = self.keys;
        let mut v: u16 = 0x0002; // bit1 signature
        macro_rules! map {
            ($logical:expr, $hwbit:expr) => {
                if k & $logical != 0 {
                    v |= 1 << $hwbit;
                }
            };
        }
        map!(KEY_R, 2);
        map!(KEY_L, 3);
        map!(KEY_RR, 4);
        map!(KEY_RL, 5);
        map!(KEY_RD, 6);
        map!(KEY_RU, 7);
        map!(KEY_START, 8);
        map!(KEY_SELECT, 9);
        map!(KEY_B, 10);
        map!(KEY_A, 11);
        map!(KEY_LR, 12);
        map!(KEY_LL, 13);
        map!(KEY_LD, 14);
        map!(KEY_LU, 15);
        v
    }

    pub fn sdlr(&self) -> u8 {
        (self.hw_state() & 0xFF) as u8
    }
    pub fn sdhr(&self) -> u8 {
        (self.hw_state() >> 8) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_bit_always_set() {
        let inp = Input::new();
        assert_eq!(inp.hw_state() & 0x0002, 0x0002);
    }

    #[test]
    fn a_button_maps_to_bit11() {
        let mut inp = Input::new();
        inp.set_keys(KEY_A);
        assert_eq!(inp.hw_state() & (1 << 11), 1 << 11);
    }

    #[test]
    fn start_maps_to_bit8() {
        let mut inp = Input::new();
        inp.set_keys(KEY_START);
        assert_eq!(inp.sdhr() & 0x01, 0x01);
    }
}
