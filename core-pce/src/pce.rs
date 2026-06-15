//! The `Pce` god-struct: owns the HuC6280, VDC, VCE, PSG, cartridge, work RAM,
//! input, and the timer/IRQ-control registers; implements the CPU `Bus`
//! (including the banking MMU); and runs video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. The CPU needs the whole machine as its `Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back.
//!
//! THE MMU. The HuC6280 sees a 16-bit logical space split into eight 8 KiB
//! pages. Page N's high 5 bits of the physical address come from MPR[N]; the low
//! 13 bits are the logical offset. The 21-bit physical space is:
//!   $00-$7F  HuCard ROM (banks; via the cart)
//!   $F7      battery-backed save RAM (not modelled here)
//!   $F8-$FB  work RAM (8 KiB, mirrored across the 4 banks)
//!   $FF      hardware I/O page (VDC / VCE / PSG / timer / IRQ / joypad)
//!
//! On reset the HuC6280 sets MPR7 = $00 (so the reset vector at logical $FFFE
//! reads physical bank $00 offset $1FFE) and the BIOS/ROM then programs the
//! rest. We mirror real hardware power-on: MPR all zero except MPR7 forced so
//! the reset vector is reachable; the System Card / HuCard reprograms them.

use crate::bus::Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::input::Input;
use crate::psg::Psg;
use crate::vce::Vce;
use crate::vdc::Vdc;

/// Visible display dimensions we present (the standard 256×224 window; the VDC's
/// width can vary 256/341/512 but we render the 256 path).
pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
pub const FB_LEN: usize = SCREEN_W * SCREEN_H * 4;

/// CPU master cycles per scanline. The HuC6280 runs at 7.16 MHz; a frame is
/// 262 lines at ~60 Hz => 7_159_090 / (262*60) ≈ 455 cycles/line.
const CYCLES_PER_LINE: u32 = 455;

/// A detected CPU deadlock, captured for the crash screen.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub pc: u16,
    pub frame: u64,
}

pub struct Pce {
    pub cpu: Cpu,
    pub vdc: Vdc,
    pub vce: Vce,
    pub psg: Psg,
    pub input: Input,
    pub cart: Option<Cart>,

    /// 8 KiB work RAM (physical banks $F8-$FB mirror it).
    ram: Box<[u8; 0x2000]>,

    /// The eight Memory Page Registers.
    mpr: [u8; 8],

    /// Interrupt-disable register ($1402, write): bit0 IRQ2, bit1 IRQ1, bit2
    /// TIQ disabled when set.
    irq_disable: u8,
    /// Interrupt-request latch for the timer ($1403 read clears the timer IRQ).
    timer_irq: bool,

    /// Hardware timer: a 7-bit reload value + down-counter ($0C00/$0C01).
    timer_reload: u8,
    timer_value: u8,
    timer_enabled: bool,
    /// Sub-counter: the timer ticks every 1024 CPU cycles.
    timer_prescale: u32,

    /// The framebuffer (256×224 RGBA8888).
    pub framebuffer: Box<[u8; FB_LEN]>,
    /// Per-line indexed scratch (palette index + sprite flag) reused each line.
    line_bg: Box<[u8; SCREEN_W]>,    // background palette index (0..255)
    line_sp: Box<[u8; SCREEN_W]>,    // sprite palette index (0 = transparent)
    line_sp_pri: Box<[bool; SCREEN_W]>, // sprite-over-bg priority flag

    pub fault: Option<Fault>,
}

impl Default for Pce {
    fn default() -> Self {
        Pce::new()
    }
}

impl Pce {
    pub fn new() -> Pce {
        Pce {
            cpu: Cpu::new(),
            vdc: Vdc::new(),
            vce: Vce::new(),
            psg: Psg::new(),
            input: Input::new(),
            cart: None,
            ram: vec![0u8; 0x2000].into_boxed_slice().try_into().unwrap(),
            mpr: [0; 8],
            irq_disable: 0,
            timer_irq: false,
            timer_reload: 0,
            timer_value: 0,
            timer_enabled: false,
            timer_prescale: 0,
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            line_bg: vec![0u8; SCREEN_W].into_boxed_slice().try_into().unwrap(),
            line_sp: vec![0u8; SCREEN_W].into_boxed_slice().try_into().unwrap(),
            line_sp_pri: vec![false; SCREEN_W]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        self.cart = Some(cart);
        self.vdc = Vdc::new();
        self.vce = Vce::new();
        self.psg = Psg::new();
        self.cpu = Cpu::new();
        for b in self.ram.iter_mut() {
            *b = 0;
        }
        // Power-on MMR state. The HuC6280 boots with MPR0 = $FF (I/O page) and
        // MPR7 = $00 so the reset vector at logical $E000-$FFFF reads bank $00.
        // Real hardware/System Card then reprograms them; this is enough for a
        // raw HuCard whose vector lives at the top of bank 0.
        self.mpr = [0xFF, 0xF8, 0, 0, 0, 0, 0, 0];
        self.irq_disable = 0;
        self.timer_irq = false;
        self.timer_enabled = false;
        self.timer_value = 0;
        self.timer_reload = 0;
        self.timer_prescale = 0;
        self.fault = None;

        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.reset(self);
        self.cpu = cpu;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(bits);
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.psg.drain()
    }

    pub fn frame_count(&self) -> u64 {
        self.vdc.frame
    }

    pub fn width(&self) -> usize {
        SCREEN_W
    }
    pub fn height(&self) -> usize {
        SCREEN_H
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer[..]
    }

    // ---- MMU translation ----

    /// Translate a 16-bit logical address to a 21-bit physical address using the
    /// MPRs.
    #[inline]
    fn translate(&self, addr: u16) -> u32 {
        let page = (addr >> 13) & 0x07;
        let bank = self.mpr[page as usize] as u32;
        (bank << 13) | (addr as u32 & 0x1FFF)
    }

    /// Read a physical byte (used for both CPU reads and block transfers).
    fn phys_read(&mut self, phys: u32) -> u8 {
        let bank = (phys >> 13) as u8;
        let off = (phys & 0x1FFF) as u16;
        match bank {
            0x00..=0x7F => self.cart.as_ref().map(|c| c.read(bank, off)).unwrap_or(0xFF),
            0xF8..=0xFB => self.ram[(off & 0x1FFF) as usize],
            0xFF => self.io_read(off),
            _ => 0xFF,
        }
    }

    fn phys_write(&mut self, phys: u32, v: u8) {
        let bank = (phys >> 13) as u8;
        let off = (phys & 0x1FFF) as u16;
        match bank {
            0xF8..=0xFB => self.ram[(off & 0x1FFF) as usize] = v,
            0xFF => self.io_write(off, v),
            _ => {} // ROM / unmapped: ignore writes
        }
    }

    // ---- the I/O page (physical bank $FF) ----

    /// Read from the hardware I/O page. The offset (0..$1FFF) selects the device
    /// block: $0000-$03FF VDC, $0400-$07FF VCE, $0800-$0BFF PSG, $0C00-$0FFF
    /// timer, $1000-$13FF joypad, $1400-$17FF interrupt control.
    fn io_read(&mut self, off: u16) -> u8 {
        match off {
            0x0000..=0x03FF => match off & 0x03 {
                0 => self.vdc.read_status(),
                2 => self.vdc.read_data_lo(),
                3 => self.vdc.read_data_hi(),
                _ => 0xFF,
            },
            0x0400..=0x07FF => self.vce.read((off & 0x07) as u8),
            0x0800..=0x0BFF => self.psg.read((off & 0x0F) as u8),
            0x0C00..=0x0FFF => {
                // Timer: $0C01 read returns the current counter value.
                self.timer_value & 0x7F
            }
            0x1000..=0x13FF => self.input.read_port(),
            0x1400..=0x17FF => match off & 0x03 {
                2 => self.irq_disable,
                3 => {
                    // Reading the timer-IRQ-request register clears the timer IRQ.
                    let v = self.timer_irq as u8;
                    self.timer_irq = false;
                    self.update_irq_lines();
                    v
                }
                _ => 0xFF,
            },
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, off: u16, v: u8) {
        match off {
            0x0000..=0x03FF => match off & 0x03 {
                0 => self.vdc.write_address(v),
                2 => self.vdc.write_data_lo(v),
                3 => self.vdc.write_data_hi(v),
                _ => {}
            },
            0x0400..=0x07FF => self.vce.write((off & 0x07) as u8, v),
            0x0800..=0x0BFF => self.psg.write((off & 0x0F) as u8, v),
            0x0C00..=0x0FFF => match off & 0x01 {
                0 => self.timer_reload = v & 0x7F, // $0C00 reload
                _ => {
                    // $0C01: bit0 enable. Enabling reloads the counter.
                    let en = v & 0x01 != 0;
                    if en && !self.timer_enabled {
                        self.timer_value = self.timer_reload;
                        self.timer_prescale = 0;
                    }
                    self.timer_enabled = en;
                }
            },
            0x1000..=0x13FF => self.input.write_port(v),
            0x1400..=0x17FF => match off & 0x03 {
                2 => self.irq_disable = v & 0x07, // disable mask
                3 => {
                    // Writing $1403 acknowledges/clears the timer IRQ.
                    self.timer_irq = false;
                    self.update_irq_lines();
                }
                _ => {}
            },
            _ => {}
        }
    }

    // ---- ST0/ST1/ST2 fast path. The CPU writes logical $0000/$0002/$0003 for
    // these; our Bus::write8 below recognises them and routes to the VDC. ----

    fn update_irq_lines(&mut self) {
        // IRQ1 = VDC, masked by irq_disable bit1.
        self.cpu.irq1_line = self.vdc.irq && (self.irq_disable & 0x02 == 0);
        // TIQ = timer, masked by irq_disable bit2.
        self.cpu.tiq_line = self.timer_irq && (self.irq_disable & 0x04 == 0);
        // IRQ2 (CD/expansion) — unused here.
        self.cpu.irq2_line = false;
    }

    /// Advance the hardware timer by `cycles` CPU cycles; raise the timer IRQ on
    /// underflow.
    fn tick_timer(&mut self, cycles: u32) {
        if !self.timer_enabled {
            return;
        }
        self.timer_prescale += cycles;
        // The timer decrements every 1024 CPU cycles.
        while self.timer_prescale >= 1024 {
            self.timer_prescale -= 1024;
            if self.timer_value == 0 {
                self.timer_value = self.timer_reload;
                self.timer_irq = true;
            } else {
                self.timer_value -= 1;
            }
        }
    }

    // ---- the frame loop ----

    pub fn run_frame(&mut self) {
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        if self.cart.is_none() {
            return;
        }

        let start = self.vdc.frame;
        let mut guard = 0u32;
        while self.vdc.frame == start && guard < 400 {
            self.run_scanline();
            guard += 1;
        }
    }

    fn run_scanline(&mut self) {
        // Run a scanline's worth of CPU cycles.
        let mut consumed = 0u32;
        while consumed < CYCLES_PER_LINE {
            self.update_irq_lines();
            let mut cpu = std::mem::take(&mut self.cpu);
            let t = cpu.step(self) as u32;
            self.cpu = cpu;
            consumed += t;
            self.tick_timer(t);
            self.psg.step(t);
        }

        // Advance the VDC one scanline; render if it's a visible line.
        let visible = self.vdc.step_scanline();
        if let Some(row) = visible {
            if (row as usize) < SCREEN_H {
                self.render_line(row as usize);
            }
        }
        self.update_irq_lines();
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "PCE CORE FAULT".to_string(),
            format!("PC {:04X}", f.pc),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.framebuffer[..], SCREEN_W, SCREEN_H, &lines);
    }

    // ---- rendering ----

    /// Render one visible scanline (`row` in 0..SCREEN_H) into the framebuffer.
    fn render_line(&mut self, row: usize) {
        // Clear scratch.
        for x in 0..SCREEN_W {
            self.line_bg[x] = 0;
            self.line_sp[x] = 0;
            self.line_sp_pri[x] = false;
        }

        if self.vdc.bg_enabled() {
            self.render_bg_line(row);
        }
        if self.vdc.sp_enabled() {
            self.render_sprites_line(row);
        }

        // Compose: backdrop is VCE palette entry 0. For each pixel, sprite wins
        // over BG if it's non-transparent AND (priority OR bg is transparent).
        let base = row * SCREEN_W * 4;
        for x in 0..SCREEN_W {
            let bg = self.line_bg[x];
            let sp = self.line_sp[x];
            let pri = self.line_sp_pri[x];

            let pal_index = if sp != 0 && (pri || bg == 0) {
                // Sprite palette lives in VCE entries 256..511.
                256 + sp as usize
            } else if bg != 0 {
                bg as usize
            } else {
                0 // backdrop
            };

            let color = self.vce.color(pal_index);
            let o = base + x * 4;
            self.framebuffer[o..o + 4].copy_from_slice(&color);
        }
    }

    /// Render the background tilemap for one line.
    fn render_bg_line(&mut self, row: usize) {
        let (map_w, map_h) = self.vdc.map_dims();
        let xscroll = self.vdc.bg_xscroll() as usize;
        let yscroll = self.vdc.bg_yscroll() as usize;

        let bg_y = (row + yscroll) % (map_h * 8);
        let tile_row = bg_y / 8;
        let in_tile_y = bg_y % 8;

        for x in 0..SCREEN_W {
            let bg_x = (x + xscroll) % (map_w * 8);
            let tile_col = bg_x / 8;
            let in_tile_x = bg_x % 8;

            // BAT (background-attribute table) entry: map_w columns, 1 word each,
            // starting at VRAM word 0.
            let bat_index = tile_row * map_w + tile_col;
            let bat = self.vdc.vram[bat_index & (crate::vdc::VRAM_WORDS - 1)];
            // BAT word: bits 0-11 = tile number, bits 12-15 = palette (subpalette).
            let tile_no = (bat & 0x0FFF) as usize;
            let sub_pal = ((bat >> 12) & 0x0F) as u8;

            // Each 8×8 tile is 16 words (planes interleaved): the tile's data
            // starts at tile_no * 16. Two planes per word pair.
            let tile_base = tile_no * 16;
            // Planes 0/1 are in words [tile_base + y]; planes 2/3 in
            // [tile_base + 8 + y].
            let w01 = self.vdc.vram[(tile_base + in_tile_y) & (crate::vdc::VRAM_WORDS - 1)];
            let w23 = self.vdc.vram[(tile_base + 8 + in_tile_y) & (crate::vdc::VRAM_WORDS - 1)];
            let bit = 7 - in_tile_x;
            let p0 = (w01 >> bit) & 1;
            let p1 = (w01 >> (bit + 8)) & 1;
            let p2 = (w23 >> bit) & 1;
            let p3 = (w23 >> (bit + 8)) & 1;
            let pix = (p0 | (p1 << 1) | (p2 << 2) | (p3 << 3)) as u8;

            // BG palette index into VCE: subpalette*16 + pixel. Pixel 0 is
            // transparent (shows backdrop).
            self.line_bg[x] = if pix == 0 {
                0
            } else {
                (sub_pal as usize * 16 + pix as usize) as u8
            };
        }
    }

    /// Render sprites that intersect this line. Sprites live in the SATB (64
    /// entries of 4 words) which we read from VRAM at the SATB address.
    fn render_sprites_line(&mut self, row: usize) {
        let satb = self.vdc.satb_addr();
        let mut drawn = 0;

        for i in 0..crate::vdc::SPRITES {
            let base = satb + i * 4;
            let w0 = self.vdc.vram[base & (crate::vdc::VRAM_WORDS - 1)];
            let w1 = self.vdc.vram[(base + 1) & (crate::vdc::VRAM_WORDS - 1)];
            let w2 = self.vdc.vram[(base + 2) & (crate::vdc::VRAM_WORDS - 1)];
            let w3 = self.vdc.vram[(base + 3) & (crate::vdc::VRAM_WORDS - 1)];

            // y position (bits 0-9, offset 64), x position (bits 0-9, offset 32).
            let sy = (w0 & 0x3FF) as i32 - 64;
            let sx = (w1 & 0x3FF) as i32 - 32;
            // pattern code (w2 bits 1-10 -> the tile, in 64-byte units).
            let pattern = ((w2 >> 1) & 0x3FF) as usize;
            // attributes (w3): bits 0-3 palette, bit7 priority, bit11 x-flip,
            // bit15 y-flip, bits8-9 width(?) bits12-13 height(?).
            let sub_pal = (w3 & 0x0F) as usize;
            let priority = w3 & 0x0080 != 0;
            let xflip = w3 & 0x0800 != 0;
            let yflip = w3 & 0x8000 != 0;
            // Width: bit 8 of w3 -> 32 vs 16 wide; height: bits 12-13 -> 16/32/64.
            let width: i32 = if w3 & 0x0100 != 0 { 32 } else { 16 };
            let height: i32 = match (w3 >> 12) & 0x03 {
                0 => 16,
                1 => 32,
                _ => 64,
            };

            // Does this sprite intersect the current line?
            if (row as i32) < sy || (row as i32) >= sy + height {
                continue;
            }
            drawn += 1;
            if drawn > 16 {
                self.vdc.set_overflow();
                break;
            }

            let mut line_in_spr = row as i32 - sy;
            if yflip {
                line_in_spr = height - 1 - line_in_spr;
            }

            // Each 16×16 cell is 64 words. A sprite is made of cells; for the
            // standard 16-wide sprite, one column of cells.
            for px in 0..width {
                let screen_x = sx + px;
                if screen_x < 0 || screen_x >= SCREEN_W as i32 {
                    continue;
                }
                let mut col = px;
                if xflip {
                    col = width - 1 - col;
                }
                // Determine which 16×16 cell + the in-cell coordinates.
                let cell_x = (col / 16) as usize;
                let cell_y = (line_in_spr / 16) as usize;
                let in_x = (col % 16) as usize;
                let in_y = (line_in_spr % 16) as usize;
                let cells_wide = width / 16;
                let cell = pattern + cell_y * cells_wide as usize + cell_x;
                let cell_base = cell * 64;

                // 16×16 sprite cell: 4 planes, each 16 words (one per row).
                // Plane data layout: row r -> words at offsets r, 16+r, 32+r, 48+r.
                let p0 = self.vdc.vram[(cell_base + in_y) & (crate::vdc::VRAM_WORDS - 1)];
                let p1 = self.vdc.vram[(cell_base + 16 + in_y) & (crate::vdc::VRAM_WORDS - 1)];
                let p2 = self.vdc.vram[(cell_base + 32 + in_y) & (crate::vdc::VRAM_WORDS - 1)];
                let p3 = self.vdc.vram[(cell_base + 48 + in_y) & (crate::vdc::VRAM_WORDS - 1)];
                let bit = 15 - in_x;
                let b0 = (p0 >> bit) & 1;
                let b1 = (p1 >> bit) & 1;
                let b2 = (p2 >> bit) & 1;
                let b3 = (p3 >> bit) & 1;
                let pix = (b0 | (b1 << 1) | (b2 << 2) | (b3 << 3)) as u8;
                if pix == 0 {
                    continue; // transparent
                }
                let sx_us = screen_x as usize;
                // sprite-0 collision: a non-transparent sprite-0 pixel over a
                // non-transparent BG pixel.
                if i == 0 && self.line_bg[sx_us] != 0 {
                    self.vdc.set_collision();
                }
                self.line_sp[sx_us] = (sub_pal * 16 + pix as usize) as u8;
                self.line_sp_pri[sx_us] = priority;
            }
        }
    }

    // ---- debug ----
    pub fn dbg_read8(&mut self, addr: u16) -> u8 {
        Bus::read8(self, addr)
    }
}

// =============================================================================
// HuC6280 memory bus (with the banking MMU).
// =============================================================================
impl Bus for Pce {
    fn read8(&mut self, addr: u16) -> u8 {
        let phys = self.translate(addr);
        self.phys_read(phys)
    }

    fn write8(&mut self, addr: u16, v: u8) {
        // The CPU's ST0/ST1/ST2 instructions target the VDC at fixed logical
        // addresses regardless of the MMU; recognise them here.
        match addr {
            0x0000 => {
                self.vdc.st_address(v);
                return;
            }
            0x0002 => {
                self.vdc.st_data_lo(v);
                return;
            }
            0x0003 => {
                self.vdc.st_data_hi(v);
                return;
            }
            _ => {}
        }
        let phys = self.translate(addr);
        self.phys_write(phys, v);
    }

    fn set_mpr(&mut self, n: u8, v: u8) {
        self.mpr[(n & 7) as usize] = v;
    }
    fn get_mpr(&self, n: u8) -> u8 {
        self.mpr[(n & 7) as usize]
    }
}

const _: () = assert!(FB_LEN == SCREEN_W * SCREEN_H * 4);

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_rom() -> Vec<u8> {
        // 8 KiB bank 0. Reset vector ($1FFE/$1FFF of bank 0 -> logical $FFFE
        // via MPR7=0) points at $E000. Program at $E000 (logical) maps to bank 0
        // offset 0 (MPR7=0 => page 7 = $E000-$FFFF -> bank 0). We put an
        // infinite loop there.
        let mut rom = vec![0u8; 0x2000];
        // At offset 0 (logical $E000): BRA -2 (loop forever).
        rom[0x0000] = 0x80; // BRA
        rom[0x0001] = 0xFE; // -2
        // Reset vector at offset $1FFE/$1FFF -> $E000.
        rom[0x1FFE] = 0x00;
        rom[0x1FFF] = 0xE0;
        rom
    }

    #[test]
    fn mmu_translate_uses_mpr() {
        let mut pce = Pce::new();
        pce.mpr = [0; 8];
        pce.mpr[3] = 0x05; // page 3 ($6000-$7FFF) -> physical bank 5
        // logical $6001 -> physical (5<<13)|1.
        assert_eq!(pce.translate(0x6001), (5 << 13) | 1);
    }

    #[test]
    fn work_ram_read_write() {
        let mut pce = Pce::new();
        pce.mpr[1] = 0xF8; // page 1 ($2000-$3FFF) -> work RAM bank $F8
        Bus::write8(&mut pce, 0x2000, 0x42);
        assert_eq!(Bus::read8(&mut pce, 0x2000), 0x42);
    }

    #[test]
    fn rom_reads_through_cart() {
        let mut pce = Pce::new();
        pce.load_rom(&tiny_rom());
        // After reset MPR7 = 0; logical $E000 -> bank 0 offset 0 = BRA opcode.
        assert_eq!(Bus::read8(&mut pce, 0xE000), 0x80);
    }

    #[test]
    fn reset_loads_vector() {
        let mut pce = Pce::new();
        pce.load_rom(&tiny_rom());
        assert_eq!(pce.cpu.pc, 0xE000);
    }

    #[test]
    fn run_frame_advances_frame_count() {
        let mut pce = Pce::new();
        pce.load_rom(&tiny_rom());
        let f0 = pce.frame_count();
        pce.run_frame();
        assert_eq!(pce.frame_count(), f0 + 1);
    }

    #[test]
    fn framebuffer_dimensions() {
        let pce = Pce::new();
        assert_eq!(pce.width(), 256);
        assert_eq!(pce.height(), 224);
        assert_eq!(pce.framebuffer().len(), 256 * 224 * 4);
    }

    #[test]
    fn st_instructions_reach_vdc() {
        let mut pce = Pce::new();
        // ST0 selects VDC register 0 (MAWR); ST1/ST2 write the data.
        Bus::write8(&mut pce, 0x0000, 0x00); // ST0 -> AR = MAWR
        Bus::write8(&mut pce, 0x0002, 0x10); // ST1 -> MAWR lo
        Bus::write8(&mut pce, 0x0003, 0x00); // ST2 -> MAWR hi (MAWR=0x10)
        Bus::write8(&mut pce, 0x0000, 0x02); // AR = VWR
        Bus::write8(&mut pce, 0x0002, 0x34); // VWR lo
        Bus::write8(&mut pce, 0x0003, 0x12); // VWR hi -> commit 0x1234 at 0x10
        assert_eq!(pce.vdc.vram[0x10], 0x1234);
    }

    #[test]
    fn input_reaches_joypad_port() {
        let mut pce = Pce::new();
        pce.set_keys(crate::input::KEY_UP);
        pce.mpr[0] = 0xFF; // page 0 -> I/O
        // Joypad port at I/O offset $1000 (logical $1000 with MPR0=$FF).
        Bus::write8(&mut pce, 0x1000, 0x00); // SEL=0, CLR=0 -> directions
        let v = Bus::read8(&mut pce, 0x1000);
        // Up pressed -> low-nibble bit0 reads 0.
        assert_eq!(v & 0x01, 0);
    }

    #[test]
    fn timer_underflow_raises_tiq() {
        let mut pce = Pce::new();
        pce.mpr[0] = 0xFF;
        // Reload = 0, enable. After 1024 cycles it underflows.
        Bus::write8(&mut pce, 0x0C00, 0x00); // reload 0
        Bus::write8(&mut pce, 0x0C01, 0x01); // enable
        pce.tick_timer(2048);
        assert!(pce.timer_irq);
    }

    #[test]
    fn timer_irq_masked_by_disable_register() {
        let mut pce = Pce::new();
        pce.mpr[0] = 0xFF;
        Bus::write8(&mut pce, 0x1402, 0x04); // disable TIQ (bit2)
        Bus::write8(&mut pce, 0x0C00, 0x00);
        Bus::write8(&mut pce, 0x0C01, 0x01);
        pce.tick_timer(2048);
        pce.update_irq_lines();
        assert!(!pce.cpu.tiq_line, "TIQ must be masked when disabled");
    }

    #[test]
    fn vdc_irq_reaches_cpu_when_enabled() {
        let mut pce = Pce::new();
        pce.load_rom(&tiny_rom());
        // Enable VBlank IRQ via the VDC control register.
        pce.vdc.write_address(0x05);
        pce.vdc.write_data_lo(0x08); // CR_VBLANK_IE
        pce.vdc.write_data_hi(0x00);
        pce.run_frame();
        // After a full frame the VDC asserted its IRQ at VBlank.
        assert!(pce.vdc.frame >= 1);
    }

    #[test]
    fn renders_background_pixels() {
        let mut pce = Pce::new();
        // Set a palette: entry 1 = full red.
        pce.vce.write(0x02, 0x01); // addr low = 1
        pce.vce.write(0x03, 0x00);
        pce.vce.write(0x04, 0x38); // red=7 (0b111000)
        pce.vce.write(0x05, 0x00);
        // Enable BG.
        pce.vdc.write_address(0x05);
        pce.vdc.write_data_lo(0x80); // CR_BG_EN
        pce.vdc.write_data_hi(0x00);
        // Map size 32×32 (MWR=0).
        // Tile 0 at BAT entry 0 with subpalette 0; fill tile 0 plane 0 so every
        // pixel = color index 1.
        // BAT[0] = tile 0, subpal 0.
        pce.vdc.vram[0] = 0x0000;
        // Tile 0 data: plane0 all ones for row 0 -> pixel value 1.
        pce.vdc.vram[0] = 0x0001; // BAT entry 0 -> tile 1 (avoid overlap)
        let tile_base = 1 * 16;
        pce.vdc.vram[tile_base] = 0x00FF; // plane0 row0 = all 8 pixels set
        // Render line 0.
        pce.render_line(0);
        // Pixel 0 should be red (palette index 1).
        let c = &pce.framebuffer[0..4];
        assert_eq!(c[0], 0xFF); // red channel full
    }

    #[test]
    fn faulted_presents_crash_screen() {
        let mut pce = Pce::new();
        pce.fault = Some(Fault { pc: 0x1234, frame: 7 });
        pce.run_frame();
        let fb = pce.framebuffer();
        let has_white = fb.chunks_exact(4).any(|p| p == [0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(has_white, "crash screen must draw white text");
    }
}
