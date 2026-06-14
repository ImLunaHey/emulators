//! The `Gbc` god-struct: owns the CPU, memory, cart, interrupt controller, and
//! a slot per IO subsystem. It implements [`Bus`], routing the full 16-bit
//! address space across `Memory` (internal RAM), `Cart` (ROM/external RAM via
//! the MBC), and the IO devices.
//!
//! Spec: Pan Docs — Memory Map. Borrow strategy mirrors the other cores:
//! everything reachable through the bus stays owned by `Gbc`; a subsystem
//! method that itself needs `&mut dyn Bus` is reached by `mem::take`-ing the
//! device out, calling it with `self` as the bus, then putting it back. The
//! frame loop / step driver lands with `cpu::exec`; this phase wires ownership
//! and the routing.

use crate::bus::Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::interrupts::{Interrupt, Irq};
use crate::memory::Memory;
use crate::ppu::Mode;
use crate::regions as R;

/// T-cycles in one frame at the base clock (154 lines × 456 dots).
const FRAME_CYCLES: u32 = 70224;

/// The Game Boy Color machine. One owner of every subsystem.
pub struct Gbc {
    pub cpu: Cpu,
    pub mem: Memory,
    pub cart: Cart,
    pub irq: Irq,

    // IO subsystems. Empty structs until their files are ported; reserved here
    // so the bus IO dispatch has a stable owner per device.
    pub ppu: crate::ppu::Ppu,
    pub apu: crate::apu::Apu,
    pub timer: crate::timer::Timer,
    pub dma: crate::dma::Dma,
    pub joypad: crate::joypad::Joypad,
    pub serial: crate::serial::Serial,

    /// Generic backing store for IO registers without a modeled side effect
    /// (0xFF00-0xFF7F). Registers handled by a device are routed before this.
    pub io_raw: [u8; R::IO_SIZE],

    /// Completed-frame counter (bumped each `run_frame`).
    pub frame_count: u32,
    /// Latched pressed-button state, applied to the joypad each step so the
    /// joypad interrupt fires within the bus.
    pub keys: u8,
}

impl Default for Gbc {
    fn default() -> Self {
        Gbc::new()
    }
}

impl Gbc {
    pub fn new() -> Self {
        Gbc {
            cpu: Cpu::new(),
            mem: Memory::new(),
            cart: Cart::empty(),
            irq: Irq::new(),
            ppu: crate::ppu::Ppu::default(),
            apu: crate::apu::Apu::default(),
            timer: crate::timer::Timer::default(),
            dma: crate::dma::Dma::default(),
            joypad: crate::joypad::Joypad::default(),
            serial: crate::serial::Serial::default(),
            io_raw: [0; R::IO_SIZE],
            frame_count: 0,
            keys: 0,
        }
    }

    /// Mount a ROM image. Parses the header, decodes the MBC, sizes external
    /// RAM, and resets the CPU to its post-boot CGB register state. The PPU's
    /// CGB-color path is enabled for CGB carts; DMG carts fall back to the
    /// greyscale (DMG-compat) palette path the CGB boot ROM would otherwise set.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.cart.load_rom(bytes);
        let cgb = self.cart.header.is_cgb();
        self.cpu = Cpu::new();
        self.ppu = crate::ppu::Ppu::new();
        self.ppu.cgb_mode = cgb;
        if !cgb {
            // DMG-compat: seed grey BG/OBJ palettes so a DMG game that never
            // writes the CGB palette ports still shows shaded output. The DMG
            // BGP/OBP registers (used by the DMG render path) drive the actual
            // shades; these CGB-RAM seeds are only used if the game flips into
            // the CGB path, which DMG carts never do.
            for pal in 0..8 {
                for c in 0..4 {
                    let shade = 31 - (c as u16) * 10;
                    let rgb555 = shade | (shade << 5) | (shade << 10);
                    let idx = pal * 8 + c * 2;
                    self.mem.bg_palette[idx] = rgb555 as u8;
                    self.mem.bg_palette[idx + 1] = (rgb555 >> 8) as u8;
                    self.mem.obj_palette[idx] = rgb555 as u8;
                    self.mem.obj_palette[idx + 1] = (rgb555 >> 8) as u8;
                }
            }
        }
    }

    /// Whether the machine is running in CGB double-speed mode (KEY1 bit 7).
    #[inline]
    fn double_speed(&self) -> bool {
        self.mem.key1 & 0x80 != 0
    }

    /// Set the pressed-button state (host input). Bits per `joypad::button`:
    /// A,B,Select,Start,Right,Left,Up,Down (bit 0..7).
    pub fn set_keys(&mut self, bits: u8) {
        self.keys = bits;
        let mut jp = core::mem::take(&mut self.joypad);
        jp.set_keys(bits, &mut self.irq);
        self.joypad = jp;
    }

    /// The 160×144 RGBA8888 framebuffer.
    #[inline]
    pub fn framebuffer(&self) -> &[u8] {
        &self.ppu.framebuffer
    }

    /// Drain interleaved-stereo f32 audio samples produced since the last call.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.apu.drain()
    }

    /// Completed-frame counter.
    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Run the machine for one full video frame (~70224 base T-cycles), stepping
    /// CPU + PPU + timer + serial + APU + HDMA in lockstep. Double-speed scales
    /// the CPU/timer/serial rates (twice the work per frame) but not the PPU/APU
    /// output clock.
    pub fn run_frame(&mut self) {
        self.ppu.frame_ready = false;
        let mut budget = FRAME_CYCLES;
        // Safety bound so a runaway never spins forever.
        let mut guard = 0u32;
        while !self.ppu.frame_ready && budget > 0 && guard < 4_000_000 {
            guard += 1;
            let used = self.step();
            budget = budget.saturating_sub(used);
        }
        self.frame_count = self.frame_count.wrapping_add(1);
    }

    // ---- battery save passthrough ----
    pub fn save_ram(&self) -> &[u8] {
        self.cart.save_ram()
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        self.cart.load_save_ram(bytes);
    }
    pub fn save_dirty(&self) -> bool {
        self.cart.ram_dirty
    }
    pub fn clear_save_dirty(&mut self) {
        self.cart.ram_dirty = false;
    }

    /// Convenience: raise an interrupt request from inside the orchestrator.
    #[inline]
    pub fn request_interrupt(&mut self, int: Interrupt) {
        self.irq.request(int);
    }

    /// Advance the machine by one CPU instruction and clock the sub-devices for
    /// the cycles it consumed. Returns the CPU T-cycles spent (double-speed
    /// aware). The CPU/IRQ are reached via `mem::take` so `self` can serve as
    /// the bus during execution.
    pub fn step(&mut self) -> u32 {
        let mut cpu = core::mem::take(&mut self.cpu);
        let mut irq = self.irq;
        let cpu_cycles = cpu.step(self, &mut irq);
        self.cpu = cpu;
        self.irq = irq;

        // Devices clocked at the base rate advance by base cycles; in
        // double-speed the CPU issued twice as many T-cycles for the same wall
        // time, so the PPU/APU see half. The timer/serial run at the CPU rate.
        let base_cycles = if self.double_speed() {
            cpu_cycles / 2
        } else {
            cpu_cycles
        };

        // Timer + serial run at the CPU clock (so they're double-speed-aware via
        // the raw cpu_cycles count).
        let mut timer = core::mem::take(&mut self.timer);
        timer.step(cpu_cycles, &mut self.irq);
        self.timer = timer;

        let mut serial = core::mem::take(&mut self.serial);
        serial.step(cpu_cycles, &mut self.irq);
        self.serial = serial;

        // PPU + APU run at the fixed base clock.
        let prev_mode = self.ppu.mode();
        let mut ppu = core::mem::take(&mut self.ppu);
        ppu.step(base_cycles, &self.mem, &mut self.irq);
        let entered_hblank = ppu.entered_hblank && prev_mode != Mode::HBlank;
        self.ppu = ppu;

        self.apu.step(base_cycles);

        // CGB H-Blank DMA: transfer one block each time we just entered mode 0.
        if entered_hblank {
            let mut dma = core::mem::take(&mut self.dma);
            dma.hdma_hblank_step(self);
            self.dma = dma;
        }

        cpu_cycles
    }

    // ---- IO register dispatch (0xFF00-0xFF7F) ----
    // Registers owned by a modeled device route to it; everything else mirrors
    // in `io_raw`. The PPU/APU/timer/etc. reads/writes are todo!() seams until
    // those modules land — the *routing* belongs to the foundation.
    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            R::REG_IF => self.irq.read_if(),

            // CGB bank selects + palette + double-speed (modeled in `Memory`).
            R::REG_VBK => self.mem.read_vbk(),
            R::REG_SVBK => self.mem.read_svbk(),
            R::REG_BCPS => self.mem.bcps,
            R::REG_BCPD => self.mem.read_bg_palette_data(),
            R::REG_OCPS => self.mem.ocps,
            R::REG_OCPD => self.mem.read_obj_palette_data(),
            R::REG_KEY1 => self.mem.key1 | 0x7E, // unused bits read as 1

            // Device-owned registers.
            0xFF00 => self.joypad.read(),
            0xFF01 | 0xFF02 => self.serial.read(addr),
            0xFF04..=0xFF07 => self.timer.read(addr),
            0xFF10..=0xFF3F => self.apu.read(addr),
            0xFF40..=0xFF45 | 0xFF47..=0xFF4B => self.ppu.read(addr),
            0xFF4C => self.io_raw[(addr - R::IO_START) as usize],
            0xFF46 => self.dma.read_oam_reg(),
            R::REG_HDMA5 => self.dma.read_hdma_ctrl(),

            // Everything else: generic backing store.
            _ => self.io_raw[(addr - R::IO_START) as usize],
        }
    }

    fn io_write(&mut self, addr: u16, v: u8) {
        match addr {
            R::REG_IF => self.irq.write_if(v),

            R::REG_VBK => self.mem.write_vbk(v),
            R::REG_SVBK => self.mem.write_svbk(v),
            R::REG_BCPS => self.mem.bcps = v,
            R::REG_BCPD => self.mem.write_bg_palette_data(v),
            R::REG_OCPS => self.mem.ocps = v,
            R::REG_OCPD => self.mem.write_obj_palette_data(v),
            // KEY1: only bit 0 (switch armed) is writable from a normal CPU
            // store; bit 7 (current speed) is read-only. The STOP instruction
            // performs the speed switch by issuing a write with bits 7+0 set
            // (a value no normal store produces) — that toggles the speed bit
            // and clears the armed bit.
            R::REG_KEY1 => {
                if v & 0x81 == 0x81 {
                    self.mem.key1 = (self.mem.key1 ^ 0x80) & 0x80;
                } else {
                    self.mem.key1 = (self.mem.key1 & 0x80) | (v & 0x01);
                }
            }

            0xFF00 => self.joypad.write(v),
            0xFF01 | 0xFF02 => self.serial.write(addr, v),
            0xFF04..=0xFF07 => self.timer.write(addr, v),
            0xFF10..=0xFF3F => self.apu.write(addr, v),
            0xFF40..=0xFF45 | 0xFF47..=0xFF4B => {
                let mut ppu = core::mem::take(&mut self.ppu);
                ppu.write(addr, v, &mut self.irq);
                self.ppu = ppu;
            }
            0xFF4C => self.io_raw[(addr - R::IO_START) as usize] = v,
            0xFF46 => {
                let mut dma = core::mem::take(&mut self.dma);
                dma.start_oam(v, self);
                self.dma = dma;
            }
            R::REG_HDMA1 => self.dma.write_hdma_src_hi(v),
            R::REG_HDMA2 => self.dma.write_hdma_src_lo(v),
            R::REG_HDMA3 => self.dma.write_hdma_dst_hi(v),
            R::REG_HDMA4 => self.dma.write_hdma_dst_lo(v),
            R::REG_HDMA5 => {
                let mut dma = core::mem::take(&mut self.dma);
                dma.write_hdma_ctrl(v, self);
                self.dma = dma;
            }

            _ => self.io_raw[(addr - R::IO_START) as usize] = v,
        }
    }
}

// ============================ Bus impl ============================
//
// Full 16-bit address-space routing (Pan Docs Memory Map):
//   0x0000-0x7FFF  cart ROM (+ MBC control on write)
//   0x8000-0x9FFF  VRAM (VBK-banked)
//   0xA000-0xBFFF  cart external RAM (MBC-gated)
//   0xC000-0xDFFF  WRAM (SVBK-banked high half)
//   0xE000-0xFDFF  echo RAM (mirror of 0xC000-0xDDFF)
//   0xFE00-0xFE9F  OAM
//   0xFEA0-0xFEFF  unusable (reads 0xFF / writes ignored)
//   0xFF00-0xFF7F  IO registers
//   0xFF80-0xFFFE  HRAM
//   0xFFFF         IE
impl Bus for Gbc {
    fn read8(&mut self, addr: u16) -> u8 {
        match addr {
            R::ROM0_START..=0x7FFF => self.cart.read_rom(addr),
            R::VRAM_START..=0x9FFF => self.mem.read_vram(addr),
            R::ERAM_START..=0xBFFF => self.cart.read_ram(addr),
            R::WRAM0_START..=0xDFFF => self.mem.read_wram(addr),
            // Echo RAM mirrors 0xC000-0xDDFF (offset -0x2000).
            R::ECHO_START..=0xFDFF => self.mem.read_wram(addr - 0x2000),
            R::OAM_START..=0xFE9F => self.mem.read_oam(addr),
            R::UNUSABLE_START..=0xFEFF => 0xFF, // not usable: open bus
            R::IO_START..=0xFF7F => self.io_read(addr),
            R::HRAM_START..=0xFFFE => self.mem.read_hram(addr),
            R::IE_REGISTER => self.irq.read_ie(),
        }
    }

    fn write8(&mut self, addr: u16, v: u8) {
        match addr {
            R::ROM0_START..=0x7FFF => self.cart.write_rom(addr, v),
            R::VRAM_START..=0x9FFF => self.mem.write_vram(addr, v),
            R::ERAM_START..=0xBFFF => self.cart.write_ram(addr, v),
            R::WRAM0_START..=0xDFFF => self.mem.write_wram(addr, v),
            R::ECHO_START..=0xFDFF => self.mem.write_wram(addr - 0x2000, v),
            R::OAM_START..=0xFE9F => self.mem.write_oam(addr, v),
            R::UNUSABLE_START..=0xFEFF => {} // not usable: ignored
            R::IO_START..=0xFF7F => self.io_write(addr, v),
            R::HRAM_START..=0xFFFE => self.mem.write_hram(addr, v),
            R::IE_REGISTER => self.irq.write_ie(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid ROM that, from 0x0100, enables the LCD + VBlank
    /// interrupt then spins. Returns the 32 KiB image.
    fn boot_rom(cgb: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x0143] = if cgb { 0xC0 } else { 0x00 }; // CGB flag
        rom[0x0147] = 0x00; // no MBC
        // Program at 0x0100:
        //   LD A,0x91 ; LD (FF40),A   ; turn LCD on (BG + tile data)
        //   EI                        ; enable interrupts
        //   (loop) JR loop
        let prog = [
            0x3E, 0x91, // LD A,0x91
            0xE0, 0x40, // LDH (0x40),A  -> LCDC
            0xFB,       // EI
            0x18, 0xFE, // JR -2  (spin)
        ];
        rom[0x0100..0x0100 + prog.len()].copy_from_slice(&prog);
        rom
    }

    #[test]
    fn runs_a_frame_and_bumps_counter() {
        let mut gbc = Gbc::new();
        gbc.load_rom(&boot_rom(true));
        gbc.write8(0xFFFF, 0x01); // enable VBlank in IE
        let before = gbc.frame_count();
        gbc.run_frame();
        assert_eq!(gbc.frame_count(), before + 1);
        // VBlank should have been reached this frame.
        assert_eq!(gbc.ppu.ly, 144);
    }

    #[test]
    fn dmg_rom_uses_dmg_palette_path() {
        let mut gbc = Gbc::new();
        gbc.load_rom(&boot_rom(false));
        assert!(!gbc.ppu.cgb_mode);
        // Several frames run without panicking and keep the framebuffer sized.
        for _ in 0..3 {
            gbc.run_frame();
        }
        assert_eq!(gbc.framebuffer().len(), 160 * 144 * 4);
    }

    #[test]
    fn keys_route_to_joypad_and_irq() {
        let mut gbc = Gbc::new();
        gbc.load_rom(&boot_rom(true));
        gbc.write8(0xFFFF, 0x10); // enable Joypad IRQ
        gbc.write8(0xFF00, 0x10); // select action group
        gbc.set_keys(crate::joypad::button::A);
        assert_eq!(gbc.read8(0xFF00) & 0x01, 0x00); // A pressed reads 0
        assert_eq!(gbc.irq.pending() & 0x10, 0x10); // joypad IRQ raised
    }

    #[test]
    fn double_speed_switch_via_key1() {
        let mut gbc = Gbc::new();
        gbc.load_rom(&boot_rom(true));
        // Arm KEY1 and execute STOP to perform the switch.
        gbc.write8(0xFF4D, 0x01); // arm speed switch
        gbc.cpu.pc = 0xC000;
        gbc.write8(0xC000, 0x10); // STOP
        gbc.write8(0xC001, 0x00);
        gbc.step();
        assert_eq!(gbc.read8(0xFF4D) & 0x80, 0x80); // now double-speed
        assert!(gbc.double_speed());
    }

    #[test]
    fn wram_and_echo_alias() {
        let mut gbc = Gbc::new();
        gbc.write8(0xC100, 0x5A);
        // Echo region 0xE000-0xFDFF mirrors 0xC000-0xDDFF.
        assert_eq!(gbc.read8(0xE100), 0x5A);
        gbc.write8(0xE200, 0x99);
        assert_eq!(gbc.read8(0xC200), 0x99);
    }

    #[test]
    fn hram_and_ie_routing() {
        let mut gbc = Gbc::new();
        gbc.write8(0xFF80, 0x12);
        assert_eq!(gbc.read8(0xFF80), 0x12);
        gbc.write8(0xFFFF, 0x1F);
        assert_eq!(gbc.read8(0xFFFF), 0x1F);
    }

    #[test]
    fn unusable_region_reads_open_bus() {
        let mut gbc = Gbc::new();
        gbc.write8(0xFEA0, 0x33); // ignored
        assert_eq!(gbc.read8(0xFEA0), 0xFF);
    }

    #[test]
    fn vram_bank_routing_via_bus() {
        let mut gbc = Gbc::new();
        gbc.write8(0x8000, 0xAA);
        gbc.write8(R::REG_VBK, 1);
        gbc.write8(0x8000, 0xBB);
        assert_eq!(gbc.read8(0x8000), 0xBB);
        gbc.write8(R::REG_VBK, 0);
        assert_eq!(gbc.read8(0x8000), 0xAA);
    }

    #[test]
    fn if_register_routes_to_irq() {
        let mut gbc = Gbc::new();
        gbc.request_interrupt(Interrupt::VBlank);
        assert_eq!(gbc.read8(R::REG_IF) & 0x01, 0x01);
    }
}
