//! SNES PPU (Picture Processing Unit). Two chips (PPU1/PPU2) on real hardware;
//! we model them as one unit. Built from anomie's "SNES PPU registers" docs and
//! fullsnes.
//!
//! Implemented: 64 KiB VRAM, 512-byte CGRAM (256 BGR555 entries), 544-byte OAM,
//! the $2100-$213F register ports (display control, BG mode/scroll/tilemap/
//! charbase, Mode 7 matrix, VRAM/CGRAM/OAM ports, main/sub screen designation,
//! basic color math). Renders BG modes 0-7 (Mode 7 affine) + OBJ sprites to a
//! 256x224 RGBA8888 framebuffer per frame (whole-frame renderer, not a
//! dot-stepped one — accurate enough to get visible graphics).
//!
//! Stubbed/partial: windows, mosaic, offset-per-tile, hi-res/interlace,
//! sub-screen color-math blending beyond a simple average, per-scanline register
//! changes (we sample registers at frame end).

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
pub const FB_LEN: usize = SCREEN_W * SCREEN_H * 4;

const VRAM_SIZE: usize = 0x10000; // 64 KiB (32K words)
const CGRAM_SIZE: usize = 0x200; // 256 entries * 2 bytes
const OAM_SIZE: usize = 0x220; // 544 bytes

pub struct Ppu {
    pub vram: Box<[u8; VRAM_SIZE]>,
    pub cgram: Box<[u8; CGRAM_SIZE]>,
    pub oam: Box<[u8; OAM_SIZE]>,
    pub framebuffer: Box<[u8; FB_LEN]>,

    pub frame: u64,

    // --- display control ---
    forced_blank: bool,
    brightness: u8, // 0-15
    bg_mode: u8,    // 0-7
    bg3_prio: bool, // mode 1 BG3 priority bit

    // --- per-BG config ---
    /// tilemap base (word address >> 0), screen size (0-3) per BG.
    bg_sc_base: [u16; 4],
    bg_sc_size: [u8; 4],
    /// character (tile) base word address per BG.
    bg_char_base: [u16; 4],
    /// scroll registers per BG (10/13-bit).
    bg_hofs: [u16; 4],
    bg_vofs: [u16; 4],
    /// write latches for the dual-write scroll registers.
    scroll_latch: u8,
    scroll_latch_h: u8,

    // --- VRAM port ---
    vram_addr: u16,
    vram_inc: u16,
    vram_inc_on_high: bool,
    vram_remap: u8,
    vram_prefetch: u16,

    // --- CGRAM port ---
    cgram_addr: u16,
    cgram_latch: u8,
    cgram_latch_full: bool,

    // --- OAM port ---
    oam_addr: u16,
    oam_latch: u8,
    oam_latch_full: bool,
    oam_priority: bool,
    oam_base_reload: u16,

    // --- OBJ config ---
    obj_size_sel: u8,
    obj_char_base: u16,
    obj_char_gap: u16,

    // --- screen designation ---
    main_screen: u8, // bits: BG1-4, OBJ
    sub_screen: u8,

    // --- color math ---
    cgwsel: u8,
    cgadsub: u8,
    fixed_color: (u8, u8, u8), // BGR 5-bit components

    // --- Mode 7 matrix ---
    m7_latch: u8,
    m7a: i16,
    m7b: i16,
    m7c: i16,
    m7d: i16,
    m7x: i16,
    m7y: i16,
    m7hofs: i16,
    m7vofs: i16,
    m7_sel: u8,

    // --- status / counters used by the orchestrator ---
    pub scanline: u16,
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
    }
}

impl Ppu {
    pub fn new() -> Ppu {
        Ppu {
            vram: vec![0u8; VRAM_SIZE].into_boxed_slice().try_into().unwrap(),
            cgram: vec![0u8; CGRAM_SIZE].into_boxed_slice().try_into().unwrap(),
            oam: vec![0u8; OAM_SIZE].into_boxed_slice().try_into().unwrap(),
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            frame: 0,
            forced_blank: true,
            brightness: 0,
            bg_mode: 0,
            bg3_prio: false,
            bg_sc_base: [0; 4],
            bg_sc_size: [0; 4],
            bg_char_base: [0; 4],
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            scroll_latch: 0,
            scroll_latch_h: 0,
            vram_addr: 0,
            vram_inc: 1,
            vram_inc_on_high: true,
            vram_remap: 0,
            vram_prefetch: 0,
            cgram_addr: 0,
            cgram_latch: 0,
            cgram_latch_full: false,
            oam_addr: 0,
            oam_latch: 0,
            oam_latch_full: false,
            oam_priority: false,
            oam_base_reload: 0,
            obj_size_sel: 0,
            obj_char_base: 0,
            obj_char_gap: 0,
            main_screen: 0,
            sub_screen: 0,
            cgwsel: 0,
            cgadsub: 0,
            fixed_color: (0, 0, 0),
            m7_latch: 0,
            m7a: 0,
            m7b: 0,
            m7c: 0,
            m7d: 0,
            m7x: 0,
            m7y: 0,
            m7hofs: 0,
            m7vofs: 0,
            m7_sel: 0,
            scanline: 0,
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer[..]
    }

    // =========================================================================
    // Register ports ($2100-$213F).
    // =========================================================================
    pub fn write_reg(&mut self, reg: u16, v: u8) {
        match reg & 0xFF {
            0x00 => {
                self.forced_blank = v & 0x80 != 0;
                self.brightness = v & 0x0F;
            }
            0x01 => {
                self.obj_size_sel = (v >> 5) & 7;
                self.obj_char_base = ((v as u16 & 0x07) << 13) & 0xFFFF;
                self.obj_char_gap = ((v as u16 >> 3) & 0x03) << 13;
            }
            0x02 => {
                self.oam_addr = (self.oam_addr & 0x100) | v as u16;
                self.oam_base_reload = (self.oam_base_reload & 0x100) | v as u16;
                self.oam_latch_full = false;
            }
            0x03 => {
                self.oam_priority = v & 0x80 != 0;
                self.oam_addr = (self.oam_addr & 0xFF) | ((v as u16 & 1) << 8);
                self.oam_base_reload = self.oam_addr;
                self.oam_latch_full = false;
            }
            0x04 => self.oam_write(v),
            0x05 => {
                self.bg_mode = v & 0x07;
                self.bg3_prio = v & 0x08 != 0;
                // bits 4-7: per-BG tile size (8x8 vs 16x16); ignored (we render 8x8).
            }
            0x06 => {} // mosaic — stubbed
            0x07..=0x0A => {
                let bg = (reg & 0xFF) as usize - 0x07;
                self.bg_sc_base[bg] = ((v as u16 >> 2) & 0x3F) << 10;
                self.bg_sc_size[bg] = v & 0x03;
            }
            0x0B => {
                self.bg_char_base[0] = ((v as u16 & 0x0F) << 12) & 0xFFFF;
                self.bg_char_base[1] = ((v as u16 >> 4) << 12) & 0xFFFF;
            }
            0x0C => {
                self.bg_char_base[2] = ((v as u16 & 0x0F) << 12) & 0xFFFF;
                self.bg_char_base[3] = ((v as u16 >> 4) << 12) & 0xFFFF;
            }
            0x0D => {
                // BG1 H scroll (dual write) + Mode 7 H.
                self.bg_hofs[0] = ((v as u16) << 8 | (self.scroll_latch as u16 & !7) | (self.scroll_latch_h as u16 & 7)) & 0x3FF;
                self.scroll_latch = v;
                self.scroll_latch_h = self.bg_hofs[0] as u8;
                self.m7hofs = ((v as u16) << 8 | self.m7_latch as u16) as i16;
                self.m7_latch = v;
            }
            0x0E => {
                self.bg_vofs[0] = (((v as u16) << 8) | self.scroll_latch as u16) & 0x3FF;
                self.scroll_latch = v;
                self.m7vofs = ((v as u16) << 8 | self.m7_latch as u16) as i16;
                self.m7_latch = v;
            }
            0x0F => { self.bg_hofs[1] = self.scroll_write(v); }
            0x10 => { self.bg_vofs[1] = self.scroll_write(v); }
            0x11 => { self.bg_hofs[2] = self.scroll_write(v); }
            0x12 => { self.bg_vofs[2] = self.scroll_write(v); }
            0x13 => { self.bg_hofs[3] = self.scroll_write(v); }
            0x14 => { self.bg_vofs[3] = self.scroll_write(v); }
            0x15 => {
                self.vram_inc = match v & 0x03 {
                    0 => 1,
                    1 => 32,
                    _ => 128,
                };
                self.vram_remap = (v >> 2) & 0x03;
                self.vram_inc_on_high = v & 0x80 != 0;
            }
            0x16 => {
                self.vram_addr = (self.vram_addr & 0xFF00) | v as u16;
                self.vram_prefetch = self.vram_read_word(self.remap_addr());
            }
            0x17 => {
                self.vram_addr = (self.vram_addr & 0x00FF) | ((v as u16) << 8);
                self.vram_prefetch = self.vram_read_word(self.remap_addr());
            }
            0x18 => {
                let a = (self.remap_addr() as usize * 2) & (VRAM_SIZE - 1);
                self.vram[a] = v;
                if !self.vram_inc_on_high {
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_inc);
                }
            }
            0x19 => {
                let a = ((self.remap_addr() as usize * 2) + 1) & (VRAM_SIZE - 1);
                self.vram[a] = v;
                if self.vram_inc_on_high {
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_inc);
                }
            }
            0x1A => self.m7_sel = v,
            0x1B => { self.m7a = self.m7_write(v); }
            0x1C => { self.m7b = self.m7_write(v); }
            0x1D => { self.m7c = self.m7_write(v); }
            0x1E => { self.m7d = self.m7_write(v); }
            0x1F => { self.m7x = self.m7_write13(v); }
            0x20 => { self.m7y = self.m7_write13(v); }
            0x21 => {
                self.cgram_addr = v as u16;
                self.cgram_latch_full = false;
            }
            0x22 => self.cgram_write(v),
            0x23..=0x25 => {} // window mask settings — stubbed
            0x26..=0x29 => {} // window positions — stubbed
            0x2A | 0x2B => {} // window logic — stubbed
            0x2C => self.main_screen = v,
            0x2D => self.sub_screen = v,
            0x2E | 0x2F => {} // window main/sub disable — stubbed
            0x30 => self.cgwsel = v,
            0x31 => self.cgadsub = v,
            0x32 => {
                // COLDATA: set fixed color components selected by bits 5-7.
                let intensity = v & 0x1F;
                if v & 0x20 != 0 {
                    self.fixed_color.0 = intensity;
                }
                if v & 0x40 != 0 {
                    self.fixed_color.1 = intensity;
                }
                if v & 0x80 != 0 {
                    self.fixed_color.2 = intensity;
                }
            }
            0x33 => {} // SETINI (interlace/overscan) — stubbed
            _ => {}
        }
    }

    pub fn read_reg(&mut self, reg: u16) -> u8 {
        match reg & 0xFF {
            0x34 => (self.m7a as i32 * (self.m7b as i32 >> 8)) as u8, // MPYL
            0x35 => ((self.m7a as i32 * (self.m7b as i32 >> 8)) >> 8) as u8, // MPYM
            0x36 => ((self.m7a as i32 * (self.m7b as i32 >> 8)) >> 16) as u8, // MPYH
            0x37 => 0, // SLHV latch — stubbed
            0x38 => {
                // OAMDATAREAD.
                let a = (self.oam_addr & 0x3FF) as usize % OAM_SIZE;
                let v = self.oam[a];
                self.oam_addr = self.oam_addr.wrapping_add(1);
                v
            }
            0x39 => {
                // VMDATALREAD.
                let v = self.vram_prefetch as u8;
                if !self.vram_inc_on_high {
                    self.vram_prefetch = self.vram_read_word(self.remap_addr());
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_inc);
                }
                v
            }
            0x3A => {
                let v = (self.vram_prefetch >> 8) as u8;
                if self.vram_inc_on_high {
                    self.vram_prefetch = self.vram_read_word(self.remap_addr());
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_inc);
                }
                v
            }
            0x3B => {
                // CGDATAREAD.
                let a = (self.cgram_addr as usize * 2) % CGRAM_SIZE;
                let v = if self.cgram_latch_full {
                    self.cgram_addr = self.cgram_addr.wrapping_add(1);
                    self.cgram[(a + 1) % CGRAM_SIZE]
                } else {
                    self.cgram[a]
                };
                self.cgram_latch_full = !self.cgram_latch_full;
                v
            }
            0x3E => 0x01, // STAT77 (PPU1 version)
            0x3F => 0x02, // STAT78 (PPU2 version, NTSC)
            _ => 0,
        }
    }

    fn scroll_write(&mut self, v: u8) -> u16 {
        // Standard BG scroll dual-write formula.
        let r = (((v as u16) << 8) | (self.scroll_latch as u16 & !7) | (self.scroll_latch_h as u16 & 7)) & 0x3FF;
        self.scroll_latch = v;
        self.scroll_latch_h = r as u8;
        r
    }

    fn m7_write(&mut self, v: u8) -> i16 {
        let r = (((v as u16) << 8) | self.m7_latch as u16) as i16;
        self.m7_latch = v;
        r
    }
    fn m7_write13(&mut self, v: u8) -> i16 {
        // 13-bit signed center coordinate.
        let raw = ((v as u16) << 8) | self.m7_latch as u16;
        self.m7_latch = v;
        let r = (raw & 0x1FFF) as i16;
        (r << 3) >> 3 // sign-extend 13-bit
    }

    #[inline]
    fn remap_addr(&self) -> u16 {
        // Address remapping for $2115 bits 2-3. Default (0) is no remap.
        let a = self.vram_addr;
        match self.vram_remap {
            0 => a,
            1 => (a & 0xFF00) | ((a & 0x00E0) >> 5) | ((a & 0x001F) << 3),
            2 => (a & 0xFE00) | ((a & 0x01C0) >> 6) | ((a & 0x003F) << 3),
            _ => (a & 0xFC00) | ((a & 0x0380) >> 7) | ((a & 0x007F) << 3),
        }
    }

    #[inline]
    fn vram_read_word(&self, word_addr: u16) -> u16 {
        let a = (word_addr as usize * 2) & (VRAM_SIZE - 1);
        (self.vram[a] as u16) | ((self.vram[a + 1] as u16) << 8)
    }

    fn cgram_write(&mut self, v: u8) {
        if !self.cgram_latch_full {
            self.cgram_latch = v;
            self.cgram_latch_full = true;
        } else {
            let a = (self.cgram_addr as usize * 2) % CGRAM_SIZE;
            self.cgram[a] = self.cgram_latch;
            self.cgram[a + 1] = v;
            self.cgram_addr = self.cgram_addr.wrapping_add(1);
            self.cgram_latch_full = false;
        }
    }

    fn oam_write(&mut self, v: u8) {
        let addr = self.oam_addr;
        if addr & 0x200 != 0 {
            // High table (one byte per write).
            let a = (0x200 + (addr & 0x1F)) as usize % OAM_SIZE;
            self.oam[a] = v;
            self.oam_addr = self.oam_addr.wrapping_add(1);
        } else if !self.oam_latch_full {
            self.oam_latch = v;
            self.oam_latch_full = true;
            // Low byte latched; the address still advances so the high byte
            // lands at the next slot.
            self.oam_addr = self.oam_addr.wrapping_add(1);
        } else {
            let a = ((addr.wrapping_sub(1)) & 0x3FF) as usize % OAM_SIZE;
            self.oam[a] = self.oam_latch;
            let a2 = (addr & 0x3FF) as usize % OAM_SIZE;
            self.oam[a2] = v;
            self.oam_latch_full = false;
            self.oam_addr = self.oam_addr.wrapping_add(1);
        }
    }

    /// Directly poke an OAM byte (used by OAM DMA path if needed).
    pub fn oam_poke(&mut self, idx: usize, v: u8) {
        if idx < OAM_SIZE {
            self.oam[idx] = v;
        }
    }

    /// Reload OAM address from the latched base (start of vblank/frame).
    pub fn oam_reload(&mut self) {
        self.oam_addr = self.oam_base_reload;
        self.oam_latch_full = false;
    }

    // =========================================================================
    // Frame rendering. Called once per frame by the orchestrator after the CPU
    // has finished the visible portion (we sample the registers as they stand).
    // =========================================================================
    pub fn render_frame(&mut self) {
        self.frame += 1;
        if self.forced_blank || self.brightness == 0 {
            // Black screen.
            for px in self.framebuffer.chunks_exact_mut(4) {
                px.copy_from_slice(&[0, 0, 0, 0xFF]);
            }
            return;
        }
        // Backdrop = CGRAM color 0.
        let backdrop = self.cgram_rgb(0);
        let mut line_main = [0u32; SCREEN_W];
        let mut line_prio = [0u8; SCREEN_W];
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                line_main[x] = pack_idx(0, 0); // (color index encoded), 0 = backdrop
                line_prio[x] = 0;
            }
            self.render_line(y, &mut line_main, &mut line_prio);
            for x in 0..SCREEN_W {
                let enc = line_main[x];
                let rgb = if enc == 0 {
                    backdrop
                } else {
                    self.cgram_rgb((enc & 0xFF) as u8)
                };
                let (r, g, b) = self.apply_brightness(rgb);
                let off = (y * SCREEN_W + x) * 4;
                self.framebuffer[off] = r;
                self.framebuffer[off + 1] = g;
                self.framebuffer[off + 2] = b;
                self.framebuffer[off + 3] = 0xFF;
            }
        }
    }

    fn render_line(&self, y: usize, out: &mut [u32; SCREEN_W], prio: &mut [u8; SCREEN_W]) {
        // Mode 7 special-cased.
        if self.bg_mode == 7 {
            self.render_mode7_line(y, out, prio);
            self.render_objects_line(y, out, prio);
            return;
        }
        // Layer order: render lowest-priority first, higher overwrites.
        // We use a simple priority value per BG mode.
        let bpp = mode_bpp(self.bg_mode);
        // BGs from 4 down to 1, low priority first; then objects interleaved by
        // a coarse priority. We do: BG4-lo, BG3-lo, OBJ0/1, BG2-lo, BG1-lo,
        // BG4-hi, BG3-hi, OBJ2/3, BG2-hi, BG1-hi (approximation).
        let order: &[(usize, u8)] = match self.bg_mode {
            0 => &[(3, 0), (2, 0), (3, 1), (2, 1), (1, 0), (0, 0), (1, 1), (0, 1)],
            1 => &[(2, 0), (1, 0), (0, 0), (1, 1), (0, 1), (2, 1)],
            _ => &[(1, 0), (0, 0), (1, 1), (0, 1)],
        };
        for &(bg, p) in order {
            if self.main_screen & (1 << bg) == 0 {
                continue;
            }
            self.render_bg_line(bg, bpp[bg.min(3)], y, p, out, prio);
        }
        self.render_objects_line(y, out, prio);
    }

    fn render_bg_line(
        &self,
        bg: usize,
        bpp: u8,
        y: usize,
        want_prio: u8,
        out: &mut [u32; SCREEN_W],
        prio: &mut [u8; SCREEN_W],
    ) {
        let hofs = self.bg_hofs[bg] as usize;
        let vofs = self.bg_vofs[bg] as usize;
        let sc_base = self.bg_sc_base[bg];
        let sc_size = self.bg_sc_size[bg];
        let char_base = self.bg_char_base[bg];
        let pal_base_shift = match bpp {
            2 => 2,  // 4 colors
            4 => 4,  // 16 colors
            _ => 8,  // 256 colors (no palette offset)
        };

        let fy = y + vofs;
        for x in 0..SCREEN_W {
            let fx = x + hofs;
            let tile_x = (fx / 8) & 0x3F;
            let tile_y = (fy / 8) & 0x3F;
            // screen quadrant selection for 32x32 / 64x32 / etc.
            let mut map = sc_base as usize;
            let big_x = (fx / 8) & 0x20 != 0;
            let big_y = (fy / 8) & 0x20 != 0;
            match sc_size {
                0 => {}
                1 => { if big_x { map += 0x400; } }
                2 => { if big_y { map += 0x400; } }
                _ => {
                    if big_x { map += 0x400; }
                    if big_y { map += 0x800; }
                }
            }
            let entry_addr = (map + tile_y * 32 + tile_x) & 0x7FFF;
            let entry = self.vram_read_word(entry_addr as u16);
            let tile_num = (entry & 0x3FF) as usize;
            let pal = ((entry >> 10) & 0x07) as u8;
            let tile_prio = ((entry >> 13) & 1) as u8;
            let flip_x = entry & 0x4000 != 0;
            let flip_y = entry & 0x8000 != 0;
            if tile_prio != want_prio {
                continue;
            }
            let mut px = fx % 8;
            let mut py = fy % 8;
            if flip_x { px = 7 - px; }
            if flip_y { py = 7 - py; }
            let words_per_tile = (bpp as usize / 2) * 8;
            let tile_addr = (char_base as usize / 2 + tile_num * words_per_tile) & 0x7FFF;
            let color = self.decode_pixel(tile_addr, px, py, bpp);
            if color == 0 {
                continue;
            }
            let cidx = if bpp == 8 {
                color
            } else {
                ((pal as usize) << pal_base_shift) + color
            };
            out[x] = pack_idx(cidx as u8, 0);
            prio[x] = 1 + want_prio + (bg as u8) * 0; // mark as occupied
        }
    }

    /// Decode the 2/4/8-bpp pixel at (px,py) within a tile starting at
    /// `tile_word_addr` (word address into VRAM).
    fn decode_pixel(&self, tile_word_addr: usize, px: usize, py: usize, bpp: u8) -> usize {
        let mut color = 0usize;
        let planes = bpp as usize / 2;
        for plane in 0..planes {
            let word_addr = (tile_word_addr + py + plane * 8) & 0x7FFF;
            let word = self.vram_read_word(word_addr as u16);
            let lo_bit = (word >> (7 - px)) & 1;
            let hi_bit = (word >> (15 - px)) & 1;
            color |= (lo_bit as usize) << (plane * 2);
            color |= (hi_bit as usize) << (plane * 2 + 1);
        }
        color
    }

    fn render_mode7_line(&self, y: usize, out: &mut [u32; SCREEN_W], _prio: &mut [u8; SCREEN_W]) {
        if self.main_screen & 1 == 0 {
            return;
        }
        let a = self.m7a as i32;
        let b = self.m7b as i32;
        let c = self.m7c as i32;
        let d = self.m7d as i32;
        let cx = self.m7x as i32;
        let cy = self.m7y as i32;
        let h = self.m7hofs as i32;
        let v = self.m7vofs as i32;
        let sy = y as i32;
        for x in 0..SCREEN_W {
            let sx = x as i32;
            // screen -> map (fixed point, 8-bit fraction).
            let vx = a * (sx + h - cx) + b * (sy + v - cy);
            let vy = c * (sx + h - cx) + d * (sy + v - cy);
            let mapx = ((vx >> 8) + cx) & 0x3FF;
            let mapy = ((vy >> 8) + cy) & 0x3FF;
            let tile_x = (mapx as usize) / 8;
            let tile_y = (mapy as usize) / 8;
            let tilemap_idx = (tile_y * 128 + tile_x) & 0x3FFF;
            // Mode 7: tilemap is interleaved with char data; tile number at even
            // byte, pixel color at odd byte.
            let tile_num = self.vram[(tilemap_idx * 2) & (VRAM_SIZE - 1)] as usize;
            let in_x = (mapx as usize) % 8;
            let in_y = (mapy as usize) % 8;
            let pix_addr = (tile_num * 64 + in_y * 8 + in_x) * 2 + 1;
            let color = self.vram[pix_addr & (VRAM_SIZE - 1)] as usize;
            if color != 0 {
                out[x] = pack_idx(color as u8, 0);
            }
        }
    }

    fn render_objects_line(&self, y: usize, out: &mut [u32; SCREEN_W], prio: &mut [u8; SCREEN_W]) {
        if self.main_screen & 0x10 == 0 {
            return;
        }
        let (sw, sh) = obj_size(self.obj_size_sel);
        // Iterate sprites 0..128. Later sprites have lower priority; draw in
        // reverse so sprite 0 wins.
        for i in (0..128).rev() {
            let base = i * 4;
            let mut ox = self.oam[base] as i32;
            let oy = self.oam[base + 1] as i32;
            let tile = self.oam[base + 2] as usize;
            let attr = self.oam[base + 3];
            // high table: 2 bits per sprite (x high bit + size toggle).
            let high = self.oam[0x200 + i / 4];
            let shift = (i % 4) * 2;
            let x_high = (high >> shift) & 1;
            let size_big = (high >> (shift + 1)) & 1;
            if x_high != 0 {
                ox -= 256;
            }
            let (w, hgt) = if size_big != 0 { obj_size_large(self.obj_size_sel) } else { (sw, sh) };
            if (y as i32) < oy || (y as i32) >= oy + hgt as i32 {
                continue;
            }
            let pal = ((attr >> 1) & 0x07) as usize;
            let obj_prio = (attr >> 4) & 0x03;
            let flip_x = attr & 0x40 != 0;
            let flip_y = attr & 0x80 != 0;
            let name_bit = (attr & 0x01) as u16;
            let mut row = (y as i32 - oy) as usize;
            if flip_y { row = hgt as usize - 1 - row; }
            for col in 0..w as usize {
                let sx = ox + col as i32;
                if sx < 0 || sx >= SCREEN_W as i32 {
                    continue;
                }
                let mut cx = col;
                if flip_x { cx = w as usize - 1 - cx; }
                // tile within the sprite grid.
                let tcol = cx / 8;
                let trow = row / 8;
                let tile_index = (tile + trow * 16 + tcol) & 0xFF;
                let char_base = self.obj_char_base + name_bit * (0x1000 + self.obj_char_gap);
                let tile_word = (char_base as usize / 2 + tile_index * 16) & 0x7FFF;
                let color = self.decode_pixel(tile_word, cx % 8, row % 8, 4);
                if color == 0 {
                    continue;
                }
                let cidx = 128 + pal * 16 + color;
                // OBJ priority vs existing pixel: simple — objects on top.
                let _ = obj_prio;
                out[sx as usize] = pack_idx(cidx as u8, 0);
                prio[sx as usize] = 0xFF;
            }
        }
    }

    // ---- color helpers ----
    fn cgram_rgb(&self, idx: u8) -> (u8, u8, u8) {
        let a = (idx as usize * 2) % CGRAM_SIZE;
        let w = (self.cgram[a] as u16) | ((self.cgram[a + 1] as u16) << 8);
        let r5 = (w & 0x1F) as u8;
        let g5 = ((w >> 5) & 0x1F) as u8;
        let b5 = ((w >> 10) & 0x1F) as u8;
        // 5-bit -> 8-bit.
        ((r5 << 3) | (r5 >> 2), (g5 << 3) | (g5 >> 2), (b5 << 3) | (b5 >> 2))
    }

    fn apply_brightness(&self, rgb: (u8, u8, u8)) -> (u8, u8, u8) {
        if self.brightness >= 15 {
            return rgb;
        }
        let f = (self.brightness as u32 + 1) * 255 / 16;
        (
            (rgb.0 as u32 * f / 255) as u8,
            (rgb.1 as u32 * f / 255) as u8,
            (rgb.2 as u32 * f / 255) as u8,
        )
    }
}

/// Pack a palette index into the line buffer (room left for flags).
#[inline]
fn pack_idx(idx: u8, _flags: u8) -> u32 {
    idx as u32 | 0x100 // bit8 marks "drawn" so index 0 isn't the backdrop
}

fn mode_bpp(mode: u8) -> [u8; 4] {
    match mode {
        0 => [2, 2, 2, 2],
        1 => [4, 4, 2, 2],
        2 => [4, 4, 2, 2],
        3 => [8, 4, 2, 2],
        4 => [8, 2, 2, 2],
        5 => [4, 2, 2, 2],
        6 => [4, 2, 2, 2],
        _ => [8, 8, 8, 8], // mode 7 handled separately
    }
}

fn obj_size(sel: u8) -> (u8, u8) {
    match sel {
        0 => (8, 8),
        1 => (8, 8),
        2 => (8, 8),
        3 => (16, 16),
        4 => (16, 16),
        5 => (32, 32),
        6 => (16, 16),
        _ => (16, 16),
    }
}
fn obj_size_large(sel: u8) -> (u8, u8) {
    match sel {
        0 => (16, 16),
        1 => (32, 32),
        2 => (64, 64),
        3 => (32, 32),
        4 => (64, 64),
        5 => (64, 64),
        6 => (32, 32),
        _ => (32, 32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_word_port_roundtrip() {
        let mut ppu = Ppu::new();
        ppu.write_reg(0x15, 0x80); // inc on high byte, +1
        ppu.write_reg(0x16, 0x00); // addr lo
        ppu.write_reg(0x17, 0x00); // addr hi
        ppu.write_reg(0x18, 0xCD); // low byte
        ppu.write_reg(0x19, 0xAB); // high byte (advances)
        assert_eq!(ppu.vram[0], 0xCD);
        assert_eq!(ppu.vram[1], 0xAB);
        assert_eq!(ppu.vram_addr, 1);
    }

    #[test]
    fn cgram_color_decode() {
        let mut ppu = Ppu::new();
        ppu.write_reg(0x21, 0x01); // cgram addr = 1
        // white = 0x7FFF.
        ppu.write_reg(0x22, 0xFF);
        ppu.write_reg(0x22, 0x7F);
        let (r, g, b) = ppu.cgram_rgb(1);
        assert_eq!((r, g, b), (255, 255, 255));
    }

    #[test]
    fn forced_blank_renders_black() {
        let mut ppu = Ppu::new();
        ppu.write_reg(0x00, 0x80); // forced blank
        ppu.render_frame();
        assert_eq!(&ppu.framebuffer[0..4], &[0, 0, 0, 0xFF]);
        assert_eq!(ppu.frame, 1);
    }

    #[test]
    fn brightness_scales() {
        let mut ppu = Ppu::new();
        ppu.brightness = 15;
        assert_eq!(ppu.apply_brightness((255, 255, 255)).0, 255);
        ppu.brightness = 7;
        // Half brightness should roughly halve the channel.
        let half = ppu.apply_brightness((255, 255, 255)).0;
        assert!(half > 100 && half < 160, "got {half}");
    }
}
