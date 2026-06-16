//! Virtual Boy cartridge: ROM + optional battery-backed SRAM.
//!
//! Spec: Planet Virtual Boy "Sacred Tech Scroll" cartridge section. A VB cart
//! is dead simple — no mapper. ROM is mirrored across the 0x07000000 region;
//! SRAM (when present) sits at 0x06000000. The 544-byte ROM *footer* lives at
//! the very end of the ROM image (offset romlen-0x220), holding the game title,
//! maker code, game code, and version:
//!   -0x220  game title (20 bytes, shift-JIS/ASCII)
//!   -0x20C  reserved (25 bytes)
//!   -0x1F3  maker code (2 bytes)
//!   -0x1F1  game code (4 bytes)
//!   -0x1ED  ROM version (1 byte)
//!
//! ROM sizes are powers of two; the address decode mirrors a smaller ROM up to
//! fill the 16 MiB window by masking the address with (len-1).

pub const SRAM_SIZE: usize = 8 * 1024; // 8 KiB cartridge SRAM (typical)

pub struct Cart {
    pub rom: Vec<u8>,
    /// Address mask = rom.len()-1 (ROM sizes are powers of two; non-pow2 ROMs
    /// fall back to modulo).
    rom_mask: u32,
    pub sram: Vec<u8>,
    pub has_battery: bool,
    pub ram_dirty: bool,

    pub title: String,
    pub maker_code: String,
    pub game_code: String,
    pub version: u8,
}

impl Cart {
    pub fn load(bytes: &[u8]) -> Cart {
        let rom = bytes.to_vec();
        let len = rom.len().max(1);
        let rom_mask = if len.is_power_of_two() {
            (len - 1) as u32
        } else {
            // Non-power-of-two: use the next-lower power of two as the mask so
            // reads stay in bounds; the read() also clamps via modulo.
            (len.next_power_of_two() / 2).saturating_sub(1) as u32
        };

        let (title, maker_code, game_code, version) = Self::parse_footer(&rom);

        Cart {
            rom,
            rom_mask,
            sram: vec![0u8; SRAM_SIZE],
            has_battery: true,
            ram_dirty: false,
            title,
            maker_code,
            game_code,
            version,
        }
    }

    fn parse_footer(rom: &[u8]) -> (String, String, String, u8) {
        if rom.len() < 0x220 {
            return (String::new(), String::new(), String::new(), 0);
        }
        let base = rom.len() - 0x220;
        let read_str = |off: usize, n: usize| -> String {
            rom[base + off..base + off + n]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { ' ' })
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let title = read_str(0x00, 20);
        let maker_code = read_str(0x19, 2);
        let game_code = read_str(0x1B, 4);
        let version = rom[base + 0x1F];
        (title, maker_code, game_code, version)
    }

    /// Read a byte from the cartridge ROM window (mirrored).
    #[inline]
    pub fn read_rom(&self, addr: u32) -> u8 {
        if self.rom.is_empty() {
            return 0xFF;
        }
        let idx = if self.rom.len().is_power_of_two() {
            (addr & self.rom_mask) as usize
        } else {
            (addr as usize) % self.rom.len()
        };
        self.rom[idx]
    }

    #[inline]
    pub fn read_sram(&self, addr: u32) -> u8 {
        let idx = (addr as usize) % self.sram.len().max(1);
        self.sram.get(idx).copied().unwrap_or(0xFF)
    }

    #[inline]
    pub fn write_sram(&mut self, addr: u32, v: u8) {
        let n = self.sram.len();
        if n == 0 {
            return;
        }
        self.sram[(addr as usize) % n] = v;
        self.ram_dirty = true;
    }

    pub fn save_ram(&self) -> &[u8] {
        &self.sram
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        let n = self.sram.len().min(bytes.len());
        self.sram[..n].copy_from_slice(&bytes[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_mirrors_power_of_two() {
        let mut rom = vec![0u8; 1024];
        rom[0] = 0xAB;
        rom[1023] = 0xCD;
        let cart = Cart::load(&rom);
        assert_eq!(cart.read_rom(0), 0xAB);
        assert_eq!(cart.read_rom(1023), 0xCD);
        // Mirror: addr 1024 wraps to 0.
        assert_eq!(cart.read_rom(1024), 0xAB);
    }

    #[test]
    fn footer_parse() {
        let mut rom = vec![0u8; 2048];
        let base = rom.len() - 0x220;
        let name = b"TEST GAME";
        rom[base..base + name.len()].copy_from_slice(name);
        rom[base + 0x1F] = 7; // version
        let cart = Cart::load(&rom);
        assert_eq!(cart.title, "TEST GAME");
        assert_eq!(cart.version, 7);
    }

    #[test]
    fn sram_roundtrip() {
        let mut cart = Cart::load(&vec![0u8; 1024]);
        cart.write_sram(0x10, 0x99);
        assert_eq!(cart.read_sram(0x10), 0x99);
        assert!(cart.ram_dirty);
    }
}
