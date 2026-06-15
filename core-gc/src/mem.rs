//! Raw memory regions for the GameCube: 24 MB main RAM and a slot for the 2 MB
//! IPL boot ROM. Mirrors the PS1 core's `Mem`: this struct owns the "dumb"
//! backing storage only — it has no knowledge of the I/O devices or of
//! virtual-address translation (the [`crate::bus::Bus`] does that).
//!
//! **BIG-ENDIAN.** The Gekko is a big-endian PowerPC: a 32-bit word stored at
//! offset `n` keeps its most-significant byte at the lowest address (`b[n]`).
//! Every multi-byte accessor below is big-endian, in deliberate contrast to the
//! little-endian PS1/GBA cores. Getting this wrong silently corrupts every word
//! the CPU reads, so the byte order is asserted by unit tests at the bottom.

use crate::regions as R;

/// Heap-allocate a zeroed fixed-size region without ever placing `N` bytes on
/// the stack (`Box::new([0; N])` would, and RAM is 24 MB).
#[inline]
fn boxed_region<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

// ---- big-endian slice helpers (most-significant byte at the lowest address) --
#[inline]
fn rd16_be(b: &[u8], off: usize) -> u32 {
    ((b[off] as u32) << 8) | (b[off + 1] as u32)
}
#[inline]
fn rd32_be(b: &[u8], off: usize) -> u32 {
    ((b[off] as u32) << 24)
        | ((b[off + 1] as u32) << 16)
        | ((b[off + 2] as u32) << 8)
        | (b[off + 3] as u32)
}
#[inline]
fn rd64_be(b: &[u8], off: usize) -> u64 {
    ((rd32_be(b, off) as u64) << 32) | (rd32_be(b, off + 4) as u64)
}
#[inline]
fn wr16_be(b: &mut [u8], off: usize, v: u32) {
    b[off] = ((v >> 8) & 0xFF) as u8;
    b[off + 1] = (v & 0xFF) as u8;
}
#[inline]
fn wr32_be(b: &mut [u8], off: usize, v: u32) {
    b[off] = ((v >> 24) & 0xFF) as u8;
    b[off + 1] = ((v >> 16) & 0xFF) as u8;
    b[off + 2] = ((v >> 8) & 0xFF) as u8;
    b[off + 3] = (v & 0xFF) as u8;
}
#[inline]
fn wr64_be(b: &mut [u8], off: usize, v: u64) {
    wr32_be(b, off, (v >> 32) as u32);
    wr32_be(b, off + 4, v as u32);
}

/// The raw GameCube memory regions. Owns no I/O devices (see module docs).
pub struct Mem {
    /// 24 MB main 1T-SRAM. Reached cached via `0x8000_0000` and uncached via
    /// `0xC000_0000`; the bus folds both to the physical offset. Range-checked
    /// (not power-of-two), so out-of-range offsets are dropped/return 0.
    pub ram: Box<[u8; R::RAM_SIZE]>,
    /// 2 MB IPL boot ROM @ physical 0x0F00_0000 (virtual 0xFFF0_0000). Empty
    /// (all-zero) until [`Mem::load_ipl`] is called; reads of an unloaded IPL
    /// return 0. Power-of-two sized, so its window can be masked.
    pub ipl: Box<[u8; R::IPL_SIZE]>,
    /// True once an IPL image has been loaded.
    pub ipl_loaded: bool,
}

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    pub fn new() -> Self {
        Mem {
            ram: boxed_region(),
            ipl: boxed_region(),
            ipl_loaded: false,
        }
    }

    /// Load an IPL boot-ROM image (must be ≤ 2 MB). Bytes beyond the image stay
    /// zero. A real GameCube cannot boot a game without it (it holds BS1/BS2).
    pub fn load_ipl(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(R::IPL_SIZE);
        self.ipl[..n].copy_from_slice(&bytes[..n]);
        self.ipl_loaded = true;
    }

    // ---- main RAM (offset is a physical RAM offset, already in 0..RAM_SIZE
    //      range when classified; we still guard since RAM isn't a power of 2) --
    #[inline]
    pub fn ram_read8(&self, off: u32) -> u32 {
        let i = off as usize;
        if i < R::RAM_SIZE {
            self.ram[i] as u32
        } else {
            0
        }
    }
    #[inline]
    pub fn ram_read16(&self, off: u32) -> u32 {
        let i = (off as usize) & !1;
        if i + 1 < R::RAM_SIZE {
            rd16_be(&self.ram[..], i)
        } else {
            0
        }
    }
    #[inline]
    pub fn ram_read32(&self, off: u32) -> u32 {
        let i = (off as usize) & !3;
        if i + 3 < R::RAM_SIZE {
            rd32_be(&self.ram[..], i)
        } else {
            0
        }
    }
    #[inline]
    pub fn ram_read64(&self, off: u32) -> u64 {
        let i = (off as usize) & !7;
        if i + 7 < R::RAM_SIZE {
            rd64_be(&self.ram[..], i)
        } else {
            0
        }
    }
    #[inline]
    pub fn ram_write8(&mut self, off: u32, v: u32) {
        let i = off as usize;
        if i < R::RAM_SIZE {
            self.ram[i] = (v & 0xFF) as u8;
        }
    }
    #[inline]
    pub fn ram_write16(&mut self, off: u32, v: u32) {
        let i = (off as usize) & !1;
        if i + 1 < R::RAM_SIZE {
            wr16_be(&mut self.ram[..], i, v);
        }
    }
    #[inline]
    pub fn ram_write32(&mut self, off: u32, v: u32) {
        let i = (off as usize) & !3;
        if i + 3 < R::RAM_SIZE {
            wr32_be(&mut self.ram[..], i, v);
        }
    }
    #[inline]
    pub fn ram_write64(&mut self, off: u32, v: u64) {
        let i = (off as usize) & !7;
        if i + 7 < R::RAM_SIZE {
            wr64_be(&mut self.ram[..], i, v);
        }
    }

    // ---- IPL ROM (read-only, power-of-two-masked) ----
    #[inline]
    pub fn ipl_read8(&self, off: u32) -> u32 {
        self.ipl[(off as usize) & (R::IPL_SIZE - 1)] as u32
    }
    #[inline]
    pub fn ipl_read16(&self, off: u32) -> u32 {
        let i = (off as usize) & (R::IPL_SIZE - 1) & !1;
        rd16_be(&self.ipl[..], i)
    }
    #[inline]
    pub fn ipl_read32(&self, off: u32) -> u32 {
        let i = (off as usize) & (R::IPL_SIZE - 1) & !3;
        rd32_be(&self.ipl[..], i)
    }
    #[inline]
    pub fn ipl_read64(&self, off: u32) -> u64 {
        let i = (off as usize) & (R::IPL_SIZE - 1) & !7;
        rd64_be(&self.ipl[..], i)
    }

    // ---- generic big-endian slice helpers, exported for the bus/IO seams ----
    #[inline]
    pub fn rd32_be(b: &[u8], off: usize) -> u32 {
        rd32_be(b, off)
    }
    #[inline]
    pub fn wr32_be(b: &mut [u8], off: usize, v: u32) {
        wr32_be(b, off, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_is_big_endian() {
        let mut m = Mem::new();
        m.ram_write32(0x100, 0x1122_3344);
        // Most-significant byte at the lowest address.
        assert_eq!(m.ram[0x100], 0x11);
        assert_eq!(m.ram[0x101], 0x22);
        assert_eq!(m.ram[0x102], 0x33);
        assert_eq!(m.ram[0x103], 0x44);
        assert_eq!(m.ram_read32(0x100), 0x1122_3344);
    }

    #[test]
    fn ram_16_and_8_big_endian() {
        let mut m = Mem::new();
        m.ram_write16(0x200, 0xABCD);
        assert_eq!(m.ram[0x200], 0xAB);
        assert_eq!(m.ram[0x201], 0xCD);
        assert_eq!(m.ram_read16(0x200), 0xABCD);
        m.ram_write8(0x202, 0xFF);
        assert_eq!(m.ram_read8(0x202), 0xFF);
    }

    #[test]
    fn ram_64_round_trips_big_endian() {
        let mut m = Mem::new();
        m.ram_write64(0x300, 0x0011_2233_4455_6677);
        assert_eq!(m.ram[0x300], 0x00);
        assert_eq!(m.ram[0x307], 0x77);
        assert_eq!(m.ram_read64(0x300), 0x0011_2233_4455_6677);
    }

    #[test]
    fn out_of_range_ram_is_safe() {
        let mut m = Mem::new();
        // Far beyond 24 MB: writes drop, reads return 0 (no panic).
        m.ram_write32(0x0200_0000, 0xDEAD_BEEF);
        assert_eq!(m.ram_read32(0x0200_0000), 0);
    }

    #[test]
    fn ipl_round_trips_big_endian() {
        let mut m = Mem::new();
        let mut img = vec![0u8; 16];
        img[0] = 0xDE;
        img[1] = 0xAD;
        img[2] = 0xBE;
        img[3] = 0xEF;
        m.load_ipl(&img);
        assert!(m.ipl_loaded);
        assert_eq!(m.ipl_read32(0), 0xDEAD_BEEF);
    }
}
