//! Cartridge: ROM loading + LoROM/HiROM address mapping with battery SRAM.
//! Built from the SNES memory-map docs (fullsnes, superfamicom.org).
//!
//! The SNES header lives at a fixed offset inside the ROM image: $7FC0 for
//! LoROM, $FFC0 for HiROM (plus $40FFC0 for ExHiROM, which we treat as HiROM).
//! We score both candidate layouts and pick the better one. A 512-byte copier
//! header (file length & 0x7FFF == 512) is stripped first.
//!
//! ## Mapping
//!
//! LoROM: 32 KiB ROM chunks mapped at $8000-$FFFF of each bank. Banks $00-$7D
//! and $80-$FF; the chunk index is `(bank & 0x7F) * 0x8000 + (offset-0x8000)`,
//! wrapped to the ROM size. SRAM appears at banks $70-$7D / $F0-$FF, $0000-$7FFF.
//!
//! HiROM: 64 KiB ROM chunks mapped at $0000-$FFFF of banks $40-$7D / $C0-$FF,
//! and at $8000-$FFFF of banks $00-$3F / $80-$BF. SRAM at banks $20-$3F /
//! $A0-$BF, $6000-$7FFF.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapMode {
    LoRom,
    HiRom,
}

/// Parsed cartridge header fields we care about.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub map_mode: MapMode,
    /// SRAM size in bytes (0 if none).
    pub sram_size: usize,
    /// True if the cartridge has a battery (so SRAM should persist).
    pub has_battery: bool,
}

pub struct Cart {
    pub rom: Vec<u8>,
    pub map_mode: MapMode,
    pub header: Header,

    /// Battery-backed cartridge SRAM.
    pub sram: Vec<u8>,
    pub sram_dirty: bool,
}

impl Cart {
    /// Load a ROM image, auto-detecting LoROM vs HiROM. A 512-byte copier
    /// header is stripped if present.
    pub fn load(bytes: &[u8]) -> Cart {
        let rom: Vec<u8> = if bytes.len() % 0x8000 == 512 {
            bytes[512..].to_vec()
        } else {
            bytes.to_vec()
        };
        let header = detect_header(&rom);
        let sram = vec![0u8; header.sram_size];
        Cart {
            rom,
            map_mode: header.map_mode,
            header,
            sram,
            sram_dirty: false,
        }
    }

    #[inline]
    fn rom_byte(&self, idx: usize) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        self.rom[idx % self.rom.len()]
    }

    /// SRAM index for a CPU address, or None if the address isn't SRAM.
    #[inline]
    fn sram_index(&self, bank: u8, off: u16) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        match self.map_mode {
            MapMode::LoRom => {
                // Banks $70-$7D and $F0-$FF, $0000-$7FFF.
                let b = bank & 0x7F;
                if (0x70..=0x7D).contains(&b) && off < 0x8000 {
                    let bank_idx = (b - 0x70) as usize;
                    let idx = bank_idx * 0x8000 + off as usize;
                    return Some(idx % self.sram.len());
                }
                None
            }
            MapMode::HiRom => {
                // Banks $20-$3F / $A0-$BF, $6000-$7FFF.
                let b = bank & 0x7F;
                if (0x20..=0x3F).contains(&b) && (0x6000..0x8000).contains(&off) {
                    let bank_idx = (b - 0x20) as usize;
                    let idx = bank_idx * 0x2000 + (off as usize - 0x6000);
                    return Some(idx % self.sram.len());
                }
                None
            }
        }
    }

    /// ROM index for a CPU address, or None if the address isn't ROM here.
    #[inline]
    fn rom_index(&self, bank: u8, off: u16) -> Option<usize> {
        match self.map_mode {
            MapMode::LoRom => {
                let b = (bank & 0x7F) as usize;
                if off >= 0x8000 {
                    Some(b * 0x8000 + (off as usize - 0x8000))
                } else {
                    None
                }
            }
            MapMode::HiRom => {
                let b = (bank & 0x7F) as usize;
                if (0x40..=0x7D).contains(&(bank & 0x7F)) || bank >= 0xC0 {
                    // Full-bank mapping.
                    Some((b & 0x3F) * 0x10000 + off as usize)
                } else if off >= 0x8000 {
                    // $00-$3F / $80-$BF: high half mirrors the full-bank map.
                    Some((b & 0x3F) * 0x10000 + off as usize)
                } else {
                    None
                }
            }
        }
    }

    /// CPU read of a cartridge address (already split into bank+offset).
    pub fn read(&self, bank: u8, off: u16) -> Option<u8> {
        if let Some(idx) = self.sram_index(bank, off) {
            return Some(self.sram[idx]);
        }
        self.rom_index(bank, off).map(|idx| self.rom_byte(idx))
    }

    /// CPU write to a cartridge address (SRAM only; ROM writes are ignored).
    pub fn write(&mut self, bank: u8, off: u16, v: u8) {
        if let Some(idx) = self.sram_index(bank, off) {
            self.sram[idx] = v;
            self.sram_dirty = true;
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

/// Score a candidate header at `base`: a higher score means a more plausible
/// SNES header. Heuristic from fullsnes / common loaders: the complement and
/// checksum should sum to $FFFF, the reset vector should point into ROM, and
/// the title should be printable.
fn score_header(rom: &[u8], base: usize) -> i32 {
    if base + 0x50 > rom.len() {
        return -1000;
    }
    let mut score = 0i32;

    // Title: 21 bytes at base..base+0x15, should be mostly printable ASCII.
    let mut printable = 0;
    for &c in &rom[base..base + 0x15] {
        if (0x20..=0x7E).contains(&c) {
            printable += 1;
        }
    }
    score += printable;

    // Checksum + complement at base+0x1C..0x20 should be ~$FFFF.
    let comp = u16::from_le_bytes([rom[base + 0x1C], rom[base + 0x1D]]);
    let csum = u16::from_le_bytes([rom[base + 0x1E], rom[base + 0x1F]]);
    if comp ^ csum == 0xFFFF {
        score += 32;
    }

    // Reset vector at base+0x3C should point into $8000-$FFFF.
    let reset = u16::from_le_bytes([rom[base + 0x3C], rom[base + 0x3D]]);
    if reset >= 0x8000 {
        score += 8;
    }

    score
}

/// Detect the cartridge layout + header fields.
fn detect_header(rom: &[u8]) -> Header {
    // LoROM header at $7FC0, HiROM at $FFC0.
    let lo_score = score_header(rom, 0x7FC0);
    let hi_score = score_header(rom, 0xFFC0);

    let (base, map_mode) = if hi_score > lo_score {
        (0xFFC0usize, MapMode::HiRom)
    } else {
        (0x7FC0usize, MapMode::LoRom)
    };

    // Header layout: +$26 = ROM type (chipset), +$28 = sram size (log2 KiB).
    let rom_type = rom.get(base + 0x26).copied().unwrap_or(0);
    let sram_log = rom.get(base + 0x28).copied().unwrap_or(0);
    let sram_size = if sram_log == 0 {
        0
    } else {
        (1usize << sram_log) * 1024
    };
    // ROM type low nibble: 0/3 = ROM only / ROM+RAM, 2 = ROM+RAM+battery, etc.
    let has_battery = matches!(rom_type & 0x0F, 0x02 | 0x05 | 0x06 | 0x09 | 0x0A) || sram_size > 0;

    Header {
        map_mode,
        sram_size: sram_size.min(0x80000), // cap at 512 KiB
        has_battery,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal LoROM image with a valid-ish header.
    fn build_lorom(reset: u16) -> Vec<u8> {
        let mut rom = vec![0u8; 0x10000]; // 64 KiB = 2 LoROM banks
        let base = 0x7FC0;
        // Title.
        for (i, &c) in b"TESTROM              ".iter().enumerate() {
            rom[base + i] = c;
        }
        rom[base + 0x25] = 0x20; // map byte (LoROM, slow)
        rom[base + 0x26] = 0x02; // ROM+RAM+battery
        rom[base + 0x28] = 0x03; // 8 KiB SRAM
        // checksum/complement.
        rom[base + 0x1C] = 0x00;
        rom[base + 0x1D] = 0x00;
        rom[base + 0x1E] = 0xFF;
        rom[base + 0x1F] = 0xFF;
        // reset vector at $7FFC.
        rom[0x7FFC] = (reset & 0xFF) as u8;
        rom[0x7FFD] = (reset >> 8) as u8;
        rom
    }

    #[test]
    fn detects_lorom() {
        let rom = build_lorom(0x8000);
        let cart = Cart::load(&rom);
        assert_eq!(cart.map_mode, MapMode::LoRom);
        assert_eq!(cart.sram.len(), 8 * 1024);
        assert!(cart.header.has_battery);
    }

    #[test]
    fn lorom_rom_mapping() {
        let mut rom = build_lorom(0x8000);
        // Place a sentinel at bank 0 offset $8000 -> rom index 0.
        rom[0x0000] = 0xAB;
        // bank 1 ($01:8000) -> rom index 0x8000.
        rom[0x8000] = 0xCD;
        let cart = Cart::load(&rom);
        assert_eq!(cart.read(0x00, 0x8000), Some(0xAB));
        assert_eq!(cart.read(0x01, 0x8000), Some(0xCD));
        // $80 banks mirror $00 banks.
        assert_eq!(cart.read(0x80, 0x8000), Some(0xAB));
        // Low half of a $00 bank is not ROM in LoROM (it's the system area).
        assert_eq!(cart.read(0x00, 0x0000), None);
    }

    #[test]
    fn lorom_sram_roundtrip() {
        let rom = build_lorom(0x8000);
        let mut cart = Cart::load(&rom);
        cart.write(0x70, 0x0000, 0x99);
        assert_eq!(cart.read(0x70, 0x0000), Some(0x99));
        assert!(cart.sram_dirty);
    }

    #[test]
    fn copier_header_stripped() {
        let mut rom = build_lorom(0x8000);
        let mut with_hdr = vec![0u8; 512];
        with_hdr.append(&mut rom);
        let cart = Cart::load(&with_hdr);
        assert_eq!(cart.map_mode, MapMode::LoRom);
    }
}
