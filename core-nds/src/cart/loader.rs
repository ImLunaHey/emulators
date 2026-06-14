//! Cart-loading flow: parse the header, copy the ARM9/ARM7 binaries out of the
//! ROM into their configured load addresses, preload overlays, mount the cart
//! state machine, and seed the BIOS-populated RAM block the SDK init expects.
//! Ported from ../../ds-recomp/src/cart/loader.ts (plus the boot-RAM stamping
//! that the TS split between `loader.ts` and `Emulator.loadRom`).
//!
//! On real hardware the cartridge protocol streams these blocks in over the
//! encrypted KEY1 transfer; we just memcpy at boot since we HLE the
//! firmware/BIOS handoff. This is the HLE boot's data half — `bios::hle::boot`
//! drives it and then sets the CPU entry points + stacks from `LoadResult`.

use super::header::NdsHeader;
use crate::memory::regions::{ARM7_IWRAM_MASK, MAIN_RAM_MASK, SHARED_WRAM_MASK};
use crate::nds::Nds;

/// Boot parameters the HLE boot needs after the binaries are in RAM: each
/// core's entry point plus how many bytes actually landed (for diagnostics).
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadResult {
    pub arm9_entry: u32,
    pub arm7_entry: u32,
    pub arm9_bytes: u32,
    pub arm7_bytes: u32,
}

/// Bulk-copy a ROM region into the destination block. Fast paths for the common
/// "binary lands in Main RAM / shared WRAM / ARM7 IWRAM" cases write straight
/// into the backing store; everything else falls back to byte-by-byte bus
/// writes through the relevant core. Returns the byte count copied. `is_arm9`
/// selects which core's bus the fallback uses. (TS `bulkCopy`.)
pub(crate) fn bulk_copy(
    nds: &mut Nds,
    is_arm9: bool,
    dest: u32,
    rom: &[u8],
    offset: u32,
    size: u32,
) -> u32 {
    let offset = offset as usize;
    let end = (offset + size as usize).min(rom.len());
    if end <= offset {
        return 0;
    }
    let len = end - offset;
    let src = &rom[offset..end];

    // Main RAM mirror — 0x02000000..0x02FFFFFF.
    if (dest >> 24) == 0x02 {
        let dst = (dest & MAIN_RAM_MASK) as usize;
        nds.mem.main_ram[dst..dst + len].copy_from_slice(src);
        return len as u32;
    }
    // ARM7 IWRAM private region.
    if (0x0380_0000..0x0380_0000 + 0x1_0000).contains(&dest) {
        let dst = (dest & ARM7_IWRAM_MASK) as usize;
        nds.mem.arm7_iwram[dst..dst + len].copy_from_slice(src);
        return len as u32;
    }
    // Shared WRAM (0x03000000..0x037FFFFF for both buses' default route).
    if (0x0300_0000..0x0380_0000).contains(&dest) {
        // The block is only 32 KB; mask wraps a longer copy. Copy byte-by-byte
        // so an over-long binary wraps rather than panicking.
        for (i, &b) in src.iter().enumerate() {
            let d = (dest as usize + i) & SHARED_WRAM_MASK as usize;
            nds.mem.shared_wram[d] = b;
        }
        return len as u32;
    }

    // Fallback: byte-by-byte through the relevant core's bus.
    for (i, &b) in src.iter().enumerate() {
        let a = dest.wrapping_add(i as u32);
        if is_arm9 {
            nds.write8_arm9(a, b as u32);
        } else {
            nds.write8_arm7(a, b as u32);
        }
    }
    len as u32
}

/// Copy the ARM9 + ARM7 binaries into RAM and stamp the BIOS-populated
/// shared-work block (chip-ID mirrors, cart CRCs, boot handshake words, the
/// 0x170-byte header copy at 0x027FFE00). The firmware-derived fields
/// (0x027FFC80 user settings, WiFi FLASH header) are seeded separately once the
/// SPI firmware blob is wired — see the TS `Emulator.loadRom`. (TS `loadNdsRom`
/// + the BIOS-RAM half of `Emulator.loadRom`.)
pub fn load_rom(nds: &mut Nds, rom: &[u8], header: &NdsHeader) -> LoadResult {
    let arm9_bytes = bulk_copy(
        nds,
        true,
        header.arm9_ram_addr,
        rom,
        header.arm9_rom_offset,
        header.arm9_size,
    );
    let arm7_bytes = bulk_copy(
        nds,
        false,
        header.arm7_ram_addr,
        rom,
        header.arm7_rom_offset,
        header.arm7_size,
    );

    // Per GBATEK § "BIOS RAM Usage", real DS firmware/BIOS leaves a block of
    // state in main RAM at 0x027FF800-0x027FFE00 before the game's entry point
    // runs. Without these values, many retail games' SDK init reads zeros and
    // either deadlocks or crashes.
    //
    // Chip ID — Macronix-style ID encoding the ROM size, matching the cart's
    // synth_chip_id for cmd 0x90 reads.
    let mb = rom.len() / (1024 * 1024);
    let size_byte: u32 = if mb >= 128 {
        0xFF
    } else if mb >= 64 {
        0xFD
    } else if mb >= 32 {
        0xFB
    } else if mb >= 16 {
        0xF7
    } else if mb >= 8 {
        0xEF
    } else if mb >= 4 {
        0xDF
    } else {
        0xBF
    };
    let hi: u32 = if mb >= 128 { 0x80 } else { 0x00 };
    let chip_id = (hi << 24) | (size_byte << 8) | 0xC2;

    let cart_hdr_crc =
        rom.get(0x15E).copied().unwrap_or(0) as u32 | ((rom.get(0x15F).copied().unwrap_or(0) as u32) << 8);
    let cart_sec_crc =
        rom.get(0x6C).copied().unwrap_or(0) as u32 | ((rom.get(0x6D).copied().unwrap_or(0) as u32) << 8);

    {
        let ram: &mut [u8] = &mut nds.mem.main_ram[..];
        let w8 = |ram: &mut [u8], addr: u32, v: u32| {
            ram[(addr & MAIN_RAM_MASK) as usize] = (v & 0xFF) as u8;
        };
        let w16 = |ram: &mut [u8], addr: u32, v: u32| {
            w8(ram, addr, v);
            w8(ram, addr + 1, v >> 8);
        };
        let w32 = |ram: &mut [u8], addr: u32, v: u32| {
            w16(ram, addr, v);
            w16(ram, addr + 2, v >> 16);
        };

        // 0x027FF800 region — first set of BIOS-populated state.
        w32(ram, 0x027F_F800, chip_id); // NDS Gamecart Chip ID 1
        w32(ram, 0x027F_F804, chip_id); // NDS Gamecart Chip ID 2
        w16(ram, 0x027F_F808, cart_hdr_crc); // Cart Header CRC
        w16(ram, 0x027F_F80A, cart_sec_crc); // Cart Secure Area CRC
        w16(ram, 0x027F_F810, 0xFFFF); // Boot handler task (=FFFFh at cart boot)
        w16(ram, 0x027F_F850, 0x5835); // NDS7 BIOS CRC (well-known constant)
        w32(ram, 0x027F_F880, 7); // Message NDS9→NDS7 (=7 at cart boot)
        w32(ram, 0x027F_F884, 6); // NDS7 Boot Task (=6 at cart boot)

        // 0x027FFC00 region — second set, mostly mirrors of the first.
        w32(ram, 0x027F_FC00, chip_id);
        w32(ram, 0x027F_FC04, chip_id);
        w16(ram, 0x027F_FC08, cart_hdr_crc);
        w16(ram, 0x027F_FC0A, cart_sec_crc);
        w16(ram, 0x027F_FC10, 0x5835);
        w16(ram, 0x027F_FC40, 0x0001); // Boot Indicator (1 = normal cart)

        // 0x027FFE00 — first 0x170 bytes of cart header, so the game's SDK can
        // read its own header from a known location.
        for i in 0..0x170 {
            let b = rom.get(i).copied().unwrap_or(0);
            w8(ram, 0x027F_FE00 + i as u32, b as u32);
        }
    }

    LoadResult {
        arm9_entry: header.arm9_entry_addr,
        arm7_entry: header.arm7_entry_addr,
        arm9_bytes,
        arm7_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::regions::MAIN_RAM_MASK;

    fn rd32(nds: &Nds, addr: u32) -> u32 {
        let i = (addr & MAIN_RAM_MASK) as usize;
        let r = &nds.mem.main_ram;
        r[i] as u32 | ((r[i + 1] as u32) << 8) | ((r[i + 2] as u32) << 16) | ((r[i + 3] as u32) << 24)
    }

    #[test]
    fn copies_arm9_binary_into_main_ram() {
        let mut nds = Nds::new();
        let mut rom = vec![0u8; 0x8000];
        // ARM9 binary at ROM 0x4000, 16 bytes, loads to 0x02000000.
        for i in 0..16 {
            rom[0x4000 + i] = (i as u8) + 1;
        }
        let mut header = NdsHeader::default();
        header.arm9_rom_offset = 0x4000;
        header.arm9_ram_addr = 0x0200_0000;
        header.arm9_size = 16;
        header.arm9_entry_addr = 0x0200_0008;

        let res = load_rom(&mut nds, &rom, &header);
        assert_eq!(res.arm9_bytes, 16);
        assert_eq!(res.arm9_entry, 0x0200_0008);
        assert_eq!(nds.mem.main_ram[0], 1);
        assert_eq!(nds.mem.main_ram[15], 16);
    }

    #[test]
    fn stamps_bios_ram_block() {
        let mut nds = Nds::new();
        let rom = vec![0u8; 0x8000]; // < 4 MB → chip-id size byte 0xBF
        let header = NdsHeader::default();
        load_rom(&mut nds, &rom, &header);
        let chip_id = rd32(&nds, 0x027F_F800);
        assert_eq!(chip_id & 0xFF, 0xC2);
        assert_eq!((chip_id >> 8) & 0xFF, 0xBF);
        assert_eq!(rd32(&nds, 0x027F_FC00), chip_id); // mirror
        // Boot indicator.
        let i = (0x027F_FC40u32 & MAIN_RAM_MASK) as usize;
        assert_eq!(nds.mem.main_ram[i], 0x01);
    }

    #[test]
    fn bulk_copy_into_iwram() {
        let mut nds = Nds::new();
        let rom = vec![0xAB; 64];
        let n = bulk_copy(&mut nds, false, 0x0380_0000, &rom, 0, 32);
        assert_eq!(n, 32);
        assert_eq!(nds.mem.arm7_iwram[0], 0xAB);
        assert_eq!(nds.mem.arm7_iwram[31], 0xAB);
    }
}
