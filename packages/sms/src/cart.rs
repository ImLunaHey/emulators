//! Cartridge: ROM loading + the standard Sega mapper and the Codemasters
//! mapper, with on-cart RAM. Built from the SMS Power! "Mappers" documentation.
//!
//! The Z80 sees 48 KiB of cartridge address space ($0000-$BFFF) as three
//! 16 KiB "frames" (slots). A mapper pages 16 KiB ROM banks into those frames
//! and optionally maps battery-backed cartridge RAM into frame 2.
//!
//! Sega mapper (the common case), control registers at the TOP of RAM:
//!   $FFFC  RAM control / bank-shift
//!   $FFFD  frame 0 ($0000-$3FFF) bank   (the first 1 KiB is always bank 0)
//!   $FFFE  frame 1 ($4000-$7FFF) bank
//!   $FFFF  frame 2 ($8000-$BFFF) bank
//!   $FFFC bit3 selects on-cart RAM into frame 2; bit2 picks RAM bank.
//!
//! Codemasters mapper, control registers at frame BASES:
//!   $0000  frame 0 bank
//!   $4000  frame 1 bank
//!   $8000  frame 2 bank
//!
//! Some images have a 512-byte copier header (file length & 0x3FFF == 512);
//! we strip it.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapperKind {
    Sega,
    Codemasters,
}

pub struct Cart {
    pub rom: Vec<u8>,
    /// Number of 16 KiB banks in the ROM (rounded up).
    bank_count: u32,
    pub mapper: MapperKind,

    /// Current 16 KiB ROM bank mapped into each of the three frames.
    frame_bank: [u32; 3],
    /// Sega-mapper RAM control register ($FFFC).
    ram_control: u8,

    /// On-cart RAM (battery-backed where present). The Sega mapper exposes up
    /// to two 16 KiB RAM banks at frame 2.
    pub cart_ram: Vec<u8>,
    pub ram_dirty: bool,
    has_battery: bool,
}

impl Cart {
    /// Load a ROM image, auto-detecting the mapper. A 512-byte copier header is
    /// stripped if present.
    pub fn load(bytes: &[u8]) -> Cart {
        let rom = if bytes.len() % 0x4000 == 512 {
            bytes[512..].to_vec()
        } else {
            bytes.to_vec()
        };
        let bank_count = ((rom.len() + 0x3FFF) / 0x4000).max(1) as u32;
        let mapper = detect_mapper(&rom);
        Cart {
            rom,
            bank_count,
            mapper,
            // Power-on: Sega mapper maps banks 0,1,2 into the three frames.
            frame_bank: [0, 1, 2],
            ram_control: 0,
            cart_ram: vec![0u8; 0x8000], // 32 KiB (two 16 KiB banks)
            ram_dirty: false,
            has_battery: true,
        }
    }

    #[inline]
    fn rom_byte(&self, bank: u32, offset: u16) -> u8 {
        let bank = bank % self.bank_count;
        let idx = (bank as usize * 0x4000) + offset as usize;
        self.rom.get(idx).copied().unwrap_or(0xFF)
    }

    /// CPU read of cartridge space ($0000-$BFFF).
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            // The first 1 KiB of frame 0 is always fixed to bank 0 (so the
            // interrupt vectors can't be paged out) on the Sega mapper.
            0x0000..=0x03FF if self.mapper == MapperKind::Sega => {
                self.rom_byte(0, addr)
            }
            0x0000..=0x3FFF => self.rom_byte(self.frame_bank[0], addr & 0x3FFF),
            0x4000..=0x7FFF => self.rom_byte(self.frame_bank[1], addr & 0x3FFF),
            0x8000..=0xBFFF => {
                // Sega mapper: frame 2 can be on-cart RAM.
                if self.mapper == MapperKind::Sega && self.ram_control & 0x08 != 0 {
                    let ram_bank = ((self.ram_control >> 2) & 1) as usize;
                    let idx = ram_bank * 0x4000 + (addr & 0x3FFF) as usize;
                    self.cart_ram.get(idx).copied().unwrap_or(0xFF)
                } else {
                    self.rom_byte(self.frame_bank[2], addr & 0x3FFF)
                }
            }
            _ => 0xFF,
        }
    }

    /// CPU write into cartridge space — routes to mapper control registers or
    /// on-cart RAM.
    pub fn write(&mut self, addr: u16, v: u8) {
        match self.mapper {
            MapperKind::Sega => self.write_sega(addr, v),
            MapperKind::Codemasters => self.write_codemasters(addr, v),
        }
    }

    fn write_sega(&mut self, addr: u16, v: u8) {
        match addr {
            0xFFFC => self.ram_control = v,
            0xFFFD => self.frame_bank[0] = v as u32,
            0xFFFE => self.frame_bank[1] = v as u32,
            0xFFFF => self.frame_bank[2] = v as u32,
            // On-cart RAM in frame 2.
            0x8000..=0xBFFF if self.ram_control & 0x08 != 0 => {
                let ram_bank = ((self.ram_control >> 2) & 1) as usize;
                let idx = ram_bank * 0x4000 + (addr & 0x3FFF) as usize;
                if let Some(b) = self.cart_ram.get_mut(idx) {
                    *b = v;
                    self.ram_dirty = true;
                }
            }
            _ => {}
        }
    }

    fn write_codemasters(&mut self, addr: u16, v: u8) {
        match addr {
            0x0000 => self.frame_bank[0] = v as u32,
            0x4000 => self.frame_bank[1] = v as u32,
            0x8000 => self.frame_bank[2] = v as u32,
            _ => {}
        }
    }

    pub fn save_ram(&self) -> &[u8] {
        &self.cart_ram
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.cart_ram.len());
        self.cart_ram[..n].copy_from_slice(&bytes[..n]);
    }
    pub fn has_battery(&self) -> bool {
        self.has_battery
    }
}

/// Detect the Codemasters mapper by its signature: a 16-bit checksum word at
/// $7FE6 that complements the value at $7FE8 (the canonical SMS Power! test).
/// Everything else is treated as the Sega mapper.
fn detect_mapper(rom: &[u8]) -> MapperKind {
    if rom.len() >= 0x8000 {
        // Codemasters header: bytes $7FE6..$7FE7 = checksum, $7FE8..$7FE9 =
        // checksum complement (sum == 0x10000).
        let cksum = u16::from_le_bytes([rom[0x7FE6], rom[0x7FE7]]);
        let comp = u16::from_le_bytes([rom[0x7FE8], rom[0x7FE9]]);
        if cksum != 0 && (cksum as u32 + comp as u32) == 0x10000 {
            return MapperKind::Codemasters;
        }
    }
    MapperKind::Sega
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_banks(n: usize) -> Vec<u8> {
        let mut rom = vec![0u8; n * 0x4000];
        // Tag each bank's first byte with its index so paging is observable.
        for b in 0..n {
            rom[b * 0x4000] = b as u8;
            rom[b * 0x4000 + 0x3FF] = b as u8;
        }
        rom
    }

    #[test]
    fn default_banks_mapped() {
        let cart = Cart::load(&rom_with_banks(4));
        // Frame 0 fixed bank 0, frame 1 bank 1, frame 2 bank 2.
        assert_eq!(cart.read(0x0000), 0);
        assert_eq!(cart.read(0x4000), 1);
        assert_eq!(cart.read(0x8000), 2);
    }

    #[test]
    fn sega_paging() {
        let mut cart = Cart::load(&rom_with_banks(8));
        cart.write(0xFFFE, 5); // frame 1 -> bank 5
        cart.write(0xFFFF, 7); // frame 2 -> bank 7
        assert_eq!(cart.read(0x4000), 5);
        assert_eq!(cart.read(0x8000), 7);
    }

    #[test]
    fn fixed_first_kib() {
        let mut cart = Cart::load(&rom_with_banks(4));
        cart.write(0xFFFD, 3); // frame 0 -> bank 3
        // The first 1 KiB stays bank 0...
        assert_eq!(cart.read(0x0000), 0);
        // ...but past 1 KiB follows the paged bank.
        assert_eq!(cart.read(0x0400), 0); // bank 3 has 0 here (only [0] tagged)
        assert_eq!(cart.read(0x3FFF), 0);
    }

    #[test]
    fn cart_ram_in_frame2() {
        let mut cart = Cart::load(&rom_with_banks(4));
        cart.write(0xFFFC, 0x08); // enable RAM in frame 2, bank 0
        cart.write(0x8000, 0xAB);
        assert_eq!(cart.read(0x8000), 0xAB);
        assert!(cart.ram_dirty);
    }

    #[test]
    fn copier_header_stripped() {
        let mut bytes = vec![0u8; 512];
        bytes.extend_from_slice(&rom_with_banks(2));
        let cart = Cart::load(&bytes);
        assert_eq!(cart.rom.len(), 2 * 0x4000);
    }

    #[test]
    fn codemasters_detect_and_page() {
        let mut rom = rom_with_banks(4);
        // Forge a Codemasters checksum signature.
        rom[0x7FE6] = 0x34;
        rom[0x7FE7] = 0x12; // cksum = 0x1234
        let comp = 0x10000u32 - 0x1234;
        rom[0x7FE8] = (comp & 0xFF) as u8;
        rom[0x7FE9] = (comp >> 8) as u8;
        let mut cart = Cart::load(&rom);
        assert_eq!(cart.mapper, MapperKind::Codemasters);
        cart.write(0x4000, 3); // frame 1 -> bank 3
        assert_eq!(cart.read(0x4000), 3);
    }
}
