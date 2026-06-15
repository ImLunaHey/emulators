//! The `Nes` god-struct: owns the CPU, PPU, APU, cartridge, RAM, and
//! controllers; implements the CPU `Bus` and the PPU `PpuBus`; and runs frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one god-struct
//! owns every subsystem. Cross-subsystem calls take `&mut` references as
//! parameters rather than storing them. The borrow knot — the CPU needs the
//! whole machine as its `Bus`, and the PPU needs the cart as its `PpuBus` — is
//! resolved by `mem::take`-ing the active subsystem out of `self` and passing
//! `self` (or a sub-borrow) as the collaborator.

use crate::apu::Apu;
use crate::bus::Bus;
use crate::cart::{Cart, CartError, Mirroring};
use crate::cpu::Cpu;
use crate::input::Controllers;
use crate::ppu::{Ppu, PpuBus, FB_LEN, SCREEN_H, SCREEN_W};

/// PPU dots per CPU cycle (NTSC).
const DOTS_PER_CPU: u32 = 3;

/// A latched CPU fault, captured for the crash screen. The 6502's JAM/KIL
/// illegal opcodes hard-halt the processor; we detect one and freeze.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    /// The JAM/KIL opcode that halted the CPU.
    pub opcode: u8,
    /// Program counter of the halting opcode.
    pub pc: u16,
}

pub struct Nes {
    pub cpu: Cpu,
    pub ppu: Ppu,
    pub apu: Apu,
    pub cart: Option<Cart>,
    pub controllers: Controllers,

    /// 2 KiB internal RAM (mirrored to $1FFF).
    ram: Box<[u8; 0x800]>,

    /// True while an OAM DMA ($4014) is being serviced this step.
    oam_dma_page: Option<u8>,

    /// Set once the CPU executes a JAM/KIL illegal opcode. While set,
    /// [`Nes::run_frame`] freezes the CPU and re-presents the crash screen.
    pub fault: Option<Fault>,
}

impl Default for Nes {
    fn default() -> Self {
        Nes::new()
    }
}

impl Nes {
    pub fn new() -> Nes {
        Nes {
            cpu: Cpu::new(),
            ppu: Ppu::new(),
            apu: Apu::new(),
            cart: None,
            controllers: Controllers::new(),
            ram: vec![0u8; 0x800].into_boxed_slice().try_into().unwrap(),
            oam_dma_page: None,
            fault: None,
        }
    }

    /// Load an iNES / NES 2.0 ROM and reset the CPU to its vector.
    pub fn load_rom(&mut self, bytes: &[u8]) -> Result<(), CartError> {
        let cart = Cart::from_ines(bytes)?;
        self.cart = Some(cart);
        self.ppu = Ppu::new();
        self.apu = Apu::new();
        self.cpu = Cpu::new();
        self.fault = None;
        // Reset reads the reset vector through the bus.
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.reset(self);
        self.cpu = cpu;
        Ok(())
    }

    pub fn set_keys(&mut self, buttons: u8) {
        self.controllers.set_keys(0, buttons);
    }
    pub fn set_keys_port(&mut self, port: usize, buttons: u8) {
        self.controllers.set_keys(port, buttons);
    }

    pub fn framebuffer(&self) -> &[u8] {
        self.ppu.framebuffer()
    }

    pub fn frame_count(&self) -> u64 {
        self.ppu.frame
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.apu.drain()
    }

    /// Run one full video frame: step the CPU, and for each CPU cycle advance
    /// the PPU 3 dots and the APU 1 cycle, until the PPU completes a frame.
    pub fn run_frame(&mut self) {
        if self.cart.is_none() {
            return;
        }
        // Already faulted: freeze the CPU and keep presenting the crash screen.
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        let start_frame = self.ppu.frame;
        // Safety valve: a frame is ~29780 CPU cycles; cap to avoid a runaway
        // loop if rendering is disabled (then `frame` still advances via the
        // dot counter, so this rarely trips).
        let mut guard = 0u32;
        while self.ppu.frame == start_frame && guard < 200_000 {
            self.step_instruction();
            guard += 1;
            // The CPU latches a JAM/KIL illegal opcode (hard halt). Capture the
            // fault, draw the crash screen, and stop running this frame.
            if let Some((opcode, pc)) = self.cpu.jam {
                self.fault = Some(Fault { opcode, pc });
                self.present_crash();
                return;
            }
        }
    }

    /// Draw the crash screen into the PPU framebuffer from the latched
    /// [`Fault`]. Called every frame once faulted so the panel stays presented.
    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "NES CORE FAULT".to_string(),
            format!("ILLEGAL OP {:02X}", f.opcode),
            format!("PC {:04X}", f.pc),
        ];
        crate::crash::render(
            &mut self.ppu.framebuffer[..],
            SCREEN_W,
            SCREEN_H,
            &lines,
        );
    }

    /// Step one CPU instruction and clock the PPU/APU for its cycles.
    fn step_instruction(&mut self) {
        // Sample the PPU's NMI line into the CPU before executing.
        if self.ppu.nmi_signal {
            self.ppu.nmi_signal = false;
            self.cpu.nmi_pending = true;
        }
        // Mapper (MMC3) + APU frame IRQ feed the CPU IRQ line (level).
        let mapper_irq = self
            .cart
            .as_mut()
            .map(|c| c.take_irq())
            .unwrap_or(false);
        self.cpu.irq_line = mapper_irq || self.apu.frame_irq;

        let mut cpu = std::mem::take(&mut self.cpu);
        let cycles = cpu.step(self);
        self.cpu = cpu;

        // Service a deferred OAM DMA (513/514 cycles); fold it into clocking.
        let mut total = cycles;
        if let Some(page) = self.oam_dma_page.take() {
            self.do_oam_dma(page);
            total += 514;
        }

        for _ in 0..total {
            self.clock_ppu_apu();
        }
    }

    #[inline]
    fn clock_ppu_apu(&mut self) {
        // 3 PPU dots.
        let mut ppu = std::mem::take(&mut self.ppu);
        for _ in 0..DOTS_PER_CPU {
            ppu.step(self);
        }
        self.ppu = ppu;
        // 1 APU cycle.
        self.apu.step();
    }

    fn do_oam_dma(&mut self, page: u8) {
        let base = (page as u16) << 8;
        for i in 0..256u16 {
            let v = self.read8(base + i);
            self.ppu.oam_dma_byte(i as u8, v);
        }
    }

    // ---- debug accessors ----
    pub fn dbg_read8(&mut self, addr: u16) -> u8 {
        self.read8(addr)
    }
}

// =============================================================================
// CPU memory map: the `Bus` trait the CPU codes against.
//   $0000-$1FFF  2 KiB internal RAM, mirrored every $800
//   $2000-$3FFF  PPU registers, mirrored every 8
//   $4000-$4017  APU + IO registers
//   $4018-$401F  test mode (ignored)
//   $4020-$FFFF  cartridge (PRG ROM/RAM, mapper) via Cart
// =============================================================================
impl Bus for Nes {
    fn read8(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => {
                // PPU registers need the PPU's PpuBus (= self). Take the PPU out.
                let mut ppu = std::mem::take(&mut self.ppu);
                let v = ppu.read_reg(self, addr & 7);
                self.ppu = ppu;
                v
            }
            0x4016 => self.controllers.read(0),
            0x4017 => self.controllers.read(1),
            0x4015 => self.apu.read_status(),
            0x4000..=0x4014 | 0x4018..=0x401F => 0, // write-only / open bus
            0x4020..=0xFFFF => self
                .cart
                .as_mut()
                .map(|c| c.cpu_read(addr))
                .unwrap_or(0),
        }
    }

    fn write8(&mut self, addr: u16, v: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize] = v,
            0x2000..=0x3FFF => {
                let mut ppu = std::mem::take(&mut self.ppu);
                ppu.write_reg(self, addr & 7, v);
                self.ppu = ppu;
            }
            0x4014 => {
                // OAM DMA: defer the 256-byte copy to the instruction boundary
                // so it clocks the right number of cycles.
                self.oam_dma_page = Some(v);
            }
            0x4016 => self.controllers.write_strobe(v),
            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_reg(addr, v),
            0x4018..=0x401F => {}
            0x4020..=0xFFFF => {
                if let Some(c) = self.cart.as_mut() {
                    c.cpu_write(addr, v);
                }
            }
        }
    }
}

// =============================================================================
// PPU bus: CHR (pattern tables) + nametable mirroring, routed via the cart.
// =============================================================================
impl PpuBus for Nes {
    fn chr_read(&mut self, addr: u16) -> u8 {
        self.cart.as_mut().map(|c| c.chr_read(addr)).unwrap_or(0)
    }
    fn chr_write(&mut self, addr: u16, v: u8) {
        if let Some(c) = self.cart.as_mut() {
            c.chr_write(addr, v);
        }
    }
    fn mirroring(&mut self) -> Mirroring {
        self.cart
            .as_ref()
            .map(|c| c.mirroring())
            .unwrap_or(Mirroring::Horizontal)
    }
    fn ppu_a12(&mut self, addr: u16) {
        if let Some(c) = self.cart.as_mut() {
            c.ppu_a12_clock(addr);
        }
    }
}

// `mem::take` needs `Default` on the swapped-out subsystems; the framebuffer
// box dominates Ppu's default cost but it's only a per-instruction swap of a
// pointer-sized field set (the box itself moves, not its contents).
const _: () = assert!(FB_LEN == 256 * 240 * 4);

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny NROM ROM: 1×16KiB PRG, 1×8KiB CHR, horizontal mirroring.
    fn build_nrom(prg_fill: impl Fn(&mut [u8])) -> Vec<u8> {
        let mut rom = Vec::new();
        rom.extend_from_slice(b"NES\x1A");
        rom.push(1); // 1×16KiB PRG
        rom.push(1); // 1×8KiB CHR
        rom.push(0); // flags6: horizontal, mapper 0
        rom.push(0); // flags7
        rom.extend_from_slice(&[0u8; 8]);
        let mut prg = vec![0u8; 16 * 1024];
        prg_fill(&mut prg);
        // Reset vector -> $8000.
        prg[0x3FFC] = 0x00;
        prg[0x3FFD] = 0x80;
        rom.extend_from_slice(&prg);
        rom.extend_from_slice(&[0u8; 8 * 1024]); // CHR
        rom
    }

    #[test]
    fn ines_parse_and_reset_vector() {
        let rom = build_nrom(|_| {});
        let mut nes = Nes::new();
        nes.load_rom(&rom).unwrap();
        assert_eq!(nes.cpu.pc, 0x8000);
        assert_eq!(nes.cart.as_ref().unwrap().mapper_id, 0);
    }

    #[test]
    fn ram_mirroring() {
        let mut nes = Nes::new();
        nes.write8(0x0000, 0x42);
        assert_eq!(nes.read8(0x0800), 0x42); // mirror
        assert_eq!(nes.read8(0x1000), 0x42);
        assert_eq!(nes.read8(0x1800), 0x42);
    }

    #[test]
    fn runs_a_simple_program() {
        // LDA #$05; STA $0010; loop forever.
        let rom = build_nrom(|prg| {
            prg[0x0000] = 0xA9; // LDA #$05
            prg[0x0001] = 0x05;
            prg[0x0002] = 0x85; // STA $10
            prg[0x0003] = 0x10;
            prg[0x0004] = 0x4C; // JMP $8004 (spin)
            prg[0x0005] = 0x04;
            prg[0x0006] = 0x80;
        });
        let mut nes = Nes::new();
        nes.load_rom(&rom).unwrap();
        // A handful of instructions is enough to execute LDA/STA.
        for _ in 0..10 {
            nes.step_instruction();
        }
        assert_eq!(nes.read8(0x0010), 0x05);
    }

    #[test]
    fn run_frame_advances_frame_count() {
        let rom = build_nrom(|prg| {
            prg[0x0000] = 0x4C; // JMP $8000 (spin)
            prg[0x0001] = 0x00;
            prg[0x0002] = 0x80;
        });
        let mut nes = Nes::new();
        nes.load_rom(&rom).unwrap();
        // Enable rendering so the PPU's dot counter drives frames.
        nes.write8(0x2001, 0x18);
        let f0 = nes.frame_count();
        nes.run_frame();
        assert_eq!(nes.frame_count(), f0 + 1);
    }

    #[test]
    fn jam_opcode_faults_and_draws_crash_screen() {
        // A JAM/KIL illegal opcode (0x02) at the reset vector should hard-halt
        // the CPU: latch a fault and paint the crash screen.
        let rom = build_nrom(|prg| {
            prg[0x0000] = 0x02; // JAM
        });
        let mut nes = Nes::new();
        nes.load_rom(&rom).unwrap();
        // Enable rendering so the frame loop is exercised; the JAM should
        // short-circuit it before a frame completes.
        nes.write8(0x2001, 0x18);
        assert!(nes.fault.is_none());
        nes.run_frame();

        // Fault captured with the opcode and the PC of the JAM ($8000).
        let f = nes.fault.expect("JAM should set a fault");
        assert_eq!(f.opcode, 0x02);
        assert_eq!(f.pc, 0x8000);

        // The framebuffer is the dark-blue crash background plus white text:
        // it must contain at least one non-background (white text) pixel.
        let fb = nes.framebuffer();
        let bg = [0x10u8, 0x10, 0x60, 0xFF];
        let has_text = fb.chunks_exact(4).any(|px| px != bg);
        assert!(has_text, "crash screen should have non-background text pixels");

        // Re-running keeps the CPU frozen and re-presents the crash screen.
        let f0 = nes.frame_count();
        nes.run_frame();
        assert_eq!(nes.frame_count(), f0, "CPU should be frozen after fault");
        assert!(nes.fault.is_some());
    }

    #[test]
    fn oam_dma_copies_page() {
        let rom = build_nrom(|_| {});
        let mut nes = Nes::new();
        nes.load_rom(&rom).unwrap();
        // Fill page $02 in RAM.
        for i in 0..256u16 {
            nes.write8(0x0200 + i, i as u8);
        }
        nes.write8(0x4014, 0x02); // trigger DMA
        nes.do_oam_dma(0x02);
        // OAM[0] should be byte 0 of the page.
        assert_eq!(nes.ppu.oam[0], 0);
        assert_eq!(nes.ppu.oam[5], 5);
    }
}
