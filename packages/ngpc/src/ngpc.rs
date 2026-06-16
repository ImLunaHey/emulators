//! The `Ngpc` god-struct: owns the TLCS-900/H main CPU, the Z80 sound CPU, the
//! K1GE/K2GE video controller, the T6W28 PSG, the cartridge, work RAM, the
//! shared sound RAM, the system registers (input/timers/interrupt control), and
//! implements the CPU `Bus`; runs video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. The CPU needs the whole machine as its `Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back. No `Rc`/`RefCell`.
//!
//! Memory map (TLCS-900 24-bit space):
//!   0x000000-0x0000FF  internal CPU I/O registers (timers, DMA, PSG/DAC ports,
//!                      Z80 control) — we model the sound + control bytes used
//!   0x004000-0x006BFF  main work RAM (12 KiB on-chip; 0x4000-0x6FFF)
//!   0x006C00-0x006FFF  BIOS workspace + system registers (0x6Fxx: input, RTC,
//!                      interrupt vectors/levels)
//!   0x007000-0x007FFF  shared sound RAM (Z80 0x0000-0x0FFF)
//!   0x008000-0x00BFFF  K1GE/K2GE registers + palette + tilemaps + patterns
//!   0x200000-0x3FFFFF  cartridge ROM (window 1)
//!   0x800000-0x9FFFFF  cartridge ROM (window 2)
//!   0xFF0000-0xFFFFFF  BIOS ROM (we HLE the reset vector / boot)

use crate::cart::Cart;
use crate::cpu::bus::Bus;
use crate::cpu::Cpu;
use crate::input::Input;
use crate::psg::Psg;
use crate::video::{Video, FB_LEN, HEIGHT, TOTAL_LINES, WIDTH};
use crate::z80::Cpu as Z80;

/// TLCS-900/H cycles per scanline ≈ 6.144 MHz / (199 lines × 60 Hz) ≈ 515.
const CYCLES_PER_LINE: u32 = 515;

/// V-blank interrupt level + vector (NGPC: INT4). The BIOS would program the
/// real vector table; we HLE it to a fixed handler-address slot the game writes
/// at the 0x6FCC system-register vector.
const VBLANK_LEVEL: u8 = 5;

/// A latched CPU fault (illegal/unimplemented opcode), surfaced as a crash
/// screen so the host shows a legible readout instead of a frozen frame.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub pc: u32,
    pub frame: u64,
}

pub struct Ngpc {
    pub cpu: Cpu,
    pub z80: Z80,
    pub video: Video,
    pub psg: Psg,
    pub input: Input,
    pub cart: Option<Cart>,

    /// 12 KiB main work RAM (0x4000-0x6FFF). Boxed.
    ram: Box<[u8; 0x3000]>,
    /// 4 KiB shared sound RAM (0x7000-0x7FFF).
    sound_ram: Box<[u8; 0x1000]>,
    /// 256-byte internal CPU I/O register file (0x0000-0x00FF).
    io: Box<[u8; 0x100]>,
    /// System register file (0x6F00-0x6FFF).
    sysreg: Box<[u8; 0x100]>,

    /// Communication latch between the main CPU and the Z80 (port 0xBC).
    pub comm: u8,
    /// Z80 enabled/running (port 0xB8/0xB9).
    pub z80_enabled: bool,

    /// Accumulated audio clocks owed to the PSG.
    audio_owed: u32,

    pub fault: Option<Fault>,
}

impl Ngpc {
    pub fn new() -> Ngpc {
        Ngpc {
            cpu: Cpu::new(),
            z80: Z80::new(),
            video: Video::new(true),
            psg: Psg::new(),
            input: Input::new(),
            cart: None,
            ram: vec![0u8; 0x3000].into_boxed_slice().try_into().unwrap(),
            sound_ram: vec![0u8; 0x1000].into_boxed_slice().try_into().unwrap(),
            io: vec![0u8; 0x100].into_boxed_slice().try_into().unwrap(),
            sysreg: vec![0u8; 0x100].into_boxed_slice().try_into().unwrap(),
            comm: 0,
            z80_enabled: false,
            audio_owed: 0,
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        let color = cart.color;
        let entry = cart.entry;
        self.cart = Some(cart);
        // Reset machine state for a clean boot.
        self.video = Video::new(color);
        self.psg = Psg::new();
        self.cpu = Cpu::new();
        self.z80 = Z80::new();
        for b in self.ram.iter_mut() {
            *b = 0;
        }
        for b in self.sound_ram.iter_mut() {
            *b = 0;
        }
        for b in self.io.iter_mut() {
            *b = 0;
        }
        for b in self.sysreg.iter_mut() {
            *b = 0;
        }
        self.fault = None;
        // HLE boot: the BIOS sets the stack to the top of RAM, drops the
        // interrupt mask so the game's V-blank handler runs, and jumps to the
        // cart's header entry point.
        self.cpu.set_xsp(0x006C00);
        self.cpu.set_ilm(0); // allow maskable interrupts
        // 0x6F95 = display-mode mirror (0x10 = colour unit).
        self.sysreg[0x95] = if color { 0x10 } else { 0x00 };
        self.cpu.pc = entry & 0xFF_FFFF;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(bits);
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.psg.drain()
    }

    pub fn frame_count(&self) -> u64 {
        self.video.frame
    }

    pub fn width(&self) -> usize {
        WIDTH
    }
    pub fn height(&self) -> usize {
        HEIGHT
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.video.framebuffer[..]
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
        for _ in 0..TOTAL_LINES {
            self.run_scanline();
            if self.fault.is_some() {
                self.present_crash();
                return;
            }
        }
    }

    fn run_scanline(&mut self) {
        let mut consumed = 0u32;
        while consumed < CYCLES_PER_LINE {
            // Latch the pending interrupt level/vector into the CPU.
            if self.video.vblank_irq {
                self.cpu.int_request = VBLANK_LEVEL;
                // Vector slot 0x6FCC holds the game's V-blank handler address.
                self.cpu.int_vector = 0x006FCC;
                self.video.vblank_irq = false;
            }
            let mut cpu = std::mem::take(&mut self.cpu);
            let (t, illegal) = cpu.step(self);
            self.cpu = cpu;
            if illegal {
                self.fault = Some(Fault {
                    pc: self.cpu.pc(),
                    frame: self.video.frame,
                });
                return;
            }
            consumed += t;
            self.audio_owed += t;
        }
        // Feed the PSG.
        let owed = self.audio_owed;
        self.audio_owed = 0;
        self.psg.step(owed);
        // Advance the video one scanline.
        self.video.step_line();
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "NGPC CORE FAULT".to_string(),
            "ILLEGAL OPCODE".to_string(),
            format!("PC {:06X}", f.pc),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.video.framebuffer[..], WIDTH, HEIGHT, &lines);
    }
}

impl Default for Ngpc {
    fn default() -> Self {
        Ngpc::new()
    }
}

// =============================================================================
// TLCS-900/H memory bus.
// =============================================================================
impl Bus for Ngpc {
    fn read8(&mut self, addr: u32) -> u8 {
        let a = addr & 0xFF_FFFF;
        match a {
            0x000000..=0x0000FF => self.io[a as usize],
            0x004000..=0x006BFF => self.ram[(a - 0x4000) as usize],
            0x006C00..=0x006FFF => {
                // System register window. Input register at 0x6F82.
                match a {
                    0x006F82 => self.input.register(),
                    _ => self.sysreg[(a & 0xFF) as usize],
                }
            }
            0x007000..=0x007FFF => self.sound_ram[(a - 0x7000) as usize],
            0x008000..=0x00BFFF => self.video.read(a),
            0x200000..=0x3FFFFF => self.cart.as_ref().map(|c| c.read(a - 0x200000)).unwrap_or(0xFF),
            0x800000..=0x9FFFFF => self.cart.as_ref().map(|c| c.read(a - 0x800000)).unwrap_or(0xFF),
            _ => 0xFF,
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        let a = addr & 0xFF_FFFF;
        match a {
            0x000000..=0x0000FF => {
                self.io[a as usize] = v;
                match a {
                    0x0000A0 => self.psg.write_right(v),
                    0x0000A1 => self.psg.write_left(v),
                    0x0000A2 => self.psg.write_dac_l(v),
                    0x0000A3 => self.psg.write_dac_r(v),
                    0x0000B8 | 0x0000B9 => self.z80_enabled = v != 0,
                    0x0000BC => self.comm = v,
                    _ => {}
                }
            }
            0x004000..=0x006BFF => self.ram[(a - 0x4000) as usize] = v,
            0x006C00..=0x006FFF => self.sysreg[(a & 0xFF) as usize] = v,
            0x007000..=0x007FFF => self.sound_ram[(a - 0x7000) as usize] = v,
            0x008000..=0x00BFFF => self.video.write(a, v),
            // Cartridge flash-command writes accepted + ignored.
            _ => {}
        }
    }
}

const _: () = assert!(FB_LEN == WIDTH * HEIGHT * 4);

#[cfg(test)]
mod tests {
    use super::*;

    fn header_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1_0000];
        // Entry = 0x200200; colour.
        rom[0x1C] = 0x00;
        rom[0x1D] = 0x02;
        rom[0x1E] = 0x20;
        rom[0x23] = 0x10;
        // Program at offset 0x200: NOP loop (0x00 ; JR T,-2 -> 0x68 0xFE).
        rom[0x200] = 0x00; // NOP
        rom[0x201] = 0x68; // JR T (cc=8), d8
        rom[0x202] = 0xFD; // -3 -> back to 0x200
        rom
    }

    #[test]
    fn work_ram_roundtrip() {
        let mut n = Ngpc::new();
        n.write8(0x004100, 0x42);
        assert_eq!(n.read8(0x004100), 0x42);
    }

    #[test]
    fn sound_ram_roundtrip() {
        let mut n = Ngpc::new();
        n.write8(0x007010, 0x99);
        assert_eq!(n.read8(0x007010), 0x99);
    }

    #[test]
    fn input_register_reads_through() {
        let mut n = Ngpc::new();
        n.set_keys(crate::input::KEY_A);
        assert_eq!(n.read8(0x006F82) & 0x10, 0x10);
    }

    #[test]
    fn rom_reads_through_both_windows() {
        let mut n = Ngpc::new();
        n.load_rom(&header_rom());
        // Header byte 0x23 = 0x10 at ROM offset 0x23 -> window1 0x200023.
        assert_eq!(n.read8(0x200023), 0x10);
        assert_eq!(n.read8(0x800023), 0x10);
    }

    #[test]
    fn load_rom_sets_entry_pc() {
        let mut n = Ngpc::new();
        n.load_rom(&header_rom());
        assert_eq!(n.cpu.pc(), 0x200200);
        assert!(n.video.color);
    }

    #[test]
    fn run_frame_advances_count_and_runs_cpu() {
        let mut n = Ngpc::new();
        n.load_rom(&header_rom());
        let f0 = n.frame_count();
        n.run_frame();
        assert_eq!(n.frame_count(), f0 + 1);
        // The NOP/JR loop must keep PC in the program region (no fault).
        assert!(n.fault.is_none());
    }

    #[test]
    fn psg_write_through_io_port() {
        let mut n = Ngpc::new();
        n.write8(0x0000A1, 0x90); // PSG left: ch0 volume
        n.write8(0x0000A2, 0xFF); // DAC L
        // No panic; values land.
        assert_eq!(n.io[0xA2], 0xFF);
    }

    #[test]
    fn z80_control_port() {
        let mut n = Ngpc::new();
        n.write8(0x0000B8, 0x01);
        assert!(n.z80_enabled);
    }
}
