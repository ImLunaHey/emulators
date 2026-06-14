//! NDS math accelerators (ARM9-only): a non-blocking divider (0x04000280..
//! 0x040002AF) and a non-blocking square-root unit (0x040002B0..0x040002BF).
//! `Nds` owns one `DsMath` on the ARM9 side; the ARM7 IO map has none. Ported
//! from ../../ds-recomp/src/io/ds_math.ts.
//!
//! A computation is triggered by writing any operand or the control word and
//! finishes a few cycles later on real HW; we compute synchronously and report
//! busy = 0. The TS used JS `BigInt`; the Rust port uses `i64` / `u64` (and
//! `i128` for the 64/64 division remainder where intermediate overflow is
//! possible). Self-contained — no external deps.
//!
//! Register addresses are matched on the low 28 bits (`addr & 0x0FFFFFFF`) by
//! the IO dispatch; the methods here take that masked address.

pub const DIVCNT: u32 = 0x0400_0280;
pub const DIV_NUMER: u32 = 0x0400_0290; // 64-bit
pub const DIV_DENOM: u32 = 0x0400_0298; // 64-bit
pub const DIV_RESULT: u32 = 0x0400_02A0; // 64-bit quotient
pub const DIVREM_RESULT: u32 = 0x0400_02A8; // 64-bit remainder
pub const SQRTCNT: u32 = 0x0400_02B0;
pub const SQRT_RESULT: u32 = 0x0400_02B4; // 32-bit
pub const SQRT_PARAM: u32 = 0x0400_02B8; // 64-bit

#[derive(Default)]
pub struct DsMath {
    /// Storage as little-endian byte arrays so byte/half/word access is
    /// uniform (matches how games poke these registers).
    pub divcnt: u32,
    pub numer: [u8; 8],
    pub denom: [u8; 8],
    pub result: [u8; 8],
    pub remain: [u8; 8],

    pub sqrtcnt: u32,
    pub sqrt_res: [u8; 4],
    pub sqrt_param: [u8; 8],
}

#[inline]
fn u32_le(b: &[u8; 8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
fn u64_le(b: &[u8; 8]) -> u64 {
    u64::from_le_bytes(*b)
}

#[inline]
fn write64_le(b: &mut [u8; 8], v: u64) {
    *b = v.to_le_bytes();
}

impl DsMath {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute the quotient + remainder from DIVCNT mode (0 = 32/32, 1/3 =
    /// 64/32, 2 = 64/64). Updates the DIVCNT div-by-zero error bit (14) from
    /// the FULL 64-bit denom, and reproduces the 32/32 div-by-zero quirk
    /// (high half = sign-extension of the numerator).
    fn recompute_div(&mut self) {
        let mode = self.divcnt & 0x3;

        // Sign-extend operands to i128 according to the selected widths so the
        // truncating division below never overflows (i64::MIN / -1 etc).
        let (n, d): (i128, i128) = match mode {
            0 => (
                (u32_le(&self.numer, 0) as i32) as i128,
                (u32_le(&self.denom, 0) as i32) as i128,
            ),
            // mode 3 is reserved; on real HW behaves like mode 1 (64/32).
            1 | 3 => (
                (u64_le(&self.numer) as i64) as i128,
                (u32_le(&self.denom, 0) as i32) as i128,
            ),
            _ => (
                (u64_le(&self.numer) as i64) as i128,
                (u64_le(&self.denom) as i64) as i128,
            ),
        };

        // Error bit (DIVCNT bit 14) checks the FULL 64-bit DENOM register,
        // regardless of mode. 32/32 mode with denom_lo=0 but denom_hi!=0
        // produces div-by-zero *result* behavior WITHOUT setting the error bit
        // (because the full 64-bit denom is non-zero).
        let full_denom = u64_le(&self.denom);
        if full_denom == 0 {
            self.divcnt = (self.divcnt | 0x4000) & 0xFFFF;
        } else {
            self.divcnt = (self.divcnt & !0x4000) & 0xFFFF;
        }

        // Division-by-zero *result* behavior keys off the mode-selected denom.
        let div_by_zero = d == 0;
        let (q, r): (i128, i128) = if div_by_zero {
            (if n < 0 { 1 } else { -1 }, n)
        } else {
            // Rust integer division truncates toward zero, matching the HW /
            // BigInt semantics.
            let q = n / d;
            (q, n - q * d)
        };

        if mode == 0 && div_by_zero {
            // In 32/32 mode div-by-0, real HW writes a buggy high half for the
            // result: the high half is sign-extension of the *numerator*, not
            // of the low quotient. The remainder follows normal sign-extension
            // (= the numerator anyway).
            let num_high: u64 = if n < 0 { 0xFFFF_FFFF } else { 0 };
            let q_lo = (q as i32 as u32) as u64;
            write64_le(&mut self.result, (num_high << 32) | q_lo);
            write64_le(&mut self.remain, r as i64 as u64);
        } else {
            write64_le(&mut self.result, q as i64 as u64);
            write64_le(&mut self.remain, r as i64 as u64);
        }
    }

    /// Recompute the integer square root of the 32- or 64-bit param.
    fn recompute_sqrt(&mut self) {
        let is64 = (self.sqrtcnt & 1) != 0;
        let v: u64 = if is64 {
            u64_le(&self.sqrt_param)
        } else {
            u32_le(&self.sqrt_param, 0) as u64
        };
        // Integer sqrt of a u64; the true result fits in u32 (sqrt(2^64-1) <
        // 2^32), so no clamp is actually needed, but compute defensively.
        let res = integer_sqrt(v).min(0xFFFF_FFFF);
        self.sqrt_res = (res as u32).to_le_bytes();
    }

    /// `addr` is pre-masked to the low 28 bits by the IO dispatch.
    pub fn read8(&self, addr: u32) -> u32 {
        match addr {
            DIVCNT => self.divcnt & 0xFF,
            a if a == DIVCNT + 1 => (self.divcnt >> 8) & 0xFF,
            a if (DIV_NUMER..DIV_NUMER + 8).contains(&a) => self.numer[(a - DIV_NUMER) as usize] as u32,
            a if (DIV_DENOM..DIV_DENOM + 8).contains(&a) => self.denom[(a - DIV_DENOM) as usize] as u32,
            a if (DIV_RESULT..DIV_RESULT + 8).contains(&a) => self.result[(a - DIV_RESULT) as usize] as u32,
            a if (DIVREM_RESULT..DIVREM_RESULT + 8).contains(&a) => {
                self.remain[(a - DIVREM_RESULT) as usize] as u32
            }
            SQRTCNT => self.sqrtcnt & 0xFF,
            a if a == SQRTCNT + 1 => (self.sqrtcnt >> 8) & 0xFF,
            a if (SQRT_RESULT..SQRT_RESULT + 4).contains(&a) => self.sqrt_res[(a - SQRT_RESULT) as usize] as u32,
            a if (SQRT_PARAM..SQRT_PARAM + 8).contains(&a) => {
                self.sqrt_param[(a - SQRT_PARAM) as usize] as u32
            }
            _ => 0,
        }
    }

    pub fn write8(&mut self, addr: u32, value: u32) {
        let v = (value & 0xFF) as u8;
        match addr {
            DIVCNT => {
                self.divcnt = (self.divcnt & 0xFF00) | v as u32;
                self.recompute_div();
            }
            a if a == DIVCNT + 1 => {
                self.divcnt = (self.divcnt & 0x00FF) | ((v as u32) << 8);
                self.recompute_div();
            }
            a if (DIV_NUMER..DIV_NUMER + 8).contains(&a) => {
                self.numer[(a - DIV_NUMER) as usize] = v;
                self.recompute_div();
            }
            a if (DIV_DENOM..DIV_DENOM + 8).contains(&a) => {
                self.denom[(a - DIV_DENOM) as usize] = v;
                self.recompute_div();
            }
            SQRTCNT => {
                self.sqrtcnt = (self.sqrtcnt & 0xFF00) | v as u32;
                self.recompute_sqrt();
            }
            a if a == SQRTCNT + 1 => {
                self.sqrtcnt = (self.sqrtcnt & 0x00FF) | ((v as u32) << 8);
                self.recompute_sqrt();
            }
            a if (SQRT_PARAM..SQRT_PARAM + 8).contains(&a) => {
                self.sqrt_param[(a - SQRT_PARAM) as usize] = v;
                self.recompute_sqrt();
            }
            // Writes to DIV_RESULT / DIVREM_RESULT / SQRT_RESULT are no-ops.
            _ => {}
        }
    }

    // Wide accessors compose from byte (slow but correct, matching the TS).
    pub fn read16(&self, addr: u32) -> u32 {
        self.read8(addr) | (self.read8(addr + 1) << 8)
    }
    pub fn read32(&self, addr: u32) -> u32 {
        self.read8(addr)
            | (self.read8(addr + 1) << 8)
            | (self.read8(addr + 2) << 16)
            | (self.read8(addr + 3) << 24)
    }
    pub fn write16(&mut self, addr: u32, value: u32) {
        self.write8(addr, value & 0xFF);
        self.write8(addr + 1, (value >> 8) & 0xFF);
    }
    pub fn write32(&mut self, addr: u32, value: u32) {
        self.write16(addr, value & 0xFFFF);
        self.write16(addr + 2, (value >> 16) & 0xFFFF);
    }
}

/// Floor of the integer square root of a `u64` via Newton's method. Computed
/// in `u128` so `x + n/x` never overflows when `n` approaches `u64::MAX`.
fn integer_sqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let n = n as u128;
    let mut x = n;
    let mut y = (x + 1) >> 1;
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u64(b: &[u8; 8]) -> u64 {
        u64::from_le_bytes(*b)
    }

    // Drive the device the way a game would: set mode + operands via word
    // writes, then read back through the byte-composed read32 path.
    fn set_div(m: &mut DsMath, mode: u32, numer: u64, denom: u64) {
        m.write16(DIVCNT, mode & 0xFFFF);
        m.write32(DIV_NUMER, (numer & 0xFFFF_FFFF) as u32);
        m.write32(DIV_NUMER + 4, (numer >> 32) as u32);
        m.write32(DIV_DENOM, (denom & 0xFFFF_FFFF) as u32);
        m.write32(DIV_DENOM + 4, (denom >> 32) as u32);
    }

    fn quotient(m: &DsMath) -> u64 {
        (m.read32(DIV_RESULT) as u64) | ((m.read32(DIV_RESULT + 4) as u64) << 32)
    }
    fn remainder(m: &DsMath) -> u64 {
        (m.read32(DIVREM_RESULT) as u64) | ((m.read32(DIVREM_RESULT + 4) as u64) << 32)
    }

    #[test]
    fn div_32_32_basic() {
        let mut m = DsMath::new();
        set_div(&mut m, 0, 100, 7);
        assert_eq!(read_u64(&m.result) as i64, 14);
        assert_eq!(read_u64(&m.remain) as i64, 2);
        assert_eq!(m.divcnt & 0x4000, 0, "no div-by-zero error");
    }

    #[test]
    fn div_32_32_truncates_toward_zero() {
        let mut m = DsMath::new();
        // -7 / 2 should truncate toward zero => -3 remainder -1.
        set_div(&mut m, 0, (-7i64 as u64) & 0xFFFF_FFFF, 2);
        assert_eq!(read_u64(&m.result) as i64, -3);
        assert_eq!(read_u64(&m.remain) as i64, -1);
    }

    #[test]
    fn div_64_32_no_overflow_on_min() {
        // i64::MIN / -1 would overflow a naive i64 path; i128 keeps it sane.
        let mut m = DsMath::new();
        set_div(&mut m, 1, i64::MIN as u64, (-1i32 as u32) as u64);
        // Truncated 128-bit result is 2^63, which wraps to i64::MIN when stored.
        assert_eq!(read_u64(&m.result), 0x8000_0000_0000_0000);
        assert_eq!(read_u64(&m.remain) as i64, 0);
    }

    #[test]
    fn div_64_64_basic() {
        let mut m = DsMath::new();
        set_div(&mut m, 2, 0x0000_0010_0000_0000, 0x0000_0000_0000_0010);
        assert_eq!(read_u64(&m.result), 0x0000_0001_0000_0000);
        assert_eq!(remainder(&m), 0);
    }

    #[test]
    fn div_by_zero_sets_error_bit_full_denom() {
        let mut m = DsMath::new();
        set_div(&mut m, 0, 5, 0);
        assert_ne!(m.divcnt & 0x4000, 0, "error bit set when full denom is zero");
        // 32/32 div-by-zero: q = -1 (numerator positive), high half = sign-ext
        // of numerator (positive => 0).
        assert_eq!(quotient(&m), 0x0000_0000_FFFF_FFFF);
        assert_eq!(remainder(&m), 5);
    }

    #[test]
    fn div_by_zero_negative_numerator_high_half_quirk() {
        let mut m = DsMath::new();
        set_div(&mut m, 0, (-5i64 as u64) & 0xFFFF_FFFF, 0);
        // q = +1 in low half; high half = sign-ext of numerator (negative =>
        // 0xFFFFFFFF) — the documented 32/32 div-by-zero quirk.
        assert_eq!(quotient(&m), 0xFFFF_FFFF_0000_0001);
        assert_eq!(remainder(&m) as i64, -5);
    }

    #[test]
    fn div_32_32_zero_lo_nonzero_hi_denom_no_error_but_divides() {
        // denom_lo = 0, denom_hi != 0: the mode-0 (32-bit) denom is 0 so the
        // result uses div-by-zero behavior, but the FULL 64-bit denom is
        // non-zero so the error bit must stay clear (RockWrestler quirk).
        let mut m = DsMath::new();
        m.write16(DIVCNT, 0);
        m.write32(DIV_NUMER, 9);
        m.write32(DIV_NUMER + 4, 0);
        m.write32(DIV_DENOM, 0); // lo
        m.write32(DIV_DENOM + 4, 1); // hi != 0
        assert_eq!(m.divcnt & 0x4000, 0, "error bit clear: full denom nonzero");
        assert_eq!(quotient(&m), 0x0000_0000_FFFF_FFFF); // q = -1, hi = 0
        assert_eq!(remainder(&m), 9);
    }

    #[test]
    fn mode_3_behaves_like_mode_1() {
        let mut m = DsMath::new();
        set_div(&mut m, 3, 1_000_000_000_000, 1000);
        let q3 = read_u64(&m.result);
        set_div(&mut m, 1, 1_000_000_000_000, 1000);
        let q1 = read_u64(&m.result);
        assert_eq!(q3, q1);
        assert_eq!(q1, 1_000_000_000);
    }

    #[test]
    fn sqrt_32bit() {
        let mut m = DsMath::new();
        m.write16(SQRTCNT, 0); // 32-bit mode
        m.write32(SQRT_PARAM, 144);
        assert_eq!(m.read32(SQRT_RESULT), 12);
    }

    #[test]
    fn sqrt_32bit_floor() {
        let mut m = DsMath::new();
        m.write16(SQRTCNT, 0);
        m.write32(SQRT_PARAM, 1000);
        assert_eq!(m.read32(SQRT_RESULT), 31); // floor(sqrt(1000)) = 31
    }

    #[test]
    fn sqrt_64bit_max() {
        let mut m = DsMath::new();
        m.write16(SQRTCNT, 1); // 64-bit mode
        m.write32(SQRT_PARAM, 0xFFFF_FFFF);
        m.write32(SQRT_PARAM + 4, 0xFFFF_FFFF);
        // floor(sqrt(2^64-1)) = 2^32 - 1 = 0xFFFFFFFF
        assert_eq!(m.read32(SQRT_RESULT), 0xFFFF_FFFF);
    }

    #[test]
    fn sqrt_64bit_uses_high_word() {
        let mut m = DsMath::new();
        m.write16(SQRTCNT, 1);
        // value = 2^32 => sqrt = 2^16 = 65536
        m.write32(SQRT_PARAM, 0);
        m.write32(SQRT_PARAM + 4, 1);
        assert_eq!(m.read32(SQRT_RESULT), 65536);
    }

    #[test]
    fn sqrt_32bit_ignores_high_word() {
        let mut m = DsMath::new();
        m.write16(SQRTCNT, 0); // 32-bit: only low word matters
        m.write32(SQRT_PARAM, 144);
        m.write32(SQRT_PARAM + 4, 0xDEAD_BEEF);
        assert_eq!(m.read32(SQRT_RESULT), 12);
    }

    #[test]
    fn byte_read_back_matches() {
        let mut m = DsMath::new();
        set_div(&mut m, 0, 0x1234_5678, 0x0000_0010);
        // Reading the quotient byte-by-byte must equal the composed word read.
        let composed = m.read32(DIV_RESULT);
        let bytes = m.read8(DIV_RESULT)
            | (m.read8(DIV_RESULT + 1) << 8)
            | (m.read8(DIV_RESULT + 2) << 16)
            | (m.read8(DIV_RESULT + 3) << 24);
        assert_eq!(composed, bytes);
        assert_eq!(composed, 0x0123_4567);
    }

    #[test]
    fn divcnt_readback_preserves_busy_zero() {
        let mut m = DsMath::new();
        set_div(&mut m, 2, 10, 3);
        // Mode bits preserved, busy bit (15) never set since we compute sync.
        assert_eq!(m.read8(DIVCNT) & 0x3, 2);
        assert_eq!(m.divcnt & 0x8000, 0);
    }
}
