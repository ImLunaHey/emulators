//! GameCube memory-map sizes / masks / physical-address ranges.
//!
//! Built from YAGCD §2 ("Memory Map"). The Gekko boots with the MMU enabled and
//! a fixed set of BAT (Block Address Translation) windows the IPL installs;
//! emulators treat them as a static map. The two windows software actually uses
//! to reach the 24 MB main DRAM are:
//!
//! | virtual base   | cacheable | meaning                                  |
//! |----------------|-----------|------------------------------------------|
//! | `0x8000_0000`  | yes       | cached view of main RAM (the normal one) |
//! | `0xC000_0000`  | no        | uncached mirror of the *same* main RAM   |
//!
//! Both fold to the same physical DRAM at `0x0000_0000`. The hardware-register
//! window lives at physical `0x0C00_0000` (virtual `0xCC00_0000`), and the
//! 2 MB IPL/boot ROM at the top of the address space (`0xFFF0_0000`). YAGCD §2.
//!
//! `paddr = vaddr & 0x0FFF_FFFF` folds the cached/uncached BAT windows down to
//! a 256 MB physical space ([`mask_region`]); the physical address is then
//! classified by [`crate::bus::classify`].

// ---- region sizes ----
/// 24 MB main 1T-SRAM ("Splash"). NOT a power of two — the bus range-checks it
/// rather than masking (unlike the PS1's power-of-two RAM).
pub const RAM_SIZE: usize = 0x0180_0000; // 24 MiB
/// 16 MB ARAM ("Auxiliary RAM", DSP-attached audio RAM). Not CPU-addressable
/// directly (reached via the DSP/DMA), modelled here only as a documented size.
pub const ARAM_SIZE: usize = 0x0100_0000; // 16 MiB
/// 2 MB embedded framebuffer/texture SRAM inside Flipper (the "EFB"/1T-SRAM).
pub const EFB_SIZE: usize = 0x0020_0000; // 2 MiB
/// 2 MB IPL boot ROM (the "bootrom"/BS1+BS2 + the IPL menu + fonts). YAGCD §10.
pub const IPL_SIZE: usize = 0x0020_0000; // 2 MiB
/// Hardware-register window span (PI/MI/DSP/DI/SI/EXI/AI/CP/PE/VI, etc.).
pub const HW_SIZE: usize = 0x0001_0000; // 64 KiB of MMIO

// ---- physical base addresses (post-mask, into the 256 MB physical window) ----
pub const RAM_BASE: u32 = 0x0000_0000;
/// Hardware register block (physical). Virtual `0xCC00_0000` folds here.
pub const HW_BASE: u32 = 0x0C00_0000;
/// IPL boot ROM (physical). The IPL sits at the top of the address space; the
/// virtual reset window `0xFFF0_0000` folds here under [`PHYS_MASK`] (the reset
/// vector lives here, see [`crate::cpu::state::RESET_VECTOR`]).
pub const IPL_BASE: u32 = 0x0FF0_0000;

// ---- physical end addresses (exclusive) ----
pub const RAM_END: u32 = RAM_BASE + RAM_SIZE as u32; // 0x0180_0000
pub const HW_END: u32 = HW_BASE + HW_SIZE as u32; // 0x0C01_0000
pub const IPL_END: u32 = IPL_BASE + IPL_SIZE as u32; // 0x0F20_0000

/// The IPL ROM is power-of-two sized (2 MB), so its window can be masked.
pub const IPL_MASK: u32 = IPL_SIZE as u32 - 1;

/// The physical address mask: the cached (`0x8xxx_xxxx`) and uncached
/// (`0xCxxx_xxxx`) BAT windows both alias physical DRAM, so stripping the top
/// nibble folds them together. The IPL window at `0xFFF0_0000` also folds into
/// `0x0FF0_0000` under this mask, landing inside the [`IPL_BASE`] range.
pub const PHYS_MASK: u32 = 0x0FFF_FFFF;

/// Translate a Gekko virtual (effective) address to a physical address.
///
/// This is a *simplified* BAT model sufficient for the foundation: the IPL maps
/// the cached/uncached RAM windows and the MMIO/IPL windows linearly, and every
/// window we model differs only in the top nibble, so a single mask folds them
/// to physical space. A real Gekko consults DBAT/IBAT/segment registers and the
/// page table; that lives in a future `mmu` module.
#[inline]
pub fn mask_region(addr: u32) -> u32 {
    addr & PHYS_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_and_uncached_windows_alias_same_physical() {
        // 0x8000_1000 (cached) and 0xC000_1000 (uncached) are the same DRAM.
        assert_eq!(mask_region(0x8000_1000), 0x0000_1000);
        assert_eq!(mask_region(0xC000_1000), 0x0000_1000);
    }

    #[test]
    fn hw_window_folds_to_physical_mmio() {
        assert_eq!(mask_region(0xCC00_3000), HW_BASE + 0x3000);
    }

    #[test]
    fn ipl_window_folds_into_ipl_range() {
        // The reset vector 0xFFF0_0100 folds to 0x0FF0_0100, inside IPL_BASE..END.
        let p = mask_region(0xFFF0_0100);
        assert!((IPL_BASE..IPL_END).contains(&p), "{p:#010X}");
    }
}
