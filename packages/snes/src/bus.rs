//! The 65816's memory interface. The 5A22's 65C816 core has a 24-bit address
//! space (256 banks of 64 KiB). `Snes` (see `snes.rs`) is the production
//! implementor; CPU unit tests use a flat-RAM stub.
//!
//! This mirrors the sibling cores' `Bus` indirection: the CPU codes against
//! `&mut dyn Bus` and never knows which device backs a given address. Each
//! `read8`/`write8` also represents one memory-access cycle, which the
//! orchestrator uses for (approximate) timing.

pub trait Bus {
    /// Read a byte from the 24-bit address space.
    fn read8(&mut self, addr: u32) -> u8;
    /// Write a byte to the 24-bit address space.
    fn write8(&mut self, addr: u32, v: u8);

    /// 16-bit little-endian read. The high byte's address wraps within the
    /// bank's low 16 bits only when the caller asks (most 16-bit operand reads
    /// do NOT wrap the bank); callers that need bank-wrapping use the explicit
    /// helpers in the CPU. This default simply increments the full 24-bit addr.
    #[inline]
    fn read16(&mut self, addr: u32) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// 16-bit little-endian write (full 24-bit address increment).
    #[inline]
    fn write16(&mut self, addr: u32, v: u16) {
        self.write8(addr, (v & 0xFF) as u8);
        self.write8(addr.wrapping_add(1), (v >> 8) as u8);
    }
}
