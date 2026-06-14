//! NDS overlay loader. Large DS games (Pokemon Platinum, etc.) split their ARM9
//! code across many overlay blobs stored in the cart's FAT instead of inlining
//! everything in the boot ARM9 binary. The overlay table lives at
//! `header.arm9_overlay_offset` and is an array of 32-byte descriptors; each
//! names a destination address + size + a FAT index pointing at the actual
//! bytes. Ported from ../../ds-recomp/src/cart/overlays.ts.
//!
//! On real hardware overlays page in via cart commands when the runtime needs
//! them. We aren't modeling on-demand loading yet, so we preload them all at
//! boot — that risks address collisions between overlays sharing RAM windows,
//! but it gets the game past the "BL to uninitialized RAM" failures otherwise.
//!
//! Ownership (CONTRACT.md): the TS passed `bus9`/`bus7` *and* `mem`; the fast
//! path writes straight into `mem.main_ram`, the slow path goes byte-by-byte
//! through the bus. Here `load_all` takes `&mut Nds` so it can use both the
//! shared backing store directly and the per-core bus accessors.

use super::header::{read_u32, NdsHeader};
use crate::memory::regions::MAIN_RAM_MASK;
use crate::nds::Nds;

const OVERLAY_INFO_SIZE: u32 = 32;
const FAT_ENTRY_SIZE: u32 = 8;

/// One 32-byte overlay descriptor from the overlay table.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayInfo {
    pub overlay_id: u32,
    pub ram_address: u32,
    pub ram_size: u32,
    pub bss_size: u32,
    pub file_id: u32,
}

/// A FAT entry: a [start, end) byte range into the ROM image.
#[derive(Clone, Copy, Debug, Default)]
pub struct FatEntry {
    pub start: u32,
    pub end: u32,
}

/// Aggregate counters returned from a full-overlay preload — how many overlays
/// and bytes landed per core, and how many were skipped for window collisions.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayLoadStats {
    pub arm9_loaded: u32,
    pub arm7_loaded: u32,
    pub arm9_bytes: u32,
    pub arm7_bytes: u32,
    pub collisions: u32,
}

/// Decode the 32-byte overlay descriptor at `offset` in the ROM image.
pub(crate) fn read_overlay_info(rom: &[u8], offset: u32) -> OverlayInfo {
    let off = offset as usize;
    OverlayInfo {
        overlay_id: read_u32(rom, off + 0x00),
        ram_address: read_u32(rom, off + 0x04),
        ram_size: read_u32(rom, off + 0x08),
        bss_size: read_u32(rom, off + 0x0C),
        file_id: read_u32(rom, off + 0x18),
    }
}

/// Decode the FAT entry for `file_id` (base + file_id * 8).
pub(crate) fn read_fat_entry(rom: &[u8], fat_base: u32, file_id: u32) -> FatEntry {
    let off = (fat_base + file_id * FAT_ENTRY_SIZE) as usize;
    FatEntry {
        start: read_u32(rom, off),
        end: read_u32(rom, off + 4),
    }
}

/// Preload every ARM9 + ARM7 overlay into RAM at boot. Walks both overlay
/// tables, skips zero-length and window-colliding entries (ARM9 only, per the
/// TS), copies the bytes and zeroes the BSS tail.
pub fn load_all(nds: &mut Nds, rom: &[u8], header: &NdsHeader) -> OverlayLoadStats {
    let mut stats = OverlayLoadStats::default();

    // ARM9 overlays.
    if header.arm9_overlay_size >= OVERLAY_INFO_SIZE {
        let count = header.arm9_overlay_size / OVERLAY_INFO_SIZE;
        // Track RAM byte ranges already covered by a previously-loaded overlay
        // so we don't stomp on it. Large games share RAM windows across many
        // overlays — only the first-loaded per window is what the game wants
        // resident at boot.
        let mut taken: Vec<(u32, u32)> = Vec::new();
        for i in 0..count {
            let info = read_overlay_info(rom, header.arm9_overlay_offset + i * OVERLAY_INFO_SIZE);
            let fat = read_fat_entry(rom, header.fat_offset, info.file_id);
            if fat.end <= fat.start {
                continue;
            }
            let src_len = fat.end - fat.start;
            let start = info.ram_address;
            let end = info.ram_address + src_len + info.bss_size;
            if taken
                .iter()
                .any(|&(rs, re)| ranges_overlap(start, end, rs, re))
            {
                stats.collisions += 1;
                continue;
            }
            taken.push((start, end));
            stats.arm9_bytes += copy_overlay(nds, true, info.ram_address, rom, fat.start, src_len);
            zero_bss(nds, true, info.ram_address + src_len, info.bss_size);
            stats.arm9_loaded += 1;
        }
    }

    // ARM7 overlays (rare, but some games use them — no collision tracking,
    // matching the TS).
    if header.arm7_overlay_size >= OVERLAY_INFO_SIZE {
        let count = header.arm7_overlay_size / OVERLAY_INFO_SIZE;
        for i in 0..count {
            let info = read_overlay_info(rom, header.arm7_overlay_offset + i * OVERLAY_INFO_SIZE);
            let fat = read_fat_entry(rom, header.fat_offset, info.file_id);
            if fat.end <= fat.start {
                continue;
            }
            let src_len = fat.end - fat.start;
            stats.arm7_bytes += copy_overlay(nds, false, info.ram_address, rom, fat.start, src_len);
            zero_bss(nds, false, info.ram_address + src_len, info.bss_size);
            stats.arm7_loaded += 1;
        }
    }

    stats
}

/// Fast-path bulk copy from ROM into Main RAM (the only window overlays
/// actually target — both the 0x02 mirror and the 0x01 alias). Slower paths
/// fall through to byte-by-byte bus writes.
fn copy_overlay(nds: &mut Nds, is_arm9: bool, dest: u32, rom: &[u8], src: u32, size: u32) -> u32 {
    let src = src as usize;
    let end = (src + size as usize).min(rom.len());
    if end <= src {
        return 0;
    }
    let len = end - src;
    if (dest >> 24) == 0x02 || (dest >> 24) == 0x01 {
        let dst = (dest & MAIN_RAM_MASK) as usize;
        nds.mem.main_ram[dst..dst + len].copy_from_slice(&rom[src..end]);
        return len as u32;
    }
    for i in 0..len {
        let a = dest.wrapping_add(i as u32);
        let b = rom[src + i] as u32;
        if is_arm9 {
            nds.write8_arm9(a, b);
        } else {
            nds.write8_arm7(a, b);
        }
    }
    len as u32
}

/// Zero the BSS tail of an overlay (uninitialized-data segment).
fn zero_bss(nds: &mut Nds, is_arm9: bool, dest: u32, size: u32) {
    if size == 0 {
        return;
    }
    if (dest >> 24) == 0x02 || (dest >> 24) == 0x01 {
        let dst = (dest & MAIN_RAM_MASK) as usize;
        for b in &mut nds.mem.main_ram[dst..dst + size as usize] {
            *b = 0;
        }
        return;
    }
    for i in 0..size {
        let a = dest.wrapping_add(i);
        if is_arm9 {
            nds.write8_arm9(a, 0);
        } else {
            nds.write8_arm7(a, 0);
        }
    }
}

/// Two half-open byte ranges overlap. Used to skip overlays sharing a RAM
/// window (only the first-loaded per window is what the game wants resident).
#[inline]
pub(crate) fn ranges_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    !(a_end <= b_start || a_start >= b_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(rom: &mut [u8], off: usize, v: u32) {
        rom[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn ranges_overlap_logic() {
        assert!(ranges_overlap(0, 10, 5, 15));
        assert!(!ranges_overlap(0, 10, 10, 20)); // touching, not overlapping
        assert!(!ranges_overlap(20, 30, 0, 10));
        assert!(ranges_overlap(0, 100, 40, 50)); // containment
    }

    #[test]
    fn reads_overlay_info_and_fat() {
        let mut rom = vec![0u8; 0x100];
        put_u32(&mut rom, 0x00, 3); // overlay_id
        put_u32(&mut rom, 0x04, 0x0200_4000); // ram_address
        put_u32(&mut rom, 0x08, 0x800); // ram_size
        put_u32(&mut rom, 0x0C, 0x40); // bss_size
        put_u32(&mut rom, 0x18, 5); // file_id
        let info = read_overlay_info(&rom, 0);
        assert_eq!(info.overlay_id, 3);
        assert_eq!(info.ram_address, 0x0200_4000);
        assert_eq!(info.bss_size, 0x40);
        assert_eq!(info.file_id, 5);

        // FAT entry for file_id 5 at base 0x80.
        put_u32(&mut rom, 0x80 + 5 * 8, 0x1000); // start
        put_u32(&mut rom, 0x80 + 5 * 8 + 4, 0x1010); // end
        let fat = read_fat_entry(&rom, 0x80, 5);
        assert_eq!(fat.start, 0x1000);
        assert_eq!(fat.end, 0x1010);
    }

    #[test]
    fn loads_single_arm9_overlay_into_main_ram() {
        let mut nds = crate::nds::Nds::new();
        let mut rom = vec![0u8; 0x2000];
        // Overlay table at 0x100: one 32-byte descriptor.
        let ovl_off = 0x100u32;
        put_u32(&mut rom, ovl_off as usize + 0x00, 0); // id
        put_u32(&mut rom, ovl_off as usize + 0x04, 0x0200_1000); // ram_address
        put_u32(&mut rom, ovl_off as usize + 0x08, 16); // ram_size
        put_u32(&mut rom, ovl_off as usize + 0x0C, 8); // bss_size
        put_u32(&mut rom, ovl_off as usize + 0x18, 0); // file_id 0
        // FAT base at 0x200, file 0 → [0x300, 0x310).
        let fat_off = 0x200u32;
        put_u32(&mut rom, fat_off as usize + 0, 0x300);
        put_u32(&mut rom, fat_off as usize + 4, 0x310);
        for i in 0..16 {
            rom[0x300 + i] = (i as u8) + 0x20;
        }

        let mut header = NdsHeader::default();
        header.arm9_overlay_offset = ovl_off;
        header.arm9_overlay_size = OVERLAY_INFO_SIZE;
        header.fat_offset = fat_off;

        let stats = load_all(&mut nds, &rom, &header);
        assert_eq!(stats.arm9_loaded, 1);
        assert_eq!(stats.arm9_bytes, 16);
        assert_eq!(stats.collisions, 0);
        // Bytes landed at 0x02001000.
        let base = (0x0200_1000u32 & MAIN_RAM_MASK) as usize;
        assert_eq!(nds.mem.main_ram[base], 0x20);
        assert_eq!(nds.mem.main_ram[base + 15], 0x2F);
        // BSS zeroed after the 16 copied bytes.
        assert_eq!(nds.mem.main_ram[base + 16], 0);
        assert_eq!(nds.mem.main_ram[base + 23], 0);
    }

    #[test]
    fn skips_colliding_overlays() {
        let mut nds = crate::nds::Nds::new();
        let mut rom = vec![0u8; 0x2000];
        let ovl_off = 0x100u32;
        // Two overlays targeting the same RAM window.
        for k in 0..2u32 {
            let base = ovl_off as usize + (k as usize) * 32;
            put_u32(&mut rom, base + 0x00, k);
            put_u32(&mut rom, base + 0x04, 0x0200_1000); // same address
            put_u32(&mut rom, base + 0x08, 16);
            put_u32(&mut rom, base + 0x0C, 0);
            put_u32(&mut rom, base + 0x18, k); // file_id k
        }
        let fat_off = 0x200u32;
        for k in 0..2u32 {
            put_u32(&mut rom, fat_off as usize + (k as usize) * 8, 0x300 + k * 0x10);
            put_u32(&mut rom, fat_off as usize + (k as usize) * 8 + 4, 0x310 + k * 0x10);
        }
        let mut header = NdsHeader::default();
        header.arm9_overlay_offset = ovl_off;
        header.arm9_overlay_size = OVERLAY_INFO_SIZE * 2;
        header.fat_offset = fat_off;

        let stats = load_all(&mut nds, &rom, &header);
        assert_eq!(stats.arm9_loaded, 1);
        assert_eq!(stats.collisions, 1);
    }
}
