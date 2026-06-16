//! Nintendo 64 memory-map constants: physical-address ranges of every RCP
//! register block, RDRAM, the PIF, and the cartridge domain.
//!
//! Built from the n64brew wiki "Memory map". The VR4300 exposes a 64-bit
//! virtual space, but software (and IPL3) run almost entirely in the 32-bit
//! compatibility segments KSEG0 (0x8000_0000.., cached) and KSEG1
//! (0xA000_0000.., uncached), both of which alias the same *physical* space
//! `vaddr & 0x1FFF_FFFF`. We translate with [`virt_to_phys`] and route on the
//! resulting physical address.
//!
//! The N64 is BIG-ENDIAN: multi-byte values in RDRAM / ROM are stored MSB-first.

// ---- RDRAM (main memory) ----
/// 4 MB base RDRAM; an Expansion Pak adds a second 4 MB for 8 MB total. We
/// always allocate the full 8 MB so an Expansion-Pak title never faults — the
/// RI/RDRAM config registers report 8 MB.
pub const RDRAM_SIZE: usize = 0x80_0000; // 8 MB
pub const RDRAM_BASE: u32 = 0x0000_0000;
pub const RDRAM_END: u32 = 0x03F0_0000; // RDRAM registers begin here

// ---- RDRAM registers (memory controller) ----
pub const RDRAM_REGS_BASE: u32 = 0x03F0_0000;
pub const RDRAM_REGS_END: u32 = 0x0400_0000;

// ---- RSP (Reality Signal Processor) ----
pub const SP_DMEM_BASE: u32 = 0x0400_0000; // 4 KB data memory
pub const SP_DMEM_END: u32 = 0x0400_1000;
pub const SP_IMEM_BASE: u32 = 0x0400_1000; // 4 KB instruction memory
pub const SP_IMEM_END: u32 = 0x0400_2000;
pub const SP_REGS_BASE: u32 = 0x0404_0000; // SP control registers
pub const SP_REGS_END: u32 = 0x0404_2000;

// ---- RDP (Reality Display Processor) command + span registers ----
pub const DP_CMD_BASE: u32 = 0x0410_0000;
pub const DP_CMD_END: u32 = 0x0410_0020;
pub const DP_SPAN_BASE: u32 = 0x0420_0000;
pub const DP_SPAN_END: u32 = 0x0420_0010;

// ---- MI (MIPS Interface) — interrupt + version registers ----
pub const MI_BASE: u32 = 0x0430_0000;
pub const MI_END: u32 = 0x0430_0010;

// ---- VI (Video Interface) ----
pub const VI_BASE: u32 = 0x0440_0000;
pub const VI_END: u32 = 0x0440_0038;

// ---- AI (Audio Interface) ----
pub const AI_BASE: u32 = 0x0450_0000;
pub const AI_END: u32 = 0x0450_0018;

// ---- PI (Peripheral Interface) — cartridge DMA ----
pub const PI_BASE: u32 = 0x0460_0000;
pub const PI_END: u32 = 0x0460_0034;

// ---- RI (RDRAM Interface) ----
pub const RI_BASE: u32 = 0x0470_0000;
pub const RI_END: u32 = 0x0470_0020;

// ---- SI (Serial Interface) — PIF/controller access ----
pub const SI_BASE: u32 = 0x0480_0000;
pub const SI_END: u32 = 0x0480_001C;

// ---- Cartridge domains (PI bus) ----
/// Cartridge ROM (domain 1, address 2) — the game image is mapped here.
pub const CART_ROM_BASE: u32 = 0x1000_0000;
pub const CART_ROM_END: u32 = 0x1FC0_0000;

// ---- PIF ----
pub const PIF_ROM_BASE: u32 = 0x1FC0_0000; // 2 KB boot ROM
pub const PIF_ROM_END: u32 = 0x1FC0_07C0;
pub const PIF_RAM_BASE: u32 = 0x1FC0_07C0; // 64 B PIF RAM (joybus command area)
pub const PIF_RAM_END: u32 = 0x1FC0_0800;

/// Translate a 32-bit virtual address to a physical address. KSEG0/KSEG1 are
/// the cached/uncached 512 MB direct-mapped windows over the low 512 MB of
/// physical space; both mask to `vaddr & 0x1FFF_FFFF`. KUSEG/KSEG2/KSEG3 would
/// require the TLB — for the foundation we treat any address by stripping the
/// top three bits, which is correct for the direct-mapped segments IPL3 and
/// game boot code use. (Mapped TLB segments are handled by the TLB in COP0;
/// the bus is reached only after translation.)
#[inline]
pub fn virt_to_phys(vaddr: u32) -> u32 {
    vaddr & 0x1FFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kseg0_and_kseg1_alias_same_physical() {
        assert_eq!(virt_to_phys(0x8000_1234), 0x0000_1234);
        assert_eq!(virt_to_phys(0xA000_1234), 0x0000_1234);
    }

    #[test]
    fn cart_rom_entry_is_in_kseg1() {
        // The cart is mapped at 0xB000_0000 (KSEG1 view of 0x1000_0000).
        assert_eq!(virt_to_phys(0xB000_0000), CART_ROM_BASE);
    }
}
