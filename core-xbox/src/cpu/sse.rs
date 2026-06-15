//! SSE / SSE2 XMM register file for the IA-32 interpreter.
//!
//! The Xbox CPU is a Pentium III "Coppermine" — it has the full SSE feature set
//! (eight 128-bit XMM registers + MXCSR) on top of the legacy x87/MMX units.
//! Real game code (Halo 2's vector math) leans on SSE *heavily*: packed-single
//! arithmetic (`MULPS`/`ADDPS`), the `XORPS xmm,xmm` zero idiom, scalar moves
//! (`MOVSS`/`MOVSD`), and the int<->float conversions. Hitting any of those used
//! to raise #UD and stall the boot; this module gives the interpreter somewhere
//! to land the data.
//!
//! # Where the state lives
//!
//! Exactly like the x87 unit in [`super::fpu`]: the architectural integer state
//! in [`super::state::Cpu`] is out of scope to modify, so the XMM register file
//! is kept in a **thread-local** [`XmmFile`] reached through [`with_xmm`]. A
//! single guest is single-threaded through the interpreter (one `Cpu::step` at a
//! time, no re-entrancy), so a `thread_local!` is sound. If the core ever ran
//! multiple guests on one OS thread this would need to move onto an owned field
//! — but that means editing `state.rs`, which is out of scope here.
//!
//! # The register model
//!
//! Each XMM register is stored as a raw `[u8; 16]` so that partial accesses
//! (scalar lane 0, the low/high 64-bit halves, MOVD's dword) and full 128-bit
//! bitwise logic all fall out trivially. Lanes are **little-endian**: lane 0 of
//! a 4×f32 register occupies bytes 0..4, i.e. the lowest-addressed bytes. All
//! the typed accessors honour that ordering.

use std::cell::RefCell;

/// The default MXCSR after reset: all exception flags clear, all masks set,
/// round-to-nearest. Value 0x1F80 is what a real CPU loads at init.
pub const MXCSR_DEFAULT: u32 = 0x1F80;

/// The eight 128-bit XMM registers plus the MXCSR control/status word.
///
/// Bytes are kept raw and little-endian; the typed accessors reinterpret a
/// register as 4×f32 / 2×f64 / 4×u32 / 2×u64 as needed. Scalar instructions
/// touch only lane 0 and leave the upper bytes untouched; packed instructions
/// touch every lane.
pub struct XmmFile {
    /// Eight registers, 128 bits each, stored as raw little-endian bytes.
    regs: [[u8; 16]; 8],
    /// SSE control/status register (rounding mode + exception masks/flags).
    pub mxcsr: u32,
}

impl Default for XmmFile {
    fn default() -> Self {
        Self::new()
    }
}

impl XmmFile {
    pub fn new() -> Self {
        XmmFile {
            regs: [[0u8; 16]; 8],
            mxcsr: MXCSR_DEFAULT,
        }
    }

    /// FXRSTOR/reset: zero every register and restore the default MXCSR.
    pub fn init(&mut self) {
        self.regs = [[0u8; 16]; 8];
        self.mxcsr = MXCSR_DEFAULT;
    }

    // ---- raw bytes ----
    #[inline]
    pub fn bytes(&self, r: usize) -> [u8; 16] {
        self.regs[r & 7]
    }
    #[inline]
    pub fn set_bytes(&mut self, r: usize, b: [u8; 16]) {
        self.regs[r & 7] = b;
    }

    // ---- typed views (little-endian lane order) ----
    #[inline]
    pub fn f32s(&self, r: usize) -> [f32; 4] {
        let b = self.regs[r & 7];
        let mut out = [0f32; 4];
        for (i, o) in out.iter_mut().enumerate() {
            let j = i * 4;
            *o = f32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]);
        }
        out
    }
    #[inline]
    pub fn set_f32s(&mut self, r: usize, v: [f32; 4]) {
        let reg = &mut self.regs[r & 7];
        for (i, x) in v.iter().enumerate() {
            reg[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
    }
    #[inline]
    pub fn f64s(&self, r: usize) -> [f64; 2] {
        let b = self.regs[r & 7];
        [
            f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            f64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
        ]
    }
    #[inline]
    pub fn set_f64s(&mut self, r: usize, v: [f64; 2]) {
        let reg = &mut self.regs[r & 7];
        reg[0..8].copy_from_slice(&v[0].to_le_bytes());
        reg[8..16].copy_from_slice(&v[1].to_le_bytes());
    }
    #[inline]
    pub fn u32s(&self, r: usize) -> [u32; 4] {
        let b = self.regs[r & 7];
        let mut out = [0u32; 4];
        for (i, o) in out.iter_mut().enumerate() {
            let j = i * 4;
            *o = u32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]);
        }
        out
    }
    #[inline]
    pub fn set_u32s(&mut self, r: usize, v: [u32; 4]) {
        let reg = &mut self.regs[r & 7];
        for (i, x) in v.iter().enumerate() {
            reg[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
    }
    #[inline]
    pub fn u64s(&self, r: usize) -> [u64; 2] {
        let b = self.regs[r & 7];
        [
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
        ]
    }
    #[inline]
    pub fn set_u64s(&mut self, r: usize, v: [u64; 2]) {
        let reg = &mut self.regs[r & 7];
        reg[0..8].copy_from_slice(&v[0].to_le_bytes());
        reg[8..16].copy_from_slice(&v[1].to_le_bytes());
    }

    // ---- scalar lane-0 helpers (upper lanes are left untouched) ----
    /// Lane 0 as f32 (bytes 0..4).
    #[inline]
    pub fn lane0_f32(&self, r: usize) -> f32 {
        let b = self.regs[r & 7];
        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
    /// Write lane 0 as f32, preserving bytes 4..16.
    #[inline]
    pub fn set_lane0_f32(&mut self, r: usize, v: f32) {
        self.regs[r & 7][0..4].copy_from_slice(&v.to_le_bytes());
    }
    /// Lane 0 as f64 (bytes 0..8).
    #[inline]
    pub fn lane0_f64(&self, r: usize) -> f64 {
        let b = self.regs[r & 7];
        f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }
    /// Write lane 0 as f64, preserving bytes 8..16.
    #[inline]
    pub fn set_lane0_f64(&mut self, r: usize, v: f64) {
        self.regs[r & 7][0..8].copy_from_slice(&v.to_le_bytes());
    }
    /// Low dword (bytes 0..4) — MOVD destination/source.
    #[inline]
    pub fn dword0(&self, r: usize) -> u32 {
        let b = self.regs[r & 7];
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
    /// Write the low dword (bytes 0..4), preserving bytes 4..16.
    #[inline]
    pub fn set_dword0(&mut self, r: usize, v: u32) {
        self.regs[r & 7][0..4].copy_from_slice(&v.to_le_bytes());
    }
    /// Low qword (bytes 0..8) — MOVQ / MOVLPS half.
    #[inline]
    pub fn qword_lo(&self, r: usize) -> u64 {
        self.u64s(r)[0]
    }
    /// High qword (bytes 8..16) — MOVHPS half.
    #[inline]
    pub fn qword_hi(&self, r: usize) -> u64 {
        self.u64s(r)[1]
    }
    /// Write the low qword (bytes 0..8), preserving the high qword.
    #[inline]
    pub fn set_qword_lo(&mut self, r: usize, v: u64) {
        self.regs[r & 7][0..8].copy_from_slice(&v.to_le_bytes());
    }
    /// Write the high qword (bytes 8..16), preserving the low qword.
    #[inline]
    pub fn set_qword_hi(&mut self, r: usize, v: u64) {
        self.regs[r & 7][8..16].copy_from_slice(&v.to_le_bytes());
    }
}

thread_local! {
    /// The shared SSE unit for the interpreter on this thread. See the module
    /// docs for why this is a thread-local rather than a `Cpu` field.
    static XMM: RefCell<XmmFile> = RefCell::new(XmmFile::new());
}

/// Run `f` with mutable access to the thread-local XMM register file.
pub fn with_xmm<R>(f: impl FnOnce(&mut XmmFile) -> R) -> R {
    XMM.with(|c| f(&mut c.borrow_mut()))
}

/// Reset the thread-local XMM file (tests use this to isolate state).
pub fn reset_xmm() {
    XMM.with(|c| c.borrow_mut().init());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_lane_roundtrip_is_little_endian() {
        let mut x = XmmFile::new();
        x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(x.f32s(0), [1.0, 2.0, 3.0, 4.0]);
        // lane 0 lives in the lowest bytes
        assert_eq!(x.lane0_f32(0), 1.0);
        let b = x.bytes(0);
        assert_eq!(&b[0..4], &1.0f32.to_le_bytes());
    }

    #[test]
    fn scalar_write_preserves_upper_lanes() {
        let mut x = XmmFile::new();
        x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]);
        x.set_lane0_f32(0, 9.0);
        assert_eq!(x.f32s(0), [9.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn f64_and_qword_halves() {
        let mut x = XmmFile::new();
        x.set_f64s(0, [1.5, 2.5]);
        assert_eq!(x.f64s(0), [1.5, 2.5]);
        assert_eq!(x.qword_lo(0), 1.5f64.to_bits());
        assert_eq!(x.qword_hi(0), 2.5f64.to_bits());
        x.set_qword_hi(0, 7);
        assert_eq!(x.qword_lo(0), 1.5f64.to_bits(), "low half preserved");
        assert_eq!(x.qword_hi(0), 7);
    }

    #[test]
    fn dword0_matches_movd_view() {
        let mut x = XmmFile::new();
        x.set_u32s(0, [0xDEAD_BEEF, 0, 0, 0]);
        assert_eq!(x.dword0(0), 0xDEAD_BEEF);
    }
}
