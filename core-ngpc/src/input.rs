//! Controller input. Built from the NeoPop-Core "Memory Map" notes and the
//! ngpcspec hardware doc.
//!
//! The NGPC has a single D-pad + A + B + Option. The BIOS exposes the button
//! state at system register `0x6F82`, ACTIVE HIGH (a 1 bit means pressed):
//!   bit0 Up  bit1 Down  bit2 Left  bit3 Right  bit4 A  bit5 B  bit6 Option
//!   bit7 unused
//! (The physical GPIO lines are active-low; the value at 0x6F82 is the
//! normalized "pressed = set" status the BIOS maintains, and that is what we
//! emulate since we HLE the input register.)
//!
//! `set_keys` takes the same ACTIVE-HIGH bitmask, so the mapping is 1:1.

pub const KEY_UP: u32 = 1 << 0;
pub const KEY_DOWN: u32 = 1 << 1;
pub const KEY_LEFT: u32 = 1 << 2;
pub const KEY_RIGHT: u32 = 1 << 3;
pub const KEY_A: u32 = 1 << 4;
pub const KEY_B: u32 = 1 << 5;
pub const KEY_OPTION: u32 = 1 << 6;

#[derive(Default)]
pub struct Input {
    /// Active-high pressed mask.
    pressed: u32,
}

impl Input {
    pub fn new() -> Input {
        Input::default()
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.pressed = bits;
    }

    /// Value read at system register `0x6F82` (active high). Bit7 reads 0.
    pub fn register(&self) -> u8 {
        (self.pressed & 0x7F) as u8
    }

    /// True if Option is held (some games use it like a Start/Pause).
    pub fn option_pressed(&self) -> bool {
        self.pressed & KEY_OPTION != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_active_high() {
        let mut io = Input::new();
        io.set_keys(KEY_UP | KEY_A);
        let v = io.register();
        assert_eq!(v & 0x01, 0x01); // up pressed -> 1
        assert_eq!(v & 0x10, 0x10); // A pressed -> 1
        assert_eq!(v & 0x02, 0x00); // down released -> 0
    }

    #[test]
    fn bit7_clear() {
        let mut io = Input::new();
        io.set_keys(0xFFFF_FFFF);
        assert_eq!(io.register() & 0x80, 0);
    }

    #[test]
    fn option_helper() {
        let mut io = Input::new();
        io.set_keys(KEY_OPTION);
        assert!(io.option_pressed());
    }
}
