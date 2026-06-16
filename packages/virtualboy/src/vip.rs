//! VIP — the Virtual Boy Video Image Processor. Renders the stereoscopic
//! red-on-black display: 384x224 per eye, monochrome with 4 brightness levels
//! encoded via the BRTA/BRTB/BRTC/REST registers. Built from the Planet Virtual
//! Boy "Sacred Tech Scroll" VIP chapter.
//!
//! Memory layout (the VIP owns 0x00000000-0x0007FFFF, 512 KiB DRAM/VRAM):
//!   0x00000000  Left frame buffer 0   (0x6000 bytes, 384x256 @ 2bpp columns)
//!   0x00006000  CHR (character) table 0 (interleaved with the left FB region)
//!   0x00008000  Left frame buffer 1
//!   0x00010000  Right frame buffer 0
//!   0x00018000  Right frame buffer 1
//!   0x00020000  BG map memory (16 KiB worth of 512x512 maps) + world params
//!   0x0003D800  World attribute table (32 worlds x 32 bytes)
//!   0x0003DC00  Column table (CTA) data
//!   0x0003E000  OAM / object attribute memory (1024 objects x 8 bytes)
//!   0x0005F800  Character table (full 2048 chars x 16 bytes) mirror
//!   0x0005F800..0x00060000 etc.
//!   0x0007FE00+ I/O registers (INTPND, DPSTTS, XPSTTS, BRT*, ...)
//!
//! Characters: 8x8 pixels, 2 bits per pixel = 16 bytes each, 2048 total. The
//! 2-bit pixel value indexes a 4-entry palette (GPLT/JPLT); each palette entry
//! selects one of 4 brightness levels (0..3). Brightness 0 is always black;
//! 1/2/3 map to BRTA / BRTA+BRTB / BRTA+BRTB+BRTC intensities.
//!
//! We render the LEFT eye into an RGBA framebuffer with R = brightness, G=B=0.
//!
//! IMPLEMENTED: normal + H-bias BG worlds, OBJ (sprite) worlds, BGMap/CHR/OAM
//! memory, GPLT/JPLT palettes, BRT brightness mapping, the column table is
//! treated as uniform (no per-column repeat), frame (XPEND/SBHIT) + drawing
//! interrupts, the DPSTTS/XPSTTS status registers.
//! STUBBED/PARTIAL: affine + H-bias warping are approximated as normal scroll;
//! the column table brightness repeat-count is ignored; LED duty / anaglyph.

pub const DISP_W: usize = 384;
pub const DISP_H: usize = 224;
pub const FB_LEN: usize = DISP_W * DISP_H * 4;

/// VIP DRAM size (512 KiB).
pub const VRAM_SIZE: usize = 0x0008_0000;

// ---- Interrupt bits (INTPND / INTENB / INTCLR) ----
pub const INT_SCANERR: u16 = 1 << 0;
pub const INT_LFBEND: u16 = 1 << 1;
pub const INT_RFBEND: u16 = 1 << 2;
pub const INT_GAMESTART: u16 = 1 << 3;
pub const INT_FRAMESTART: u16 = 1 << 4;
pub const INT_SBHIT: u16 = 1 << 13;
pub const INT_XPEND: u16 = 1 << 14; // drawing finished
pub const INT_TIMEERR: u16 = 1 << 15;

pub struct Vip {
    /// The full 512 KiB VIP DRAM/VRAM. Boxed so the god-struct stays small.
    pub vram: Box<[u8; VRAM_SIZE]>,

    /// Left-eye RGBA8888 output (384x224).
    pub framebuffer: Box<[u8; FB_LEN]>,

    // ---- I/O registers ----
    pub intpnd: u16,
    pub intenb: u16,
    pub dpctrl: u16, // display control (DPSTTS write side)
    pub xpctrl: u16, // drawing control (XPSTTS write side)
    pub brta: u8,
    pub brtb: u8,
    pub brtc: u8,
    pub rest: u8,
    pub frmcyc: u16, // frame repeat (game frame = display frame * (frmcyc+1))
    pub spt: [u16; 4], // OBJ group control pointers (SPT0..SPT3)
    pub gplt: [u8; 4], // BG palettes 0-3
    pub jplt: [u8; 4], // OBJ palettes 0-3
    pub bkcol: u8,     // backdrop colour

    /// Display on/off (DPCTRL bit DISP).
    pub disp_on: bool,
    /// Drawing on/off (XPCTRL bit XPEN).
    pub draw_on: bool,

    /// Which framebuffer pair is being displayed (toggles each game frame).
    pub fb_select: bool,

    pub frame: u64,
}

impl Vip {
    pub fn new() -> Vip {
        Vip {
            vram: vec![0u8; VRAM_SIZE].into_boxed_slice().try_into().unwrap(),
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            intpnd: 0,
            intenb: 0,
            dpctrl: 0,
            xpctrl: 0,
            brta: 0,
            brtb: 0,
            brtc: 0,
            rest: 0,
            frmcyc: 0,
            spt: [0; 4],
            gplt: [0; 4],
            jplt: [0; 4],
            bkcol: 0,
            disp_on: false,
            draw_on: false,
            fb_select: false,
            frame: 0,
        }
    }

    /// Does the VIP currently assert its interrupt line to the CPU?
    pub fn irq_asserted(&self) -> bool {
        self.intpnd & self.intenb != 0
    }

    // =====================================================================
    // VRAM access (byte/halfword). Most VIP memory is naturally halfword.
    // =====================================================================
    #[inline]
    pub fn read8(&self, addr: u32) -> u8 {
        let a = (addr as usize) & (VRAM_SIZE - 1);
        // I/O register block at 0x5F800..0x60000? No — VIP regs are at the very
        // top, handled by read_reg. Plain DRAM here.
        self.vram[a]
    }
    #[inline]
    pub fn write8(&mut self, addr: u32, v: u8) {
        let a = (addr as usize) & (VRAM_SIZE - 1);
        self.vram[a] = v;
    }
    #[inline]
    pub fn read16(&self, addr: u32) -> u16 {
        let a = (addr as usize) & (VRAM_SIZE - 1) & !1;
        u16::from_le_bytes([self.vram[a], self.vram[a + 1]])
    }
    #[inline]
    pub fn write16(&mut self, addr: u32, v: u16) {
        let a = (addr as usize) & (VRAM_SIZE - 1) & !1;
        let b = v.to_le_bytes();
        self.vram[a] = b[0];
        self.vram[a + 1] = b[1];
    }

    // =====================================================================
    // VIP register block (mapped at 0x0005F800-0x0005FFFF on real hardware).
    // The Vb god-struct routes accesses in that window here.
    // =====================================================================
    pub fn read_reg(&self, addr: u32) -> u16 {
        match addr & 0x7E {
            0x00 => self.intpnd,
            0x02 => self.intenb,
            // INTCLR reads as 0.
            0x04 => 0,
            0x20 => self.dpstts(),
            0x22 => self.dpctrl,
            0x24 => self.brta as u16,
            0x26 => self.brtb as u16,
            0x28 => self.brtc as u16,
            0x2A => self.rest as u16,
            0x2E => self.frmcyc,
            0x30 => self.cta(),
            0x40 => self.xpstts(),
            0x42 => self.xpctrl,
            0x44 => 0x0004, // VIP version
            0x48 => self.spt[0],
            0x4A => self.spt[1],
            0x4C => self.spt[2],
            0x4E => self.spt[3],
            0x60 => self.gplt[0] as u16,
            0x62 => self.gplt[1] as u16,
            0x64 => self.gplt[2] as u16,
            0x66 => self.gplt[3] as u16,
            0x68 => self.jplt[0] as u16,
            0x6A => self.jplt[1] as u16,
            0x6C => self.jplt[2] as u16,
            0x6E => self.jplt[3] as u16,
            0x70 => self.bkcol as u16,
            _ => 0,
        }
    }

    pub fn write_reg(&mut self, addr: u32, v: u16) {
        match addr & 0x7E {
            // INTPND is read-only; writes ignored.
            0x00 => {}
            0x02 => self.intenb = v,
            0x04 => self.intpnd &= !v, // INTCLR
            0x20 => {} // DPSTTS read-only
            0x22 => {
                self.dpctrl = v;
                self.disp_on = v & 0x0002 != 0; // DISP bit
            }
            0x24 => self.brta = v as u8,
            0x26 => self.brtb = v as u8,
            0x28 => self.brtc = v as u8,
            0x2A => self.rest = v as u8,
            0x2E => self.frmcyc = v & 0x000F,
            0x40 => {} // XPSTTS read-only
            0x42 => {
                self.xpctrl = v;
                self.draw_on = v & 0x0002 != 0; // XPEN bit
            }
            0x48 => self.spt[0] = v,
            0x4A => self.spt[1] = v,
            0x4C => self.spt[2] = v,
            0x4E => self.spt[3] = v,
            0x60 => self.gplt[0] = v as u8,
            0x62 => self.gplt[1] = v as u8,
            0x64 => self.gplt[2] = v as u8,
            0x66 => self.gplt[3] = v as u8,
            0x68 => self.jplt[0] = v as u8,
            0x6A => self.jplt[1] = v as u8,
            0x6C => self.jplt[2] = v as u8,
            0x6E => self.jplt[3] = v as u8,
            0x70 => self.bkcol = v as u8,
            _ => {}
        }
    }

    fn dpstts(&self) -> u16 {
        // DISP enable + scan-ready flags. Bit1 DISP, bits 2-5 FCLK/SCANRDY etc.
        let mut v = 0u16;
        if self.disp_on {
            v |= 0x0002;
        }
        v |= 0x0040; // SCANRDY
        v
    }
    fn xpstts(&self) -> u16 {
        let mut v = 0u16;
        if self.draw_on {
            v |= 0x0002; // XPEN
        }
        v
    }
    fn cta(&self) -> u16 {
        0
    }

    // =====================================================================
    // Frame: render the left eye and raise frame interrupts.
    //
    // Called once per game frame by the Vb god-struct. We:
    //   1. raise FRAMESTART/GAMESTART,
    //   2. render the world stack into the left framebuffer,
    //   3. raise XPEND (drawing complete) + LFBEND.
    // =====================================================================
    pub fn run_frame(&mut self) {
        self.frame += 1;
        self.fb_select = !self.fb_select;

        // Frame-start interrupts (always asserted at frame boundary).
        self.intpnd |= INT_FRAMESTART | INT_GAMESTART;

        if self.disp_on && self.draw_on {
            self.render_left_eye();
        } else {
            // Display off -> black screen (red intensity 0).
            for px in self.framebuffer.chunks_exact_mut(4) {
                px.copy_from_slice(&[0, 0, 0, 0xFF]);
            }
        }

        // Drawing complete + left framebuffer end.
        self.intpnd |= INT_XPEND | INT_LFBEND | INT_RFBEND;
    }

    /// Map a brightness level (0..3) and palette to a red RGBA pixel.
    #[inline]
    fn brightness_to_rgba(&self, level: u8) -> [u8; 4] {
        let red = match level & 3 {
            0 => 0u16,
            1 => self.brta as u16,
            2 => self.brta as u16 + self.brtb as u16,
            _ => self.brta as u16 + self.brtb as u16 + self.brtc as u16,
        };
        let r = red.min(255) as u8;
        [r, 0, 0, 0xFF]
    }

    /// Translate a 2-bit character colour through a palette register into a
    /// brightness level (0..3). Palette byte packs four 2-bit fields.
    #[inline]
    fn palette_level(palette: u8, color: u8) -> u8 {
        (palette >> ((color & 3) * 2)) & 3
    }

    fn render_left_eye(&mut self) {
        // Backdrop: BKCOL is a brightness level.
        let backdrop = self.brightness_to_rgba(self.bkcol & 3);
        for px in self.framebuffer.chunks_exact_mut(4) {
            px.copy_from_slice(&backdrop);
        }

        // World attribute table: 32 worlds, 32 bytes each, drawn from world 31
        // down to world 0 (higher index = drawn first / behind). Each world's
        // header bit (END) stops the stack.
        const WAT_BASE: u32 = 0x0003_D800;
        // We render front-to-back is wrong for painter's; render world 31..0 so
        // lower worlds (later) overwrite. We iterate 31 down to 0 and paint;
        // a set END bit terminates.
        for w in (0..32).rev() {
            let wbase = WAT_BASE + (w as u32) * 32;
            let header = self.read16(wbase);
            // END bit (bit 6) -> stop processing the world stack.
            if header & 0x0040 != 0 {
                continue;
            }
            // LON (bit 15) — display on left eye. We render the left eye only.
            if header & 0x8000 == 0 {
                continue;
            }
            let bgm = (header >> 12) & 3; // BG map type: 0 normal,1 H-bias,2 affine,3 OBJ
            match bgm {
                3 => self.render_obj_world(wbase, header),
                _ => self.render_bg_world(wbase, header),
            }
        }
    }

    /// Render a normal / H-bias / affine BG world (approximated as a scrolled
    /// tilemap). Reads the world attributes and blits the visible window.
    fn render_bg_world(&mut self, wbase: u32, header: u16) {
        // World attribute fields (halfword offsets):
        //   +0  header (LON RON BGM SCX SCY OVR END BG-map-base)
        //   +2  GX (destination X, signed 10-bit)
        //   +4  GP (parallax, signed)
        //   +6  GY (destination Y)
        //   +8  MX (source X)
        //   +A  MP (source parallax)
        //   +C  MY (source Y)
        //   +E  W  (window width-1)
        //   +10 H  (window height-1)
        let gx = sign_ext(self.read16(wbase + 2) & 0x03FF, 10);
        let gy = self.read16(wbase + 6) as i16 as i32;
        let mx = sign_ext(self.read16(wbase + 8) & 0x1FFF, 13);
        let my = sign_ext(self.read16(wbase + 0xC) & 0x1FFF, 13);
        let w = (self.read16(wbase + 0xE) & 0x03FF) as i32; // width-1
        let h = (self.read16(wbase + 0x10) & 0x03FF) as i32; // height-1

        let map_base = (header & 0x000F) as u32; // base BG map index
        let scx = ((header >> 10) & 3) as u32; // map columns: 1<<scx
        let scy = ((header >> 8) & 3) as u32; // map rows: 1<<scy
        let maps_x = 1u32 << scx;
        let maps_y = 1u32 << scy;

        const BGMAP_BASE: u32 = 0x0002_0000;
        let total_w = (maps_x * 64 * 8) as i32; // pixels (64 tiles per map * 8)
        let total_h = (maps_y * 64 * 8) as i32;
        if total_w == 0 || total_h == 0 {
            return;
        }

        for dy in 0..=h {
            let sy = dy + my;
            let py = gy + dy;
            if py < 0 || py >= DISP_H as i32 {
                continue;
            }
            let map_y = sy.rem_euclid(total_h);
            for dx in 0..=w {
                let sx = dx + mx;
                let px = gx + dx;
                if px < 0 || px >= DISP_W as i32 {
                    continue;
                }
                let map_x = sx.rem_euclid(total_w);

                // Which sub-map (for the multi-map SCX/SCY arrangement)?
                let tile_col = (map_x / 8) as u32;
                let tile_row = (map_y / 8) as u32;
                let sub_x = (tile_col / 64) % maps_x;
                let sub_y = (tile_row / 64) % maps_y;
                let sub_map = (map_base + sub_y * maps_x + sub_x) & 0xF;
                let in_col = tile_col % 64;
                let in_row = tile_row % 64;
                let cell_addr =
                    BGMAP_BASE + (sub_map * 0x2000) + (in_row * 64 + in_col) * 2;
                let cell = self.read16(cell_addr);

                let char_no = (cell & 0x07FF) as u32;
                let pal = ((cell >> 14) & 3) as usize;
                let hflip = cell & 0x2000 != 0;
                let vflip = cell & 0x1000 != 0;

                let mut fx = (map_x % 8) as u32;
                let mut fy = (map_y % 8) as u32;
                if hflip {
                    fx = 7 - fx;
                }
                if vflip {
                    fy = 7 - fy;
                }

                let color = self.char_pixel(char_no, fx, fy);
                if color == 0 {
                    continue; // transparent
                }
                let level = Self::palette_level(self.gplt[pal], color);
                let rgba = self.brightness_to_rgba(level);
                let off = (py as usize * DISP_W + px as usize) * 4;
                self.framebuffer[off..off + 4].copy_from_slice(&rgba);
            }
        }
    }

    /// Render an OBJ (sprite) world. OBJ worlds draw from OAM rather than a map;
    /// the objects to draw are bounded by the SPT group pointers.
    fn render_obj_world(&mut self, _wbase: u32, _header: u16) {
        const OAM_BASE: u32 = 0x0003_E000;
        // Draw all 1024 objects from the highest SPT group down. For simplicity
        // we draw objects 0..=spt[3] (the top group end), painting in reverse so
        // lower-indexed objects appear in front.
        let last = (self.spt[3] & 0x03FF) as i32;
        for o in (0..=last).rev() {
            let obase = OAM_BASE + (o as u32) * 8;
            let jx = sign_ext(self.read16(obase) & 0x03FF, 10);
            let jp_word = self.read16(obase + 2);
            let jy = (self.read16(obase + 4) as i16 as i32) & 0xFF; // 8-bit Y
            let jy = if jy >= 224 { jy - 256 } else { jy };
            let cell = self.read16(obase + 6);

            // JCA char index, palette, flips.
            let char_no = (cell & 0x07FF) as u32;
            let pal = ((cell >> 14) & 3) as usize;
            let hflip = cell & 0x2000 != 0;
            let vflip = cell & 0x1000 != 0;
            // JLON (bit14 of jp_word) — display on left eye.
            if jp_word & 0x8000 == 0 {
                continue;
            }

            for ty in 0..8 {
                let py = jy + ty;
                if py < 0 || py >= DISP_H as i32 {
                    continue;
                }
                for tx in 0..8 {
                    let px = jx + tx;
                    if px < 0 || px >= DISP_W as i32 {
                        continue;
                    }
                    let mut fx = tx as u32;
                    let mut fy = ty as u32;
                    if hflip {
                        fx = 7 - fx;
                    }
                    if vflip {
                        fy = 7 - fy;
                    }
                    let color = self.char_pixel(char_no, fx, fy);
                    if color == 0 {
                        continue;
                    }
                    let level = Self::palette_level(self.jplt[pal], color);
                    let rgba = self.brightness_to_rgba(level);
                    let off = (py as usize * DISP_W + px as usize) * 4;
                    self.framebuffer[off..off + 4].copy_from_slice(&rgba);
                }
            }
        }
    }

    /// Fetch the 2-bit colour of pixel (x,y) within character `char_no`.
    /// Characters are 8x8 @ 2bpp = 16 bytes; the table is split across two
    /// regions on real hardware but the 0x6000-based "character RAM" plus the
    /// 0x1E000 mirror are unified into our VRAM. We use the canonical character
    /// table base at 0x00078000 (CHR table region) with wrap.
    #[inline]
    fn char_pixel(&self, char_no: u32, x: u32, y: u32) -> u8 {
        // Character data: 4 banks at 0x6000,0xE000,0x16000,0x1E000 in classic
        // VB layout. We use the linear character table at 0x00078000 which the
        // bus also mirrors the bank regions into. Address = base + char*16 +
        // y*2; the two bytes hold 8 pixels (2bpp, little-endian, LSB = leftmost).
        const CHR_BASE: u32 = 0x0007_8000;
        let row_addr = CHR_BASE + char_no * 16 + y * 2;
        let row = self.read16(row_addr);
        ((row >> (x * 2)) & 3) as u8
    }
}

impl Default for Vip {
    fn default() -> Self {
        Vip::new()
    }
}

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sign_ext(v: u16, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v as i32) << shift) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_mapping() {
        let mut vip = Vip::new();
        vip.brta = 32;
        vip.brtb = 64;
        vip.brtc = 32;
        assert_eq!(vip.brightness_to_rgba(0), [0, 0, 0, 0xFF]);
        assert_eq!(vip.brightness_to_rgba(1), [32, 0, 0, 0xFF]);
        assert_eq!(vip.brightness_to_rgba(2), [96, 0, 0, 0xFF]);
        assert_eq!(vip.brightness_to_rgba(3), [128, 0, 0, 0xFF]);
    }

    #[test]
    fn palette_unpacks_fields() {
        // palette 0b11_10_01_00 = entries [0,1,2,3]
        let p = 0b11_10_01_00;
        assert_eq!(Vip::palette_level(p, 0), 0);
        assert_eq!(Vip::palette_level(p, 1), 1);
        assert_eq!(Vip::palette_level(p, 2), 2);
        assert_eq!(Vip::palette_level(p, 3), 3);
    }

    #[test]
    fn reg_intclr_clears_pending() {
        let mut vip = Vip::new();
        vip.intpnd = INT_XPEND | INT_FRAMESTART;
        vip.write_reg(0x04, INT_XPEND); // INTCLR
        assert_eq!(vip.intpnd, INT_FRAMESTART);
    }

    #[test]
    fn irq_gated_by_enable() {
        let mut vip = Vip::new();
        vip.intpnd = INT_XPEND;
        assert!(!vip.irq_asserted());
        vip.intenb = INT_XPEND;
        assert!(vip.irq_asserted());
    }

    #[test]
    fn char_pixel_decodes_2bpp() {
        let mut vip = Vip::new();
        // Character 0, row 0 = 0b...11_10_01_00 across 8 px (LSB leftmost).
        // pixel0=0, pixel1=1, pixel2=2, pixel3=3
        const CHR_BASE: u32 = 0x0007_8000;
        vip.write16(CHR_BASE, 0b11100100);
        assert_eq!(vip.char_pixel(0, 0, 0), 0);
        assert_eq!(vip.char_pixel(0, 1, 0), 1);
        assert_eq!(vip.char_pixel(0, 2, 0), 2);
        assert_eq!(vip.char_pixel(0, 3, 0), 3);
    }

    #[test]
    fn run_frame_raises_xpend_and_advances() {
        let mut vip = Vip::new();
        vip.disp_on = true;
        vip.draw_on = true;
        let f0 = vip.frame;
        vip.run_frame();
        assert_eq!(vip.frame, f0 + 1);
        assert!(vip.intpnd & INT_XPEND != 0);
        assert!(vip.intpnd & INT_FRAMESTART != 0);
    }

    #[test]
    fn bg_world_renders_a_tile() {
        let mut vip = Vip::new();
        vip.disp_on = true;
        vip.draw_on = true;
        vip.brta = 100;
        vip.gplt[0] = 0b11_10_01_00;

        // Character 1: fill row 0 with color 3 across all 8 pixels.
        const CHR_BASE: u32 = 0x0007_8000;
        vip.write16(CHR_BASE + 1 * 16, 0xFFFF); // all pixels color 3

        // BG map 0 cell (0,0) -> char 1, palette 0.
        const BGMAP_BASE: u32 = 0x0002_0000;
        vip.write16(BGMAP_BASE, 0x0001);

        // World 31: enabled (LON), normal BG, placed at GX=0,GY=0, window 8x8,
        // map base 0, scx=scy=0.
        const WAT_BASE: u32 = 0x0003_D800;
        let wbase = WAT_BASE + 31 * 32;
        vip.write16(wbase, 0x8000); // LON set, BGM=0, base=0
        vip.write16(wbase + 2, 0); // GX
        vip.write16(wbase + 6, 0); // GY
        vip.write16(wbase + 8, 0); // MX
        vip.write16(wbase + 0xC, 0); // MY
        vip.write16(wbase + 0xE, 7); // W = 8-1
        vip.write16(wbase + 0x10, 7); // H = 8-1
        // Mark worlds 0..30 as END so the stack stops above world 31's content
        // not necessary, but ensure they don't draw garbage: set END bit (0x40).
        for w in 0..31 {
            vip.write16(WAT_BASE + w * 32, 0x0040);
        }

        vip.run_frame();
        // Pixel (0,0) should now be the row-0 color-3 -> level 3 -> red 100.
        let off = 0;
        assert_eq!(vip.framebuffer[off], 100);
        assert_eq!(vip.framebuffer[off + 1], 0);
    }
}
