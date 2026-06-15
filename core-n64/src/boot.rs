//! HLE boot — a high-level emulation of the PIF / IPL handoff.
//!
//! On real hardware the PIF runs IPL1/IPL2, then the cart's IPL3 (the first
//! 0x1000 bytes after the header) copies the game's boot segment from the cart
//! into RDRAM at 0x80000400 and jumps to the header's entry point, leaving the
//! CPU registers in a documented state that depends on the CIC chip.
//!
//! Reproducing IPL3 exactly requires the (copyrighted) boot ROM, so we HLE it:
//! copy the boot segment (header + IPL3 + first MB region) into RDRAM the way
//! IPL3 would, set the GPRs/COP0 to the post-IPL3 state, and jump to the entry
//! point. Register values follow the widely-documented "cic 6102" boot state
//! (the most common CIC) from the n64brew wiki "Boot process".
//!
//! `setup` is called by the god-struct after a ROM is loaded; it mutates the
//! CPU and returns the bytes to copy into RDRAM (so the caller, which owns
//! RDRAM, performs the write — keeping the borrow simple).

use crate::cart::Header;
use crate::cpu::cop0::{R_CONFIG, R_COUNT, R_STATUS};
use crate::cpu::Cpu;

/// RDRAM virtual address IPL3 copies the boot segment to (0x80000400).
pub const BOOT_SEGMENT_VADDR: u32 = 0x8000_0400;
/// Physical RDRAM offset of the boot segment.
pub const BOOT_SEGMENT_PADDR: usize = 0x0000_0400;
/// Size of the boot segment IPL3 copies (1 MB minus the 0x1000 header+IPL3).
pub const BOOT_SEGMENT_SIZE: usize = 0x10_0000 - 0x1000;

/// Result of HLE boot setup: the bytes to copy into RDRAM and the offset.
pub struct BootCopy {
    pub rdram_offset: usize,
    pub bytes: Vec<u8>,
}

/// Configure the CPU into the post-IPL3 state for a loaded (normalised) ROM and
/// return the boot-segment copy the caller must apply to RDRAM. `rom` is the
/// big-endian cart image. Returns `None` if the ROM has no valid header.
pub fn setup(cpu: &mut Cpu, rom: &[u8]) -> Option<BootCopy> {
    let header = Header::parse(rom)?;

    // --- COP0 to a sane running state: leave kernel mode, clear BEV/ERL so
    // exceptions take the RAM vectors, and zero Count. (IPL3 has finished, so
    // we are no longer in the reset error state.)
    cpu.cop0.reg[R_STATUS] = 0x2400_0000; // CU1|CU0 set, BEV/ERL/EXL clear, IE clear
    cpu.cop0.reg[R_CONFIG] = 0x7006_E463;
    cpu.cop0.reg[R_COUNT] = 0;

    // --- GPRs as IPL3 (CIC 6102) leaves them. The well-known subset that
    // matters: the boot code reads these. Values from the n64brew wiki.
    let g = &mut cpu.regs;
    g[1] = 0x0000_0000_0000_0001;
    g[2] = 0xFFFF_FFFF_0EBD_A536;
    g[3] = 0xFFFF_FFFF_0EBD_A536;
    g[4] = 0x0000_0000_0000_A536;
    g[5] = 0xFFFF_FFFF_C0F1_D859;
    g[6] = 0xFFFF_FFFF_A4001F0C;
    g[7] = 0xFFFF_FFFF_A4001F08;
    g[8] = 0x0000_0000_0000_00C0;
    g[10] = 0x0000_0000_0000_0040;
    g[11] = 0xFFFF_FFFF_A400_0040;
    g[12] = 0xFFFF_FFFF_ED10_D0B3;
    g[13] = 0x0000_0000_1402_A4CC;
    g[14] = 0xFFFF_FFFF_2DE1_08EA;
    g[15] = 0x0000_0000_3103_E121;
    g[19] = 0x0000_0000_0000_0001; // osRomType (cart)
    g[20] = 0x0000_0000_0000_0001; // osTvType (NTSC)
    g[21] = 0x0000_0000_0000_0000;
    g[22] = 0x0000_0000_0000_003F; // osCicId (6102)
    g[23] = 0x0000_0000_0000_0006;
    g[24] = 0x0000_0000_0000_0000;
    g[25] = 0xFFFF_FFFF_9DEB_B54F;
    // Stack pointer (sp / r29) at the top of the cached RDRAM region.
    g[29] = 0xFFFF_FFFF_A400_1FF0;
    g[31] = 0xFFFF_FFFF_A400_1550; // ra
    g[0] = 0;

    // --- jump to the cart's entry point (a KSEG0/KSEG1 virtual address).
    let entry = header.entry_point as i32 as i64 as u64;
    cpu.pc = entry;
    cpu.next_pc = entry.wrapping_add(4);
    cpu.current_pc = entry;
    cpu.in_delay_slot = false;
    cpu.branch_taken = false;
    cpu.exception_pending = false;

    // --- boot-segment copy: cart [0x1000 ..] -> RDRAM 0x400. IPL3 copies the
    // segment whose length is encoded in the boot block, but copying up to the
    // available ROM (capped at the boot-segment window) is sufficient for the
    // entry code to be present at its expected address.
    let src_start = 0x1000;
    let avail = rom.len().saturating_sub(src_start);
    let n = avail.min(BOOT_SEGMENT_SIZE);
    let bytes = rom[src_start..src_start + n].to_vec();

    Some(BootCopy {
        rdram_offset: BOOT_SEGMENT_PADDR,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_entry(entry: u32) -> Vec<u8> {
        let mut rom = vec![0u8; 0x101000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // z64 magic
        rom[0x08..0x0C].copy_from_slice(&entry.to_be_bytes());
        // Put a recognisable byte at the start of the boot segment (0x1000).
        rom[0x1000] = 0xAB;
        rom
    }

    #[test]
    fn setup_jumps_to_entry_and_clears_bev() {
        let mut cpu = Cpu::new();
        let copy = setup(&mut cpu, &rom_with_entry(0x8000_0400)).unwrap();
        assert_eq!(cpu.pc, 0xFFFF_FFFF_8000_0400);
        assert_eq!(cpu.next_pc, 0xFFFF_FFFF_8000_0404);
        // BEV cleared post-IPL3.
        assert_eq!(cpu.cop0.status() & crate::cpu::cop0::ST_BEV, 0);
    }

    #[test]
    fn setup_copies_boot_segment() {
        let mut cpu = Cpu::new();
        let copy = setup(&mut cpu, &rom_with_entry(0x8000_0400)).unwrap();
        assert_eq!(copy.rdram_offset, 0x400);
        assert_eq!(copy.bytes[0], 0xAB); // first byte of the boot segment
    }

    #[test]
    fn setup_sets_sp_into_rdram() {
        let mut cpu = Cpu::new();
        let _ = setup(&mut cpu, &rom_with_entry(0x8000_0400)).unwrap();
        assert_eq!(cpu.regs[29], 0xFFFF_FFFF_A400_1FF0);
    }
}
