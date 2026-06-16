//! COP1 (FPU) for the VR4300 — register file + control register + the basic
//! arithmetic the foundation needs.
//!
//! Scope (PARTIAL — see `lib.rs` matrix): the 32 floating-point registers
//! (accessible as 32 single-precision or, in 64-bit FR mode, 32 double
//! registers), the FCR31 control/status register, the load/store/move
//! instructions (LWC1/SWC1/LDC1/SDC1/MTC1/MFC1/DMTC1/DMFC1/CTC1/CFC1), and the
//! common single/double arithmetic (ADD/SUB/MUL/DIV/MOV/NEG/ABS/SQRT) plus
//! CVT/compare. Rounding-mode and the full IEEE exception-flag plumbing are
//! simplified: we compute with the host f32/f64 and round-to-nearest. This is
//! enough for game boot code (which configures FCR31 and does a few moves) but
//! is NOT a cycle/precision-accurate FPU.

/// COP1 floating-point unit register file.
pub struct Cop1 {
    /// 32 FP registers, stored as raw 64-bit bit patterns. Single-precision
    /// ops read/write the low 32 bits; double-precision ops use the full 64.
    pub fpr: [u64; 32],
    /// FCR0: implementation/revision (read-only constant).
    pub fcr0: u32,
    /// FCR31: control/status (rounding mode, condition bit, exception flags).
    pub fcr31: u32,
}

/// FCR31 condition bit (set by C.cond.fmt, tested by BC1T/BC1F).
pub const FCR31_C: u32 = 1 << 23;

impl Default for Cop1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cop1 {
    pub fn new() -> Self {
        Cop1 {
            fpr: [0; 32],
            // Revision register for the VR4300's FPU.
            fcr0: 0x0000_0B00,
            fcr31: 0,
        }
    }

    // ---- single precision (low 32 bits of the register) ----
    #[inline]
    pub fn read_s(&self, i: usize) -> f32 {
        f32::from_bits(self.fpr[i] as u32)
    }
    #[inline]
    pub fn write_s(&mut self, i: usize, v: f32) {
        self.fpr[i] = (self.fpr[i] & 0xFFFF_FFFF_0000_0000) | v.to_bits() as u64;
    }

    // ---- double precision (full 64 bits) ----
    #[inline]
    pub fn read_d(&self, i: usize) -> f64 {
        f64::from_bits(self.fpr[i])
    }
    #[inline]
    pub fn write_d(&mut self, i: usize, v: f64) {
        self.fpr[i] = v.to_bits();
    }

    // ---- raw word access (MTC1/MFC1 move the integer bit pattern) ----
    #[inline]
    pub fn read_w(&self, i: usize) -> u32 {
        self.fpr[i] as u32
    }
    #[inline]
    pub fn write_w(&mut self, i: usize, v: u32) {
        self.fpr[i] = (self.fpr[i] & 0xFFFF_FFFF_0000_0000) | v as u64;
    }
    #[inline]
    pub fn read_dw(&self, i: usize) -> u64 {
        self.fpr[i]
    }
    #[inline]
    pub fn write_dw(&mut self, i: usize, v: u64) {
        self.fpr[i] = v;
    }

    /// Read FCR0 / FCR31 (CFC1).
    pub fn read_ctrl(&self, reg: u32) -> u32 {
        match reg {
            0 => self.fcr0,
            31 => self.fcr31,
            _ => 0,
        }
    }

    /// Write FCR31 (CTC1); FCR0 is read-only.
    pub fn write_ctrl(&mut self, reg: u32, v: u32) {
        if reg == 31 {
            self.fcr31 = v;
        }
    }

    /// Set/clear the FCR31 condition bit (result of a compare).
    #[inline]
    pub fn set_condition(&mut self, set: bool) {
        if set {
            self.fcr31 |= FCR31_C;
        } else {
            self.fcr31 &= !FCR31_C;
        }
    }

    /// Read the FCR31 condition bit (for BC1T/BC1F).
    #[inline]
    pub fn condition(&self) -> bool {
        self.fcr31 & FCR31_C != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_double_roundtrip() {
        let mut f = Cop1::new();
        f.write_s(1, 3.5);
        assert_eq!(f.read_s(1), 3.5);
        f.write_d(2, 1.25e9);
        assert_eq!(f.read_d(2), 1.25e9);
    }

    #[test]
    fn mtc1_writes_low_word_only() {
        let mut f = Cop1::new();
        f.write_dw(3, 0xAAAA_BBBB_CCCC_DDDD);
        f.write_w(3, 0x1234_5678);
        assert_eq!(f.read_dw(3), 0xAAAA_BBBB_1234_5678);
    }

    #[test]
    fn condition_bit_roundtrips() {
        let mut f = Cop1::new();
        assert!(!f.condition());
        f.set_condition(true);
        assert!(f.condition());
        assert_eq!(f.read_ctrl(31) & FCR31_C, FCR31_C);
    }
}
