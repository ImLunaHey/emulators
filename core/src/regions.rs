//! GBA memory map sizes / masks. Ported from src/memory/regions.ts.

pub const BIOS_SIZE: usize = 0x4000; // 16 KB
pub const EWRAM_SIZE: usize = 0x40000; // 256 KB
pub const IWRAM_SIZE: usize = 0x8000; // 32 KB
pub const IO_SIZE: usize = 0x400; // 1 KB
pub const PRAM_SIZE: usize = 0x400; // 1 KB
pub const VRAM_SIZE: usize = 0x18000; // 96 KB
pub const OAM_SIZE: usize = 0x400; // 1 KB
pub const SRAM_SIZE: usize = 0x10000; // 64 KB visible (Flash 128K banked)
pub const FLASH_SIZE: usize = 0x20000; // 128 KB

pub const REGION_BIOS: u32 = 0x0;
pub const REGION_EWRAM: u32 = 0x2;
pub const REGION_IWRAM: u32 = 0x3;
pub const REGION_IO: u32 = 0x4;
pub const REGION_PRAM: u32 = 0x5;
pub const REGION_VRAM: u32 = 0x6;
pub const REGION_OAM: u32 = 0x7;
pub const REGION_ROM_0: u32 = 0x8;
pub const REGION_ROM_1: u32 = 0x9;
pub const REGION_ROM_2: u32 = 0xA;
pub const REGION_ROM_3: u32 = 0xB;
pub const REGION_ROM_4: u32 = 0xC;
pub const REGION_ROM_5: u32 = 0xD;
pub const REGION_SRAM: u32 = 0xE;
pub const REGION_SRAM2: u32 = 0xF;
