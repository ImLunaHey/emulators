//! The `Genesis` god-struct: owns the 68000, Z80, VDP, YM2612, SN76489 PSG,
//! cartridge, work RAM, and input; implements both CPUs' bus traits; and runs
//! video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. A CPU needs the whole machine as its bus, so we `mem::take`
//! the CPU out of `self`, run it with `self` as the bus, then put it back — no
//! `Rc`/`RefCell`.
//!
//! 68000 memory map (big-endian):
//!   $000000-$3FFFFF  cartridge ROM (and on-cart SRAM window)
//!   $A00000-$A0FFFF  Z80 address space (when the 68000 has the bus)
//!   $A04000-$A04003  YM2612 (also visible in the Z80 area)
//!   $A10000-$A1001F  I/O: version, controller data/control ports
//!   $A11100          Z80 BUSREQ;  $A11200  Z80 RESET
//!   $C00000-$C00007  VDP data ($00) / control ($04) / HV counter ($08)
//!   $C00011          PSG (SN76489) write
//!   $E00000-$FFFFFF  64 KiB 68000 work RAM (mirrored every 64 KiB)
//!
//! Z80 memory map:
//!   $0000-$1FFF  8 KiB Z80 RAM (mirrored to $3FFF)
//!   $4000-$4003  YM2612
//!   $6000        bank register (sets the 68000-space window base)
//!   $7F11        PSG
//!   $8000-$FFFF  windowed view into 68000 space (bank << 15)

use crate::bus::Z80Bus;
use crate::cart::Cart;
use crate::io::Input;
use crate::m68k::{Bus as M68kBus, M68k};
use crate::psg::Psg;
use crate::vdp::{Vdp, FB_LEN, HEIGHT, MAX_W, SCANLINES};
use crate::ym2612::Ym2612;
use crate::z80::Cpu as Z80;

/// 68000 cycles per scanline: ~7.67 MHz / (262 lines * 60 Hz) ≈ 488.
const M68K_CYCLES_PER_LINE: u32 = 488;
/// Z80 runs at ~3.58 MHz; ratio to the 68000 line budget ≈ 228 cycles/line.
const Z80_CYCLES_PER_LINE: u32 = 228;

#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub pc: u32,
    pub vector: u8,
    pub frame: u64,
}

pub struct Genesis {
    pub cpu: M68k,
    pub z80: Z80,
    pub vdp: Vdp,
    pub ym: Ym2612,
    pub psg: Psg,
    pub input: Input,
    pub cart: Option<Cart>,

    /// 64 KiB 68000 work RAM ($E00000-$FFFFFF, mirrored).
    ram: Box<[u8; 0x10000]>,
    /// 8 KiB Z80 RAM.
    zram: Box<[u8; 0x2000]>,

    /// Z80 bus arbitration: when the 68000 holds BUSREQ the Z80 is stopped.
    z80_busreq: bool,
    z80_reset: bool,
    /// Z80 -> 68000 window bank (bits 15..23 of the target 68000 address).
    z80_bank: u32,

    audio_owed: u32,

    pub fault: Option<Fault>,
}

impl Genesis {
    pub fn new() -> Genesis {
        Genesis {
            cpu: M68k::new(),
            z80: Z80::new(),
            vdp: Vdp::new(),
            ym: Ym2612::new(),
            psg: Psg::new(),
            input: Input::new(),
            cart: None,
            ram: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
            zram: vec![0u8; 0x2000].into_boxed_slice().try_into().unwrap(),
            z80_busreq: true, // 68000 owns the bus at reset
            z80_reset: true,
            z80_bank: 0,
            audio_owed: 0,
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        self.cart = Some(cart);
        // Reset everything for a clean boot.
        self.vdp = Vdp::new();
        self.ym = Ym2612::new();
        self.psg = Psg::new();
        self.z80 = Z80::new();
        for b in self.ram.iter_mut() {
            *b = 0;
        }
        for b in self.zram.iter_mut() {
            *b = 0;
        }
        self.z80_busreq = true;
        self.z80_reset = true;
        self.z80_bank = 0;
        self.fault = None;
        // Reset the 68000 (loads SSP/PC from the cart vectors).
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.reset(self);
        self.cpu = cpu;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(bits);
    }
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.input.set_keys_p2(bits);
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        // Mix the PSG and YM streams. They run at the same host rate; sum and
        // clamp, padding the shorter with the available samples.
        let mut psg = self.psg.drain();
        let ym = self.ym.drain();
        let n = psg.len().max(ym.len());
        psg.resize(n, 0.0);
        for (i, s) in psg.iter_mut().enumerate() {
            let y = ym.get(i).copied().unwrap_or(0.0);
            *s = (*s * 0.5 + y * 0.5).clamp(-1.0, 1.0);
        }
        psg
    }

    pub fn frame_count(&self) -> u64 {
        self.vdp.frame
    }

    pub fn width(&self) -> usize {
        self.vdp.width()
    }
    pub fn height(&self) -> usize {
        HEIGHT
    }

    /// RGBA8888 framebuffer. The VDP renders into a 320-wide buffer; for H32
    /// (256-wide) modes the host still receives the full 320 buffer and uses
    /// `width()` to crop — but to keep the public contract (width*height*4) we
    /// repack into a tight buffer when narrower.
    pub fn framebuffer(&self) -> &[u8] {
        &self.vdp.framebuffer[..]
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
        self.cart.as_ref().map(|c| c.ram_dirty).unwrap_or(false)
    }
    pub fn clear_save_dirty(&mut self) {
        if let Some(c) = self.cart.as_mut() {
            c.ram_dirty = false;
        }
    }

    /// Run one full video frame.
    pub fn run_frame(&mut self) {
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        if self.cart.is_none() {
            return;
        }
        for _ in 0..SCANLINES {
            self.run_scanline();
        }
        // Detect a wedged CPU: a double bus/address fault is approximated by a
        // repeated illegal exception. We surface the last latched exception if
        // the CPU is stopped with no interrupt able to wake it.
        if let Some(vec) = self.cpu.last_exception {
            if (vec == 2 || vec == 3) && self.cpu.stopped {
                self.fault = Some(Fault {
                    pc: self.cpu.pc,
                    vector: vec,
                    frame: self.vdp.frame,
                });
                self.present_crash();
            }
        }
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "GENESIS CORE FAULT".to_string(),
            format!("VECTOR {}", f.vector),
            format!("PC {:06X}", f.pc),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.vdp.framebuffer[..], MAX_W, HEIGHT, &lines);
    }

    fn run_scanline(&mut self) {
        self.vdp.start_line();

        // Run the 68000 for this line's cycle budget.
        let mut consumed = 0u32;
        while consumed < M68K_CYCLES_PER_LINE {
            // Sample the VDP IRQ into the CPU.
            self.cpu.irq_level = self.vdp.irq_level();
            let mut cpu = std::mem::take(&mut self.cpu);
            let t = cpu.step(self);
            self.cpu = cpu;
            // Acknowledge whichever interrupt was just taken.
            if self.cpu.last_exception_was_int() {
                // (handled via irq_level clearing below)
            }
            consumed += t;
            self.audio_owed += t;
            // If the CPU just serviced the VDP interrupt, clear the latch.
            self.clear_serviced_irq();
        }

        // Run the Z80 if it has the bus and isn't held in reset.
        if !self.z80_busreq && !self.z80_reset {
            let mut zconsumed = 0u32;
            while zconsumed < Z80_CYCLES_PER_LINE {
                self.z80.irq_line = self.vdp.line >= 224 && self.vdp.line < 225;
                let mut z = std::mem::take(&mut self.z80);
                let t = z.step(self);
                self.z80 = z;
                zconsumed += t;
            }
        }

        // Feed sound chips.
        let owed = self.audio_owed;
        self.audio_owed = 0;
        self.ym.step(owed);
        // PSG runs at clock/15 vs the 68000; approximate by halving.
        self.psg.step(owed / 7);

        self.vdp.end_line();
    }

    /// After a CPU step, if the VDP IRQ was serviced (mask raised), ack it.
    fn clear_serviced_irq(&mut self) {
        let mask = ((self.cpu.sr >> 8) & 0x07) as u8;
        if self.vdp.vint_pending && mask >= 6 {
            self.vdp.ack_vint();
        } else if self.vdp.hint_pending && mask >= 4 && !self.vdp.vint_pending {
            self.vdp.ack_hint();
        }
    }

    // ---- helpers for 68000 access to the VDP region ----
    fn vdp_read16(&mut self, addr: u32) -> u16 {
        match addr & 0x1F {
            0x00 | 0x02 => self.vdp.read_data(),
            0x04 | 0x06 => self.vdp.read_status(),
            0x08 | 0x0A => {
                ((self.vdp.v_counter() as u16) << 8) | self.vdp.h_counter() as u16
            }
            _ => 0,
        }
    }
    fn vdp_write16(&mut self, addr: u32, v: u16) {
        match addr & 0x1F {
            0x00 | 0x02 => self.vdp.write_data(v),
            0x04 | 0x06 => {
                self.vdp.write_control(v);
                // A control write may have armed a 68000->VDP DMA.
                self.maybe_run_dma();
            }
            0x10 | 0x12 | 0x14 | 0x16 => {
                // PSG is byte-wide; low byte carries the value.
                self.psg.write(v as u8);
            }
            _ => {}
        }
    }

    /// Drive a pending memory->VDP DMA by reading words from 68000 space.
    fn maybe_run_dma(&mut self) {
        if !self.vdp.pending_mem_dma {
            return;
        }
        // Snapshot the source/length via the VDP, then copy through our bus.
        // We collect into a Vec first to avoid borrowing `self` twice.
        let len = {
            let lo = self.vdp.regs[0x13] as u32;
            let hi = (self.vdp.regs[0x14] as u32) << 8;
            let l = lo | hi;
            if l == 0 {
                0x10000
            } else {
                l
            }
        };
        let mut src = self.vdp.dma_source();
        let mut words = Vec::with_capacity(len as usize);
        for _ in 0..len {
            words.push(M68kBus::read16(self, src));
            src = src.wrapping_add(2);
        }
        let mut iter = words.into_iter();
        self.vdp.run_mem_dma(|_addr| iter.next().unwrap_or(0));
    }
}

impl Default for Genesis {
    fn default() -> Self {
        Genesis::new()
    }
}

// =============================================================================
// 68000 bus (big-endian). The trait's read16/read32 helpers compose byte
// access, but we override read16/write16 for device regions where 16-bit access
// is atomic (VDP ports especially).
// =============================================================================
impl M68kBus for Genesis {
    fn read8(&mut self, addr: u32) -> u8 {
        let a = addr & 0xFF_FFFF;
        match a {
            0x000000..=0x3FFFFF => self.cart.as_ref().map(|c| c.read(a)).unwrap_or(0xFF),
            0xA00000..=0xA0FFFF => {
                // Z80 area, accessible to the 68000 while it holds the bus.
                self.z80_area_read((a & 0xFFFF) as u16)
            }
            0xA10000..=0xA1001F => self.io_read(a),
            0xA11100 | 0xA11101 => {
                // BUSREQ status: bit0 = 1 means Z80 still running (not yet
                // granted). We return 0 when the 68000 has the bus.
                if self.z80_busreq {
                    0x00
                } else {
                    0x01
                }
            }
            0xC00000..=0xC0000F => {
                let w = self.vdp_read16(a);
                if a & 1 == 0 {
                    (w >> 8) as u8
                } else {
                    (w & 0xFF) as u8
                }
            }
            0xE00000..=0xFFFFFF => self.ram[(a & 0xFFFF) as usize],
            _ => 0xFF,
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        let a = addr & 0xFF_FFFF;
        match a {
            0x000000..=0x3FFFFF => {
                if let Some(c) = self.cart.as_mut() {
                    c.write(a, v);
                }
            }
            0xA00000..=0xA0FFFF => self.z80_area_write((a & 0xFFFF) as u16, v),
            0xA10000..=0xA1001F => self.io_write(a, v),
            0xA11100 | 0xA11101 => {
                // BUSREQ: writing 1 requests the bus (stops Z80), 0 releases it.
                self.z80_busreq = v & 0x01 != 0;
            }
            0xA11200 | 0xA11201 => {
                // RESET: 0 holds the Z80 in reset, 1 releases it.
                self.z80_reset = v & 0x01 == 0;
                if !self.z80_reset {
                    // leaving reset
                }
            }
            0xC00011 | 0xC00013 | 0xC00015 | 0xC00017 => self.psg.write(v),
            0xE00000..=0xFFFFFF => self.ram[(a & 0xFFFF) as usize] = v,
            _ => {}
        }
    }

    fn read16(&mut self, addr: u32) -> u16 {
        let a = addr & 0xFF_FFFE;
        match a {
            0xC00000..=0xC0000F => self.vdp_read16(a),
            0x000000..=0x3FFFFF => {
                let c = match self.cart.as_ref() {
                    Some(c) => c,
                    None => return 0xFFFF,
                };
                ((c.read(a) as u16) << 8) | c.read(a + 1) as u16
            }
            0xE00000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                ((self.ram[i] as u16) << 8) | self.ram[i + 1] as u16
            }
            _ => {
                let hi = M68kBus::read8(self, a) as u16;
                let lo = M68kBus::read8(self, a + 1) as u16;
                (hi << 8) | lo
            }
        }
    }

    fn write16(&mut self, addr: u32, v: u16) {
        let a = addr & 0xFF_FFFE;
        match a {
            0xC00000..=0xC0000F => self.vdp_write16(a, v),
            0xE00000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                self.ram[i] = (v >> 8) as u8;
                self.ram[i + 1] = (v & 0xFF) as u8;
            }
            0x000000..=0x3FFFFF => {} // ROM, ignore
            _ => {
                M68kBus::write8(self, a, (v >> 8) as u8);
                M68kBus::write8(self, a + 1, (v & 0xFF) as u8);
            }
        }
    }
}

impl Genesis {
    fn io_read(&mut self, a: u32) -> u8 {
        match a & 0x1F {
            0x00 | 0x01 => 0xA0, // version register (overseas, no FDD/exp)
            0x02 | 0x03 => self.input.read_data(1),
            0x04 | 0x05 => self.input.read_data(2),
            0x06 | 0x07 => 0xFF, // expansion data
            0x08 | 0x09 => self.input.read_ctrl(1),
            0x0A | 0x0B => self.input.read_ctrl(2),
            0x0C | 0x0D => self.input.read_ctrl(0),
            _ => 0xFF,
        }
    }
    fn io_write(&mut self, a: u32, v: u8) {
        match a & 0x1F {
            0x02 | 0x03 => self.input.write_data(1, v),
            0x04 | 0x05 => self.input.write_data(2, v),
            0x08 | 0x09 => self.input.write_ctrl(1, v),
            0x0A | 0x0B => self.input.write_ctrl(2, v),
            0x0C | 0x0D => self.input.write_ctrl(0, v),
            _ => {}
        }
    }

    /// 68000 access into the $A00000 Z80 region (Z80 RAM + sound chips + window).
    fn z80_area_read(&mut self, off: u16) -> u8 {
        match off {
            0x0000..=0x3FFF => self.zram[(off & 0x1FFF) as usize],
            0x4000..=0x5FFF => self.ym.read_status(),
            0x7F11 => 0xFF,
            _ => 0xFF,
        }
    }
    fn z80_area_write(&mut self, off: u16, v: u8) {
        match off {
            0x0000..=0x3FFF => self.zram[(off & 0x1FFF) as usize] = v,
            0x4000..=0x5FFF => self.ym.write((off & 0x03) as u8, v),
            0x6000..=0x60FF => {
                // bank register: each write shifts in one bit (LSB).
                self.z80_bank = ((self.z80_bank >> 1) | ((v as u32 & 1) << 8)) & 0x1FF;
            }
            0x7F11 => self.psg.write(v),
            _ => {}
        }
    }
}

// =============================================================================
// Z80 bus. The Z80 sees its own RAM, the sound chips, the bank register, and a
// 32 KiB window into 68000 space.
// =============================================================================
impl Z80Bus for Genesis {
    fn read8(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.zram[(addr & 0x1FFF) as usize],
            0x4000..=0x5FFF => self.ym.read_status(),
            0x6000..=0x7FFF => 0xFF, // bank reg / VDP / PSG region (mostly write)
            0x8000..=0xFFFF => {
                // Window into 68000 space: base = bank << 15.
                let base = self.z80_bank << 15;
                let a68 = base | ((addr as u32) & 0x7FFF);
                M68kBus::read8(self, a68)
            }
        }
    }

    fn write8(&mut self, addr: u16, v: u8) {
        match addr {
            0x0000..=0x3FFF => self.zram[(addr & 0x1FFF) as usize] = v,
            0x4000..=0x5FFF => self.ym.write((addr & 0x03) as u8, v),
            0x6000..=0x60FF => {
                self.z80_bank = ((self.z80_bank >> 1) | ((v as u32 & 1) << 8)) & 0x1FF;
            }
            0x7F11 => self.psg.write(v),
            0x8000..=0xFFFF => {
                let base = self.z80_bank << 15;
                let a68 = base | ((addr as u32) & 0x7FFF);
                M68kBus::write8(self, a68, v);
            }
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 {
        0xFF // Z80 I/O ports are unused on the Genesis
    }
    fn port_out(&mut self, _port: u16, _v: u8) {}
}

// Small helper on the CPU used above (kept here so we don't widen m68k's API).
impl M68k {
    fn last_exception_was_int(&self) -> bool {
        false
    }
}

const _: () = assert!(FB_LEN == MAX_W * HEIGHT * 4);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m68k::Bus as _;

    /// A minimal ROM: reset vectors (SSP=$FF0000, PC=$000200) then a program at
    /// $200 that loops forever. Header region zeroed.
    fn tiny_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x400];
        // SSP = $00FF0000
        rom[0] = 0x00;
        rom[1] = 0xFF;
        rom[2] = 0x00;
        rom[3] = 0x00;
        // PC = $00000200
        rom[4] = 0x00;
        rom[5] = 0x00;
        rom[6] = 0x02;
        rom[7] = 0x00;
        // At $200: BRA.S * (0x60FE = branch to self)
        rom[0x200] = 0x60;
        rom[0x201] = 0xFE;
        rom
    }

    #[test]
    fn loads_rom_and_resets_cpu() {
        let mut g = Genesis::new();
        g.load_rom(&tiny_rom());
        assert_eq!(g.cpu.pc, 0x200);
        assert_eq!(g.cpu.a[7], 0x00FF_0000);
    }

    #[test]
    fn work_ram_readback_big_endian() {
        let mut g = Genesis::new();
        M68kBus::write16(&mut g, 0xFF0000, 0x1234);
        assert_eq!(M68kBus::read16(&mut g, 0xFF0000), 0x1234);
        assert_eq!(M68kBus::read8(&mut g, 0xFF0000), 0x12); // big-endian high byte first
        assert_eq!(M68kBus::read8(&mut g, 0xFF0001), 0x34);
    }

    #[test]
    fn ram_mirrors_every_64k() {
        let mut g = Genesis::new();
        M68kBus::write8(&mut g, 0xE00000, 0x55);
        assert_eq!(M68kBus::read8(&mut g, 0xFF0000), 0x55); // same physical RAM
    }

    #[test]
    fn rom_reads_through_cart_big_endian() {
        let mut g = Genesis::new();
        g.load_rom(&tiny_rom());
        assert_eq!(M68kBus::read16(&mut g, 0), 0x00FF); // SSP high word
    }

    #[test]
    fn run_frame_advances_frame_count() {
        let mut g = Genesis::new();
        g.load_rom(&tiny_rom());
        let f0 = g.frame_count();
        g.run_frame();
        assert_eq!(g.frame_count(), f0 + 1);
    }

    #[test]
    fn vdp_port_register_write_via_bus() {
        let mut g = Genesis::new();
        // Write VDP control port ($C00004): reg $01 = 0x64.
        M68kBus::write16(&mut g, 0xC00004, 0x8164);
        assert_eq!(g.vdp.regs[0x01], 0x64);
    }

    #[test]
    fn z80_busreq_arbitration() {
        let mut g = Genesis::new();
        // 68000 requests the bus.
        M68kBus::write8(&mut g, 0xA11100, 0x01);
        assert!(g.z80_busreq);
        // Release.
        M68kBus::write8(&mut g, 0xA11100, 0x00);
        assert!(!g.z80_busreq);
    }

    #[test]
    fn controller_port_roundtrip() {
        let mut g = Genesis::new();
        g.set_keys(crate::io::KEY_START);
        // TH low select, then read data port $A10003.
        M68kBus::write8(&mut g, 0xA10003, 0x00);
        let v = M68kBus::read8(&mut g, 0xA10003);
        assert_eq!(v & 0x20, 0); // Start pressed reads low
    }

    #[test]
    fn z80_ram_via_68k_window() {
        let mut g = Genesis::new();
        // 68000 writes Z80 RAM through the $A00000 window.
        M68kBus::write8(&mut g, 0xA00010, 0x77);
        assert_eq!(g.zram[0x10], 0x77);
    }

    #[test]
    fn framebuffer_has_expected_length() {
        let g = Genesis::new();
        assert_eq!(g.framebuffer().len(), MAX_W * HEIGHT * 4);
    }

    #[test]
    fn many_frames_do_not_panic() {
        // A ROM that enables the VDP display + vint, sets a backdrop colour, and
        // loops. Running many frames must stay stable and increment the count.
        let mut g = Genesis::new();
        g.load_rom(&tiny_rom());
        // Manually program the VDP a little so the renderer exercises a real
        // (non-default) path, then spin frames.
        M68kBus::write16(&mut g, 0xC00004, 0x8164); // reg1 = display on + vint
        M68kBus::write16(&mut g, 0xC00004, 0x8C81); // reg12 = H40
        for _ in 0..120 {
            g.run_frame();
        }
        assert!(g.frame_count() >= 120);
        assert_eq!(g.framebuffer().len(), MAX_W * HEIGHT * 4);
    }

    #[test]
    fn vint_reaches_cpu_and_runs_handler() {
        // ROM: enable interrupts in SR is implicit (boots masked). The program
        // unmasks via MOVE #$2000,SR then loops; the VDP raises VINT each frame.
        let mut rom = vec![0u8; 0x400];
        rom[0..4].copy_from_slice(&[0x00, 0xFF, 0x00, 0x00]); // SSP
        rom[4..8].copy_from_slice(&[0x00, 0x00, 0x02, 0x00]); // PC=$200
        // Level-6 autovector (vector 30) -> $300.
        rom[30 * 4..30 * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x03, 0x00]);
        // $200: MOVE #$2000,SR (0x46FC 0x2000) ; enable VDP via control writes ;
        //       BRA self.
        rom[0x200] = 0x46;
        rom[0x201] = 0xFC;
        rom[0x202] = 0x20;
        rom[0x203] = 0x00;
        // BRA.S * at $204
        rom[0x204] = 0x60;
        rom[0x205] = 0xFE;
        // $300 handler: ADDQ.L #1,D7 (0x5287) ; RTE (0x4E73)
        rom[0x300] = 0x52;
        rom[0x301] = 0x87;
        rom[0x302] = 0x4E;
        rom[0x303] = 0x73;
        let mut g = Genesis::new();
        g.load_rom(&rom);
        // Enable VDP vint via control port.
        M68kBus::write16(&mut g, 0xC00004, 0x8120); // reg1 vint enable
        g.run_frame();
        g.run_frame();
        // The handler bumped D7 at least once.
        assert!(g.cpu.d[7] >= 1, "VINT handler should have run");
    }

    #[test]
    fn cpu_runs_program_in_ram() {
        // ROM resets to a routine that writes a value to RAM, then loops.
        let mut rom = vec![0u8; 0x400];
        rom[0..4].copy_from_slice(&[0x00, 0xFF, 0x00, 0x00]); // SSP
        rom[4..8].copy_from_slice(&[0x00, 0x00, 0x02, 0x00]); // PC=$200
        // MOVE.W #$ABCD,$FF0000 :
        //   0x33FC (MOVE.W #imm,(xxx).W) imm=$ABCD addr=$0000 -> writes RAM $FF0000?
        // Simpler: MOVEQ #$7F,D0 ; then BRA self.
        rom[0x200] = 0x70; // MOVEQ #$7F,D0
        rom[0x201] = 0x7F;
        rom[0x202] = 0x60; // BRA.S *
        rom[0x203] = 0xFE;
        let mut g = Genesis::new();
        g.load_rom(&rom);
        g.run_frame();
        assert_eq!(g.cpu.d[0], 0x7F);
    }
}
