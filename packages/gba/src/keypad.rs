// GBA keypad — 10-bit register; 0 = pressed, 1 = released.
//  A  B  Sel Sta R  L  U  D  Rs Ls
// bit0 1  2   3  4  5  6  7  8  9

// NOT `const enum` so we can do reverse name lookups (`Key[k]`) — the
// gamepad/UI code uses string names ("A", "UP") for accessibility
// labels and remapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Key {
    A = 0,
    B = 1,
    Select = 2,
    Start = 3,
    Right = 4,
    Left = 5,
    Up = 6,
    Down = 7,
    R = 8,
    L = 9,
}

pub struct Keypad {
    // Logical held bitmask (1 = held). Inverted on read to match the GBA's
    // "released" polarity. The UI highlight reads this directly so a held
    // turbo button stays lit even while it autofires.
    pub pressed: u32,
    // Keys that autofire while held: the game sees them pressed only on the
    // "on" phase, which tickTurbo() flips once per emulated frame (~30 Hz).
    pub turbo_mask: u32,
    turbo_phase: u32,
}

impl Default for Keypad {
    fn default() -> Self {
        Self {
            pressed: 0,
            turbo_mask: 0,
            turbo_phase: 0,
        }
    }
}

impl Keypad {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&mut self, k: Key) {
        self.pressed |= 1 << (k as u32);
    }

    pub fn release(&mut self, k: Key) {
        self.pressed &= !(1 << (k as u32));
    }

    // Advance the autofire phase — call once per emulated frame.
    pub fn tick_turbo(&mut self) {
        self.turbo_phase ^= 1;
    }

    pub fn read16(&self) -> u32 {
        let mut effective = self.pressed;
        // On the "off" phase, drop the held turbo keys so they read released.
        if self.turbo_mask != 0 && self.turbo_phase == 0 {
            effective &= !self.turbo_mask;
        }
        (!effective) & 0x3FF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_all_released() {
        let kp = Keypad::new();
        assert_eq!(kp.read16(), 0x3FF);
    }

    #[test]
    fn press_clears_bit_on_read() {
        let mut kp = Keypad::new();
        kp.press(Key::A);
        assert_eq!(kp.read16(), 0x3FF & !(1 << 0));
        kp.release(Key::A);
        assert_eq!(kp.read16(), 0x3FF);
    }

    #[test]
    fn turbo_keys_pulse_with_phase() {
        let mut kp = Keypad::new();
        kp.press(Key::B);
        kp.turbo_mask = 1 << (Key::B as u32);
        // phase 0 (off): the turbo key reads released.
        assert_eq!(kp.read16(), 0x3FF);
        // phase 1 (on): the turbo key reads pressed.
        kp.tick_turbo();
        assert_eq!(kp.read16(), 0x3FF & !(1 << 1));
    }
}
