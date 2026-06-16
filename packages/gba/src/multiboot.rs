//! GBA multiboot ("Single-Pak link" / Normal-mode boot) support.
//!
//! Multiboot is how one cartridge shares a small program with cartridge-less
//! GBAs over the link cable: the **parent** (the cart) ships an encrypted image
//! to each **child**, whose BIOS has booted into receive mode. The child runs
//! the image from EWRAM (`0x02000000`, entry at `0x020000C0`) — this is what
//! powers single-cartridge multiplayer and Download Play.
//!
//! This module provides two things:
//!   1. The child-receive primitive (`prepare_child_image`): lay a multiboot
//!      `.mb` image into EWRAM and report the entry point so the CPU can boot
//!      it. This makes multiboot ROMs runnable directly.
//!   2. The parent-side crypto/CRC primitives (`Session`) used by the SWI 0x25
//!      (MultiBoot) HLE to encrypt the stream and compute the verification CRC.
//!      The actual byte-shipping needs a connected child over the link
//!      transport; without one, MultiBoot reports failure (no slaves), which is
//!      the faithful single-unit result.
//!
//! The encryption/CRC follow the documented hardware behavior, cross-checked
//! against gba-link-connection's `LinkCableMultiboot` (`sendRomPart` /
//! `calculateCRCData`) and GBATEK's "Multiboot transfer protocol". Constants and
//! the exact word transform are reproduced below.

// The cartridge header occupies the first 0xC0 bytes; the encrypted payload
// starts at word index 0xC0/4 and the child's entry point is 0x020000C0.
pub const HEADER_SIZE: u32 = 0xC0;
pub const EWRAM_BASE: u32 = 0x0200_0000;
pub const CHILD_ENTRY: u32 = EWRAM_BASE + HEADER_SIZE;

// Payload-length constraints (the part after the 0xC0 header): a multiple of
// 0x10, at least 0x100, at most 0x3FF40 bytes.
pub const MIN_PAYLOAD: u32 = 0x100;
pub const MAX_PAYLOAD: u32 = 0x3_FF40;

// Per-mode magic. Normal-32 is the fast/stable mode the SWI uses by default
// here; Multi-play-16 shares the structure with different constants.
const CRC_NORMAL_START: u32 = 0xC387;
const CRC_NORMAL_XOR: u32 = 0xC37B;
const DATA_NORMAL_XOR: u32 = 0x4320_2F2F;
const CRC_MULTI_START: u32 = 0xFFF8;
const CRC_MULTI_XOR: u32 = 0xA517;
const DATA_MULTI_XOR: u32 = 0x6465_646F;

// The session-key LCG multiplier (`m = m * SEED_MULTIPLIER + 1`).
const SEED_MULTIPLIER: u32 = 0x6F64_6573;

/// Transfer mode (SWI 0x25 r1). Normal-32 vs Multi-play-16 select different
/// crypto constants and unit sizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal32,
    Multi16,
}

impl Mode {
    /// Decode the SWI 0x25 `r1` transfer-mode argument. 0 and 2 are Normal-32
    /// (256 kHz / 2 MHz); 1 is Multi-play-16.
    pub fn from_swi_arg(r1: u32) -> Mode {
        match r1 {
            1 => Mode::Multi16,
            _ => Mode::Normal32,
        }
    }

    fn crc_start(self) -> u32 {
        match self {
            Mode::Normal32 => CRC_NORMAL_START,
            Mode::Multi16 => CRC_MULTI_START,
        }
    }
    fn crc_xor(self) -> u32 {
        match self {
            Mode::Normal32 => CRC_NORMAL_XOR,
            Mode::Multi16 => CRC_MULTI_XOR,
        }
    }
    fn data_xor(self) -> u32 {
        match self {
            Mode::Normal32 => DATA_NORMAL_XOR,
            Mode::Multi16 => DATA_MULTI_XOR,
        }
    }
}

/// The subset of GBATEK's `MultiBootParam` (the r0 struct) the HLE reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct MultiBootParam {
    pub handshake_data: u8,
    pub client_data: [u8; 3],
    pub palette_data: u8,
    pub client_bit: u8,
    pub boot_srcp: u32,
    pub boot_endp: u32,
}

impl MultiBootParam {
    /// Parse the fields we use from a `MultiBootParam` image read out of GBA
    /// memory (at least 0x28 bytes, at the documented offsets).
    pub fn parse(b: &[u8]) -> Option<MultiBootParam> {
        if b.len() < 0x28 {
            return None;
        }
        let rd32 = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Some(MultiBootParam {
            handshake_data: b[0x14],
            client_data: [b[0x19], b[0x1A], b[0x1B]],
            palette_data: b[0x1C],
            client_bit: b[0x1E],
            boot_srcp: rd32(0x20),
            boot_endp: rd32(0x24),
        })
    }

    /// Payload byte count (after the 0xC0 header), or `None` if the
    /// `boot_srcp..boot_endp` range violates the hardware constraints.
    pub fn payload_len(&self) -> Option<u32> {
        let len = self.boot_endp.checked_sub(self.boot_srcp)?;
        if !(MIN_PAYLOAD..=MAX_PAYLOAD).contains(&len) || len % 0x10 != 0 {
            return None;
        }
        Some(len)
    }
}

/// Per-transfer crypto state: the rolling session key (`seed`) and CRC
/// accumulator. Encrypt each payload word with `encrypt_word` (in order from
/// the first payload word), feed each *plaintext* word to `crc_word`, then
/// `finish_crc` with the final factor to get the verification CRC.
pub struct Session {
    mode: Mode,
    seed: u32,
    crc: u32,
}

impl Session {
    /// Start a session. `seed` is `palette_data | client_data[0..2] << 8..24`;
    /// the CRC accumulator starts from the per-mode constant.
    pub fn new(mode: Mode, param: &MultiBootParam) -> Session {
        let seed = param.palette_data as u32
            | (param.client_data[0] as u32) << 8
            | (param.client_data[1] as u32) << 16
            | (param.client_data[2] as u32) << 24;
        Session {
            mode,
            seed,
            crc: mode.crc_start(),
        }
    }

    /// Encrypt one payload word. `word_index` is the word's index from the start
    /// of the image (the first payload word is `HEADER_SIZE/4`). Advances the
    /// session key first, then applies the address + key + mode transform —
    /// matching `LinkCableMultiboot::sendRomPart`.
    pub fn encrypt_word(&mut self, plain: u32, word_index: u32) -> u32 {
        self.seed = self.seed.wrapping_mul(SEED_MULTIPLIER).wrapping_add(1);
        let addr_term = 0xFE00_0000u32.wrapping_sub(word_index << 2);
        let base = plain ^ addr_term ^ self.seed;
        base ^ self.mode.data_xor()
    }

    /// Fold one *plaintext* payload word into the CRC (the LSB-first shift-XOR
    /// from `calculateCRCData`).
    pub fn crc_word(&mut self, mut data: u32) {
        let xor = self.mode.crc_xor();
        let mut c = self.crc;
        for _ in 0..32 {
            let bit = (c ^ data) & 1;
            data >>= 1;
            c >>= 1;
            if bit != 0 {
                c ^= xor;
            }
        }
        self.crc = c;
    }

    /// Finalize: mask to 16 bits and fold in the final factor (`handshake_data |
    /// client randoms << 8..24`), yielding the 16-bit CRC both ends compare.
    pub fn finish_crc(&mut self, final_factor: u32) -> u32 {
        self.crc &= 0xFFFF;
        self.crc_word(final_factor);
        self.crc
    }
}

/// Validate and lay a multiboot image into an EWRAM buffer for a child to boot.
/// Copies the whole image to the start of `ewram`, then stamps the BIOS-written
/// header bytes: boot mode at 0xC4 (1 = Normal, 2 = Multi-play) and client id at
/// 0xC5 (1..3). Returns the entry point (`CHILD_ENTRY`) on success, or `None`
/// if the image is too small/large for EWRAM or below the header size.
pub fn prepare_child_image(
    image: &[u8],
    ewram: &mut [u8],
    mode: Mode,
    client_id: u8,
) -> Option<u32> {
    let len = image.len();
    if len <= HEADER_SIZE as usize || len > ewram.len() {
        return None;
    }
    ewram[..len].copy_from_slice(image);
    ewram[0xC4] = match mode {
        Mode::Normal32 => 1,
        Mode::Multi16 => 2,
    };
    ewram[0xC5] = client_id;
    Some(CHILD_ENTRY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param() -> MultiBootParam {
        MultiBootParam {
            handshake_data: 0x11,
            client_data: [0xAA, 0xBB, 0xCC],
            palette_data: 0x93,
            client_bit: 0b0010,
            boot_srcp: 0x0800_00C0,
            boot_endp: 0x0800_00C0 + 0x100,
        }
    }

    #[test]
    fn payload_len_constraints() {
        let mut p = param();
        assert_eq!(p.payload_len(), Some(0x100));
        // Too small.
        p.boot_endp = p.boot_srcp + 0x10;
        assert_eq!(p.payload_len(), None);
        // Not a multiple of 0x10.
        p.boot_endp = p.boot_srcp + 0x108;
        assert_eq!(p.payload_len(), None);
        // Too large.
        p.boot_endp = p.boot_srcp + MAX_PAYLOAD + 0x10;
        assert_eq!(p.payload_len(), None);
    }

    #[test]
    fn parse_reads_documented_offsets() {
        let mut b = vec![0u8; 0x28];
        b[0x14] = 0x11;
        b[0x19] = 0xAA;
        b[0x1A] = 0xBB;
        b[0x1B] = 0xCC;
        b[0x1C] = 0x93;
        b[0x1E] = 0b0010;
        b[0x20..0x24].copy_from_slice(&0x0800_00C0u32.to_le_bytes());
        b[0x24..0x28].copy_from_slice(&0x0800_01C0u32.to_le_bytes());
        let p = MultiBootParam::parse(&b).unwrap();
        assert_eq!(p.handshake_data, 0x11);
        assert_eq!(p.client_data, [0xAA, 0xBB, 0xCC]);
        assert_eq!(p.palette_data, 0x93);
        assert_eq!(p.client_bit, 0b0010);
        assert_eq!(p.boot_srcp, 0x0800_00C0);
        assert_eq!(p.boot_endp, 0x0800_01C0);
        assert_eq!(p.payload_len(), Some(0x100));
        // Short buffers are rejected.
        assert!(MultiBootParam::parse(&b[..0x20]).is_none());
    }

    #[test]
    fn seed_init_and_lcg_advance() {
        let p = param();
        let mut s = Session::new(Mode::Normal32, &p);
        // seed = palette | c0<<8 | c1<<16 | c2<<24.
        assert_eq!(s.seed, 0xCCBB_AA93);
        // First encrypt advances the LCG once before transforming.
        let before = s.seed;
        let _ = s.encrypt_word(0, 0x30);
        assert_eq!(s.seed, before.wrapping_mul(SEED_MULTIPLIER).wrapping_add(1));
    }

    #[test]
    fn encrypt_word_matches_reference_transform() {
        let p = param();
        let mut s = Session::new(Mode::Normal32, &p);
        // Reproduce sendRomPart for the first payload word (index 0x30 = byte
        // 0xC0): seed advances, then base = plain ^ (0xFE000000 - (i<<2)) ^ seed,
        // then ^ DATA_NORMAL_XOR.
        let plain = 0xDEAD_BEEFu32;
        let i = HEADER_SIZE / 4; // 0x30
        let expected_seed = 0xCCBB_AA93u32
            .wrapping_mul(SEED_MULTIPLIER)
            .wrapping_add(1);
        let base = plain ^ (0xFE00_0000u32.wrapping_sub(i << 2)) ^ expected_seed;
        let expected = base ^ DATA_NORMAL_XOR;
        assert_eq!(s.encrypt_word(plain, i), expected);
    }

    #[test]
    fn crc_is_deterministic_and_factor_sensitive() {
        let p = param();
        let words = [0x1111_1111u32, 0x2222_2222, 0xDEAD_BEEF, 0x0000_0001];
        let run = |factor: u32| {
            let mut s = Session::new(Mode::Normal32, &p);
            for (k, &w) in words.iter().enumerate() {
                s.crc_word(w);
                // (encryption advances seed independently; CRC is plaintext)
                let _ = s.encrypt_word(w, HEADER_SIZE / 4 + k as u32);
            }
            s.finish_crc(factor)
        };
        let a = run(0x11);
        // 16-bit result, and a different final factor changes it.
        assert_eq!(a & 0xFFFF_0000, 0);
        assert_ne!(a, run(0x12));
        // Deterministic.
        assert_eq!(a, run(0x11));
    }

    #[test]
    fn modes_differ() {
        let p = param();
        let mut n = Session::new(Mode::Normal32, &p);
        let mut m = Session::new(Mode::Multi16, &p);
        assert_ne!(n.encrypt_word(0xABCD, 0x30), m.encrypt_word(0xABCD, 0x30));
    }

    #[test]
    fn prepare_child_image_lays_out_ewram() {
        // Header (0xC0) + a small payload.
        let mut image = vec![0u8; 0xC0 + 0x40];
        image[0xC0] = 0x12;
        image[0xC1] = 0x34;
        let mut ewram = vec![0u8; 0x4_0000];
        let entry = prepare_child_image(&image, &mut ewram, Mode::Normal32, 1).unwrap();
        assert_eq!(entry, 0x0200_00C0);
        assert_eq!(ewram[0xC0], 0x12);
        assert_eq!(ewram[0xC1], 0x34);
        assert_eq!(ewram[0xC4], 1); // boot mode = Normal
        assert_eq!(ewram[0xC5], 1); // client id
        // Header-only / oversize images are rejected.
        assert!(prepare_child_image(&image[..0xC0], &mut ewram, Mode::Normal32, 1).is_none());
        assert!(prepare_child_image(&vec![0u8; 0x5_0000], &mut ewram, Mode::Normal32, 1).is_none());
    }
}
