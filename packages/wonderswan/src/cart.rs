//! Cartridge: ROM loading, the WonderSwan ROM footer, and the linear bank
//! registers that page 64 KiB / 1 MiB windows of cart ROM into the V30MZ
//! address space. Built from the WonderSwan dev wiki ("Cartridge" / "Memory
//! map").
//!
//! On the WonderSwan the cartridge HEADER lives at the END of the ROM image,
//! in the last 16 bytes of the last 64 KiB bank (file offset `len-16 .. len`):
//!   +0x00  developer id
//!   +0x01  minimum support code / "color" flag
//!   +0x02  cart number
//!   +0x03  version
//!   +0x04  ROM size code
//!   +0x05  save type / size code
//!   +0x06  flags (bit0: 0=horizontal/1=vertical orientation; bit2: ROM speed; ...)
//!   +0x07  RTC present
//!   +0x08  checksum (16-bit, little-endian)
//!
//! The address space (segment*16 + offset, 20 bits) is divided into 64 KiB
//! pages. Pages are selected by the bank registers in I/O space:
//!   I/O $C0  ROM bank base for the linear region (high bits)
//!   I/O $C1  SRAM bank   (maps into segment 0x1000-0x1FFF, the $10000 window)
//!   I/O $C2  ROM bank 0  (segment 0x2000-0x2FFF window, the $20000-$2FFFF area)
//!   I/O $C3  ROM bank 1  (segment 0x3000-0x3FFF .. $30000-$FFFFF linear, the
//!            top region where the boot vector at 0xFFFF0 lives)
//!
//! We model the common case: the top region ($40000-$FFFFF, i.e. the last
//! several 64 KiB banks selected by `$C3`) maps the high banks of ROM, with the
//! very last bank always visible at the top so the reset vector resolves. SRAM
//! is battery-backed.

/// Parsed WonderSwan ROM footer (the 16-byte header at the END of the image).
#[derive(Clone, Copy, Debug, Default)]
pub struct Footer {
    pub developer: u8,
    pub color: bool,
    pub cart_number: u8,
    pub version: u8,
    pub rom_size_code: u8,
    pub save_code: u8,
    pub flags: u8,
    pub rtc: u8,
    pub checksum: u16,
}

impl Footer {
    /// `true` if the game is meant to be held vertically (rotated). Bit0 of the
    /// flags byte: 0 = horizontal, 1 = vertical.
    pub fn vertical(&self) -> bool {
        self.flags & 0x01 != 0
    }
}

pub struct Cart {
    pub rom: Vec<u8>,
    pub footer: Footer,

    /// Number of 64 KiB ROM banks (rounded up, at least 1).
    bank_count: u32,

    /// Battery-backed SRAM (size from the save code; default 32 KiB).
    pub sram: Vec<u8>,
    pub sram_dirty: bool,

    // Bank registers (I/O $C0-$C3). Each selects a 64 KiB bank.
    pub bank_rom_linear: u8, // $C0
    pub bank_sram: u8,       // $C1
    pub bank_rom0: u8,       // $C2  -> segment window 0x2xxx
    pub bank_rom1: u8,       // $C3  -> segment window 0x3xxx
}

impl Cart {
    pub fn load(bytes: &[u8]) -> Cart {
        let rom = bytes.to_vec();
        let footer = parse_footer(&rom);
        let bank_count = ((rom.len() + 0xFFFF) / 0x10000).max(1) as u32;
        let sram_size = sram_size_for(footer.save_code);
        Cart {
            rom,
            footer,
            bank_count,
            sram: vec![0u8; sram_size.max(0x8000)],
            sram_dirty: false,
            // Power-on bank values per the WS boot defaults: the ROM banks point
            // at the last banks so the top region (and reset vector) resolve.
            bank_rom_linear: 0xFF,
            bank_sram: 0x00,
            bank_rom0: 0xFF,
            bank_rom1: 0xFF,
        }
    }

    #[inline]
    fn rom_byte(&self, bank: u32, offset: u16) -> u8 {
        let bank = if self.bank_count == 0 {
            0
        } else {
            bank % self.bank_count
        };
        let idx = (bank as usize) * 0x10000 + offset as usize;
        self.rom.get(idx).copied().unwrap_or(0xFF)
    }

    /// Read from the cartridge-mapped portion of the address space. `addr` is the
    /// 20-bit physical address. The WonderSwan maps:
    ///   0x10000-0x1FFFF  SRAM (bank `$C1`)
    ///   0x20000-0x2FFFF  ROM bank `$C2`
    ///   0x30000-0x3FFFF  ROM bank `$C3`
    ///   0x40000-0xFFFFF  linear ROM via `$C0` (12 banks of the linear window),
    ///                    with the top bank always mapping the last ROM bank so
    ///                    the reset vector at 0xFFFF0 resolves.
    pub fn read(&self, addr: u32) -> u8 {
        match addr {
            0x10000..=0x1FFFF => {
                let idx = (self.bank_sram as usize) * 0x10000 + (addr & 0xFFFF) as usize;
                self.sram.get(idx).copied().unwrap_or(0xFF)
            }
            0x20000..=0x2FFFF => self.rom_byte(self.bank_rom0 as u32, (addr & 0xFFFF) as u16),
            0x30000..=0x3FFFF => self.rom_byte(self.bank_rom1 as u32, (addr & 0xFFFF) as u16),
            0x40000..=0xFFFFF => {
                // The linear region is 12 contiguous 64 KiB pages [0x4..0xF].
                // The top page (0xF, i.e. 0xF0000-0xFFFFF) ALWAYS maps the last
                // physical ROM bank so the V30MZ reset vector at 0xFFFF0 always
                // resolves regardless of the bank register. The lower pages map
                // banks relative to the linear base ($C0), counting backwards
                // from the last bank so a contiguous ROM image lays out under
                // the reset bank — matching the WS boot default ($C0 = 0xFF).
                let page = (addr >> 16) & 0xF; // 0x4..=0xF
                let from_top = 0xF - page; // 0 for the reset page, up to 11
                let last = self.bank_count.wrapping_sub(1);
                let bank = last.wrapping_sub(from_top);
                self.rom_byte(bank, (addr & 0xFFFF) as u16)
            }
            _ => 0xFF,
        }
    }

    /// Write into cartridge space — only SRAM is writable.
    pub fn write(&mut self, addr: u32, v: u8) {
        if let 0x10000..=0x1FFFF = addr {
            let idx = (self.bank_sram as usize) * 0x10000 + (addr & 0xFFFF) as usize;
            if let Some(b) = self.sram.get_mut(idx) {
                *b = v;
                self.sram_dirty = true;
            }
        }
    }

    pub fn save_ram(&self) -> &[u8] {
        &self.sram
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&bytes[..n]);
    }
}

/// Parse the 16-byte footer at the END of the ROM image.
pub fn parse_footer(rom: &[u8]) -> Footer {
    if rom.len() < 16 {
        return Footer::default();
    }
    let h = &rom[rom.len() - 16..];
    Footer {
        developer: h[0],
        color: h[1] != 0,
        cart_number: h[2],
        version: h[3],
        rom_size_code: h[4],
        save_code: h[5],
        flags: h[6],
        rtc: h[7],
        checksum: u16::from_le_bytes([h[14], h[15]]),
    }
}

/// SRAM size in bytes for a save-type code (WS dev-wiki table). Codes that mean
/// EEPROM produce a small backing store; the common SRAM codes map to KiB.
fn sram_size_for(code: u8) -> usize {
    match code {
        0x00 => 0,         // none
        0x01 => 8 * 1024,  // 64 Kbit SRAM
        0x02 => 32 * 1024, // 256 Kbit
        0x03 => 128 * 1024,
        0x04 => 256 * 1024,
        0x05 => 512 * 1024,
        // EEPROM codes (0x10/0x20/0x50): small backing.
        0x10 => 128,
        0x20 => 2 * 1024,
        0x50 => 1024,
        _ => 32 * 1024,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_banks(n: usize) -> Vec<u8> {
        let mut rom = vec![0u8; n * 0x10000];
        for b in 0..n {
            rom[b * 0x10000] = b as u8; // tag each bank's first byte
        }
        rom
    }

    #[test]
    fn footer_parse() {
        let mut rom = rom_with_banks(2);
        let len = rom.len();
        let h = &mut rom[len - 16..];
        h[6] = 0x01; // vertical
        h[1] = 0x01; // color
        h[14] = 0x34;
        h[15] = 0x12;
        let f = parse_footer(&rom);
        assert!(f.vertical());
        assert!(f.color);
        assert_eq!(f.checksum, 0x1234);
    }

    #[test]
    fn reset_vector_resolves_to_last_bank() {
        // With linear bank base 0xFF (power-on), the top page (0xF) must map the
        // very last ROM bank, so reads near 0xFFFF0 see that bank's contents.
        let n = 8;
        let mut rom = rom_with_banks(n);
        // Put a marker at the last bank, offset 0xFFF0 (the reset area).
        rom[(n - 1) * 0x10000 + 0xFFF0] = 0xAB;
        let cart = Cart::load(&rom);
        assert_eq!(cart.read(0xFFFF0), 0xAB);
    }

    #[test]
    fn sram_read_write() {
        let cart_rom = rom_with_banks(2);
        let mut cart = Cart::load(&cart_rom);
        cart.write(0x10000, 0x77);
        assert_eq!(cart.read(0x10000), 0x77);
        assert!(cart.sram_dirty);
    }

    #[test]
    fn rom0_bank_select() {
        let mut cart = Cart::load(&rom_with_banks(8));
        cart.bank_rom0 = 5;
        assert_eq!(cart.read(0x20000), 5); // bank 5's tag byte
    }
}
