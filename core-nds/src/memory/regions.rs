//! Nintendo DS memory map constants. Ported from
//! ../../ds-recomp/src/memory/regions.ts.
//!
//! Both CPUs share Main RAM (4 MB) but otherwise see different regions —
//! ARM9 has CP15 TCMs and the bigger VRAM window, ARM7 has its own 64 KB
//! IWRAM and the WRAM block. GBATEK §"DS Memory Map" is the canonical
//! reference.

// Main RAM (shared 4 MB block, mirrored in higher 24 MB).
pub const MAIN_RAM_BASE: u32 = 0x0200_0000;
pub const MAIN_RAM_SIZE: usize = 4 * 1024 * 1024;
pub const MAIN_RAM_MASK: u32 = (MAIN_RAM_SIZE as u32) - 1;

// Shared WRAM (32 KB block — can be split 0/16/32 KB between CPUs by
// WRAMCNT on ARM9). Visible to ARM9 at 0x03000000 and to ARM7 at
// 0x03000000 (when allocated to it) plus its own 64 KB IWRAM at 0x03800000.
pub const SHARED_WRAM_BASE: u32 = 0x0300_0000;
pub const SHARED_WRAM_SIZE: usize = 32 * 1024;
pub const SHARED_WRAM_MASK: u32 = (SHARED_WRAM_SIZE as u32) - 1;

// ARM7 IWRAM — 64 KB, always at 0x03800000 from ARM7's view.
pub const ARM7_IWRAM_BASE: u32 = 0x0380_0000;
pub const ARM7_IWRAM_SIZE: usize = 64 * 1024;
pub const ARM7_IWRAM_MASK: u32 = (ARM7_IWRAM_SIZE as u32) - 1;

// IO ports.
pub const IO_BASE: u32 = 0x0400_0000;

// Palette RAM (ARM9 only) — 1 KB engine A + 1 KB engine B.
pub const PRAM_BASE: u32 = 0x0500_0000;
pub const PRAM_SIZE: usize = 2 * 1024;

// VRAM (ARM9 only, lots of bank-routing complexity). Banks A..I total
// 656 KB; each bank gets mapped to an LCDC / BG / OBJ / texture / ext.
// palette slot via VRAMCNT_A..G.
pub const VRAM_BASE: u32 = 0x0600_0000;
pub const VRAM_TOTAL_SIZE: usize = 656 * 1024;

// OAM (ARM9 only) — 1 KB engine A + 1 KB engine B.
pub const OAM_BASE: u32 = 0x0700_0000;
pub const OAM_SIZE: usize = 2 * 1024;

// Cartridge ROM/RAM windows (GBA-slot compatibility).
pub const GBA_ROM_BASE: u32 = 0x0800_0000;
pub const GBA_RAM_BASE: u32 = 0x0A00_0000;

// ARM9 TCMs are configured via CP15 — they live at addresses chosen by
// the running code. We expose their (physical) sizes for the bus to
// allocate.
pub const ITCM_SIZE: usize = 32 * 1024;
pub const DTCM_SIZE: usize = 16 * 1024;

// Per-CPU BIOS region. Reads from 0x00000000..0x00003FFF (and the
// 0xFFFF0000..0xFFFF3FFF high-vector mirror on ARM9) hit this.
pub const BIOS_SIZE: usize = 16 * 1024;

// WiFi MMIO window, inside the larger ARM7 IO region. Detected before the
// general IO dispatch. Ported from ../../ds-recomp/src/io/wifi.ts.
pub const WIFI_BASE: u32 = 0x0480_0000;
pub const WIFI_END: u32 = 0x0480_8000; // exclusive
