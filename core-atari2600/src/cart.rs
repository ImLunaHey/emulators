//! Atari 2600 cartridge: plain ROM plus the standard size-detected
//! bank-switching schemes.
//!
//! Spec: AtariAge "Bankswitching" / Kevin Horton's bankswitch notes. The 6507
//! only sees a 4 KiB cartridge window at $F000-$FFFF (mirrored down through the
//! address space). Larger ROMs page extra banks into that window by *reading or
//! writing* "hotspot" addresses near the top of the window:
//!
//!   - **2K**: a 2 KiB image mirrored twice into the 4 KiB window.
//!   - **4K**: a flat 4 KiB image, no banking.
//!   - **F8 (8K)**: two 4 KiB banks; access $1FF8 selects bank 0, $1FF9 bank 1.
//!   - **F6 (16K)**: four banks; $1FF6..$1FF9 select banks 0..3.
//!   - **F4 (32K)**: eight banks; $1FF4..$1FFB select banks 0..7.
//!
//! Hotspots are decoded on the low 12 bits (the cartridge window is 4 KiB), so
//! they trigger regardless of which $xF000 mirror the program uses.

/// Detected bank-switching scheme, chosen from the ROM size at load time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mapper {
    /// 2K or 4K flat ROM (no banking).
    Flat,
    /// F8 — 8K, two banks, hotspots $1FF8/$1FF9.
    F8,
    /// F6 — 16K, four banks, hotspots $1FF6..$1FF9.
    F6,
    /// F4 — 32K, eight banks, hotspots $1FF4..$1FFB.
    F4,
}

pub struct Cart {
    rom: Box<[u8]>,
    mapper: Mapper,
    /// Active 4 KiB bank (index into `rom` in 4 KiB units).
    bank: u8,
    /// Mask applied to the in-window offset; 0x7FF for 2K, 0xFFF otherwise.
    addr_mask: u16,
}

impl Default for Cart {
    fn default() -> Self {
        Cart {
            rom: vec![0u8; 0x1000].into_boxed_slice(),
            mapper: Mapper::Flat,
            bank: 0,
            addr_mask: 0x0FFF,
        }
    }
}

impl Cart {
    /// Load a ROM image, detecting the mapper from its length. Sizes that don't
    /// match a known scheme are padded/truncated to the nearest sane window.
    pub fn load(bytes: &[u8]) -> Cart {
        let (mapper, addr_mask) = match bytes.len() {
            0..=2048 => (Mapper::Flat, 0x07FF),
            2049..=4096 => (Mapper::Flat, 0x0FFF),
            8192 => (Mapper::F8, 0x0FFF),
            16384 => (Mapper::F6, 0x0FFF),
            32768 => (Mapper::F4, 0x0FFF),
            _ => (Mapper::Flat, 0x0FFF),
        };

        // Normalise the backing store to a multiple of the window so reads never
        // index out of bounds.
        let rom = match mapper {
            Mapper::Flat => {
                let win = (addr_mask as usize) + 1;
                let mut v = vec![0u8; win];
                let n = bytes.len().min(win);
                v[..n].copy_from_slice(&bytes[..n]);
                v.into_boxed_slice()
            }
            _ => {
                let want = match mapper {
                    Mapper::F8 => 8192,
                    Mapper::F6 => 16384,
                    Mapper::F4 => 32768,
                    Mapper::Flat => unreachable!(),
                };
                let mut v = vec![0u8; want];
                let n = bytes.len().min(want);
                v[..n].copy_from_slice(&bytes[..n]);
                v.into_boxed_slice()
            }
        };

        Cart {
            rom,
            mapper,
            // F6/F8/F4 power up on the highest bank (the one holding the reset
            // vector at the top of ROM) on real hardware; many emulators boot
            // bank 0. Use the last bank to match the common case.
            bank: match mapper {
                Mapper::Flat => 0,
                Mapper::F8 => 1,
                Mapper::F6 => 3,
                Mapper::F4 => 7,
            },
            addr_mask,
        }
    }

    pub fn mapper(&self) -> Mapper {
        self.mapper
    }

    /// Read from the cartridge window. `addr` is the full CPU address; only the
    /// low 12 bits matter for the window, and the low 13 select the hotspot.
    pub fn read(&mut self, addr: u16) -> u8 {
        self.hotspot(addr);
        let off = (addr & self.addr_mask) as usize;
        let base = (self.bank as usize) * 0x1000;
        // Flat masks to its own size; banked always uses a 4 KiB window.
        let idx = if self.mapper == Mapper::Flat {
            off % self.rom.len()
        } else {
            base + off
        };
        self.rom[idx]
    }

    /// Writes to the cartridge window do nothing except potentially trip a
    /// bank-switch hotspot.
    pub fn write(&mut self, addr: u16) {
        self.hotspot(addr);
    }

    /// Decode a bank-switch hotspot from the low 12 bits of `addr`.
    fn hotspot(&mut self, addr: u16) {
        let a = addr & 0x0FFF;
        match self.mapper {
            Mapper::Flat => {}
            Mapper::F8 => match a {
                0x0FF8 => self.bank = 0,
                0x0FF9 => self.bank = 1,
                _ => {}
            },
            Mapper::F6 => match a {
                0x0FF6 => self.bank = 0,
                0x0FF7 => self.bank = 1,
                0x0FF8 => self.bank = 2,
                0x0FF9 => self.bank = 3,
                _ => {}
            },
            Mapper::F4 => match a {
                0x0FF4 => self.bank = 0,
                0x0FF5 => self.bank = 1,
                0x0FF6 => self.bank = 2,
                0x0FF7 => self.bank = 3,
                0x0FF8 => self.bank = 4,
                0x0FF9 => self.bank = 5,
                0x0FFA => self.bank = 6,
                0x0FFB => self.bank = 7,
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_2k() {
        let c = Cart::load(&[0xAA; 2048]);
        assert_eq!(c.mapper(), Mapper::Flat);
        assert_eq!(c.addr_mask, 0x07FF);
    }

    #[test]
    fn detects_4k() {
        let c = Cart::load(&[0xAA; 4096]);
        assert_eq!(c.mapper(), Mapper::Flat);
        assert_eq!(c.addr_mask, 0x0FFF);
    }

    #[test]
    fn two_k_mirrors_into_4k_window() {
        let mut rom = vec![0u8; 2048];
        rom[0] = 0x11;
        rom[0x7FF] = 0x22;
        let mut c = Cart::load(&rom);
        // $F000 and $F800 map to the same 2K image.
        assert_eq!(c.read(0xF000), 0x11);
        assert_eq!(c.read(0xF800), 0x11);
        assert_eq!(c.read(0xFFFF), 0x22);
    }

    #[test]
    fn f8_bank_switch() {
        let mut rom = vec![0u8; 8192];
        rom[0x0000] = 0xB0; // bank 0, offset 0
        rom[0x1000] = 0xB1; // bank 1, offset 0
        let mut c = Cart::load(&rom);
        assert_eq!(c.mapper(), Mapper::F8);
        // Boots on bank 1.
        assert_eq!(c.read(0xF000), 0xB1);
        // Select bank 0 via hotspot $1FF8.
        c.read(0xFFF8);
        assert_eq!(c.read(0xF000), 0xB0);
        // Back to bank 1.
        c.read(0xFFF9);
        assert_eq!(c.read(0xF000), 0xB1);
    }

    #[test]
    fn f6_bank_switch() {
        let mut rom = vec![0u8; 16384];
        for b in 0..4 {
            rom[b * 0x1000] = 0xC0 + b as u8;
        }
        let mut c = Cart::load(&rom);
        c.read(0xFFF6);
        assert_eq!(c.read(0xF000), 0xC0);
        c.read(0xFFF7);
        assert_eq!(c.read(0xF000), 0xC1);
        c.read(0xFFF9);
        assert_eq!(c.read(0xF000), 0xC3);
    }

    #[test]
    fn f4_bank_switch() {
        let mut rom = vec![0u8; 32768];
        for b in 0..8 {
            rom[b * 0x1000] = 0xD0 + b as u8;
        }
        let mut c = Cart::load(&rom);
        assert_eq!(c.mapper(), Mapper::F4);
        c.read(0xFFF4);
        assert_eq!(c.read(0xF000), 0xD0);
        c.read(0xFFFB);
        assert_eq!(c.read(0xF000), 0xD7);
    }
}
