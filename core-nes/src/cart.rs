//! Cartridge: iNES / NES 2.0 header parsing + PRG/CHR storage + mirroring.
//!
//! Spec: NESdev wiki "INES" and "NES 2.0" pages. The 16-byte header is:
//!   0-3   "NES\x1A"
//!   4     PRG-ROM size in 16 KiB units
//!   5     CHR-ROM size in 8 KiB units (0 ⇒ the board uses CHR-RAM)
//!   6     flags6: mirroring (bit0), battery (bit1), trainer (bit2),
//!         four-screen (bit3), mapper low nibble (bits4-7)
//!   7     flags7: VS/PlayChoice (bits0-1), NES2.0 id (bits2-3 == 10),
//!         mapper high nibble (bits4-7)
//!   8..   sized by the format (iNES vs NES 2.0)
//!
//! A 512-byte trainer (flags6 bit2) precedes the PRG data when present.

use crate::mapper::Mapper;

/// Nametable mirroring arrangement. Drives the PPU's nametable address fold.
/// MMC1/MMC3 switch this at runtime via `Cart::mirroring`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mirroring {
    Horizontal,
    Vertical,
    /// Both nametables map to the same 1 KiB (single-screen, lower bank).
    SingleLower,
    /// Both nametables map to the same 1 KiB (single-screen, upper bank).
    SingleUpper,
    /// Cartridge supplies its own VRAM — four independent nametables.
    FourScreen,
}

pub struct Cart {
    pub mapper: Mapper,
    pub prg_rom: Vec<u8>,
    pub chr: Vec<u8>,
    /// True when CHR is writable RAM (header CHR size was 0).
    pub chr_is_ram: bool,
    /// 8 KiB of optional cartridge work/save RAM at $6000-$7FFF.
    pub prg_ram: Vec<u8>,
    pub has_battery: bool,
    /// Mirroring as decoded from the header; mappers may override at runtime
    /// (read through `Cart::mirroring()`).
    base_mirroring: Mirroring,
    pub mapper_id: u16,
}

#[derive(Debug)]
pub enum CartError {
    BadMagic,
    TooShort,
    UnsupportedMapper(u16),
}

impl Cart {
    /// Parse an iNES / NES 2.0 image. Returns an error for a bad header or a
    /// mapper this core doesn't implement.
    pub fn from_ines(bytes: &[u8]) -> Result<Cart, CartError> {
        if bytes.len() < 16 {
            return Err(CartError::TooShort);
        }
        if &bytes[0..4] != b"NES\x1A" {
            return Err(CartError::BadMagic);
        }

        let flags6 = bytes[6];
        let flags7 = bytes[7];
        let is_nes2 = (flags7 & 0x0C) == 0x08;

        let mut prg_units = bytes[4] as usize;
        let mut chr_units = bytes[5] as usize;
        let mut mapper_id = ((flags7 & 0xF0) as u16) | ((flags6 >> 4) as u16);

        if is_nes2 {
            // NES 2.0: byte 8 extends the mapper number (and submapper) and the
            // high nibble of the ROM-size bytes.
            mapper_id |= ((bytes[8] as u16) & 0x0F) << 8;
            let prg_hi = (bytes[9] & 0x0F) as usize;
            let chr_hi = ((bytes[9] >> 4) & 0x0F) as usize;
            prg_units |= prg_hi << 8;
            chr_units |= chr_hi << 8;
        }

        let has_trainer = (flags6 & 0x04) != 0;
        let four_screen = (flags6 & 0x08) != 0;
        let has_battery = (flags6 & 0x02) != 0;

        let base_mirroring = if four_screen {
            Mirroring::FourScreen
        } else if (flags6 & 0x01) != 0 {
            Mirroring::Vertical
        } else {
            Mirroring::Horizontal
        };

        let prg_size = prg_units * 16 * 1024;
        let chr_size = chr_units * 8 * 1024;

        let mut off = 16;
        if has_trainer {
            off += 512;
        }
        if bytes.len() < off + prg_size {
            return Err(CartError::TooShort);
        }
        let prg_rom = bytes[off..off + prg_size].to_vec();
        off += prg_size;

        let (chr, chr_is_ram) = if chr_size == 0 {
            // CHR-RAM board: 8 KiB default.
            (vec![0u8; 8 * 1024], true)
        } else {
            if bytes.len() < off + chr_size {
                return Err(CartError::TooShort);
            }
            (bytes[off..off + chr_size].to_vec(), false)
        };

        let mapper = Mapper::new(mapper_id, prg_units, chr_units.max(1))
            .ok_or(CartError::UnsupportedMapper(mapper_id))?;

        Ok(Cart {
            mapper,
            prg_rom,
            chr,
            chr_is_ram,
            prg_ram: vec![0u8; 8 * 1024],
            has_battery,
            base_mirroring,
            mapper_id,
        })
    }

    /// Current mirroring — the mapper's override if it sets one, else the
    /// header default.
    #[inline]
    pub fn mirroring(&self) -> Mirroring {
        self.mapper.mirroring_override().unwrap_or(self.base_mirroring)
    }

    // ---- CPU-space cartridge access ($4020-$FFFF) ----

    #[inline]
    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        if (0x6000..0x8000).contains(&addr) {
            let i = (addr as usize - 0x6000) % self.prg_ram.len().max(1);
            return if self.prg_ram.is_empty() { 0 } else { self.prg_ram[i] };
        }
        if addr >= 0x8000 {
            let bank_off = self.mapper.prg_offset(addr);
            let i = bank_off % self.prg_rom.len().max(1);
            return self.prg_rom[i];
        }
        0
    }

    #[inline]
    pub fn cpu_write(&mut self, addr: u16, v: u8) {
        if (0x6000..0x8000).contains(&addr) {
            if !self.prg_ram.is_empty() {
                let i = (addr as usize - 0x6000) % self.prg_ram.len();
                self.prg_ram[i] = v;
            }
            return;
        }
        if addr >= 0x8000 {
            // Mapper register writes (bank switching, control, IRQ).
            self.mapper.cpu_write(addr, v);
        }
    }

    // ---- PPU-space pattern-table access ($0000-$1FFF) ----

    #[inline]
    pub fn chr_read(&mut self, addr: u16) -> u8 {
        let off = self.mapper.chr_offset(addr) % self.chr.len().max(1);
        self.chr[off]
    }

    #[inline]
    pub fn chr_write(&mut self, addr: u16, v: u8) {
        if self.chr_is_ram {
            let off = self.mapper.chr_offset(addr) % self.chr.len().max(1);
            self.chr[off] = v;
        }
    }

    /// MMC3 clocks its scanline IRQ counter on PPU A12 rising edges; the PPU
    /// reports each rendered fetch so the mapper can drive `irq_pending`.
    #[inline]
    pub fn ppu_a12_clock(&mut self, addr: u16) {
        self.mapper.ppu_a12_clock(addr);
    }

    /// Take the mapper's pending IRQ line (MMC3). Edge-consumed by the bus.
    #[inline]
    pub fn take_irq(&mut self) -> bool {
        self.mapper.take_irq()
    }
}
