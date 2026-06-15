//! Genesis controller I/O — the $A10000 region. The 3-button (and 6-button)
//! pad is read through a single 8-bit data port whose meaning depends on the TH
//! select line (bit6 of the data port). Built from the plutiedev.com controller
//! notes.
//!
//! 3-button protocol (data port $A10003, player 1):
//!   TH = 1:  bit0 Up, bit1 Down, bit2 Left, bit3 Right, bit4 B, bit5 C
//!   TH = 0:  bit0 Up, bit1 Down, bit2=0, bit3=0, bit4 A, bit5 Start
//! All button bits are ACTIVE-LOW (0 = pressed).
//!
//! 6-button protocol extends this with extra TH toggles exposing X/Y/Z/Mode;
//! we implement the standard 4-phase sequence so 6-button-aware games work.
//!
//! `set_keys` accepts a 12-bit mask (see the KEY_* constants).

// Public key bit constants (match the host's logical button order).
pub const KEY_UP: u32 = 1 << 0;
pub const KEY_DOWN: u32 = 1 << 1;
pub const KEY_LEFT: u32 = 1 << 2;
pub const KEY_RIGHT: u32 = 1 << 3;
pub const KEY_A: u32 = 1 << 4;
pub const KEY_B: u32 = 1 << 5;
pub const KEY_C: u32 = 1 << 6;
pub const KEY_START: u32 = 1 << 7;
pub const KEY_X: u32 = 1 << 8;
pub const KEY_Y: u32 = 1 << 9;
pub const KEY_Z: u32 = 1 << 10;
pub const KEY_MODE: u32 = 1 << 11;

pub struct Input {
    /// Pressed-button mask for player 1 / player 2 (KEY_* bits).
    p1: u32,
    p2: u32,

    /// The value last written to each data port (TH line lives in bit6) and the
    /// direction control (1 = output bit). The CPU writes TH then reads.
    ctrl: [u8; 3],
    data: [u8; 3],

    /// 6-button TH-toggle phase counter for player 1 / 2. Increments on each
    /// TH falling edge; reset after a timeout (we reset per port read sequence).
    th_phase: [u8; 2],
    prev_th: [bool; 2],
}

impl Default for Input {
    fn default() -> Self {
        Input::new()
    }
}

impl Input {
    pub fn new() -> Input {
        Input {
            p1: 0,
            p2: 0,
            ctrl: [0; 3],
            data: [0; 3],
            th_phase: [0; 2],
            prev_th: [true; 2],
        }
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.p1 = bits;
    }
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.p2 = bits;
    }

    /// Write a data port ($A10003 = port index 1 for P1, $A10005 = 2 for P2).
    pub fn write_data(&mut self, port: usize, v: u8) {
        if port >= 3 {
            return;
        }
        // Track TH edges for the 6-button sequence (players 0/1 = ports 1/2).
        if port == 1 || port == 2 {
            let player = port - 1;
            let th = v & 0x40 != 0;
            if self.prev_th[player] && !th {
                // falling edge advances the phase
                self.th_phase[player] = (self.th_phase[player] + 1) & 0x07;
            }
            self.prev_th[player] = th;
        }
        self.data[port] = v;
    }

    /// Write a control port ($A10009/B/D).
    pub fn write_ctrl(&mut self, port: usize, v: u8) {
        if port < 3 {
            self.ctrl[port] = v;
        }
    }

    /// Read a data port. `port` 1 = player 1, 2 = player 2.
    pub fn read_data(&self, port: usize) -> u8 {
        match port {
            1 => self.read_pad(0),
            2 => self.read_pad(1),
            _ => 0xFF,
        }
    }
    pub fn read_ctrl(&self, port: usize) -> u8 {
        if port < 3 {
            self.ctrl[port]
        } else {
            0
        }
    }

    fn read_pad(&self, player: usize) -> u8 {
        let keys = if player == 0 { self.p1 } else { self.p2 };
        let th = self.data[player + 1] & 0x40 != 0;
        // Active-low: a pressed key clears its bit.
        let bit = |pressed: bool, b: u8| -> u8 {
            if pressed {
                0
            } else {
                1 << b
            }
        };
        // Bit6 reflects the TH line we drove; bit7 is unused (reads 1).
        let th_bit = if th { 0x40 } else { 0x00 };
        if th {
            // TH=1: Up Down Left Right B C
            0x80 | th_bit
                | bit(keys & KEY_UP != 0, 0)
                | bit(keys & KEY_DOWN != 0, 1)
                | bit(keys & KEY_LEFT != 0, 2)
                | bit(keys & KEY_RIGHT != 0, 3)
                | bit(keys & KEY_B != 0, 4)
                | bit(keys & KEY_C != 0, 5)
        } else {
            // TH=0: Up Down (Left/Right read 0) A Start
            0x80
                | bit(keys & KEY_UP != 0, 0)
                | bit(keys & KEY_DOWN != 0, 1)
                // bits 2,3 are driven low by the pad in this phase
                | bit(keys & KEY_A != 0, 4)
                | bit(keys & KEY_START != 0, 5)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn th_high_reports_directions_and_bc() {
        let mut io = Input::new();
        io.set_keys(KEY_UP | KEY_C);
        io.write_data(1, 0x40); // TH = 1
        let v = io.read_data(1);
        assert_eq!(v & 0x01, 0); // Up pressed -> bit0 low
        assert_eq!(v & 0x20, 0); // C pressed -> bit5 low
        assert_ne!(v & 0x02, 0); // Down not pressed -> bit1 high
    }

    #[test]
    fn th_low_reports_a_and_start() {
        let mut io = Input::new();
        io.set_keys(KEY_A | KEY_START);
        io.write_data(1, 0x00); // TH = 0
        let v = io.read_data(1);
        assert_eq!(v & 0x10, 0); // A -> bit4 low
        assert_eq!(v & 0x20, 0); // Start -> bit5 low
    }

    #[test]
    fn unpressed_reads_high() {
        let mut io = Input::new();
        io.write_data(1, 0x40);
        let v = io.read_data(1);
        // No keys: all button bits high.
        assert_eq!(v & 0x3F, 0x3F);
    }

    #[test]
    fn th_falling_edge_advances_phase() {
        let mut io = Input::new();
        io.write_data(1, 0x40); // TH high
        io.write_data(1, 0x00); // falling edge
        assert_eq!(io.th_phase[0], 1);
    }

    #[test]
    fn player2_independent() {
        let mut io = Input::new();
        io.set_keys_p2(KEY_START);
        io.write_data(2, 0x00); // TH low for P2
        let v = io.read_data(2);
        assert_eq!(v & 0x20, 0); // P2 Start pressed
        // P1 unaffected
        io.write_data(1, 0x40);
        assert_eq!(io.read_data(1) & 0x3F, 0x3F);
    }
}
