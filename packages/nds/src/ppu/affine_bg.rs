//! Affine + extended-BG scanline renderer (DS engines A/B). Ported from
//! ../../ds-recomp/src/ppu/affine_bg.ts and adapted from the GBA core's
//! `render_mode_affine` (../../core/src/ppu.rs).
//!
//! On the DS, BG2/BG3 can be:
//!   - "affine": always affine-tile (8bpp tile map), ignores BGxCNT bit 7;
//!   - "extended": BGxCNT bit 7 selects affine-tile vs 256-color bitmap vs
//!     direct-color (BGR555) bitmap.
//! Both feed this renderer; `force_tile` distinguishes them.
//!
//! Three sub-modes (chosen by `affine_sub_mode`):
//!   - Tile: 8-bit tile indices in the screen map (one byte/entry, NO flip or
//!     palette bits like text mode), 256-color tile pixels in char data, affine
//!     transform applied per pixel. Sizes 128/256/512/1024 square.
//!   - BitmapPalette (extended bit7=1, bit2=0): 256-color bitmap, affine.
//!   - BitmapDirect (extended bit7=1, bit2=1): BGR555 direct-color bitmap with
//!     bit15 as per-pixel alpha, affine.
//! For the extended bitmap modes the size table differs (512x256 for code 2).
//!
//! The affine math (Q8.8 fixed-point):
//!   world_x = (ref_x_latched + PA*x) >> 8
//!   world_y = (ref_y_latched + PC*x) >> 8
//! The PB/PD per-line advance lives in `advance_affine_ref_for_scanline`, which
//! the compositor calls once per visible HBlank (GBATEK "PB/PD added at HBlank").
//!
//! Wraparound is BGxCNT bit 13: set → wrap modulo BG size; clear → out-of-bounds
//! samples read transparent.
//!
//! Output convention (matches text_bg / the compositor's packed format from
//! engine_a.rs): each `out` entry is a packed u32 — bits 0..14 BGR555, bit15
//! transparent (PX_TRANSPARENT), bits 16..17 layer, bits 18..19 priority. Opaque
//! pixels clear bit15 and OR in the layer/priority field.
//!
//! Ownership (CONTRACT.md): NO `&mut Nds`. Takes the BG register state +
//! borrowed VRAM/PRAM slices + resolved bases, writes one scanline of packed
//! pixels into `out`.

use super::engine_a::{BgRegs, PX_TRANSPARENT};

/// Affine TILE-mode sizes (BGxCNT bit 7 = 0), in pixels. SQUARE — the size
/// field is log2 of the width in tiles:
///   00 →  128x128 ( 16x16 tiles)
///   01 →  256x256 ( 32x32 tiles)
///   10 →  512x512 ( 64x64 tiles)
///   11 → 1024x1024 (128x128 tiles)
const AFFINE_TILE_SIZES: [(i32, i32); 4] = [(128, 128), (256, 256), (512, 512), (1024, 1024)];

/// Extended-BITMAP sizes (BGxCNT bit 7 = 1). NOT identical to the affine-tile
/// sizes for codes 2/3:
///   00 → 128x128
///   01 → 256x256
///   10 → 512x256
///   11 → 512x512
const EXT_BITMAP_SIZES: [(i32, i32); 4] = [(128, 128), (256, 256), (512, 256), (512, 512)];

/// Which extended/affine sub-mode a BG slot resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AffineSubMode {
    Tile,
    BitmapPalette,
    BitmapDirect,
}

/// Decide the affine sub-mode for a BG. `force_tile` short-circuits the bit-7
/// check for "plain affine" slots (the caller passes true for DISPCNT modes
/// 1/2 BG2/BG3 affine slots): those are always tile regardless of BGxCNT bit 7.
/// Otherwise (extended slot) bit 7 selects bitmap, and bit 2 picks direct vs
/// palette.
#[inline]
fn affine_sub_mode(bgcnt: u32, force_tile: bool) -> AffineSubMode {
    if force_tile || (bgcnt & 0x80) == 0 {
        AffineSubMode::Tile
    } else if (bgcnt & 0x4) != 0 {
        AffineSubMode::BitmapDirect
    } else {
        AffineSubMode::BitmapPalette
    }
}

/// Render one affine/extended-BG scanline for layer `bg` (2 or 3) at screen row
/// `y` into `out`. `bg_regs` carries PA..PD + the per-frame latched reference
/// (`ref_x_latched`/`ref_y_latched`) the wave accumulates PB/PD into per row.
/// `force_tile` = true for plain-affine slots (ignore BGxCNT bit 7); false for
/// extended slots (bit 7 decides tile vs bitmap).
///
/// `bg_vram_base` is the engine's resolved BG VRAM base (the router/vramcnt
/// already mapped the engine's BG region into a flat slice that starts at
/// `bg_vram_base`). `char_extra`/`screen_extra` are the DISPCNT global char/map
/// base offsets (engine A only; 0 for engine B). `pram_base` is the engine's
/// palette base within `pram` (0 / 0x400). `ext_pal` is the resolved extended
/// palette slice for this BG slot, when extended palettes are enabled (unused
/// by affine tile mode on hardware — affine-tile is implicitly palette bank 0 —
/// but threaded for parity with text_bg's signature).
#[allow(clippy::too_many_arguments)]
pub fn render_affine_bg_scanline(
    bg: usize,
    y: u32,
    bg_regs: &BgRegs,
    vram: &[u8],
    bg_vram_base: usize,
    char_extra: usize,
    screen_extra: usize,
    pram: &[u8],
    pram_base: usize,
    ext_pal: Option<&[u8]>,
    force_tile: bool,
    out: &mut [u32],
) {
    let _ = (y, ext_pal); // y is implicit in the pre-accumulated ref latch
    let bgcnt = bg_regs.cnt[bg];
    let sub_mode = affine_sub_mode(bgcnt, force_tile);

    let size_code = ((bgcnt >> 14) & 0x3) as usize;
    let (w, h) = match sub_mode {
        AffineSubMode::Tile => AFFINE_TILE_SIZES[size_code],
        AffineSubMode::BitmapPalette | AffineSubMode::BitmapDirect => EXT_BITMAP_SIZES[size_code],
    };

    // BGxCNT bit 13 = wraparound enable.
    let wrap = (bgcnt & 0x2000) != 0;

    // Priority (BGxCNT bits 0..1) + layer id packed into the high bits of each
    // opaque pixel (mirrors text_bg / the GBA core's `layer_hi`).
    let priority = bgcnt & 0x3;
    let layer_hi = ((bg as u32) << 16) | (priority << 18);

    let pa = bg_regs.pa[bg];
    let pc = bg_regs.pc[bg];
    let ref_x = bg_regs.ref_x_latched[bg];
    let ref_y = bg_regs.ref_y_latched[bg];

    match sub_mode {
        AffineSubMode::Tile => render_affine_tile(
            bgcnt,
            vram,
            bg_vram_base,
            char_extra,
            screen_extra,
            pram,
            pram_base,
            w,
            h,
            wrap,
            ref_x,
            ref_y,
            pa,
            pc,
            layer_hi,
            out,
        ),
        AffineSubMode::BitmapPalette => render_affine_bitmap_palette(
            bgcnt,
            vram,
            bg_vram_base,
            pram,
            pram_base,
            w,
            h,
            wrap,
            ref_x,
            ref_y,
            pa,
            pc,
            layer_hi,
            out,
        ),
        AffineSubMode::BitmapDirect => render_affine_bitmap_direct(
            bgcnt,
            vram,
            bg_vram_base,
            w,
            h,
            wrap,
            ref_x,
            ref_y,
            pa,
            pc,
            layer_hi,
            out,
        ),
    }
}

/// Wrap or reject an out-of-bounds sample coord. Returns the in-bounds coord, or
/// `None` (read transparent) when wrap is off and the coord is outside [0, size).
#[inline]
fn map_coord(c: i32, size: i32, wrap: bool) -> Option<i32> {
    if wrap {
        // Euclidean modulo keeps negatives positive.
        Some(c.rem_euclid(size))
    } else if c < 0 || c >= size {
        None
    } else {
        Some(c)
    }
}

/// Read a little-endian BGR555 entry from a palette slice at byte offset `off`.
#[inline]
fn pram_color(pram: &[u8], off: usize) -> u32 {
    (pram[off] as u32) | ((pram[off + 1] as u32) << 8)
}

/// Affine TILE mode: 8-bit tile index in the screen map (one byte/entry, no flip
/// or palette bits), 256-color (8bpp) tile pixels — 64 bytes/tile. The screen
/// map stride is (w/8) bytes per tile row. Palette is implicitly bank 0 of this
/// engine's PRAM (DS affine-tile BGs don't use extended palettes).
#[allow(clippy::too_many_arguments)]
fn render_affine_tile(
    bgcnt: u32,
    vram: &[u8],
    bg_vram_base: usize,
    char_extra: usize,
    screen_extra: usize,
    pram: &[u8],
    pram_base: usize,
    w: i32,
    h: i32,
    wrap: bool,
    ref_x: i32,
    ref_y: i32,
    pa: i32,
    pc: i32,
    layer_hi: u32,
    out: &mut [u32],
) {
    // Screen/char bases: same encoding as text mode. Bits 8..12 → screen base in
    // 0x800 units, bits 2..5 → char base in 0x4000 units. Engine A's global
    // offsets (DISPCNT char/map base) come in as char_extra / screen_extra.
    let screen_base = ((bgcnt >> 8) & 0x1F) as usize * 0x800 + screen_extra;
    let char_base = ((bgcnt >> 2) & 0xF) as usize * 0x4000 + char_extra;
    let tiles_per_row = (w >> 3) as usize;

    let mut cur_x = ref_x;
    let mut cur_y = ref_y;
    for px in out.iter_mut() {
        let world_x = cur_x >> 8;
        let world_y = cur_y >> 8;
        cur_x = cur_x.wrapping_add(pa);
        cur_y = cur_y.wrapping_add(pc);

        let (wx, wy) = match (map_coord(world_x, w, wrap), map_coord(world_y, h, wrap)) {
            (Some(wx), Some(wy)) => (wx, wy),
            _ => {
                *px = PX_TRANSPARENT;
                continue;
            }
        };

        let tile_x = (wx >> 3) as usize;
        let tile_y = (wy >> 3) as usize;
        let map_off = bg_vram_base + screen_base + tile_y * tiles_per_row + tile_x;
        if map_off >= vram.len() {
            *px = PX_TRANSPARENT;
            continue;
        }
        let tile_num = vram[map_off] as usize;
        let px_off =
            bg_vram_base + char_base + tile_num * 64 + (wy & 7) as usize * 8 + (wx & 7) as usize;
        if px_off >= vram.len() {
            *px = PX_TRANSPARENT;
            continue;
        }
        let pal_idx = vram[px_off] as usize;
        if pal_idx == 0 {
            *px = PX_TRANSPARENT;
            continue;
        }
        let c = pram_color(pram, pram_base + pal_idx * 2);
        *px = (c & 0x7FFF) | layer_hi;
    }
}

/// Extended-affine 256-color bitmap. The bitmap origin is BGxCNT "screen base"
/// (bits 8..12) × 0x4000 from the engine's BG VRAM base. One byte/pixel palette
/// index; stride is `w` bytes per row. Palette is this engine's PRAM bank 0.
#[allow(clippy::too_many_arguments)]
fn render_affine_bitmap_palette(
    bgcnt: u32,
    vram: &[u8],
    bg_vram_base: usize,
    pram: &[u8],
    pram_base: usize,
    w: i32,
    h: i32,
    wrap: bool,
    ref_x: i32,
    ref_y: i32,
    pa: i32,
    pc: i32,
    layer_hi: u32,
    out: &mut [u32],
) {
    let base_off = ((bgcnt >> 8) & 0x1F) as usize * 0x4000;
    let bitmap_start = bg_vram_base + base_off;
    let w_us = w as usize;

    let mut cur_x = ref_x;
    let mut cur_y = ref_y;
    for px in out.iter_mut() {
        let world_x = cur_x >> 8;
        let world_y = cur_y >> 8;
        cur_x = cur_x.wrapping_add(pa);
        cur_y = cur_y.wrapping_add(pc);

        let (wx, wy) = match (map_coord(world_x, w, wrap), map_coord(world_y, h, wrap)) {
            (Some(wx), Some(wy)) => (wx as usize, wy as usize),
            _ => {
                *px = PX_TRANSPARENT;
                continue;
            }
        };

        let off = bitmap_start + wy * w_us + wx;
        if off >= vram.len() {
            *px = PX_TRANSPARENT;
            continue;
        }
        let pal_idx = vram[off] as usize;
        if pal_idx == 0 {
            *px = PX_TRANSPARENT;
            continue;
        }
        let c = pram_color(pram, pram_base + pal_idx * 2);
        *px = (c & 0x7FFF) | layer_hi;
    }
}

/// Extended-affine 16-bit direct-color bitmap (BGR555/pixel). DS honours bit 15
/// as per-pixel alpha: a pixel with bit 15 = 0 reads transparent. Stride is
/// `w*2` bytes per row.
#[allow(clippy::too_many_arguments)]
fn render_affine_bitmap_direct(
    bgcnt: u32,
    vram: &[u8],
    bg_vram_base: usize,
    w: i32,
    h: i32,
    wrap: bool,
    ref_x: i32,
    ref_y: i32,
    pa: i32,
    pc: i32,
    layer_hi: u32,
    out: &mut [u32],
) {
    let base_off = ((bgcnt >> 8) & 0x1F) as usize * 0x4000;
    let bitmap_start = bg_vram_base + base_off;
    let w_us = w as usize;

    let mut cur_x = ref_x;
    let mut cur_y = ref_y;
    for px in out.iter_mut() {
        let world_x = cur_x >> 8;
        let world_y = cur_y >> 8;
        cur_x = cur_x.wrapping_add(pa);
        cur_y = cur_y.wrapping_add(pc);

        let (wx, wy) = match (map_coord(world_x, w, wrap), map_coord(world_y, h, wrap)) {
            (Some(wx), Some(wy)) => (wx as usize, wy as usize),
            _ => {
                *px = PX_TRANSPARENT;
                continue;
            }
        };

        let off = bitmap_start + (wy * w_us + wx) * 2;
        if off + 1 >= vram.len() {
            *px = PX_TRANSPARENT;
            continue;
        }
        let c = (vram[off] as u32) | ((vram[off + 1] as u32) << 8);
        // Bit 15 = alpha for direct-color bitmaps. Clear → transparent.
        if (c & 0x8000) == 0 {
            *px = PX_TRANSPARENT;
            continue;
        }
        *px = (c & 0x7FFF) | layer_hi;
    }
}

/// Advance the per-frame affine reference latch by one scanline's worth of
/// PB/PD (called once per visible HBlank by the compositor). Adapted from the
/// TS `advanceAffineRefForScanline`. Mutates `ref_*_latched` in place.
pub fn advance_affine_ref_for_scanline(bg_regs: &mut BgRegs, bg: usize) {
    bg_regs.ref_x_latched[bg] = bg_regs.ref_x_latched[bg].wrapping_add(bg_regs.pb[bg]);
    bg_regs.ref_y_latched[bg] = bg_regs.ref_y_latched[bg].wrapping_add(bg_regs.pd[bg]);
}

// =====================================================================
// Tests — adapted from the GBA core's affine-BG suite (../../core/src/ppu.rs)
// and the DS affine_bg.ts behaviour. Each test seeds a `BgRegs`, a flat VRAM
// slice + PRAM, calls `render_affine_bg_scanline` in isolation, and asserts the
// packed-pixel output.
// =====================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const T: u32 = PX_TRANSPARENT;

    /// Write a little-endian u16 into `buf` at logical index `i` (byte off i*2).
    fn put16(buf: &mut [u8], i: usize, v: u16) {
        buf[i * 2] = (v & 0xFF) as u8;
        buf[i * 2 + 1] = (v >> 8) as u8;
    }

    /// A BgRegs with identity affine (PA=PD=0x100, PB=PC=0, ref=0) for `bg`.
    fn identity_regs(bg: usize, cnt: u32) -> BgRegs {
        let mut r = BgRegs::default();
        r.cnt[bg] = cnt;
        r.pa[bg] = 0x100;
        r.pb[bg] = 0;
        r.pc[bg] = 0;
        r.pd[bg] = 0x100;
        r.ref_x[bg] = 0;
        r.ref_y[bg] = 0;
        r.ref_x_latched[bg] = 0;
        r.ref_y_latched[bg] = 0;
        r
    }

    // ─── tile mode ──────────────────────────────────────────────────────────

    // Identity affine, 256x256 (32x32 tiles), tile 0 in map[0], an 8bpp tile
    // whose pixels are palette index 5 → first 8 px of the line read color[5].
    #[test]
    fn affine_tile_identity_samples_tile0() {
        // size code 01 → 256x256. char base 1 (0x4000) so tile char data does
        // not collide with the screen map at offset 0.
        let cnt = (1 << 14) | (1 << 2);
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        for i in 0..64 {
            vram[0x4000 + i] = 5;
        }
        // screen map entry 0 = tile 0.
        vram[0] = 0;
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 5, 0x1234 & 0x7FFF);
        let mut out = [0u32; 256];

        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );

        // First 8 pixels are within tile (0,0) → palette 5.
        let want = (0x1234u32 & 0x7FFF) | (2 << 16);
        for i in 0..8 {
            assert_eq!(out[i], want, "px {i}");
        }
    }

    // palette index 0 in the tile reads transparent.
    #[test]
    fn affine_tile_index0_transparent() {
        let cnt = (1 << 14) | (1 << 2);
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        vram[0] = 0; // tile 0
                     // char data left all-zero → palette index 0 everywhere.
        let pram = vec![0u8; 0x800];
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        for (i, &p) in out.iter().enumerate() {
            assert_eq!(p, T, "px {i}");
        }
    }

    // Out-of-bounds without wrap reads transparent; with wrap it samples mod size.
    #[test]
    fn affine_tile_wrap_vs_noclip() {
        // 128x128 (size 00, 16x16 tiles). Put ref at x = 200<<8 (past 128) so the
        // first pixel is out of bounds.
        let cnt = (0 << 14) | (1 << 2); // size 00, char base 1
        let mut regs = identity_regs(2, cnt);
        regs.ref_x_latched[2] = 200 << 8;
        regs.ref_y_latched[2] = 0;
        let mut vram = vec![0u8; 0x20000];
        // tile 0 char data = index 7 everywhere.
        for i in 0..64 {
            vram[0x4000 + i] = 7;
        }
        // map all-zero (tile 0).
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 7, 0x03FF);
        let mut out = [0u32; 256];

        // No wrap: first pixel (world_x=200 >= 128) transparent.
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        assert_eq!(out[0], T);

        // Wrap on (bit 13): 200 mod 128 = 72, in-bounds → opaque index 7.
        regs.cnt[2] = cnt | 0x2000;
        let mut out2 = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out2,
        );
        let want = 0x03FFu32 | (2 << 16);
        assert_eq!(out2[0], want);
    }

    // Negative coord with wrap stays positive (rem_euclid).
    #[test]
    fn affine_tile_wrap_negative() {
        let cnt = (1 << 14) | (1 << 2) | 0x2000; // 256x256, char base 1, wrap
        let mut regs = identity_regs(2, cnt);
        regs.ref_x_latched[2] = -(1 << 8); // world_x = -1 → wraps to 255
        regs.ref_y_latched[2] = 0;
        let mut vram = vec![0u8; 0x20000];
        // tile at (31,0) — world (248..255) → tile_x 31, map idx 31.
        vram[31] = 1; // map entry 31 → tile 1
        for i in 0..64 {
            vram[0x4000 + 64 + i] = 9; // tile 1 char data = index 9
        }
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 9, 0x0555);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        let want = 0x0555u32 | (2 << 16);
        assert_eq!(out[0], want, "wrapped negative sample");
    }

    // Priority from BGxCNT bits 0-1 lands in bits 18-19 of the packed pixel.
    #[test]
    fn affine_priority_packed() {
        let cnt = (1 << 14) | (1 << 2) | 0x2; // priority 2, size 01, char base 1
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        for i in 0..64 {
            vram[0x4000 + i] = 1;
        }
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 1, 0x0001);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        assert_eq!(out[0] >> 18 & 0x3, 2, "priority field");
        assert_eq!(out[0] >> 16 & 0x3, 2, "layer field = bg2");
    }

    // BG3 affine works (layer field = 3).
    #[test]
    fn affine_bg3_layer() {
        let cnt = (1 << 14) | (1 << 2);
        let regs = identity_regs(3, cnt);
        let mut vram = vec![0u8; 0x20000];
        for i in 0..64 {
            vram[0x4000 + i] = 2;
        }
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 2, 0x0123);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            3, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        assert_eq!(out[0] >> 16 & 0x3, 3, "layer field = bg3");
        assert_eq!(out[0] & 0x7FFF, 0x0123);
    }

    // ─── extended bitmap modes ──────────────────────────────────────────────

    // Extended 256-color bitmap (bit7=1, bit2=0). Identity, 256x256.
    #[test]
    fn ext_bitmap_palette() {
        // bit7 set (0x80), bit2 clear → palette bitmap. size 01 → 256x256.
        // screen base 0 → bitmap origin = bg_vram_base.
        let cnt = 0x80 | (1 << 14);
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        // bitmap pixel (0,0) = palette index 3.
        vram[0] = 3;
        // pixel (1,0) = 0 → transparent.
        vram[1] = 0;
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 3, 0x0ABC);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, false, &mut out,
        );
        assert_eq!(out[0], 0x0ABCu32 | (2 << 16));
        assert_eq!(out[1], T);
    }

    // Extended direct-color bitmap (bit7=1, bit2=1). bit15 = alpha.
    #[test]
    fn ext_bitmap_direct() {
        let cnt = 0x80 | 0x4 | (1 << 14); // bit7 + bit2, size 01 → 256x256
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        // pixel 0: alpha set → opaque BGR555 0x3DEF.
        put16(&mut vram, 0, 0x8000 | 0x3DEF);
        // pixel 1: alpha clear → transparent.
        put16(&mut vram, 1, 0x0DEF);
        let pram = vec![0u8; 0x800];
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, false, &mut out,
        );
        assert_eq!(out[0], (0x3DEFu32 & 0x7FFF) | (2 << 16));
        assert_eq!(out[1], T);
    }

    // force_tile overrides bit7: even with bit7+bit2 set, a forced slot renders
    // as tile mode (samples the char/map, not the bitmap).
    #[test]
    fn force_tile_overrides_bit7() {
        let cnt = 0x80 | 0x4 | (1 << 14) | (1 << 2); // would be bitmap-direct
        let regs = identity_regs(2, cnt);
        let mut vram = vec![0u8; 0x20000];
        vram[0] = 0; // map tile 0
        for i in 0..64 {
            vram[0x4000 + i] = 4; // tile 0 char = index 4
        }
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 4, 0x0246);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, true, &mut out,
        );
        // Tile-mode result, not direct-bitmap.
        assert_eq!(out[0], 0x0246u32 | (2 << 16));
    }

    // ext bitmap size code 2 is 512x256 (not 512x512). y just below 256 is in
    // bounds; a y at 256 (no wrap) is out of bounds.
    #[test]
    fn ext_bitmap_size_512x256() {
        let cnt = 0x80 | (2 << 14); // palette bitmap, size 10 → 512x256
        let mut regs = identity_regs(2, cnt);
        // place ref at y = 255 → in bounds; one row's worth.
        regs.ref_y_latched[2] = 255 << 8;
        let mut vram = vec![0u8; 0x40000];
        // pixel (0, 255): off = 255*512 + 0.
        vram[255 * 512] = 8;
        let mut pram = vec![0u8; 0x800];
        put16(&mut pram, 8, 0x0111);
        let mut out = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, false, &mut out,
        );
        assert_eq!(out[0], 0x0111u32 | (2 << 16), "y=255 in bounds for 512x256");

        // y = 256 with no wrap → transparent.
        regs.ref_y_latched[2] = 256 << 8;
        let mut out2 = [0u32; 256];
        render_affine_bg_scanline(
            2, 0, &regs, &vram, 0, 0, 0, &pram, 0, None, false, &mut out2,
        );
        assert_eq!(out2[0], T, "y=256 out of bounds for 512x256");
    }

    // ─── ref advance ────────────────────────────────────────────────────────

    #[test]
    fn advance_ref_accumulates_pb_pd() {
        let mut regs = BgRegs::default();
        regs.pb[2] = 0x40;
        regs.pd[2] = -0x20;
        regs.ref_x_latched[2] = 1000;
        regs.ref_y_latched[2] = 2000;
        advance_affine_ref_for_scanline(&mut regs, 2);
        assert_eq!(regs.ref_x_latched[2], 1000 + 0x40);
        assert_eq!(regs.ref_y_latched[2], 2000 - 0x20);
        advance_affine_ref_for_scanline(&mut regs, 2);
        assert_eq!(regs.ref_x_latched[2], 1000 + 0x80);
        assert_eq!(regs.ref_y_latched[2], 2000 - 0x40);
    }

    // PB causes per-scanline vertical movement: after advancing, the latched ref
    // shifts so a later row samples a different texel. Sanity: identity + PB step
    // does not change ref_x when PB=0.
    #[test]
    fn advance_ref_pb_zero_noop_x() {
        let mut regs = BgRegs::default();
        regs.pb[3] = 0;
        regs.pd[3] = 0x100;
        regs.ref_x_latched[3] = 500;
        regs.ref_y_latched[3] = 0;
        advance_affine_ref_for_scanline(&mut regs, 3);
        assert_eq!(regs.ref_x_latched[3], 500);
        assert_eq!(regs.ref_y_latched[3], 0x100);
    }

    #[test]
    fn sub_mode_selection() {
        // force_tile always tile.
        assert_eq!(affine_sub_mode(0xFF, true), AffineSubMode::Tile);
        // bit7 clear → tile.
        assert_eq!(affine_sub_mode(0x00, false), AffineSubMode::Tile);
        // bit7 set, bit2 clear → palette bitmap.
        assert_eq!(affine_sub_mode(0x80, false), AffineSubMode::BitmapPalette);
        // bit7 set, bit2 set → direct bitmap.
        assert_eq!(affine_sub_mode(0x84, false), AffineSubMode::BitmapDirect);
    }
}
