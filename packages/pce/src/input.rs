//! Standard PC Engine pad — the 2-bit SEL/CLR multiplexed joypad protocol.
//!
//! Spec: Archaic Pixels "Joypad", pcedev wiki "I/O port". The pad connects to
//! the I/O port at physical $1FF000 (logical depends on the MMU mapping of bank
//! $FF). The CPU writes two control bits to the port:
//!   - bit0 = SEL  (selects which nibble of the pad is read)
//!   - bit1 = CLR  (clear / reset the multiplexer)
//!
//! It reads back the low nibble (bits 0-3, active LOW). With SEL = 0 the
//! directional nibble is returned (bit0 Up, bit1 Right, bit2 Down, bit3 Left);
//! with SEL = 1 the button nibble is returned (bit0 I, bit1 II, bit2 Select,
//! bit3 Run).
//!
//! `set_keys` mask bit order (matches the host launcher):
//!   bit0 Up, bit1 Down, bit2 Left, bit3 Right,
//!   bit4 I (button 1), bit5 II (button 2), bit6 Select, bit7 Run.

pub const KEY_UP: u32 = 1 << 0;
pub const KEY_DOWN: u32 = 1 << 1;
pub const KEY_LEFT: u32 = 1 << 2;
pub const KEY_RIGHT: u32 = 1 << 3;
pub const KEY_I: u32 = 1 << 4;
pub const KEY_II: u32 = 1 << 5;
pub const KEY_SELECT: u32 = 1 << 6;
pub const KEY_RUN: u32 = 1 << 7;

#[derive(Default)]
pub struct Input {
    /// Live `set_keys` button mask (see bit order above).
    keys: u32,
    /// Latched SEL line (bit0 of the last port write).
    sel: bool,
    /// Latched CLR line (bit1 of the last port write).
    clr: bool,
}

impl Input {
    pub fn new() -> Input {
        Input::default()
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.keys = bits;
    }

    /// CPU write to the joypad port: bit0 = SEL, bit1 = CLR.
    pub fn write_port(&mut self, v: u8) {
        self.sel = v & 0x01 != 0;
        self.clr = v & 0x02 != 0;
    }

    /// CPU read of the joypad port: the low nibble carries the selected pad
    /// nibble (active LOW — a pressed button reads 0). The high nibble reads as
    /// the country/clock bits; we return 0 there (bit6 = 0 => Japanese, which is
    /// what most HuCards expect; bit7 = CD presence = 0 / absent).
    pub fn read_port(&self) -> u8 {
        // While CLR is asserted the pad outputs all-released (the multiplexer is
        // held in reset).
        if self.clr {
            return 0x0F;
        }
        let pressed = |k: u32| self.keys & k != 0;
        let nibble = if self.sel {
            // Buttons: I, II, Select, Run.
            let mut n = 0u8;
            if pressed(KEY_I) { n |= 0x01; }
            if pressed(KEY_II) { n |= 0x02; }
            if pressed(KEY_SELECT) { n |= 0x04; }
            if pressed(KEY_RUN) { n |= 0x08; }
            n
        } else {
            // Directions: Up, Right, Down, Left.
            let mut n = 0u8;
            if pressed(KEY_UP) { n |= 0x01; }
            if pressed(KEY_RIGHT) { n |= 0x02; }
            if pressed(KEY_DOWN) { n |= 0x04; }
            if pressed(KEY_LEFT) { n |= 0x08; }
            n
        };
        // Active low: invert the pressed bits into the low nibble.
        (!nibble) & 0x0F
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_nibble_active_low() {
        let mut inp = Input::new();
        inp.set_keys(KEY_UP);
        inp.write_port(0x00); // SEL=0 -> directions, CLR=0
        // Up is bit0; pressed => bit0 reads 0, others 1 -> 0b1110.
        assert_eq!(inp.read_port() & 0x0F, 0x0E);
    }

    #[test]
    fn button_nibble_selected() {
        let mut inp = Input::new();
        inp.set_keys(KEY_I | KEY_RUN);
        inp.write_port(0x01); // SEL=1 -> buttons
        // I = bit0, Run = bit3 pressed -> those bits 0 -> 0b0110.
        assert_eq!(inp.read_port() & 0x0F, 0x06);
    }

    #[test]
    fn clr_holds_released() {
        let mut inp = Input::new();
        inp.set_keys(KEY_UP | KEY_I);
        inp.write_port(0x02); // CLR=1
        assert_eq!(inp.read_port() & 0x0F, 0x0F); // all released
    }

    #[test]
    fn nothing_pressed_reads_all_ones() {
        let mut inp = Input::new();
        inp.write_port(0x00);
        assert_eq!(inp.read_port() & 0x0F, 0x0F);
    }
}
