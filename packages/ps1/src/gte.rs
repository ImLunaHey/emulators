//! GTE (COP2) — Geometry Transformation Engine.
//!
//! Built from scratch against nocash's psx-spx ("Geometry Transformation Engine
//! (GTE)"). COP2 is a fixed-point vector/matrix coprocessor: 32 data registers
//! (r0..r31 — vectors, accumulators, the screen/colour FIFOs, the rotation
//! results) and 32 control registers (r32..r63 — the rotation matrix,
//! translation vector, light/colour matrices, screen offset/projection, FLAG).
//!
//! It is driven entirely by the CPU's COP2 opcodes:
//! * MFC2/CFC2  — read a data / control register ([`Gte::read_data`] /
//!   [`Gte::read_control`]).
//! * MTC2/CTC2  — write a data / control register ([`Gte::write_data`] /
//!   [`Gte::write_control`]).
//! * LWC2/SWC2  — load/store a single data register (same paths as MTC2/MFC2).
//! * COP2 *command* (RTPS, NCLIP, MVMVA, NCDS, AVSZ3/4, …) — runs a geometry
//!   operation over the registers ([`Gte::command`]).
//!
//! The register file is kept as raw `u32` words (the skeleton's `data` /
//! `control` arrays); the fixed-point sub-fields are unpacked per access. All
//! arithmetic mirrors the hardware: 16/32-bit packed fields, 44-bit MAC1..3
//! accumulators with overflow flags, the Unsigned Newton-Raphson reciprocal,
//! and the FLAG (r63) saturation register.

/// The GTE register file. 32 data + 32 control registers, all stored as raw
/// 32-bit words; the geometry ops unpack the fixed-point sub-fields.
#[derive(Debug, Clone)]
pub struct Gte {
    /// Data registers r0..r31 (VXY0, IR0..IR3, MAC0..MAC3, the SXY/SZ/RGB
    /// FIFOs, LZCS/LZCR, …).
    pub data: [u32; 32],
    /// Control registers r32..r63 (rotation matrix, TR, light/colour matrices,
    /// screen offset/projection, FLAG) addressed 0..31 here.
    pub control: [u32; 32],
}

impl Default for Gte {
    fn default() -> Self {
        Self::new()
    }
}

// ---- data register indices (cop2r0..r31) ----
const VXY0: usize = 0;
const VZ0: usize = 1;
#[allow(dead_code)]
const VXY1: usize = 2;
#[allow(dead_code)]
const VZ1: usize = 3;
#[allow(dead_code)]
const VXY2: usize = 4;
#[allow(dead_code)]
const VZ2: usize = 5;
const RGBC: usize = 6;
const OTZ: usize = 7;
const IR0: usize = 8;
const IR1: usize = 9;
const IR2: usize = 10;
const IR3: usize = 11;
const SXY0: usize = 12;
const SXY1: usize = 13;
const SXY2: usize = 14;
const SXYP: usize = 15;
const SZ0: usize = 16;
const SZ1: usize = 17;
const SZ2: usize = 18;
const SZ3: usize = 19;
const RGB0: usize = 20;
const RGB1: usize = 21;
const RGB2: usize = 22;
const MAC0: usize = 24;
const MAC1: usize = 25;
#[allow(dead_code)]
const MAC2: usize = 26;
#[allow(dead_code)]
const MAC3: usize = 27;
const IRGB: usize = 28;
const ORGB: usize = 29;
const LZCS: usize = 30;
const LZCR: usize = 31;

// ---- control register indices (cop2r32..r63 -> 0..31) ----
const RT11_12: usize = 0; // RT11,RT12
#[allow(dead_code)]
const RT13_21: usize = 1;
#[allow(dead_code)]
const RT22_23: usize = 2;
#[allow(dead_code)]
const RT31_32: usize = 3;
const RT33: usize = 4;
const TRX: usize = 5;
#[allow(dead_code)]
const TRY: usize = 6;
#[allow(dead_code)]
const TRZ: usize = 7;
const L11_12: usize = 8;
#[allow(dead_code)]
const L13_21: usize = 9;
#[allow(dead_code)]
const L22_23: usize = 10;
#[allow(dead_code)]
const L31_32: usize = 11;
const L33: usize = 12;
const RBK: usize = 13;
#[allow(dead_code)]
const GBK: usize = 14;
#[allow(dead_code)]
const BBK: usize = 15;
const LR1_2: usize = 16;
#[allow(dead_code)]
const LR3_G1: usize = 17;
#[allow(dead_code)]
const LG2_3: usize = 18;
#[allow(dead_code)]
const LB1_2: usize = 19;
const LB3: usize = 20;
const RFC: usize = 21;
#[allow(dead_code)]
const GFC: usize = 22;
#[allow(dead_code)]
const BFC: usize = 23;
const OFX: usize = 24;
const OFY: usize = 25;
const H: usize = 26;
const DQA: usize = 27;
const DQB: usize = 28;
const ZSF3: usize = 29;
const ZSF4: usize = 30;
const FLAG: usize = 31;

// ---- FLAG (cop2r63) bit positions (psx-spx) ----
const FLAG_IR0_SAT: u32 = 1 << 12; // IR0 out of 0..1000h
const FLAG_SY2_SAT: u32 = 1 << 13;
const FLAG_SX2_SAT: u32 = 1 << 14;
const FLAG_MAC0_NEG: u32 = 1 << 15;
const FLAG_MAC0_POS: u32 = 1 << 16;
const FLAG_DIVIDE_OVF: u32 = 1 << 17;
const FLAG_SZ3_SAT: u32 = 1 << 18; // SZ3/OTZ
const FLAG_B_SAT: u32 = 1 << 19; // colour-FIFO B
const FLAG_G_SAT: u32 = 1 << 20; // colour-FIFO G
const FLAG_R_SAT: u32 = 1 << 21; // colour-FIFO R
const FLAG_IR3_SAT: u32 = 1 << 22;
const FLAG_IR2_SAT: u32 = 1 << 23;
const FLAG_IR1_SAT: u32 = 1 << 24;
const FLAG_MAC3_NEG: u32 = 1 << 25;
const FLAG_MAC2_NEG: u32 = 1 << 26;
const FLAG_MAC1_NEG: u32 = 1 << 27;
const FLAG_MAC3_POS: u32 = 1 << 28;
const FLAG_MAC2_POS: u32 = 1 << 29;
const FLAG_MAC1_POS: u32 = 1 << 30;
/// Bits that, when set, OR into the master error flag (bit 31).
const FLAG_ERROR_MASK: u32 = 0x7F87_E000;

/// 257-entry Unsigned Newton-Raphson reciprocal seed table (psx-spx).
#[rustfmt::skip]
const UNR_TABLE: [u8; 257] = [
    0xFF, 0xFD, 0xFB, 0xF9, 0xF7, 0xF5, 0xF3, 0xF1, 0xEF, 0xEE, 0xEC, 0xEA, 0xE8, 0xE6, 0xE4, 0xE3,
    0xE1, 0xDF, 0xDD, 0xDC, 0xDA, 0xD8, 0xD6, 0xD5, 0xD3, 0xD1, 0xD0, 0xCE, 0xCD, 0xCB, 0xC9, 0xC8,
    0xC6, 0xC5, 0xC3, 0xC1, 0xC0, 0xBE, 0xBD, 0xBB, 0xBA, 0xB8, 0xB7, 0xB5, 0xB4, 0xB2, 0xB1, 0xB0,
    0xAE, 0xAD, 0xAB, 0xAA, 0xA9, 0xA7, 0xA6, 0xA4, 0xA3, 0xA2, 0xA0, 0x9F, 0x9E, 0x9C, 0x9B, 0x9A,
    0x99, 0x97, 0x96, 0x95, 0x94, 0x92, 0x91, 0x90, 0x8F, 0x8D, 0x8C, 0x8B, 0x8A, 0x89, 0x87, 0x86,
    0x85, 0x84, 0x83, 0x82, 0x81, 0x7F, 0x7E, 0x7D, 0x7C, 0x7B, 0x7A, 0x79, 0x78, 0x77, 0x75, 0x74,
    0x73, 0x72, 0x71, 0x70, 0x6F, 0x6E, 0x6D, 0x6C, 0x6B, 0x6A, 0x69, 0x68, 0x67, 0x66, 0x65, 0x64,
    0x63, 0x62, 0x61, 0x60, 0x5F, 0x5E, 0x5D, 0x5D, 0x5C, 0x5B, 0x5A, 0x59, 0x58, 0x57, 0x56, 0x55,
    0x54, 0x53, 0x53, 0x52, 0x51, 0x50, 0x4F, 0x4E, 0x4D, 0x4D, 0x4C, 0x4B, 0x4A, 0x49, 0x48, 0x48,
    0x47, 0x46, 0x45, 0x44, 0x43, 0x43, 0x42, 0x41, 0x40, 0x3F, 0x3F, 0x3E, 0x3D, 0x3C, 0x3C, 0x3B,
    0x3A, 0x39, 0x39, 0x38, 0x37, 0x36, 0x36, 0x35, 0x34, 0x33, 0x33, 0x32, 0x31, 0x31, 0x30, 0x2F,
    0x2E, 0x2E, 0x2D, 0x2C, 0x2C, 0x2B, 0x2A, 0x2A, 0x29, 0x28, 0x28, 0x27, 0x26, 0x26, 0x25, 0x24,
    0x24, 0x23, 0x22, 0x22, 0x21, 0x20, 0x20, 0x1F, 0x1E, 0x1E, 0x1D, 0x1D, 0x1C, 0x1B, 0x1B, 0x1A,
    0x19, 0x19, 0x18, 0x18, 0x17, 0x16, 0x16, 0x15, 0x15, 0x14, 0x14, 0x13, 0x12, 0x12, 0x11, 0x11,
    0x10, 0x0F, 0x0F, 0x0E, 0x0E, 0x0D, 0x0D, 0x0C, 0x0C, 0x0B, 0x0A, 0x0A, 0x09, 0x09, 0x08, 0x08,
    0x07, 0x07, 0x06, 0x06, 0x05, 0x05, 0x04, 0x04, 0x03, 0x03, 0x02, 0x02, 0x01, 0x01, 0x00, 0x00,
    0x00,
];

/// The decoded COP2 command word (the 25-bit immediate of the instruction).
#[derive(Debug, Clone, Copy)]
struct Cmd(u32);

impl Cmd {
    /// Real command number, bits 0..5.
    fn opcode(self) -> u32 {
        self.0 & 0x3F
    }
    /// `sf` — shift fraction: 1 ⇒ results SAR by 12, 0 ⇒ no shift.
    fn shift(self) -> u32 {
        if self.0 & (1 << 19) != 0 {
            12
        } else {
            0
        }
    }
    /// `lm` — saturate IR to 0..7FFF (true) or -8000..7FFF (false).
    fn lm(self) -> bool {
        self.0 & (1 << 10) != 0
    }
    /// MVMVA matrix select (bits 17..18).
    fn mx(self) -> u32 {
        (self.0 >> 17) & 3
    }
    /// MVMVA vector select (bits 15..16).
    fn vx(self) -> u32 {
        (self.0 >> 15) & 3
    }
    /// MVMVA translation select (bits 13..14).
    fn cv(self) -> u32 {
        (self.0 >> 13) & 3
    }
}

/// Sign-extend the low 16 bits of `v`.
#[inline]
fn sx16(v: u32) -> i32 {
    v as i16 as i32
}
/// Low half of a packed pair as a signed 16-bit value.
#[inline]
fn lo16(v: u32) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}
/// High half of a packed pair as a signed 16-bit value.
#[inline]
fn hi16(v: u32) -> i32 {
    ((v >> 16) & 0xFFFF) as u16 as i16 as i32
}

impl Gte {
    pub fn new() -> Self {
        Gte {
            data: [0; 32],
            control: [0; 32],
        }
    }

    // ============================ register file =========================

    /// MFC2: read a *data* register (r0..r31), applying the hardware read
    /// quirks (SXYP mirrors SXY2, the ORGB/LZCR derived registers, …).
    pub fn read_data(&self, reg: u32) -> u32 {
        let r = (reg & 0x1F) as usize;
        match r {
            // SXYP is a read mirror of SXY2 (the newest screen-XY FIFO entry).
            SXYP => self.data[SXY2],
            // IRGB read-back returns the packed 5:5:5 form of IR1..3 (>>7,
            // clamped 0..1F). ORGB holds the same value; we keep them in sync
            // when IR is written, so both read straight back.
            IRGB | ORGB => self.data[ORGB],
            // LZCR is the leading-zero count of LZCS, recomputed on LZCS write.
            _ => self.data[r],
        }
    }

    /// CFC2: read a *control* register (r32..r63, addressed 0..31). The matrix
    /// diagonals' last element (RT33/L33/LB3) and the H register sign-extend
    /// their 16-bit value on read — a documented hardware bug.
    pub fn read_control(&self, reg: u32) -> u32 {
        let r = (reg & 0x1F) as usize;
        match r {
            RT33 | L33 | LB3 | DQA | ZSF3 | ZSF4 | H => sx16(self.control[r]) as u32,
            _ => self.control[r],
        }
    }

    /// MTC2 / LWC2: write a *data* register. Some registers have side effects:
    /// SXYP shifts the screen-XY FIFO, IRGB unpacks a 5:5:5 colour into IR1..3,
    /// and LZCS recomputes the leading-zero count LZCR.
    pub fn write_data(&mut self, reg: u32, v: u32) {
        let r = (reg & 0x1F) as usize;
        match r {
            // Writing SXYP pushes the FIFO: SXY0<-SXY1<-SXY2<-new.
            SXYP => {
                self.data[SXY0] = self.data[SXY1];
                self.data[SXY1] = self.data[SXY2];
                self.data[SXY2] = v;
            }
            // IRGB: unpack a 5:5:5 colour into IR1/2/3 (each *0x80) and mirror
            // into ORGB for read-back.
            IRGB => {
                self.data[IRGB] = v & 0x7FFF;
                let r5 = v & 0x1F;
                let g5 = (v >> 5) & 0x1F;
                let b5 = (v >> 10) & 0x1F;
                self.data[IR1] = (r5 * 0x80) as u16 as u32;
                self.data[IR2] = (g5 * 0x80) as u16 as u32;
                self.data[IR3] = (b5 * 0x80) as u16 as u32;
                self.data[ORGB] = v & 0x7FFF;
            }
            // ORGB / LZCR are read-only mirrors; ignore direct writes.
            ORGB | LZCR => {}
            // LZCS: store, and recompute LZCR (count of leading equal bits).
            LZCS => {
                self.data[LZCS] = v;
                self.data[LZCR] = leading_count(v);
            }
            _ => self.data[r] = v,
        }
    }

    /// CTC2: write a *control* register. Plain storage; the read-back
    /// sign-extension quirks live in [`Gte::read_control`].
    pub fn write_control(&mut self, reg: u32, v: u32) {
        self.control[(reg & 0x1F) as usize] = v;
    }

    // ============================ field accessors =======================
    // Vectors V0..V2 (X,Y in the XY word, Z in the Z word).
    fn vx(&self, n: usize) -> i32 {
        lo16(self.data[VXY0 + n * 2])
    }
    fn vy(&self, n: usize) -> i32 {
        hi16(self.data[VXY0 + n * 2])
    }
    fn vz(&self, n: usize) -> i32 {
        lo16(self.data[VZ0 + n * 2])
    }

    fn ir(&self, n: usize) -> i32 {
        sx16(self.data[IR0 + n])
    }
    fn set_ir(&mut self, n: usize, v: i32) {
        self.data[IR0 + n] = (v as i16 as u16) as u32;
    }
    fn ir0(&self) -> i32 {
        sx16(self.data[IR0])
    }

    fn mac(&self, n: usize) -> i64 {
        // MAC1..3 are stored as 32-bit; we sign-extend for read-back chaining.
        (self.data[MAC1 + (n - 1)] as i32) as i64
    }
    fn set_mac(&mut self, n: usize, v: i64) {
        self.data[MAC1 + (n - 1)] = v as i32 as u32;
    }

    // Rotation matrix RT (control). element(row,col), 1-based.
    fn rt(&self, row: usize, col: usize) -> i32 {
        self.matrix_elem(RT11_12, row, col)
    }
    // Light matrix.
    fn lm_matrix(&self, row: usize, col: usize) -> i32 {
        self.matrix_elem(L11_12, row, col)
    }
    // Light colour matrix.
    fn lc_matrix(&self, row: usize, col: usize) -> i32 {
        self.matrix_elem(LR1_2, row, col)
    }
    /// Fetch element (row,col) [1-based] of a 3x3 matrix packed across 5 words
    /// starting at `base` (m11|m12, m13|m21, m22|m23, m31|m32, m33).
    fn matrix_elem(&self, base: usize, row: usize, col: usize) -> i32 {
        let idx = (row - 1) * 3 + (col - 1); // 0..8
        match idx {
            0 => lo16(self.control[base]),
            1 => hi16(self.control[base]),
            2 => lo16(self.control[base + 1]),
            3 => hi16(self.control[base + 1]),
            4 => lo16(self.control[base + 2]),
            5 => hi16(self.control[base + 2]),
            6 => lo16(self.control[base + 3]),
            7 => hi16(self.control[base + 3]),
            _ => lo16(self.control[base + 4]),
        }
    }

    fn tr(&self, n: usize) -> i64 {
        self.control[TRX + n] as i32 as i64
    }
    fn bk(&self, n: usize) -> i64 {
        self.control[RBK + n] as i32 as i64
    }
    fn fc(&self, n: usize) -> i64 {
        self.control[RFC + n] as i32 as i64
    }

    fn rgbc_byte(&self, n: usize) -> i64 {
        ((self.data[RGBC] >> (n * 8)) & 0xFF) as i64
    }
    fn code(&self) -> u32 {
        (self.data[RGBC] >> 24) & 0xFF
    }

    // ============================ FLAG handling =========================
    fn set_flag_bit(&mut self, bit: u32) {
        self.control[FLAG] |= bit;
    }
    fn clear_flags(&mut self) {
        self.control[FLAG] = 0;
    }
    fn finalize_flags(&mut self) {
        if self.control[FLAG] & FLAG_ERROR_MASK != 0 {
            self.control[FLAG] |= 1 << 31;
        }
    }

    // ---- saturation primitives (psx-spx lm_B/C/D/E/G/H) ----

    /// lm_B: saturate a MAC value to an IR register. `lm` true ⇒ clamp 0..7FFF.
    fn lm_b(&mut self, n: usize, value: i32, lm: bool) -> i32 {
        let (min, max) = if lm { (0, 0x7FFF) } else { (-0x8000, 0x7FFF) };
        let bit = match n {
            1 => FLAG_IR1_SAT,
            2 => FLAG_IR2_SAT,
            _ => FLAG_IR3_SAT,
        };
        if value < min {
            self.set_flag_bit(bit);
            min
        } else if value > max {
            self.set_flag_bit(bit);
            max
        } else {
            value
        }
    }

    /// lm_C: saturate a colour component to 0..FF.
    fn lm_c(&mut self, n: usize, value: i32) -> i32 {
        let bit = match n {
            0 => FLAG_R_SAT,
            1 => FLAG_G_SAT,
            _ => FLAG_B_SAT,
        };
        if value < 0 {
            self.set_flag_bit(bit);
            0
        } else if value > 0xFF {
            self.set_flag_bit(bit);
            0xFF
        } else {
            value
        }
    }

    /// lm_D: saturate SZ3 / OTZ to 0..FFFF.
    fn lm_d(&mut self, value: i64) -> u16 {
        if value < 0 {
            self.set_flag_bit(FLAG_SZ3_SAT);
            0
        } else if value > 0xFFFF {
            self.set_flag_bit(FLAG_SZ3_SAT);
            0xFFFF
        } else {
            value as u16
        }
    }

    /// lm_G: saturate a screen coordinate to -400..3FF.
    fn lm_g(&mut self, value: i64, is_x: bool) -> i32 {
        let bit = if is_x { FLAG_SX2_SAT } else { FLAG_SY2_SAT };
        if value < -0x400 {
            self.set_flag_bit(bit);
            -0x400
        } else if value > 0x3FF {
            self.set_flag_bit(bit);
            0x3FF
        } else {
            value as i32
        }
    }

    /// lm_H: saturate IR0 to 0..1000.
    fn lm_h(&mut self, value: i64) -> i32 {
        if value < 0 {
            self.set_flag_bit(FLAG_IR0_SAT);
            0
        } else if value > 0x1000 {
            self.set_flag_bit(FLAG_IR0_SAT);
            0x1000
        } else {
            value as i32
        }
    }

    /// Check a 44-bit MAC1..3 accumulator value for overflow, setting the
    /// positive/negative FLAG bit. Returns the value unchanged (the flag is the
    /// only effect; the stored MAC is the low 32 bits after the SAR shift).
    fn check_mac(&mut self, n: usize, value: i64) -> i64 {
        const MAX: i64 = (1 << 43) - 1;
        const MIN: i64 = -(1 << 43);
        let (pos, neg) = match n {
            1 => (FLAG_MAC1_POS, FLAG_MAC1_NEG),
            2 => (FLAG_MAC2_POS, FLAG_MAC2_NEG),
            _ => (FLAG_MAC3_POS, FLAG_MAC3_NEG),
        };
        if value > MAX {
            self.set_flag_bit(pos);
        } else if value < MIN {
            self.set_flag_bit(neg);
        }
        value
    }

    /// Check MAC0's 32-bit accumulator for overflow.
    fn check_mac0(&mut self, value: i64) -> i64 {
        if value > i32::MAX as i64 {
            self.set_flag_bit(FLAG_MAC0_POS);
        } else if value < i32::MIN as i64 {
            self.set_flag_bit(FLAG_MAC0_NEG);
        }
        value
    }

    // ---- FIFO pushes ----
    fn push_sz(&mut self, sz: u16) {
        self.data[SZ0] = self.data[SZ1];
        self.data[SZ1] = self.data[SZ2];
        self.data[SZ2] = self.data[SZ3];
        self.data[SZ3] = sz as u32;
    }
    fn push_sxy(&mut self, sx: i32, sy: i32) {
        self.data[SXY0] = self.data[SXY1];
        self.data[SXY1] = self.data[SXY2];
        self.data[SXY2] = ((sx as u16 as u32) & 0xFFFF) | (((sy as u16 as u32) & 0xFFFF) << 16);
    }
    fn push_rgb(&mut self, r: i32, g: i32, b: i32) {
        let code = self.code();
        let r = self.lm_c(0, r);
        let g = self.lm_c(1, g);
        let b = self.lm_c(2, b);
        self.data[RGB0] = self.data[RGB1];
        self.data[RGB1] = self.data[RGB2];
        self.data[RGB2] =
            (r as u32 & 0xFF) | ((g as u32 & 0xFF) << 8) | ((b as u32 & 0xFF) << 16) | (code << 24);
    }

    // ============================ command dispatch ======================

    /// Execute a COP2 command (the 25-bit command word: opcode + sf/lm/mvmva
    /// fields). Decodes bits 0..5 and runs the corresponding geometry op,
    /// updating the MAC/IR/FIFO registers and the FLAG saturation register.
    pub fn command(&mut self, command: u32) {
        let cmd = Cmd(command);
        self.clear_flags();
        match cmd.opcode() {
            0x01 => self.cmd_rtps(cmd, 0, true),
            0x06 => self.cmd_nclip(),
            0x0C => self.cmd_op(cmd),
            0x10 => self.cmd_dpcs(cmd, false),
            0x11 => self.cmd_intpl(cmd),
            0x12 => self.cmd_mvmva(cmd),
            0x13 => self.cmd_ncds(cmd, false, true, true),
            0x14 => self.cmd_cdp(cmd),
            0x16 => self.cmd_ncdt(cmd),
            0x1B => self.cmd_ncds(cmd, false, false, true),
            0x1C => self.cmd_cc(cmd),
            0x1E => self.cmd_ncds(cmd, false, true, false),
            0x20 => self.cmd_nct(cmd, false, true, false),
            0x28 => self.cmd_sqr(cmd),
            0x29 => self.cmd_dcpl(cmd),
            0x2A => self.cmd_dpct(cmd),
            0x2D => self.cmd_avsz3(),
            0x2E => self.cmd_avsz4(),
            0x30 => self.cmd_rtpt(cmd),
            0x3D => self.cmd_gpf(cmd),
            0x3E => self.cmd_gpl(cmd),
            0x3F => self.cmd_nct(cmd, false, false, true),
            _ => {}
        }
        self.finalize_flags();
    }

    // ---- RTPS / RTPT: perspective transform ----
    fn cmd_rtps(&mut self, cmd: Cmd, v: usize, last: bool) {
        let sf = cmd.shift();
        let (vx, vy, vz) = (self.vx(v) as i64, self.vy(v) as i64, self.vz(v) as i64);

        // MAC1..3 = (TR*1000h + RT*V) >> sf.
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = self.tr(r) << 12;
            acc = self.check_mac(row, acc + self.rt(row, 1) as i64 * vx);
            acc = self.check_mac(row, acc + self.rt(row, 2) as i64 * vy);
            acc = self.check_mac(row, acc + self.rt(row, 3) as i64 * vz);
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);

        let lm = cmd.lm();
        let ir1 = self.lm_b(1, mac[0] as i32, lm);
        let ir2 = self.lm_b(2, mac[1] as i32, lm);
        self.set_ir(1, ir1);
        self.set_ir(2, ir2);
        // IR3's FLAG.22 check always uses lm=0 (psx-spx), but the *stored*
        // value still respects lm.
        let mac3 = mac[2] as i32;
        let _ = self.lm_b(3, mac3, false); // FLAG only
        let ir3 = mac3.clamp(if lm { 0 } else { -0x8000 }, 0x7FFF);
        self.set_ir(3, ir3);

        // SZ3 FIFO push uses the *unshifted* MAC3 (>> 12 of the sf=0 form).
        let sz = self.lm_d((mac[2] >> ((1 - sf / 12) * 12)).clamp(0, 0xFFFF));
        self.push_sz(sz);

        // Perspective division H / SZ3.
        let depth = self.divide();

        let mac0x = self.check_mac0(depth as i64 * ir1 as i64 + self.control[OFX] as i32 as i64);
        let sx = self.lm_g(mac0x >> 16, true);
        let mac0y = self.check_mac0(depth as i64 * ir2 as i64 + self.control[OFY] as i32 as i64);
        let sy = self.lm_g(mac0y >> 16, false);
        self.push_sxy(sx, sy);

        if last {
            // Depth cueing: MAC0 = DQB + DQA*depth ; IR0 = MAC0 >> 12.
            let dqa = sx16(self.control[DQA]) as i64;
            let dqb = self.control[DQB] as i32 as i64;
            let mac0 = self.check_mac0(depth as i64 * dqa + dqb);
            self.data[MAC0] = mac0 as i32 as u32;
            let ir0 = self.lm_h(mac0 >> 12);
            self.data[IR0] = (ir0 as i16 as u16) as u32;
        }
    }

    fn cmd_rtpt(&mut self, cmd: Cmd) {
        self.cmd_rtps(cmd, 0, false);
        self.cmd_rtps(cmd, 1, false);
        self.cmd_rtps(cmd, 2, true);
    }

    /// The Unsigned Newton-Raphson reciprocal: depth = (H / SZ3) saturated to
    /// 1FFFFh. Sets FLAG.17 on overflow / divide-by-zero.
    fn divide(&mut self) -> u32 {
        let h = (self.control[H] & 0xFFFF) as u32;
        let sz3 = self.data[SZ3] & 0xFFFF;
        if h >= sz3 * 2 || sz3 == 0 {
            self.set_flag_bit(FLAG_DIVIDE_OVF);
            return 0x1FFFF;
        }
        let z = sz3.leading_zeros() - 16; // SZ3 is 16-bit; shift count 0..15
        let n = (h << z) as u64;
        let d = (sz3 << z) as u64; // 8000h..FFFFh
        let u = UNR_TABLE[((d - 0x7FC0) >> 7) as usize] as u64 + 0x101;
        let d = (0x0200_0080u64.wrapping_sub(d * u)) >> 8;
        let d = (0x0000_0080u64.wrapping_add(d * u)) >> 8;
        let result = ((n * d) + 0x8000) >> 16;
        result.min(0x1FFFF) as u32
    }

    // ---- NCLIP ----
    fn cmd_nclip(&mut self) {
        let (sx0, sy0) = (lo16(self.data[SXY0]) as i64, hi16(self.data[SXY0]) as i64);
        let (sx1, sy1) = (lo16(self.data[SXY1]) as i64, hi16(self.data[SXY1]) as i64);
        let (sx2, sy2) = (lo16(self.data[SXY2]) as i64, hi16(self.data[SXY2]) as i64);
        let mac0 = sx0 * sy1 + sx1 * sy2 + sx2 * sy0 - sx0 * sy2 - sx1 * sy0 - sx2 * sy1;
        let mac0 = self.check_mac0(mac0);
        self.data[MAC0] = mac0 as i32 as u32;
    }

    // ---- AVSZ3 / AVSZ4 ----
    fn cmd_avsz3(&mut self) {
        let zsf3 = sx16(self.control[ZSF3]) as i64;
        let sum =
            (self.data[SZ1] & 0xFFFF) as i64 + (self.data[SZ2] & 0xFFFF) as i64 + (self.data[SZ3] & 0xFFFF) as i64;
        let mac0 = self.check_mac0(zsf3 * sum);
        self.data[MAC0] = mac0 as i32 as u32;
        let otz = self.lm_d(mac0 >> 12);
        self.data[OTZ] = otz as u32;
    }

    fn cmd_avsz4(&mut self) {
        let zsf4 = sx16(self.control[ZSF4]) as i64;
        let sum = (self.data[SZ0] & 0xFFFF) as i64
            + (self.data[SZ1] & 0xFFFF) as i64
            + (self.data[SZ2] & 0xFFFF) as i64
            + (self.data[SZ3] & 0xFFFF) as i64;
        let mac0 = self.check_mac0(zsf4 * sum);
        self.data[MAC0] = mac0 as i32 as u32;
        let otz = self.lm_d(mac0 >> 12);
        self.data[OTZ] = otz as u32;
    }

    // ---- MVMVA ----
    fn cmd_mvmva(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let lm = cmd.lm();
        // Vector select.
        let (vx, vy, vz): (i64, i64, i64) = match cmd.vx() {
            0 => (self.vx(0) as i64, self.vy(0) as i64, self.vz(0) as i64),
            1 => (self.vx(1) as i64, self.vy(1) as i64, self.vz(1) as i64),
            2 => (self.vx(2) as i64, self.vy(2) as i64, self.vz(2) as i64),
            _ => (self.ir(1) as i64, self.ir(2) as i64, self.ir(3) as i64),
        };
        let v = [vx, vy, vz];

        // Matrix select (3 ⇒ a garbage "matrix" from the colour regs; modelled
        // as zero here — the FC-bug path below covers the relevant case).
        let mtx = |g: &Gte, row: usize, col: usize| -> i64 {
            match cmd.mx() {
                0 => g.rt(row, col) as i64,
                1 => g.lm_matrix(row, col) as i64,
                2 => g.lc_matrix(row, col) as i64,
                _ => 0,
            }
        };
        // Translation select.
        let tr = |g: &Gte, n: usize| -> i64 {
            match cmd.cv() {
                0 => g.tr(n),
                1 => g.bk(n),
                2 => g.fc(n),
                _ => 0,
            }
        };

        let mut mac = [0i64; 3];
        let buggy_fc = cmd.cv() == 2;
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = if buggy_fc { 0 } else { tr(self, r) << 12 };
            if buggy_fc {
                // The FC translation bug: the first matrix*vector term is
                // computed (and overflow-checked) but then discarded; only the
                // remaining two terms accumulate.
                let _ = self.check_mac(row, (tr(self, r) << 12) + mtx(self, row, 1) * v[0]);
                acc = self.check_mac(row, acc + mtx(self, row, 2) * v[1]);
                acc = self.check_mac(row, acc + mtx(self, row, 3) * v[2]);
            } else {
                acc = self.check_mac(row, acc + mtx(self, row, 1) * v[0]);
                acc = self.check_mac(row, acc + mtx(self, row, 2) * v[1]);
                acc = self.check_mac(row, acc + mtx(self, row, 3) * v[2]);
            }
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        let ir1 = self.lm_b(1, mac[0] as i32, lm);
        let ir2 = self.lm_b(2, mac[1] as i32, lm);
        let ir3 = self.lm_b(3, mac[2] as i32, lm);
        self.set_ir(1, ir1);
        self.set_ir(2, ir2);
        self.set_ir(3, ir3);
    }

    // ---- SQR ----
    fn cmd_sqr(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        for n in 1..=3 {
            let ir = self.ir(n) as i64;
            let mac = self.check_mac(n, ir * ir) >> sf;
            self.set_mac(n, mac);
            // SQR results are always positive ⇒ lm is irrelevant, but FLAG uses
            // the active lm value.
            let v = self.lm_b(n, mac as i32, cmd.lm());
            self.set_ir(n, v);
        }
    }

    // ---- OP (cross product with the matrix diagonal) ----
    fn cmd_op(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let (d1, d2, d3) = (
            self.rt(1, 1) as i64,
            self.rt(2, 2) as i64,
            self.rt(3, 3) as i64,
        );
        let (ir1, ir2, ir3) = (self.ir(1) as i64, self.ir(2) as i64, self.ir(3) as i64);
        let m1 = self.check_mac(1, ir3 * d2 - ir2 * d3) >> sf;
        let m2 = self.check_mac(2, ir1 * d3 - ir3 * d1) >> sf;
        let m3 = self.check_mac(3, ir2 * d1 - ir1 * d2) >> sf;
        self.set_mac(1, m1);
        self.set_mac(2, m2);
        self.set_mac(3, m3);
        let lm = cmd.lm();
        let v1 = self.lm_b(1, m1 as i32, lm);
        let v2 = self.lm_b(2, m2 as i32, lm);
        let v3 = self.lm_b(3, m3 as i32, lm);
        self.set_ir(1, v1);
        self.set_ir(2, v2);
        self.set_ir(3, v3);
    }

    // ---- colour interpolation helper: MAC = MAC + (FC-MAC)*IR0 ----
    /// Apply the far-colour interpolation in place over `mac` (the 64-bit
    /// pre-shift accumulators), shifting by `sf` afterwards.
    fn interpolate(&mut self, mut mac: [i64; 3], sf: u32) -> [i64; 3] {
        let ir0 = self.ir0() as i64;
        for r in 0..3 {
            // (FC<<12 - MAC) >> sf, saturated to IR (lm=0) — discarded result,
            // FLAG only — then * IR0 + MAC.
            let t = (self.fc(r) << 12) - mac[r];
            let t = self.check_mac(r + 1, t) >> sf;
            let ir = self.lm_b(r + 1, t as i32, false) as i64;
            mac[r] = self.check_mac(r + 1, ir * ir0 + mac[r]);
        }
        mac
    }

    /// Store MAC1..3 (post-shift), push the RGB FIFO from MAC>>4, and saturate
    /// into IR1..3 — the common tail of every colour command.
    fn color_tail(&mut self, mac: [i64; 3], sf: u32, lm: bool) {
        let mac = [mac[0] >> sf, mac[1] >> sf, mac[2] >> sf];
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        self.push_rgb((mac[0] >> 4) as i32, (mac[1] >> 4) as i32, (mac[2] >> 4) as i32);
        let v1 = self.lm_b(1, mac[0] as i32, lm);
        let v2 = self.lm_b(2, mac[1] as i32, lm);
        let v3 = self.lm_b(3, mac[2] as i32, lm);
        self.set_ir(1, v1);
        self.set_ir(2, v2);
        self.set_ir(3, v3);
    }

    // ---- DPCS / DPCT (depth cue colour) ----
    fn cmd_dpcs(&mut self, cmd: Cmd, from_fifo: bool) {
        let sf = cmd.shift();
        let (r, g, b) = if from_fifo {
            (
                (self.data[RGB0] & 0xFF) as i64,
                ((self.data[RGB0] >> 8) & 0xFF) as i64,
                ((self.data[RGB0] >> 16) & 0xFF) as i64,
            )
        } else {
            (self.rgbc_byte(0), self.rgbc_byte(1), self.rgbc_byte(2))
        };
        let mac = [r << 16, g << 16, b << 16];
        let mac = self.interpolate(mac, sf);
        self.color_tail(mac, sf, cmd.lm());
    }

    fn cmd_dpct(&mut self, cmd: Cmd) {
        // Reads RGB0 three times (each push shifts a fresh value in).
        for _ in 0..3 {
            self.cmd_dpcs(cmd, true);
        }
    }

    // ---- INTPL ----
    fn cmd_intpl(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let mac = [
            (self.ir(1) as i64) << 12,
            (self.ir(2) as i64) << 12,
            (self.ir(3) as i64) << 12,
        ];
        let mac = self.interpolate(mac, sf);
        self.color_tail(mac, sf, cmd.lm());
    }

    // ---- DCPL ----
    fn cmd_dcpl(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let mac = [
            self.rgbc_byte(0) * self.ir(1) as i64,
            self.rgbc_byte(1) * self.ir(2) as i64,
            self.rgbc_byte(2) * self.ir(3) as i64,
        ];
        let mac = [mac[0] << 4, mac[1] << 4, mac[2] << 4];
        let mac = self.interpolate(mac, sf);
        self.color_tail(mac, sf, cmd.lm());
    }

    // ---- NCS/NCT/NCDS/NCDT/NCCS/NCCT family ----
    /// One normal-colour vertex. `interp` toggles the FC interpolation (NCD*),
    /// `mul_color` multiplies by RGBC at the end (NC?S keep the colour for
    /// NCCS/NCDS; NCS does not interpolate). `_t` distinguishes single/triple
    /// callers (handled by the wrappers).
    fn nc_vertex(&mut self, cmd: Cmd, v: usize, interp: bool, mul_color: bool) {
        let sf = cmd.shift();
        let lm = cmd.lm();
        let (vx, vy, vz) = (self.vx(v) as i64, self.vy(v) as i64, self.vz(v) as i64);
        let vvec = [vx, vy, vz];

        // Step 1: MAC = (LLM * V) >> sf ; IR = lm_B(MAC).
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = self.check_mac(row, self.lm_matrix(row, 1) as i64 * vvec[0]);
            acc = self.check_mac(row, acc + self.lm_matrix(row, 2) as i64 * vvec[1]);
            acc = self.check_mac(row, acc + self.lm_matrix(row, 3) as i64 * vvec[2]);
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        let i1 = self.lm_b(1, mac[0] as i32, lm);
        let i2 = self.lm_b(2, mac[1] as i32, lm);
        let i3 = self.lm_b(3, mac[2] as i32, lm);
        self.set_ir(1, i1);
        self.set_ir(2, i2);
        self.set_ir(3, i3);

        // Step 2: MAC = (BK*1000h + LCM * IR) >> sf ; IR = lm_B(MAC).
        let ir = [i1 as i64, i2 as i64, i3 as i64];
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = self.bk(r) << 12;
            acc = self.check_mac(row, acc + self.lc_matrix(row, 1) as i64 * ir[0]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 2) as i64 * ir[1]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 3) as i64 * ir[2]);
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        let i1 = self.lm_b(1, mac[0] as i32, lm);
        let i2 = self.lm_b(2, mac[1] as i32, lm);
        let i3 = self.lm_b(3, mac[2] as i32, lm);
        self.set_ir(1, i1);
        self.set_ir(2, i2);
        self.set_ir(3, i3);

        // Step 3: colour. NCDS/NCCS multiply by RGBC; NCS just copies IR.
        let mut mac = if mul_color {
            [
                (self.rgbc_byte(0) * i1 as i64) << 4,
                (self.rgbc_byte(1) * i2 as i64) << 4,
                (self.rgbc_byte(2) * i3 as i64) << 4,
            ]
        } else {
            [(i1 as i64) << 4, (i2 as i64) << 4, (i3 as i64) << 4]
        };
        if interp {
            mac = self.interpolate(mac, sf);
        }
        self.color_tail(mac, sf, lm);
    }

    /// NCDS/NCCS/NCS — single vertex (V0).
    fn cmd_ncds(&mut self, cmd: Cmd, _depth: bool, interp: bool, mul_color: bool) {
        self.nc_vertex(cmd, 0, interp, mul_color);
    }

    /// CDP — colour depth cue (uses IR as input, like NCDS step 2 onward, lm=1).
    fn cmd_cdp(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let ir = [self.ir(1) as i64, self.ir(2) as i64, self.ir(3) as i64];
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = self.bk(r) << 12;
            acc = self.check_mac(row, acc + self.lc_matrix(row, 1) as i64 * ir[0]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 2) as i64 * ir[1]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 3) as i64 * ir[2]);
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        let i1 = self.lm_b(1, mac[0] as i32, true);
        let i2 = self.lm_b(2, mac[1] as i32, true);
        let i3 = self.lm_b(3, mac[2] as i32, true);
        self.set_ir(1, i1);
        self.set_ir(2, i2);
        self.set_ir(3, i3);
        let mac = [
            (self.rgbc_byte(0) * i1 as i64) << 4,
            (self.rgbc_byte(1) * i2 as i64) << 4,
            (self.rgbc_byte(2) * i3 as i64) << 4,
        ];
        let mac = self.interpolate(mac, sf);
        self.color_tail(mac, sf, true);
    }

    /// CC — colour conversion (lm=1, no interpolation; IR is the input).
    fn cmd_cc(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let ir = [self.ir(1) as i64, self.ir(2) as i64, self.ir(3) as i64];
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let row = r + 1;
            let mut acc = self.bk(r) << 12;
            acc = self.check_mac(row, acc + self.lc_matrix(row, 1) as i64 * ir[0]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 2) as i64 * ir[1]);
            acc = self.check_mac(row, acc + self.lc_matrix(row, 3) as i64 * ir[2]);
            *m = acc >> sf;
        }
        self.set_mac(1, mac[0]);
        self.set_mac(2, mac[1]);
        self.set_mac(3, mac[2]);
        let i1 = self.lm_b(1, mac[0] as i32, true);
        let i2 = self.lm_b(2, mac[1] as i32, true);
        let i3 = self.lm_b(3, mac[2] as i32, true);
        self.set_ir(1, i1);
        self.set_ir(2, i2);
        self.set_ir(3, i3);
        let mac = [
            (self.rgbc_byte(0) * i1 as i64) << 4,
            (self.rgbc_byte(1) * i2 as i64) << 4,
            (self.rgbc_byte(2) * i3 as i64) << 4,
        ];
        self.color_tail(mac, sf, true);
    }

    /// NCDT/NCCT/NCT — three vertices (V0,V1,V2).
    fn cmd_nct(&mut self, cmd: Cmd, _depth: bool, interp: bool, mul_color: bool) {
        self.nc_vertex(cmd, 0, interp, mul_color);
        self.nc_vertex(cmd, 1, interp, mul_color);
        self.nc_vertex(cmd, 2, interp, mul_color);
    }

    /// NCDT wrapper (triple, interpolated, colour-multiplied).
    fn cmd_ncdt(&mut self, cmd: Cmd) {
        self.cmd_nct(cmd, false, true, true);
    }

    // ---- GPF / GPL (general-purpose interpolation) ----
    fn cmd_gpf(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let ir0 = self.ir0() as i64;
        let mac = [
            self.check_mac(1, self.ir(1) as i64 * ir0),
            self.check_mac(2, self.ir(2) as i64 * ir0),
            self.check_mac(3, self.ir(3) as i64 * ir0),
        ];
        self.color_tail(mac, sf, cmd.lm());
    }

    fn cmd_gpl(&mut self, cmd: Cmd) {
        let sf = cmd.shift();
        let ir0 = self.ir0() as i64;
        // GPL seeds the accumulator with the current MAC1..3 (<<sf), then adds
        // IR*IR0.
        let base = [
            (self.mac(1)) << sf,
            (self.mac(2)) << sf,
            (self.mac(3)) << sf,
        ];
        let mac = [
            self.check_mac(1, self.ir(1) as i64 * ir0 + base[0]),
            self.check_mac(2, self.ir(2) as i64 * ir0 + base[1]),
            self.check_mac(3, self.ir(3) as i64 * ir0 + base[2]),
        ];
        self.color_tail(mac, sf, cmd.lm());
    }
}

/// Count of leading bits equal to bit 31 (for LZCR). For a positive value this
/// is the leading-zero count; for a negative value it is the leading-one count.
/// Always 1..32.
fn leading_count(v: u32) -> u32 {
    if v & 0x8000_0000 != 0 {
        (!v).leading_zeros()
    } else {
        v.leading_zeros()
    }
    .max(if v == 0 { 32 } else { 0 })
    .min(32)
    .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_files_round_trip() {
        let mut gte = Gte::new();
        gte.write_data(1, 0xDEAD_BEEF);
        gte.write_control(0, 0x0000_1FFF);
        assert_eq!(gte.read_data(1), 0xDEAD_BEEF);
        assert_eq!(gte.read_control(0), 0x0000_1FFF);
    }

    #[test]
    fn sxyp_mirrors_and_shifts_fifo() {
        let mut gte = Gte::new();
        gte.data[SXY2] = 0x1111_2222;
        // Reading SXYP mirrors SXY2.
        assert_eq!(gte.read_data(SXYP as u32), 0x1111_2222);
        // Writing SXYP pushes the FIFO.
        gte.write_data(SXYP as u32, 0xAAAA_BBBB);
        assert_eq!(gte.data[SXY1], 0x1111_2222);
        assert_eq!(gte.data[SXY2], 0xAAAA_BBBB);
    }

    #[test]
    fn control_read_sign_extends_h() {
        let mut gte = Gte::new();
        gte.write_control(H as u32, 0x0000_8000);
        // CFC2 of H sign-extends the unsigned 16-bit value (hardware bug).
        assert_eq!(gte.read_control(H as u32), 0xFFFF_8000);
    }

    #[test]
    fn lzcs_computes_leading_count() {
        let mut gte = Gte::new();
        // Positive: leading zeros.
        gte.write_data(LZCS as u32, 0x0000_FFFF);
        assert_eq!(gte.read_data(LZCR as u32), 16);
        // Negative: leading ones.
        gte.write_data(LZCS as u32, 0xFFFF_0000);
        assert_eq!(gte.read_data(LZCR as u32), 16);
        // All zero ⇒ 32.
        gte.write_data(LZCS as u32, 0);
        assert_eq!(gte.read_data(LZCR as u32), 32);
        // All ones ⇒ 32.
        gte.write_data(LZCS as u32, 0xFFFF_FFFF);
        assert_eq!(gte.read_data(LZCR as u32), 32);
    }

    #[test]
    fn nclip_signed_area() {
        let mut gte = Gte::new();
        // A CCW triangle: (0,0), (2,0), (0,2). Signed area sign distinguishes
        // winding. Pack SX|SY little-endian.
        gte.data[SXY0] = 0; // (0,0)
        gte.data[SXY1] = 2; // (2,0) -> SX=2 SY=0
        gte.data[SXY2] = 2 << 16; // (0,2) -> SX=0 SY=2
        gte.command(0x1400_0006); // NCLIP
        let mac0 = gte.data[MAC0] as i32;
        // SX0*SY1+SX1*SY2+SX2*SY0 - SX0*SY2-SX1*SY0-SX2*SY1
        // = 0 + 2*2 + 0 - 0 - 0 - 0 = 4
        assert_eq!(mac0, 4);
    }

    #[test]
    fn avsz3_averages_z() {
        let mut gte = Gte::new();
        gte.write_control(ZSF3 as u32, 0x0000_0155); // ~ 1/3 in 1/12-fixed ... use simple value
        gte.data[SZ1] = 100;
        gte.data[SZ2] = 200;
        gte.data[SZ3] = 300;
        gte.command(0x1580_002D); // AVSZ3
        let zsf3 = 0x155i64;
        let expect_mac0 = zsf3 * (100 + 200 + 300);
        assert_eq!(gte.data[MAC0] as i32 as i64, expect_mac0);
        assert_eq!(gte.data[OTZ], ((expect_mac0 >> 12).clamp(0, 0xFFFF)) as u32);
    }

    #[test]
    fn rtps_identity_matrix_translates() {
        let mut gte = Gte::new();
        // Identity rotation matrix RT (diagonal = 1 in integer terms uses 0x1000
        // for 1.0 with sf=1; but with sf=0 a value of 1 multiplies directly).
        // Use sf=1 (12-bit fraction): RT diagonal = 0x1000 (=1.0).
        gte.write_control(RT11_12 as u32, 0x0000_1000); // RT11=0x1000 RT12=0
        gte.write_control(RT13_21 as u32, 0x0000_0000); // RT13=0 RT21=0
        gte.write_control(RT22_23 as u32, 0x0000_1000); // RT22=0x1000 RT23=0
        gte.write_control(RT31_32 as u32, 0x0000_0000); // RT31=0 RT32=0
        gte.write_control(RT33 as u32, 0x0000_1000); // RT33=0x1000
        // Translation (0,0,0).
        // Vector V0 = (10, 20, 30).
        gte.data[VXY0] = (10u32 & 0xFFFF) | (20u32 << 16);
        gte.data[VZ0] = 30;
        // H and SZ default so division overflows; that's fine for IR check.
        gte.write_control(H as u32, 0x0000_0200);
        gte.command(0x0018_0001); // RTPS, sf=1
        // MAC = (RT*V)>>12 = (0x1000*10)>>12 = 10, etc.
        assert_eq!(gte.data[MAC1] as i32, 10);
        assert_eq!(gte.data[MAC2] as i32, 20);
        assert_eq!(gte.data[MAC3] as i32, 30);
        assert_eq!(sx16(gte.data[IR1]), 10);
        assert_eq!(sx16(gte.data[IR2]), 20);
        assert_eq!(sx16(gte.data[IR3]), 30);
        // SZ3 FIFO got 30.
        assert_eq!(gte.data[SZ3] & 0xFFFF, 30);
    }

    #[test]
    fn ir_saturation_sets_flag() {
        let mut gte = Gte::new();
        // RT11 huge, V huge -> MAC1 overflows IR's +7FFF, lm=0.
        gte.write_control(RT11_12 as u32, 0x0000_7FFF);
        gte.write_control(RT22_23 as u32, 0x0000_7FFF);
        gte.write_control(RT33 as u32, 0x0000_7FFF);
        gte.data[VXY0] = (0x7FFFu32 & 0xFFFF) | (0x7FFFu32 << 16);
        gte.data[VZ0] = 0x7FFF;
        gte.command(0x0018_0001); // RTPS sf=1
        // IR1 must be clamped to 0x7FFF and the IR1 saturation flag set.
        assert_eq!(sx16(gte.data[IR1]), 0x7FFF);
        assert_ne!(gte.control[FLAG] & FLAG_IR1_SAT, 0);
        // Master error flag set.
        assert_ne!(gte.control[FLAG] & (1 << 31), 0);
    }

    #[test]
    fn divide_basic_reciprocal() {
        let mut gte = Gte::new();
        // H=0x100, SZ3=0x200 -> H < 2*SZ3, depth ~ H/SZ3 * 1.0 in 1/65536? The
        // UNR returns (H/SZ3) in .16 fixed roughly. Just assert no overflow flag
        // and a sane value.
        gte.write_control(H as u32, 0x0000_0100);
        gte.data[SZ3] = 0x0200;
        let depth = gte.divide();
        // 0x100/0x200 = 0.5 in .16 fixed = 0x8000.
        assert!((0x7F00..=0x8100).contains(&depth), "depth={depth:#x}");
        assert_eq!(gte.control[FLAG] & FLAG_DIVIDE_OVF, 0);
    }

    #[test]
    fn divide_overflow_when_sz3_too_small() {
        let mut gte = Gte::new();
        gte.write_control(H as u32, 0x0000_FFFF);
        gte.data[SZ3] = 1; // H >= SZ3*2 -> overflow.
        let depth = gte.divide();
        assert_eq!(depth, 0x1FFFF);
        assert_ne!(gte.control[FLAG] & FLAG_DIVIDE_OVF, 0);
    }

    #[test]
    fn sqr_squares_ir() {
        let mut gte = Gte::new();
        gte.set_ir(1, 3);
        gte.set_ir(2, 4);
        gte.set_ir(3, 5);
        gte.command(0x0000_0028); // SQR sf=0
        assert_eq!(gte.data[MAC1] as i32, 9);
        assert_eq!(gte.data[MAC2] as i32, 16);
        assert_eq!(gte.data[MAC3] as i32, 25);
        assert_eq!(sx16(gte.data[IR1]), 9);
    }

    #[test]
    fn op_cross_product() {
        let mut gte = Gte::new();
        // Diagonal D = (1,1,1) with sf=0.
        gte.write_control(RT11_12 as u32, 1);
        gte.write_control(RT22_23 as u32, 1);
        gte.write_control(RT33 as u32, 1);
        gte.set_ir(1, 1);
        gte.set_ir(2, 2);
        gte.set_ir(3, 3);
        // OP: MAC1 = IR3*D2 - IR2*D3 = 3*1 - 2*1 = 1
        //     MAC2 = IR1*D3 - IR3*D1 = 1 - 3 = -2
        //     MAC3 = IR2*D1 - IR1*D2 = 2 - 1 = 1
        gte.command(0x0000_000C); // OP sf=0
        assert_eq!(gte.data[MAC1] as i32, 1);
        assert_eq!(gte.data[MAC2] as i32, -2);
        assert_eq!(gte.data[MAC3] as i32, 1);
    }

    #[test]
    fn rgb_fifo_push_saturates_and_keeps_code() {
        let mut gte = Gte::new();
        gte.data[RGBC] = 0x2A00_0000; // CODE = 0x2A
        // Drive a colour command that overflows components positive.
        gte.set_ir(1, 0x7FFF);
        gte.set_ir(2, 0x7FFF);
        gte.set_ir(3, 0x7FFF);
        gte.data[IR0] = 0; // no interpolation contribution past base
        gte.command(0x0000_0011); // INTPL sf=0
        // The pushed RGB2 should carry CODE in the high byte.
        assert_eq!((gte.data[RGB2] >> 24) & 0xFF, 0x2A);
        // Components saturate to 0xFF (R sat flag set).
        assert_eq!(gte.data[RGB2] & 0xFF, 0xFF);
        assert_ne!(gte.control[FLAG] & FLAG_R_SAT, 0);
    }

    #[test]
    fn mvmva_uses_selected_matrix_and_vector() {
        let mut gte = Gte::new();
        // Light matrix LLM diagonal = 2 (sf=0), vector IR (v=3).
        gte.write_control(L11_12 as u32, 2);
        gte.write_control(L22_23 as u32, 2);
        gte.write_control(L33 as u32, 2);
        gte.set_ir(1, 5);
        gte.set_ir(2, 6);
        gte.set_ir(3, 7);
        // mx=1 (LLM), v=3 (IR), cv=3 (none), sf=0, lm=0.
        let word = (1 << 17) | (3 << 15) | (3 << 13) | 0x12;
        gte.command(word);
        assert_eq!(gte.data[MAC1] as i32, 10);
        assert_eq!(gte.data[MAC2] as i32, 12);
        assert_eq!(gte.data[MAC3] as i32, 14);
    }

    #[test]
    fn flag_cleared_at_command_start() {
        let mut gte = Gte::new();
        gte.control[FLAG] = 0xFFFF_FFFF;
        // A clean NCLIP (no saturation) should reset FLAG to 0.
        gte.data[SXY0] = 0;
        gte.data[SXY1] = 0;
        gte.data[SXY2] = 0;
        gte.command(0x1400_0006); // NCLIP
        assert_eq!(gte.control[FLAG], 0);
    }
}
