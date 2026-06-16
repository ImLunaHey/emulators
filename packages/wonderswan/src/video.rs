//! WonderSwan video: two scrolling tile-map layers (SCR1 background, SCR2
//! foreground) + a sprite layer, rendered to a 224×144 RGBA8888 framebuffer.
//! Built from the WonderSwan dev wiki ("Display", "Video memory", "I/O ports").
//!
//! VRAM layout (lives inside the system RAM; the video unit reads it through the
//! god-struct on the orchestrator's behalf, so this module is given a `&[u8]`
//! VRAM view each scanline):
//!   * Tiles: 2bpp (mono) or 4bpp (color) 8×8 patterns. Mono tile base is fixed;
//!     color tiles are 4bpp and can live in the larger color VRAM.
//!   * Tilemap: 32×32 entries, each a 16-bit word: tile index (low 9 bits),
//!     palette (bits 9-12), flip-x (bit14)/flip-y (bit15).
//!   * Sprite table: 128 entries of 4 bytes (tile/attr, palette/flip, y, x).
//!   * Palettes: mono uses 8 palettes × 4 shade-indices into a 16-step grey
//!     pool; color uses 16 palettes × 16 colours of 12-bit RGB stored in the
//!     color palette RAM region.
//!
//! Timing: 159 scanlines total (144 visible + vblank). A line counter, a
//! line-compare register, and the vblank boundary raise the corresponding
//! interrupt bits, which the god-struct feeds to the interrupt controller.

pub const SCREEN_W: usize = 224;
pub const SCREEN_H: usize = 144;
pub const TOTAL_LINES: u16 = 159;
pub const FB_LEN: usize = SCREEN_W * SCREEN_H * 4;

/// Display control register bits (I/O $00, DISP_CTRL).
const DISP_SCR1_ENABLE: u8 = 0x01;
const DISP_SCR2_ENABLE: u8 = 0x02;
const DISP_SPR_ENABLE: u8 = 0x04;

pub struct Video {
    pub color: bool,

    /// RGBA8888 framebuffer (224×144).
    pub framebuffer: Box<[u8; FB_LEN]>,

    /// Current scanline (0..TOTAL_LINES).
    pub line: u16,
    /// Frame counter.
    pub frame: u64,

    // ---- display registers (I/O $00-$17) ----
    pub disp_ctrl: u8,    // $00
    pub back_color: u8,   // $01
    pub scr1_x: u8,       // $10 scroll
    pub scr1_y: u8,       // $11
    pub scr2_x: u8,       // $12
    pub scr2_y: u8,       // $13
    pub map_base: u8,     // $07: SCR1 base (low nibble) / SCR2 base (high nibble)
    pub spr_base: u8,     // $04 sprite table base (in 512-byte units)
    pub spr_first: u8,    // $05 first sprite
    pub spr_count: u8,    // $06 sprite count
    pub line_compare: u8, // $03 LCMP

    /// Mono grey shade pool: 8 entries, each a 4-bit shade (0=light..15=dark).
    pub shade_lut: [u8; 8],
    /// Mono palettes: $20-$3F, 16 bytes = 8 palettes × 2 bytes (4 nibble indices
    /// into `shade_lut`).
    pub mono_palettes: [u8; 32],

    /// Interrupt status latched this step: bit0 line-compare, bit1 vblank.
    pub irq_line_match: bool,
    pub irq_vblank: bool,
}

impl Default for Video {
    fn default() -> Self {
        Video::new(false)
    }
}

impl Video {
    pub fn new(color: bool) -> Video {
        Video {
            color,
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            line: 0,
            frame: 0,
            disp_ctrl: 0,
            back_color: 0,
            scr1_x: 0,
            scr1_y: 0,
            scr2_x: 0,
            scr2_y: 0,
            map_base: 0,
            spr_base: 0,
            spr_first: 0,
            spr_count: 0,
            line_compare: 0xFF,
            shade_lut: [0, 2, 4, 6, 9, 11, 13, 15],
            mono_palettes: [0; 32],
            irq_line_match: false,
            irq_vblank: false,
        }
    }

    /// Map a 4-bit grey shade (0=lightest..15=darkest) to an RGBA pixel.
    fn grey_rgba(shade: u8) -> [u8; 4] {
        let s = shade & 0x0F;
        // 0 = white, 15 = black on the LCD; invert so darker shade -> darker px.
        let v = 255 - (s as u16 * 255 / 15) as u8;
        [v, v, v, 0xFF]
    }

    /// Decode a 12-bit RGB color word (0x0RGB) to RGBA, expanding 4-bit channels.
    fn color_rgba(word: u16) -> [u8; 4] {
        let r = ((word >> 8) & 0xF) as u8;
        let g = ((word >> 4) & 0xF) as u8;
        let b = (word & 0xF) as u8;
        [r * 17, g * 17, b * 17, 0xFF]
    }

    /// Advance one scanline: render the visible line, update counters, set the
    /// line-compare and vblank interrupt flags. `vram` is the full system RAM
    /// slice (VRAM occupies its low portion); `palette_ram` is the color palette
    /// region (color model only).
    pub fn step_scanline(&mut self, vram: &[u8], palette_ram: &[u8]) {
        self.irq_line_match = false;
        self.irq_vblank = false;

        let cur = self.line;
        if cur < SCREEN_H as u16 {
            self.render_line(cur as usize, vram, palette_ram);
        }

        // Line-compare interrupt.
        if cur as u8 == self.line_compare {
            self.irq_line_match = true;
        }

        self.line += 1;
        if self.line == SCREEN_H as u16 {
            self.irq_vblank = true;
        }
        if self.line >= TOTAL_LINES {
            self.line = 0;
            self.frame += 1;
        }
    }

    /// Fetch a tilemap entry (16-bit word) from `vram` at the given map base and
    /// tile coordinate (wrapped to the 32×32 map).
    #[inline]
    fn map_entry(vram: &[u8], base: usize, tx: usize, ty: usize) -> u16 {
        let idx = base + ((ty & 31) * 32 + (tx & 31)) * 2;
        let lo = vram.get(idx).copied().unwrap_or(0) as u16;
        let hi = vram.get(idx + 1).copied().unwrap_or(0) as u16;
        (hi << 8) | lo
    }

    /// Read a pixel's color index (0..15 for color, 0..3 for mono 2bpp) from a
    /// tile. `tile` is the tile number, `px`/`py` the in-tile pixel (0..7).
    #[inline]
    fn tile_pixel(&self, vram: &[u8], tile: usize, px: usize, py: usize) -> u8 {
        if self.color {
            // 4bpp packed: 32 bytes/tile, 4 bits/pixel, 2 pixels per byte.
            let base = tile * 32 + py * 4 + (px / 2);
            let byte = vram.get(base).copied().unwrap_or(0);
            if px & 1 == 0 {
                byte >> 4
            } else {
                byte & 0x0F
            }
        } else {
            // 2bpp planar: 16 bytes/tile, 2 planes interleaved per row.
            let base = tile * 16 + py * 2;
            let p0 = vram.get(base).copied().unwrap_or(0);
            let p1 = vram.get(base + 1).copied().unwrap_or(0);
            let bit = 7 - px;
            ((p0 >> bit) & 1) | (((p1 >> bit) & 1) << 1)
        }
    }

    /// Resolve a tile pixel index + palette to an RGBA color.
    fn resolve_color(&self, palette: u8, index: u8, palette_ram: &[u8]) -> [u8; 4] {
        if self.color {
            // 16 palettes × 16 colors × 2 bytes (12-bit RGB).
            let off = (palette as usize * 16 + index as usize) * 2;
            let lo = palette_ram.get(off).copied().unwrap_or(0) as u16;
            let hi = palette_ram.get(off + 1).copied().unwrap_or(0) as u16;
            Self::color_rgba((hi << 8) | lo)
        } else {
            // Mono: palette*2 bytes hold 4 nibble shade-pool indices.
            let pbyte = self.mono_palettes[(palette as usize * 2 + (index as usize / 2)) & 31];
            let shade_idx = if index & 1 == 0 {
                pbyte & 0x0F
            } else {
                pbyte >> 4
            };
            Self::grey_rgba(self.shade_lut[(shade_idx & 7) as usize])
        }
    }

    fn render_line(&mut self, y: usize, vram: &[u8], palette_ram: &[u8]) {
        // Background fill.
        let bg = if self.color {
            let off = (self.back_color as usize) * 2;
            let lo = palette_ram.get(off).copied().unwrap_or(0) as u16;
            let hi = palette_ram.get(off + 1).copied().unwrap_or(0) as u16;
            Self::color_rgba((hi << 8) | lo)
        } else {
            Self::grey_rgba(self.shade_lut[(self.back_color & 7) as usize])
        };

        let scr1_base = ((self.map_base & 0x07) as usize) * 0x800;
        let scr2_base = (((self.map_base >> 4) & 0x07) as usize) * 0x800;

        let scr1_on = self.disp_ctrl & DISP_SCR1_ENABLE != 0;
        let scr2_on = self.disp_ctrl & DISP_SCR2_ENABLE != 0;
        let spr_on = self.disp_ctrl & DISP_SPR_ENABLE != 0;

        // Scanline row buffer of (rgba, opaque) for layering.
        let mut row = [[0u8; 4]; SCREEN_W];
        for px in row.iter_mut() {
            *px = bg;
        }

        // SCR1 (background) then SCR2 (foreground) painted opaquely except index 0.
        for x in 0..SCREEN_W {
            if scr1_on {
                if let Some(c) = self.fetch_map_pixel(
                    vram,
                    palette_ram,
                    scr1_base,
                    x,
                    y,
                    self.scr1_x,
                    self.scr1_y,
                ) {
                    row[x] = c;
                }
            }
            if scr2_on {
                if let Some(c) = self.fetch_map_pixel(
                    vram,
                    palette_ram,
                    scr2_base,
                    x,
                    y,
                    self.scr2_x,
                    self.scr2_y,
                ) {
                    row[x] = c;
                }
            }
        }

        // Sprites.
        if spr_on {
            self.render_sprites_line(&mut row, y, vram, palette_ram);
        }

        // Blit the row.
        let dst = y * SCREEN_W * 4;
        for (x, px) in row.iter().enumerate() {
            let o = dst + x * 4;
            self.framebuffer[o..o + 4].copy_from_slice(px);
        }
    }

    /// Resolve one tilemap layer pixel at screen (x,y) with scroll. Returns the
    /// color if the pixel is non-transparent (index != 0), else None.
    #[allow(clippy::too_many_arguments)]
    fn fetch_map_pixel(
        &self,
        vram: &[u8],
        palette_ram: &[u8],
        base: usize,
        x: usize,
        y: usize,
        scroll_x: u8,
        scroll_y: u8,
    ) -> Option<[u8; 4]> {
        let mx = (x + scroll_x as usize) & 0xFF; // 256-px wrap (32 tiles × 8)
        let my = (y + scroll_y as usize) & 0xFF;
        let tx = mx / 8;
        let ty = my / 8;
        let entry = Self::map_entry(vram, base, tx, ty);
        let tile = (entry & 0x01FF) as usize;
        let palette = ((entry >> 9) & 0x0F) as u8;
        let flip_x = entry & 0x4000 != 0;
        let flip_y = entry & 0x8000 != 0;
        let mut px = mx & 7;
        let mut py = my & 7;
        if flip_x {
            px = 7 - px;
        }
        if flip_y {
            py = 7 - py;
        }
        let idx = self.tile_pixel(vram, tile, px, py);
        if idx == 0 {
            return None; // transparent
        }
        Some(self.resolve_color(palette, idx, palette_ram))
    }

    fn render_sprites_line(
        &self,
        row: &mut [[u8; 4]; SCREEN_W],
        y: usize,
        vram: &[u8],
        palette_ram: &[u8],
    ) {
        // Sprite table base is in 512-byte units, masked to VRAM.
        let table = (self.spr_base as usize) * 0x200;
        let first = self.spr_first as usize;
        let count = (self.spr_count as usize).min(128);
        for i in 0..count {
            let s = (first + i) & 0x7F;
            let off = table + s * 4;
            let b0 = vram.get(off).copied().unwrap_or(0); // tile low
            let b1 = vram.get(off + 1).copied().unwrap_or(0); // attr
            let sy = vram.get(off + 2).copied().unwrap_or(0) as usize;
            let sx = vram.get(off + 3).copied().unwrap_or(0) as usize;
            let tile = (b0 as usize) | (((b1 & 0x01) as usize) << 8);
            let palette = (b1 >> 1) & 0x07; // sprite palettes 8..15 region
            let flip_x = b1 & 0x40 != 0;
            let flip_y = b1 & 0x80 != 0;
            // Vertical span check.
            if y < sy || y >= sy + 8 {
                continue;
            }
            let mut py = y - sy;
            if flip_y {
                py = 7 - py;
            }
            for col in 0..8 {
                let xx = sx + col;
                if xx >= SCREEN_W {
                    continue;
                }
                let mut px = col;
                if flip_x {
                    px = 7 - px;
                }
                let idx = self.tile_pixel(vram, tile, px, py);
                if idx == 0 {
                    continue;
                }
                // Sprites use the upper palette bank (8..15) on color.
                let pal = if self.color { palette + 8 } else { palette };
                row[xx] = self.resolve_color(pal, idx, palette_ram);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_endpoints() {
        // Shade 0 -> white, shade 15 -> black.
        assert_eq!(Video::grey_rgba(0), [255, 255, 255, 255]);
        assert_eq!(Video::grey_rgba(15), [0, 0, 0, 255]);
    }

    #[test]
    fn color_word_expands() {
        // 0x0F00 = full red.
        assert_eq!(Video::color_rgba(0x0F00), [255, 0, 0, 255]);
        assert_eq!(Video::color_rgba(0x000F), [0, 0, 255, 255]);
    }

    #[test]
    fn scanline_counter_and_vblank() {
        let mut v = Video::new(false);
        let vram = vec![0u8; 0x4000];
        let pal = vec![0u8; 0x200];
        // Step to the vblank boundary (line 144).
        for _ in 0..SCREEN_H {
            v.step_scanline(&vram, &pal);
        }
        assert!(v.irq_vblank, "vblank flag set when crossing line 144");
        assert_eq!(v.line, SCREEN_H as u16);
    }

    #[test]
    fn line_compare_interrupt() {
        let mut v = Video::new(false);
        v.line_compare = 10;
        let vram = vec![0u8; 0x4000];
        let pal = vec![0u8; 0x200];
        let mut hit = false;
        for _ in 0..20 {
            v.step_scanline(&vram, &pal);
            if v.irq_line_match {
                hit = true;
            }
        }
        assert!(hit, "line-compare interrupt fires at the target line");
    }

    #[test]
    fn frame_wraps_after_total_lines() {
        let mut v = Video::new(false);
        let vram = vec![0u8; 0x4000];
        let pal = vec![0u8; 0x200];
        let f0 = v.frame;
        for _ in 0..TOTAL_LINES {
            v.step_scanline(&vram, &pal);
        }
        assert_eq!(v.frame, f0 + 1);
        assert_eq!(v.line, 0);
    }

    #[test]
    fn renders_a_color_tile() {
        // Color model: enable SCR1, put a tile at map (0,0) using palette 1
        // color 1, set palette[1][1] to red. Expect top-left pixel red.
        let mut v = Video::new(true);
        v.disp_ctrl = DISP_SCR1_ENABLE;
        v.map_base = 0x00; // scr1 base 0
        let mut vram = vec![0u8; 0x10000];
        // Map entry at (0,0): tile=1, palette=1.
        let entry: u16 = 1 | (1 << 9);
        vram[0] = (entry & 0xFF) as u8;
        vram[1] = (entry >> 8) as u8;
        // Tile 1, 4bpp packed: 32 bytes at offset 32. Set pixel (0,0) to index 1.
        vram[32] = 0x10; // high nibble = pixel0 = 1
        // Palette RAM: palette 1, color 1 = red (0x0F00).
        let mut pal = vec![0u8; 0x200];
        let off = (1 * 16 + 1) * 2;
        pal[off] = 0x00;
        pal[off + 1] = 0x0F;
        v.render_line(0, &vram, &pal);
        assert_eq!(&v.framebuffer[0..4], &[255, 0, 0, 255]);
    }
}
