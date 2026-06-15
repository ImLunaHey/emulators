//! Xbox physical-memory map: region sizes and physical-address ranges.
//!
//! Built from XboxDevWiki "Memory". The Xbox uses a flat 32-bit physical space
//! with 64 MB of *unified* DDR at the bottom (CPU and GPU share it — there is no
//! separate VRAM), a sparse band of memory-mapped I/O up high (the NV2A register
//! block, the MCPX, the APU, …), and the 256 KB flash BIOS mirrored across the
//! top 16 MB so the x86 reset vector at `0xFFFF_FFF0` lands inside it.
//!
//! | physical range                 | meaning                                |
//! |--------------------------------|----------------------------------------|
//! | `0x0000_0000..0x0400_0000`     | 64 MB unified DDR RAM                   |
//! | `0xFD00_0000..0xFF00_0000`     | MMIO band (NV2A @ FD00_0000, MCPX, …)   |
//! | `0xFF00_0000..0x1_0000_0000`   | flash BIOS, 256 KB mirrored over 16 MB  |
//!
//! Address translation: at reset paging is off (CR0.PG = 0), so a *linear*
//! address (segment base + offset, computed by the CPU) equals the *physical*
//! address. A real Xbox BIOS turns on paging almost immediately; a proper MMU
//! belongs in a future module. For this foundation [`mask_region`] is the
//! identity (linear == physical), and the physical address is classified by
//! [`crate::bus::classify`].

// ---- region sizes ----
/// 64 MB unified DDR (retail). Power-of-two, so the bus could mask it; we
/// range-check instead for clarity (and so a future 128 MB debug-kit mode is a
/// one-line change).
pub const RAM_SIZE: usize = 0x0400_0000; // 64 MiB
/// 128 MB unified DDR — the size on debug/development kits. Documented here for
/// reference; retail (and this core) use [`RAM_SIZE`].
pub const RAM_SIZE_DEBUG: usize = 0x0800_0000; // 128 MiB
/// 256 KB flash BIOS (the 2BL + the encrypted kernel image). Power-of-two, so
/// its mirror window can be masked.
pub const FLASH_SIZE: usize = 0x0004_0000; // 256 KiB

// ---- physical base addresses ----
pub const RAM_BASE: u32 = 0x0000_0000;
/// Memory-mapped I/O band. The NV2A register block sits at the bottom of it
/// (`0xFD00_0000`); the rest is MCPX / APU / misc device registers.
pub const MMIO_BASE: u32 = 0xFD00_0000;
/// NV2A GPU register block (the "PMC/PFIFO/PGRAPH/…" windows). Inside the MMIO
/// band; modelled as open-bus for now (no device behaviour).
pub const NV2A_BASE: u32 = 0xFD00_0000;
/// Flash BIOS mirror window: the 256 KB image repeats across the top 16 MB of
/// the address space, so `0xFFFF_FFF0` (the x86 reset vector) reads flash.
pub const FLASH_BASE: u32 = 0xFF00_0000;

// ---- physical end addresses (exclusive; FLASH runs to the 4 GB top) ----
pub const RAM_END: u32 = RAM_BASE + RAM_SIZE as u32; // 0x0400_0000
pub const MMIO_END: u32 = FLASH_BASE; // MMIO runs up to the flash mirror

/// The flash image is power-of-two sized (256 KB), so its mirror window masks.
pub const FLASH_MASK: u32 = FLASH_SIZE as u32 - 1;

/// Translate a CPU *linear* address to a *physical* address. With paging off
/// (the reset state) this is the identity; a real Gekko-style MMU/paging model
/// belongs in a future module. Kept as a named function so the bus reads the
/// same on both cores and so turning on paging later is localized here.
#[inline]
pub fn mask_region(addr: u32) -> u32 {
    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_is_64mb_at_zero() {
        assert_eq!(RAM_BASE, 0);
        assert_eq!(RAM_END, 0x0400_0000);
    }

    #[test]
    fn reset_vector_is_inside_flash_mirror() {
        // The x86 reset vector 0xFFFF_FFF0 must land in the flash window and, once
        // masked to the 256 KB image, point near the end of flash.
        assert!(0xFFFF_FFF0u32 >= FLASH_BASE);
        assert_eq!(0xFFFF_FFF0u32 & FLASH_MASK, 0x3_FFF0);
    }

    #[test]
    fn mmio_band_sits_below_flash() {
        assert!(MMIO_BASE < FLASH_BASE);
        assert_eq!(MMIO_END, FLASH_BASE);
        assert_eq!(NV2A_BASE, MMIO_BASE);
    }
}
