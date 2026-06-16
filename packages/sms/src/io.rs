//! Controller input + the SMS/GG I/O control register. Built from the SMS
//! Power! "Peripherals" + "Ports" documentation.
//!
//! Controller ports $DC and $DD return the two D-pads + buttons, ACTIVE LOW
//! (a 0 bit means pressed). The SMS has two 6-button-equivalent pads
//! (Up/Down/Left/Right + button 1 + button 2). The Game Gear has a single
//! pad whose Start button lives in a GG-only register at port $00.
//!
//! Port $DC (controller port A/B data):
//!   bit0 P1 Up    bit1 P1 Down  bit2 P1 Left  bit3 P1 Right
//!   bit4 P1 B1    bit5 P1 B2    bit6 P2 Up    bit7 P2 Down
//! Port $DD (controller port B/misc):
//!   bit0 P2 Left  bit1 P2 Right bit2 P2 B1    bit3 P2 B2
//!   bit4 Reset    bit5 (unused) bit6 TH-A     bit7 TH-B
//!
//! `set_keys` accepts an ACTIVE-HIGH bitmask (1 = pressed) which we invert.
//! Bit order of the `set_keys` mask (per joypad, this core uses joypad 1):
//!   bit0 Up  bit1 Down  bit2 Left  bit3 Right  bit4 B1  bit5 B2
//!   bit6 Start/Pause
//!
//! Pause on the SMS is wired to the Z80 NMI (handled in `sms.rs`); on the GG
//! Start is read from the GG START register ($00, bit7, active low).

pub const KEY_UP: u32 = 1 << 0;
pub const KEY_DOWN: u32 = 1 << 1;
pub const KEY_LEFT: u32 = 1 << 2;
pub const KEY_RIGHT: u32 = 1 << 3;
pub const KEY_B1: u32 = 1 << 4; // "1" / button A
pub const KEY_B2: u32 = 1 << 5; // "2" / button B
pub const KEY_START: u32 = 1 << 6; // GG Start / SMS Pause

#[derive(Default)]
pub struct Input {
    /// Active-high pressed mask for player 1.
    p1: u32,
    /// Active-high pressed mask for player 2 (SMS only).
    p2: u32,
}

impl Input {
    pub fn new() -> Input {
        Input::default()
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.p1 = bits;
    }
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.p2 = bits;
    }

    /// Port $DC value (active low).
    pub fn port_dc(&self) -> u8 {
        let mut v = 0xFFu8;
        if self.p1 & KEY_UP != 0 {
            v &= !0x01;
        }
        if self.p1 & KEY_DOWN != 0 {
            v &= !0x02;
        }
        if self.p1 & KEY_LEFT != 0 {
            v &= !0x04;
        }
        if self.p1 & KEY_RIGHT != 0 {
            v &= !0x08;
        }
        if self.p1 & KEY_B1 != 0 {
            v &= !0x10;
        }
        if self.p1 & KEY_B2 != 0 {
            v &= !0x20;
        }
        if self.p2 & KEY_UP != 0 {
            v &= !0x40;
        }
        if self.p2 & KEY_DOWN != 0 {
            v &= !0x80;
        }
        v
    }

    /// Port $DD value (active low). The reset line (bit4) reads released (1).
    pub fn port_dd(&self) -> u8 {
        let mut v = 0xFFu8;
        if self.p2 & KEY_LEFT != 0 {
            v &= !0x01;
        }
        if self.p2 & KEY_RIGHT != 0 {
            v &= !0x02;
        }
        if self.p2 & KEY_B1 != 0 {
            v &= !0x04;
        }
        if self.p2 & KEY_B2 != 0 {
            v &= !0x08;
        }
        v
    }

    /// Game Gear START register ($00): bit7 = Start (active low), bit6 = region
    /// (NTSC). The other bits read 1.
    pub fn gg_start(&self) -> u8 {
        let mut v = 0xFFu8;
        if self.p1 & KEY_START != 0 {
            v &= !0x80;
        }
        v
    }

    /// True if the SMS Pause (mapped to KEY_START) is currently pressed —
    /// `sms.rs` edge-detects this into an NMI.
    pub fn pause_pressed(&self) -> bool {
        self.p1 & KEY_START != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_active_low() {
        let mut io = Input::new();
        io.set_keys(KEY_UP | KEY_B1);
        let v = io.port_dc();
        assert_eq!(v & 0x01, 0); // up pressed -> 0
        assert_eq!(v & 0x10, 0); // b1 pressed -> 0
        assert_ne!(v & 0x02, 0); // down released -> 1
    }

    #[test]
    fn dd_player2() {
        let mut io = Input::new();
        io.set_keys_p2(KEY_LEFT);
        assert_eq!(io.port_dd() & 0x01, 0);
    }

    #[test]
    fn gg_start_bit() {
        let mut io = Input::new();
        io.set_keys(KEY_START);
        assert_eq!(io.gg_start() & 0x80, 0);
        assert!(io.pause_pressed());
    }
}
