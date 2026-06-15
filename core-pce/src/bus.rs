//! The HuC6280 CPU memory interface.
//!
//! Unlike a plain 6502, the HuC6280 has a banking MMU: the 16-bit logical
//! address space is split into eight 8 KiB pages, and each page is mapped — via
//! a Memory Page Register (MPR0..MPR7) — into a 21-bit (2 MiB) PHYSICAL address
//! space. The CPU's TAM/TMA instructions load/store these MPRs.
//!
//! To keep the CPU core clean, the [`Bus`] still exposes byte `read8`/`write8`
//! over the 16-bit LOGICAL space; the MMU translation lives inside the
//! implementor ([`crate::pce::Pce`]). The CPU additionally needs to set MPRs
//! (TAM/TMA) and read them, and to read the I/O page directly for block
//! transfers, so the trait carries `set_mpr`/`get_mpr` hooks.
//!
//! [`crate::pce::Pce`] is the production implementor; CPU unit tests use a flat
//! 64 KiB RAM stub that ignores banking.

pub trait Bus {
    /// Read one byte from the 16-bit LOGICAL address space (MMU-translated by
    /// the implementor).
    fn read8(&mut self, addr: u16) -> u8;
    /// Write one byte to the 16-bit LOGICAL address space.
    fn write8(&mut self, addr: u16, v: u8);

    /// Load Memory Page Register `n` (0..=7) — backs the TAM instruction.
    fn set_mpr(&mut self, n: u8, v: u8);
    /// Read Memory Page Register `n` (0..=7) — backs the TMA instruction.
    fn get_mpr(&self, n: u8) -> u8;

    /// 16-bit little-endian read helper (e.g. for vectors), default-derived.
    #[inline]
    fn read16(&mut self, addr: u16) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
}
