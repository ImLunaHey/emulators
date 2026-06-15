//! K1GE (mono) / K2GE (colour) video controller. Built from the ngpcspec
//! hardware doc and the NeoPop memory-map notes.
//!
//! Display: 160×152 visible; a 256×256 virtual tile map. Two scroll planes +
//! 64 sprites. Tiles are 8×8, 2 bits-per-pixel (4 colours each), 16 bytes/tile.
//!
//! Memory (CPU addresses; this module owns the 0x8000-0xBFFF window):
//!   0x8000-0x80FF  control registers (scroll, interrupt-enable, raster, …)
//!   0x8100-0x817F  mono shade LUTs (K1GE)
//!   0x8200-0x83FF  palette RAM (16-bit entries, 12-bit RGB)
//!   0x8800-0x88FF  sprite (OAM) attribute table, 64 × 4 bytes
//!   0x8C00-0x8C3F  per-sprite palette select (K2GE)
//!   0x9000-0x97FF  scroll-plane-1 tilemap (32×32 × 2-byte entries)
//!   0x9800-0x9FFF  scroll-plane-2 tilemap
//!   0xA000-0xBFFF  pattern / character RAM (tiles)
//!
//! Key registers:
//!   0x8000 ICR : bit6 enable H-blank(line) IRQ, bit7 enable V-blank IRQ
//!   0x8009 RAS.V : current scanline (read), also line-compare
//!   0x8012 : bit7 NEG (invert RGB), bits0-2 outside-window colour
//!   0x8020/0x8021 sprite scroll X/Y
//!   0x8030 bit7 plane priority (0: plane1 front)
//!   0x8032/0x8033 plane1 scroll X/Y   0x8034/0x8035 plane2 scroll X/Y
//!   0x8118 background-colour register (bits6-7 on, bits0-2 colour index)
//!   0x87E0 2D soft reset (write 0x52)   0x87E2 bit7: mode (0 K2GE colour)

pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 152;
pub const FB_LEN: usize = WIDTH * HEIGHT * 4;
/// Visible lines + V-blank ≈ 199 total scanlines.
pub const TOTAL_LINES: u32 = 199;

pub struct Video {
    /// The whole 0x8000-0xBFFF window as one boxed region (offset = addr-0x8000).
    pub vram: Box<[u8; 0x4000]>,

    /// RGBA8888 framebuffer.
    pub framebuffer: Box<[u8; FB_LEN]>,

    /// Current scanline (0..TOTAL_LINES).
    pub line: u32,
    /// Completed-frame counter.
    pub frame: u64,

    /// True if the game cart marked colour mode (K2GE).
    pub color: bool,

    /// Interrupt request latches consumed by the bus owner.
    pub vblank_irq: bool,
    pub hblank_irq: bool,
}

impl Video {
    pub fn new(color: bool) -> Video {
        Video {
            vram: vec![0u8; 0x4000].into_boxed_slice().try_into().unwrap(),
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            line: 0,
            frame: 0,
            color,
            vblank_irq: false,
            hblank_irq: false,
        }
    }

    #[inline]
    fn reg(&self, addr: u32) -> u8 {
        self.vram[(addr - 0x8000) as usize]
    }

    pub fn read(&self, addr: u32) -> u8 {
        match addr {
            0x8008 => 0, // RAS.H (we report 0)
            0x8009 => self.line.min(255) as u8, // RAS.V current line
            0x8010 => {
                // 2D status: bit6 = in V-blank.
                if self.line >= HEIGHT as u32 {
                    0x40
                } else {
                    0x00
                }
            }
            0x8000..=0xBFFF => self.vram[(addr - 0x8000) as usize],
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u32, v: u8) {
        if (0x8000..=0xBFFF).contains(&addr) {
            self.vram[(addr - 0x8000) as usize] = v;
        }
    }

    /// Advance one scanline. Renders the visible line, raises H/V-blank IRQ
    /// latches per the ICR, and wraps the frame.
    pub fn step_line(&mut self) {
        if self.line < HEIGHT as u32 {
            self.render_line(self.line as usize);
            // Line(H-blank) interrupt if enabled.
            if self.reg(0x8000) & 0x40 != 0 {
                self.hblank_irq = true;
            }
        }
        self.line += 1;
        if self.line == HEIGHT as u32 {
            // Entering V-blank.
            if self.reg(0x8000) & 0x80 != 0 {
                self.vblank_irq = true;
            }
        }
        if self.line >= TOTAL_LINES {
            self.line = 0;
            self.frame += 1;
        }
    }

    /// Convert a 12-bit RGB palette entry (4 bits/channel) to RGBA8888,
    /// honouring the NEG (invert) bit of register 0x8012.
    fn palette_rgba(&self, entry: u16) -> [u8; 4] {
        let mut r = (entry & 0x0F) as u8;
        let mut g = ((entry >> 4) & 0x0F) as u8;
        let mut b = ((entry >> 8) & 0x0F) as u8;
        if self.reg(0x8012) & 0x80 != 0 {
            r = 0x0F - r;
            g = 0x0F - g;
            b = 0x0F - b;
        }
        // Scale 4-bit -> 8-bit (x * 17).
        [r * 17, g * 17, b * 17, 0xFF]
    }

    /// Read a 16-bit palette-RAM entry at table base `base` for palette `pal`
    /// (4 colours each), colour index `idx` (0..4).
    fn palette_color(&self, base: u32, pal: usize, idx: usize) -> [u8; 4] {
        let off = base + ((pal * 4 + idx) as u32) * 2;
        let lo = self.read(off) as u16;
        let hi = self.read(off + 1) as u16;
        self.palette_rgba((hi << 8) | lo)
    }

    /// Fetch one pixel (0..3) from tile `tile` at (tx,ty) in pattern RAM.
    fn tile_pixel(&self, tile: usize, tx: usize, ty: usize) -> u8 {
        // 16 bytes/tile, 2 bytes/row, 2bpp, low bits = rightmost pixel.
        let base = 0xA000 + (tile * 16) as u32 + (ty * 2) as u32;
        let lo = self.read(base);
        let hi = self.read(base + 1);
        // 2bpp packed: 16 bits per 8-pixel row; pixel column tx (0=leftmost)
        // occupies bits at (7-tx)*2, so the rightmost pixel (tx=7) is the low
        // two bits, per the ngpcspec "rightmost pixel in low bits".
        let w = ((hi as u16) << 8) | lo as u16;
        let shift = (7 - (tx & 7)) * 2;
        ((w >> shift) & 0x03) as u8
    }

    /// Render one visible scanline into the framebuffer.
    fn render_line(&mut self, y: usize) {
        // Background colour (register 0x8118 / 0x8012 outside-window default).
        let bg = self.bg_color();
        for x in 0..WIDTH {
            let off = (y * WIDTH + x) * 4;
            self.framebuffer[off..off + 4].copy_from_slice(&bg);
        }
        // Plane priority: bit7 of 0x8030 — 0 means plane1 is in front. Draw the
        // back plane first, then the front plane over it.
        let plane1_front = self.reg(0x8030) & 0x80 == 0;
        let p1 = (0x9000u32, 0x8280u32, self.reg(0x8033), self.reg(0x8032));
        let p2 = (0x9800u32, 0x8300u32, self.reg(0x8035), self.reg(0x8034));
        let (back, front) = if plane1_front { (p2, p1) } else { (p1, p2) };
        self.render_plane(y, back.0, back.1, back.2, back.3);
        self.render_plane(y, front.0, front.1, front.2, front.3);
        self.render_sprites(y);
    }

    fn bg_color(&self) -> [u8; 4] {
        // K2GE background: register 0x8118, bits6-7 enable, bits0-2 index into
        // the background palette at 0x83E0.
        let r = self.reg(0x8118);
        if r & 0xC0 != 0 {
            let idx = (r & 0x07) as usize;
            let off = 0x83E0 + (idx as u32) * 2;
            let lo = self.read(off) as u16;
            let hi = self.read(off + 1) as u16;
            self.palette_rgba((hi << 8) | lo)
        } else {
            [0, 0, 0, 0xFF]
        }
    }

    /// Render one scroll plane onto line `y`. `map` = tilemap base,
    /// `palbase` = palette-table base, `scy`/`scx` = scroll offsets.
    fn render_plane(&mut self, y: usize, map: u32, palbase: u32, scy: u8, scx: u8) {
        let sy = (y + scy as usize) & 0xFF;
        let trow = sy / 8;
        let ty = sy & 7;
        for x in 0..WIDTH {
            let sx = (x + scx as usize) & 0xFF;
            let tcol = sx / 8;
            // 32 tiles per row, 2 bytes per entry.
            let ent = map + ((trow * 32 + tcol) * 2) as u32;
            let b0 = self.read(ent);
            let b1 = self.read(ent + 1);
            let tile = (b0 as usize) | (((b1 & 0x01) as usize) << 8);
            let pal = ((b1 >> 1) & 0x0F) as usize;
            let hflip = b1 & 0x80 != 0;
            let vflip = b1 & 0x40 != 0;
            let mut px = sx & 7;
            let mut py = ty;
            if hflip {
                px = 7 - px;
            }
            if vflip {
                py = 7 - py;
            }
            let ci = self.tile_pixel(tile, px, py);
            if ci == 0 {
                continue; // colour 0 = transparent
            }
            let color = self.palette_color(palbase, pal, ci as usize);
            let off = (y * WIDTH + x) * 4;
            self.framebuffer[off..off + 4].copy_from_slice(&color);
        }
    }

    /// Render the sprite layer onto line `y`. 64 sprites at 0x8800, 4 bytes
    /// each. Sprites are 8×8. Honours H/V chain (inherit X/Y from previous).
    fn render_sprites(&mut self, y: usize) {
        let spx = self.reg(0x8020) as usize;
        let spy = self.reg(0x8021) as usize;
        let mut prev_x = 0usize;
        let mut prev_y = 0usize;
        for s in 0..64 {
            let base = 0x8800 + (s * 4) as u32;
            let b0 = self.read(base);
            let b1 = self.read(base + 1);
            let mut sx = self.read(base + 2) as usize;
            let mut sy = self.read(base + 3) as usize;
            // H/V chain: inherit position from the previous sprite.
            if b1 & 0x04 != 0 {
                sx = prev_x;
            }
            if b1 & 0x02 != 0 {
                sy = prev_y;
            }
            prev_x = sx;
            prev_y = sy;
            let prio = (b1 >> 3) & 0x03;
            if prio == 0 {
                continue; // hidden
            }
            let tile = (b0 as usize) | (((b1 & 0x01) as usize) << 8);
            let hflip = b1 & 0x80 != 0;
            let vflip = b1 & 0x40 != 0;
            // Apply the sprite-plane scroll offset.
            let py_screen = (sy + spy) & 0xFF;
            if y < py_screen || y >= py_screen + 8 {
                continue;
            }
            let mut ty = y - py_screen;
            if vflip {
                ty = 7 - ty;
            }
            // Per-sprite palette select (K2GE) at 0x8C00.
            let pal = (self.read(0x8C00 + s as u32) & 0x0F) as usize;
            for col in 0..8 {
                let x_screen = (sx + spx + col) & 0x1FF;
                if x_screen >= WIDTH {
                    continue;
                }
                let mut tx = col;
                if hflip {
                    tx = 7 - tx;
                }
                let ci = self.tile_pixel(tile, tx, ty);
                if ci == 0 {
                    continue;
                }
                let color = self.palette_color(0x8200, pal, ci as usize);
                let off = (y * WIDTH + x_screen) * 4;
                self.framebuffer[off..off + 4].copy_from_slice(&color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_write_read() {
        let mut v = Video::new(true);
        v.write(0x9000, 0xAB);
        assert_eq!(v.read(0x9000), 0xAB);
    }

    #[test]
    fn palette_12bit_to_rgba() {
        let v = Video::new(true);
        // entry = 0x0F0F (R=15,G=0,B=15) -> magenta.
        let c = v.palette_rgba(0x0F0F);
        assert_eq!(c, [0xFF, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn neg_inverts() {
        let mut v = Video::new(true);
        v.write(0x8012, 0x80); // NEG
        let c = v.palette_rgba(0x0000); // black -> white when inverted
        assert_eq!(c, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn vblank_irq_raised_when_enabled() {
        let mut v = Video::new(true);
        v.write(0x8000, 0x80); // enable V-blank IRQ
        for _ in 0..HEIGHT {
            v.step_line();
        }
        assert!(v.vblank_irq);
    }

    #[test]
    fn frame_wraps_after_total_lines() {
        let mut v = Video::new(true);
        let f0 = v.frame;
        for _ in 0..TOTAL_LINES {
            v.step_line();
        }
        assert_eq!(v.frame, f0 + 1);
        assert_eq!(v.line, 0);
    }

    #[test]
    fn tile_pixel_decodes_2bpp() {
        let mut v = Video::new(true);
        // tile 0, row 0. Combined 16-bit word w = (hi<<8)|lo. Pixel column tx
        // (0=leftmost) reads bits (7-tx)*2. Put a 3 at the rightmost pixel
        // (tx=7 -> shift 0) and a 1 at tx=6 (shift 2).
        v.write(0xA000, 0b0000_0111); // lo
        v.write(0xA001, 0b0000_0000); // hi
        assert_eq!(v.tile_pixel(0, 7, 0), 0b11); // rightmost = low 2 bits = 11
        assert_eq!(v.tile_pixel(0, 6, 0), 0b01); // next = bits 2-3 = 01
        assert_eq!(v.tile_pixel(0, 0, 0), 0b00); // leftmost = 0
    }
}
