//! Cartridge: owns the ROM image + external RAM bytes and the live MBC, and
//! exposes the bus-facing read/write routing for the cart-mapped windows
//! (0x0000-0x7FFF ROM + control registers, 0xA000-0xBFFF external RAM).
//!
//! Spec: Pan Docs — The Cartridge Header + MBCs. `load_rom` parses the header,
//! decodes the controller, sizes the external RAM, and mounts everything.

pub mod header;
pub mod mbc;

use header::CartHeader;
use mbc::{Mbc, MbcKind};

/// A mounted cartridge.
pub struct Cart {
    /// The full ROM image (padded so bank math never indexes past the end).
    pub rom: Vec<u8>,
    /// External (battery-backable) RAM, sized from the header.
    pub ram: Vec<u8>,
    /// The live memory-bank-controller state.
    pub mbc: Mbc,
    /// The parsed header (title / CGB flag / type / sizes).
    pub header: CartHeader,
    /// Whether the cart advertises battery-backed save RAM.
    pub has_battery: bool,
    /// Set on any external-RAM write so the host knows to persist the save.
    pub ram_dirty: bool,
}

impl Default for Cart {
    fn default() -> Self {
        Cart::empty()
    }
}

impl Cart {
    /// An empty cart (no ROM mounted yet). Reads return open-bus 0xFF.
    pub fn empty() -> Cart {
        let header = CartHeader {
            title: String::new(),
            cgb_flag: header::CgbFlag::Dmg,
            cart_type: 0,
            rom_size_code: 0,
            ram_size_code: 0,
        };
        Cart {
            rom: Vec::new(),
            ram: Vec::new(),
            mbc: Mbc::new(MbcKind::NoMbc, 2, 0),
            header,
            has_battery: false,
            ram_dirty: false,
        }
    }

    /// Parse the header, decode the MBC, size the RAM, and mount the ROM.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        let header = CartHeader::parse(bytes);
        let kind = MbcKind::from_cart_type(header.cart_type);
        let has_battery = MbcKind::has_battery(header.cart_type);

        // Pad the ROM up to the header-declared size (and at least one bank) so
        // bank-relative indexing is always in bounds.
        let declared = header.rom_size_bytes().max(crate::regions::ROM_BANK_SIZE * 2);
        let mut rom = vec![0u8; declared.max(bytes.len())];
        rom[..bytes.len()].copy_from_slice(bytes);

        // MBC2 carries 512 x 4-bit of built-in RAM regardless of the RAM-size
        // byte; everything else uses the header's RAM-size field.
        let (ram_bytes, ram_banks) = match kind {
            MbcKind::Mbc2 => (512usize, 1u8),
            _ => (header.ram_size_bytes(), header.ram_banks() as u8),
        };
        let ram = vec![0u8; ram_bytes];

        let rom_banks = (rom.len() / crate::regions::ROM_BANK_SIZE).max(2) as u16;
        let mbc = Mbc::new(kind, rom_banks, ram_banks);

        self.rom = rom;
        self.ram = ram;
        self.mbc = mbc;
        self.header = header;
        self.has_battery = has_battery;
        self.ram_dirty = false;
    }

    // ---- bus-facing accessors (the `Gbc` Bus impl routes the cart windows here) ----

    /// Read a ROM byte (0x0000-0x7FFF). Open-bus 0xFF if unmapped.
    #[inline]
    pub fn read_rom(&self, addr: u16) -> u8 {
        let off = self.mbc.rom_offset(addr);
        self.rom.get(off).copied().unwrap_or(0xFF)
    }

    /// Write to a ROM-window address (0x0000-0x7FFF) — an MBC control register.
    #[inline]
    pub fn write_rom(&mut self, addr: u16, value: u8) {
        self.mbc.write_control(addr, value);
    }

    /// Read external RAM (0xA000-0xBFFF). Returns 0xFF when RAM is
    /// disabled/absent (open bus), and is also where MBC3 RTC reads would be
    /// served once the RTC tick lands.
    #[inline]
    pub fn read_ram(&self, addr: u16) -> u8 {
        match self.mbc.ram_offset(addr) {
            Some(off) => self.ram.get(off).copied().unwrap_or(0xFF),
            None => 0xFF,
        }
    }

    /// Write external RAM (0xA000-0xBFFF). No-op when RAM is disabled/absent.
    #[inline]
    pub fn write_ram(&mut self, addr: u16, value: u8) {
        if let Some(off) = self.mbc.ram_offset(addr) {
            if let Some(slot) = self.ram.get_mut(off) {
                // MBC2 RAM is 4-bit; the upper nibble reads back as 1s, but we
                // store the raw value and mask on read elsewhere. Keep it simple
                // here in the foundation.
                *slot = value;
                self.ram_dirty = true;
            }
        }
    }

    /// Current save-RAM image (for writing a `.sav`).
    pub fn save_ram(&self) -> &[u8] {
        &self.ram
    }
    /// Load a previously saved `.sav` into external RAM.
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.ram.len());
        self.ram[..n].copy_from_slice(&bytes[..n]);
        self.ram_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(cart_type: u8, rom_code: u8, ram_code: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x0147] = cart_type;
        rom[0x0148] = rom_code;
        rom[0x0149] = ram_code;
        // marker bytes in bank 0 and bank 1 to verify routing
        rom[0x0000] = 0xAA;
        rom[0x4000] = 0xBB; // start of bank 1
        rom
    }

    #[test]
    fn mbc1_rom_bank_switch_routes() {
        // 128 KiB ROM (8 banks), MBC1+RAM.
        let mut bytes = synth(0x02, 0x02, 0x02);
        bytes.resize(0x8000 * 4, 0);
        // put a marker at the start of bank 2 (offset 0x8000)
        bytes[0x8000] = 0xCC;
        let mut cart = Cart::empty();
        cart.load_rom(&bytes);

        // bank 0 fixed
        assert_eq!(cart.read_rom(0x0000), 0xAA);
        // default high window = bank 1
        assert_eq!(cart.read_rom(0x4000), 0xBB);
        // select bank 2
        cart.write_rom(0x2000, 0x02);
        assert_eq!(cart.read_rom(0x4000), 0xCC);
    }

    #[test]
    fn external_ram_gated_by_enable() {
        let mut cart = Cart::empty();
        cart.load_rom(&synth(0x03, 0x00, 0x02)); // MBC1 + battery RAM, 8 KiB
        assert!(cart.has_battery);
        // RAM disabled by default: write ignored, read open-bus.
        cart.write_ram(0xA000, 0x42);
        assert_eq!(cart.read_ram(0xA000), 0xFF);
        // enable then write/read back.
        cart.write_rom(0x0000, 0x0A);
        cart.write_ram(0xA000, 0x42);
        assert_eq!(cart.read_ram(0xA000), 0x42);
        assert!(cart.ram_dirty);
    }
}
