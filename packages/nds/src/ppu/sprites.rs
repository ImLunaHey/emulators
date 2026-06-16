//! OBJ (sprite) scanline renderer (DS engines A/B). Ported from
//! ../../ds-recomp/src/ppu/sprites.ts and adapted from the GBA core's
//! `render_sprites` (../../core/src/ppu.rs).
//!
//! DS deltas over the GBA:
//!   - OBJ extended palettes: when DISPCNT bit31 is set AND a sprite is 8bpp,
//!     the OAM palette-bank field (attr2 bits 12..15) selects one of 16
//!     256-color sub-palettes inside the engine's 8 KB OBJ-ext-palette region
//!     (resolved by the caller and passed in as `ext_pal`) instead of the base
//!     OBJ PRAM.
//!   - Larger OBJ VRAM (up to 256 KB) and a programmable 1D tile-boundary
//!     granularity (DISPCNT bits 20..21 → 32/64/128/256-byte boundary).
//!   - The DS "bitmap OBJ" mode (attr0 mode == 3): the sprite samples a direct
//!     BGR555 bitmap out of OBJ VRAM instead of a tile/palette lookup. The
//!     bitmap base + stride depend on DISPCNT bits 4..6 (the OBJ bitmap mapping
//!     mode: 2D 128px-wide, 2D 256px-wide, or 1D). Alpha (bit15 of the source
//!     halfword) gates per-pixel transparency.
//!   - Per-engine OAM / OBJ palette base are passed in via `oam_base` /
//!     `obj_pram_base` so the same body serves both engines.
//!
//! Ownership (CONTRACT.md): NO `&mut Nds`. Takes DISPCNT/MOSAIC + borrowed
//! OAM/VRAM/PRAM slices (+ resolved OBJ bases + optional ext-palette slice) and
//! writes one scanline of packed OBJ pixels into `out`.
//!
//! Packed pixel format (engine_a.rs): bits 0..14 BGR555, bit15 transparent
//! (`PX_TRANSPARENT`), bits 16..17 layer (OBJ pixels leave it 0; they're
//! identified by being in `obj_line` and not transparent), bits 18..19
//! priority, bit20 OBJ semi-transparent, bit21 OBJ window.

use super::engine_a::{LINE_W, PX_TRANSPARENT};

/// Sprite pixel dimensions indexed by `[shape][size]` → (width, height).
/// From GBATEK §"OBJ Attribute 0/1". Shape 3 is prohibited (handled by the
/// caller skipping it). Widths and heights split so a single table lookup
/// yields both. (Adapted from the GBA core's SIZE_W/SIZE_H but kept as a single
/// (w,h) table to mirror the TS `SHAPE_SIZE` layout.)
const SHAPE_SIZE: [[(i32, i32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)], // square
    [(16, 8), (32, 8), (32, 16), (64, 32)], // horizontal
    [(8, 16), (8, 32), (16, 32), (32, 64)], // vertical
];

/// Render one OBJ scanline at screen row `y` into `out`.
///
/// Sprites are read from `oam[oam_base..]`; tiles / bitmaps from
/// `vram[obj_vram_base..]`; the standard OBJ palette from `pram[obj_pram_base..]`,
/// or `ext_pal` (the engine's resolved 8 KB OBJ-ext-palette region) when
/// 256-color extended palettes are active (DISPCNT bit31) on an 8bpp sprite.
///
/// `out` must be `LINE_W` wide and is expected to arrive cleared to
/// `PX_TRANSPARENT` by the compositor.
#[allow(clippy::too_many_arguments)]
pub fn render_obj_scanline(
    y: u32,
    dispcnt: u32,
    mosaic: u32,
    oam: &[u8],
    oam_base: usize,
    vram: &[u8],
    obj_vram_base: usize,
    pram: &[u8],
    obj_pram_base: usize,
    ext_pal: Option<&[u8]>,
    out: &mut [u32],
) {
    // DISPCNT bit 4 = OBJ 1D character mapping (0 = 2D, 1 = 1D).
    let obj_1d = (dispcnt & 0x10) != 0;
    // DISPCNT bits 20..21 = 1D tile-boundary granularity: 32 << n bytes.
    let tile_boundary: i32 = 32 << ((dispcnt >> 20) & 0x3);
    // DISPCNT bit 6 = OBJ bitmap 1D mapping; bit 5 = bitmap 2D dimension select
    // (0 → 128px wide 2D bitmap, 1 → 256px wide 2D bitmap). bits 5..6 together
    // form the "OBJ bitmap mapping" mode.
    let obj_bmp_1d = (dispcnt & 0x40) != 0;
    let obj_bmp_2d_wide = (dispcnt & 0x20) != 0;
    // DISPCNT bit 31 = OBJ extended palette enable.
    let obj_ext_pal_en = (dispcnt & 0x8000_0000) != 0;
    // 1D bitmap boundary: DISPCNT bit 22 → 128 or 256-byte units.
    let obj_bmp_1d_boundary: i32 = if (dispcnt & 0x0040_0000) != 0 { 256 } else { 128 };

    let obj_mosaic_h = (((mosaic >> 8) & 0xF) as i32) + 1;
    let obj_mosaic_v = (((mosaic >> 12) & 0xF) as i32) + 1;

    let y = y as i32;

    for s in 0..128 {
        let off = oam_base + s * 8;
        let a0 = (oam[off] as u32) | ((oam[off + 1] as u32) << 8);
        let a1 = (oam[off + 2] as u32) | ((oam[off + 3] as u32) << 8);
        let a2 = (oam[off + 4] as u32) | ((oam[off + 5] as u32) << 8);

        let affine = (a0 & 0x0100) != 0;
        // Bit 9 means "disabled" only for non-affine sprites; for affine it is
        // the double-size flag.
        let disabled = !affine && (a0 & 0x0200) != 0;
        if disabled {
            continue;
        }

        let mode = (a0 >> 10) & 0x3; // 0=normal, 1=semi-trans, 2=window, 3=bitmap
        let shape = ((a0 >> 14) & 0x3) as usize;
        if shape == 3 {
            continue; // prohibited shape
        }
        let size = ((a1 >> 14) & 0x3) as usize;
        let (w, h) = SHAPE_SIZE[shape][size];

        let mosaic_on = (a0 & 0x1000) != 0;
        let is_8bpp = (a0 & 0x2000) != 0;
        let is_bitmap = mode == 3;
        let semi = mode == 1;
        let obj_window = mode == 2;

        // Double-size only applies to affine sprites; it widens the on-screen
        // bounding box (giving the matrix room to rotate) without changing the
        // texel range.
        let double_size = affine && (a0 & 0x0200) != 0;
        let draw_w = if double_size { w * 2 } else { w };
        let draw_h = if double_size { h * 2 } else { h };

        // Y position wraps within 256; treat the top half of that range as
        // negative so partially-offscreen sprites at the top still draw.
        let mut y_pos = (a0 & 0xFF) as i32;
        if y_pos >= 192 {
            y_pos -= 256;
        }
        let in_sprite_y = y - y_pos;
        if in_sprite_y < 0 || in_sprite_y >= draw_h {
            continue;
        }

        // X position is 9-bit signed.
        let mut x_pos = (a1 & 0x1FF) as i32;
        if x_pos >= 256 {
            x_pos -= 512;
        }
        if x_pos + draw_w <= 0 || x_pos >= LINE_W as i32 {
            continue;
        }

        let priority = (a2 >> 10) & 0x3;
        let pal_bank = (a2 >> 12) & 0xF;
        let tile_num = (a2 & 0x3FF) as i32;

        // Packed high bits common to every pixel of this sprite. OBJ pixels
        // carry no explicit layer field (the compositor identifies them by being
        // in obj_line and non-transparent); priority sits in bits 18..19.
        let pix_hi = (priority << 18)
            | (if semi { 1 << 20 } else { 0 })
            | (if obj_window { 1 << 21 } else { 0 });

        // Tiles-per-tile and the 1D/2D row stride (in 32-byte tile units for
        // tile sprites). For 8bpp a tile occupies 2 tile-number slots.
        let tiles_per_tile = if is_8bpp { 2 } else { 1 };
        let row_stride_tiles = if obj_1d {
            (w >> 3) * tiles_per_tile
        } else {
            32
        };

        // Affine matrix (PA..PD, signed Q8.8) pulled from the OAM affine column
        // (bytes 6/7 of OAM entries 4*idx + [0..3]). Identity by default.
        let mut p_a: i32 = 0x100;
        let mut p_b: i32 = 0;
        let mut p_c: i32 = 0;
        let mut p_d: i32 = 0x100;
        if affine {
            let mat_idx = ((a1 >> 9) & 0x1F) as usize;
            let mb = oam_base + mat_idx * 32;
            p_a = sign16((oam[mb + 6] as u32) | ((oam[mb + 7] as u32) << 8));
            p_b = sign16((oam[mb + 14] as u32) | ((oam[mb + 15] as u32) << 8));
            p_c = sign16((oam[mb + 22] as u32) | ((oam[mb + 23] as u32) << 8));
            p_d = sign16((oam[mb + 30] as u32) | ((oam[mb + 31] as u32) << 8));
        }

        let cx = draw_w >> 1;
        let cy = draw_h >> 1;
        let half_w = w >> 1;
        let half_h = h >> 1;

        // The mosaic snaps the source scanline to the start of its V block.
        let eff_sprite_y = if mosaic_on {
            in_sprite_y - (in_sprite_y % obj_mosaic_v)
        } else {
            in_sprite_y
        };

        if !affine {
            let hflip = (a1 & 0x1000) != 0;
            let vflip = (a1 & 0x2000) != 0;
            let mut ty = eff_sprite_y;
            if vflip {
                ty = h - 1 - ty;
            }
            for px in 0..w {
                let screen_x = x_pos + px;
                if screen_x < 0 || screen_x >= LINE_W as i32 {
                    continue;
                }
                // Mosaic snaps the screen X to a block start, then re-derives the
                // sprite-relative pixel from it.
                let eff_px = if mosaic_on {
                    let sx = screen_x - (screen_x % obj_mosaic_h);
                    sx - x_pos
                } else {
                    px
                };
                if eff_px < 0 || eff_px >= w {
                    continue;
                }
                let tx = if hflip { w - 1 - eff_px } else { eff_px };
                plot_obj_pixel(
                    screen_x as usize,
                    tx,
                    ty,
                    w,
                    h,
                    tile_num,
                    pal_bank,
                    priority,
                    pix_hi,
                    is_8bpp,
                    is_bitmap,
                    obj_1d,
                    tile_boundary,
                    row_stride_tiles,
                    obj_bmp_1d,
                    obj_bmp_2d_wide,
                    obj_bmp_1d_boundary,
                    obj_ext_pal_en,
                    vram,
                    obj_vram_base,
                    pram,
                    obj_pram_base,
                    ext_pal,
                    out,
                );
            }
            continue;
        }

        // Affine path: walk the on-screen bounding box, transforming each pixel
        // back into texel space via the 8.8 matrix and rejecting samples outside
        // [0,w) x [0,h). Mosaic is applied by quantizing the destination column.
        let dy = eff_sprite_y - cy;
        let base_x = p_a
            .wrapping_mul(-cx)
            .wrapping_add(p_b.wrapping_mul(dy))
            .wrapping_add(half_w << 8);
        let base_y = p_c
            .wrapping_mul(-cx)
            .wrapping_add(p_d.wrapping_mul(dy))
            .wrapping_add(half_h << 8);
        for px in 0..draw_w {
            let screen_x = x_pos + px;
            if screen_x < 0 || screen_x >= LINE_W as i32 {
                continue;
            }
            // For mosaic, sample at the start of the H block (in screen space).
            let eff_px = if mosaic_on {
                let sx = screen_x - (screen_x % obj_mosaic_h);
                sx - x_pos
            } else {
                px
            };
            let src_x = base_x.wrapping_add(p_a.wrapping_mul(eff_px)) >> 8;
            let src_y = base_y.wrapping_add(p_c.wrapping_mul(eff_px)) >> 8;
            if src_x < 0 || src_x >= w || src_y < 0 || src_y >= h {
                continue;
            }
            plot_obj_pixel(
                screen_x as usize,
                src_x,
                src_y,
                w,
                h,
                tile_num,
                pal_bank,
                priority,
                pix_hi,
                is_8bpp,
                is_bitmap,
                obj_1d,
                tile_boundary,
                row_stride_tiles,
                obj_bmp_1d,
                obj_bmp_2d_wide,
                obj_bmp_1d_boundary,
                obj_ext_pal_en,
                vram,
                obj_vram_base,
                pram,
                obj_pram_base,
                ext_pal,
                out,
            );
        }
    }
}

/// Sample one sprite texel at sprite-local `(tx, ty)` and, if opaque and
/// higher-priority than what is already there, write it into `out[screen_x]`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn plot_obj_pixel(
    screen_x: usize,
    tx: i32,
    ty: i32,
    w: i32,
    h: i32,
    tile_num: i32,
    pal_bank: u32,
    priority: u32,
    pix_hi: u32,
    is_8bpp: bool,
    is_bitmap: bool,
    obj_1d: bool,
    tile_boundary: i32,
    row_stride_tiles: i32,
    obj_bmp_1d: bool,
    obj_bmp_2d_wide: bool,
    obj_bmp_1d_boundary: i32,
    obj_ext_pal_en: bool,
    vram: &[u8],
    obj_vram_base: usize,
    pram: &[u8],
    obj_pram_base: usize,
    ext_pal: Option<&[u8]>,
    out: &mut [u32],
) {
    debug_assert!(tx >= 0 && tx < w && ty >= 0 && ty < h);

    // Priority test up front: a non-transparent existing pixel with priority <=
    // ours wins (sprites are walked in OAM order; lower priority value = front).
    let cur = out[screen_x];
    if (cur & PX_TRANSPARENT) == 0 && ((cur >> 18) & 3) <= priority {
        return;
    }

    if is_bitmap {
        // DS bitmap OBJ: the sprite is a direct BGR555 bitmap in OBJ VRAM.
        // Address layout depends on the OBJ bitmap mapping mode.
        let addr = if obj_bmp_1d {
            // 1D: contiguous rows, base = tile_num * boundary.
            obj_vram_base as i32 + tile_num * obj_bmp_1d_boundary + (ty * w + tx) * 2
        } else {
            // 2D: the OBJ bitmap area is a fixed-width canvas (128 or 256 px).
            // tile_num low/high bits index the sprite's top-left within it.
            let canvas_w = if obj_bmp_2d_wide { 256 } else { 128 };
            let mask = if obj_bmp_2d_wide { 0x1F } else { 0x0F };
            let bx = (tile_num & mask) * 16;
            let by = (tile_num >> if obj_bmp_2d_wide { 5 } else { 4 }) * 8;
            obj_vram_base as i32 + ((by + ty) * canvas_w + (bx + tx)) * 2
        };
        if addr < 0 {
            return;
        }
        let addr = addr as usize;
        if addr + 1 >= vram.len() {
            return;
        }
        let raw = (vram[addr] as u32) | ((vram[addr + 1] as u32) << 8);
        // Bit 15 = alpha (1 = opaque). 0 → transparent.
        if (raw & 0x8000) == 0 {
            return;
        }
        out[screen_x] = (raw & 0x7FFF) | pix_hi;
        return;
    }

    let tile_col = tx >> 3;
    let tile_row = ty >> 3;
    let in_tile_x = tx & 7;
    let in_tile_y = ty & 7;

    if is_8bpp {
        // 8bpp tile = 64 bytes. 1D uses the programmable tile boundary for the
        // base; 2D uses a fixed 32-byte tile-number granularity.
        let addr = if obj_1d {
            obj_vram_base as i32
                + tile_num * tile_boundary
                + (tile_row * row_stride_tiles + tile_col * 2) * 32
                + in_tile_y * 8
                + in_tile_x
        } else {
            // 2D: 32 tiles per row in tile-number space, but 8bpp tiles span
            // two tile-number slots, so a tile is (tile_num*32) + row*1024 +
            // col*64 bytes from the base.
            obj_vram_base as i32
                + tile_num * 32
                + tile_row * 1024
                + tile_col * 64
                + in_tile_y * 8
                + in_tile_x
        };
        if addr < 0 {
            return;
        }
        let addr = addr as usize;
        if addr >= vram.len() {
            return;
        }
        let idx = vram[addr] as u32;
        if idx == 0 {
            return; // palette index 0 = transparent
        }
        let color = lookup_8bpp(idx, pal_bank, obj_ext_pal_en, ext_pal, pram, obj_pram_base);
        out[screen_x] = (color & 0x7FFF) | pix_hi;
    } else {
        // 4bpp tile = 32 bytes.
        let addr = if obj_1d {
            obj_vram_base as i32
                + tile_num * tile_boundary
                + (tile_row * row_stride_tiles + tile_col) * 32
                + in_tile_y * 4
                + (in_tile_x >> 1)
        } else {
            obj_vram_base as i32
                + tile_num * 32
                + tile_row * 1024
                + tile_col * 32
                + in_tile_y * 4
                + (in_tile_x >> 1)
        };
        if addr < 0 {
            return;
        }
        let addr = addr as usize;
        if addr >= vram.len() {
            return;
        }
        let byte = vram[addr] as u32;
        let idx = if (in_tile_x & 1) != 0 { byte >> 4 } else { byte & 0xF };
        if idx == 0 {
            return; // palette index 0 = transparent
        }
        // 4bpp: 16-entry bank from the OBJ palette base.
        let pal_off = obj_pram_base + ((pal_bank * 16 + idx) * 2) as usize;
        let color = pram16(pram, pal_off);
        out[screen_x] = (color & 0x7FFF) | pix_hi;
    }
}

/// Resolve an 8bpp OBJ palette index into a BGR555 color, honoring OBJ extended
/// palettes when enabled. When ext-pal is on, `pal_bank` selects one of 16
/// 256-color sub-palettes (512 bytes each) inside `ext_pal`; a 0x0000 entry (or
/// an unmapped region) falls back to base PRAM — matching the TS renderer's
/// friendlier behavior for emulator state-machine paths that never populate the
/// ext-pal bank.
#[inline]
fn lookup_8bpp(
    idx: u32,
    pal_bank: u32,
    obj_ext_pal_en: bool,
    ext_pal: Option<&[u8]>,
    pram: &[u8],
    obj_pram_base: usize,
) -> u32 {
    if obj_ext_pal_en {
        if let Some(ep) = ext_pal {
            let eoff = (pal_bank * 512 + idx * 2) as usize;
            if eoff + 1 < ep.len() {
                let c = (ep[eoff] as u32) | ((ep[eoff + 1] as u32) << 8);
                if c != 0 {
                    return c;
                }
            }
        }
    }
    // Base 256-color OBJ palette (index 0..255 from the OBJ palette base).
    pram16(pram, obj_pram_base + (idx * 2) as usize)
}

/// Read a little-endian BGR555 halfword from `pram` at byte offset `off`.
#[inline]
fn pram16(pram: &[u8], off: usize) -> u32 {
    if off + 1 >= pram.len() {
        return 0;
    }
    (pram[off] as u32) | ((pram[off + 1] as u32) << 8)
}

/// Sign-extend a 16-bit value held in the low half of a u32 into an i32.
#[inline]
fn sign16(v: u32) -> i32 {
    (((v & 0xFFFF) << 16) as i32) >> 16
}

#[cfg(test)]
mod tests {
    use super::*;

    // Engine-A-style flat buffers big enough for OBJ VRAM + OAM + PRAM.
    fn fresh_line() -> Vec<u32> {
        vec![PX_TRANSPARENT; LINE_W]
    }

    /// An OAM buffer with all 128 sprite slots disabled (non-affine + bit9 set).
    /// On real hardware every OAM slot is an active sprite; a freshly-zeroed OAM
    /// is 128 valid 8x8 sprites stacked at (0,0). To exercise one sprite in
    /// isolation a test must start from an all-disabled OAM and enable the slots
    /// it cares about. (Engine A: 0x800 bytes = 128 * 8 + the affine columns.)
    fn fresh_oam() -> Vec<u8> {
        let mut oam = vec![0u8; 0x800];
        for s in 0..128 {
            // a0 = 0x0200: rot/scale clear, bit9 (disable) set.
            oam[s * 8 + 1] = 0x02;
        }
        oam
    }

    /// Write an OAM entry at slot `s` (engine-A base 0).
    fn set_oam(oam: &mut [u8], s: usize, a0: u16, a1: u16, a2: u16) {
        let o = s * 8;
        oam[o] = a0 as u8;
        oam[o + 1] = (a0 >> 8) as u8;
        oam[o + 2] = a1 as u8;
        oam[o + 3] = (a1 >> 8) as u8;
        oam[o + 4] = a2 as u8;
        oam[o + 5] = (a2 >> 8) as u8;
    }

    fn set_pram16(pram: &mut [u8], idx: usize, color: u16) {
        pram[idx * 2] = color as u8;
        pram[idx * 2 + 1] = (color >> 8) as u8;
    }

    #[test]
    fn regular_4bpp_2d_sprite_draws_first_tile_row() {
        let mut oam = fresh_oam();
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];

        // 8x8 (shape=square,size=0), 4bpp, at (x=10, y=0), tile 0, palbank 1.
        set_oam(&mut oam, 0, 0x0000, 0x000A, (1 << 12) | 0);
        // 4bpp pixel value 5 in palbank 1 → palette entry (1*16+5)=21.
        // OBJ palette base for engine A = 0x200.
        let obj_pram_base = 0x200usize;
        set_pram16(&mut pram, obj_pram_base / 2 + 21, 0x7C1F);

        // Tile 0, row 0: every pixel = index 5 (nibble 0x55 → low=5, high=5).
        for b in 0..4 {
            vram[b] = 0x55;
        }

        let mut out = fresh_line();
        // DISPCNT: 2D mapping (bit4 clear), no ext-pal.
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );

        // Pixels x=10..18 should be opaque with our color.
        for x in 10..18 {
            assert_eq!(out[x] & 0x8000, 0, "pixel {x} should be opaque");
            assert_eq!(out[x] & 0x7FFF, 0x7C1F, "pixel {x} color");
        }
        // Outside the sprite stays transparent.
        assert_eq!(out[9] & 0x8000, PX_TRANSPARENT);
        assert_eq!(out[18] & 0x8000, PX_TRANSPARENT);
    }

    #[test]
    fn palette_index_zero_is_transparent() {
        let mut oam = vec![0u8; 0x800];
        let vram = vec![0u8; 0x20000]; // all zero → all index 0
        let pram = vec![0u8; 0x800];
        set_oam(&mut oam, 0, 0x0000, 0x0000, 0);
        let mut out = fresh_line();
        render_obj_scanline(0, 0, 0, &oam, 0, &vram, 0, &pram, 0x200, None, &mut out);
        for x in 0..16 {
            assert_eq!(out[x] & 0x8000, PX_TRANSPARENT);
        }
    }

    #[test]
    fn hflip_reverses_row() {
        let mut oam = fresh_oam();
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        // 8x8 4bpp, hflip set (a1 bit12), at x=0, tile 0, palbank 0.
        set_oam(&mut oam, 0, 0x0000, 0x1000, 0);
        // Row 0: only the leftmost pixel (col 0) is index 1; rest 0.
        // 4bpp: byte0 low nibble = col0, high nibble = col1.
        vram[0] = 0x01;
        set_pram16(&mut pram, obj_pram_base / 2 + 1, 0x1234);

        let mut out = fresh_line();
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );
        // With hflip the source col0 lands at screen x=7.
        assert_eq!(out[7] & 0x8000, 0);
        assert_eq!(out[7] & 0x7FFF, 0x1234);
        assert_eq!(out[0] & 0x8000, PX_TRANSPARENT);
    }

    #[test]
    fn higher_priority_sprite_wins() {
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        // Sprite 0: priority 2, color A. Sprite 1: priority 0, color B.
        // Both at x=0, 8x8, 4bpp, tile 0, palbank 0, full row of index 1.
        set_oam(&mut oam, 0, 0x0000, 0x0000, 2 << 10);
        set_oam(&mut oam, 1, 0x0000, 0x0000, 0 << 10);
        for b in 0..4 {
            vram[b] = 0x11;
        }
        set_pram16(&mut pram, obj_pram_base / 2 + 1, 0x0AAA);

        let mut out = fresh_line();
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );
        // Priority 0 (sprite 1) wins: priority bits = 0.
        assert_eq!((out[0] >> 18) & 3, 0);
    }

    #[test]
    fn semi_transparent_and_window_flags() {
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        // mode=1 (semi-transparent) → a0 bit 10.
        set_oam(&mut oam, 0, 1 << 10, 0x0000, 0);
        for b in 0..4 {
            vram[b] = 0x11;
        }
        set_pram16(&mut pram, obj_pram_base / 2 + 1, 0x1111);

        let mut out = fresh_line();
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );
        assert_ne!(out[0] & (1 << 20), 0, "semi-transparent flag set");
        assert_eq!(out[0] & (1 << 21), 0, "obj-window flag clear");
    }

    #[test]
    fn obj_window_pixel_sets_window_bit() {
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];

        // mode=2 (OBJ window) → a0 bits 10..11 = 0b10.
        set_oam(&mut oam, 0, 2 << 10, 0x0000, 0);
        for b in 0..4 {
            vram[b] = 0x11;
        }

        let mut out = fresh_line();
        render_obj_scanline(0, 0, 0, &oam, 0, &vram, 0, &pram, 0x200, None, &mut out);
        assert_ne!(out[0] & (1 << 21), 0, "obj-window flag set");
    }

    #[test]
    fn ext_palette_8bpp_lookup() {
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        // 8x8, 8bpp (a0 bit13), ext-pal bank 3 (a2 bits 12..15), at x=0, tile 0.
        set_oam(&mut oam, 0, 0x2000, 0x0000, 3 << 12);
        // 8bpp tile 0, row 0, col 0 = index 7.
        vram[0] = 7;

        // Ext-pal region: bank 3, index 7 → offset 3*512 + 7*2.
        let mut ext = vec![0u8; 0x2000];
        let eoff = 3 * 512 + 7 * 2;
        ext[eoff] = 0xCD;
        ext[eoff + 1] = 0x6A; // 0x6ACD

        let mut out = fresh_line();
        // DISPCNT bit31 = ext-pal enable.
        render_obj_scanline(
            0,
            0x8000_0000,
            0,
            &oam,
            0,
            &vram,
            0,
            &pram,
            obj_pram_base,
            Some(&ext),
            &mut out,
        );
        assert_eq!(out[0] & 0x8000, 0);
        assert_eq!(out[0] & 0x7FFF, 0x6ACD & 0x7FFF);
    }

    #[test]
    fn ext_palette_zero_entry_falls_back_to_pram() {
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        set_oam(&mut oam, 0, 0x2000, 0x0000, 0); // 8bpp, bank 0
        vram[0] = 9; // index 9
        // ext-pal slot 0 index 9 = 0x0000 (unset) → fall back to base PRAM.
        let ext = vec![0u8; 0x2000];
        set_pram16(&mut pram, obj_pram_base / 2 + 9, 0x4242);

        let mut out = fresh_line();
        render_obj_scanline(
            0,
            0x8000_0000,
            0,
            &oam,
            0,
            &vram,
            0,
            &pram,
            obj_pram_base,
            Some(&ext),
            &mut out,
        );
        assert_eq!(out[0] & 0x7FFF, 0x4242);
    }

    #[test]
    fn bitmap_obj_direct_color() {
        let mut oam = fresh_oam();
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];

        // mode=3 (bitmap), 8x8, 1D bitmap mapping, at x=0, tile_num 0.
        set_oam(&mut oam, 0, 3 << 10, 0x0000, 0);
        // 1D bitmap: base = tile_num*boundary + (y*w + x)*2. y=0,x=0 → addr 0.
        // Opaque BGR555 color (bit15 set).
        vram[0] = 0x34;
        vram[1] = 0x92; // 0x9234, bit15 set, color = 0x1234
        // x=1 → addr 2: alpha clear → transparent.
        vram[2] = 0x34;
        vram[3] = 0x12; // 0x1234, bit15 clear

        let mut out = fresh_line();
        // DISPCNT bit6 = OBJ bitmap 1D mapping.
        render_obj_scanline(0, 0x40, 0, &oam, 0, &vram, 0, &pram, 0x200, None, &mut out);
        assert_eq!(out[0] & 0x8000, 0, "x0 opaque");
        assert_eq!(out[0] & 0x7FFF, 0x1234);
        assert_eq!(out[1] & 0x8000, PX_TRANSPARENT, "x1 transparent (alpha clear)");
    }

    #[test]
    fn affine_identity_matches_regular() {
        // An affine sprite with the identity matrix should render the same as a
        // regular sprite of the same tile data.
        let mut oam = fresh_oam();
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        // Affine sprite (a0 bit8), 8x8, matrix index 0, at x=20, tile 0.
        set_oam(&mut oam, 0, 0x0100, 0x0014, 0);
        // Affine matrix 0: identity PA=PD=0x100, PB=PC=0. Stored in OAM column
        // bytes 6/7 of entries 0..3.
        let set_mat = |oam: &mut [u8], entry: usize, val: u16| {
            oam[entry * 8 + 6] = val as u8;
            oam[entry * 8 + 7] = (val >> 8) as u8;
        };
        set_mat(&mut oam, 0, 0x0100); // PA
        set_mat(&mut oam, 1, 0x0000); // PB
        set_mat(&mut oam, 2, 0x0000); // PC
        set_mat(&mut oam, 3, 0x0100); // PD

        // Tile 0 row 0 = index 3 across.
        for b in 0..4 {
            vram[b] = 0x33;
        }
        set_pram16(&mut pram, obj_pram_base / 2 + 3, 0x2BCD);

        let mut out = fresh_line();
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );
        for x in 20..28 {
            assert_eq!(out[x] & 0x8000, 0, "affine pixel {x} opaque");
            assert_eq!(out[x] & 0x7FFF, 0x2BCD, "affine pixel {x} color");
        }
    }

    #[test]
    fn disabled_and_prohibited_shape_are_skipped() {
        let mut oam = fresh_oam();
        let mut vram = vec![0u8; 0x20000];
        let pram = vec![0u8; 0x800];

        // Sprite 0: non-affine, disabled (bit9 set, bit8 clear).
        set_oam(&mut oam, 0, 0x0200, 0x0000, 0);
        // Sprite 1: prohibited shape 3 (a0 bits 14..15 = 0b11).
        set_oam(&mut oam, 1, 0xC000, 0x0000, 0);
        for b in 0..4 {
            vram[b] = 0x11;
        }

        let mut out = fresh_line();
        render_obj_scanline(0, 0, 0, &oam, 0, &vram, 0, &pram, 0x200, None, &mut out);
        for x in 0..16 {
            assert_eq!(out[x] & 0x8000, PX_TRANSPARENT, "nothing drawn at {x}");
        }
    }

    #[test]
    fn x_position_sign_extends() {
        // A sprite at x = 0x1F8 (-8) with an 8px width is fully offscreen left;
        // at x = 0x1FC (-4) half of it pokes onto the screen.
        let mut oam = vec![0u8; 0x800];
        let mut vram = vec![0u8; 0x20000];
        let mut pram = vec![0u8; 0x800];
        let obj_pram_base = 0x200usize;

        set_oam(&mut oam, 0, 0x0000, 0x01FC, 0); // x = -4
        for b in 0..4 {
            vram[b] = 0x22;
        }
        set_pram16(&mut pram, obj_pram_base / 2 + 2, 0x0F0F);

        let mut out = fresh_line();
        render_obj_scanline(
            0, 0, 0, &oam, 0, &vram, 0, &pram, obj_pram_base, None, &mut out,
        );
        // Columns 4..8 of the sprite (px>=4) land on screen at x=0..4.
        for x in 0..4 {
            assert_eq!(out[x] & 0x8000, 0, "pixel {x} opaque");
        }
    }
}
