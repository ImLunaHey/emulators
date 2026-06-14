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
use crate::regions as R;

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
        }
    }

    /// Mount a ROM image. Parses the header, decodes the MBC, sizes external
    /// RAM, and resets the CPU to its post-boot CGB register state.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.cart.load_rom(bytes);
        self.cpu = Cpu::new();
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

    /// Advance the machine by one instruction (drives interrupt service +
    /// decode). The instruction interpreter lives in `cpu::exec`; until it
    /// lands this is a seam.
    pub fn step(&mut self) -> u32 {
        // Interrupt servicing first (handles the IME push-PC-and-jump), then
        // instruction execute. Both need the CPU and the bus (= self), so we
        // take the CPU/IRQ out and pass self.
        todo!("cpu::exec: fetch/decode/execute one instruction")
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

            // Device-owned registers — seams until those modules land.
            0xFF00 => todo!("joypad P1/JOYP read"),
            0xFF01 | 0xFF02 => todo!("serial SB/SC read"),
            0xFF04..=0xFF07 => todo!("timer DIV/TIMA/TMA/TAC read"),
            0xFF10..=0xFF3F => todo!("APU register read"),
            0xFF40..=0xFF45 | 0xFF47..=0xFF4B | 0xFF4C => todo!("PPU LCD register read"),
            0xFF46 => todo!("OAM DMA register read"),
            R::REG_HDMA5 => todo!("HDMA length/status read"),

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
            // KEY1: only bit 0 (switch armed) is writable; bit 7 is read-only.
            R::REG_KEY1 => self.mem.key1 = (self.mem.key1 & 0x80) | (v & 0x01),

            0xFF00 => todo!("joypad P1/JOYP write"),
            0xFF01 | 0xFF02 => todo!("serial SB/SC write"),
            0xFF04..=0xFF07 => todo!("timer DIV/TIMA/TMA/TAC write"),
            0xFF10..=0xFF3F => todo!("APU register write"),
            0xFF40..=0xFF45 | 0xFF47..=0xFF4B | 0xFF4C => todo!("PPU LCD register write"),
            0xFF46 => todo!("OAM DMA start"),
            R::REG_HDMA1..=R::REG_HDMA5 => todo!("HDMA source/dest/length write"),

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
