//! NDS cartridge header parser. The first 0x200 bytes of an `.nds` file
//! describe where the ARM9/ARM7 binaries live in the ROM, where they load into
//! RAM, their entry points, plus FAT/FNT/overlay/banner offsets and the game
//! title + 4-char game code. GBATEK §"DS Cartridge Header" is the canonical
//! reference. Ported from ../../ds-recomp/src/cart/header.ts.
//!
//! Idiomatic-Rust note: the TS parser returned an object of `number`s assembled
//! from a `DataView`. Here every field is a fixed-width integer read
//! little-endian from the raw bytes. `parse` returns `Result` instead of the TS
//! `throw` so the loader can surface a malformed-ROM error without panicking.

/// Parsed DS cartridge header. All multi-byte fields are little-endian.
#[derive(Clone, Debug, Default)]
pub struct NdsHeader {
    /// 12-char ASCII game title (trailing NULs/spaces stripped).
    pub title: String,
    /// 4-char ASCII game code (e.g. "CPUE" = Pokemon Platinum USA).
    pub game_code: String,
    /// 2-char ASCII maker code (e.g. "01" = Nintendo).
    pub maker_code: String,
    /// 0 = NDS, 2 = NDS+DSi, 3 = DSi-only.
    pub unit_code: u8,
    /// ROM size = 128 KB << capacity_shift.
    pub capacity_shift: u8,
    pub rom_version: u8,

    pub arm9_rom_offset: u32,
    pub arm9_entry_addr: u32,
    pub arm9_ram_addr: u32,
    pub arm9_size: u32,

    pub arm7_rom_offset: u32,
    pub arm7_entry_addr: u32,
    pub arm7_ram_addr: u32,
    pub arm7_size: u32,

    pub fnt_offset: u32,
    pub fnt_size: u32,
    pub fat_offset: u32,
    pub fat_size: u32,

    pub arm9_overlay_offset: u32,
    pub arm9_overlay_size: u32,
    pub arm7_overlay_offset: u32,
    pub arm7_overlay_size: u32,

    pub banner_offset: u32,
    pub header_crc: u16,
    pub total_used_rom_size: u32,
}

impl NdsHeader {
    /// Parse the 512-byte header out of the ROM image. Returns `Err` when the
    /// image is shorter than the header (the TS threw here).
    pub fn parse(rom: &[u8]) -> Result<NdsHeader, HeaderError> {
        if rom.len() < 0x200 {
            return Err(HeaderError::TooSmall);
        }
        Ok(NdsHeader {
            title: read_ascii(rom, 0x000, 12),
            game_code: read_ascii(rom, 0x00C, 4),
            maker_code: read_ascii(rom, 0x010, 2),
            unit_code: rom[0x012],
            capacity_shift: rom[0x014],
            rom_version: rom[0x01E],

            arm9_rom_offset: read_u32(rom, 0x020),
            arm9_entry_addr: read_u32(rom, 0x024),
            arm9_ram_addr: read_u32(rom, 0x028),
            arm9_size: read_u32(rom, 0x02C),

            arm7_rom_offset: read_u32(rom, 0x030),
            arm7_entry_addr: read_u32(rom, 0x034),
            arm7_ram_addr: read_u32(rom, 0x038),
            arm7_size: read_u32(rom, 0x03C),

            fnt_offset: read_u32(rom, 0x040),
            fnt_size: read_u32(rom, 0x044),
            fat_offset: read_u32(rom, 0x048),
            fat_size: read_u32(rom, 0x04C),

            arm9_overlay_offset: read_u32(rom, 0x050),
            arm9_overlay_size: read_u32(rom, 0x054),
            arm7_overlay_offset: read_u32(rom, 0x058),
            arm7_overlay_size: read_u32(rom, 0x05C),

            banner_offset: read_u32(rom, 0x068),
            header_crc: read_u16(rom, 0x15E),
            total_used_rom_size: read_u32(rom, 0x080),
        })
    }
}

/// Why a header parse failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderError {
    /// ROM image is smaller than the 512-byte header.
    TooSmall,
}

/// Read a little-endian u16 at `off` (0 if out of range).
#[inline]
pub(crate) fn read_u16(rom: &[u8], off: usize) -> u16 {
    let b0 = rom.get(off).copied().unwrap_or(0) as u16;
    let b1 = rom.get(off + 1).copied().unwrap_or(0) as u16;
    b0 | (b1 << 8)
}

/// Read a little-endian u32 at `off` (0 if out of range).
#[inline]
pub(crate) fn read_u32(rom: &[u8], off: usize) -> u32 {
    let b0 = rom.get(off).copied().unwrap_or(0) as u32;
    let b1 = rom.get(off + 1).copied().unwrap_or(0) as u32;
    let b2 = rom.get(off + 2).copied().unwrap_or(0) as u32;
    let b3 = rom.get(off + 3).copied().unwrap_or(0) as u32;
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

/// Decode an ASCII run, trimming trailing NULs/spaces, mapping non-printable
/// bytes to '?'. Mirrors the TS `readAscii`.
pub(crate) fn read_ascii(rom: &[u8], offset: usize, length: usize) -> String {
    let mut end = offset + length;
    // Trim trailing NULs / spaces.
    while end > offset {
        match rom.get(end - 1).copied() {
            Some(0) | Some(0x20) => end -= 1,
            _ => break,
        }
    }
    let mut out = String::new();
    for i in offset..end {
        let b = rom.get(i).copied().unwrap_or(0);
        out.push(if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '?'
        });
    }
    out
}

/// Human-readable unit-code name (NDS / NDS+DSi / DSi-only).
pub fn unit_code_name(unit: u8) -> &'static str {
    match unit {
        0 => "NDS",
        2 => "NDS + DSi",
        3 => "DSi-only",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rom() -> Vec<u8> {
        let mut r = vec![0u8; 0x200];
        // Title "TESTGAME" padded with NULs.
        r[0..8].copy_from_slice(b"TESTGAME");
        // Game code at 0x0C.
        r[0x0C..0x10].copy_from_slice(b"CPUE");
        // Maker code at 0x10.
        r[0x10..0x12].copy_from_slice(b"01");
        r[0x012] = 0; // unit code = NDS
        r[0x014] = 9; // capacity shift
        r[0x01E] = 1; // rom version
        // arm9 fields at 0x20..
        r[0x020..0x024].copy_from_slice(&0x4000u32.to_le_bytes());
        r[0x024..0x028].copy_from_slice(&0x0200_0800u32.to_le_bytes());
        r[0x028..0x02C].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        r[0x02C..0x030].copy_from_slice(&0x1000u32.to_le_bytes());
        // arm7 fields at 0x30..
        r[0x030..0x034].copy_from_slice(&0x8000u32.to_le_bytes());
        r[0x034..0x038].copy_from_slice(&0x0380_0000u32.to_le_bytes());
        r[0x038..0x03C].copy_from_slice(&0x0380_0000u32.to_le_bytes());
        r[0x03C..0x040].copy_from_slice(&0x800u32.to_le_bytes());
        r[0x15E..0x160].copy_from_slice(&0xABCDu16.to_le_bytes());
        r
    }

    #[test]
    fn parses_fields_le() {
        let rom = make_rom();
        let h = NdsHeader::parse(&rom).unwrap();
        assert_eq!(h.title, "TESTGAME");
        assert_eq!(h.game_code, "CPUE");
        assert_eq!(h.maker_code, "01");
        assert_eq!(h.unit_code, 0);
        assert_eq!(h.capacity_shift, 9);
        assert_eq!(h.arm9_rom_offset, 0x4000);
        assert_eq!(h.arm9_entry_addr, 0x0200_0800);
        assert_eq!(h.arm9_ram_addr, 0x0200_0000);
        assert_eq!(h.arm9_size, 0x1000);
        assert_eq!(h.arm7_ram_addr, 0x0380_0000);
        assert_eq!(h.arm7_size, 0x800);
        assert_eq!(h.header_crc, 0xABCD);
    }

    #[test]
    fn rejects_short_rom() {
        let rom = vec![0u8; 0x100];
        assert_eq!(NdsHeader::parse(&rom).unwrap_err(), HeaderError::TooSmall);
    }

    #[test]
    fn read_ascii_trims_trailing_nul_and_space() {
        let mut b = vec![0u8; 16];
        b[0..4].copy_from_slice(b"HI  ");
        assert_eq!(read_ascii(&b, 0, 8), "HI");
        // Non-printable maps to '?'.
        b[0] = 0x01;
        assert_eq!(read_ascii(&b, 0, 2), "?I");
    }

    #[test]
    fn unit_code_names() {
        assert_eq!(unit_code_name(0), "NDS");
        assert_eq!(unit_code_name(2), "NDS + DSi");
        assert_eq!(unit_code_name(3), "DSi-only");
        assert_eq!(unit_code_name(7), "unknown");
    }
}
