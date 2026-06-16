//! The NEC V810's memory interface. Unlike the Z80, the V810 has a single
//! 32-bit byte-addressed memory space (no separate I/O port space) — all
//! peripherals (VIP, VSU, hardware control registers, cartridge) are
//! memory-mapped. [`crate::vb::Vb`] is the production implementor; CPU unit
//! tests use a flat-RAM stub.
//!
//! This mirrors the sibling cores' `Bus` indirection: the CPU codes against
//! `&mut dyn Bus` and never knows which device backs a given address.
//!
//! All accesses are little-endian. The V810 traps on misaligned 16/32-bit
//! accesses on real hardware, but in practice well-behaved software is aligned;
//! we mask the low address bits so an unaligned access reads the aligned word
//! rather than faulting (matches how most VB emulators behave).

pub trait Bus {
    /// Read a byte from the 32-bit memory space.
    fn read8(&mut self, addr: u32) -> u8;
    /// Write a byte to the 32-bit memory space.
    fn write8(&mut self, addr: u32, v: u8);

    /// 16-bit little-endian read. Default-derived from two byte reads so
    /// implementors only need supply `read8`/`write8`, but [`crate::vb::Vb`]
    /// overrides these for speed and to route halfword-granular device regs.
    #[inline]
    fn read16(&mut self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read8(a) as u16;
        let hi = self.read8(a.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// 16-bit little-endian write.
    #[inline]
    fn write16(&mut self, addr: u32, v: u16) {
        let a = addr & !1;
        self.write8(a, (v & 0xFF) as u8);
        self.write8(a.wrapping_add(1), (v >> 8) as u8);
    }

    /// 32-bit little-endian read.
    #[inline]
    fn read32(&mut self, addr: u32) -> u32 {
        let a = addr & !3;
        let lo = self.read16(a) as u32;
        let hi = self.read16(a.wrapping_add(2)) as u32;
        (hi << 16) | lo
    }

    /// 32-bit little-endian write.
    #[inline]
    fn write32(&mut self, addr: u32, v: u32) {
        let a = addr & !3;
        self.write16(a, (v & 0xFFFF) as u16);
        self.write16(a.wrapping_add(2), (v >> 16) as u16);
    }
}
