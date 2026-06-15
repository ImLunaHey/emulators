//! SNES controller input. The standard pad has 12 buttons; the auto-joypad
//! read latches a 16-bit shift register per controller, MSB first:
//!
//!   bit15 B, 14 Y, 13 Select, 12 Start, 11 Up, 10 Down, 9 Left, 8 Right,
//!   bit7 A, 6 X, 5 L, 4 R, 3-0 always 0 (signature).
//!
//! The host `set_keys(bits)` packs buttons in a friendly order (see [`Key`]);
//! we translate to the hardware shift-register layout here.

/// Host-facing button bit positions (the order the launcher uses across cores).
#[derive(Clone, Copy)]
pub enum Key {
    B = 0,
    Y = 1,
    Select = 2,
    Start = 3,
    Up = 4,
    Down = 5,
    Left = 6,
    Right = 7,
    A = 8,
    X = 9,
    L = 10,
    R = 11,
}

#[derive(Default)]
pub struct Controllers {
    /// Host button bitmask per port (using [`Key`] bit positions).
    pub host: [u32; 2],
    /// 16-bit hardware shift register per port, latched at auto-read time.
    shift: [u16; 2],
    /// $4016 strobe latch for the manual serial-read path.
    strobe: bool,
}

impl Controllers {
    pub fn new() -> Controllers {
        Controllers::default()
    }

    pub fn set_keys(&mut self, port: usize, bits: u32) {
        if port < 2 {
            self.host[port] = bits;
        }
    }

    /// Translate the host bitmask into the hardware MSB-first shift register.
    fn hw_bits(host: u32) -> u16 {
        let mut v = 0u16;
        let b = |k: Key| (host >> (k as u32)) & 1 == 1;
        if b(Key::B) {
            v |= 1 << 15;
        }
        if b(Key::Y) {
            v |= 1 << 14;
        }
        if b(Key::Select) {
            v |= 1 << 13;
        }
        if b(Key::Start) {
            v |= 1 << 12;
        }
        if b(Key::Up) {
            v |= 1 << 11;
        }
        if b(Key::Down) {
            v |= 1 << 10;
        }
        if b(Key::Left) {
            v |= 1 << 9;
        }
        if b(Key::Right) {
            v |= 1 << 8;
        }
        if b(Key::A) {
            v |= 1 << 7;
        }
        if b(Key::X) {
            v |= 1 << 6;
        }
        if b(Key::L) {
            v |= 1 << 5;
        }
        if b(Key::R) {
            v |= 1 << 4;
        }
        v
    }

    /// Latch the current button states into both shift registers. Called by the
    /// orchestrator at the start of auto-joypad read each frame.
    pub fn latch(&mut self) {
        self.shift[0] = Self::hw_bits(self.host[0]);
        self.shift[1] = Self::hw_bits(self.host[1]);
    }

    /// The 16-bit auto-read value for a port ($4218/$421A etc.).
    pub fn auto_read(&self, port: usize) -> u16 {
        self.shift[port & 1]
    }

    // ---- manual serial path ($4016 write strobe / $4016-$4017 reads) ----
    pub fn write_strobe(&mut self, v: u8) {
        self.strobe = v & 1 == 1;
        if self.strobe {
            self.latch();
        }
    }

    /// Read one serial bit from a port (LSB of the returned byte). Each read
    /// shifts the register left (MSB-first), returning 0s after 16 reads.
    pub fn read_serial(&mut self, port: usize) -> u8 {
        let p = port & 1;
        if self.strobe {
            // While strobed, always return the current B-button state.
            return ((Self::hw_bits(self.host[p]) >> 15) & 1) as u8;
        }
        let bit = (self.shift[p] >> 15) & 1;
        self.shift[p] = self.shift[p].wrapping_shl(1) | 1; // shift in 1s after exhaustion
        bit as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_read_layout() {
        let mut c = Controllers::new();
        // Press B and Start.
        c.set_keys(0, (1 << Key::B as u32) | (1 << Key::Start as u32));
        c.latch();
        let v = c.auto_read(0);
        assert_eq!(v & (1 << 15), 1 << 15); // B
        assert_eq!(v & (1 << 12), 1 << 12); // Start
        assert_eq!(v & (1 << 14), 0); // Y not pressed
    }

    #[test]
    fn serial_shift_order() {
        let mut c = Controllers::new();
        c.set_keys(0, 1 << Key::B as u32);
        c.write_strobe(1);
        c.write_strobe(0);
        // First serial read = B (bit15) = 1.
        assert_eq!(c.read_serial(0), 1);
        // Subsequent reads = 0 until we reach the exhausted region.
        assert_eq!(c.read_serial(0), 0);
    }
}
