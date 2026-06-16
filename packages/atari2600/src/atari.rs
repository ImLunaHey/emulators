//! The `Atari` god-struct: owns the 6507 CPU, the TIA, the RIOT, the
//! cartridge, and input; implements the CPU `Bus`; and runs video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one god-struct
//! owns every subsystem. The CPU needs the whole machine as its `Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back. No `Rc`/`RefCell`.
//!
//! CPU memory map (the 6507 only wires the low 13 address bits, so everything
//! mirrors heavily; we mask to 13 bits and decode from there):
//!   - `A12 == 0` and `A7 == 0`  -> TIA registers (A5..0 decode the register)
//!   - `A12 == 0` and `A7 == 1`  -> RIOT (A9 selects RAM at $80-$FF vs the I/O
//!     and timer block at $280-$2FF)
//!   - `A12 == 1`                -> cartridge ROM window ($1000-$1FFF / $F000)
//!
//! The lockstep is the whole point of the machine: one CPU cycle = three TIA
//! colour clocks. `run_frame` executes one instruction, clocks the TIA 3× its
//! cycles and the RIOT 1× its cycles, then — if the instruction wrote WSYNC —
//! free-runs the TIA to the end of the scanline while the CPU is stalled.

use crate::bus::Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::riot::Riot;
use crate::tia::{Tia, FB_LEN, SAMPLE_RATE, VISIBLE_LINES, VISIBLE_W};

/// TIA colour clocks per CPU cycle.
const COLOR_CLOCKS_PER_CPU: u32 = 3;

/// Host audio sample rate (re-exported for callers wiring up audio output).
#[allow(dead_code)]
pub const AUDIO_SAMPLE_RATE: u32 = SAMPLE_RATE;

// Input bit layout for `set_keys` (matches the sibling cores' Up/Down/Left/
// Right/Fire ordering, plus the console switches).
const KEY_UP: u32 = 1 << 0;
const KEY_DOWN: u32 = 1 << 1;
const KEY_LEFT: u32 = 1 << 2;
const KEY_RIGHT: u32 = 1 << 3;
const KEY_FIRE: u32 = 1 << 4;
const KEY_RESET: u32 = 1 << 5;
const KEY_SELECT: u32 = 1 << 6;

/// A latched CPU fault, captured for the crash screen. The 6507's JAM/KIL
/// illegal opcodes hard-halt the processor; we detect one and freeze.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub opcode: u8,
    pub pc: u16,
}

pub struct Atari {
    pub cpu: Cpu,
    pub tia: Tia,
    pub riot: Riot,
    pub cart: Option<Cart>,

    /// Player-1/2 joystick + console-switch bitmask (see the `KEY_*` consts).
    keys_p0: u32,
    keys_p1: u32,

    pub fault: Option<Fault>,
}

impl Default for Atari {
    fn default() -> Self {
        Atari::new()
    }
}

impl Atari {
    pub fn new() -> Atari {
        Atari {
            cpu: Cpu::new(),
            tia: Tia::new(),
            riot: Riot::new(),
            cart: None,
            keys_p0: 0,
            keys_p1: 0,
            fault: None,
        }
    }

    /// Load a 2K/4K/8K/16K/32K cartridge image and reset the CPU to its vector.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.cart = Some(Cart::load(bytes));
        self.tia = Tia::new();
        self.riot = Riot::new();
        self.cpu = Cpu::new();
        self.fault = None;
        self.apply_input();
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.reset(self);
        self.cpu = cpu;
    }

    /// Player-1 input bitmask (Up/Down/Left/Right/Fire + Reset/Select).
    pub fn set_keys(&mut self, bits: u32) {
        self.keys_p0 = bits;
        self.apply_input();
    }

    /// Player-2 joystick bitmask (same Up/Down/Left/Right/Fire ordering; the
    /// console switches are shared and read from the player-1 mask).
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.keys_p1 = bits;
        self.apply_input();
    }

    pub fn framebuffer(&self) -> &[u8] {
        self.tia.framebuffer()
    }

    pub fn frame_count(&self) -> u64 {
        self.tia.frame
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.tia.drain_audio()
    }

    pub fn width(&self) -> usize {
        VISIBLE_W
    }

    pub fn height(&self) -> usize {
        VISIBLE_LINES
    }

    /// Translate the host input bitmasks into the SWCHA/SWCHB/INPT latches.
    fn apply_input(&mut self) {
        // SWCHA: bits 7-4 = player 1 (right joystick), bits 3-0 = player 2
        // (left joystick), per the Programmer's Guide. A line reads 0 when the
        // direction is pressed.
        //   P1: bit7 up, bit6 down, bit5 left, bit4 right
        //   P2: bit3 up, bit2 down, bit1 left, bit0 right
        let mut swcha: u8 = 0xFF;
        let p0 = self.keys_p0;
        if p0 & KEY_UP != 0 {
            swcha &= !0x80;
        }
        if p0 & KEY_DOWN != 0 {
            swcha &= !0x40;
        }
        if p0 & KEY_LEFT != 0 {
            swcha &= !0x20;
        }
        if p0 & KEY_RIGHT != 0 {
            swcha &= !0x10;
        }
        let p1 = self.keys_p1;
        if p1 & KEY_UP != 0 {
            swcha &= !0x08;
        }
        if p1 & KEY_DOWN != 0 {
            swcha &= !0x04;
        }
        if p1 & KEY_LEFT != 0 {
            swcha &= !0x02;
        }
        if p1 & KEY_RIGHT != 0 {
            swcha &= !0x01;
        }
        self.riot.swcha = swcha;

        // SWCHB: bit0 reset, bit1 select (0 = pressed). bit3 colour/BW (1 =
        // colour), bits 6/7 difficulty (1 = B). We keep colour + difficulty at
        // their released defaults and toggle reset/select from the input mask.
        let mut swchb: u8 = 0b1100_1011;
        if p0 & KEY_RESET != 0 {
            swchb &= !0x01;
        }
        if p0 & KEY_SELECT != 0 {
            swchb &= !0x02;
        }
        self.riot.swchb = swchb;

        // Fire buttons -> INPT4/INPT5.
        self.tia
            .set_fire(p0 & KEY_FIRE != 0, p1 & KEY_FIRE != 0);
    }

    /// Run one full video frame: execute instructions, clocking the TIA 3× and
    /// the RIOT 1× per CPU cycle, until the TIA finishes a frame.
    pub fn run_frame(&mut self) {
        if self.cart.is_none() {
            return;
        }
        self.apply_input();

        if self.fault.is_some() {
            self.present_crash();
            return;
        }

        self.tia.frame_done = false;
        // Safety valve: a frame is ~19912 CPU cycles. Cap the instruction count
        // generously to avoid a runaway loop if the program never lets the beam
        // complete a frame.
        let mut guard = 0u32;
        while !self.tia.frame_done && guard < 100_000 {
            self.step_instruction();
            guard += 1;
            if let Some((opcode, pc)) = self.cpu.jam {
                self.fault = Some(Fault { opcode, pc });
                self.present_crash();
                return;
            }
        }
    }

    /// Step one CPU instruction and clock the TIA/RIOT for its cycles, then
    /// honour any WSYNC by free-running the TIA to the end of the scanline.
    fn step_instruction(&mut self) {
        let mut cpu = std::mem::take(&mut self.cpu);
        let cycles = cpu.step(self) as u32;
        self.cpu = cpu;

        self.clock(cycles);

        // WSYNC: the CPU is stalled until the TIA reaches the start of the next
        // scanline. The TIA clears `wsync` itself when the line wraps.
        let mut wsync_guard = 0u32;
        while self.tia.wsync && wsync_guard < CLOCKS_GUARD {
            self.tia.tick();
            wsync_guard += 1;
        }
    }

    /// Advance the TIA by `3 * cycles` colour clocks and the RIOT by `cycles`.
    fn clock(&mut self, cycles: u32) {
        for _ in 0..(cycles * COLOR_CLOCKS_PER_CPU) {
            self.tia.tick();
        }
        self.riot.step(cycles);
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "ATARI 2600 FAULT".to_string(),
            format!("ILLEGAL OP {:02X}", f.opcode),
            format!("PC {:04X}", f.pc),
        ];
        crate::crash::render(
            &mut self.tia.framebuffer[..],
            VISIBLE_W,
            VISIBLE_LINES,
            &lines,
        );
    }

    // ---- debug accessor ----
    pub fn dbg_read8(&mut self, addr: u16) -> u8 {
        self.read8(addr)
    }
}

/// Upper bound on colour clocks to free-run during a single WSYNC stall (a
/// little over one scanline of slack).
const CLOCKS_GUARD: u32 = 300;

// =============================================================================
// CPU memory map. The 6507 wires only A0-A12, so we mask to 13 bits and decode.
// =============================================================================
impl Bus for Atari {
    fn read8(&mut self, addr: u16) -> u8 {
        let a = addr & 0x1FFF;
        if a & 0x1000 != 0 {
            // Cartridge window.
            return self
                .cart
                .as_mut()
                .map(|c| c.read(a))
                .unwrap_or(0);
        }
        if a & 0x0080 == 0 {
            // TIA registers (read map: collisions + input ports in A0-A3).
            self.tia.read(a & 0x0F)
        } else if a & 0x0200 == 0 {
            // RIOT RAM ($80-$FF).
            self.riot.ram[(a & 0x7F) as usize]
        } else {
            // RIOT I/O + timer ($280-$2FF).
            self.riot.read(a & 0x07)
        }
    }

    fn write8(&mut self, addr: u16, v: u8) {
        let a = addr & 0x1FFF;
        if a & 0x1000 != 0 {
            // Writes into the cartridge window can trip bank-switch hotspots.
            if let Some(c) = self.cart.as_mut() {
                c.write(a);
            }
            return;
        }
        if a & 0x0080 == 0 {
            // TIA registers (write map decodes A0-A5).
            self.tia.write(a & 0x3F, v);
        } else if a & 0x0200 == 0 {
            // RIOT RAM.
            self.riot.ram[(a & 0x7F) as usize] = v;
        } else {
            // RIOT I/O + timer.
            self.riot.write(a & 0x1F, v);
        }
    }
}

const _: () = assert!(FB_LEN == VISIBLE_W * VISIBLE_LINES * 4);

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 4K ROM with a fill closure; sets the reset vector to $F000.
    fn build_rom(fill: impl Fn(&mut [u8])) -> Vec<u8> {
        let mut rom = vec![0u8; 4096];
        fill(&mut rom);
        // Reset vector at $FFFC/$FFFD (offset $FFC/$FFD in the 4K image) -> $F000.
        rom[0x0FFC] = 0x00;
        rom[0x0FFD] = 0xF0;
        rom
    }

    #[test]
    fn loads_rom_and_sets_reset_vector() {
        let rom = build_rom(|_| {});
        let mut a = Atari::new();
        a.load_rom(&rom);
        assert_eq!(a.cpu.pc, 0xF000);
    }

    #[test]
    fn riot_ram_read_write() {
        let mut a = Atari::new();
        a.write8(0x0080, 0x5A);
        assert_eq!(a.read8(0x0080), 0x5A);
        // Mirrored: $0180 maps to the same RAM (A7 set, A9 clear, A12 clear).
        assert_eq!(a.read8(0x0180), 0x5A);
    }

    #[test]
    fn cartridge_read_through_window() {
        let rom = build_rom(|r| {
            r[0x0000] = 0xAB;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        assert_eq!(a.read8(0xF000), 0xAB);
        // Mirror: $1000 (13-bit) hits the same window.
        assert_eq!(a.read8(0x1000), 0xAB);
    }

    #[test]
    fn runs_a_simple_program() {
        // LDA #$05 ; STA $80 ; JMP self
        let rom = build_rom(|r| {
            r[0x0000] = 0xA9; // LDA #$05
            r[0x0001] = 0x05;
            r[0x0002] = 0x85; // STA $80
            r[0x0003] = 0x80;
            r[0x0004] = 0x4C; // JMP $F004
            r[0x0005] = 0x04;
            r[0x0006] = 0xF0;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        for _ in 0..10 {
            a.step_instruction();
        }
        assert_eq!(a.read8(0x0080), 0x05);
    }

    #[test]
    fn wsync_stalls_to_line_end() {
        // STA WSYNC ($02) ; JMP self. After a WSYNC, the TIA must be at the
        // start of a fresh scanline (clock 0) when the CPU resumes.
        let rom = build_rom(|r| {
            r[0x0000] = 0x85; // STA $02 (WSYNC)
            r[0x0001] = 0x02;
            r[0x0002] = 0x4C; // JMP $F000
            r[0x0003] = 0x00;
            r[0x0004] = 0xF0;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        a.step_instruction(); // STA WSYNC -> stalls to line end
        assert!(!a.tia.wsync, "WSYNC should have released");
    }

    #[test]
    fn run_frame_advances_frame_count() {
        // Spin forever; the TIA's free-running beam still completes a frame.
        let rom = build_rom(|r| {
            r[0x0000] = 0x4C; // JMP $F000
            r[0x0001] = 0x00;
            r[0x0002] = 0xF0;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        let f0 = a.frame_count();
        a.run_frame();
        assert_eq!(a.frame_count(), f0 + 1);
    }

    #[test]
    fn jam_faults_and_draws_crash_screen() {
        let rom = build_rom(|r| {
            r[0x0000] = 0x02; // JAM
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        assert!(a.fault.is_none());
        a.run_frame();
        let f = a.fault.expect("JAM should fault");
        assert_eq!(f.opcode, 0x02);
        assert_eq!(f.pc, 0xF000);
        // Crash screen has non-background pixels.
        let fb = a.framebuffer();
        let bg = [0x10u8, 0x10, 0x60, 0xFF];
        assert!(fb.chunks_exact(4).any(|px| px != bg));
        // Re-running keeps the CPU frozen.
        let fc = a.frame_count();
        a.run_frame();
        assert_eq!(a.frame_count(), fc);
    }

    #[test]
    fn input_maps_to_swcha() {
        let mut a = Atari::new();
        a.set_keys(KEY_LEFT);
        // P1 left -> SWCHA bit 5 cleared.
        assert_eq!(a.riot.swcha & 0x20, 0x00);
        a.set_keys(0);
        assert_eq!(a.riot.swcha, 0xFF);
    }

    #[test]
    fn fire_maps_to_inpt4() {
        let mut a = Atari::new();
        a.set_keys(KEY_FIRE);
        assert_eq!(a.tia.read(0x0C) & 0x80, 0x00); // INPT4 pressed -> bit7 low
        a.set_keys(0);
        assert_eq!(a.tia.read(0x0C) & 0x80, 0x80);
    }

    #[test]
    fn reset_switch_maps_to_swchb() {
        let mut a = Atari::new();
        a.set_keys(KEY_RESET);
        assert_eq!(a.riot.swchb & 0x01, 0x00);
    }

    #[test]
    fn renders_a_colored_picture() {
        // A minimal "kernel": set the background colour and a solid playfield,
        // then spin. The free-running beam should paint the whole visible window
        // with non-black pixels, proving the TIA renderer produces a picture.
        let rom = build_rom(|r| {
            r[0x0000] = 0xA9; // LDA #$1E  (bright background hue)
            r[0x0001] = 0x1E;
            r[0x0002] = 0x85; // STA COLUBK ($09)
            r[0x0003] = 0x09;
            r[0x0004] = 0xA9; // LDA #$FF  (all playfield bits)
            r[0x0005] = 0xFF;
            r[0x0006] = 0x85; // STA PF0 ($0D)
            r[0x0007] = 0x0D;
            r[0x0008] = 0x85; // STA PF1 ($0E)
            r[0x0009] = 0x0E;
            r[0x000A] = 0x85; // STA PF2 ($0F)
            r[0x000B] = 0x0F;
            r[0x000C] = 0xA9; // LDA #$0E  (white playfield)
            r[0x000D] = 0x0E;
            r[0x000E] = 0x85; // STA COLUPF ($08)
            r[0x000F] = 0x08;
            r[0x0010] = 0x4C; // JMP self
            r[0x0011] = 0x10;
            r[0x0012] = 0xF0;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        // Run several frames so the registers are set and the beam paints.
        for _ in 0..3 {
            a.run_frame();
        }
        let fb = a.framebuffer();
        // Count non-black pixels: a painted picture has many.
        let lit = fb.chunks_exact(4).filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0).count();
        assert!(
            lit > VISIBLE_W * VISIBLE_LINES / 2,
            "expected the visible window to be painted, only {lit} lit pixels"
        );
    }

    #[test]
    fn produces_audio_samples() {
        let rom = build_rom(|r| {
            // Set channel 0 to a pure tone at full volume, then spin.
            r[0x0000] = 0xA9; // LDA #$0C
            r[0x0001] = 0x0C;
            r[0x0002] = 0x8D; // STA AUDC0 ($15)
            r[0x0003] = 0x15;
            r[0x0004] = 0x00;
            r[0x0005] = 0xA9; // LDA #$0F
            r[0x0006] = 0x0F;
            r[0x0007] = 0x8D; // STA AUDV0 ($19)
            r[0x0008] = 0x19;
            r[0x0009] = 0x00;
            r[0x000A] = 0x4C; // JMP self
            r[0x000B] = 0x0A;
            r[0x000C] = 0xF0;
        });
        let mut a = Atari::new();
        a.load_rom(&rom);
        a.run_frame();
        let audio = a.drain_audio();
        // One NTSC frame ≈ 735 samples at 44.1 KHz.
        assert!(audio.len() > 500, "expected ~735 samples, got {}", audio.len());
    }
}
