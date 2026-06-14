//! PlayStation 1 memory-map sizes / masks / physical-address ranges.
//!
//! Built from psx-spx "Memory Map". The R3000A exposes a 32-bit virtual
//! address space split into segments (KUSEG/KSEG0/KSEG1/KSEG2). For the
//! common case KUSEG/KSEG0/KSEG1 all alias the same physical memory via
//! `paddr = vaddr & 0x1FFF_FFFF` ([`mask_region`]); KSEG2 (0xC000_0000..) is
//! left unmasked because the cache-control register lives at 0xFFFE_0130.

// ---- region sizes ----
pub const RAM_SIZE: usize = 0x20_0000; // 2 MB main RAM
pub const SCRATCHPAD_SIZE: usize = 0x400; // 1 KB scratchpad (D-cache-as-RAM)
pub const BIOS_SIZE: usize = 0x8_0000; // 512 KB BIOS ROM
pub const IO_SIZE: usize = 0x1000; // 4 KB hardware I/O window (0x1000..0x2000); EXP2 starts at 0x2000

// ---- physical base addresses (post-mask, KUSEG-relative) ----
pub const RAM_BASE: u32 = 0x0000_0000;
pub const EXP1_BASE: u32 = 0x1F00_0000; // Expansion Region 1 (8 MB window)
pub const SCRATCHPAD_BASE: u32 = 0x1F80_0000;
pub const IO_BASE: u32 = 0x1F80_1000; // hardware I/O ports
pub const EXP2_BASE: u32 = 0x1F80_2000; // Expansion Region 2 (8 KB)
pub const EXP3_BASE: u32 = 0x1FA0_0000; // Expansion Region 3 (2 MB)
pub const BIOS_BASE: u32 = 0x1FC0_0000;

// ---- physical end addresses (exclusive) ----
pub const RAM_END: u32 = RAM_BASE + RAM_SIZE as u32;
pub const EXP1_END: u32 = EXP1_BASE + 0x80_0000; // 8 MB
pub const SCRATCHPAD_END: u32 = SCRATCHPAD_BASE + SCRATCHPAD_SIZE as u32;
pub const IO_END: u32 = IO_BASE + IO_SIZE as u32; // 0x1F80_2000
pub const EXP2_END: u32 = EXP2_BASE + 0x2000; // 8 KB
pub const EXP3_END: u32 = EXP3_BASE + 0x20_0000; // 2 MB
pub const BIOS_END: u32 = BIOS_BASE + BIOS_SIZE as u32;

/// 2 MB main RAM is mirrored four times across the first 8 MB of the physical
/// address space (RAM_SIZE is a power of two, so a mask folds the mirrors).
pub const RAM_MASK: u32 = RAM_SIZE as u32 - 1;
pub const SCRATCHPAD_MASK: u32 = SCRATCHPAD_SIZE as u32 - 1;
pub const BIOS_MASK: u32 = BIOS_SIZE as u32 - 1;

/// Cache Control register (lives in KSEG2 at a fixed virtual address; psx-spx).
pub const CACHE_CONTROL_ADDR: u32 = 0xFFFE_0130;

/// Per-segment mask table indexed by the top 3 bits of the virtual address.
/// KUSEG (0x0..0x7FFF_FFFF) and KSEG2 (0xC000_0000..) pass through unmasked
/// (KUSEG is identity below 0x2000_0000; KSEG2 must keep 0xFFFE_xxxx intact);
/// KSEG0 (0x8.., index 4) and KSEG1 (0xA.., index 5) strip the high bits to
/// the same physical address. This mirrors the canonical PSX region table.
const REGION_MASK: [u32; 8] = [
    // KUSEG (2048 MB) — identity (RAM mirrors handled by RAM_MASK at routing).
    0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF,
    // KSEG0 (512 MB) — strip leading 1 bit.
    0x7FFF_FFFF,
    // KSEG1 (512 MB) — strip leading 3 bits.
    0x1FFF_FFFF,
    // KSEG2 (1024 MB) — identity (cache-control window).
    0xFFFF_FFFF, 0xFFFF_FFFF,
];

/// Translate a virtual address to a physical address by masking off the
/// segment bits. For KUSEG/KSEG0/KSEG1 this yields `vaddr & 0x1FFF_FFFF` once
/// folded into the 0..0x1FFF_FFFF physical window; KSEG2 passes through so the
/// cache-control register at 0xFFFE_0130 stays addressable.
#[inline]
pub fn mask_region(addr: u32) -> u32 {
    addr & REGION_MASK[(addr >> 29) as usize]
}
