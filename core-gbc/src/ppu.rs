//! The PPU (LCD controller): mode FSM, background/window/sprite rendering, and
//! CGB color, producing a 160×144 RGBA8888 framebuffer.
//!
//! Spec: Pan Docs — LCDC, STAT, Rendering, Pixel FIFO, OAM, Palettes, CGB
//! Registers (gbdev.io/pandocs).
//!
//! ## Timing
//! Each scanline is 456 dots. A frame is 154 lines (0-153): lines 0-143 are
//! visible, 144-153 are V-Blank. Within a visible line the mode sequence is:
//!   * mode 2 (OAM scan)   — dots 0..80
//!   * mode 3 (drawing)    — dots 80..252 (we use a fixed length)
//!   * mode 0 (H-Blank)    — until 456
//! and mode 1 (V-Blank) spans lines 144-153. STAT (0xFF41) exposes the mode in
//! bits 1-0, the LY==LYC coincidence in bit 2, and per-source STAT interrupt
//! enables in bits 6-3. The V-Blank interrupt fires on entering line 144.
//!
//! ## Rendering model
//! We render a full scanline at the mode-2→3 boundary into an internal line
//! buffer of colors, then commit it to the RGBA framebuffer. This "scanline
//! renderer" is simpler than a dot-accurate FIFO but visually identical for the
//! vast majority of games.

use crate::interrupts::{Interrupt, Irq};
use crate::memory::Memory;

pub const SCREEN_W: usize = 160;
pub const SCREEN_H: usize = 144;
const DOTS_PER_LINE: u32 = 456;
const TOTAL_LINES: u8 = 154;
const OAM_DOTS: u32 = 80;
const DRAW_DOTS: u32 = 172;

// LCDC (0xFF40) bit masks.
const LCDC_ENABLE: u8 = 0x80;
const LCDC_WIN_MAP: u8 = 0x40; // window tile-map area (0=9800, 1=9C00)
const LCDC_WIN_ENABLE: u8 = 0x20;
const LCDC_TILE_DATA: u8 = 0x10; // 0=8800 signed, 1=8000 unsigned
const LCDC_BG_MAP: u8 = 0x08; // bg tile-map area
const LCDC_OBJ_SIZE: u8 = 0x04; // 0=8x8, 1=8x16
const LCDC_OBJ_ENABLE: u8 = 0x02;
const LCDC_BG_ENABLE: u8 = 0x01; // DMG: BG enable; CGB: BG/Win master priority

// STAT (0xFF41) bit masks.
const STAT_LYC_IE: u8 = 0x40;
const STAT_OAM_IE: u8 = 0x20;
const STAT_VBLANK_IE: u8 = 0x10;
const STAT_HBLANK_IE: u8 = 0x08;
const STAT_LYC_FLAG: u8 = 0x04;

/// PPU mode (STAT bits 1-0). Closed enum, matched exhaustively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Drawing = 3,
}

pub struct Ppu {
    // ---- LCD registers ----
    pub lcdc: u8, // 0xFF40
    pub stat: u8, // 0xFF41 (bits 6-3 writable; 2-0 are status)
    pub scy: u8,  // 0xFF42
    pub scx: u8,  // 0xFF43
    pub ly: u8,   // 0xFF44 (read-only)
    pub lyc: u8,  // 0xFF45
    pub bgp: u8,  // 0xFF47 (DMG BG palette)
    pub obp0: u8, // 0xFF48 (DMG OBJ palette 0)
    pub obp1: u8, // 0xFF49 (DMG OBJ palette 1)
    pub wy: u8,   // 0xFF4A
    pub wx: u8,   // 0xFF4B

    /// Whether the cart runs in CGB mode (true → use CGB palettes/attributes).
    pub cgb_mode: bool,

    mode: Mode,
    dot: u32,
    /// Internal window line counter (only advances when the window is drawn).
    window_line: u8,
    /// True once V-Blank for the current frame has been entered (frame ready).
    pub frame_ready: bool,
    /// Set true the dot the PPU enters H-Blank on a visible line (HDMA trigger).
    pub entered_hblank: bool,

    /// RGBA8888 output, SCREEN_W*SCREEN_H*4 bytes.
    pub framebuffer: Box<[u8]>,
    /// Per-pixel BG color index (0-3) of the current scanline, for OBJ priority.
    bg_color_index: [u8; SCREEN_W],
    /// Per-pixel BG-over-OBJ priority flag (CGB BG attr bit 7 + DMG default).
    bg_priority: [bool; SCREEN_W],
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            lcdc: 0x91,
            stat: 0x80,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            cgb_mode: true,
            mode: Mode::OamScan,
            dot: 0,
            window_line: 0,
            frame_ready: false,
            entered_hblank: false,
            framebuffer: vec![0u8; SCREEN_W * SCREEN_H * 4].into_boxed_slice(),
            bg_color_index: [0; SCREEN_W],
            bg_priority: [false; SCREEN_W],
        }
    }

    #[inline]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Advance the PPU by `cycles` dots (T-cycles at the PPU's fixed clock; the
    /// PPU is *not* affected by CGB double-speed). Drives the mode FSM, raises
    /// V-Blank/STAT interrupts, and renders scanlines.
    pub fn step(&mut self, cycles: u32, mem: &Memory, irq: &mut Irq) {
        self.entered_hblank = false;
        if self.lcdc & LCDC_ENABLE == 0 {
            // LCD off: PPU is reset, LY=0, mode 0. No interrupts.
            self.ly = 0;
            self.dot = 0;
            self.mode = Mode::HBlank;
            self.window_line = 0;
            self.stat = (self.stat & 0xF8) | (Mode::HBlank as u8);
            return;
        }

        let mut remaining = cycles;
        while remaining > 0 {
            let step = remaining; // we can process in one chunk per call cheaply
            self.dot += step;
            remaining -= step;

            // Advance through the line, handling mode transitions.
            self.tick_line(mem, irq);
        }
    }

    /// Resolve mode transitions and line advance for the accumulated `dot`.
    fn tick_line(&mut self, mem: &Memory, irq: &mut Irq) {
        loop {
            if self.ly < SCREEN_H as u8 {
                // Visible line: OAM scan → drawing → H-Blank.
                if self.dot < OAM_DOTS {
                    self.set_mode(Mode::OamScan, irq);
                } else if self.dot < OAM_DOTS + DRAW_DOTS {
                    if self.mode != Mode::Drawing {
                        self.set_mode(Mode::Drawing, irq);
                        // Render the scanline once, at the start of drawing.
                        self.render_scanline(mem);
                    }
                } else if self.dot < DOTS_PER_LINE {
                    if self.mode != Mode::HBlank {
                        self.set_mode(Mode::HBlank, irq);
                        self.entered_hblank = true;
                    }
                }
            } else {
                // V-Blank lines (144-153).
                if self.mode != Mode::VBlank {
                    self.set_mode(Mode::VBlank, irq);
                    if self.ly == SCREEN_H as u8 {
                        irq.request(Interrupt::VBlank);
                        self.frame_ready = true;
                    }
                }
            }

            if self.dot >= DOTS_PER_LINE {
                self.dot -= DOTS_PER_LINE;
                self.ly += 1;
                if self.ly >= TOTAL_LINES {
                    self.ly = 0;
                    self.window_line = 0;
                }
                self.check_lyc(irq);
                // Continue the loop to settle the new line's mode.
            } else {
                break;
            }
        }
    }

    fn set_mode(&mut self, mode: Mode, irq: &mut Irq) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.stat = (self.stat & 0xF8) | (mode as u8);
        // STAT interrupt on entering an enabled mode (modes 0/1/2).
        let fire = match mode {
            Mode::HBlank => self.stat & STAT_HBLANK_IE != 0,
            Mode::VBlank => self.stat & STAT_VBLANK_IE != 0,
            Mode::OamScan => self.stat & STAT_OAM_IE != 0,
            Mode::Drawing => false,
        };
        if fire {
            irq.request(Interrupt::Stat);
        }
    }

    fn check_lyc(&mut self, irq: &mut Irq) {
        if self.ly == self.lyc {
            self.stat |= STAT_LYC_FLAG;
            if self.stat & STAT_LYC_IE != 0 {
                irq.request(Interrupt::Stat);
            }
        } else {
            self.stat &= !STAT_LYC_FLAG;
        }
    }

    // ---- IO register access (0xFF40-0xFF4B, minus banked ones) ----
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => self.stat | 0x80, // bit 7 reads 1
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, v: u8, irq: &mut Irq) {
        match addr {
            0xFF40 => {
                let was_on = self.lcdc & LCDC_ENABLE != 0;
                self.lcdc = v;
                if was_on && v & LCDC_ENABLE == 0 {
                    // Turning the LCD off resets the FSM.
                    self.ly = 0;
                    self.dot = 0;
                    self.window_line = 0;
                    self.mode = Mode::HBlank;
                    self.stat = (self.stat & 0xF8) | (Mode::HBlank as u8);
                }
            }
            0xFF41 => {
                // Bits 6-3 writable; bits 2-0 (mode + LYC flag) read-only.
                self.stat = (v & 0x78) | (self.stat & 0x07) | 0x80;
            }
            0xFF42 => self.scy = v,
            0xFF43 => self.scx = v,
            0xFF44 => {} // LY read-only
            0xFF45 => {
                self.lyc = v;
                if self.lcdc & LCDC_ENABLE != 0 {
                    self.check_lyc(irq);
                }
            }
            0xFF47 => self.bgp = v,
            0xFF48 => self.obp0 = v,
            0xFF49 => self.obp1 = v,
            0xFF4A => self.wy = v,
            0xFF4B => self.wx = v,
            _ => {}
        }
    }

    // ============================ Rendering ============================

    fn render_scanline(&mut self, mem: &Memory) {
        let ly = self.ly as usize;
        if ly >= SCREEN_H {
            return;
        }
        self.render_bg_window(mem);
        if self.lcdc & LCDC_OBJ_ENABLE != 0 {
            self.render_sprites(mem);
        }
    }

    /// Read a VRAM byte from a specific bank (0 or 1). CGB BG attributes live in
    /// bank 1 at the same offsets as the tile-map in bank 0.
    #[inline]
    fn vram(mem: &Memory, bank: usize, addr: u16) -> u8 {
        let off = bank * crate::regions::VRAM_BANK_SIZE
            + ((addr as usize) & (crate::regions::VRAM_BANK_SIZE - 1));
        mem.vram[off]
    }

    fn render_bg_window(&mut self, mem: &Memory) {
        let ly = self.ly as usize;
        let fb_row = ly * SCREEN_W * 4;

        // On DMG, LCDC bit 0 clear blanks BG/window (white). On CGB, bit 0 is a
        // master priority bit; BG still renders. We treat !cgb && !bg_enable as
        // a blank line.
        let bg_enabled = self.cgb_mode || (self.lcdc & LCDC_BG_ENABLE != 0);

        let win_enabled = self.lcdc & LCDC_WIN_ENABLE != 0
            && (self.lcdc & LCDC_BG_ENABLE != 0 || self.cgb_mode)
            && self.wy <= self.ly;
        let wx = self.wx.wrapping_sub(7);

        let mut window_drawn = false;

        for x in 0..SCREEN_W {
            let use_window = win_enabled && x as u8 >= wx && self.wx <= 166;

            let (map_base, tile_x, tile_y) = if use_window {
                window_drawn = true;
                let map = if self.lcdc & LCDC_WIN_MAP != 0 { 0x9C00 } else { 0x9800 };
                let wx_off = (x as u8).wrapping_sub(wx);
                (map, wx_off as usize, self.window_line as usize)
            } else {
                let map = if self.lcdc & LCDC_BG_MAP != 0 { 0x9C00 } else { 0x9800 };
                let bx = (x as u8).wrapping_add(self.scx);
                let by = (self.ly).wrapping_add(self.scy);
                (map, bx as usize, by as usize)
            };

            if !bg_enabled {
                self.put_pixel(fb_row + x * 4, 0xFF, 0xFF, 0xFF);
                self.bg_color_index[x] = 0;
                self.bg_priority[x] = false;
                continue;
            }

            let tile_col = tile_x / 8;
            let tile_row = tile_y / 8;
            let map_addr = map_base + (tile_row * 32 + tile_col) as u16;
            let tile_num = Self::vram(mem, 0, map_addr);

            // CGB BG attributes from VRAM bank 1.
            let attr = if self.cgb_mode { Self::vram(mem, 1, map_addr) } else { 0 };
            let attr_bank = ((attr >> 3) & 1) as usize;
            let attr_pal = (attr & 0x07) as usize;
            let flip_x = attr & 0x20 != 0;
            let flip_y = attr & 0x40 != 0;
            let bg_to_oam_prio = attr & 0x80 != 0;

            // Resolve tile data address.
            let tile_addr = if self.lcdc & LCDC_TILE_DATA != 0 {
                0x8000 + (tile_num as u16) * 16
            } else {
                // 0x8800 signed addressing, base 0x9000.
                0x9000u16.wrapping_add(((tile_num as i8) as i16 * 16) as u16)
            };

            let mut row_in_tile = (tile_y % 8) as u16;
            if flip_y {
                row_in_tile = 7 - row_in_tile;
            }
            let lo = Self::vram(mem, attr_bank, tile_addr + row_in_tile * 2);
            let hi = Self::vram(mem, attr_bank, tile_addr + row_in_tile * 2 + 1);

            let mut bit = 7 - (tile_x % 8);
            if flip_x {
                bit = tile_x % 8;
            }
            let color_id = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);

            self.bg_color_index[x] = color_id;
            self.bg_priority[x] = bg_to_oam_prio;

            let (r, g, b) = if self.cgb_mode {
                Self::cgb_color(&mem.bg_palette, attr_pal, color_id)
            } else {
                Self::dmg_shade(self.bgp, color_id)
            };
            self.put_pixel(fb_row + x * 4, r, g, b);
        }

        if window_drawn {
            self.window_line = self.window_line.wrapping_add(1);
        }
    }

    fn render_sprites(&mut self, mem: &Memory) {
        let ly = self.ly as i16;
        let height: i16 = if self.lcdc & LCDC_OBJ_SIZE != 0 { 16 } else { 8 };
        let fb_row = self.ly as usize * SCREEN_W * 4;

        // Gather up to 10 sprites on this line, in OAM order.
        let mut sprites: Vec<(usize, i16, i16, u8, u8)> = Vec::with_capacity(10);
        for i in 0..40 {
            let base = i * 4;
            let sy = mem.oam[base] as i16 - 16;
            let sx = mem.oam[base + 1] as i16 - 8;
            let tile = mem.oam[base + 2];
            let attr = mem.oam[base + 3];
            if ly >= sy && ly < sy + height {
                sprites.push((i, sx, sy, tile, attr));
                if sprites.len() == 10 {
                    break;
                }
            }
        }

        // Priority: on DMG, smaller X wins; ties broken by OAM index. On CGB,
        // OAM index alone decides (lower index = higher priority). We draw
        // lowest-priority first so higher-priority overwrites.
        if !self.cgb_mode {
            sprites.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
        } else {
            sprites.sort_by(|a, b| b.0.cmp(&a.0));
        }

        for (_, sx, sy, tile, attr) in sprites {
            let palette = (attr & 0x07) as usize; // CGB OBJ palette
            let bank = if self.cgb_mode { ((attr >> 3) & 1) as usize } else { 0 };
            let dmg_pal = if attr & 0x10 != 0 { self.obp1 } else { self.obp0 };
            let flip_x = attr & 0x20 != 0;
            let flip_y = attr & 0x40 != 0;
            let obj_behind_bg = attr & 0x80 != 0;

            let mut row = ly - sy;
            if flip_y {
                row = height - 1 - row;
            }
            // 8x16 sprites: bit 0 of the tile index is ignored.
            let tile_index = if height == 16 { tile & 0xFE } else { tile };
            let tile_addr = 0x8000u16 + tile_index as u16 * 16 + (row as u16) * 2;
            let lo = Self::vram(mem, bank, tile_addr);
            let hi = Self::vram(mem, bank, tile_addr + 1);

            for px in 0..8i16 {
                let x = sx + px;
                if x < 0 || x >= SCREEN_W as i16 {
                    continue;
                }
                let xi = x as usize;
                let mut bit = 7 - px;
                if flip_x {
                    bit = px;
                }
                let color_id = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);
                if color_id == 0 {
                    continue; // transparent
                }

                // Priority resolution against BG/window.
                // CGB master priority (LCDC bit 0): when clear, OBJ always wins.
                let master = self.lcdc & LCDC_BG_ENABLE != 0;
                let bg_idx = self.bg_color_index[xi];
                let bg_has_prio = self.bg_priority[xi] && master;
                let obj_loses = (obj_behind_bg || bg_has_prio) && bg_idx != 0 && master;
                if obj_loses {
                    continue;
                }

                let (r, g, b) = if self.cgb_mode {
                    Self::cgb_color(&mem.obj_palette, palette, color_id)
                } else {
                    Self::dmg_shade(dmg_pal, color_id)
                };
                self.put_pixel(fb_row + xi * 4, r, g, b);
            }
        }
    }

    #[inline]
    fn put_pixel(&mut self, off: usize, r: u8, g: u8, b: u8) {
        self.framebuffer[off] = r;
        self.framebuffer[off + 1] = g;
        self.framebuffer[off + 2] = b;
        self.framebuffer[off + 3] = 0xFF;
    }

    /// DMG 2-bit shade through a palette register → grayscale RGB.
    #[inline]
    fn dmg_shade(palette: u8, color_id: u8) -> (u8, u8, u8) {
        let shade = (palette >> (color_id * 2)) & 0x03;
        let v = match shade {
            0 => 0xFF,
            1 => 0xAA,
            2 => 0x55,
            _ => 0x00,
        };
        (v, v, v)
    }

    /// CGB palette lookup: `palette` 0-7, `color_id` 0-3, into a 64-byte
    /// little-endian RGB555 palette RAM. Returns 8-bit RGB.
    #[inline]
    fn cgb_color(pram: &[u8; crate::regions::CRAM_SIZE], palette: usize, color_id: u8) -> (u8, u8, u8) {
        let idx = palette * 8 + (color_id as usize) * 2;
        let lo = pram[idx] as u16;
        let hi = pram[idx + 1] as u16;
        let rgb555 = lo | (hi << 8);
        let r5 = (rgb555 & 0x1F) as u8;
        let g5 = ((rgb555 >> 5) & 0x1F) as u8;
        let b5 = ((rgb555 >> 10) & 0x1F) as u8;
        // Scale 5-bit → 8-bit (the common (x<<3)|(x>>2) expansion).
        let exp = |c: u8| (c << 3) | (c >> 2);
        (exp(r5), exp(g5), exp(b5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_fsm_cycles_through_a_line() {
        let mut ppu = Ppu::new();
        let mem = Memory::new();
        let mut irq = Irq::new();
        ppu.lcdc = 0x80; // enable
        assert_eq!(ppu.ly, 0);
        // OAM scan at the start.
        ppu.step(10, &mem, &mut irq);
        assert_eq!(ppu.mode(), Mode::OamScan);
        // Into drawing.
        ppu.step(80, &mem, &mut irq);
        assert_eq!(ppu.mode(), Mode::Drawing);
        // Into H-Blank.
        ppu.step(180, &mem, &mut irq);
        assert_eq!(ppu.mode(), Mode::HBlank);
    }

    #[test]
    fn vblank_interrupt_at_line_144() {
        let mut ppu = Ppu::new();
        let mem = Memory::new();
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        ppu.lcdc = 0x80;
        // Run 144 full lines.
        ppu.step(456 * 144, &mem, &mut irq);
        assert_eq!(ppu.ly, 144);
        assert_eq!(ppu.mode(), Mode::VBlank);
        assert_eq!(irq.pending() & Interrupt::VBlank.mask(), Interrupt::VBlank.mask());
    }

    #[test]
    fn lyc_coincidence_sets_stat_flag() {
        let mut ppu = Ppu::new();
        let mem = Memory::new();
        let mut irq = Irq::new();
        ppu.lcdc = 0x80;
        ppu.lyc = 5;
        ppu.step(456 * 5 + 1, &mem, &mut irq);
        assert_eq!(ppu.ly, 5);
        assert_eq!(ppu.stat & STAT_LYC_FLAG, STAT_LYC_FLAG);
    }

    #[test]
    fn renders_a_bg_tile() {
        let mut ppu = Ppu::new();
        let mut mem = Memory::new();
        let mut irq = Irq::new();
        ppu.cgb_mode = false;
        ppu.lcdc = LCDC_ENABLE | LCDC_BG_ENABLE | LCDC_TILE_DATA;
        ppu.bgp = 0b11_10_01_00; // identity-ish palette

        // Tile 0: fill row 0 with color id 3 (both bitplanes set).
        // Tile data at 0x8000, map at 0x9800 already references tile 0.
        mem.vram[0] = 0xFF; // lo plane row 0
        mem.vram[1] = 0xFF; // hi plane row 0

        // Render line 0.
        ppu.step(80 + 1, &mem, &mut irq);
        // Pixel (0,0): color id 3 -> shade 3 -> black.
        assert_eq!(ppu.framebuffer[0], 0x00);
        assert_eq!(ppu.framebuffer[3], 0xFF); // alpha
    }

    #[test]
    fn framebuffer_is_correct_size() {
        let ppu = Ppu::new();
        assert_eq!(ppu.framebuffer.len(), 160 * 144 * 4);
    }
}
