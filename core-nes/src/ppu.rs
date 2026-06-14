//! The 2C02 PPU.
//!
//! Spec: NESdev wiki "PPU registers", "PPU rendering", "PPU scrolling",
//! "PPU sprite evaluation". Implements the loopy v/t/x/w scroll model,
//! background tile + attribute fetch via a shift-register pipeline, sprite
//! evaluation (8 sprites/line, sprite-0 hit, overflow), the 341-dot ×
//! 262-scanline frame, and the VBlank NMI. Renders into a 256×240 RGBA8888
//! framebuffer using the canonical NES palette.
//!
//! The PPU reaches CHR (pattern tables) + cartridge-controlled nametable
//! mirroring through a `PpuBus` trait the orchestrator implements, mirroring
//! the CPU's `Bus` indirection.

use crate::cart::Mirroring;

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 240;
pub const FB_LEN: usize = SCREEN_W * SCREEN_H * 4;

/// What the PPU needs from the rest of the machine: CHR/pattern reads (routed
/// through the mapper) and the current nametable mirroring.
pub trait PpuBus {
    fn chr_read(&mut self, addr: u16) -> u8;
    fn chr_write(&mut self, addr: u16, v: u8);
    fn mirroring(&mut self) -> Mirroring;
    /// Report a PPU bus address so the mapper can clock A12 (MMC3 IRQ).
    fn ppu_a12(&mut self, addr: u16) {
        let _ = addr;
    }
}

pub struct Ppu {
    // ---- registers ----
    ctrl: u8,   // PPUCTRL  $2000
    mask: u8,   // PPUMASK  $2001
    status: u8, // PPUSTATUS $2002
    oam_addr: u8,

    // loopy scroll registers.
    v: u16, // current VRAM address (15 bit)
    t: u16, // temporary VRAM address
    x: u8,  // fine X scroll (3 bit)
    w: bool, // write toggle

    // PPUDATA read buffer (reads are delayed by one).
    read_buffer: u8,

    // ---- memory the PPU owns ----
    pub vram: [u8; 0x800],    // 2 KiB nametable RAM
    pub palette: [u8; 0x20],  // palette RAM
    pub oam: [u8; 0x100],     // 256 bytes object attribute memory

    // ---- timing ----
    pub scanline: i16, // -1 (pre-render) .. 260
    pub dot: u16,      // 0..340
    frame_odd: bool,

    // ---- background fetch pipeline ----
    nt_byte: u8,
    at_byte: u8,
    pt_lo: u8,
    pt_hi: u8,
    bg_shift_lo: u16,
    bg_shift_hi: u16,
    at_shift_lo: u16,
    at_shift_hi: u16,
    at_latch_lo: u8,
    at_latch_hi: u8,

    // ---- sprites ----
    secondary_oam: [u8; 32], // 8 sprites × 4 bytes for the next scanline
    sprite_count: usize,
    sprite_patterns: [(u8, u8); 8], // (lo, hi) pattern bytes
    sprite_x: [u8; 8],
    sprite_attr: [u8; 8],
    sprite_is_zero: [bool; 8],

    // ---- output ----
    pub framebuffer: Box<[u8; FB_LEN]>,
    /// Set true when a VBlank starts with NMI enabled — the orchestrator pulls
    /// this to raise the CPU NMI line.
    pub nmi_signal: bool,
    /// Incremented at the end of each completed frame.
    pub frame: u64,
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
    }
}

impl Ppu {
    pub fn new() -> Ppu {
        Ppu {
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            x: 0,
            w: false,
            read_buffer: 0,
            vram: [0; 0x800],
            palette: [0; 0x20],
            oam: [0; 0x100],
            scanline: -1,
            dot: 0,
            frame_odd: false,
            nt_byte: 0,
            at_byte: 0,
            pt_lo: 0,
            pt_hi: 0,
            bg_shift_lo: 0,
            bg_shift_hi: 0,
            at_shift_lo: 0,
            at_shift_hi: 0,
            at_latch_lo: 0,
            at_latch_hi: 0,
            secondary_oam: [0xFF; 32],
            sprite_count: 0,
            sprite_patterns: [(0, 0); 8],
            sprite_x: [0; 8],
            sprite_attr: [0; 8],
            sprite_is_zero: [false; 8],
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            nmi_signal: false,
            frame: 0,
        }
    }

    #[inline]
    fn rendering_enabled(&self) -> bool {
        self.mask & 0x18 != 0
    }

    // ================= CPU-facing register access =================

    pub fn read_reg(&mut self, bus: &mut dyn PpuBus, reg: u16) -> u8 {
        match reg & 7 {
            2 => {
                // PPUSTATUS: top 3 bits + stale buffer in low 5. Reading clears
                // VBlank and the write toggle.
                let v = (self.status & 0xE0) | (self.read_buffer & 0x1F);
                self.status &= !0x80;
                self.w = false;
                v
            }
            4 => self.oam[self.oam_addr as usize],
            7 => {
                let addr = self.v & 0x3FFF;
                let result;
                if addr >= 0x3F00 {
                    // Palette reads are immediate; the buffer gets the
                    // underlying nametable byte.
                    result = self.read_palette(addr);
                    self.read_buffer = self.ppu_read(bus, addr & 0x2FFF);
                } else {
                    result = self.read_buffer;
                    self.read_buffer = self.ppu_read(bus, addr);
                }
                self.increment_v_data();
                result
            }
            _ => 0,
        }
    }

    pub fn write_reg(&mut self, bus: &mut dyn PpuBus, reg: u16, v: u8) {
        match reg & 7 {
            0 => {
                self.ctrl = v;
                // t: nametable select bits 10-11.
                self.t = (self.t & 0xF3FF) | (((v as u16) & 0x03) << 10);
            }
            1 => self.mask = v,
            3 => self.oam_addr = v,
            4 => {
                self.oam[self.oam_addr as usize] = v;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            5 => {
                if !self.w {
                    self.t = (self.t & 0xFFE0) | ((v as u16) >> 3);
                    self.x = v & 0x07;
                    self.w = true;
                } else {
                    self.t = (self.t & 0x8FFF) | (((v as u16) & 0x07) << 12);
                    self.t = (self.t & 0xFC1F) | (((v as u16) & 0xF8) << 2);
                    self.w = false;
                }
            }
            6 => {
                if !self.w {
                    self.t = (self.t & 0x80FF) | (((v as u16) & 0x3F) << 8);
                    self.w = true;
                } else {
                    self.t = (self.t & 0xFF00) | (v as u16);
                    self.v = self.t;
                    self.w = false;
                }
            }
            7 => {
                let addr = self.v & 0x3FFF;
                self.ppu_write(bus, addr, v);
                self.increment_v_data();
            }
            _ => {}
        }
    }

    /// OAM DMA: copy a 256-byte CPU page into OAM starting at oam_addr.
    pub fn oam_dma_byte(&mut self, i: u8, v: u8) {
        let dst = self.oam_addr.wrapping_add(i);
        self.oam[dst as usize] = v;
    }

    #[inline]
    fn increment_v_data(&mut self) {
        let inc = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = (self.v + inc) & 0x7FFF;
    }

    // ================= PPU-internal bus (nametables/palette/CHR) =================

    fn ppu_read(&mut self, bus: &mut dyn PpuBus, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        match a {
            0x0000..=0x1FFF => {
                bus.ppu_a12(a);
                bus.chr_read(a)
            }
            0x2000..=0x3EFF => {
                let i = self.mirror_nt(bus, a);
                self.vram[i]
            }
            _ => self.read_palette(a),
        }
    }

    fn ppu_write(&mut self, bus: &mut dyn PpuBus, addr: u16, v: u8) {
        let a = addr & 0x3FFF;
        match a {
            0x0000..=0x1FFF => {
                bus.ppu_a12(a);
                bus.chr_write(a, v);
            }
            0x2000..=0x3EFF => {
                let i = self.mirror_nt(bus, a);
                self.vram[i] = v;
            }
            _ => self.write_palette(a, v),
        }
    }

    fn mirror_nt(&mut self, bus: &mut dyn PpuBus, addr: u16) -> usize {
        let a = (addr - 0x2000) & 0x0FFF;
        let table = (a / 0x400) as usize; // 0..3
        let offset = (a % 0x400) as usize;
        let phys = match bus.mirroring() {
            Mirroring::Vertical => table & 1,
            Mirroring::Horizontal => (table >> 1) & 1,
            Mirroring::SingleLower => 0,
            Mirroring::SingleUpper => 1,
            // Four-screen would need cart VRAM; approximate with the 2 KiB by
            // folding to two tables (rare; games shipping their own VRAM).
            Mirroring::FourScreen => table & 1,
        };
        phys * 0x400 + offset
    }

    #[inline]
    fn palette_index(addr: u16) -> usize {
        let mut i = (addr & 0x1F) as usize;
        // $3F10/$14/$18/$1C mirror $3F00/$04/$08/$0C.
        if i >= 0x10 && i & 0x03 == 0 {
            i -= 0x10;
        }
        i
    }
    fn read_palette(&self, addr: u16) -> u8 {
        self.palette[Self::palette_index(addr)]
    }
    fn write_palette(&mut self, addr: u16, v: u8) {
        self.palette[Self::palette_index(addr)] = v & 0x3F;
    }

    // ================= Rendering =================

    /// Advance one PPU dot. The orchestrator calls this 3× per CPU cycle.
    pub fn step(&mut self, bus: &mut dyn PpuBus) {
        let visible = self.scanline >= 0 && self.scanline < 240;
        let pre_render = self.scanline == -1;
        let render = (visible || pre_render) && self.rendering_enabled();

        if render {
            self.tick_background(bus);
        }

        // Emit a pixel during the visible region.
        if visible && self.dot >= 1 && self.dot <= 256 {
            self.render_pixel(bus);
        }

        // Sprite evaluation: at dot 257 of each rendered line, fill secondary
        // OAM + fetch patterns for the *next* line (a simplification of the
        // hardware's interleaved evaluation; visually equivalent).
        if render && self.dot == 257 {
            self.evaluate_sprites(bus);
        }

        // VBlank start: scanline 241, dot 1. Set the flag and signal NMI.
        if self.scanline == 241 && self.dot == 1 {
            self.status |= 0x80;
            if self.ctrl & 0x80 != 0 {
                self.nmi_signal = true;
            }
        }
        // Pre-render line clears VBlank/sprite0/overflow at dot 1.
        if pre_render && self.dot == 1 {
            self.status &= !0xE0;
        }

        self.advance_dot();
    }

    fn advance_dot(&mut self) {
        self.dot += 1;
        if self.dot > 340 {
            self.dot = 0;
            self.scanline += 1;
            if self.scanline > 260 {
                self.scanline = -1;
                self.frame += 1;
                self.frame_odd = !self.frame_odd;
                // Odd-frame cycle skip: on odd frames with rendering enabled,
                // the pre-render line is one dot shorter.
                if self.frame_odd && self.rendering_enabled() {
                    self.dot = 1;
                }
            }
        }
    }

    fn tick_background(&mut self, bus: &mut dyn PpuBus) {
        let visible = self.scanline >= 0 && self.scanline < 240;
        let pre = self.scanline == -1;

        // Shift the background registers on active fetch dots.
        if (self.dot >= 2 && self.dot <= 257) || (self.dot >= 322 && self.dot <= 337) {
            self.shift_bg();
        }

        // Background tile fetches happen on dots 1-256 and 321-336 in an
        // 8-dot cycle.
        let fetch = (self.dot >= 1 && self.dot <= 256) || (self.dot >= 321 && self.dot <= 336);
        if fetch {
            match self.dot & 7 {
                1 => self.reload_shifters(),
                2 => {
                    let addr = 0x2000 | (self.v & 0x0FFF);
                    self.nt_byte = self.ppu_read(bus, addr);
                }
                4 => {
                    let addr = 0x23C0
                        | (self.v & 0x0C00)
                        | ((self.v >> 4) & 0x38)
                        | ((self.v >> 2) & 0x07);
                    self.at_byte = self.ppu_read(bus, addr);
                }
                6 => {
                    let fine_y = (self.v >> 12) & 7;
                    let base = if self.ctrl & 0x10 != 0 { 0x1000 } else { 0 };
                    let addr = base + (self.nt_byte as u16) * 16 + fine_y;
                    self.pt_lo = self.ppu_read(bus, addr);
                }
                0 => {
                    let fine_y = (self.v >> 12) & 7;
                    let base = if self.ctrl & 0x10 != 0 { 0x1000 } else { 0 };
                    let addr = base + (self.nt_byte as u16) * 16 + fine_y + 8;
                    self.pt_hi = self.ppu_read(bus, addr);
                    self.increment_coarse_x();
                }
                _ => {}
            }
        }

        if (visible || pre) && self.rendering_enabled() {
            if self.dot == 256 {
                self.increment_y();
            }
            if self.dot == 257 {
                self.copy_x();
            }
            if pre && self.dot >= 280 && self.dot <= 304 {
                self.copy_y();
            }
        }
    }

    #[inline]
    fn shift_bg(&mut self) {
        self.bg_shift_lo <<= 1;
        self.bg_shift_hi <<= 1;
        self.at_shift_lo = (self.at_shift_lo << 1) | self.at_latch_lo as u16;
        self.at_shift_hi = (self.at_shift_hi << 1) | self.at_latch_hi as u16;
    }

    fn reload_shifters(&mut self) {
        self.bg_shift_lo = (self.bg_shift_lo & 0xFF00) | self.pt_lo as u16;
        self.bg_shift_hi = (self.bg_shift_hi & 0xFF00) | self.pt_hi as u16;
        // Pick the 2-bit attribute for this tile from the quadrant.
        let coarse_x = self.v & 0x1F;
        let coarse_y = (self.v >> 5) & 0x1F;
        let shift = ((coarse_y & 2) << 1) | (coarse_x & 2);
        let at = (self.at_byte >> shift) & 0x03;
        self.at_latch_lo = at & 1;
        self.at_latch_hi = (at >> 1) & 1;
    }

    fn increment_coarse_x(&mut self) {
        if (self.v & 0x001F) == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400; // switch horizontal nametable
        } else {
            self.v += 1;
        }
    }

    fn increment_y(&mut self) {
        if (self.v & 0x7000) != 0x7000 {
            self.v += 0x1000; // fine Y++
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800; // switch vertical nametable
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    #[inline]
    fn copy_x(&mut self) {
        self.v = (self.v & !0x041F) | (self.t & 0x041F);
    }
    #[inline]
    fn copy_y(&mut self) {
        self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
    }

    fn render_pixel(&mut self, bus: &mut dyn PpuBus) {
        let px = (self.dot - 1) as usize;
        let py = self.scanline as usize;

        // ---- background pixel ----
        let mut bg_pixel = 0u8;
        let mut bg_pal = 0u8;
        if self.mask & 0x08 != 0 && !(px < 8 && self.mask & 0x02 == 0) {
            let bit = 15 - self.x as u16;
            let lo = ((self.bg_shift_lo >> bit) & 1) as u8;
            let hi = ((self.bg_shift_hi >> bit) & 1) as u8;
            bg_pixel = (hi << 1) | lo;
            let pl = ((self.at_shift_lo >> bit) & 1) as u8;
            let ph = ((self.at_shift_hi >> bit) & 1) as u8;
            bg_pal = (ph << 1) | pl;
        }

        // ---- sprite pixel ----
        let mut sp_pixel = 0u8;
        let mut sp_pal = 0u8;
        let mut sp_priority = false; // true = behind background
        let mut sp_is_zero = false;
        if self.mask & 0x10 != 0 && !(px < 8 && self.mask & 0x04 == 0) {
            for i in 0..self.sprite_count {
                let sx = self.sprite_x[i] as i16;
                let off = px as i16 - sx;
                if off < 0 || off > 7 {
                    continue;
                }
                let attr = self.sprite_attr[i];
                let flip_h = attr & 0x40 != 0;
                let bit = if flip_h { off } else { 7 - off };
                let (lo_b, hi_b) = self.sprite_patterns[i];
                let lo = (lo_b >> bit) & 1;
                let hi = (hi_b >> bit) & 1;
                let pix = (hi << 1) | lo;
                if pix == 0 {
                    continue; // transparent, look at lower-priority sprite
                }
                sp_pixel = pix;
                sp_pal = (attr & 0x03) + 4;
                sp_priority = attr & 0x20 != 0;
                sp_is_zero = self.sprite_is_zero[i];
                break;
            }
        }

        // ---- priority mux + sprite-0 hit ----
        let (pixel, palette) = if bg_pixel == 0 && sp_pixel == 0 {
            (0u8, 0u8)
        } else if bg_pixel == 0 {
            (sp_pixel, sp_pal)
        } else if sp_pixel == 0 {
            (bg_pixel, bg_pal)
        } else {
            // Sprite-0 hit fires whenever an opaque sprite-0 pixel overlaps an
            // opaque background pixel (and not at x=255).
            if sp_is_zero && px != 255 {
                self.status |= 0x40;
            }
            if sp_priority {
                (bg_pixel, bg_pal)
            } else {
                (sp_pixel, sp_pal)
            }
        };

        let pal_addr = if pixel == 0 {
            0x3F00
        } else {
            0x3F00 + (palette as u16) * 4 + pixel as u16
        };
        let color_idx = self.read_palette(pal_addr) & 0x3F;
        let rgb = NES_PALETTE[color_idx as usize];

        let fb = ((py * SCREEN_W) + px) * 4;
        self.framebuffer[fb] = rgb.0;
        self.framebuffer[fb + 1] = rgb.1;
        self.framebuffer[fb + 2] = rgb.2;
        self.framebuffer[fb + 3] = 0xFF;
        let _ = bus;
    }

    fn evaluate_sprites(&mut self, bus: &mut dyn PpuBus) {
        // Build secondary OAM for the NEXT scanline.
        let next_line = self.scanline; // sprites use the line currently finishing
        let sprite_height: i16 = if self.ctrl & 0x20 != 0 { 16 } else { 8 };
        self.secondary_oam = [0xFF; 32];
        self.sprite_count = 0;
        let mut n = 0;
        let mut overflow = false;

        for s in 0..64 {
            let y = self.oam[s * 4] as i16;
            let row = next_line - y;
            if row >= 0 && row < sprite_height {
                if n < 8 {
                    for b in 0..4 {
                        self.secondary_oam[n * 4 + b] = self.oam[s * 4 + b];
                    }
                    self.sprite_is_zero[n] = s == 0;
                    n += 1;
                } else {
                    overflow = true;
                    break;
                }
            }
        }
        if overflow {
            self.status |= 0x20;
        }
        self.sprite_count = n;

        // Fetch pattern bytes for the evaluated sprites.
        for i in 0..n {
            let y = self.secondary_oam[i * 4] as i16;
            let tile = self.secondary_oam[i * 4 + 1];
            let attr = self.secondary_oam[i * 4 + 2];
            let x = self.secondary_oam[i * 4 + 3];
            self.sprite_attr[i] = attr;
            self.sprite_x[i] = x;

            let flip_v = attr & 0x80 != 0;
            let mut row = (next_line - y) as u16;

            let addr = if sprite_height == 16 {
                // 8x16: tile bit0 selects the pattern table; tile&0xFE is base.
                if flip_v {
                    row = 15 - row;
                }
                let table = ((tile & 1) as u16) * 0x1000;
                let base_tile = (tile & 0xFE) as u16;
                let tile_off = if row >= 8 { base_tile + 1 } else { base_tile };
                table + tile_off * 16 + (row & 7)
            } else {
                if flip_v {
                    row = 7 - row;
                }
                let table = if self.ctrl & 0x08 != 0 { 0x1000 } else { 0 };
                table + (tile as u16) * 16 + (row & 7)
            };
            let lo = self.ppu_read(bus, addr);
            let hi = self.ppu_read(bus, addr + 8);
            self.sprite_patterns[i] = (lo, hi);
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer[..]
    }
}

/// The canonical NES master palette (2C02), 64 entries, RGB888. This is the
/// widely-used "FCEUX/Nestopia"-style table.
#[rustfmt::skip]
pub static NES_PALETTE: [(u8, u8, u8); 64] = [
    (0x62,0x62,0x62),(0x00,0x1F,0xB2),(0x24,0x04,0xC8),(0x52,0x00,0xB2),
    (0x73,0x00,0x76),(0x80,0x00,0x24),(0x73,0x0B,0x00),(0x52,0x28,0x00),
    (0x24,0x44,0x00),(0x00,0x57,0x00),(0x00,0x5C,0x00),(0x00,0x53,0x24),
    (0x00,0x3C,0x76),(0x00,0x00,0x00),(0x00,0x00,0x00),(0x00,0x00,0x00),
    (0xAB,0xAB,0xAB),(0x0D,0x57,0xFF),(0x4B,0x30,0xFF),(0x8A,0x13,0xFF),
    (0xBC,0x08,0xD6),(0xD2,0x12,0x69),(0xC7,0x2E,0x00),(0x9D,0x54,0x00),
    (0x60,0x7B,0x00),(0x20,0x98,0x00),(0x00,0xA3,0x00),(0x00,0x99,0x42),
    (0x00,0x7D,0xB4),(0x00,0x00,0x00),(0x00,0x00,0x00),(0x00,0x00,0x00),
    (0xFF,0xFF,0xFF),(0x53,0xAE,0xFF),(0x90,0x85,0xFF),(0xD3,0x65,0xFF),
    (0xFF,0x57,0xFF),(0xFF,0x5D,0xCF),(0xFF,0x77,0x57),(0xFA,0x9E,0x00),
    (0xBD,0xC7,0x00),(0x7A,0xE7,0x00),(0x43,0xF6,0x11),(0x26,0xEF,0x7E),
    (0x2C,0xD5,0xF6),(0x4E,0x4E,0x4E),(0x00,0x00,0x00),(0x00,0x00,0x00),
    (0xFF,0xFF,0xFF),(0xB6,0xE1,0xFF),(0xCE,0xD1,0xFF),(0xE9,0xC3,0xFF),
    (0xFF,0xBC,0xFF),(0xFF,0xBD,0xF4),(0xFF,0xC6,0xC3),(0xFF,0xD5,0x9A),
    (0xE9,0xE6,0x81),(0xCE,0xF4,0x81),(0xB6,0xFB,0x9A),(0xA9,0xFA,0xC3),
    (0xA9,0xF0,0xF4),(0xB8,0xB8,0xB8),(0x00,0x00,0x00),(0x00,0x00,0x00),
];

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyBus {
        chr: [u8; 0x2000],
        mirroring: Mirroring,
    }
    impl PpuBus for DummyBus {
        fn chr_read(&mut self, a: u16) -> u8 {
            self.chr[(a & 0x1FFF) as usize]
        }
        fn chr_write(&mut self, a: u16, v: u8) {
            self.chr[(a & 0x1FFF) as usize] = v;
        }
        fn mirroring(&mut self) -> Mirroring {
            self.mirroring
        }
    }
    fn bus() -> DummyBus {
        DummyBus { chr: [0; 0x2000], mirroring: Mirroring::Horizontal }
    }

    #[test]
    fn ppuaddr_data_write_readback() {
        let mut p = Ppu::new();
        let mut b = bus();
        // Point PPUADDR at $2000 and write a byte, then read it back (with the
        // one-byte read delay).
        p.write_reg(&mut b, 6, 0x20);
        p.write_reg(&mut b, 6, 0x00);
        p.write_reg(&mut b, 7, 0xAB);
        p.write_reg(&mut b, 6, 0x20);
        p.write_reg(&mut b, 6, 0x00);
        let _ = p.read_reg(&mut b, 7); // priming read (buffered)
        let v = p.read_reg(&mut b, 7);
        assert_eq!(v, 0xAB);
    }

    #[test]
    fn vblank_flag_and_clear_on_read() {
        let mut p = Ppu::new();
        let mut b = bus();
        p.scanline = 241;
        p.dot = 1;
        p.step(&mut b);
        // Status VBlank bit set.
        let s = p.read_reg(&mut b, 2);
        assert!(s & 0x80 != 0);
        // Reading cleared it.
        let s2 = p.read_reg(&mut b, 2);
        assert!(s2 & 0x80 == 0);
    }

    #[test]
    fn nmi_signal_when_enabled() {
        let mut p = Ppu::new();
        let mut b = bus();
        p.write_reg(&mut b, 0, 0x80); // enable NMI
        p.scanline = 241;
        p.dot = 1;
        p.step(&mut b);
        assert!(p.nmi_signal);
    }

    #[test]
    fn palette_mirroring() {
        let mut p = Ppu::new();
        p.write_palette(0x3F10, 0x21);
        assert_eq!(p.read_palette(0x3F00), 0x21); // $3F10 mirrors $3F00
    }

    #[test]
    fn frame_advances_after_full_field() {
        let mut p = Ppu::new();
        let mut b = bus();
        let start = p.frame;
        // 262 scanlines × 341 dots.
        for _ in 0..(262 * 341) {
            p.step(&mut b);
        }
        assert_eq!(p.frame, start + 1);
    }
}
