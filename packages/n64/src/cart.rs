//! Cartridge ROM: byte-order detection/normalisation and the parsed header.
//!
//! N64 ROM dumps come in three byte orders, distinguished by the first four
//! bytes (the header's initial PI BSB/DOM register value, always 0x80371240 in
//! native big-endian "z64" form):
//!
//! * `.z64` — big-endian (native). Magic `80 37 12 40`.
//! * `.n64` — little-endian (word-swapped). Magic `40 12 37 80`.
//! * `.v64` — byteswapped (half-word swapped). Magic `37 80 40 12`.
//!
//! We detect the magic and normalise everything to big-endian (`.z64`) so the
//! bus can serve the cart as a flat big-endian byte array. The header (n64brew
//! "ROM Header") then yields the boot entry point and a few fields the HLE boot
//! needs.

/// The native big-endian magic at offset 0 of a `.z64` image.
const MAGIC_Z64: [u8; 4] = [0x80, 0x37, 0x12, 0x40];
/// Little-endian (`.n64`): each 32-bit word's bytes reversed.
const MAGIC_N64: [u8; 4] = [0x40, 0x12, 0x37, 0x80];
/// Byteswapped (`.v64`): each 16-bit half's bytes swapped.
const MAGIC_V64: [u8; 4] = [0x37, 0x80, 0x40, 0x12];

/// Detected on-disk byte order of a ROM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    /// `.z64` — big-endian, native; no conversion needed.
    Z64,
    /// `.n64` — 32-bit little-endian; reverse each 4-byte word.
    N64,
    /// `.v64` — 16-bit byteswapped; swap each byte pair.
    V64,
}

/// Detect the byte order from the first four bytes. Returns `None` if the magic
/// matches no known variant (not a valid N64 ROM).
pub fn detect_byte_order(bytes: &[u8]) -> Option<ByteOrder> {
    let head: [u8; 4] = bytes.get(0..4)?.try_into().ok()?;
    match head {
        MAGIC_Z64 => Some(ByteOrder::Z64),
        MAGIC_N64 => Some(ByteOrder::N64),
        MAGIC_V64 => Some(ByteOrder::V64),
        _ => None,
    }
}

/// Normalise a ROM image to native big-endian (`.z64`) byte order in place,
/// returning the owned, converted buffer. Returns `None` for an unrecognised
/// magic.
pub fn normalize(bytes: &[u8]) -> Option<Vec<u8>> {
    let order = detect_byte_order(bytes)?;
    let mut out = bytes.to_vec();
    match order {
        ByteOrder::Z64 => {}
        ByteOrder::V64 => {
            // Swap each 16-bit half-word (B0 B1 -> B1 B0).
            for pair in out.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        ByteOrder::N64 => {
            // Reverse each 32-bit word (B0 B1 B2 B3 -> B3 B2 B1 B0).
            for word in out.chunks_exact_mut(4) {
                word.reverse();
            }
        }
    }
    Some(out)
}

/// Parsed N64 ROM header (the first 0x40 bytes), big-endian.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    /// Initial PI domain-1 latch/config word (the magic).
    pub pi_config: u32,
    /// Clock rate override (0 = default).
    pub clock_rate: u32,
    /// Boot entry virtual address — where the CPU jumps after IPL3.
    pub entry_point: u32,
    /// Release / libultra version word.
    pub release: u32,
    /// CRC1/CRC2 checksums (used by IPL3; we don't recompute them).
    pub crc1: u32,
    pub crc2: u32,
    /// Up to 20 ASCII bytes of the internal game title.
    pub title: [u8; 20],
    /// Cartridge media type / game code (4 bytes at 0x3B).
    pub game_code: [u8; 4],
}

impl Header {
    /// Parse the header from a normalised (big-endian) ROM. Returns `None` if
    /// the buffer is shorter than the 0x40-byte header.
    pub fn parse(rom: &[u8]) -> Option<Header> {
        if rom.len() < 0x40 {
            return None;
        }
        let be = |o: usize| u32::from_be_bytes(rom[o..o + 4].try_into().unwrap());
        let mut title = [0u8; 20];
        title.copy_from_slice(&rom[0x20..0x34]);
        let mut game_code = [0u8; 4];
        game_code.copy_from_slice(&rom[0x3B..0x3F]);
        Some(Header {
            pi_config: be(0x00),
            clock_rate: be(0x04),
            entry_point: be(0x08),
            release: be(0x0C),
            crc1: be(0x10),
            crc2: be(0x14),
            title,
            game_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(magic: [u8; 4]) -> Vec<u8> {
        let mut v = vec![0u8; 0x1000];
        v[0..4].copy_from_slice(&magic);
        v
    }

    #[test]
    fn detect_all_three_orders() {
        assert_eq!(detect_byte_order(&make(MAGIC_Z64)), Some(ByteOrder::Z64));
        assert_eq!(detect_byte_order(&make(MAGIC_N64)), Some(ByteOrder::N64));
        assert_eq!(detect_byte_order(&make(MAGIC_V64)), Some(ByteOrder::V64));
        assert_eq!(detect_byte_order(&[0, 1, 2, 3]), None);
    }

    #[test]
    fn v64_normalizes_to_z64() {
        // A z64 word and its v64 (byte-pair-swapped) encoding.
        let z64 = vec![0x80, 0x37, 0x12, 0x40, 0xDE, 0xAD, 0xBE, 0xEF];
        let v64 = vec![0x37, 0x80, 0x40, 0x12, 0xAD, 0xDE, 0xEF, 0xBE];
        assert_eq!(normalize(&v64).unwrap(), z64);
    }

    #[test]
    fn n64_normalizes_to_z64() {
        let z64 = vec![0x80, 0x37, 0x12, 0x40, 0xDE, 0xAD, 0xBE, 0xEF];
        let n64 = vec![0x40, 0x12, 0x37, 0x80, 0xEF, 0xBE, 0xAD, 0xDE];
        assert_eq!(normalize(&n64).unwrap(), z64);
    }

    #[test]
    fn header_parses_entry_point() {
        let mut rom = make(MAGIC_Z64);
        // entry_point at 0x08 = 0x80001000 (typical).
        rom[0x08..0x0C].copy_from_slice(&[0x80, 0x00, 0x10, 0x00]);
        rom[0x20..0x24].copy_from_slice(b"TEST");
        let h = Header::parse(&rom).unwrap();
        assert_eq!(h.entry_point, 0x8000_1000);
        assert_eq!(&h.title[0..4], b"TEST");
        assert_eq!(h.pi_config, 0x8037_1240);
    }

    #[test]
    fn normalize_rejects_garbage() {
        assert!(normalize(&[0xAA, 0xBB, 0xCC, 0xDD]).is_none());
    }
}
