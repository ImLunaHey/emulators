//! Large-bitmap BG scanline renderer (DS Engine A DISPCNT BG mode 6, BG2 only).
//! Ported/adapted from ../../ds-recomp/src/ppu/bitmap_bg.ts and the GBA core's
//! bitmap samplers (../../core/src/ppu.rs `sample_mode3`).
//!
//! Mode 6 is the DS "large screen bitmap": BG2 is a single direct-color
//! (BGR555) framebuffer of 512x512 dots, sampled with the BG2 scroll offset.
//! Engine A only — Engine B never selects mode 6 (the `bg_slot_kind` dispatch
//! in the compositor returns `Off` for B).
//!
//! Ownership (CONTRACT.md): NO `&mut Nds`. Takes the BG control/scroll + the
//! borrowed VRAM slice + resolved base, writes one scanline of packed pixels.
//!
//! ── DS extended-bitmap note ──────────────────────────────────────────────
//! The OTHER DS bitmap variants — the BG2/BG3 "extended" slots in DISPCNT
//! modes 3..5 (BGxCNT bit 7 = 1: 256-color bitmap or 16-bit direct-color
//! bitmap, sampled through the BG affine matrix) — are NOT rendered here.
//! Those are extended-affine layers and live in `affine_bg.rs`
//! (`render_affine_bg_scanline` with `force_tile = false`), matching the TS
//! `bgSlotKind` dispatch where only `kind === 'large-bitmap'` reaches this
//! function. This file is solely the mode-6 large bitmap.

use super::engine_a::{LINE_W, PX_TRANSPARENT};

/// Read a little-endian u16 from `vram` at byte offset `off`, or 0 if it would
/// read past the end of the slice (defensive — the resolved bank window may be
/// smaller than the bitmap's nominal extent).
#[inline]
fn rd16(vram: &[u8], off: usize) -> u32 {
    if off + 1 < vram.len() {
        (vram[off] as u32) | ((vram[off + 1] as u32) << 8)
    } else {
        0
    }
}

/// Render one large-bitmap (mode 6) BG2 scanline at screen row `y` into `out`.
///
/// The DS mode-6 large bitmap is a fixed 512x512 direct-color (BGR555)
/// framebuffer. `bg_cnt` supplies the priority (bits 0..1); `hofs`/`vofs` are
/// the BG2 scroll offsets (9-bit each) that position the visible 256x192
/// window inside the 512x512 bitmap. `vram` + `bg_vram_base` locate the
/// framebuffer's first byte in the resolved BG-VRAM window.
///
/// Each written pixel is packed (see engine_a.rs PX format): bits 0..14 BGR555,
/// bit 15 transparent, bits 16..17 = layer (2 = BG2), bits 18..19 = priority.
/// Bit 15 of the source halfword is the per-pixel alpha/"drawn" marker on the
/// DS (a fully-zero halfword reads transparent), so a sample with that bit
/// clear is emitted as transparent.
pub fn render_bitmap_scanline(
    bg_cnt: u32,
    hofs: u32,
    vofs: u32,
    y: u32,
    vram: &[u8],
    bg_vram_base: usize,
    out: &mut [u32],
) {
    // Large-bitmap dimensions are fixed at 512x512 on the DS (GBATEK "DS Video
    // BG Modes" — mode 6 BG2). Unlike the extended bitmap slots, BGxCNT bits
    // 14..15 do NOT select the size here.
    const W: u32 = 512;
    const H: u32 = 512;

    let priority = bg_cnt & 0x3;
    // Packed layer/priority high bits: BG2 → layer 2.
    let layer_hi = (2u32 << 16) | (priority << 18);

    let bitmap_y = (y.wrapping_add(vofs)) % H;
    let row_start = bg_vram_base + (bitmap_y as usize) * (W as usize) * 2;

    let n = out.len().min(LINE_W);
    for (x, slot) in out.iter_mut().take(n).enumerate() {
        let bx = ((x as u32).wrapping_add(hofs)) % W;
        let off = row_start + (bx as usize) * 2;
        let c = rd16(vram, off);
        // DS direct-color bitmap: bit 15 is the per-pixel alpha bit. A pixel
        // with it clear (including an all-zero halfword) is transparent.
        if (c & 0x8000) != 0 {
            *slot = (c & 0x7FFF) | layer_hi;
        } else {
            *slot = PX_TRANSPARENT;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a VRAM buffer large enough for a 512x512x2 bitmap with `base`
    /// padding in front, pre-filled with the PX_TRANSPARENT marker pattern
    /// (all-zero halfwords → transparent).
    fn make_vram(base: usize) -> Vec<u8> {
        vec![0u8; base + 512 * 512 * 2 + 4]
    }

    /// Write a BGR555 halfword (with the drawn bit explicit) into `vram` at
    /// the given (x,y) of a 512-wide direct-color bitmap.
    fn put_px(vram: &mut [u8], base: usize, x: u32, y: u32, c: u16) {
        let off = base + (y as usize * 512 + x as usize) * 2;
        vram[off] = (c & 0xFF) as u8;
        vram[off + 1] = (c >> 8) as u8;
    }

    #[test]
    fn direct_color_passthrough_with_layer_and_priority() {
        let base = 0x10000;
        let mut vram = make_vram(base);
        // bg_cnt priority = 2.
        let bg_cnt = 0x0002;
        // Put a red-ish drawn pixel at (0,0): BGR555 0x001F (red) + drawn bit.
        put_px(&mut vram, base, 0, 0, 0x001F | 0x8000);
        // And a distinct pixel at (5,0).
        put_px(&mut vram, base, 5, 0, 0x7C00 | 0x8000); // blue

        let mut out = [PX_TRANSPARENT; LINE_W];
        render_bitmap_scanline(bg_cnt, 0, 0, 0, &vram, base, &mut out);

        // Pixel 0: color 0x001F, layer 2, priority 2.
        assert_eq!(out[0] & 0x7FFF, 0x001F);
        assert_eq!((out[0] >> 16) & 0x3, 2, "layer must be BG2");
        assert_eq!((out[0] >> 18) & 0x3, 2, "priority must be 2");
        assert_eq!(out[0] & PX_TRANSPARENT, 0, "drawn pixel not transparent");

        // Pixel 5: blue.
        assert_eq!(out[5] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn undrawn_pixel_is_transparent() {
        let base = 0;
        let mut vram = make_vram(base);
        // A non-zero color but WITHOUT the drawn (bit 15) marker reads
        // transparent on the DS direct-color bitmap.
        put_px(&mut vram, base, 3, 0, 0x1234); // bit15 clear
        let mut out = [0u32; LINE_W];
        render_bitmap_scanline(0, 0, 0, 0, &vram, base, &mut out);
        assert_eq!(out[3], PX_TRANSPARENT);
        // And the all-zero default cells stay transparent too.
        assert_eq!(out[0], PX_TRANSPARENT);
    }

    #[test]
    fn vofs_selects_bitmap_row() {
        let base = 0;
        let mut vram = make_vram(base);
        // Mark row 10 column 0.
        put_px(&mut vram, base, 0, 10, 0x03E0 | 0x8000); // green
        let mut out = [PX_TRANSPARENT; LINE_W];
        // y=0 with vofs=10 should sample bitmap row 10.
        render_bitmap_scanline(0, 0, 10, 0, &vram, base, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x03E0);
    }

    #[test]
    fn hofs_scrolls_horizontally_and_wraps() {
        let base = 0;
        let mut vram = make_vram(base);
        // Mark column 511 row 0 so hofs=511 brings it to screen x=1's wrap, and
        // hofs=512 (==0 mod 512) is identity. Test the simple offset first.
        put_px(&mut vram, base, 100, 0, 0x7FFF | 0x8000); // white
        let mut out = [PX_TRANSPARENT; LINE_W];
        // hofs = 100 → screen x=0 samples bitmap x=100.
        render_bitmap_scanline(0, 100, 0, 0, &vram, base, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x7FFF);

        // Horizontal wrap: bitmap x=0 is drawn, hofs chosen so a screen x near
        // the right edge wraps back to bitmap x=0.
        let mut vram2 = make_vram(base);
        put_px(&mut vram2, base, 0, 0, 0x1F | 0x8000);
        let mut out2 = [PX_TRANSPARENT; LINE_W];
        // screen x where (x + hofs) % 512 == 0: pick hofs = 512 - 10 = 502,
        // then screen x = 10 → (10 + 502) % 512 == 0.
        render_bitmap_scanline(0, 502, 0, 0, &vram2, base, &mut out2);
        assert_eq!(out2[10] & 0x7FFF, 0x1F, "horizontal wrap to bitmap x=0");
    }

    #[test]
    fn base_offset_is_honored() {
        // A non-zero bg_vram_base must shift where the bitmap is read from.
        let base = 0x4000;
        let mut vram = make_vram(base);
        put_px(&mut vram, base, 0, 0, 0x2A2A | 0x8000);
        let mut out = [PX_TRANSPARENT; LINE_W];
        render_bitmap_scanline(0, 0, 0, 0, &vram, base, &mut out);
        assert_eq!(out[0] & 0x7FFF, 0x2A2A & 0x7FFF);
    }

    #[test]
    fn out_of_range_read_is_transparent_not_panic() {
        // A base near the end of a too-small VRAM slice must not panic; reads
        // past the end yield 0 → transparent.
        let small = vec![0u8; 16];
        let mut out = [0xDEADu32; LINE_W];
        render_bitmap_scanline(0, 0, 0, 0, &small, 0, &mut out);
        // First few cells read real zeros (transparent), later ones read OOB
        // zeros (transparent) — all transparent.
        assert!(out.iter().all(|&p| p == PX_TRANSPARENT));
    }
}
