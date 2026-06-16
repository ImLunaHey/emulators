//! The SMS VDP (a TMS9918-derived video chip) in Mode 4 — the only mode SMS
//! and Game Gear games use. Built from the SMS Power! "VDP" documentation.
//!
//! Hardware:
//!   * 16 KiB VRAM, addressed via an auto-incrementing 14-bit pointer set
//!     through the control port ($BF).
//!   * 32 bytes of CRAM (colour RAM / palette). SMS: 32 entries × 1 byte of
//!     6-bit colour (--BBGGRR). Game Gear: 32 entries × 2 bytes of 12-bit
//!     colour (----BBBBGGGGRRRR, little-endian).
//!   * 16 write-only registers (R0..R10 are meaningful) set via the control
//!     port with a 0x80|reg high byte.
//!   * A name table (32×28 tilemap of 2-byte entries) + 256 patterns (8×8,
//!     4bpp planar). Up to 8 sprites per scanline; overflow + collision flags.
//!   * A line counter that raises a line interrupt, and a frame (VBlank)
//!     interrupt at the start of the active-area-end.
//!
//! Timing model: NTSC, 262 scanlines/frame, 256 active pixels/line. We render
//! one scanline at a time (`step_scanline`) and the host clocks it from the
//! `Sms` frame loop. RGBA8888 output.

/// Full SMS framebuffer: 256×192 visible (we render lines 0..192). Game Gear
/// crops a 160×144 window out of this in `Sms::framebuffer`.
pub const SMS_W: usize = 256;
pub const SMS_H: usize = 192;
pub const FB_LEN: usize = SMS_W * SMS_H * 4;

/// Game Gear visible window (centred crop of the SMS frame).
pub const GG_W: usize = 160;
pub const GG_H: usize = 144;
/// Top-left of the GG window inside the SMS 256×192 frame.
pub const GG_X: usize = (SMS_W - GG_W) / 2; // 48
pub const GG_Y: usize = (SMS_H - GG_H) / 2; // 24

pub const SCANLINES: u16 = 262; // NTSC
pub const ACTIVE_LINES: u16 = 192;

pub struct Vdp {
    pub vram: Box<[u8; 0x4000]>,
    /// Palette RAM. 32 bytes on SMS; 64 bytes (32×2) on GG. We store 64 and
    /// only use the first 32 on SMS.
    pub cram: Box<[u8; 64]>,
    pub regs: [u8; 16],

    /// 14-bit VRAM address pointer (auto-increments after each data access).
    addr: u16,
    /// Control-port write latches the low byte first, then high byte + code.
    latch: u8,
    second_byte: bool,
    /// Address code: 0/1 = VRAM read/write, 2 = register write, 3 = CRAM write.
    code: u8,
    /// Read buffer for the VRAM read prefetch.
    read_buffer: u8,

    /// Status flags: bit7 frame INT, bit6 sprite overflow, bit5 sprite
    /// collision. Cleared on a status-port read.
    status: u8,

    /// Line interrupt counter (reloaded from R10).
    line_counter: u8,
    /// True while line-interrupt request is pending.
    line_irq: bool,
    /// True while frame (VBlank) interrupt request is pending.
    frame_irq: bool,

    pub vcounter: u16,

    /// Latched fine horizontal scroll for the current line (R8 is sampled per
    /// line so mid-frame writes only affect subsequent lines).
    pub framebuffer: Box<[u8; FB_LEN]>,
    pub frame: u64,

    /// Game Gear: 12-bit CRAM needs a one-byte latch for the low byte of a
    /// 2-byte palette write.
    is_gg: bool,
    gg_cram_latch: u8,
}

impl Vdp {
    pub fn new(is_gg: bool) -> Vdp {
        Vdp {
            vram: vec![0u8; 0x4000].into_boxed_slice().try_into().unwrap(),
            cram: vec![0u8; 64].into_boxed_slice().try_into().unwrap(),
            regs: [0; 16],
            addr: 0,
            latch: 0,
            second_byte: false,
            code: 0,
            read_buffer: 0,
            status: 0,
            line_counter: 0,
            line_irq: false,
            frame_irq: false,
            vcounter: 0,
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            frame: 0,
            is_gg,
            gg_cram_latch: 0,
        }
    }

    // ---- register helpers ----
    #[inline]
    fn display_enabled(&self) -> bool {
        self.regs[1] & 0x40 != 0
    }
    #[inline]
    fn frame_irq_enabled(&self) -> bool {
        self.regs[1] & 0x20 != 0
    }
    #[inline]
    fn line_irq_enabled(&self) -> bool {
        self.regs[0] & 0x10 != 0
    }
    /// Name table base address: R2 bits 1-3 select bits 11-13. On SMS the
    /// standard layout uses R2 & 0x0E << 10.
    #[inline]
    fn name_table_base(&self) -> u16 {
        ((self.regs[2] as u16 & 0x0E) << 10) & 0x3FFF
    }
    /// Sprite attribute table base: R5 bits 1-6 -> bits 8-13.
    #[inline]
    fn sprite_table_base(&self) -> u16 {
        ((self.regs[5] as u16 & 0x7E) << 7) & 0x3FFF
    }
    /// Sprite pattern generator base: R6 bit 2 selects bit 13 (0 or 0x2000).
    #[inline]
    fn sprite_pattern_base(&self) -> u16 {
        if self.regs[6] & 0x04 != 0 {
            0x2000
        } else {
            0
        }
    }

    // =====================================================================
    // CPU-facing ports.
    //   $7E/$7F: V/H counter (read) and PSG (write — handled in io.rs)
    //   $BE: VDP data port
    //   $BF: VDP control port
    // =====================================================================

    /// Read the data port ($BE): returns the read buffer and prefetches the
    /// next VRAM byte. Always resets the control latch.
    pub fn read_data(&mut self) -> u8 {
        self.second_byte = false;
        let v = self.read_buffer;
        self.read_buffer = self.vram[(self.addr & 0x3FFF) as usize];
        self.addr = (self.addr + 1) & 0x3FFF;
        v
    }

    /// Write the data port ($BE): stores to VRAM or CRAM depending on `code`.
    pub fn write_data(&mut self, v: u8) {
        self.second_byte = false;
        match self.code {
            3 => self.write_cram(v),
            _ => {
                self.vram[(self.addr & 0x3FFF) as usize] = v;
            }
        }
        self.read_buffer = v;
        self.addr = (self.addr + 1) & 0x3FFF;
    }

    fn write_cram(&mut self, v: u8) {
        if self.is_gg {
            // GG: 2 bytes per entry. Even address latches; odd address commits.
            let idx = (self.addr & 0x3F) as usize;
            if idx & 1 == 0 {
                self.gg_cram_latch = v;
            } else {
                self.cram[idx - 1] = self.gg_cram_latch;
                self.cram[idx] = v;
            }
        } else {
            self.cram[(self.addr & 0x1F) as usize] = v;
        }
    }

    /// Write the control port ($BF). First byte latches; second byte applies
    /// the code/address (or a register write for code 2).
    pub fn write_control(&mut self, v: u8) {
        if !self.second_byte {
            self.latch = v;
            self.second_byte = true;
        } else {
            self.second_byte = false;
            self.code = v >> 6;
            self.addr = (((v as u16) & 0x3F) << 8) | self.latch as u16;
            match self.code {
                0 => {
                    // VRAM read: prefetch the first byte.
                    self.read_buffer = self.vram[(self.addr & 0x3FFF) as usize];
                    self.addr = (self.addr + 1) & 0x3FFF;
                }
                2 => {
                    // Register write: the latched first byte is the value, the
                    // low 4 bits of the second byte select the register.
                    let regnum = (v & 0x0F) as usize;
                    self.regs[regnum] = self.latch;
                }
                _ => {}
            }
        }
    }

    /// Read the status/control port ($BF): returns the status byte and clears
    /// the frame/overflow/collision flags + the line/frame IRQ latches + the
    /// control latch.
    pub fn read_status(&mut self) -> u8 {
        let v = self.status | 0x1F; // unused low bits read as 1
        self.status = 0;
        self.frame_irq = false;
        self.line_irq = false;
        self.second_byte = false;
        v
    }

    /// True while the VDP is asserting the Z80 INT line (frame or line IRQ that
    /// is enabled).
    pub fn irq_asserted(&self) -> bool {
        (self.frame_irq && self.frame_irq_enabled())
            || (self.line_irq && self.line_irq_enabled())
    }

    /// The V counter value the CPU reads from $7E. SMS returns a folded value
    /// in the VBlank region; we return the raw active-line counter (good enough
    /// for raster effects that compare against a threshold).
    pub fn v_counter(&self) -> u8 {
        // NTSC fold: 0x00..0xDA, then jumps 0xD5..0xFF in the blanking region.
        let l = self.vcounter;
        if l <= 0xDA {
            l as u8
        } else {
            (l - 6) as u8
        }
    }

    /// Advance the VDP by one scanline. Renders the active line if it's in the
    /// visible region. Returns nothing; the host polls `irq_asserted`.
    pub fn step_scanline(&mut self) {
        let line = self.vcounter;

        // Active display lines: render + clock the line-interrupt counter.
        if line < ACTIVE_LINES {
            if self.display_enabled() {
                self.render_line(line as usize);
            } else {
                self.clear_line(line as usize);
            }
        }

        // Line interrupt counter: runs across the active area plus one extra
        // line past it (line == ACTIVE_LINES), reloading from R10 below.
        if line <= ACTIVE_LINES {
            if self.line_counter == 0 {
                self.line_counter = self.regs[10];
                self.line_irq = true;
            } else {
                self.line_counter = self.line_counter.wrapping_sub(1);
            }
        } else {
            self.line_counter = self.regs[10];
        }

        // Frame interrupt fires when entering line 192 (start of VBlank).
        if line == ACTIVE_LINES {
            self.status |= 0x80;
            self.frame_irq = true;
        }

        self.vcounter += 1;
        if self.vcounter >= SCANLINES {
            self.vcounter = 0;
            self.frame += 1;
        }
    }

    fn clear_line(&mut self, y: usize) {
        let bg = self.backdrop_rgba();
        let base = y * SMS_W * 4;
        for x in 0..SMS_W {
            let o = base + x * 4;
            self.framebuffer[o] = bg.0;
            self.framebuffer[o + 1] = bg.1;
            self.framebuffer[o + 2] = bg.2;
            self.framebuffer[o + 3] = 0xFF;
        }
    }

    /// Backdrop colour = sprite-palette entry selected by R7's low 4 bits.
    fn backdrop_rgba(&self) -> (u8, u8, u8) {
        let idx = 16 + (self.regs[7] & 0x0F) as usize;
        self.cram_rgba(idx)
    }

    /// Decode a CRAM entry to RGB. Handles SMS 6-bit and GG 12-bit formats.
    fn cram_rgba(&self, index: usize) -> (u8, u8, u8) {
        if self.is_gg {
            let lo = self.cram[(index * 2) & 0x3F];
            let hi = self.cram[((index * 2) + 1) & 0x3F];
            // ----BBBBGGGGRRRR
            let r = (lo & 0x0F) as u16;
            let g = ((lo >> 4) & 0x0F) as u16;
            let b = (hi & 0x0F) as u16;
            // 4-bit -> 8-bit: replicate the nibble.
            (
                ((r << 4) | r) as u8,
                ((g << 4) | g) as u8,
                ((b << 4) | b) as u8,
            )
        } else {
            let c = self.cram[index & 0x1F];
            // --BBGGRR (2 bits/channel)
            let r = (c & 0x03) as u16;
            let g = ((c >> 2) & 0x03) as u16;
            let b = ((c >> 4) & 0x03) as u16;
            // 2-bit -> 8-bit: ×85.
            ((r * 85) as u8, (g * 85) as u8, (b * 85) as u8)
        }
    }

    fn put_pixel(&mut self, x: usize, y: usize, rgb: (u8, u8, u8)) {
        let o = (y * SMS_W + x) * 4;
        self.framebuffer[o] = rgb.0;
        self.framebuffer[o + 1] = rgb.1;
        self.framebuffer[o + 2] = rgb.2;
        self.framebuffer[o + 3] = 0xFF;
    }

    // =====================================================================
    // Mode 4 scanline renderer.
    // =====================================================================
    fn render_line(&mut self, y: usize) {
        // Per-line: 0 = colour index (with palette select), and a "behind"
        // priority bit + an "opaque" mask for sprite collision.
        let mut bg_color = [0u8; SMS_W]; // final palette index per pixel
        let mut bg_priority = [false; SMS_W];
        let mut bg_opaque = [false; SMS_W];

        // ---- background ----
        let v_scroll = self.regs[9];
        // R0 bit6: when set, the top two rows (lines 0..15) ignore H scroll.
        let h_scroll_disabled_top = self.regs[0] & 0x40 != 0 && y < 16;
        // R0 bit7: when set, the right 8 columns ignore V scroll (handled per
        // column below).
        let v_scroll_lock_right = self.regs[0] & 0x80 != 0;

        let fine_x = if h_scroll_disabled_top {
            0
        } else {
            self.regs[8] & 7
        };
        let coarse_x = if h_scroll_disabled_top {
            0
        } else {
            (self.regs[8] >> 3) & 0x1F
        };

        let name_base = self.name_table_base();

        for screen_x in 0..SMS_W {
            // V scroll lock on the rightmost 8 columns (columns 24..31).
            let col_locked = v_scroll_lock_right && screen_x >= 192;
            let eff_vscroll = if col_locked { 0 } else { v_scroll };
            let map_y = (y as u16 + eff_vscroll as u16) % 224;
            let tile_row = (map_y / 8) % 28;
            let fine_y = (map_y % 8) as usize;

            // Horizontal: scroll shifts the screen right, so subtract.
            let sx = (screen_x as i32 - fine_x as i32) as i32;
            let tile_screen_col = (sx >> 3) as i32;
            let fine_x_pix = (sx & 7) as usize;
            let map_col =
                (((tile_screen_col - coarse_x as i32) as i32) & 0x1F) as u16;

            let entry_addr =
                (name_base + (tile_row * 32 + map_col) * 2) & 0x3FFF;
            let lo = self.vram[entry_addr as usize];
            let hi = self.vram[(entry_addr + 1) as usize] as u16;
            let tile_index = ((hi & 1) << 8) | lo as u16;
            let h_flip = hi & 0x02 != 0;
            let v_flip = hi & 0x04 != 0;
            let palette_sel = ((hi >> 3) & 1) as u8; // 0 = first 16, 1 = sprite
            let priority = hi & 0x10 != 0;

            let row = if v_flip { 7 - fine_y } else { fine_y };
            let col = if h_flip { 7 - fine_x_pix } else { fine_x_pix };

            let color = self.fetch_pattern_pixel(tile_index, row, col);
            let pal_index = (palette_sel * 16) + color;
            bg_color[screen_x] = pal_index;
            bg_priority[screen_x] = priority && color != 0;
            bg_opaque[screen_x] = color != 0;
        }

        // ---- sprites ----
        let mut sprite_color = [0u8; SMS_W];
        let mut sprite_drawn = [false; SMS_W];
        let sprite_height = if self.regs[1] & 0x02 != 0 { 16 } else { 8 };
        let sprite_zoom = self.regs[1] & 0x01 != 0;
        let eff_height = if sprite_zoom {
            sprite_height * 2
        } else {
            sprite_height
        };
        let sat = self.sprite_table_base();
        let pat_base = self.sprite_pattern_base();

        let mut count_on_line = 0;
        for i in 0..64usize {
            let sy = self.vram[(sat + i as u16) as usize] as i32;
            // Y == 0xD0 terminates the sprite list (in 192-line mode).
            if sy == 0xD0 {
                break;
            }
            let sprite_y = (sy + 1) & 0xFF; // sprite Y is stored minus 1
            let dy = y as i32 - sprite_y;
            if dy < 0 || dy >= eff_height as i32 {
                continue;
            }
            count_on_line += 1;
            if count_on_line > 8 {
                self.status |= 0x40; // sprite overflow
                break;
            }
            let sx = self.vram[(sat + 0x80 + i as u16 * 2) as usize] as i32;
            let mut tile = self.vram[(sat + 0x80 + i as u16 * 2 + 1) as usize] as u16;
            // R0 bit3: shift sprites left by 8 pixels.
            let sx = if self.regs[0] & 0x08 != 0 { sx - 8 } else { sx };

            let mut row = if sprite_zoom { dy / 2 } else { dy } as usize;
            if sprite_height == 16 {
                // 8×16 sprites: low bit of tile index is ignored; row selects.
                tile &= 0xFE;
                if row >= 8 {
                    tile |= 1;
                    row -= 8;
                }
            }
            let tile_addr_base = pat_base / 32; // tile-index space within bank
            let _ = tile_addr_base;

            for px in 0..8usize {
                let span = if sprite_zoom { 2 } else { 1 };
                for s in 0..span {
                    let screen_x = sx + (px * span + s) as i32;
                    if screen_x < 0 || screen_x >= SMS_W as i32 {
                        continue;
                    }
                    let sxu = screen_x as usize;
                    if sprite_drawn[sxu] {
                        // Collision: two sprites overlap.
                        self.status |= 0x20;
                        continue;
                    }
                    let color = self.fetch_sprite_pixel(pat_base, tile, row, px);
                    if color == 0 {
                        continue;
                    }
                    sprite_color[sxu] = 16 + color; // sprites use second palette
                    sprite_drawn[sxu] = true;
                }
            }
        }

        // ---- compose ----
        for x in 0..SMS_W {
            let mut idx = bg_color[x];
            // R0 bit5: blank the leftmost 8 pixels (mask column).
            let masked = (self.regs[0] & 0x20 != 0) && x < 8;
            if masked {
                idx = 16 + (self.regs[7] & 0x0F);
            } else if sprite_drawn[x] && !bg_priority[x] {
                idx = sprite_color[x];
            }
            let rgb = self.cram_rgba(idx as usize);
            self.put_pixel(x, y, rgb);
        }
        let _ = bg_opaque;
    }

    /// Fetch a 4bpp planar pixel from a background pattern.
    #[inline]
    fn fetch_pattern_pixel(&self, tile: u16, row: usize, col: usize) -> u8 {
        let addr = (tile as usize * 32 + row * 4) & 0x3FFF;
        let b0 = self.vram[addr];
        let b1 = self.vram[(addr + 1) & 0x3FFF];
        let b2 = self.vram[(addr + 2) & 0x3FFF];
        let b3 = self.vram[(addr + 3) & 0x3FFF];
        let bit = 7 - col;
        ((b0 >> bit) & 1)
            | (((b1 >> bit) & 1) << 1)
            | (((b2 >> bit) & 1) << 2)
            | (((b3 >> bit) & 1) << 3)
    }

    #[inline]
    fn fetch_sprite_pixel(&self, base: u16, tile: u16, row: usize, col: usize) -> u8 {
        let addr = (base as usize + tile as usize * 32 + row * 4) & 0x3FFF;
        let b0 = self.vram[addr];
        let b1 = self.vram[(addr + 1) & 0x3FFF];
        let b2 = self.vram[(addr + 2) & 0x3FFF];
        let b3 = self.vram[(addr + 3) & 0x3FFF];
        let bit = 7 - col;
        ((b0 >> bit) & 1)
            | (((b1 >> bit) & 1) << 1)
            | (((b2 >> bit) & 1) << 2)
            | (((b3 >> bit) & 1) << 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_reg(v: &mut Vdp, reg: u8, val: u8) {
        v.write_control(val);
        v.write_control(0x80 | reg);
    }

    #[test]
    fn register_write() {
        let mut v = Vdp::new(false);
        write_reg(&mut v, 1, 0x60); // display + frame IRQ on
        assert_eq!(v.regs[1], 0x60);
        assert!(v.display_enabled());
        assert!(v.frame_irq_enabled());
    }

    #[test]
    fn vram_write_read_autoincrement() {
        let mut v = Vdp::new(false);
        // Set address to 0x0000 for write (code 1).
        v.write_control(0x00);
        v.write_control(0x40); // code=1, addr=0
        v.write_data(0xAB);
        v.write_data(0xCD);
        assert_eq!(v.vram[0], 0xAB);
        assert_eq!(v.vram[1], 0xCD);
        // Read back (code 0): prefetch + buffer.
        v.write_control(0x00);
        v.write_control(0x00); // code=0, addr=0
        assert_eq!(v.read_data(), 0xAB);
        assert_eq!(v.read_data(), 0xCD);
    }

    #[test]
    fn cram_write_sms() {
        let mut v = Vdp::new(false);
        v.write_control(0x00);
        v.write_control(0xC0); // code=3 (CRAM), addr=0
        v.write_data(0x3F); // white-ish (all 6 bits)
        assert_eq!(v.cram[0], 0x3F);
        let rgb = v.cram_rgba(0);
        assert_eq!(rgb, (255, 255, 255));
    }

    #[test]
    fn cram_write_gg_12bit() {
        let mut v = Vdp::new(true);
        v.write_control(0x00);
        v.write_control(0xC0); // code=3, addr=0
        v.write_data(0xF0); // lo: R=0, G=F
        v.write_data(0x0F); // hi: B=F  -> commits entry 0
        assert_eq!(v.cram[0], 0xF0);
        assert_eq!(v.cram[1], 0x0F);
        let rgb = v.cram_rgba(0);
        assert_eq!(rgb, (0, 255, 255));
    }

    #[test]
    fn frame_interrupt_at_line_192() {
        let mut v = Vdp::new(false);
        write_reg(&mut v, 1, 0x20); // frame IRQ enabled
        // Step to line 192.
        for _ in 0..193 {
            v.step_scanline();
        }
        assert!(v.frame_irq);
        assert!(v.irq_asserted());
        // Reading status clears it.
        let s = v.read_status();
        assert!(s & 0x80 != 0);
        assert!(!v.frame_irq);
    }

    #[test]
    fn line_interrupt_fires() {
        let mut v = Vdp::new(false);
        write_reg(&mut v, 0, 0x10); // line IRQ enabled
        write_reg(&mut v, 10, 1); // line counter reload = 1 (every 2 lines)
        let mut fired = 0;
        for _ in 0..10 {
            v.step_scanline();
            if v.line_irq {
                fired += 1;
                v.read_status();
            }
        }
        assert!(fired > 0);
    }

    #[test]
    fn frame_count_advances() {
        let mut v = Vdp::new(false);
        let f0 = v.frame;
        for _ in 0..SCANLINES {
            v.step_scanline();
        }
        assert_eq!(v.frame, f0 + 1);
    }

    #[test]
    fn renders_a_solid_tile() {
        let mut v = Vdp::new(false);
        write_reg(&mut v, 1, 0x40); // display on
        write_reg(&mut v, 2, 0x0E); // name table at 0x3800
        // Palette entry 1 = pure red (R=3).
        v.write_control(0x00);
        v.write_control(0xC0); // CRAM addr 0
        v.write_data(0x00); // entry 0 backdrop
        v.write_data(0x03); // entry 1 = red
        // Tile 1: fill all 4 bitplanes' bit so color = 1 everywhere? We want
        // color index 1 -> only plane0 set. Pattern at tile 1 = addr 32.
        for r in 0..8 {
            v.vram[32 + r * 4] = 0xFF; // plane0 all set -> color bit0=1
        }
        // Name table entry (0,0) -> tile 1.
        let nt = 0x3800;
        v.vram[nt] = 0x01;
        v.vram[nt + 1] = 0x00;
        v.render_line(0);
        // Pixel (0,0) should be red.
        let o = 0;
        assert_eq!(v.framebuffer[o], 255); // R
        assert_eq!(v.framebuffer[o + 1], 0);
    }
}
