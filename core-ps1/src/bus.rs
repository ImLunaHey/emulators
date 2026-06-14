//! The PSX memory bus: virtual→physical translation, region routing, and the
//! cache-isolation quirk. Mirrors the GBA core's bus.
//!
//! The CPU codes against `&mut dyn Bus`, so it never needs to know which
//! concrete device backs a given address. The production implementor is the
//! [`crate::psx::Psx`] god-struct, which owns [`crate::memory::Mem`] plus every
//! I/O device and routes the I/O window; here we provide the [`Bus`] trait, a
//! closed [`Region`] classifier over the *physical* address, and `Mem`'s
//! handling of the three "dumb" backing regions (RAM / scratchpad / BIOS).
//!
//! Address translation: a virtual address is first folded to a physical one
//! with [`crate::regions::mask_region`] (KUSEG/KSEG0/KSEG1 alias the same
//! physical space via `& 0x1FFF_FFFF`; KSEG2 passes through for the
//! cache-control register). The physical address is then classified.

use crate::memory::Mem;
use crate::regions as R;

/// The memory interface the CPU / interpreter sees. All data and code accesses
/// route through this. `Psx` is the production implementor.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    fn read16(&mut self, addr: u32) -> u32;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, v: u32);
    fn write16(&mut self, addr: u32, v: u32);
    fn write32(&mut self, addr: u32, v: u32);

    /// Instruction fetch. Defaults to a 32-bit read (MIPS instructions are
    /// always 32-bit aligned words); the impl may override to track timing.
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }
}

/// A classified physical-address region. Closed enum + exhaustive match, per
/// the project idioms — built from the psx-spx memory map. The associated
/// payload is the region-local offset (already folded to the region size where
/// the region is power-of-two mirrored, e.g. RAM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// 2 MB main RAM (mirrored across the first 8 MB). Offset is masked.
    Ram(u32),
    /// 1 KB scratchpad. Offset is masked.
    Scratchpad(u32),
    /// 8 KB hardware I/O window (0x1F80_1000..0x1F80_3000). Offset is from
    /// [`R::IO_BASE`].
    Io(u32),
    /// 512 KB BIOS ROM. Offset is masked.
    Bios(u32),
    /// Expansion Region 1 (8 MB). Offset is from [`R::EXP1_BASE`].
    Expansion1(u32),
    /// Expansion Region 2 (8 KB). Offset is from [`R::EXP2_BASE`].
    Expansion2(u32),
    /// Expansion Region 3 (2 MB). Offset is from [`R::EXP3_BASE`].
    Expansion3(u32),
    /// Cache-control register (KSEG2, 0xFFFE_0130).
    CacheControl,
    /// Anything not mapped (open bus).
    Unmapped,
}

/// Classify a *physical* address (post [`R::mask_region`]) into a [`Region`].
#[inline]
pub fn classify(paddr: u32) -> Region {
    // The first 8 MB are the 2 MB RAM mirrored four times.
    if paddr < R::EXP1_BASE {
        return Region::Ram(paddr & R::RAM_MASK);
    }
    match paddr {
        a if (R::EXP1_BASE..R::EXP1_END).contains(&a) => Region::Expansion1(a - R::EXP1_BASE),
        a if (R::SCRATCHPAD_BASE..R::SCRATCHPAD_END).contains(&a) => {
            Region::Scratchpad(a & R::SCRATCHPAD_MASK)
        }
        a if (R::IO_BASE..R::IO_END).contains(&a) => Region::Io(a - R::IO_BASE),
        a if (R::EXP2_BASE..R::EXP2_END).contains(&a) => Region::Expansion2(a - R::EXP2_BASE),
        a if (R::EXP3_BASE..R::EXP3_END).contains(&a) => Region::Expansion3(a - R::EXP3_BASE),
        a if (R::BIOS_BASE..R::BIOS_END).contains(&a) => Region::Bios(a & R::BIOS_MASK),
        R::CACHE_CONTROL_ADDR => Region::CacheControl,
        _ => Region::Unmapped,
    }
}

/// Translate a virtual address and classify it in one step.
#[inline]
pub fn translate(vaddr: u32) -> Region {
    classify(R::mask_region(vaddr))
}

impl Mem {
    /// Read the "dumb" backing regions (RAM / scratchpad / BIOS) for a
    /// classified [`Region`]. Returns `Some(value)` if the region is backed
    /// here; `None` for I/O / expansion / cache-control / unmapped, which the
    /// `Psx` bus impl handles. `size` is 1/2/4 bytes.
    pub fn region_read(&self, region: Region, size: u8) -> Option<u32> {
        Some(match region {
            Region::Ram(off) => match size {
                1 => self.ram_read8(off),
                2 => self.ram_read16(off),
                _ => self.ram_read32(off),
            },
            Region::Scratchpad(off) => match size {
                1 => self.scratch_read8(off),
                2 => self.scratch_read16(off),
                _ => self.scratch_read32(off),
            },
            Region::Bios(off) => match size {
                1 => self.bios_read8(off),
                2 => self.bios_read16(off),
                _ => self.bios_read32(off),
            },
            _ => return None,
        })
    }

    /// Write the writable "dumb" regions (RAM / scratchpad). BIOS is read-only
    /// and silently ignores writes; I/O / expansion / cache-control are routed
    /// by the `Psx` bus impl. Returns `true` if consumed here.
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
            Region::Scratchpad(off) => {
                match size {
                    1 => self.scratch_write8(off, v),
                    2 => self.scratch_write16(off, v),
                    _ => self.scratch_write32(off, v),
                }
                true
            }
            // BIOS is ROM — writes are no-ops but still "consumed" so the bus
            // doesn't treat them as unmapped.
            Region::Bios(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kseg_segments_alias_same_physical() {
        // KUSEG / KSEG0 / KSEG1 views of RAM offset 0x1000 all classify to the
        // same RAM offset.
        assert_eq!(translate(0x0000_1000), Region::Ram(0x1000));
        assert_eq!(translate(0x8000_1000), Region::Ram(0x1000));
        assert_eq!(translate(0xA000_1000), Region::Ram(0x1000));
    }

    #[test]
    fn ram_mirrors_fold() {
        // 2 MB RAM mirrored in the first 8 MB.
        assert_eq!(translate(0x0020_0000), Region::Ram(0));
        assert_eq!(translate(0x0060_0000), Region::Ram(0));
    }

    #[test]
    fn bios_and_scratchpad_and_io_classify() {
        assert_eq!(translate(0xBFC0_0000), Region::Bios(0)); // KSEG1 BIOS
        assert_eq!(translate(0x1FC0_0010), Region::Bios(0x10)); // KUSEG BIOS
        assert_eq!(translate(0x1F80_0004), Region::Scratchpad(4));
        assert_eq!(translate(0x1F80_1070), Region::Io(0x70)); // I_STAT
    }

    #[test]
    fn cache_control_classifies() {
        assert_eq!(translate(R::CACHE_CONTROL_ADDR), Region::CacheControl);
    }
}
