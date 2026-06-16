//! ARM9 view of the DS memory map. Ported from
//! ../../ds-recomp/src/memory/bus9.ts.
//!
//! Routes reads/writes to the right backing block based on the top byte of
//! the address. CP15-controlled TCMs are checked first (their address ranges
//! are programmable) and otherwise we fall through to the standard map.
//!
//! Ownership (see CONTRACT.md): unlike the TS `Bus9`, this struct owns ONLY
//! the ARM9-private state — the ITCM/DTCM SRAM blocks and their CP15-driven
//! base/size/enable/load-mode config. Everything shared (Main RAM, WRAM,
//! PRAM, OAM, VRAM, BIOS) lives in `SharedMemory`, and the VRAM routing in
//! `VramRouter`; both are passed as `&mut`/`&` parameters by the `Nds`
//! god-struct. IO (region 0x4) is NOT handled here — the bus methods return
//! a `Resolved::Io` marker and the god-struct routes it (the TS `Bus`↔`Io`
//! cycle resolved the GBA-core way).
//!
//! Endianness: the DS is little-endian; the byte-assembling helpers below
//! match the TS `DataView` little-endian access.

use super::regions::{
    DTCM_SIZE, ITCM_SIZE, MAIN_RAM_MASK, OAM_BASE, OAM_SIZE, PRAM_BASE, PRAM_SIZE, SHARED_WRAM_MASK,
};
use super::shared::{boxed_region, SharedMemory, WramCnt};
use super::vram_router::VramRouter;

/// Where an address resolved to: a concrete `(slice, index)` backing store, an
/// IO access (the god-struct routes those), or unmapped (reads 0).
pub enum Resolved<'a> {
    Mem(&'a mut [u8], usize),
    Io,
    None,
}

pub struct Bus9 {
    // ITCM/DTCM are ARM9-private — fast on-die SRAM. CP15 control regs pick
    // the base + virtual size; physical size is fixed (16 KB DTCM, 32 KB
    // ITCM). When virtual > physical the TCM mirrors. Load-mode bits (CP15
    // ctrl bits 17 / 19) cause reads to bypass the TCM and fall through to
    // whatever's below it, while writes still go to TCM.
    pub itcm: Box<[u8; ITCM_SIZE]>,
    pub dtcm: Box<[u8; DTCM_SIZE]>,
    pub itcm_base: u32,
    pub itcm_virtual_size: u32,
    pub itcm_enabled: bool,
    pub itcm_load_mode: bool,
    pub dtcm_base: u32,
    pub dtcm_virtual_size: u32,
    pub dtcm_enabled: bool,
    pub dtcm_load_mode: bool,
}

impl Default for Bus9 {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus9 {
    pub fn new() -> Self {
        Bus9 {
            itcm: boxed_region(),
            dtcm: boxed_region(),
            itcm_base: 0x0000_0000,
            itcm_virtual_size: ITCM_SIZE as u32,
            itcm_enabled: true,
            itcm_load_mode: false,
            // Nintendo's typical placement at the end of main RAM.
            dtcm_base: 0x027C_0000,
            dtcm_virtual_size: DTCM_SIZE as u32,
            dtcm_enabled: true,
            dtcm_load_mode: false,
        }
    }

    #[inline]
    fn is_io(addr: u32) -> bool {
        (addr >> 24) == 0x04
    }

    /// Translate an address to a `(slice, index)` into the right backing
    /// store, or `Io`/`None`. `for_write` matters for the TCM load-mode bits:
    /// in load mode, reads from the TCM range fall through to the underlying
    /// memory but writes still land in the TCM.
    ///
    /// Takes the shared memory + VRAM router + PPU's VRAMCNT registers as
    /// parameters (the TS stored them as fields; we pass them in).
    pub fn resolve<'a>(
        &'a mut self,
        addr: u32,
        for_write: bool,
        mem: &'a mut SharedMemory,
        vram: &VramRouter,
        vramcnt: &[u8; 9],
    ) -> Resolved<'a> {
        if Self::is_io(addr) {
            return Resolved::Io;
        }

        // DTCM (programmable base/size). Checked before ITCM and the map.
        if self.dtcm_enabled
            && addr >= self.dtcm_base
            && addr < self.dtcm_base.wrapping_add(self.dtcm_virtual_size)
        {
            if !self.dtcm_load_mode || for_write {
                let idx = (addr - self.dtcm_base) as usize & (DTCM_SIZE - 1);
                return Resolved::Mem(&mut self.dtcm[..], idx);
            }
            // Load mode + read: fall through to whatever maps this address
            // below DTCM (typically main RAM).
        }
        // ITCM.
        if self.itcm_enabled
            && addr >= self.itcm_base
            && addr < self.itcm_base.wrapping_add(self.itcm_virtual_size)
        {
            if !self.itcm_load_mode || for_write {
                let idx = (addr - self.itcm_base) as usize & (ITCM_SIZE - 1);
                return Resolved::Mem(&mut self.itcm[..], idx);
            }
        }

        // BIOS region (low + high vectors mirror).
        if addr < 0x4000 {
            return Resolved::Mem(&mut mem.bios_arm9[..], addr as usize);
        }
        if (0xFFFF_0000..0xFFFF_4000).contains(&addr) {
            return Resolved::Mem(&mut mem.bios_arm9[..], (addr - 0xFFFF_0000) as usize);
        }
        // Main RAM mirrors fill 0x02000000–0x02FFFFFF, plus a Nintendo-SDK
        // alias at 0x01000000–0x01FFFFFF (Pokemon Platinum relocates the IRQ
        // handler there; matches the CP15 protection region the game programs).
        let top = addr >> 24;
        if top == 0x02 || top == 0x01 {
            let idx = (addr & MAIN_RAM_MASK) as usize;
            return Resolved::Mem(&mut mem.main_ram[..], idx);
        }
        // Shared WRAM block, split by WRAMCNT (ARM9 view).
        if top == 0x03 {
            return match mem.wramcnt {
                WramCnt::AllToArm9 => {
                    let idx = (addr & SHARED_WRAM_MASK) as usize;
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
                WramCnt::UpperToArm9 => {
                    let idx = 0x4000 + (addr & 0x3FFF) as usize; // upper half
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
                WramCnt::LowerToArm9 => {
                    let idx = (addr & 0x3FFF) as usize; // lower half
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
                WramCnt::AllToArm7 => Resolved::None, // ARM9 sees nothing
            };
        }
        if (PRAM_BASE..PRAM_BASE + PRAM_SIZE as u32).contains(&addr) {
            let idx = (addr - PRAM_BASE) as usize;
            return Resolved::Mem(&mut mem.pram[..], idx);
        }
        // VRAM ranges all go through the bank router (respects VRAMCNT_x).
        if (0x0600_0000..0x0700_0000).contains(&addr) {
            return match vram.resolve_arm9(addr, vramcnt) {
                Some(idx) => Resolved::Mem(&mut mem.vram[..], idx),
                None => Resolved::None,
            };
        }
        if (OAM_BASE..OAM_BASE + OAM_SIZE as u32).contains(&addr) {
            let idx = (addr - OAM_BASE) as usize;
            return Resolved::Mem(&mut mem.oam[..], idx);
        }
        Resolved::None
    }
}
