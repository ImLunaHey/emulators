//! The Z80's memory + I/O interface. The Z80 has two separate address spaces:
//! a 16-bit memory space (`read8`/`write8`) and a 16-bit I/O port space
//! (`port_in`/`port_out`) reached via the `IN`/`OUT` instructions. `Sms` (see
//! `sms.rs`) is the production implementor; CPU unit tests use a flat-RAM stub.
//!
//! This mirrors the sibling cores' `Bus` indirection: the CPU codes against
//! `&mut dyn Z80Bus` and never knows which device backs a given address/port.

pub trait Z80Bus {
    /// Read a byte from the 16-bit memory space.
    fn read8(&mut self, addr: u16) -> u8;
    /// Write a byte to the 16-bit memory space.
    fn write8(&mut self, addr: u16, v: u8);

    /// `IN A,(n)` / `IN r,(C)` — read a byte from an I/O port. The Z80 puts the
    /// port number on the low 8 bits of the address bus and (for `IN r,(C)`)
    /// B on the high 8; SMS hardware only decodes a few address bits, so most
    /// implementors mask `port & 0xFF`.
    fn port_in(&mut self, port: u16) -> u8;
    /// `OUT (n),A` / `OUT (C),r` — write a byte to an I/O port.
    fn port_out(&mut self, port: u16, v: u8);

    /// 16-bit little-endian read helper. Default-derived from two byte reads so
    /// implementors only supply `read8`.
    #[inline]
    fn read16(&mut self, addr: u16) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// 16-bit little-endian write helper.
    #[inline]
    fn write16(&mut self, addr: u16, v: u16) {
        self.write8(addr, (v & 0xFF) as u8);
        self.write8(addr.wrapping_add(1), (v >> 8) as u8);
    }
}
