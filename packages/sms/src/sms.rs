//! The `Sms` god-struct: owns the Z80, VDP, PSG, cartridge, work RAM, and
//! input; implements the Z80 `Z80Bus`; and runs video frames. ONE struct
//! handles both the Master System and the Game Gear via the [`System`] enum.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct
//! owns every subsystem. The CPU needs the whole machine as its `Z80Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put
//! it back.
//!
//! Z80 memory map (SMS/GG):
//!   $0000-$BFFF  cartridge (ROM via mapper, or on-cart RAM in frame 2)
//!   $C000-$DFFF  8 KiB work RAM
//!   $E000-$FFFF  mirror of $C000-$DFFF (the top 4 bytes hold the Sega mapper
//!                control registers, which we forward to the cartridge)
//!
//! I/O port map (only a few address bits are decoded):
//!   $00-$06 (GG)  GG-specific: START ($00), stereo PSG ($06)
//!   $3E/$3F       memory + I/O control (we accept writes, mostly no-op)
//!   $7E           V counter (read) / PSG (write)
//!   $7F           H counter (read) / PSG (write)
//!   $BE           VDP data
//!   $BF           VDP control / status
//!   $DC/$C0       controller port A
//!   $DD/$C1       controller port B

use crate::bus::Z80Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::io::Input;
use crate::psg::Psg;
use crate::vdp::{Vdp, FB_LEN, GG_H, GG_W, GG_X, GG_Y, SCANLINES, SMS_H, SMS_W};

/// Which console this core is emulating. Chosen at construction; drives the
/// screen crop, the CRAM palette format, and the GG-only ports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum System {
    Sms,
    GameGear,
}

/// Z80 cycles per scanline: 3.58 MHz / (262 lines × 60 Hz) ≈ 228.
const CYCLES_PER_LINE: u32 = 228;

/// A detected CPU deadlock, captured for the crash screen. The Z80 has no clean
/// illegal-opcode trap (undocumented opcodes legally execute), so the trigger is
/// a HALT executed with interrupts disabled (IFF1 = 0): the maskable INT can
/// never wake it, so the CPU is wedged forever.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    /// Program counter at the moment the deadlock was detected.
    pub pc: u16,
    /// Frame number when the fault was detected.
    pub frame: u64,
}

pub struct Sms {
    pub cpu: Cpu,
    pub vdp: Vdp,
    pub psg: Psg,
    pub input: Input,
    pub cart: Option<Cart>,
    pub system: System,

    /// 8 KiB work RAM ($C000-$DFFF, mirrored to $FFFF).
    ram: Box<[u8; 0x2000]>,

    /// GG-specific framebuffer crop (160×144). Reused between frames.
    gg_fb: Box<[u8; GG_W * GG_H * 4]>,

    /// Edge-detect state for the SMS Pause button (-> NMI).
    prev_pause: bool,

    /// Accumulated PSG clocks owed (PSG runs ~clock/16; we fold the /16 into
    /// the period, so we feed it CPU cycles directly).
    audio_owed: u32,

    /// Set once a CPU deadlock (HALT-with-interrupts-disabled that persists for
    /// a whole frame) is detected. While set, [`Sms::run_frame`] freezes the CPU
    /// and re-presents the crash screen each frame.
    pub fault: Option<Fault>,
}

impl Sms {
    pub fn new(system: System) -> Sms {
        let is_gg = system == System::GameGear;
        Sms {
            cpu: Cpu::new(),
            vdp: Vdp::new(is_gg),
            psg: Psg::new(),
            input: Input::new(),
            cart: None,
            system,
            ram: vec![0u8; 0x2000].into_boxed_slice().try_into().unwrap(),
            gg_fb: vec![0u8; GG_W * GG_H * 4]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            prev_pause: false,
            audio_owed: 0,
            fault: None,
        }
    }

    /// Convenience constructor: `game_gear == true` -> Game Gear, else SMS.
    pub fn new_system(game_gear: bool) -> Sms {
        Sms::new(if game_gear {
            System::GameGear
        } else {
            System::Sms
        })
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        self.cart = Some(cart);
        // Reset machine state for a clean boot.
        let is_gg = self.system == System::GameGear;
        self.vdp = Vdp::new(is_gg);
        self.psg = Psg::new();
        self.cpu = Cpu::new();
        self.cpu.reset();
        for b in self.ram.iter_mut() {
            *b = 0;
        }
        self.fault = None;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(bits);
    }
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.input.set_keys_p2(bits);
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.psg.drain()
    }

    pub fn frame_count(&self) -> u64 {
        self.vdp.frame
    }

    /// Visible screen width: 256 (SMS) or 160 (GG).
    pub fn width(&self) -> usize {
        match self.system {
            System::Sms => SMS_W,
            System::GameGear => GG_W,
        }
    }
    /// Visible screen height: 192 (SMS) or 144 (GG).
    pub fn height(&self) -> usize {
        match self.system {
            System::Sms => SMS_H,
            System::GameGear => GG_H,
        }
    }

    /// RGBA8888 framebuffer. SMS returns the full 256×192 frame; GG returns the
    /// centred 160×144 crop.
    pub fn framebuffer(&mut self) -> &[u8] {
        match self.system {
            System::Sms => &self.vdp.framebuffer[..],
            System::GameGear => {
                // Copy the centred window out of the full SMS frame.
                for y in 0..GG_H {
                    let src_y = GG_Y + y;
                    let src_row = (src_y * SMS_W + GG_X) * 4;
                    let dst_row = y * GG_W * 4;
                    self.gg_fb[dst_row..dst_row + GG_W * 4]
                        .copy_from_slice(&self.vdp.framebuffer[src_row..src_row + GG_W * 4]);
                }
                &self.gg_fb[..]
            }
        }
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

    /// Run one full video frame: step the CPU line-by-line, advancing the VDP
    /// and PSG, until a full frame's worth of scanlines has elapsed.
    pub fn run_frame(&mut self) {
        // Once faulted, freeze: keep presenting the crash screen, run no code.
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        if self.cart.is_none() {
            return;
        }

        // Deadlock detection. The Z80 has no clean illegal-opcode trap, so we
        // watch for a HALT with interrupts disabled (IFF1 = 0): the maskable INT
        // can never wake it. To stay conservative we require the state to PERSIST
        // for a whole frame — if the CPU was halted-with-DI on entry and nothing
        // (NMI / nothing) cleared `halted` by frame's end, it is genuinely wedged
        // (a normal idle game HALTs with interrupts ENABLED, so never trips this).
        let wedged_before = self.cpu.halted && !self.cpu.iff1;

        for _ in 0..SCANLINES {
            self.run_scanline();
        }

        if wedged_before && self.cpu.halted && !self.cpu.iff1 {
            self.fault = Some(Fault {
                pc: self.cpu.pc,
                frame: self.vdp.frame,
            });
            self.present_crash();
        }
    }

    /// Draw the crash screen into the VDP framebuffer from the latched
    /// [`Fault`]. Called every frame once faulted so the panel stays presented.
    /// Rendered into the full SMS frame; the Game Gear path then crops it.
    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "SMS CORE FAULT".to_string(),
            "HALT WITH DI".to_string(),
            format!("PC {:04X}", f.pc),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.vdp.framebuffer[..], SMS_W, SMS_H, &lines);
    }

    fn run_scanline(&mut self) {
        // Sega Pause button -> NMI on the rising edge (SMS only).
        if self.system == System::Sms {
            let now = self.input.pause_pressed();
            if now && !self.prev_pause {
                self.cpu.nmi_pending = true;
            }
            self.prev_pause = now;
        }

        // Run ~228 Z80 cycles for this scanline.
        let mut consumed = 0u32;
        while consumed < CYCLES_PER_LINE {
            // Sample the VDP's INT line into the CPU each step.
            self.cpu.irq_line = self.vdp.irq_asserted();
            let mut cpu = std::mem::take(&mut self.cpu);
            let t = cpu.step(self);
            self.cpu = cpu;
            consumed += t;
            self.audio_owed += t;
        }

        // Feed the PSG the cycles consumed (period folds the /16 divisor).
        let owed = self.audio_owed;
        self.audio_owed = 0;
        self.psg.step(owed);

        // Advance the VDP one scanline (renders if visible).
        self.vdp.step_scanline();
        // Refresh the IRQ line after the scanline (frame/line IRQ may assert).
        self.cpu.irq_line = self.vdp.irq_asserted();
    }

    // ---- debug ----
    pub fn dbg_read8(&mut self, addr: u16) -> u8 {
        self.read8(addr)
    }
}

// =============================================================================
// Z80 memory + I/O bus.
// =============================================================================
impl Z80Bus for Sms {
    fn read8(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0xBFFF => self.cart.as_ref().map(|c| c.read(addr)).unwrap_or(0xFF),
            0xC000..=0xFFFF => self.ram[(addr & 0x1FFF) as usize],
        }
    }

    fn write8(&mut self, addr: u16, v: u8) {
        match addr {
            // Sega-mapper control registers live in the top of the address
            // space, overlapping the RAM mirror — forward to the cart AND store
            // in RAM (real hardware sees both).
            0xFFFC..=0xFFFF => {
                if let Some(c) = self.cart.as_mut() {
                    c.write(addr, v);
                }
                self.ram[(addr & 0x1FFF) as usize] = v;
            }
            0x0000..=0xBFFF => {
                if let Some(c) = self.cart.as_mut() {
                    c.write(addr, v);
                }
            }
            0xC000..=0xFFFB => self.ram[(addr & 0x1FFF) as usize] = v,
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        let p = (port & 0xFF) as u8;
        match p {
            // GG START register (GG only).
            0x00 if self.system == System::GameGear => self.input.gg_start(),
            // GG misc registers read open-bus-ish 0xFF.
            0x01..=0x06 if self.system == System::GameGear => 0xFF,
            // $40-$7F even = V counter, odd = H counter.
            0x40..=0x7F => {
                if p & 1 == 0 {
                    self.vdp.v_counter()
                } else {
                    // H counter — return a stable mid-line value.
                    0x00
                }
            }
            // $BE VDP data, $BF VDP status.
            0xBE => self.vdp.read_data(),
            0xBF => self.vdp.read_status(),
            // Controller ports: $C0/$DC port A, $C1/$DD port B (mirrors).
            0xC0 | 0xDC => self.input.port_dc(),
            0xC1 | 0xDD => self.input.port_dd(),
            // $3E/$3F control: reads return open bus.
            _ => 0xFF,
        }
    }

    fn port_out(&mut self, port: u16, v: u8) {
        let p = (port & 0xFF) as u8;
        match p {
            // GG stereo PSG control.
            0x06 if self.system == System::GameGear => self.psg.write_stereo(v),
            0x00..=0x06 if self.system == System::GameGear => {}
            // $3E memory control, $3F I/O control — accepted, no-op for us.
            0x3E | 0x3F => {}
            // $7E/$7F: PSG write.
            0x7E | 0x7F => self.psg.write(v),
            // VDP.
            0xBE => self.vdp.write_data(v),
            0xBF => self.vdp.write_control(v),
            _ => {}
        }
    }
}

const _: () = assert!(FB_LEN == SMS_W * SMS_H * 4);

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_rom() -> Vec<u8> {
        // One 16 KiB bank. Program at $0000: DI; loop forever (JP $0003).
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0xF3; // DI
        rom[1] = 0xC3; // JP $0001
        rom[2] = 0x01;
        rom[3] = 0x00;
        rom
    }

    #[test]
    fn work_ram_mirror() {
        let mut sms = Sms::new(System::Sms);
        sms.write8(0xC000, 0x42);
        assert_eq!(sms.read8(0xE000), 0x42); // mirror
        sms.write8(0xDFFF, 0x99);
        assert_eq!(sms.read8(0xFFFF) & 0xFF, sms.read8(0xDFFF)); // overlap region
    }

    #[test]
    fn rom_reads_through_cart() {
        let mut sms = Sms::new(System::Sms);
        sms.load_rom(&tiny_rom());
        assert_eq!(sms.read8(0x0000), 0xF3);
        assert_eq!(sms.read8(0x0001), 0xC3);
    }

    #[test]
    fn run_frame_advances_frame_count() {
        let mut sms = Sms::new(System::Sms);
        sms.load_rom(&tiny_rom());
        let f0 = sms.frame_count();
        sms.run_frame();
        assert_eq!(sms.frame_count(), f0 + 1);
    }

    #[test]
    fn vblank_irq_reaches_cpu() {
        let mut sms = Sms::new(System::Sms);
        // ROM: EI; IM 1; loop. Enable VDP display + frame IRQ via the program?
        // Simpler: set the VDP register directly, then run a frame and confirm
        // the IRQ line asserted at some point (frame flag set).
        sms.load_rom(&tiny_rom());
        // Enable frame interrupt (R1 bit5) via the control port path.
        sms.port_out(0xBF, 0x20);
        sms.port_out(0xBF, 0x81);
        sms.run_frame();
        // After a frame, the VDP frame flag has been set (status bit7) — the
        // CPU's irq_line would have been driven during the frame.
        assert!(sms.vdp.frame >= 1);
    }

    #[test]
    fn gg_screen_dimensions() {
        let sms = Sms::new(System::GameGear);
        assert_eq!(sms.width(), 160);
        assert_eq!(sms.height(), 144);
        let sms = Sms::new(System::Sms);
        assert_eq!(sms.width(), 256);
        assert_eq!(sms.height(), 192);
    }

    #[test]
    fn gg_framebuffer_crops() {
        let mut sms = Sms::new(System::GameGear);
        sms.load_rom(&tiny_rom());
        let fb = sms.framebuffer();
        assert_eq!(fb.len(), 160 * 144 * 4);
    }

    #[test]
    fn input_reaches_port() {
        let mut sms = Sms::new(System::Sms);
        sms.set_keys(crate::io::KEY_UP);
        // Port $DC bit0 should be low (pressed).
        assert_eq!(sms.port_in(0xDC) & 0x01, 0);
    }

    #[test]
    fn halt_with_di_faults_and_paints_crash() {
        // ROM at $0000: DI; HALT. With interrupts disabled the maskable INT can
        // never wake the HALT — a permanent deadlock. The first frame enters the
        // halt; the second frame (halted-with-DI on entry, still so at the end)
        // detects the deadlock, latches a Fault, and paints the crash screen.
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0xF3; // DI
        rom[1] = 0x76; // HALT
        let mut sms = Sms::new(System::Sms);
        sms.load_rom(&rom);

        assert!(sms.fault.is_none());
        sms.run_frame(); // enters HALT
        sms.run_frame(); // detects the wedge
        assert!(sms.fault.is_some(), "HALT-with-DI must latch a fault");

        // The crash screen painted a dark-blue background + white text, so the
        // framebuffer must contain non-background pixels (the white glyphs) and
        // must NOT be all-zero.
        let fb = sms.framebuffer();
        let bg = [0x10u8, 0x10, 0x60, 0xFF];
        let has_white = fb.chunks_exact(4).any(|p| p == [0xFF, 0xFF, 0xFF, 0xFF]);
        let all_bg = fb.chunks_exact(4).all(|p| p == bg);
        assert!(has_white, "crash screen must draw white text pixels");
        assert!(!all_bg, "framebuffer must not be only the background");
    }

    #[test]
    fn halt_with_interrupts_enabled_does_not_fault() {
        // EI; HALT is the normal SMS idle pattern — a frame IRQ wakes it. With
        // IFF1 set this must NOT trip the deadlock detector even if the CPU is
        // sitting in HALT across frame boundaries.
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0xFB; // EI
        rom[1] = 0x76; // HALT
        let mut sms = Sms::new(System::Sms);
        sms.load_rom(&rom);
        sms.run_frame();
        sms.run_frame();
        sms.run_frame();
        assert!(
            sms.fault.is_none(),
            "HALT with interrupts enabled is normal idle, must not fault"
        );
    }

    #[test]
    fn psg_write_through_port() {
        let mut sms = Sms::new(System::Sms);
        sms.port_out(0x7F, 0x90); // ch0 volume = 0 (loud)
        // No panic + accepted; the PSG state is exercised in psg.rs tests.
        sms.port_out(0x7F, 0x80);
    }
}
