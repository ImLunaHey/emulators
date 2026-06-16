//! Genesis cartridge: the ROM image plus header parsing and (optional) on-cart
//! SRAM. Built from the Sega Mega Drive ROM header layout (offsets at $100).
//!
//! Most Genesis carts are plain linear ROM mapped at $000000. We support that,
//! plain SRAM at $200000 (battery-backed save), and the header fields needed to
//! identify the game. The common bank-switch mappers (SSF2, etc.) are stubbed
//! for now — flagged in the module docs as a next step.

pub struct Cart {
    pub rom: Vec<u8>,
    /// On-cart SRAM (battery save). Present even if the header doesn't declare
    /// it so SRAM-probing games don't read open bus.
    pub sram: Vec<u8>,
    pub sram_start: u32,
    pub sram_end: u32,
    pub ram_dirty: bool,

    pub domestic_name: String,
    pub region: String,
}

impl Cart {
    pub fn load(bytes: &[u8]) -> Cart {
        let rom = bytes.to_vec();
        // Header at $100. Guard against short ROMs.
        let read_str = |off: usize, len: usize| -> String {
            if off + len <= rom.len() {
                String::from_utf8_lossy(&rom[off..off + len])
                    .trim()
                    .to_string()
            } else {
                String::new()
            }
        };
        let domestic_name = read_str(0x120, 48);
        let region = read_str(0x1F0, 3);

        // SRAM info at $1B0: 'RA' marker, then flags + start/end addresses.
        let mut sram_start = 0x200000;
        let mut sram_end = 0x20FFFF;
        if rom.len() >= 0x1BC && &rom[0x1B0..0x1B2] == b"RA" {
            sram_start = be32(&rom, 0x1B4);
            sram_end = be32(&rom, 0x1B8);
        }
        let sram_size = if sram_end >= sram_start {
            (sram_end - sram_start + 1) as usize
        } else {
            0x10000
        };
        let sram_size = sram_size.clamp(0x2000, 0x10000);

        Cart {
            rom,
            sram: vec![0u8; sram_size],
            sram_start,
            sram_end,
            ram_dirty: false,
            domestic_name,
            region,
        }
    }

    /// Read a byte from the cartridge address space (68000 view). Covers ROM at
    /// $000000-$3FFFFF and SRAM in its declared window.
    pub fn read(&self, addr: u32) -> u8 {
        let a = addr & 0xFF_FFFF;
        if a >= self.sram_start && a <= self.sram_end && !self.sram.is_empty() {
            let off = (a - self.sram_start) as usize;
            if off < self.sram.len() {
                return self.sram[off];
            }
        }
        let off = a as usize;
        if off < self.rom.len() {
            self.rom[off]
        } else {
            0xFF
        }
    }

    pub fn write(&mut self, addr: u32, v: u8) {
        let a = addr & 0xFF_FFFF;
        if a >= self.sram_start && a <= self.sram_end {
            let off = (a - self.sram_start) as usize;
            if off < self.sram.len() {
                self.sram[off] = v;
                self.ram_dirty = true;
            }
        }
        // Writes into ROM space are ignored (no mapper yet).
    }

    pub fn save_ram(&self) -> &[u8] {
        &self.sram
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&bytes[..n]);
    }
}

fn be32(rom: &[u8], off: usize) -> u32 {
    if off + 4 <= rom.len() {
        ((rom[off] as u32) << 24)
            | ((rom[off + 1] as u32) << 16)
            | ((rom[off + 2] as u32) << 8)
            | rom[off + 3] as u32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_header() -> Vec<u8> {
        let mut r = vec![0u8; 0x400];
        r[0x120..0x120 + 5].copy_from_slice(b"SONIC");
        r[0x1F0..0x1F3].copy_from_slice(b"JUE");
        r
    }

    #[test]
    fn parses_name_and_region() {
        let c = Cart::load(&rom_with_header());
        assert!(c.domestic_name.starts_with("SONIC"));
        assert_eq!(c.region, "JUE");
    }

    #[test]
    fn reads_rom_bytes() {
        let mut r = vec![0u8; 0x400];
        r[0x10] = 0x42;
        let c = Cart::load(&r);
        assert_eq!(c.read(0x10), 0x42);
        assert_eq!(c.read(0x500), 0xFF); // beyond ROM
    }

    #[test]
    fn sram_read_write() {
        let mut c = Cart::load(&vec![0u8; 0x400]);
        let a = c.sram_start;
        c.write(a, 0x99);
        assert_eq!(c.read(a), 0x99);
        assert!(c.ram_dirty);
    }

    #[test]
    fn header_declares_sram() {
        let mut r = vec![0u8; 0x400];
        r[0x1B0..0x1B2].copy_from_slice(b"RA");
        // start $200001 end $200FFF
        r[0x1B4..0x1B8].copy_from_slice(&[0x00, 0x20, 0x00, 0x01]);
        r[0x1B8..0x1BC].copy_from_slice(&[0x00, 0x20, 0x0F, 0xFF]);
        let c = Cart::load(&r);
        assert_eq!(c.sram_start, 0x200001);
        assert_eq!(c.sram_end, 0x200FFF);
    }
}
