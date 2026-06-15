//! VI — the Video Interface. The N64's display controller: it scans a
//! framebuffer out of RDRAM to the screen at a configured origin, width, and
//! pixel format (16-bit RGBA5551 or 32-bit RGBA8888). The VI also raises a
//! vertical interrupt (MI INTR_VI) each frame when the current scanline
//! reaches VI_V_INTR.
//!
//! This is a FOUNDATION VI: it owns the register block, raises the vertical
//! interrupt once per frame, and — the load-bearing part — converts the RDRAM
//! framebuffer to the host's RGBA8888 output ([`Vi::scanout`]). That means
//! anything the CPU (or, later, the RDP) writes into RDRAM at the VI origin is
//! visible. Interlacing, AA filtering, the X/Y scale fractional resampling, and
//! the precise NTSC/PAL timing are simplified.
//!
//! Built from n64brew "Video Interface".

/// VI register byte offsets within the VI block.
pub const VI_CTRL: u32 = 0x00; // VI_STATUS / VI_CONTROL (pixel format, AA mode)
pub const VI_ORIGIN: u32 = 0x04; // framebuffer RDRAM address
pub const VI_WIDTH: u32 = 0x08; // framebuffer width in pixels
pub const VI_V_INTR: u32 = 0x0C; // scanline at which to raise the V interrupt
pub const VI_V_CURRENT: u32 = 0x10; // current half-line
pub const VI_BURST: u32 = 0x14;
pub const VI_V_SYNC: u32 = 0x18; // total half-lines per frame
pub const VI_H_SYNC: u32 = 0x1C;
pub const VI_H_SYNC_LEAP: u32 = 0x20;
pub const VI_H_VIDEO: u32 = 0x24; // active horizontal video range
pub const VI_V_VIDEO: u32 = 0x28; // active vertical video range
pub const VI_V_BURST: u32 = 0x2C;
pub const VI_X_SCALE: u32 = 0x30;
pub const VI_Y_SCALE: u32 = 0x34;

/// Pixel-type field of VI_CTRL (bits 1..0): 0/1 = blank, 2 = 16-bit, 3 = 32-bit.
const CTRL_TYPE_MASK: u32 = 0b11;
const TYPE_RGBA5551: u32 = 2;
const TYPE_RGBA8888: u32 = 3;

/// Default visible resolution we present (the canonical low-res N64 frame).
pub const DEFAULT_WIDTH: usize = 320;
pub const DEFAULT_HEIGHT: usize = 240;

pub struct Vi {
    /// The 14 VI registers, indexed by offset/4.
    pub regs: [u32; 14],
}

impl Default for Vi {
    fn default() -> Self {
        Self::new()
    }
}

impl Vi {
    pub fn new() -> Self {
        let mut regs = [0u32; 14];
        regs[(VI_WIDTH / 4) as usize] = DEFAULT_WIDTH as u32;
        Vi { regs }
    }

    pub fn read(&self, offset: u32) -> u32 {
        let idx = (offset / 4) as usize;
        if idx < self.regs.len() {
            self.regs[idx]
        } else {
            0
        }
    }

    pub fn write(&mut self, offset: u32, v: u32) {
        let idx = (offset / 4) as usize;
        if idx < self.regs.len() {
            self.regs[idx] = v;
        }
    }

    #[inline]
    pub fn origin(&self) -> u32 {
        self.regs[(VI_ORIGIN / 4) as usize] & 0x00FF_FFFF
    }
    #[inline]
    pub fn fb_width(&self) -> usize {
        (self.regs[(VI_WIDTH / 4) as usize] & 0xFFF) as usize
    }
    #[inline]
    pub fn pixel_type(&self) -> u32 {
        self.regs[(VI_CTRL / 4) as usize] & CTRL_TYPE_MASK
    }

    /// Compute the visible height from the active vertical range (VI_V_VIDEO),
    /// which holds (start << 16 | end) in half-lines; the frame height is
    /// (end - start) / 2. Falls back to the default when unconfigured.
    pub fn height(&self) -> usize {
        let vv = self.regs[(VI_V_VIDEO / 4) as usize];
        let start = (vv >> 16) & 0x3FF;
        let end = vv & 0x3FF;
        if end > start {
            (((end - start) / 2) as usize).clamp(1, 480)
        } else {
            DEFAULT_HEIGHT
        }
    }

    /// The width we present: the framebuffer width register, clamped.
    pub fn width(&self) -> usize {
        let w = self.fb_width();
        if w == 0 {
            DEFAULT_WIDTH
        } else {
            w.min(640)
        }
    }

    /// Scan the RDRAM framebuffer out to `out` (RGBA8888, width*height*4). When
    /// the VI is blanked or misconfigured we clear to black. `rdram` is the raw
    /// big-endian main-memory bytes.
    pub fn scanout(&self, rdram: &[u8], out: &mut Vec<u8>) {
        let w = self.width();
        let h = self.height();
        out.clear();
        out.resize(w * h * 4, 0);

        let ptype = self.pixel_type();
        if ptype != TYPE_RGBA5551 && ptype != TYPE_RGBA8888 {
            // Blanked: leave the cleared (opaque-black via alpha fixup) frame.
            for px in out.chunks_exact_mut(4) {
                px[3] = 0xFF;
            }
            return;
        }

        let origin = self.origin() as usize;
        let fb_w = if self.fb_width() == 0 { w } else { self.fb_width() };

        for y in 0..h {
            for x in 0..w {
                let src_x = x.min(fb_w.saturating_sub(1));
                let (r, g, b) = match ptype {
                    TYPE_RGBA5551 => {
                        let off = origin + (y * fb_w + src_x) * 2;
                        if off + 1 >= rdram.len() {
                            (0, 0, 0)
                        } else {
                            let p = u16::from_be_bytes([rdram[off], rdram[off + 1]]);
                            rgba5551_to_rgb(p)
                        }
                    }
                    _ => {
                        // RGBA8888
                        let off = origin + (y * fb_w + src_x) * 4;
                        if off + 3 >= rdram.len() {
                            (0, 0, 0)
                        } else {
                            (rdram[off], rdram[off + 1], rdram[off + 2])
                        }
                    }
                };
                let d = (y * w + x) * 4;
                out[d] = r;
                out[d + 1] = g;
                out[d + 2] = b;
                out[d + 3] = 0xFF;
            }
        }
    }
}

/// Expand a 16-bit RGBA5551 pixel (5R/5G/5B/1A, big-endian) to 8-bit RGB.
#[inline]
fn rgba5551_to_rgb(p: u16) -> (u8, u8, u8) {
    let r5 = ((p >> 11) & 0x1F) as u8;
    let g5 = ((p >> 6) & 0x1F) as u8;
    let b5 = ((p >> 1) & 0x1F) as u8;
    // 5-bit -> 8-bit by replicating the high bits.
    let e = |c: u8| (c << 3) | (c >> 2);
    (e(r5), e(g5), e(b5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba5551_expands_white_and_red() {
        // 0xFFFF = R31 G31 B31 A1 -> ~white.
        let (r, g, b) = rgba5551_to_rgb(0xFFFF);
        assert_eq!((r, g, b), (255, 255, 255));
        // Pure red: R31 = 11111, others 0. 0xF800 | A0 -> 0xF800.
        let (r, g, b) = rgba5551_to_rgb(0xF800);
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn scanout_16bit_framebuffer_to_rgba8888() {
        let mut vi = Vi::new();
        vi.write(VI_CTRL, TYPE_RGBA5551);
        vi.write(VI_ORIGIN, 0);
        vi.write(VI_WIDTH, 2);
        // V_VIDEO range yielding height 1: start=0, end=2 half-lines -> 1.
        vi.write(VI_V_VIDEO, (0 << 16) | 2);
        // Two pixels: white then red, big-endian.
        let mut rdram = vec![0u8; 16];
        rdram[0..2].copy_from_slice(&0xFFFFu16.to_be_bytes());
        rdram[2..4].copy_from_slice(&0xF800u16.to_be_bytes());
        let mut out = Vec::new();
        vi.scanout(&rdram, &mut out);
        assert_eq!(vi.width(), 2);
        assert_eq!(vi.height(), 1);
        // pixel 0 white
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
        // pixel 1 red
        assert_eq!(&out[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn scanout_blank_clears_to_opaque_black() {
        let vi = Vi::new(); // CTRL type 0 = blank
        let rdram = vec![0xABu8; 0x10000];
        let mut out = Vec::new();
        vi.scanout(&rdram, &mut out);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
    }
}
