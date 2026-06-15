//! The V30MZ's memory + I/O interface. Like the real 8086/80186, the V30MZ has
//! two separate address spaces: a 20-bit physical memory space (`read8`/`write8`,
//! addresses are `seg<<4 + off`, masked to 20 bits / 1 MiB) and a 16-bit I/O
//! port space (`port_in`/`port_out`) reached via the `IN`/`OUT` instructions.
//! `WonderSwan` (see `ws.rs`) is the production implementor; CPU unit tests use a
//! flat-RAM stub.
//!
//! This mirrors the sibling cores' `Bus` indirection (Z80Bus / Bus): the CPU
//! codes against `&mut dyn V30Bus` and never knows which device backs a given
//! address/port. All accesses are little-endian.

/// Physical address mask: the V30MZ drives a 20-bit (1 MiB) address bus.
pub const ADDR_MASK: u32 = 0xF_FFFF;

pub trait V30Bus {
    /// Read a byte from the 20-bit physical memory space.
    fn read8(&mut self, addr: u32) -> u8;
    /// Write a byte to the 20-bit physical memory space.
    fn write8(&mut self, addr: u32, v: u8);

    /// `IN AL,(port)` — read a byte from a 16-bit I/O port.
    fn port_in8(&mut self, port: u16) -> u8;
    /// `OUT (port),AL` — write a byte to a 16-bit I/O port.
    fn port_out8(&mut self, port: u16, v: u8);

    /// 16-bit little-endian memory read. Default-derived from two byte reads.
    #[inline]
    fn read16(&mut self, addr: u32) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8((addr + 1) & ADDR_MASK) as u16;
        (hi << 8) | lo
    }

    /// 16-bit little-endian memory write.
    #[inline]
    fn write16(&mut self, addr: u32, v: u16) {
        self.write8(addr, (v & 0xFF) as u8);
        self.write8((addr + 1) & ADDR_MASK, (v >> 8) as u8);
    }

    /// 16-bit little-endian I/O read. The WonderSwan's I/O is byte-oriented but
    /// `IN AX,dx` reads two consecutive ports.
    #[inline]
    fn port_in16(&mut self, port: u16) -> u16 {
        let lo = self.port_in8(port) as u16;
        let hi = self.port_in8(port.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// 16-bit little-endian I/O write.
    #[inline]
    fn port_out16(&mut self, port: u16, v: u16) {
        self.port_out8(port, (v & 0xFF) as u8);
        self.port_out8(port.wrapping_add(1), (v >> 8) as u8);
    }
}
