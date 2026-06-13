//! Memory bus. Ported from src/memory/bus.ts.
//!
//! The TS `Bus` class held a reference to `Io` (and `Io` held the bus back),
//! a cycle Rust can't express directly. We break it: `Mem` owns only the raw
//! memory regions + ROM and routes the "dumb" regions (BIOS/EWRAM/IWRAM/
//! PRAM/VRAM/OAM/ROM). The top-level `Gba` struct owns `Mem` plus every IO
//! device and implements the [`Bus`] trait, routing the IO region (0x4),
//! SRAM/Flash (0xE/0xF), EEPROM (0xD in eeprom mode), and the cart-GPIO/RTC
//! window before delegating everything else to `Mem`.
//!
//! The CPU and recompiler-free interpreter code against `&mut dyn Bus`, so
//! they never need to know which concrete device backs a given address.

use crate::regions as R;

/// The memory interface the CPU/interpreter sees. All accesses are routed
/// through this; `Gba` is the production implementor.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    fn read16(&mut self, addr: u32) -> u32;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, v: u32);
    fn write16(&mut self, addr: u32, v: u32);
    fn write32(&mut self, addr: u32, v: u32);

    /// Code-fetch helpers mirror the reads but let the impl track open bus.
    fn fetch16(&mut self, addr: u32) -> u32 {
        self.read16(addr)
    }
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }

    /// BIOS HLE SWI interception hook. The CPU calls this from
    /// `software_interrupt` before falling back to a real SVC exception;
    /// `Gba` overrides it to run the high-level BIOS. Default: not handled.
    fn try_hle_swi(&mut self, _cpu: &mut crate::cpu::Cpu, _comment: u32) -> bool {
        false
    }
}

#[inline]
fn rd16(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8)
}
#[inline]
fn rd32(b: &[u8], off: usize) -> u32 {
    (b[off] as u32)
        | ((b[off + 1] as u32) << 8)
        | ((b[off + 2] as u32) << 16)
        | ((b[off + 3] as u32) << 24)
}
#[inline]
fn wr16(b: &mut [u8], off: usize, v: u32) {
    b[off] = (v & 0xFF) as u8;
    b[off + 1] = ((v >> 8) & 0xFF) as u8;
}
#[inline]
fn wr32(b: &mut [u8], off: usize, v: u32) {
    b[off] = (v & 0xFF) as u8;
    b[off + 1] = ((v >> 8) & 0xFF) as u8;
    b[off + 2] = ((v >> 16) & 0xFF) as u8;
    b[off + 3] = ((v >> 24) & 0xFF) as u8;
}

/// Heap-allocate a zeroed fixed-size region without ever placing `N` bytes on
/// the stack (`Box::new([0; N])` would, and EWRAM is 256 KB).
#[inline]
fn boxed_region<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

// ---- const-generic, power-of-2-masked accessors for the fixed RAM regions ----
// Masking against the compile-time `N - 1` (a power of two) at the same site as
// the index lets LLVM prove the access is in-bounds and drop the bounds check.
// The redundant `& !1` / `& !3` re-assert alignment so `i + 1` / `i + 3` are
// also provably in range.
#[inline]
fn a_rd8<const N: usize>(a: &[u8; N], addr: u32) -> u32 {
    a[(addr as usize) & (N - 1)] as u32
}
#[inline]
fn a_rd16<const N: usize>(a: &[u8; N], addr: u32) -> u32 {
    let i = (addr as usize) & (N - 1) & !1;
    (a[i] as u32) | ((a[i + 1] as u32) << 8)
}
#[inline]
fn a_rd32<const N: usize>(a: &[u8; N], addr: u32) -> u32 {
    let i = (addr as usize) & (N - 1) & !3;
    (a[i] as u32)
        | ((a[i + 1] as u32) << 8)
        | ((a[i + 2] as u32) << 16)
        | ((a[i + 3] as u32) << 24)
}
#[inline]
fn a_wr8<const N: usize>(a: &mut [u8; N], addr: u32, v: u8) {
    a[(addr as usize) & (N - 1)] = v;
}
#[inline]
fn a_wr16<const N: usize>(a: &mut [u8; N], addr: u32, v: u32) {
    let i = (addr as usize) & (N - 1) & !1;
    a[i] = (v & 0xFF) as u8;
    a[i + 1] = ((v >> 8) & 0xFF) as u8;
}
#[inline]
fn a_wr32<const N: usize>(a: &mut [u8; N], addr: u32, v: u32) {
    let i = (addr as usize) & (N - 1) & !3;
    a[i] = (v & 0xFF) as u8;
    a[i + 1] = ((v >> 8) & 0xFF) as u8;
    a[i + 2] = ((v >> 16) & 0xFF) as u8;
    a[i + 3] = ((v >> 24) & 0xFF) as u8;
}

/// Raw memory regions + ROM. Owns no IO devices (see module docs).
pub struct Mem {
    // Fixed-size regions live in boxed power-of-2 arrays so the masked accessors
    // (`a_rd*` / `a_wr*`) compile without bounds checks. VRAM (96 KB) is boxed
    // too for consistency but is indexed via `vram_off`'s fold, so it keeps the
    // slice-based helpers. ROM stays a `Vec` (variable cart size).
    pub bios: Box<[u8; R::BIOS_SIZE]>,
    pub ewram: Box<[u8; R::EWRAM_SIZE]>,
    pub iwram: Box<[u8; R::IWRAM_SIZE]>,
    pub pram: Box<[u8; R::PRAM_SIZE]>,
    pub vram: Box<[u8; R::VRAM_SIZE]>,
    pub oam: Box<[u8; R::OAM_SIZE]>,
    pub rom: Vec<u8>,
    /// (size - 1) for power-of-2 ROMs, else 0 (modulo fallback).
    pub rom_mask: u32,
    pub bios_open_bus: u32,
    pub last_fetched: u32,
}

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    pub fn new() -> Self {
        Mem {
            bios: boxed_region(),
            ewram: boxed_region(),
            iwram: boxed_region(),
            pram: boxed_region(),
            vram: boxed_region(),
            oam: boxed_region(),
            rom: Vec::new(),
            rom_mask: 0,
            bios_open_bus: 0xE129_F000,
            last_fetched: 0,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let pad32 = (bytes.len() + 3) & !3;
        let mut copy = vec![0u8; pad32];
        copy[..bytes.len()].copy_from_slice(bytes);
        let n = copy.len() as u32;
        self.rom_mask = if n > 0 && (n & (n - 1)) == 0 { n - 1 } else { 0 };
        self.rom = copy;
    }

    /// VRAM is 96 KB mirrored into a 128 KB window with the upper 32 KB
    /// mirrored from the previous 32 KB block.
    #[inline]
    fn vram_off(&self, addr: u32) -> usize {
        let mut off = (addr & 0x1FFFF) as usize;
        if off >= 0x18000 {
            off -= 0x8000;
        }
        off
    }

    /// ROM offset, mirrored to the cart's actual size.
    #[inline]
    fn rom_off(&self, addr: u32) -> usize {
        let off = addr & 0x01FF_FFFF;
        if self.rom_mask != 0 {
            return (off & self.rom_mask) as usize;
        }
        if self.rom.is_empty() {
            return off as usize;
        }
        (off as usize) % self.rom.len()
    }

    // ---- reads (memory regions + ROM only; IO/SRAM/EEPROM handled by Gba) ----
    pub fn read8(&self, addr: u32) -> u32 {
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_BIOS => {
                if (addr as usize) < R::BIOS_SIZE {
                    self.bios[addr as usize] as u32
                } else {
                    0
                }
            }
            R::REGION_EWRAM => a_rd8(&self.ewram, addr),
            R::REGION_IWRAM => a_rd8(&self.iwram, addr),
            R::REGION_PRAM => a_rd8(&self.pram, addr),
            R::REGION_VRAM => self.vram[self.vram_off(addr)] as u32,
            R::REGION_OAM => a_rd8(&self.oam, addr),
            R::REGION_ROM_0..=R::REGION_ROM_5 => {
                let off = self.rom_off(addr);
                if off < self.rom.len() {
                    self.rom[off] as u32
                } else {
                    (addr >> 1) & 0xFF
                }
            }
            _ => 0,
        }
    }

    pub fn read16(&self, addr: u32) -> u32 {
        let addr = addr & !1;
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_BIOS => {
                if (addr as usize) < R::BIOS_SIZE {
                    rd16(&self.bios[..], addr as usize)
                } else {
                    0
                }
            }
            R::REGION_EWRAM => a_rd16(&self.ewram, addr),
            R::REGION_IWRAM => a_rd16(&self.iwram, addr),
            R::REGION_PRAM => a_rd16(&self.pram, addr),
            R::REGION_VRAM => rd16(&self.vram[..], self.vram_off(addr)),
            R::REGION_OAM => a_rd16(&self.oam, addr),
            R::REGION_ROM_0..=R::REGION_ROM_5 => {
                let off = self.rom_off(addr);
                if off + 1 < self.rom.len() {
                    rd16(&self.rom, off)
                } else {
                    (addr >> 1) & 0xFFFF
                }
            }
            _ => 0,
        }
    }

    pub fn read32(&self, addr: u32) -> u32 {
        let addr = addr & !3;
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_BIOS => {
                if (addr as usize) < R::BIOS_SIZE {
                    rd32(&self.bios[..], addr as usize)
                } else {
                    self.bios_open_bus
                }
            }
            R::REGION_EWRAM => a_rd32(&self.ewram, addr),
            R::REGION_IWRAM => a_rd32(&self.iwram, addr),
            R::REGION_PRAM => a_rd32(&self.pram, addr),
            R::REGION_VRAM => rd32(&self.vram[..], self.vram_off(addr)),
            R::REGION_OAM => a_rd32(&self.oam, addr),
            R::REGION_ROM_0..=R::REGION_ROM_5 => {
                let off = self.rom_off(addr);
                if off + 3 < self.rom.len() {
                    rd32(&self.rom, off)
                } else {
                    addr
                }
            }
            _ => 0,
        }
    }

    // ---- writes (memory regions only) ----
    pub fn write8(&mut self, addr: u32, v: u32) {
        let v = (v & 0xFF) as u8;
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_EWRAM => a_wr8(&mut self.ewram, addr, v),
            R::REGION_IWRAM => a_wr8(&mut self.iwram, addr, v),
            R::REGION_PRAM => {
                // 8-bit writes to PRAM/VRAM/OAM broadcast to a halfword.
                let off = (addr as usize) & (R::PRAM_SIZE - 2);
                self.pram[off] = v;
                self.pram[off + 1] = v;
            }
            R::REGION_VRAM => {
                let off = self.vram_off(addr) & !1;
                // 8-bit writes to OBJ tiles (0x10000+) are ignored.
                if off >= 0x10000 {
                    return;
                }
                self.vram[off] = v;
                self.vram[off + 1] = v;
            }
            R::REGION_OAM => {} // OAM ignores byte writes
            _ => {}
        }
    }

    pub fn write16(&mut self, addr: u32, v: u32) {
        let addr = addr & !1;
        let v = v & 0xFFFF;
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_EWRAM => a_wr16(&mut self.ewram, addr, v),
            R::REGION_IWRAM => a_wr16(&mut self.iwram, addr, v),
            R::REGION_PRAM => a_wr16(&mut self.pram, addr, v),
            R::REGION_VRAM => {
                let off = self.vram_off(addr);
                wr16(&mut self.vram[..], off, v);
            }
            R::REGION_OAM => a_wr16(&mut self.oam, addr, v),
            _ => {}
        }
    }

    pub fn write32(&mut self, addr: u32, v: u32) {
        let addr = addr & !3;
        let region = (addr >> 24) & 0xF;
        match region {
            R::REGION_EWRAM => a_wr32(&mut self.ewram, addr, v),
            R::REGION_IWRAM => a_wr32(&mut self.iwram, addr, v),
            R::REGION_PRAM => a_wr32(&mut self.pram, addr, v),
            R::REGION_VRAM => {
                let off = self.vram_off(addr);
                wr32(&mut self.vram[..], off, v);
            }
            R::REGION_OAM => a_wr32(&mut self.oam, addr, v),
            _ => {}
        }
    }
}
