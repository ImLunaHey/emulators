//! HuCard ROM cartridge: holds the ROM image and maps physical 8 KiB banks.
//!
//! Spec: Archaic Pixels "HuCard", pcedev wiki "Memory map". A HuCard plugs
//! into the HuC6280's 2 MiB physical address space (banks $00-$7F = ROM,
//! $F8-$FB = RAM, $FF = I/O). The MMU (see `cpu.rs`) maps logical 8 KiB pages
//! onto these physical banks; the cartridge only needs to answer "what ROM byte
//! lives at physical bank B, offset O?".
//!
//! ROM SIZE QUIRKS:
//!   - Some dumps carry a 512-byte header; if `len % 8192 == 512` we strip it.
//!   - The classic 384 KiB ("populous") layout: such a ROM is split as the low
//!     256 KiB mapped to banks $00-$1F and the high 128 KiB MIRRORED across
//!     banks $40-$7F. We special-case 384 KiB to reproduce that bank layout so
//!     those titles boot.
//!   - Smaller ROMs mirror to fill the $00-$7F bank window.

/// Physical bank size: 8 KiB.
pub const BANK_SIZE: usize = 0x2000;

pub struct Cart {
    /// The ROM image (header stripped), padded to a whole number of banks.
    rom: Box<[u8]>,
    /// Number of 8 KiB banks in `rom`.
    banks: usize,
    /// True for the 384 KiB split-mirror layout.
    is_384k: bool,
}

impl Cart {
    /// Load a HuCard image. Strips a 512-byte copier header if present, pads to
    /// a bank boundary, and records the 384 KiB layout flag.
    pub fn load(bytes: &[u8]) -> Cart {
        // Strip a 512-byte header (some dumps prepend one).
        let data: &[u8] = if bytes.len() % BANK_SIZE == 512 {
            &bytes[512..]
        } else {
            bytes
        };

        let is_384k = data.len() == 384 * 1024;

        // Pad up to a whole number of banks.
        let banks = data.len().div_ceil(BANK_SIZE).max(1);
        let mut rom = vec![0xFFu8; banks * BANK_SIZE];
        rom[..data.len()].copy_from_slice(data);

        Cart {
            rom: rom.into_boxed_slice(),
            banks,
            is_384k,
        }
    }

    /// Map a physical bank number ($00-$7F is ROM) to a ROM offset, applying the
    /// mirroring / 384 KiB-split rules. Returns the byte at `offset` within the
    /// resolved bank.
    pub fn read(&self, bank: u8, offset: u16) -> u8 {
        let off = (offset as usize) & (BANK_SIZE - 1);
        let rom_bank = self.resolve_bank(bank);
        let base = rom_bank * BANK_SIZE;
        self.rom[base + off]
    }

    /// Resolve a physical bank ($00-$7F) into an index into `self.rom`'s banks.
    fn resolve_bank(&self, bank: u8) -> usize {
        let b = bank as usize;
        if self.is_384k {
            // 384 KiB = 48 banks: low 256 KiB (32 banks) at $00-$1F, high
            // 128 KiB (16 banks) mirrored across $40-$7F.
            if b < 0x40 {
                b % 32
            } else {
                32 + ((b - 0x40) % 16)
            }
        } else {
            // Plain mirror: wrap into the available banks.
            b % self.banks
        }
    }

    pub fn bank_count(&self) -> usize {
        self.banks
    }
    pub fn is_384k(&self) -> bool {
        self.is_384k
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_512_byte_header() {
        // 8 KiB of ROM + a 512-byte header.
        let mut bytes = vec![0u8; 512 + BANK_SIZE];
        bytes[512] = 0xAA; // first real ROM byte
        let cart = Cart::load(&bytes);
        assert_eq!(cart.read(0, 0), 0xAA);
        assert_eq!(cart.bank_count(), 1);
    }

    #[test]
    fn small_rom_mirrors() {
        // One 8 KiB bank; bank 0 and bank 1 (mirror) should read identically.
        let mut bytes = vec![0u8; BANK_SIZE];
        bytes[0] = 0x42;
        let cart = Cart::load(&bytes);
        assert_eq!(cart.read(0, 0), 0x42);
        assert_eq!(cart.read(1, 0), 0x42); // mirror
        assert_eq!(cart.read(0x7F, 0), 0x42);
    }

    #[test]
    fn detects_384k_layout() {
        let bytes = vec![0u8; 384 * 1024];
        let cart = Cart::load(&bytes);
        assert!(cart.is_384k());
        assert_eq!(cart.bank_count(), 48);
    }

    #[test]
    fn split_384k_high_banks_mirror() {
        let mut bytes = vec![0u8; 384 * 1024];
        // Mark the first byte of the high 128 KiB region (bank 32).
        bytes[32 * BANK_SIZE] = 0x99;
        let cart = Cart::load(&bytes);
        // Physical bank $40 maps to rom bank 32 in the split layout.
        assert_eq!(cart.read(0x40, 0), 0x99);
        // And $50 = 32 + (16 % 16) = bank 32 again — the high window mirrors
        // every 16 banks, so $50 reads the same marked byte as $40.
        assert_eq!(cart.read(0x50, 0), 0x99);
    }

    #[test]
    fn offset_within_bank() {
        let mut bytes = vec![0u8; BANK_SIZE * 2];
        bytes[BANK_SIZE + 5] = 0x77; // bank 1, offset 5
        let cart = Cart::load(&bytes);
        assert_eq!(cart.read(1, 5), 0x77);
    }
}
