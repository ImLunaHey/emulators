//! Game Boy Color memory-map sizes and region boundaries.
//!
//! Spec: Pan Docs — Memory Map (gbdev.io/pandocs/Memory_Map.html). The CGB is a
//! superset of the DMG: VRAM gains a second bank (selected by VBK 0xFF4F) and
//! WRAM gains banks 1-7 (selected by SVBK 0xFF70).
//!
//! ```text
//! 0x0000-0x3FFF  ROM bank 0           (cart, fixed)
//! 0x4000-0x7FFF  ROM bank N           (cart, MBC-switched)
//! 0x8000-0x9FFF  VRAM                 (2 banks on CGB, VBK)
//! 0xA000-0xBFFF  External cart RAM    (MBC-switched)
//! 0xC000-0xCFFF  WRAM bank 0          (fixed)
//! 0xD000-0xDFFF  WRAM bank 1-7        (CGB: SVBK; DMG: fixed bank 1)
//! 0xE000-0xFDFF  Echo RAM             (mirror of 0xC000-0xDDFF)
//! 0xFE00-0xFE9F  OAM (sprite attrs)
//! 0xFEA0-0xFEFF  Not usable
//! 0xFF00-0xFF7F  IO registers
//! 0xFF80-0xFFFE  HRAM
//! 0xFFFF         IE (interrupt enable)
//! ```

// ---- Region sizes (bytes) ----
/// One ROM bank window (bank 0 and the switchable bank are each 16 KiB).
pub const ROM_BANK_SIZE: usize = 0x4000; // 16 KiB
/// One VRAM bank. CGB has two; DMG has one.
pub const VRAM_BANK_SIZE: usize = 0x2000; // 8 KiB
/// VRAM banks on a CGB.
pub const VRAM_BANKS: usize = 2;
/// One external (cartridge) RAM bank.
pub const ERAM_BANK_SIZE: usize = 0x2000; // 8 KiB
/// One WRAM bank window. CGB has 8 banks (bank 0 fixed + banks 1-7 switchable).
pub const WRAM_BANK_SIZE: usize = 0x1000; // 4 KiB
/// WRAM banks on a CGB (bank 0..=7).
pub const WRAM_BANKS: usize = 8;
/// Object Attribute Memory (40 sprites x 4 bytes).
pub const OAM_SIZE: usize = 0xA0; // 160 bytes
/// IO register window 0xFF00-0xFF7F.
pub const IO_SIZE: usize = 0x80; // 128 bytes
/// High RAM 0xFF80-0xFFFE.
pub const HRAM_SIZE: usize = 0x7F; // 127 bytes
/// CGB background/object palette RAM: 8 palettes x 4 colors x 2 bytes.
pub const CRAM_SIZE: usize = 0x40; // 64 bytes each for BG and OBJ

// ---- Region boundaries (inclusive start, exclusive end) ----
pub const ROM0_START: u16 = 0x0000;
pub const ROM0_END: u16 = 0x4000;
pub const ROMN_START: u16 = 0x4000;
pub const ROMN_END: u16 = 0x8000;
pub const VRAM_START: u16 = 0x8000;
pub const VRAM_END: u16 = 0xA000;
pub const ERAM_START: u16 = 0xA000;
pub const ERAM_END: u16 = 0xC000;
pub const WRAM0_START: u16 = 0xC000;
pub const WRAM0_END: u16 = 0xD000;
pub const WRAMN_START: u16 = 0xD000;
pub const WRAMN_END: u16 = 0xE000;
pub const ECHO_START: u16 = 0xE000;
pub const ECHO_END: u16 = 0xFE00;
pub const OAM_START: u16 = 0xFE00;
pub const OAM_END: u16 = 0xFEA0;
pub const UNUSABLE_START: u16 = 0xFEA0;
pub const UNUSABLE_END: u16 = 0xFF00;
pub const IO_START: u16 = 0xFF00;
pub const IO_END: u16 = 0xFF80;
pub const HRAM_START: u16 = 0xFF80;
pub const HRAM_END: u16 = 0xFFFF;
pub const IE_REGISTER: u16 = 0xFFFF;

// ---- IO register addresses (the subset the foundation models directly) ----
pub const REG_IF: u16 = 0xFF0F; // Interrupt flag
pub const REG_KEY1: u16 = 0xFF4D; // CGB double-speed prepare/status
pub const REG_VBK: u16 = 0xFF4F; // VRAM bank select (CGB)
pub const REG_HDMA1: u16 = 0xFF51; // HDMA source high
pub const REG_HDMA2: u16 = 0xFF52; // HDMA source low
pub const REG_HDMA3: u16 = 0xFF53; // HDMA dest high
pub const REG_HDMA4: u16 = 0xFF54; // HDMA dest low
pub const REG_HDMA5: u16 = 0xFF55; // HDMA length/mode/start
pub const REG_BCPS: u16 = 0xFF68; // BG palette index (a.k.a. BGPI)
pub const REG_BCPD: u16 = 0xFF69; // BG palette data  (a.k.a. BGPD)
pub const REG_OCPS: u16 = 0xFF6A; // OBJ palette index (a.k.a. OBPI)
pub const REG_OCPD: u16 = 0xFF6B; // OBJ palette data  (a.k.a. OBPD)
pub const REG_SVBK: u16 = 0xFF70; // WRAM bank select (CGB)
