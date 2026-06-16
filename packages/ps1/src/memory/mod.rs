//! Raw memory regions for the PSX: 2 MB main RAM, 1 KB scratchpad, and a slot
//! for the 512 KB BIOS ROM. Mirrors the GBA core's `Mem`: this struct owns the
//! "dumb" backing storage only — it has no knowledge of the I/O devices or of
//! virtual-address translation (the [`crate::bus::Bus`] does that). All access
//! is little-endian.

use crate::regions as R;

/// Heap-allocate a zeroed fixed-size region without ever placing `N` bytes on
/// the stack (`Box::new([0; N])` would, and RAM is 2 MB).
#[inline]
fn boxed_region<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

// ---- little-endian slice helpers ----
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

// ---- const-generic, power-of-2-masked accessors (drop the bounds check) ----
// All three backing regions (RAM 2 MB, scratchpad 1 KB, BIOS 512 KB) are
// power-of-two sized, so masking against `N - 1` at the index site lets LLVM
// prove the access in-bounds. The extra `& !1` / `& !3` re-assert alignment so
// `i + 1` / `i + 3` are also provably in range.
#[inline]
fn a_rd8<const N: usize>(a: &[u8; N], off: u32) -> u32 {
    a[(off as usize) & (N - 1)] as u32
}
#[inline]
fn a_rd16<const N: usize>(a: &[u8; N], off: u32) -> u32 {
    let i = (off as usize) & (N - 1) & !1;
    (a[i] as u32) | ((a[i + 1] as u32) << 8)
}
#[inline]
fn a_rd32<const N: usize>(a: &[u8; N], off: u32) -> u32 {
    let i = (off as usize) & (N - 1) & !3;
    (a[i] as u32)
        | ((a[i + 1] as u32) << 8)
        | ((a[i + 2] as u32) << 16)
        | ((a[i + 3] as u32) << 24)
}
#[inline]
fn a_wr8<const N: usize>(a: &mut [u8; N], off: u32, v: u32) {
    a[(off as usize) & (N - 1)] = (v & 0xFF) as u8;
}
#[inline]
fn a_wr16<const N: usize>(a: &mut [u8; N], off: u32, v: u32) {
    let i = (off as usize) & (N - 1) & !1;
    a[i] = (v & 0xFF) as u8;
    a[i + 1] = ((v >> 8) & 0xFF) as u8;
}
#[inline]
fn a_wr32<const N: usize>(a: &mut [u8; N], off: u32, v: u32) {
    let i = (off as usize) & (N - 1) & !3;
    a[i] = (v & 0xFF) as u8;
    a[i + 1] = ((v >> 8) & 0xFF) as u8;
    a[i + 2] = ((v >> 16) & 0xFF) as u8;
    a[i + 3] = ((v >> 24) & 0xFF) as u8;
}

/// The raw PSX memory regions. Owns no I/O devices (see module docs).
pub struct Mem {
    /// 2 MB main RAM. Mirrored four times across the first 8 MB of the physical
    /// space; the bus folds the mirror with [`R::RAM_MASK`].
    pub ram: Box<[u8; R::RAM_SIZE]>,
    /// 1 KB scratchpad (the data cache repurposed as fast RAM) @ 0x1F80_0000.
    pub scratchpad: Box<[u8; R::SCRATCHPAD_SIZE]>,
    /// 512 KB BIOS ROM @ 0x1FC0_0000. Empty (all-zero) until [`Mem::load_bios`]
    /// is called; reads of an unloaded BIOS return 0.
    pub bios: Box<[u8; R::BIOS_SIZE]>,
    /// True once a BIOS image has been loaded.
    pub bios_loaded: bool,
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
            scratchpad: boxed_region(),
            bios: boxed_region(),
            bios_loaded: false,
        }
    }

    /// Load a BIOS image (must be ≤ 512 KB). Bytes beyond the image stay zero.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(R::BIOS_SIZE);
        self.bios[..n].copy_from_slice(&bytes[..n]);
        self.bios_loaded = true;
    }

    // ---- main RAM (offset already folded by the caller) ----
    #[inline]
    pub fn ram_read8(&self, off: u32) -> u32 {
        a_rd8(&self.ram, off)
    }
    #[inline]
    pub fn ram_read16(&self, off: u32) -> u32 {
        a_rd16(&self.ram, off)
    }
    #[inline]
    pub fn ram_read32(&self, off: u32) -> u32 {
        a_rd32(&self.ram, off)
    }
    #[inline]
    pub fn ram_write8(&mut self, off: u32, v: u32) {
        a_wr8(&mut self.ram, off, v)
    }
    #[inline]
    pub fn ram_write16(&mut self, off: u32, v: u32) {
        a_wr16(&mut self.ram, off, v)
    }
    #[inline]
    pub fn ram_write32(&mut self, off: u32, v: u32) {
        a_wr32(&mut self.ram, off, v)
    }

    // ---- scratchpad ----
    #[inline]
    pub fn scratch_read8(&self, off: u32) -> u32 {
        a_rd8(&self.scratchpad, off)
    }
    #[inline]
    pub fn scratch_read16(&self, off: u32) -> u32 {
        a_rd16(&self.scratchpad, off)
    }
    #[inline]
    pub fn scratch_read32(&self, off: u32) -> u32 {
        a_rd32(&self.scratchpad, off)
    }
    #[inline]
    pub fn scratch_write8(&mut self, off: u32, v: u32) {
        a_wr8(&mut self.scratchpad, off, v)
    }
    #[inline]
    pub fn scratch_write16(&mut self, off: u32, v: u32) {
        a_wr16(&mut self.scratchpad, off, v)
    }
    #[inline]
    pub fn scratch_write32(&mut self, off: u32, v: u32) {
        a_wr32(&mut self.scratchpad, off, v)
    }

    // ---- BIOS ROM (read-only) ----
    #[inline]
    pub fn bios_read8(&self, off: u32) -> u32 {
        a_rd8(&self.bios, off)
    }
    #[inline]
    pub fn bios_read16(&self, off: u32) -> u32 {
        a_rd16(&self.bios, off)
    }
    #[inline]
    pub fn bios_read32(&self, off: u32) -> u32 {
        a_rd32(&self.bios, off)
    }

    // ---- generic slice helpers, exported for the bus/IO seams ----
    #[inline]
    pub fn rd16(b: &[u8], off: usize) -> u32 {
        rd16(b, off)
    }
    #[inline]
    pub fn rd32(b: &[u8], off: usize) -> u32 {
        rd32(b, off)
    }
    #[inline]
    pub fn wr16(b: &mut [u8], off: usize, v: u32) {
        wr16(b, off, v)
    }
    #[inline]
    pub fn wr32(b: &mut [u8], off: usize, v: u32) {
        wr32(b, off, v)
    }
}
