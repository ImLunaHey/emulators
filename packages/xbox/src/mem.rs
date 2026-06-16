//! Raw memory regions for the Xbox: 64 MB unified DDR RAM and a slot for the
//! 256 KB flash BIOS. Mirrors the PS1/GC cores' `Mem`: this struct owns the
//! "dumb" backing storage only — it has no knowledge of the I/O devices or of
//! virtual-address translation (the [`crate::bus::Bus`] does that).
//!
//! **LITTLE-ENDIAN.** The Pentium III is x86: a 32-bit word stored at offset `n`
//! keeps its *least*-significant byte at the lowest address (`b[n]`). Every
//! multi-byte accessor below is little-endian, in deliberate contrast to the
//! big-endian GameCube core. Getting this wrong silently corrupts every word the
//! CPU reads, so the byte order is asserted by unit tests at the bottom.

use crate::regions as R;

/// Heap-allocate a zeroed fixed-size region without ever placing `N` bytes on
/// the stack (`Box::new([0; N])` would, and RAM is 64 MB).
#[inline]
fn boxed_region<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

// ---- little-endian slice helpers (least-significant byte at the lowest addr) --
#[inline]
fn rd16_le(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8)
}
#[inline]
fn rd32_le(b: &[u8], off: usize) -> u32 {
    (b[off] as u32)
        | ((b[off + 1] as u32) << 8)
        | ((b[off + 2] as u32) << 16)
        | ((b[off + 3] as u32) << 24)
}
#[inline]
fn wr16_le(b: &mut [u8], off: usize, v: u32) {
    b[off] = (v & 0xFF) as u8;
    b[off + 1] = ((v >> 8) & 0xFF) as u8;
}
#[inline]
fn wr32_le(b: &mut [u8], off: usize, v: u32) {
    b[off] = (v & 0xFF) as u8;
    b[off + 1] = ((v >> 8) & 0xFF) as u8;
    b[off + 2] = ((v >> 16) & 0xFF) as u8;
    b[off + 3] = ((v >> 24) & 0xFF) as u8;
}

/// The raw Xbox memory regions. Owns no I/O devices (see module docs).
pub struct Mem {
    /// 64 MB unified DDR at physical `0x0000_0000`. Range-checked so an
    /// out-of-range offset is dropped (writes) / returns 0 (reads).
    pub ram: Box<[u8; R::RAM_SIZE]>,
    /// 256 KB flash BIOS, mirrored across the top 16 MB. Empty (all-zero) until
    /// [`Mem::load_bios`] is called; reads of an unloaded BIOS return 0.
    /// Power-of-two sized, so its window can be masked.
    pub flash: Box<[u8; R::FLASH_SIZE]>,
    /// True once a flash/BIOS image has been loaded.
    pub flash_loaded: bool,
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
            flash: boxed_region(),
            flash_loaded: false,
        }
    }

    /// Load a flash/BIOS image. A retail image is 256 KB; 1 MB images (4× the
    /// 256 KB block) are accepted by taking the *last* 256 KB (the reset vector
    /// and 2BL live at the top). Bytes beyond the image stay zero. A real Xbox
    /// cannot boot without it.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(R::FLASH_SIZE);
        // Take the tail so over-sized (mirrored) dumps still align the reset
        // vector at the very top of the 256 KB block.
        let src = &bytes[bytes.len() - n..];
        self.flash[..n].copy_from_slice(src);
        self.flash_loaded = true;
    }

    // ---- main RAM (offset is a physical RAM offset; range-checked) ----
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
        let i = off as usize;
        if i + 1 < R::RAM_SIZE {
            rd16_le(&self.ram[..], i)
        } else {
            0
        }
    }
    #[inline]
    pub fn ram_read32(&self, off: u32) -> u32 {
        let i = off as usize;
        if i + 3 < R::RAM_SIZE {
            rd32_le(&self.ram[..], i)
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
        let i = off as usize;
        if i + 1 < R::RAM_SIZE {
            wr16_le(&mut self.ram[..], i, v);
        }
    }
    #[inline]
    pub fn ram_write32(&mut self, off: u32, v: u32) {
        let i = off as usize;
        if i + 3 < R::RAM_SIZE {
            wr32_le(&mut self.ram[..], i, v);
        }
    }

    // ---- flash BIOS (read-only, power-of-two-masked to the 256 KB image) ----
    #[inline]
    pub fn flash_read8(&self, off: u32) -> u32 {
        self.flash[(off as usize) & (R::FLASH_SIZE - 1)] as u32
    }
    #[inline]
    pub fn flash_read16(&self, off: u32) -> u32 {
        // x86 allows misaligned access; assemble byte-wise across the mirror so a
        // read that straddles the 256 KB wrap still returns sane bytes.
        let lo = self.flash_read8(off);
        let hi = self.flash_read8(off.wrapping_add(1));
        lo | (hi << 8)
    }
    #[inline]
    pub fn flash_read32(&self, off: u32) -> u32 {
        let b0 = self.flash_read8(off);
        let b1 = self.flash_read8(off.wrapping_add(1));
        let b2 = self.flash_read8(off.wrapping_add(2));
        let b3 = self.flash_read8(off.wrapping_add(3));
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_is_little_endian() {
        let mut m = Mem::new();
        m.ram_write32(0x100, 0x1122_3344);
        // Least-significant byte at the lowest address.
        assert_eq!(m.ram[0x100], 0x44);
        assert_eq!(m.ram[0x101], 0x33);
        assert_eq!(m.ram[0x102], 0x22);
        assert_eq!(m.ram[0x103], 0x11);
        assert_eq!(m.ram_read32(0x100), 0x1122_3344);
    }

    #[test]
    fn ram_16_and_8_little_endian() {
        let mut m = Mem::new();
        m.ram_write16(0x200, 0xABCD);
        assert_eq!(m.ram[0x200], 0xCD);
        assert_eq!(m.ram[0x201], 0xAB);
        assert_eq!(m.ram_read16(0x200), 0xABCD);
        m.ram_write8(0x202, 0xFF);
        assert_eq!(m.ram_read8(0x202), 0xFF);
    }

    #[test]
    fn out_of_range_ram_is_safe() {
        let mut m = Mem::new();
        // Far beyond 64 MB: writes drop, reads return 0 (no panic).
        m.ram_write32(0x1000_0000, 0xDEAD_BEEF);
        assert_eq!(m.ram_read32(0x1000_0000), 0);
    }

    #[test]
    fn flash_round_trips_little_endian_and_takes_tail() {
        let mut m = Mem::new();
        // An over-sized (1 MB) dump: only the last 256 KB is kept.
        let mut img = vec![0u8; R::FLASH_SIZE * 4];
        let n = img.len();
        img[n - 4] = 0xEF;
        img[n - 3] = 0xBE;
        img[n - 2] = 0xAD;
        img[n - 1] = 0xDE;
        m.load_bios(&img);
        assert!(m.flash_loaded);
        // The last word of the 256 KB block reads back little-endian.
        assert_eq!(m.flash_read32((R::FLASH_SIZE - 4) as u32), 0xDEAD_BEEF);
    }

    #[test]
    fn flash_mirrors_across_window() {
        let mut m = Mem::new();
        let mut img = vec![0u8; R::FLASH_SIZE];
        img[0] = 0x55;
        m.load_bios(&img);
        // Offset 0 and offset FLASH_SIZE alias the same byte (mirror).
        assert_eq!(m.flash_read8(0), 0x55);
        assert_eq!(m.flash_read8(R::FLASH_SIZE as u32), 0x55);
    }
}
