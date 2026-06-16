//! The `Snes` god-struct: owns the 65816 CPU, the PPU, the APU (SPC700 + DSP),
//! the cartridge, 128 KiB WRAM, the DMA controller, and input; implements the
//! CPU [`Bus`]; and runs video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. The CPU needs the whole machine as its `Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back. The same `mem::take` trick resolves PPU/APU/DMA cross-calls.
//!
//! ## SNES memory map (simplified, per fullsnes)
//!
//! Banks $00-$3F and $80-$BF, low half ($0000-$7FFF):
//!   $0000-$1FFF  WRAM mirror (first 8 KiB of the 128 KiB WRAM)
//!   $2100-$213F  B-bus: PPU registers
//!   $2140-$2143  B-bus: APU I/O ports
//!   $2180-$2183  WRAM port (read/write + 24-bit address)
//!   $4016-$4017  manual joypad
//!   $4200-$421F  CPU I/O: NMI/IRQ enable, multiply/divide, auto-joypad
//!   $4300-$437F  DMA channels
//!   $8000-$FFFF  cartridge ROM
//! Banks $7E-$7F: the full 128 KiB WRAM.
//! Banks $40-$7D / $C0-$FF: cartridge (HiROM full banks / LoROM upper halves).

use crate::apu::Apu;
use crate::bus::Bus;
use crate::cart::Cart;
use crate::dma::{transfer_pattern, Dma};
use crate::input::Controllers;
use crate::ppu::{Ppu, FB_LEN, SCREEN_H, SCREEN_W};
use crate::cpu::Cpu;

/// Approximate CPU cycles per scanline (NTSC: 1364 master / 6 ~ 227, but our
/// cycle counts are memory-access based; this paces a frame sensibly).
const CYCLES_PER_LINE: u32 = 1100;
const VISIBLE_LINES: u32 = 224;
const TOTAL_LINES: u32 = 262;

/// A latched CPU fault, captured for the crash screen (STP executed).
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub opcode: u8,
    pub pc: u16,
}

pub struct Snes {
    pub cpu: Cpu,
    pub ppu: Ppu,
    pub apu: Apu,
    pub dma: Dma,
    pub cart: Option<Cart>,
    pub input: Controllers,

    /// 128 KiB work RAM.
    wram: Box<[u8; 0x20000]>,
    /// $2181-$2183 WRAM port address.
    wram_addr: u32,

    // --- CPU I/O registers ---
    nmi_enable: bool,
    irq_mode: u8,
    auto_joypad: bool,
    /// NMI flag ($4210 bit7), set at vblank, cleared on read.
    nmi_flag: bool,
    /// Multiply/divide unit.
    mul_a: u8,
    mul_result: u16,
    div_dividend: u16,
    div_result: u16,
    div_remainder: u16,
    /// H/V IRQ target positions.
    htime: u16,
    vtime: u16,

    pub fault: Option<Fault>,
}

impl Default for Snes {
    fn default() -> Self {
        Snes::new()
    }
}

impl Snes {
    pub fn new() -> Snes {
        Snes {
            cpu: Cpu::new(),
            ppu: Ppu::new(),
            apu: Apu::new(),
            dma: Dma::new(),
            cart: None,
            input: Controllers::new(),
            wram: vec![0u8; 0x20000].into_boxed_slice().try_into().unwrap(),
            wram_addr: 0,
            nmi_enable: false,
            irq_mode: 0,
            auto_joypad: false,
            nmi_flag: false,
            mul_a: 0,
            mul_result: 0,
            div_dividend: 0,
            div_result: 0,
            div_remainder: 0,
            htime: 0,
            vtime: 0,
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        self.cart = Some(cart);
        self.ppu = Ppu::new();
        self.apu = Apu::new();
        self.dma = Dma::new();
        self.cpu = Cpu::new();
        self.fault = None;
        // Reset reads the reset vector through the bus.
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.reset(self);
        self.cpu = cpu;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(0, bits);
    }
    pub fn set_keys_port(&mut self, port: usize, bits: u32) {
        self.input.set_keys(port, bits);
    }

    pub fn framebuffer(&self) -> &[u8] {
        self.ppu.framebuffer()
    }
    pub fn width(&self) -> usize {
        SCREEN_W
    }
    pub fn height(&self) -> usize {
        SCREEN_H
    }
    pub fn frame_count(&self) -> u64 {
        self.ppu.frame
    }
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.apu.drain()
    }

    // ---- battery save ----
    pub fn save_ram(&self) -> Vec<u8> {
        self.cart.as_ref().map(|c| c.save_ram().to_vec()).unwrap_or_default()
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

    /// Run one full video frame.
    pub fn run_frame(&mut self) {
        if self.cart.is_none() {
            return;
        }
        if self.fault.is_some() {
            self.present_crash();
            return;
        }

        // Latch input at frame start (auto-joypad read happens in vblank, but
        // sampling here is close enough for a whole-frame renderer).
        self.input.latch();

        // Visible scanlines: run the CPU + APU. HDMA would run per line; we run
        // a coarse HDMA pass at the top of the frame.
        for line in 0..TOTAL_LINES {
            // Enter vblank: set NMI flag, fire NMI if enabled.
            if line == VISIBLE_LINES {
                self.nmi_flag = true;
                self.ppu.oam_reload();
                if self.nmi_enable {
                    self.cpu.nmi_pending = true;
                }
            }
            self.run_cycles(CYCLES_PER_LINE);
            if self.fault.is_some() {
                self.present_crash();
                return;
            }
        }

        // Render the whole frame from the final PPU state.
        let mut ppu = std::mem::take(&mut self.ppu);
        ppu.render_frame();
        self.ppu = ppu;

        self.nmi_flag = false;
    }

    /// Run the CPU + APU for approximately `cycles` CPU cycles.
    fn run_cycles(&mut self, cycles: u32) {
        let mut spent = 0u32;
        let mut guard = 0u32;
        while spent < cycles && guard < 200_000 {
            // Level IRQ.
            self.cpu.irq_line = false;

            let mut cpu = std::mem::take(&mut self.cpu);
            let used = cpu.step(self);
            let stopped = cpu.stopped;
            let cpu_fault = cpu.fault;
            self.cpu = cpu;

            // Run the APU roughly in lockstep (SPC ~= same order of cycles).
            self.apu.step(used);

            spent += used;
            guard += 1;

            if stopped {
                if let Some((op, pc)) = cpu_fault {
                    self.fault = Some(Fault { opcode: op, pc });
                }
                return;
            }
        }
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "SNES CORE FAULT".to_string(),
            format!("STP OP {:02X}", f.opcode),
            format!("PC {:04X}", f.pc),
        ];
        crate::crash::render(&mut self.ppu.framebuffer[..], SCREEN_W, SCREEN_H, &lines);
        self.ppu.frame += 1;
    }

    // =========================================================================
    // DMA execution. General-purpose DMA ($420B) runs a full transfer now.
    // =========================================================================
    fn run_gpdma(&mut self, mask: u8) {
        for chan in 0..8 {
            if mask & (1 << chan) == 0 {
                continue;
            }
            let (params, b_addr, mut a_addr, a_bank, count) = {
                let c = &self.dma.ch[chan];
                (c.params, c.b_addr, c.a_addr, c.a_bank, c.count)
            };
            let to_ppu = params & 0x80 == 0; // bit7: 0 = A->B (CPU->PPU)
            let step = match (params >> 3) & 0x03 {
                0 => 1i32,  // increment
                2 => -1i32, // decrement
                _ => 0,     // fixed
            };
            let pattern = transfer_pattern(params);
            let mut pi = 0usize;
            let mut transferred = 0u32;
            let total = if count == 0 { 0x10000 } else { count as u32 };
            while transferred < total {
                let b = 0x2100u16 + b_addr as u16 + pattern[pi % pattern.len()] as u16;
                let a = ((a_bank as u32) << 16) | a_addr as u32;
                if to_ppu {
                    let v = self.read8(a);
                    self.write8(b as u32, v);
                } else {
                    let v = self.read8(b as u32);
                    self.write8(a, v);
                }
                a_addr = (a_addr as i32 + step) as u16;
                pi += 1;
                transferred += 1;
            }
            // a_bank is unchanged by GP-DMA (only the 16-bit A-address steps);
            // the byte count drains to zero.
            let c = &mut self.dma.ch[chan];
            c.a_addr = a_addr;
            c.count = 0;
        }
    }

    // ---- WRAM port ($2180-$2183) ----
    fn wram_port_read(&mut self) -> u8 {
        let v = self.wram[(self.wram_addr & 0x1FFFF) as usize];
        self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
        v
    }
    fn wram_port_write(&mut self, v: u8) {
        self.wram[(self.wram_addr & 0x1FFFF) as usize] = v;
        self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
    }

    // ---- CPU I/O registers ($4200-$421F) ----
    fn read_cpu_io(&mut self, addr: u16) -> u8 {
        match addr {
            0x4210 => {
                // RDNMI: bit7 = NMI flag (cleared on read), low nibble = CPU version.
                let v = (self.nmi_flag as u8) << 7 | 0x02;
                self.nmi_flag = false;
                v
            }
            0x4211 => 0,    // TIMEUP (IRQ flag) — stubbed
            0x4212 => {
                // HVBJOY: bit7 vblank, bit6 hblank, bit0 auto-joypad busy.
                // Report vblank set so polling loops progress.
                0x80
            }
            0x4214 => self.div_result as u8,
            0x4215 => (self.div_result >> 8) as u8,
            0x4216 => self.div_remainder as u8 | self.mul_result as u8,
            0x4217 => (self.div_remainder >> 8) as u8 | (self.mul_result >> 8) as u8,
            0x4218 => self.input.auto_read(0) as u8,
            0x4219 => (self.input.auto_read(0) >> 8) as u8,
            0x421A => self.input.auto_read(1) as u8,
            0x421B => (self.input.auto_read(1) >> 8) as u8,
            0x421C..=0x421F => 0, // joypads 3/4
            _ => 0,
        }
    }
    fn write_cpu_io(&mut self, addr: u16, v: u8) {
        match addr {
            0x4200 => {
                self.nmi_enable = v & 0x80 != 0;
                self.irq_mode = (v >> 4) & 0x03;
                self.auto_joypad = v & 0x01 != 0;
            }
            0x4202 => self.mul_a = v,
            0x4203 => {
                // Multiply: result = mul_a * v.
                self.mul_result = self.mul_a as u16 * v as u16;
            }
            0x4204 => self.div_dividend = (self.div_dividend & 0xFF00) | v as u16,
            0x4205 => self.div_dividend = (self.div_dividend & 0x00FF) | ((v as u16) << 8),
            0x4206 => {
                // Divide dividend / v.
                if v == 0 {
                    self.div_result = 0xFFFF;
                    self.div_remainder = self.div_dividend;
                } else {
                    self.div_result = self.div_dividend / v as u16;
                    self.div_remainder = self.div_dividend % v as u16;
                }
            }
            0x4207 => self.htime = (self.htime & 0xFF00) | v as u16,
            0x4208 => self.htime = (self.htime & 0x00FF) | ((v as u16) << 8),
            0x4209 => self.vtime = (self.vtime & 0xFF00) | v as u16,
            0x420A => self.vtime = (self.vtime & 0x00FF) | ((v as u16) << 8),
            0x420B => self.run_gpdma(v),
            0x420C => self.dma.hdma_enable = v,
            0x420D => {} // MEMSEL (FastROM) — no timing effect here
            _ => {}
        }
    }
}

// =============================================================================
// CPU memory map: the `Bus` trait the CPU codes against.
// =============================================================================
impl Bus for Snes {
    fn read8(&mut self, addr: u32) -> u8 {
        let bank = (addr >> 16) as u8;
        let off = addr as u16;

        // WRAM banks $7E-$7F.
        if bank == 0x7E {
            return self.wram[off as usize];
        }
        if bank == 0x7F {
            return self.wram[0x10000 + off as usize];
        }

        // System area in banks $00-$3F / $80-$BF.
        let sys = (bank & 0x7F) <= 0x3F;
        if sys && off < 0x8000 {
            match off {
                0x0000..=0x1FFF => return self.wram[off as usize],
                0x2100..=0x213F => {
                    let mut ppu = std::mem::take(&mut self.ppu);
                    let v = ppu.read_reg(off);
                    self.ppu = ppu;
                    return v;
                }
                0x2140..=0x217F => return self.apu.cpu_read_port((off & 3) as usize),
                0x2180 => return self.wram_port_read(),
                0x2181..=0x2183 => return 0,
                0x4016 => return self.input.read_serial(0),
                0x4017 => return self.input.read_serial(1),
                0x4200..=0x421F => return self.read_cpu_io(off),
                0x4300..=0x437F => return self.dma.read_reg(off),
                _ => {}
            }
        }

        // Cartridge.
        if let Some(c) = self.cart.as_ref() {
            if let Some(v) = c.read(bank, off) {
                return v;
            }
        }
        0
    }

    fn write8(&mut self, addr: u32, v: u8) {
        let bank = (addr >> 16) as u8;
        let off = addr as u16;

        if bank == 0x7E {
            self.wram[off as usize] = v;
            return;
        }
        if bank == 0x7F {
            self.wram[0x10000 + off as usize] = v;
            return;
        }

        let sys = (bank & 0x7F) <= 0x3F;
        if sys && off < 0x8000 {
            match off {
                0x0000..=0x1FFF => {
                    self.wram[off as usize] = v;
                    return;
                }
                0x2100..=0x213F => {
                    let mut ppu = std::mem::take(&mut self.ppu);
                    ppu.write_reg(off, v);
                    self.ppu = ppu;
                    return;
                }
                0x2140..=0x217F => {
                    self.apu.cpu_write_port((off & 3) as usize, v);
                    return;
                }
                0x2180 => {
                    self.wram_port_write(v);
                    return;
                }
                0x2181 => {
                    self.wram_addr = (self.wram_addr & 0x1FF00) | v as u32;
                    return;
                }
                0x2182 => {
                    self.wram_addr = (self.wram_addr & 0x100FF) | ((v as u32) << 8);
                    return;
                }
                0x2183 => {
                    self.wram_addr = (self.wram_addr & 0x0FFFF) | ((v as u32 & 1) << 16);
                    return;
                }
                0x4016 => {
                    self.input.write_strobe(v);
                    return;
                }
                0x4200..=0x421F => {
                    self.write_cpu_io(off, v);
                    return;
                }
                0x4300..=0x437F => {
                    self.dma.write_reg(off, v);
                    return;
                }
                _ => {}
            }
        }

        // Cartridge SRAM.
        if let Some(c) = self.cart.as_mut() {
            c.write(bank, off, v);
        }
    }
}

const _: () = assert!(FB_LEN == SCREEN_W * SCREEN_H * 4);

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny LoROM with a given reset routine at bank0 $8000.
    fn build_rom(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0u8; 0x10000];
        // Header at $7FC0.
        let base = 0x7FC0;
        for (i, &c) in b"SNESTEST             ".iter().enumerate() {
            rom[base + i] = c;
        }
        rom[base + 0x1C] = 0x00;
        rom[base + 0x1D] = 0x00;
        rom[base + 0x1E] = 0xFF;
        rom[base + 0x1F] = 0xFF;
        // Reset vector -> $8000.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Code at LoROM bank0 offset 0 ($00:8000).
        for (i, &b) in code.iter().enumerate() {
            rom[i] = b;
        }
        rom
    }

    #[test]
    fn load_and_reset_vector() {
        let rom = build_rom(&[0xEA]); // NOP
        let mut snes = Snes::new();
        snes.load_rom(&rom);
        assert_eq!(snes.cpu.pc, 0x8000);
    }

    #[test]
    fn wram_mirror_and_banks() {
        let mut snes = Snes::new();
        snes.write8(0x000010, 0x42);
        // Mirror in bank $80.
        assert_eq!(snes.read8(0x800010), 0x42);
        // Full WRAM at $7E.
        snes.write8(0x7E1234, 0x99);
        assert_eq!(snes.read8(0x7E1234), 0x99);
    }

    #[test]
    fn runs_simple_program() {
        // Native 16-bit: CLC, XCE, REP #$30, LDA #$1234, STA $7E0000, STP.
        let rom = build_rom(&[
            0x18,             // CLC
            0xFB,             // XCE
            0xC2, 0x30,       // REP #$30
            0xA9, 0x34, 0x12, // LDA #$1234
            0x8F, 0x00, 0x00, 0x7E, // STA $7E0000 (long)
            0xDB,             // STP
        ]);
        let mut snes = Snes::new();
        snes.load_rom(&rom);
        // Step the CPU directly until it stops.
        for _ in 0..50 {
            let mut cpu = std::mem::take(&mut snes.cpu);
            cpu.step(&mut snes);
            let stopped = cpu.stopped;
            snes.cpu = cpu;
            if stopped {
                break;
            }
        }
        assert_eq!(snes.read8(0x7E0000), 0x34);
        assert_eq!(snes.read8(0x7E0001), 0x12);
    }

    #[test]
    fn multiply_register() {
        let mut snes = Snes::new();
        snes.write8(0x004202, 0x10); // WRMPYA
        snes.write8(0x004203, 0x10); // WRMPYB -> 0x100
        assert_eq!(snes.read8(0x004216), 0x00);
        assert_eq!(snes.read8(0x004217), 0x01);
    }

    #[test]
    fn divide_register() {
        let mut snes = Snes::new();
        snes.write8(0x004204, 0x64); // dividend lo = 100
        snes.write8(0x004205, 0x00);
        snes.write8(0x004206, 0x07); // / 7 -> 14 r 2
        assert_eq!(snes.read8(0x004214), 14);
        assert_eq!(snes.read8(0x004216), 2);
    }

    #[test]
    fn gpdma_transfers_to_ppu() {
        let mut snes = Snes::new();
        // Put data in WRAM at $00:1000.
        for i in 0..4 {
            snes.write8(0x001000 + i, (0x10 + i) as u8);
        }
        // DMA channel 0: A->B, mode 0 (single byte), to $2122 (CGRAM data).
        snes.write8(0x004300, 0x00); // params: A->B, increment, mode 0
        snes.write8(0x004301, 0x22); // B addr = $2122
        snes.write8(0x004302, 0x00); // a_addr lo
        snes.write8(0x004303, 0x10); // a_addr hi -> $1000
        snes.write8(0x004304, 0x00); // a_bank
        snes.write8(0x004305, 0x04); // count = 4
        snes.write8(0x004306, 0x00);
        snes.write8(0x00420B, 0x01); // start channel 0
        // CGRAM should have received the 4 bytes (2 colors).
        assert_eq!(snes.ppu.cgram[0], 0x10);
        assert_eq!(snes.ppu.cgram[1], 0x11);
    }

    #[test]
    fn nmi_flag_set_in_vblank() {
        let rom = build_rom(&[0x80, 0xFE]); // BRA -2 (spin)
        let mut snes = Snes::new();
        snes.load_rom(&rom);
        let f0 = snes.frame_count();
        snes.run_frame();
        assert_eq!(snes.frame_count(), f0 + 1);
    }

    #[test]
    fn renders_a_visible_background() {
        // Drive the PPU through the CPU bus exactly as a game would: load a solid
        // tile + a palette, point BG1 at it, enable it on the main screen, then
        // render a frame and confirm non-black pixels reach the framebuffer.
        let rom = build_rom(&[0x80, 0xFE]); // spin
        let mut snes = Snes::new();
        snes.load_rom(&rom);

        // CGRAM: color 0 black, color 1 = white ($7FFF).
        snes.write8(0x002121, 0x00); // CGADD = 0
        snes.write8(0x002122, 0x00); // color 0 lo
        snes.write8(0x002122, 0x00); // color 0 hi
        snes.write8(0x002122, 0xFF); // color 1 lo
        snes.write8(0x002122, 0x7F); // color 1 hi

        // VRAM: write a solid 2bpp tile (all pixels color 1) at char base word 0.
        snes.write8(0x002115, 0x80); // VMAIN: inc on high byte, +1
        snes.write8(0x002116, 0x00); // VMADDL
        snes.write8(0x002117, 0x00); // VMADDH
        for _ in 0..8 {
            // 2bpp: plane0 = 0xFF (all low bits set), plane1 = 0x00.
            snes.write8(0x002118, 0xFF); // low byte (plane 0)
            snes.write8(0x002119, 0x00); // high byte (plane 1)
        }

        // Tilemap at word $0400: fill with tile 0, palette 0.
        snes.write8(0x002116, 0x00);
        snes.write8(0x002117, 0x04); // VRAM word addr = $0400
        for _ in 0..32 * 32 {
            snes.write8(0x002118, 0x00); // tile number low
            snes.write8(0x002119, 0x00); // attributes
        }

        // BG1 tilemap base = $0400 (>>10 = 1), char base = 0.
        snes.write8(0x002107, 0x04); // BG1SC: SC base = word $0400, size 0
        snes.write8(0x00210B, 0x00); // BG12NBA: BG1 char base 0
        snes.write8(0x002105, 0x00); // BGMODE 0
        snes.write8(0x00212C, 0x01); // TM: enable BG1 on main screen
        snes.write8(0x002100, 0x0F); // INIDISP: brightness 15, not blanked

        let mut ppu = std::mem::take(&mut snes.ppu);
        ppu.render_frame();
        snes.ppu = ppu;

        // Color index 1 (white) should appear somewhere on screen.
        let fb = snes.framebuffer();
        let has_white = fb.chunks_exact(4).any(|px| px[0] > 200 && px[1] > 200 && px[2] > 200);
        assert!(has_white, "BG1 solid tile should render white pixels");
    }

    #[test]
    fn stp_triggers_crash_screen() {
        let rom = build_rom(&[0xDB]); // STP immediately
        let mut snes = Snes::new();
        snes.load_rom(&rom);
        snes.run_frame();
        assert!(snes.fault.is_some());
        // Crash screen drawn: at least one non-black pixel.
        let fb = snes.framebuffer();
        let bg = [0x10u8, 0x10, 0x60, 0xFF];
        assert!(fb.chunks_exact(4).any(|px| px == bg));
    }
}
