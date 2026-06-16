//! Cartridge header parsing.
//!
//! Spec: Pan Docs — The Cartridge Header (gbdev.io/pandocs/The_Cartridge_Header.html).
//! The header occupies 0x0100-0x014F. We parse the fields the foundation needs:
//! title, the CGB flag (0x0143), the cartridge type / MBC (0x0147), and the
//! ROM/RAM size bytes (0x0148/0x0149).

/// CGB-compatibility flag at 0x0143.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CgbFlag {
    /// Pre-CGB cartridge (the byte is part of the title, not a flag).
    Dmg,
    /// 0x80 — supports CGB enhancements, backwards compatible with DMG.
    CgbEnhanced,
    /// 0xC0 — CGB-only.
    CgbOnly,
}

/// Parsed, validated cartridge header.
pub struct CartHeader {
    /// ASCII game title (0x0134-0x0143, NUL-trimmed).
    pub title: String,
    /// CGB-compatibility flag (0x0143).
    pub cgb_flag: CgbFlag,
    /// Raw cartridge-type byte (0x0147) — decoded into the MBC enum elsewhere.
    pub cart_type: u8,
    /// Raw ROM-size byte (0x0148).
    pub rom_size_code: u8,
    /// Raw RAM-size byte (0x0149).
    pub ram_size_code: u8,
}

impl CartHeader {
    /// Parse a ROM image. A too-short image yields a zeroed/`Dmg` header so the
    /// caller can still mount a stub cart without panicking.
    pub fn parse(rom: &[u8]) -> CartHeader {
        let byte = |addr: usize| rom.get(addr).copied().unwrap_or(0);

        // Title runs 0x0134..=0x0143 (16 bytes); later carts shrink it, but we
        // simply trim trailing NULs / non-printables.
        let mut title = String::new();
        for i in 0x0134..=0x0143 {
            let b = byte(i);
            if b == 0 {
                break;
            }
            if b.is_ascii_graphic() || b == b' ' {
                title.push(b as char);
            }
        }
        let title = title.trim_end().to_string();

        let cgb_flag = match byte(0x0143) {
            0x80 => CgbFlag::CgbEnhanced,
            0xC0 => CgbFlag::CgbOnly,
            _ => CgbFlag::Dmg,
        };

        CartHeader {
            title,
            cgb_flag,
            cart_type: byte(0x0147),
            rom_size_code: byte(0x0148),
            ram_size_code: byte(0x0149),
        }
    }

    /// Number of 16 KiB ROM banks implied by the ROM-size byte (0x0148).
    /// `0x00`=2 banks (32 KiB) up to `0x08`=512 banks (8 MiB). Each step
    /// doubles: banks = 2 << code.
    pub fn rom_banks(&self) -> usize {
        match self.rom_size_code {
            0x00..=0x08 => 2usize << self.rom_size_code,
            _ => 2, // unknown — assume the minimum 32 KiB.
        }
    }

    /// Total ROM size in bytes implied by the header.
    pub fn rom_size_bytes(&self) -> usize {
        self.rom_banks() * crate::regions::ROM_BANK_SIZE
    }

    /// Number of 8 KiB external-RAM banks implied by the RAM-size byte (0x0149).
    /// 0x00=none, 0x02=1 (8 KiB), 0x03=4 (32 KiB), 0x04=16 (128 KiB),
    /// 0x05=8 (64 KiB). 0x01 is unused on retail carts (treated as none).
    pub fn ram_banks(&self) -> usize {
        match self.ram_size_code {
            0x02 => 1,
            0x03 => 4,
            0x04 => 16,
            0x05 => 8,
            _ => 0,
        }
    }

    /// Total external-RAM size in bytes implied by the header.
    pub fn ram_size_bytes(&self) -> usize {
        self.ram_banks() * crate::regions::ERAM_BANK_SIZE
    }

    /// True if the cartridge advertises CGB support.
    pub fn is_cgb(&self) -> bool {
        matches!(self.cgb_flag, CgbFlag::CgbEnhanced | CgbFlag::CgbOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(cart_type: u8, rom_code: u8, ram_code: u8, cgb: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x0143] = cgb;
        rom[0x0147] = cart_type;
        rom[0x0148] = rom_code;
        rom[0x0149] = ram_code;
        // title "ABC"
        rom[0x0134] = b'A';
        rom[0x0135] = b'B';
        rom[0x0136] = b'C';
        rom
    }

    #[test]
    fn parses_sizes_and_flag() {
        let rom = synth(0x1B, 0x05, 0x03, 0xC0);
        let h = CartHeader::parse(&rom);
        assert_eq!(h.title, "ABC");
        assert_eq!(h.cgb_flag, CgbFlag::CgbOnly);
        assert!(h.is_cgb());
        assert_eq!(h.rom_banks(), 64); // 2 << 5
        assert_eq!(h.ram_banks(), 4);
        assert_eq!(h.ram_size_bytes(), 4 * 0x2000);
    }

    #[test]
    fn dmg_flag_when_not_80_or_c0() {
        let rom = synth(0x00, 0x00, 0x00, 0x00);
        let h = CartHeader::parse(&rom);
        assert_eq!(h.cgb_flag, CgbFlag::Dmg);
        assert_eq!(h.rom_banks(), 2);
        assert_eq!(h.ram_banks(), 0);
    }
}
