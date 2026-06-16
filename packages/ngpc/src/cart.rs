//! Cartridge: ROM loading + header parsing. Built from the ngpcspec hardware
//! doc.
//!
//! The cartridge appears in the TLCS-900 address space at TWO 2 MB windows:
//!   0x200000-0x3FFFFF  ROM chip 1
//!   0x800000-0x9FFFFF  ROM chip 2
//! Real carts use flash chips with their own windowed banking; commercial games
//! that fit in 2 MB live entirely in the first window, and we map the ROM image
//! linearly into both windows (larger games' second chip image follows the
//! first). Flash command writes (used only for in-game save flashing) are
//! accepted and ignored — saves are handled via the work-RAM/SRAM path instead.
//!
//! ROM header (at the start of ROM = 0x200000):
//!   0x00  28 B  copyright / recognition string
//!   0x1C   4 B  program entry/start address (little-endian, 24-bit used)
//!   0x20   2 B  game ID (BCD, little-endian)
//!   0x22   1 B  version
//!   0x23   1 B  system/compatibility: 0x00 = mono only, 0x10 = colour
//!   0x24  12 B  game title (ASCII)

pub struct Cart {
    pub rom: Vec<u8>,
    /// True if the header marks the game colour-capable (byte 0x23 == 0x10).
    pub color: bool,
    /// Program entry point from header offset 0x1C (24-bit).
    pub entry: u32,
}

impl Cart {
    pub fn load(bytes: &[u8]) -> Cart {
        let rom = bytes.to_vec();
        let color = rom.get(0x23).copied() == Some(0x10);
        let entry = u32::from_le_bytes([
            rom.get(0x1C).copied().unwrap_or(0),
            rom.get(0x1D).copied().unwrap_or(0),
            rom.get(0x1E).copied().unwrap_or(0),
            0,
        ]) & 0x00FF_FFFF;
        Cart { rom, color, entry }
    }

    /// ASCII game title from header offset 0x24 (up to 12 bytes), trimmed.
    pub fn title(&self) -> String {
        let mut s = String::new();
        for i in 0x24..0x30 {
            match self.rom.get(i) {
                Some(&b) if b.is_ascii_graphic() || b == b' ' => s.push(b as char),
                _ => break,
            }
        }
        s.trim().to_string()
    }

    /// Read a byte at a cartridge-window offset (0 = start of ROM = 0x200000).
    /// Returns 0xFF past the end of the image.
    pub fn read(&self, offset: u32) -> u8 {
        self.rom.get(offset as usize).copied().unwrap_or(0xFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1_0000];
        // Copyright string.
        for (i, b) in b"COPYRIGHT BY SNK CORPORATION".iter().enumerate() {
            rom[i] = *b;
        }
        // Entry = 0x200100.
        rom[0x1C] = 0x00;
        rom[0x1D] = 0x01;
        rom[0x1E] = 0x20;
        // Colour.
        rom[0x23] = 0x10;
        // Title.
        for (i, b) in b"TEST GAME".iter().enumerate() {
            rom[0x24 + i] = *b;
        }
        rom
    }

    #[test]
    fn parses_color_flag_and_entry() {
        let c = Cart::load(&header_rom());
        assert!(c.color);
        assert_eq!(c.entry, 0x0020_0100);
    }

    #[test]
    fn parses_title() {
        let c = Cart::load(&header_rom());
        assert_eq!(c.title(), "TEST GAME");
    }

    #[test]
    fn mono_flag() {
        let mut rom = header_rom();
        rom[0x23] = 0x00;
        let c = Cart::load(&rom);
        assert!(!c.color);
    }

    #[test]
    fn read_past_end_is_ff() {
        let c = Cart::load(&[0u8; 16]);
        assert_eq!(c.read(1000), 0xFF);
    }
}
