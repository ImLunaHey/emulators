//! The Xbox memory bus: linear→physical translation, region routing, and the
//! [`Bus`] trait the CPU codes against. Mirrors the PS1/GC cores' bus.
//!
//! The CPU never needs to know which concrete device backs a given address. The
//! production implementor is the [`crate::xbox::Xbox`] god-struct, which owns
//! [`crate::mem::Mem`] plus every I/O device and routes the MMIO band; here we
//! provide the [`Bus`] trait, a closed [`Region`] classifier over the *physical*
//! address, and `Mem`'s handling of the "dumb" backing regions (RAM / flash).
//!
//! **LITTLE-ENDIAN.** x86 is little-endian; the 16/32-bit accessors return
//! values assembled least-significant-byte-first (the [`Mem`] accessors enforce
//! this). Byte order lives entirely in [`crate::mem`].
//!
//! Address translation: with paging off (the reset state) a CPU linear address
//! equals the physical address, so [`crate::regions::mask_region`] is the
//! identity here; the physical address is then classified.

use crate::mem::Mem;
use crate::regions as R;

/// The memory interface the CPU / interpreter sees. All data and code accesses
/// route through this. [`crate::xbox::Xbox`] is the production implementor.
///
/// x86 is little-endian and supports unaligned 8/16/32-bit access, so the
/// accessors take an arbitrary address and return/accept a `u32` (8/16-bit
/// values use the low bits). Port I/O (`in`/`out`) is a separate 64 KB space.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    fn read16(&mut self, addr: u32) -> u32;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, v: u32);
    fn write16(&mut self, addr: u32, v: u32);
    fn write32(&mut self, addr: u32, v: u32);

    /// Instruction fetch byte. Defaults to a plain [`Bus::read8`]; an impl may
    /// override to track instruction-fetch timing or a prefetch cache.
    fn fetch8(&mut self, addr: u32) -> u8 {
        self.read8(addr) as u8
    }

    /// Read from the 64 KB x86 port-I/O space (`in` instruction). Devices live
    /// here on a real Xbox (the SMBus, the PIC/PIT, …); the foundation returns
    /// open-bus 0xFF bits and ignores writes.
    fn port_in(&mut self, _port: u16, _size: u8) -> u32 {
        0xFFFF_FFFF
    }
    /// Write to the port-I/O space (`out` instruction).
    fn port_out(&mut self, _port: u16, _size: u8, _v: u32) {}
}

/// A classified physical-address region. Closed enum + exhaustive match, per the
/// project idioms — built from the XboxDevWiki memory map. The payload is the
/// region-local offset (flash is pre-masked to its 256 KB size; RAM keeps the
/// raw offset since it is range-checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// 64 MB unified DDR (physical `0x0000_0000..0x0400_0000`). Offset is raw.
    Ram(u32),
    /// MMIO band (NV2A @ `0xFD00_0000`, MCPX, APU, …). Offset from [`R::MMIO_BASE`].
    Mmio(u32),
    /// 256 KB flash BIOS mirror. Offset is masked to 256 KB.
    Flash(u32),
    /// Anything not mapped (open bus).
    Unmapped,
}

/// Classify a *physical* address (post [`R::mask_region`]) into a [`Region`].
#[inline]
pub fn classify(paddr: u32) -> Region {
    match paddr {
        a if (R::RAM_BASE..R::RAM_END).contains(&a) => Region::Ram(a - R::RAM_BASE),
        a if (R::MMIO_BASE..R::MMIO_END).contains(&a) => Region::Mmio(a - R::MMIO_BASE),
        // Flash runs from FLASH_BASE to the very top of the 4 GB space.
        a if a >= R::FLASH_BASE => Region::Flash((a - R::FLASH_BASE) & R::FLASH_MASK),
        _ => Region::Unmapped,
    }
}

/// Translate a linear address and classify it in one step.
#[inline]
pub fn translate(laddr: u32) -> Region {
    classify(R::mask_region(laddr))
}

impl Mem {
    /// Read the "dumb" backing regions (RAM / flash) for a classified [`Region`].
    /// Returns `Some(value)` if backed here; `None` for MMIO / unmapped, which
    /// the `Xbox` bus impl handles. `size` is 1/2/4 bytes.
    pub fn region_read(&self, region: Region, size: u8) -> Option<u32> {
        Some(match region {
            Region::Ram(off) => match size {
                1 => self.ram_read8(off),
                2 => self.ram_read16(off),
                _ => self.ram_read32(off),
            },
            Region::Flash(off) => match size {
                1 => self.flash_read8(off),
                2 => self.flash_read16(off),
                _ => self.flash_read32(off),
            },
            _ => return None,
        })
    }

    /// Write the writable "dumb" regions (RAM only). Flash is read-only and
    /// silently ignores writes; MMIO is routed by the `Xbox` bus impl. Returns
    /// `true` if consumed here.
    pub fn region_write(&mut self, region: Region, size: u8, v: u32) -> bool {
        match region {
            Region::Ram(off) => {
                match size {
                    1 => self.ram_write8(off, v),
                    2 => self.ram_write16(off, v),
                    _ => self.ram_write32(off, v),
                }
                true
            }
            // Flash is ROM — writes are no-ops but still "consumed".
            Region::Flash(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_classifies() {
        assert_eq!(translate(0x0000_1000), Region::Ram(0x1000));
        assert_eq!(translate(0x03FF_FFFF), Region::Ram(0x03FF_FFFF));
    }

    #[test]
    fn mmio_band_classifies() {
        assert_eq!(translate(0xFD00_0000), Region::Mmio(0));
        assert_eq!(translate(0xFD00_1000), Region::Mmio(0x1000));
    }

    #[test]
    fn flash_window_and_reset_vector() {
        assert_eq!(translate(0xFF00_0000), Region::Flash(0));
        // The x86 reset vector folds to the top of the 256 KB flash image.
        assert_eq!(translate(0xFFFF_FFF0), Region::Flash(0x3_FFF0));
    }

    #[test]
    fn hole_between_ram_and_mmio_is_unmapped() {
        assert_eq!(translate(0x8000_0000), Region::Unmapped);
    }
}
