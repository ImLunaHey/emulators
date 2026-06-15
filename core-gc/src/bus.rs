//! The GameCube memory bus: virtual→physical translation, region routing, and
//! the [`Bus`] trait the CPU codes against. Mirrors the PS1 core's bus.
//!
//! The CPU never needs to know which concrete device backs a given address. The
//! production implementor is the [`crate::gc::Gc`] god-struct, which owns
//! [`crate::mem::Mem`] plus every I/O device and routes the MMIO window; here we
//! provide the [`Bus`] trait, a closed [`Region`] classifier over the *physical*
//! address, and `Mem`'s handling of the "dumb" backing regions (RAM / IPL).
//!
//! **BIG-ENDIAN.** PowerPC is big-endian; the trait exposes `read64`/`write64`
//! in addition to the 8/16/32 widths because Gekko has 64-bit FPR loads/stores
//! (`lfd`/`stfd`) and paired-single moves. Byte order lives in [`crate::mem`].
//!
//! Address translation: a virtual (effective) address is folded to a physical
//! one with [`crate::regions::mask_region`] (the cached `0x8xxx_xxxx` and
//! uncached `0xCxxx_xxxx` BAT windows alias the same DRAM via `& 0x0FFF_FFFF`).
//! The physical address is then classified.

use crate::mem::Mem;
use crate::regions as R;

/// The memory interface the CPU / interpreter sees. All data and code accesses
/// route through this. [`crate::gc::Gc`] is the production implementor.
///
/// PowerPC is big-endian, so the 16/32/64-bit accessors return values already
/// assembled most-significant-byte-first (the [`Mem`] accessors enforce this).
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    fn read16(&mut self, addr: u32) -> u32;
    fn read32(&mut self, addr: u32) -> u32;
    fn read64(&mut self, addr: u32) -> u64;
    fn write8(&mut self, addr: u32, v: u32);
    fn write16(&mut self, addr: u32, v: u32);
    fn write32(&mut self, addr: u32, v: u32);
    fn write64(&mut self, addr: u32, v: u64);

    /// Instruction fetch. Defaults to a 32-bit read (PowerPC instructions are
    /// always 32-bit aligned words); the impl may override to track timing.
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }
}

/// A classified physical-address region. Closed enum + exhaustive match, per
/// the project idioms — built from the YAGCD memory map. The payload is the
/// region-local offset (IPL is pre-masked to its power-of-two size; RAM keeps
/// the raw offset since it is range-checked, not masked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// 24 MB main RAM (physical 0x0000_0000..0x0180_0000). Offset is raw.
    Ram(u32),
    /// Hardware register window (PI/MI/DSP/DI/SI/EXI/AI/CP/PE/VI). Offset is
    /// from [`R::HW_BASE`].
    Hw(u32),
    /// 2 MB IPL boot ROM. Offset is masked to 2 MB.
    Ipl(u32),
    /// Anything not mapped (open bus).
    Unmapped,
}

/// Classify a *physical* address (post [`R::mask_region`]) into a [`Region`].
#[inline]
pub fn classify(paddr: u32) -> Region {
    match paddr {
        a if (R::RAM_BASE..R::RAM_END).contains(&a) => Region::Ram(a - R::RAM_BASE),
        a if (R::HW_BASE..R::HW_END).contains(&a) => Region::Hw(a - R::HW_BASE),
        a if (R::IPL_BASE..R::IPL_END).contains(&a) => Region::Ipl(a - R::IPL_BASE),
        _ => Region::Unmapped,
    }
}

/// Translate a virtual address and classify it in one step.
#[inline]
pub fn translate(vaddr: u32) -> Region {
    classify(R::mask_region(vaddr))
}

impl Mem {
    /// Read the "dumb" backing regions (RAM / IPL) for a classified [`Region`].
    /// Returns `Some(value)` if backed here; `None` for HW / unmapped, which the
    /// `Gc` bus impl handles. `size` is 1/2/4/8 bytes (8 returns the low 32 bits
    /// here; use [`Mem::region_read64`] for the full doubleword).
    pub fn region_read(&self, region: Region, size: u8) -> Option<u32> {
        Some(match region {
            Region::Ram(off) => match size {
                1 => self.ram_read8(off),
                2 => self.ram_read16(off),
                _ => self.ram_read32(off),
            },
            Region::Ipl(off) => match size {
                1 => self.ipl_read8(off),
                2 => self.ipl_read16(off),
                _ => self.ipl_read32(off),
            },
            _ => return None,
        })
    }

    /// 64-bit read of a "dumb" backing region (RAM / IPL).
    pub fn region_read64(&self, region: Region) -> Option<u64> {
        Some(match region {
            Region::Ram(off) => self.ram_read64(off),
            Region::Ipl(off) => self.ipl_read64(off),
            _ => return None,
        })
    }

    /// Write the writable "dumb" regions (RAM only). IPL is read-only and
    /// silently ignores writes; HW is routed by the `Gc` bus impl. Returns
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
            // IPL is ROM — writes are no-ops but still "consumed".
            Region::Ipl(_) => true,
            _ => false,
        }
    }

    /// 64-bit write of a writable "dumb" region (RAM only).
    pub fn region_write64(&mut self, region: Region, v: u64) -> bool {
        match region {
            Region::Ram(off) => {
                self.ram_write64(off, v);
                true
            }
            Region::Ipl(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_uncached_alias_same_ram_offset() {
        // The cached (0x8..) and uncached (0xC..) views of RAM offset 0x1000
        // both classify to the same RAM offset.
        assert_eq!(translate(0x8000_1000), Region::Ram(0x1000));
        assert_eq!(translate(0xC000_1000), Region::Ram(0x1000));
    }

    #[test]
    fn hw_window_classifies() {
        // 0xCC00_3000 is the SI/DSP/DI MMIO window at HW_BASE + 0x3000.
        assert_eq!(translate(0xCC00_3000), Region::Hw(0x3000));
    }

    #[test]
    fn ipl_window_classifies() {
        assert_eq!(translate(0xFFF0_0000), Region::Ipl(0));
        assert_eq!(translate(0xFFF0_0010), Region::Ipl(0x10));
    }

    #[test]
    fn unmapped_addresses() {
        // A physical hole between RAM (ends 0x0180_0000) and HW (0x0C00_0000).
        assert_eq!(translate(0x8400_0000), Region::Unmapped);
    }
}
