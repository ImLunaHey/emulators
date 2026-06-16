//! Interrupt scaffold for the LR35902.
//!
//! Spec: Pan Docs — Interrupts (gbdev.io/pandocs/Interrupts.html). The CPU has
//! a master enable (`IME`), an enable mask `IE` at 0xFFFF, and a request flag
//! `IF` at 0xFF0F. There are five interrupt sources, each with a fixed handler
//! vector in the 0x40..=0x60 range; lower bit index = higher priority.
//!
//! On service the standard sequence is: clear IME, clear the serviced bit in
//! IF, push the current PC onto the stack (high byte then low byte), and jump
//! to the vector. This file owns the IE/IF registers and the priority/vector
//! decoding; the actual stack push + PC jump live in the CPU dispatch helper
//! (cpu::state), which calls [`Irq::highest_priority`].

/// The five interrupt kinds, ordered by priority bit (bit 0 = highest).
///
/// Closed enum, exhaustively matched everywhere — no catch-all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// Bit 0 — V-Blank. Vector 0x40.
    VBlank,
    /// Bit 1 — LCD STAT. Vector 0x48.
    Stat,
    /// Bit 2 — Timer overflow. Vector 0x50.
    Timer,
    /// Bit 3 — Serial transfer complete. Vector 0x58.
    Serial,
    /// Bit 4 — Joypad. Vector 0x60.
    Joypad,
}

impl Interrupt {
    /// The IE/IF bit index for this interrupt (0..=4).
    #[inline]
    pub const fn bit(self) -> u8 {
        match self {
            Interrupt::VBlank => 0,
            Interrupt::Stat => 1,
            Interrupt::Timer => 2,
            Interrupt::Serial => 3,
            Interrupt::Joypad => 4,
        }
    }

    /// The IE/IF mask for this interrupt.
    #[inline]
    pub const fn mask(self) -> u8 {
        1 << self.bit()
    }

    /// The handler vector address this interrupt jumps to.
    #[inline]
    pub const fn vector(self) -> u16 {
        match self {
            Interrupt::VBlank => 0x40,
            Interrupt::Stat => 0x48,
            Interrupt::Timer => 0x50,
            Interrupt::Serial => 0x58,
            Interrupt::Joypad => 0x60,
        }
    }

    /// Interrupts in descending priority order (VBlank first).
    pub const ALL: [Interrupt; 5] = [
        Interrupt::VBlank,
        Interrupt::Stat,
        Interrupt::Timer,
        Interrupt::Serial,
        Interrupt::Joypad,
    ];
}

/// IE (0xFFFF) + IF (0xFF0F). Only the low 5 bits are meaningful; the upper 3
/// bits of IF read back as 1 on hardware, so we store them set.
#[derive(Clone, Copy)]
pub struct Irq {
    /// Interrupt Enable mask (0xFFFF). Bits 0-4 used; upper bits ignored.
    pub ie: u8,
    /// Interrupt Flag / request (0xFF0F). Bits 0-4 used; bits 5-7 read as 1.
    pub iflag: u8,
}

impl Default for Irq {
    fn default() -> Self {
        Self::new()
    }
}

/// Bits 5-7 of IF are unimplemented and read back as 1.
const IF_UNUSED: u8 = 0xE0;

impl Irq {
    pub fn new() -> Self {
        Irq {
            ie: 0,
            iflag: IF_UNUSED,
        }
    }

    /// Raise an interrupt request (set its bit in IF).
    #[inline]
    pub fn request(&mut self, int: Interrupt) {
        self.iflag |= int.mask();
    }

    /// Acknowledge / clear a serviced interrupt's request bit.
    #[inline]
    pub fn acknowledge(&mut self, int: Interrupt) {
        self.iflag &= !int.mask();
    }

    /// CPU read of IE (0xFFFF). All 8 bits are readable.
    #[inline]
    pub fn read_ie(&self) -> u8 {
        self.ie
    }
    /// CPU write of IE (0xFFFF). All 8 bits writable.
    #[inline]
    pub fn write_ie(&mut self, v: u8) {
        self.ie = v;
    }

    /// CPU read of IF (0xFF0F). Upper 3 bits read as 1.
    #[inline]
    pub fn read_if(&self) -> u8 {
        self.iflag | IF_UNUSED
    }
    /// CPU write of IF (0xFF0F). Upper 3 bits stay set.
    #[inline]
    pub fn write_if(&mut self, v: u8) {
        self.iflag = (v & 0x1F) | IF_UNUSED;
    }

    /// Bits that are both enabled (IE) and requested (IF), masked to the 5
    /// real interrupts. Nonzero means an interrupt is pending regardless of
    /// IME — used to wake from HALT (HALT exits on any pending IE&IF even when
    /// IME is clear).
    #[inline]
    pub fn pending(&self) -> u8 {
        self.ie & self.iflag & 0x1F
    }

    /// The highest-priority pending interrupt (enabled AND requested), or None.
    /// Priority is by ascending bit index (VBlank wins ties).
    #[inline]
    pub fn highest_priority(&self) -> Option<Interrupt> {
        let p = self.pending();
        if p == 0 {
            return None;
        }
        Interrupt::ALL.into_iter().find(|int| p & int.mask() != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_and_bits() {
        assert_eq!(Interrupt::VBlank.vector(), 0x40);
        assert_eq!(Interrupt::Joypad.vector(), 0x60);
        assert_eq!(Interrupt::Timer.bit(), 2);
        assert_eq!(Interrupt::Stat.mask(), 0b0000_0010);
    }

    #[test]
    fn priority_is_vblank_first() {
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        irq.request(Interrupt::Joypad);
        irq.request(Interrupt::VBlank);
        assert_eq!(irq.highest_priority(), Some(Interrupt::VBlank));
        irq.acknowledge(Interrupt::VBlank);
        assert_eq!(irq.highest_priority(), Some(Interrupt::Joypad));
    }

    #[test]
    fn if_upper_bits_read_set() {
        let mut irq = Irq::new();
        irq.write_if(0x00);
        assert_eq!(irq.read_if(), 0xE0);
    }

    #[test]
    fn pending_ignores_ime() {
        let mut irq = Irq::new();
        irq.write_ie(0x04);
        irq.request(Interrupt::Timer);
        assert_eq!(irq.pending(), 0x04);
    }
}
