//! Text-mode BG scanline renderer (DS engines A/B). Ported from
//! ../../ds-recomp/src/ppu/text_bg.ts and adapted from the GBA core's
//! `render_mode_text` (../../core/src/ppu.rs).
//!
//! Ownership (CONTRACT.md): NO `&mut Nds`. The renderer takes the engine's BG
//! register state + the borrowed VRAM/PRAM byte slices + the resolved bank/
//! palette bases, and writes one scanline of packed pixels into `out` (the
//! engine's `bg_line[bg]`, format defined in engine_a.rs: BGR555 | drawn-bit |
//! layer | priority).
//!
//! DS deltas over the GBA text BG (`render_mode_text`):
//!   - 256-px-wide scanline (LINE_W) instead of 240.
//!   - Engine A folds the DISPCNT global char/screen base offsets (bits 24..29)
//!     into the per-BG char/screen base via `char_extra` / `screen_extra`
//!     (engine B passes 0).
//!   - Tile/map fetches are relative to the engine's BG VRAM window base
//!     (`bg_vram_base`), not a fixed 0; there's no GBA-style 0x10000 OBJ-area
//!     clamp because the DS routes BG VRAM through its own window.
//!   - Extended palettes: when `ext_pal` is `Some(slice)` AND the BG is 8bpp,
//!     the per-tile palette bank (screen-entry bits 12..15) selects one of 16
//!     256-color sub-palettes inside `slice` (512 bytes / 256 entries each)
//!     instead of the flat base PRAM. The caller resolves which VRAM bank backs
//!     the slot (incl. the BG0/BG1 bit-13 slot swap) and passes the slice; here
//!     we just index it. If the indexed entry would be out of range we fall
//!     back to base PRAM (mirrors the TS `idx < 0` fallback).

use super::engine_a::{BgRegs, LINE_W, PX_TRANSPARENT};

/// Read a little-endian u16 from a byte slice.
#[inline]
fn rd16(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8)
}

/// Render one text-BG scanline for layer `bg` at screen row `y` into `out`
/// (len = LINE_W packed pixels). Reads the BGxCNT/HOFS/VOFS from `bg_regs`,
/// tile + map data from `vram` (offset by the engine's resolved `bg_vram_base`
/// plus the DISPCNT global char/screen base extras folded into `char_extra` and
/// `screen_extra`), and palette from `pram` at `pram_base`.
///
/// `out` is expected to already hold this BG's previous content / transparency;
/// this writer overwrites every visible pixel and marks empty ones transparent,
/// so callers may pass a buffer pre-cleared to `PX_TRANSPARENT`.
///
/// DS deltas over the GBA: extended palettes (when `ext_pal` is `Some(slice)`
/// the 16-bit per-entry palette is indexed by the tile's 8-bit palette bank);
/// the engine global char/screen base offsets.
#[allow(clippy::too_many_arguments)]
pub fn render_text_scanline(
    bg: usize,
    y: u32,
    bg_regs: &BgRegs,
    mosaic: u32,
    vram: &[u8],
    bg_vram_base: usize,
    char_extra: usize,
    screen_extra: usize,
    pram: &[u8],
    pram_base: usize,
    ext_pal: Option<&[u8]>,
    out: &mut [u32],
) {
    // BG tilemap dimensions per BGxCNT size code (bits 14..15).
    //   0 = 256x256, 1 = 512x256, 2 = 256x512, 3 = 512x512 (px).
    const SIZE_W: [u32; 4] = [256, 512, 256, 512];
    const SIZE_H: [u32; 4] = [256, 256, 512, 512];

    let ctrl = bg_regs.cnt[bg];
    let priority = ctrl & 3;
    // Per-BG char/screen base, then add the engine's DISPCNT-global extras.
    let char_base = (((ctrl >> 2) & 0xF) * 0x4000) as usize + char_extra;
    let screen_base = (((ctrl >> 8) & 0x1F) * 0x800) as usize + screen_extra;
    let color_mode8 = (ctrl & 0x80) != 0;
    let mosaic_on = (ctrl & 0x40) != 0;
    let size_idx = ((ctrl >> 14) & 3) as usize;
    let map_w = SIZE_W[size_idx];
    let map_h = SIZE_H[size_idx];

    // BG mosaic: MOSAIC low nibble = horizontal block size - 1, second nibble =
    // vertical block size - 1. Each axis quantizes sample coords to integer
    // multiples of the block size (the chunky transition/damage-flash look).
    let mos_bg_h = if mosaic_on { (mosaic & 0x0F) + 1 } else { 1 };
    let mos_bg_v = if mosaic_on {
        ((mosaic >> 4) & 0x0F) + 1
    } else {
        1
    };

    let hofs = bg_regs.hofs[bg];
    let vofs = bg_regs.vofs[bg];
    let y_src = if mosaic_on { y - (y % mos_bg_v) } else { y };
    let y_eff = (y_src + vofs) & (map_h - 1);

    // Packed high bits (layer id + priority) for every drawn pixel of this BG.
    let layer_hi = ((bg as u32) << 16) | (priority << 18);

    // Ext-palette path is only meaningful in 8bpp mode (4bpp always uses the
    // per-tile 16-color sub-palette of base PRAM).
    let ext_pal = if color_mode8 { ext_pal } else { None };

    for x in 0u32..LINE_W as u32 {
        let x_mos = if mosaic_on { x - (x % mos_bg_h) } else { x };
        let x_eff = (x_mos + hofs) & (map_w - 1);

        // Pick the 256x256 screen block (quadrant) within the tilemap. Each
        // quadrant is 0x800 bytes (32x32 16-bit entries). Quadrant ordering is
        // TL, TR, BL, BR — but for a 512x256 (width-only) map there are only
        // two quadrants laid left/right, and for 256x512 only top/bottom, so we
        // collapse the block index accordingly.
        let mut map_off = screen_base;
        if map_w == 512 && x_eff >= 256 {
            map_off += 0x800;
        }
        if map_h == 512 && y_eff >= 256 {
            map_off += if map_w == 512 { 0x1000 } else { 0x800 };
        }

        let tile_x = (x_eff & 0xFF) >> 3;
        let tile_y = (y_eff & 0xFF) >> 3;
        let map_addr = bg_vram_base + map_off + ((tile_y * 32 + tile_x) * 2) as usize;
        if map_addr + 1 >= vram.len() {
            out[x as usize] = PX_TRANSPARENT;
            continue;
        }
        let entry = rd16(vram, map_addr);
        let tile_idx = entry & 0x3FF;
        let hflip = (entry & 0x400) != 0;
        let vflip = (entry & 0x800) != 0;
        let pal_bank = (entry >> 12) & 0xF;

        let mut in_tile_x = x_eff & 7;
        let mut in_tile_y = y_eff & 7;
        if hflip {
            in_tile_x ^= 7;
        }
        if vflip {
            in_tile_y ^= 7;
        }

        if color_mode8 {
            // 64 bytes per tile (8bpp linear).
            let tile_addr = bg_vram_base
                + char_base
                + (tile_idx * 64 + in_tile_y * 8 + in_tile_x) as usize;
            if tile_addr >= vram.len() {
                out[x as usize] = PX_TRANSPARENT;
                continue;
            }
            let pix = vram[tile_addr] as u32;
            if pix == 0 {
                out[x as usize] = PX_TRANSPARENT;
                continue;
            }
            let color = lookup_8bpp(pix, pal_bank, ext_pal, pram, pram_base);
            out[x as usize] = (color & 0x7FFF) | layer_hi;
        } else {
            // 32 bytes per tile (4bpp packed: two pixels per byte).
            let tile_addr = bg_vram_base
                + char_base
                + (tile_idx * 32 + in_tile_y * 4 + (in_tile_x >> 1)) as usize;
            if tile_addr >= vram.len() {
                out[x as usize] = PX_TRANSPARENT;
                continue;
            }
            let byte = vram[tile_addr] as u32;
            let pix = if (in_tile_x & 1) != 0 {
                byte >> 4
            } else {
                byte & 0xF
            };
            if pix == 0 {
                out[x as usize] = PX_TRANSPARENT;
                continue;
            }
            let pal_off = pram_base + ((pal_bank * 16 + pix) * 2) as usize;
            let color = if pal_off + 1 < pram.len() {
                rd16(pram, pal_off)
            } else {
                0
            };
            out[x as usize] = (color & 0x7FFF) | layer_hi;
        }
    }
}

/// 8bpp palette lookup: extended palette when `ext_pal` is `Some`, else flat
/// base PRAM. The extended slot is a flat 512-byte (256-entry) sub-palette per
/// `pal_bank`; an out-of-range index falls back to base PRAM (TS `idx < 0`).
#[inline]
fn lookup_8bpp(
    pix: u32,
    pal_bank: u32,
    ext_pal: Option<&[u8]>,
    pram: &[u8],
    pram_base: usize,
) -> u32 {
    if let Some(slice) = ext_pal {
        let off = (pal_bank * 512 + pix * 2) as usize;
        if off + 1 < slice.len() {
            return rd16(slice, off);
        }
        // No bank mapped for this slot → fall back to base PRAM.
    }
    let pal_off = pram_base + (pix * 2) as usize;
    if pal_off + 1 < pram.len() {
        rd16(pram, pal_off)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put16(buf: &mut [u8], i: usize, v: u16) {
        buf[i * 2] = (v & 0xFF) as u8;
        buf[i * 2 + 1] = (v >> 8) as u8;
    }

    /// Fresh BgRegs with everything zeroed (matches `Default`).
    fn regs() -> BgRegs {
        BgRegs::default()
    }

    /// Out buffer pre-filled transparent, as the compositor does before calling.
    fn fresh_out() -> Vec<u32> {
        vec![PX_TRANSPARENT; LINE_W]
    }

    /// Fill a 4bpp BG tile (32 bytes) at char-relative slot with one pixel value
    /// (both nibbles), at flat vram offset `base + slot*32`.
    fn fill_tile4bpp(vram: &mut [u8], base: usize, slot: usize, v: u8) {
        let off = base + slot * 32;
        let byte = v | (v << 4);
        for i in 0..32 {
            vram[off + i] = byte;
        }
    }

    /// Fill an 8bpp BG tile (64 bytes) with one pixel value.
    fn fill_tile8bpp(vram: &mut [u8], base: usize, slot: usize, v: u8) {
        let off = base + slot * 64;
        for i in 0..64 {
            vram[off + i] = v;
        }
    }

    /// Write a text screen-entry (tile/flip/palbank) at map base + index.
    fn set_map_entry(vram: &mut [u8], screen_base: usize, idx: usize, entry: u16) {
        let off = screen_base + idx * 2;
        vram[off] = (entry & 0xFF) as u8;
        vram[off + 1] = (entry >> 8) as u8;
    }

    // The tilemap lives at a dedicated screen base so it doesn't overlap the
    // tile char data (which starts at char base 0). 0x6000 / 0x800 = 12, so the
    // BGxCNT screen-base field (bits 8..12) must be 12.
    const MAP_BASE: usize = 0x6000;
    const SCREEN_BITS: u32 = 12 << 8;

    // 4bpp: a single tile at map[0] with palette bank/colour resolves through
    // base PRAM and lands on the whole leftmost 8 px.
    #[test]
    fn text_4bpp_basic() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // tile 0, 4bpp, pixel value 1 → palette entry (bank0*16 + 1) = 1.
        fill_tile4bpp(&mut vram, 0, 0, 1);
        put16(&mut pram, 1, 0x7C00); // entry 1 = pure blue (BGR555 bit10..14)
        set_map_entry(&mut vram, MAP_BASE, 0, 0); // map[0] = tile 0
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS; // 4bpp, size 0, prio 0, char base 0

        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);

        for x in 0..8 {
            assert_eq!(out[x] & 0x7FFF, 0x7C00, "px {x}");
            assert_eq!(out[x] & 0x8000, 0, "px {x} should be drawn");
            assert_eq!((out[x] >> 16) & 3, 0, "layer id 0");
        }
    }

    // Color index 0 is always transparent regardless of palette contents.
    #[test]
    fn text_4bpp_index0_transparent() {
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];
        fill_tile4bpp(&mut vram, 0, 0, 0); // all pixels index 0
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        for x in 0..8 {
            assert_eq!(out[x], PX_TRANSPARENT, "px {x} transparent");
        }
    }

    // Priority + layer id are folded into the packed pixel high bits.
    #[test]
    fn text_priority_and_layer_bits() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        fill_tile4bpp(&mut vram, 0, 0, 1);
        put16(&mut pram, 1, 0x1234);
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut r = regs();
        r.cnt[2] = SCREEN_BITS | 2; // priority 2 on BG2
        let mut out = fresh_out();
        render_text_scanline(2, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        assert_eq!((out[0] >> 16) & 3, 2, "layer id 2");
        assert_eq!((out[0] >> 18) & 3, 2, "priority 2");
        assert_eq!(out[0] & 0x7FFF, 0x1234);
    }

    // Horizontal flip mirrors the in-tile X so the differing pixel columns swap.
    #[test]
    fn text_hflip() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // Build tile 0 row 0: px0=1, px1..7=2 (4bpp, two pixels/byte).
        let off = 0usize; // tile 0, row 0 = off..off+4
        vram[off] = 1 | (2 << 4); // px0=1, px1=2
        vram[off + 1] = 2 | (2 << 4); // px2=2, px3=2
        vram[off + 2] = 2 | (2 << 4);
        vram[off + 3] = 2 | (2 << 4);
        put16(&mut pram, 1, 0x0001);
        put16(&mut pram, 2, 0x0002);
        set_map_entry(&mut vram, MAP_BASE, 0, 0x0400); // tile 0 + hflip
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        // With hflip, screen px0 samples in_tile_x = 7 (index 2), px7 samples
        // in_tile_x = 0 (index 1).
        assert_eq!(out[0] & 0x7FFF, 0x0002, "hflip px0 = original px7");
        assert_eq!(out[7] & 0x7FFF, 0x0001, "hflip px7 = original px0");
    }

    // Vertical flip mirrors the in-tile Y.
    #[test]
    fn text_vflip() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // tile 0: row 0 px0 = 1, row 7 px0 = 2 (4bpp, 4 bytes/row).
        vram[0] = 1; // row 0, px0=1
        vram[7 * 4] = 2; // row 7, px0=2
        put16(&mut pram, 1, 0x0001);
        put16(&mut pram, 2, 0x0002);
        set_map_entry(&mut vram, MAP_BASE, 0, 0x0800); // tile 0 + vflip
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        // y=0 with vflip samples in_tile_y = 7 → colour 2.
        assert_eq!(out[0] & 0x7FFF, 0x0002, "vflip y0 = row 7");
    }

    // 8bpp without ext palette indexes flat base PRAM by the raw byte.
    #[test]
    fn text_8bpp_base_palette() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // char_base 1 (BGxCNT bits 2..3 = 1) → char at 0x4000.
        let char_base = 0x4000usize;
        fill_tile8bpp(&mut vram, char_base, 0, 200);
        put16(&mut pram, 200, 0x03E0); // entry 200 = green
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut r = regs();
        r.cnt[1] = SCREEN_BITS | 0x80 | (1 << 2); // 8bpp, char_base 1
        let mut out = fresh_out();
        render_text_scanline(1, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        for x in 0..8 {
            assert_eq!(out[x] & 0x7FFF, 0x03E0, "px {x}");
        }
    }

    // 8bpp WITH ext palette: per-tile palBank picks the 256-entry sub-palette.
    #[test]
    fn text_8bpp_ext_palette() {
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];
        fill_tile8bpp(&mut vram, 0, 0, 5); // color idx 5
        // palBank 3 → ext slot offset 3*512 + 5*2.
        set_map_entry(&mut vram, MAP_BASE, 0, 3 << 12);
        // ext palette: 16 banks * 512 bytes.
        let mut ext = vec![0u8; 16 * 512];
        let off = 3 * 512 + 5 * 2;
        ext[off] = 0xCD;
        ext[off + 1] = 0x6A; // 0x6ACD & 0x7FFF
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS | 0x80; // 8bpp
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, Some(&ext), &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x6ACD & 0x7FFF);
    }

    // Ext palette with NO bank mapped for the entry (slice too short) falls back
    // to flat base PRAM.
    #[test]
    fn text_8bpp_ext_palette_fallback() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        fill_tile8bpp(&mut vram, 0, 0, 5);
        put16(&mut pram, 5, 0x5AB5); // base PRAM entry 5
        set_map_entry(&mut vram, MAP_BASE, 0, 3 << 12); // palBank 3
        // ext slice covers only bank 0 (512 bytes) → bank 3 out of range.
        let ext = vec![0u8; 512];
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS | 0x80;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, Some(&ext), &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x5AB5 & 0x7FFF, "fallback to base PRAM");
    }

    // Ext palette is ignored for 4bpp (only the 16-color base sub-palette).
    #[test]
    fn text_4bpp_ignores_ext_palette() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        fill_tile4bpp(&mut vram, 0, 0, 1);
        put16(&mut pram, 1, 0x1111);
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut ext = vec![0u8; 16 * 512];
        ext[2] = 0xFF;
        ext[3] = 0x7F; // would be 0x7FFF if (wrongly) used
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, Some(&ext), &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x1111, "4bpp must use base PRAM, not ext");
    }

    // Horizontal scroll (HOFS) shifts the sampled world column.
    #[test]
    fn text_hofs_scroll() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // tile 0 = colour 1, tile 1 = colour 2. HOFS 8 → screen px0 samples
        // world x 8, which is tile column 1 → tile at map[1].
        fill_tile4bpp(&mut vram, 0, 0, 1);
        fill_tile4bpp(&mut vram, 0, 1, 2);
        put16(&mut pram, 1, 0x0001);
        put16(&mut pram, 2, 0x0002);
        set_map_entry(&mut vram, MAP_BASE, 0, 0); // map[0] = tile 0
        set_map_entry(&mut vram, MAP_BASE, 1, 1); // map[1] = tile 1
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        r.hofs[0] = 8;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x0002, "HOFS 8 samples tile 1");
    }

    // Vertical scroll into the second 256-px quadrant of a 256x512 map selects
    // the bottom screen block (map_off += 0x800).
    #[test]
    fn text_vofs_quadrant_select() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        fill_tile4bpp(&mut vram, 0, 0, 1);
        fill_tile4bpp(&mut vram, 0, 2, 2); // tile slot 2 = colour 2
        put16(&mut pram, 1, 0x0001);
        put16(&mut pram, 2, 0x0002);
        // Bottom quadrant lives at screen_base + 0x800. tile_y for y=0 there = 0.
        set_map_entry(&mut vram, MAP_BASE, 0, 0); // top map[0] = tile 0
        set_map_entry(&mut vram, MAP_BASE + 0x800, 0, 2); // bottom map[0] = tile 2
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS | (2 << 14); // size code 2 → 256x512
        r.vofs[0] = 256; // scroll into bottom quadrant
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x0002, "VOFS 256 selects bottom quadrant");
    }

    // char_extra / screen_extra (the DISPCNT engine-A global bases) shift both
    // the map and char fetches.
    #[test]
    fn text_global_base_extras() {
        let mut vram = vec![0u8; 0x40000];
        let mut pram = vec![0u8; 0x800];
        let char_extra = 0x10000usize;
        let screen_extra = 0x20000usize;
        fill_tile4bpp(&mut vram, char_extra, 0, 1);
        put16(&mut pram, 1, 0x2222);
        // BGxCNT screen base 0 + screen_extra → map at 0x20000.
        set_map_entry(&mut vram, screen_extra, 0, 0);
        let r = regs(); // cnt[0]=0: char base 0, screen base 0
        let mut out = fresh_out();
        render_text_scanline(
            0, 0, &r, 0, &vram, 0, char_extra, screen_extra, &pram, 0, None, &mut out,
        );
        assert_eq!(out[0] & 0x7FFF, 0x2222);
    }

    // Engine B palette base (pram_base = 0x400) is honoured.
    #[test]
    fn text_engine_b_pram_base() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        fill_tile4bpp(&mut vram, 0, 0, 1);
        // entry (0x400/2 + 1) = pram index 0x201 for engine B.
        put16(&mut pram, 0x200 + 1, 0x4444);
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS;
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0x400, None, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x4444);
    }

    // Mosaic quantizes horizontal sample coords into blocks.
    #[test]
    fn text_mosaic_horizontal() {
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        // tile 0 row 0: px0=1, px1=2, px2=3, px3=4.
        vram[0] = 1 | (2 << 4);
        vram[1] = 3 | (4 << 4);
        for i in 1..5 {
            put16(&mut pram, i, i as u16);
        }
        set_map_entry(&mut vram, MAP_BASE, 0, 0);
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS | 0x40; // mosaic enable bit
        let mosaic = 1; // h block size = 2 (low nibble 1 → +1)
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, mosaic, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        // Block size 2: px0,px1 both sample world x0 → colour 1.
        assert_eq!(out[0] & 0x7FFF, 1);
        assert_eq!(out[1] & 0x7FFF, 1, "mosaic block: px1 reuses px0 sample");
        assert_eq!(out[2] & 0x7FFF, 3, "next block samples world x2");
    }

    // Out-of-range tile fetch (tile pointing beyond vram) yields transparent
    // rather than panicking.
    #[test]
    fn text_oob_tile_transparent() {
        // vram only covers the map quadrant + a couple entries; the tile data
        // address (0x3FF * 64) is way past the end.
        let mut vram = vec![0u8; MAP_BASE + 0x10];
        let pram = vec![0u8; 0x800];
        set_map_entry(&mut vram, MAP_BASE, 0, 0x3FF);
        let mut r = regs();
        r.cnt[0] = SCREEN_BITS | 0x80; // 8bpp
        let mut out = fresh_out();
        render_text_scanline(0, 0, &r, 0, &vram, 0, 0, 0, &pram, 0, None, &mut out);
        assert_eq!(out[0], PX_TRANSPARENT);
    }
}
