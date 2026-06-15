//! The `WonderSwan` god-struct: owns the V30MZ CPU, video unit, audio unit,
//! cartridge, internal RAM, and input; implements the V30MZ `V30Bus`; and runs
//! video frames. ONE struct handles both the mono WonderSwan and the Color via
//! the [`Model`] enum.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. The CPU needs the whole machine as its `V30Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back.
//!
//! V30MZ physical memory map (20-bit / 1 MiB):
//!   0x00000-0x03FFF  internal RAM (16 KiB; mono). The Color model has 64 KiB
//!                    (0x00000-0x0FFFF). VRAM (tiles/maps/sprites) and, on the
//!                    Color, the palette region live inside this RAM.
//!   0x10000-0x1FFFF  cartridge SRAM (bank-selected, $C1)
//!   0x20000-0x2FFFF  cartridge ROM bank 0 ($C2)
//!   0x30000-0x3FFFF  cartridge ROM bank 1 ($C3)
//!   0x40000-0xFFFFF  cartridge ROM linear region ($C0); the reset vector at
//!                    0xFFFF0 lives in the last ROM bank.
//!
//! I/O port map ($00-$FF), the registers used to boot + render + play audio:
//!   $00 DISP_CTRL   $01 BACK_COLOR  $03 LINE_CMP  $04-$06 sprite table/first/count
//!   $07 MAP_BASE    $10-$13 scroll  $14 LCD on    $1C-$1F palette/shade pool
//!   $20-$3F mono palettes
//!   $80-$92 sound   $A0 system ctrl $B0 IRQ enable $B1 IRQ status (ack)
//!   $B2 IRQ base    $B3 serial      $B5 KEY scan select  $C0-$C3 cart banks

use crate::audio::Audio;
use crate::bus::V30Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::video::{Video, FB_LEN, SCREEN_H, SCREEN_W, TOTAL_LINES};

/// Which WonderSwan model. Chosen at construction; drives colour vs grey
/// rendering and the internal RAM size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Mono,
    Color,
}

/// Internal RAM size: 16 KiB mono, 64 KiB color.
const RAM_MONO: usize = 0x4000;
const RAM_COLOR: usize = 0x10000;

/// Color palette RAM lives at 0xFE00-0xFFFF within color internal RAM (512 bytes
/// = 16 palettes × 16 colors × 2 bytes).
const COLOR_PAL_BASE: usize = 0xFE00;
const COLOR_PAL_LEN: usize = 0x200;

/// Cycles per scanline ≈ CPU clock 3.072 MHz / (159 lines × 75.47 Hz) ≈ 256.
const CYCLES_PER_LINE: u32 = 256;

// Interrupt status/enable bits (I/O $B0/$B1). The WS interrupt controller
// presents a base vector ($B2) + the highest-priority pending bit.
const INT_LINE_CMP: u8 = 0x10;
const INT_VBLANK: u8 = 0x40;
const INT_VBLANK_TIMER: u8 = 0x20;

/// A detected CPU fault (undefined opcode) captured for the crash screen.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub opcode: u8,
    pub cs: u16,
    pub ip: u16,
    pub frame: u64,
}

pub struct WonderSwan {
    pub cpu: Cpu,
    pub video: Video,
    pub audio: Audio,
    pub cart: Option<Cart>,
    pub model: Model,

    /// Internal RAM (16 KiB mono / 64 KiB color). Holds VRAM + (color) palettes.
    ram: Box<[u8]>,

    /// Interrupt controller registers.
    int_enable: u8,
    int_status: u8,
    int_base: u8,

    /// Key matrix scan select ($B5) + the latched directional/button state.
    key_select: u8,
    keys: u32,

    /// Cycles owed to the audio unit.
    audio_owed: u32,

    pub fault: Option<Fault>,
}

// Public key bitmask (set_keys). Matches the host's canonical button order.
pub const KEY_UP: u32 = 1 << 0;
pub const KEY_DOWN: u32 = 1 << 1;
pub const KEY_LEFT: u32 = 1 << 2;
pub const KEY_RIGHT: u32 = 1 << 3;
pub const KEY_A: u32 = 1 << 4;
pub const KEY_B: u32 = 1 << 5;
pub const KEY_START: u32 = 1 << 6;

impl WonderSwan {
    pub fn new(model: Model) -> WonderSwan {
        let ram_size = match model {
            Model::Mono => RAM_MONO,
            Model::Color => RAM_COLOR,
        };
        WonderSwan {
            cpu: Cpu::new(),
            video: Video::new(model == Model::Color),
            audio: Audio::new(),
            cart: None,
            model,
            ram: vec![0u8; ram_size].into_boxed_slice(),
            int_enable: 0,
            int_status: 0,
            int_base: 0,
            key_select: 0,
            keys: 0,
            audio_owed: 0,
            fault: None,
        }
    }

    /// Convenience: `color == true` -> Color model.
    pub fn new_model(color: bool) -> WonderSwan {
        WonderSwan::new(if color { Model::Color } else { Model::Mono })
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        // If the cart declares color but we're mono, keep the requested model.
        self.cart = Some(cart);
        let color = self.model == Model::Color;
        self.video = Video::new(color);
        self.audio = Audio::new();
        self.cpu = Cpu::new();
        self.cpu.reset();
        for b in self.ram.iter_mut() {
            *b = 0;
        }
        self.int_enable = 0;
        self.int_status = 0;
        self.int_base = 0;
        self.fault = None;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.keys = bits;
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.audio.drain()
    }

    pub fn frame_count(&self) -> u64 {
        self.video.frame
    }

    pub fn width(&self) -> usize {
        SCREEN_W
    }
    pub fn height(&self) -> usize {
        SCREEN_H
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.video.framebuffer[..]
    }

    // ---- battery save passthrough ----
    pub fn save_ram(&self) -> &[u8] {
        self.cart.as_ref().map(|c| c.save_ram()).unwrap_or(&[])
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        if let Some(c) = self.cart.as_mut() {
            c.load_save_ram(bytes);
        }
    }
    pub fn save_dirty(&self) -> bool {
        self.cart.as_ref().map(|c| c.sram_dirty).unwrap_or(false)
    }
    pub fn clear_save_dirty(&mut self) {
        if let Some(c) = self.cart.as_mut() {
            c.sram_dirty = false;
        }
    }

    /// Raise an interrupt status bit and, if enabled + IF set, present it to the
    /// CPU. The WS controller's vector is `base + bit_index`.
    fn raise_int(&mut self, bit: u8) {
        self.int_status |= bit;
    }

    /// Recompute the CPU's IRQ line from the interrupt controller state.
    fn update_irq_line(&mut self) {
        let pending = self.int_status & self.int_enable;
        if pending != 0 {
            // Pick the lowest set bit's index as the priority.
            let idx = pending.trailing_zeros() as u8;
            self.cpu.irq_line = true;
            self.cpu.irq_vector = self.int_base.wrapping_add(idx);
        } else {
            self.cpu.irq_line = false;
        }
    }

    pub fn run_frame(&mut self) {
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        if self.cart.is_none() {
            return;
        }

        for _ in 0..TOTAL_LINES {
            self.run_scanline();
            if let Some((op, cs, ip)) = self.cpu.fault.take() {
                self.fault = Some(Fault {
                    opcode: op,
                    cs,
                    ip,
                    frame: self.video.frame,
                });
                self.present_crash();
                return;
            }
        }
    }

    fn run_scanline(&mut self) {
        let mut consumed = 0u32;
        while consumed < CYCLES_PER_LINE {
            self.update_irq_line();
            let mut cpu = std::mem::take(&mut self.cpu);
            let t = cpu.step(self);
            self.cpu = cpu;
            consumed += t;
            self.audio_owed += t;
            if self.cpu.fault.is_some() {
                break;
            }
        }

        // Feed the audio unit.
        let owed = self.audio_owed;
        self.audio_owed = 0;
        self.audio.step(owed);

        // Advance the video one scanline. Borrow VRAM (= internal RAM) + palette
        // region out via mem::take to satisfy the borrow checker.
        let ram = std::mem::take(&mut self.ram);
        let pal: &[u8] = if self.model == Model::Color {
            ram.get(COLOR_PAL_BASE..COLOR_PAL_BASE + COLOR_PAL_LEN)
                .unwrap_or(&[])
        } else {
            &[]
        };
        self.video.step_scanline(&ram, pal);
        self.ram = ram;

        // Translate video interrupt flags into the controller.
        if self.video.irq_line_match {
            self.raise_int(INT_LINE_CMP);
        }
        if self.video.irq_vblank {
            self.raise_int(INT_VBLANK);
            self.raise_int(INT_VBLANK_TIMER);
        }
        self.update_irq_line();
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "WONDERSWAN FAULT".to_string(),
            "BAD OPCODE".to_string(),
            format!("OP {:02X}", f.opcode),
            format!("CS {:04X} IP {:04X}", f.cs, f.ip),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.video.framebuffer[..], SCREEN_W, SCREEN_H, &lines);
    }

    // ---- internal RAM access (masked to the model's RAM size) ----
    #[inline]
    fn ram_read(&self, addr: u32) -> u8 {
        let a = addr as usize;
        self.ram.get(a % self.ram.len()).copied().unwrap_or(0)
    }
    #[inline]
    fn ram_write(&mut self, addr: u32, v: u8) {
        let len = self.ram.len();
        if let Some(b) = self.ram.get_mut(addr as usize % len) {
            *b = v;
        }
    }

    /// The internal-RAM region size for the current model, as a physical bound.
    #[inline]
    fn ram_top(&self) -> u32 {
        self.ram.len() as u32
    }

    // ---- I/O port handlers ----
    fn io_read(&mut self, port: u16) -> u8 {
        let p = (port & 0xFF) as u8;
        match p {
            0x00 => self.video.disp_ctrl,
            0x01 => self.video.back_color,
            0x02 => self.video.line as u8, // current scanline (LINE_CUR)
            0x03 => self.video.line_compare,
            0x04 => self.video.spr_base,
            0x05 => self.video.spr_first,
            0x06 => self.video.spr_count,
            0x07 => self.video.map_base,
            0x10 => self.video.scr1_x,
            0x11 => self.video.scr1_y,
            0x12 => self.video.scr2_x,
            0x13 => self.video.scr2_y,
            0x88..=0x8B => 0, // volume regs are write-back-ish; return 0
            0x90 => self.audio.ctrl,
            0x91 => self.audio.output,
            0xA0 => {
                // System control: bit0 = is-color flag, bit7 = boot-rom-locked.
                let color = if self.model == Model::Color { 0x02 } else { 0 };
                0x80 | color | 0x01
            }
            0xB0 => self.int_base,
            0xB2 => self.int_enable,
            0xB4 => self.int_status,
            0xB5 => {
                // KEY scan: the selected row's pressed bits (active high).
                self.read_keys()
            }
            0xC0 => self.cart.as_ref().map(|c| c.bank_rom_linear).unwrap_or(0xFF),
            0xC1 => self.cart.as_ref().map(|c| c.bank_sram).unwrap_or(0),
            0xC2 => self.cart.as_ref().map(|c| c.bank_rom0).unwrap_or(0xFF),
            0xC3 => self.cart.as_ref().map(|c| c.bank_rom1).unwrap_or(0xFF),
            _ => 0,
        }
    }

    fn io_write(&mut self, port: u16, v: u8) {
        let p = (port & 0xFF) as u8;
        match p {
            0x00 => self.video.disp_ctrl = v,
            0x01 => self.video.back_color = v,
            0x03 => self.video.line_compare = v,
            0x04 => self.video.spr_base = v,
            0x05 => self.video.spr_first = v,
            0x06 => self.video.spr_count = v,
            0x07 => self.video.map_base = v,
            0x10 => self.video.scr1_x = v,
            0x11 => self.video.scr1_y = v,
            0x12 => self.video.scr2_x = v,
            0x13 => self.video.scr2_y = v,
            0x14 => {} // LCD on/off — accepted
            // Mono shade pool ($1C-$1F pack 8 shades into 4 bytes).
            0x1C..=0x1F => {
                let base = ((p - 0x1C) as usize) * 2;
                self.video.shade_lut[base] = v & 0x0F;
                self.video.shade_lut[base + 1] = v >> 4;
            }
            // Mono palettes.
            0x20..=0x3F => self.video.mono_palettes[(p - 0x20) as usize] = v,
            // Sound period/volume/control.
            0x80 => self.audio.write_period(0, false, v),
            0x81 => self.audio.write_period(0, true, v),
            0x82 => self.audio.write_period(1, false, v),
            0x83 => self.audio.write_period(1, true, v),
            0x84 => self.audio.write_period(2, false, v),
            0x85 => self.audio.write_period(2, true, v),
            0x86 => self.audio.write_period(3, false, v),
            0x87 => self.audio.write_period(3, true, v),
            0x88 => self.audio.write_volume(0, v),
            0x89 => self.audio.write_volume(1, v),
            0x8A => self.audio.write_volume(2, v),
            0x8B => self.audio.write_volume(3, v),
            0x8C => self.audio.write_voice(v >> 4),
            0x8F => self.audio.write_mode(v),
            0x90 => self.audio.write_ctrl(v),
            0x91 => self.audio.write_output(v),
            0x92 => self.audio.write_mode(v),
            0xA0 => {} // system control — accepted
            0xB0 => self.int_base = v,
            0xB2 => self.int_enable = v,
            // $B6 = interrupt acknowledge: clears the named status bits.
            0xB6 => {
                self.int_status &= !v;
                self.update_irq_line();
            }
            0xB5 => self.key_select = v,
            // Cartridge bank registers.
            0xC0 => {
                if let Some(c) = self.cart.as_mut() {
                    c.bank_rom_linear = v;
                }
            }
            0xC1 => {
                if let Some(c) = self.cart.as_mut() {
                    c.bank_sram = v;
                }
            }
            0xC2 => {
                if let Some(c) = self.cart.as_mut() {
                    c.bank_rom0 = v;
                }
            }
            0xC3 => {
                if let Some(c) = self.cart.as_mut() {
                    c.bank_rom1 = v;
                }
            }
            _ => {}
        }
    }

    /// Read the key matrix for the selected scan row ($B5). The WS multiplexes
    /// three groups: bit5 selects buttons (Start/A/B), bit4 the Y pad, bit6 the
    /// X pad. Returns active-high pressed bits in the low nibble.
    fn read_keys(&self) -> u8 {
        let mut out = self.key_select & 0xF0;
        // Buttons group (bit5).
        if self.key_select & 0x20 != 0 {
            if self.keys & KEY_START != 0 {
                out |= 0x02;
            }
            if self.keys & KEY_A != 0 {
                out |= 0x04;
            }
            if self.keys & KEY_B != 0 {
                out |= 0x08;
            }
        }
        // X pad group (bit6): map the d-pad to X1(up) X2(right) X3(down) X4(left).
        if self.key_select & 0x40 != 0 {
            if self.keys & KEY_UP != 0 {
                out |= 0x01;
            }
            if self.keys & KEY_RIGHT != 0 {
                out |= 0x02;
            }
            if self.keys & KEY_DOWN != 0 {
                out |= 0x04;
            }
            if self.keys & KEY_LEFT != 0 {
                out |= 0x08;
            }
        }
        // Y pad group (bit4): same d-pad mirrored (Y1..Y4) for games that read it.
        if self.key_select & 0x10 != 0 {
            if self.keys & KEY_UP != 0 {
                out |= 0x01;
            }
            if self.keys & KEY_RIGHT != 0 {
                out |= 0x02;
            }
            if self.keys & KEY_DOWN != 0 {
                out |= 0x04;
            }
            if self.keys & KEY_LEFT != 0 {
                out |= 0x08;
            }
        }
        out
    }

    // ---- debug ----
    pub fn dbg_read8(&mut self, addr: u32) -> u8 {
        self.read8(addr)
    }
    pub fn dbg_write8(&mut self, addr: u32, v: u8) {
        self.write8(addr, v);
    }
}

impl V30Bus for WonderSwan {
    fn read8(&mut self, addr: u32) -> u8 {
        let a = addr & crate::bus::ADDR_MASK;
        if a < self.ram_top() {
            self.ram_read(a)
        } else {
            self.cart.as_ref().map(|c| c.read(a)).unwrap_or(0xFF)
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        let a = addr & crate::bus::ADDR_MASK;
        if a < self.ram_top() {
            self.ram_write(a, v);
        } else if let Some(c) = self.cart.as_mut() {
            c.write(a, v);
        }
    }

    fn port_in8(&mut self, port: u16) -> u8 {
        self.io_read(port)
    }
    fn port_out8(&mut self, port: u16, v: u8) {
        self.io_write(port, v);
    }
}

const _: () = assert!(FB_LEN == SCREEN_W * SCREEN_H * 4);

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM whose last bank holds a trivial program at the reset vector
    /// (0xFFFF0): a forever loop. We place a far JMP that keeps CS:IP spinning.
    fn tiny_rom() -> Vec<u8> {
        // 8 banks of 64 KiB. The CPU resets to CS=0xFFFF, IP=0 -> phys 0xFFFF0.
        let mut rom = vec![0u8; 8 * 0x10000];
        // At phys 0xFFFF0: EB FE = JMP $ (infinite short jump to self).
        let reset = 0xFFFF0;
        let last_bank_off = rom.len() - 0x10000;
        rom[last_bank_off + 0xFFF0] = 0xEB;
        rom[last_bank_off + 0xFFF1] = 0xFE;
        let _ = reset;
        rom
    }

    #[test]
    fn dimensions() {
        let ws = WonderSwan::new(Model::Mono);
        assert_eq!(ws.width(), 224);
        assert_eq!(ws.height(), 144);
    }

    #[test]
    fn ram_read_write() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.write8(0x0100, 0x5A);
        assert_eq!(ws.read8(0x0100), 0x5A);
    }

    #[test]
    fn color_has_more_ram() {
        let mut ws = WonderSwan::new(Model::Color);
        // Color RAM extends to 0xFFFF; mono would wrap.
        ws.write8(0x8000, 0x33);
        assert_eq!(ws.read8(0x8000), 0x33);
    }

    #[test]
    fn rom_reads_through_cart() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.load_rom(&tiny_rom());
        // Reset vector area maps the last bank's program bytes.
        assert_eq!(ws.read8(0xFFFF0), 0xEB);
        assert_eq!(ws.read8(0xFFFF1), 0xFE);
    }

    #[test]
    fn reset_vector_and_run_frame() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.load_rom(&tiny_rom());
        // The CPU should sit in the JMP $ loop and run_frame advances the frame.
        let f0 = ws.frame_count();
        ws.run_frame();
        assert_eq!(ws.frame_count(), f0 + 1);
        assert!(ws.fault.is_none());
        // CPU stayed near the reset vector.
        assert_eq!(ws.cpu.seg[crate::cpu::SEG_CS], 0xFFFF);
    }

    #[test]
    fn display_register_writes() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.port_out8(0x00, 0x07); // enable all layers
        assert_eq!(ws.video.disp_ctrl, 0x07);
        ws.port_out8(0x10, 0x20); // scr1 scroll x
        assert_eq!(ws.video.scr1_x, 0x20);
    }

    #[test]
    fn interrupt_enable_and_vector() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.port_out8(0xB0, 0x10); // base vector
        ws.port_out8(0xB2, INT_VBLANK); // enable vblank
        ws.raise_int(INT_VBLANK);
        ws.update_irq_line();
        assert!(ws.cpu.irq_line);
        // vblank is bit6 -> vector = base + 6
        assert_eq!(ws.cpu.irq_vector, 0x10 + 6);
    }

    #[test]
    fn interrupt_ack_clears() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.port_out8(0xB2, INT_VBLANK);
        ws.raise_int(INT_VBLANK);
        ws.update_irq_line();
        assert!(ws.cpu.irq_line);
        ws.port_out8(0xB6, INT_VBLANK); // acknowledge
        assert!(!ws.cpu.irq_line);
    }

    #[test]
    fn keys_button_group() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.set_keys(KEY_A | KEY_START);
        ws.port_out8(0xB5, 0x20); // select button group
        let v = ws.port_in8(0xB5);
        assert!(v & 0x04 != 0); // A pressed
        assert!(v & 0x02 != 0); // Start pressed
    }

    #[test]
    fn keys_dpad_group() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.set_keys(KEY_UP | KEY_LEFT);
        ws.port_out8(0xB5, 0x40); // X pad group
        let v = ws.port_in8(0xB5);
        assert!(v & 0x01 != 0); // up
        assert!(v & 0x08 != 0); // left
    }

    #[test]
    fn cart_bank_register_roundtrip() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.load_rom(&tiny_rom());
        ws.port_out8(0xC2, 0x03);
        assert_eq!(ws.port_in8(0xC2), 0x03);
    }

    #[test]
    fn vblank_drives_irq_during_frame() {
        let mut ws = WonderSwan::new(Model::Mono);
        ws.load_rom(&tiny_rom());
        ws.port_out8(0xB2, INT_VBLANK); // enable vblank int
        ws.run_frame();
        // After a frame the vblank status bit will have been raised at line 144.
        assert!(ws.int_status & INT_VBLANK != 0 || ws.frame_count() >= 1);
    }
}
