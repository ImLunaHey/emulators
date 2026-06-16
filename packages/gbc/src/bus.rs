//! The memory interface the CPU sees.
//!
//! The LR35902 only does 8-bit memory accesses (16-bit ops decompose into two
//! byte accesses, low byte first), so the bus is byte-granular. The `Gbc`
//! god-struct is the production implementor; it routes the full address space
//! across `Memory` (internal RAM), `Cart` (ROM/external RAM via the MBC), and
//! the IO devices.
//!
//! Cross-subsystem cycles (PPU↔bus, etc.) are resolved the same way as the
//! other cores: `Gbc` owns everything and methods that need the bus take
//! `&mut dyn Bus`. The CPU/exec code targets `&mut dyn Bus` so it never needs
//! to know which device backs an address.

pub trait Bus {
    /// Read one byte. All CPU memory reads funnel through here.
    fn read8(&mut self, addr: u16) -> u8;
    /// Write one byte. All CPU memory writes funnel through here.
    fn write8(&mut self, addr: u16, v: u8);

    /// Little-endian 16-bit read (low byte at `addr`). Default decomposes into
    /// two byte reads, matching the hardware's two bus cycles.
    fn read16(&mut self, addr: u16) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }
    /// Little-endian 16-bit write (low byte first).
    fn write16(&mut self, addr: u16, v: u16) {
        self.write8(addr, v as u8);
        self.write8(addr.wrapping_add(1), (v >> 8) as u8);
    }
}
