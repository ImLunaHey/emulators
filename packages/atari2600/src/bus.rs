//! The CPU memory interface. The 6507 is byte-addressed; the bus is
//! `read8`/`write8` over a 16-bit address space (only the low 13 bits are wired
//! on the real chip — [`Atari`](crate::Atari) masks the address itself). The
//! production implementor is `Atari` (see `atari.rs`); CPU unit tests use a
//! flat-RAM stub.
//!
//! This mirrors the sibling cores' `Bus` indirection: the CPU codes against
//! `&mut dyn Bus` and never knows which device backs a given address.

pub trait Bus {
    fn read8(&mut self, addr: u16) -> u8;
    fn write8(&mut self, addr: u16, v: u8);

    /// 16-bit little-endian read helper (e.g. for vectors). Default-derived
    /// from two byte reads so implementors only supply `read8`.
    #[inline]
    fn read16(&mut self, addr: u16) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
}
