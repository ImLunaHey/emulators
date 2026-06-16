//! DS 3D engine per-vertex Gouraud lighting (GBATEK §"3D Display Engine —
//! Lighting"). Ported from ../../ds-recomp/src/ppu/gx_lighting.ts.
//!
//! The vertex-shader stage runs when `NORMAL` (cmd 0x21) is issued: the normal
//! is transformed by the vector matrix, then each of the four hardware lights —
//! if enabled by `POLYGON_ATTR` bits 0..3 — contributes a diffuse + ambient
//! term. The lit color becomes the current vertex color used by subsequent
//! `VTX_*` commands.
//!
//! The DS color space is BGR555 (5 bits/channel, 0..31). Diffuse * lightColor is
//! `(a * b) >> 5` so the product stays in 0..31.
//!
//! Fixed-point note: the TS reference stored light + normal vectors as floats in
//! `[-1, +1)` (Q1.9 / 512). Here the hardware is fixed-point, so vectors are kept
//! as `i32` in Q12 (4096 = 1.0). The dot product accumulates in `i64` then shifts
//! back to Q12. Specular is intentionally NOT modelled (matches the TS).

/// Number of hardware lights.
pub const NUM_LIGHTS: usize = 4;

/// Fixed-point fractional bits used for the light/normal vectors (Q12).
pub const FP_SHIFT: u32 = 12;
/// 1.0 in the Q12 vector space.
pub const FP_ONE: i32 = 1 << FP_SHIFT;

/// The four lights' direction vectors + emission colors. Each direction points
/// FROM the surface TO the light (a direction, not a position); the diffuse term
/// is `max(0, -dot(normal, lightVec))`.
#[derive(Clone)]
pub struct LightState {
    /// 4 lights × (x, y, z) in Q12.
    pub vectors: [[i32; 3]; NUM_LIGHTS],
    /// 4 BGR555 colors.
    pub colors: [u16; NUM_LIGHTS],
}

impl Default for LightState {
    fn default() -> Self {
        LightState {
            vectors: [[0; 3]; NUM_LIGHTS],
            colors: [0; NUM_LIGHTS],
        }
    }
}

impl LightState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cmd 0x32 `LIGHT_VECTOR`. Param layout (GBATEK):
    ///   bits 0..9 = X, 10..19 = Y, 20..29 = Z (10-bit signed Q1.9),
    ///   bits 30..31 = light index. The vector is already transformed by the
    ///   vector matrix upstream (gx passes the transformed value); here we just
    ///   store the Q1.9 unpack widened to Q12.
    pub fn set_vector(&mut self, packed: u32) {
        let idx = ((packed >> 30) & 0x3) as usize;
        // signExtend10(v) / 512 in the TS → widen Q1.9 into Q12 (×8).
        self.vectors[idx][0] = q9_to_q12(packed & 0x3FF);
        self.vectors[idx][1] = q9_to_q12((packed >> 10) & 0x3FF);
        self.vectors[idx][2] = q9_to_q12((packed >> 20) & 0x3FF);
    }

    /// Cmd 0x33 `LIGHT_COLOR`: bits 0..14 = BGR555, bits 30..31 = light index.
    pub fn set_color(&mut self, packed: u32) {
        let idx = ((packed >> 30) & 0x3) as usize;
        self.colors[idx] = (packed & 0x7FFF) as u16;
    }
}

/// Material colors for the lighting equation. The "use as vertex color" bit
/// (DIF_AMB bit 15) is mirrored into `set_vertex_color`; the "use shininess
/// table" bit (SPE_EMI bit 15) is ignored (no specular).
#[derive(Clone, Copy, Default)]
pub struct MaterialState {
    pub diffuse: u16,
    pub ambient: u16,
    pub specular: u16,
    pub emission: u16,
    /// DIF_AMB bit 15: also latch diffuse as the current vertex color.
    pub set_vertex_color: bool,
}

impl MaterialState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cmd 0x30 `DIF_AMB`: bits 0..14 diffuse, bit 15 set-vertex-color,
    /// bits 16..30 ambient.
    pub fn set_dif_amb(&mut self, packed: u32) {
        self.diffuse = (packed & 0x7FFF) as u16;
        self.ambient = ((packed >> 16) & 0x7FFF) as u16;
        self.set_vertex_color = (packed & 0x8000) != 0;
    }

    /// Cmd 0x31 `SPE_EMI`: bits 0..14 specular, bits 16..30 emission.
    pub fn set_spe_emi(&mut self, packed: u32) {
        self.specular = (packed & 0x7FFF) as u16;
        self.emission = ((packed >> 16) & 0x7FFF) as u16;
    }
}

/// Sign-extend a 10-bit Q1.9 field and widen to Q12 (the vector space used by
/// the dot product). 1.0 in Q1.9 = 512; ×8 lifts it to Q12 (4096).
#[inline]
fn q9_to_q12(v: u32) -> i32 {
    let s = ((v & 0x1FF) as i32) - ((v & 0x200) as i32);
    s << (FP_SHIFT - 9)
}

/// Cmd 0x21 `NORMAL` parameter unpack: three 10-bit signed Q1.9 components,
/// returned in Q12.
#[inline]
pub fn unpack_normal(packed: u32) -> [i32; 3] {
    [
        q9_to_q12(packed & 0x3FF),
        q9_to_q12((packed >> 10) & 0x3FF),
        q9_to_q12((packed >> 20) & 0x3FF),
    ]
}

/// Compute the lit vertex color for the given surface normal (in Q12, already
/// transformed into the light-vector space). `polygon_attr` bits 0..3 enable
/// lights 0..3. Returns BGR555 (0..0x7FFF). Mirrors the TS `computeVertexColor`.
pub fn compute_vertex_color(
    normal: [i32; 3],
    polygon_attr: u32,
    material: &MaterialState,
    lights: &LightState,
) -> u16 {
    // Start from emission. Each channel is 0..31.
    let mut r = (material.emission & 0x1F) as i32;
    let mut g = ((material.emission >> 5) & 0x1F) as i32;
    let mut b = ((material.emission >> 10) & 0x1F) as i32;

    let d_r = (material.diffuse & 0x1F) as i32;
    let d_g = ((material.diffuse >> 5) & 0x1F) as i32;
    let d_b = ((material.diffuse >> 10) & 0x1F) as i32;
    let a_r = (material.ambient & 0x1F) as i32;
    let a_g = ((material.ambient >> 5) & 0x1F) as i32;
    let a_b = ((material.ambient >> 10) & 0x1F) as i32;

    for i in 0..NUM_LIGHTS {
        if (polygon_attr & (1 << i)) == 0 {
            continue;
        }
        let lv = lights.vectors[i];
        let lc = lights.colors[i];
        let l_r = (lc & 0x1F) as i32;
        let l_g = ((lc >> 5) & 0x1F) as i32;
        let l_b = ((lc >> 10) & 0x1F) as i32;

        // Diffuse: max(0, -dot(normal, lightVec)). Both operands are Q12, so the
        // i64 product is Q24; shift back to Q12.
        let dot: i64 = (normal[0] as i64) * (lv[0] as i64)
            + (normal[1] as i64) * (lv[1] as i64)
            + (normal[2] as i64) * (lv[2] as i64);
        let mut diff = -(dot >> FP_SHIFT); // Q12
        if diff < 0 {
            diff = 0;
        }
        // diff is Q12 in [0, ~1]; scale to a 0..31 factor (×32 then >>12).
        let diff_scaled = ((diff * 32) >> FP_SHIFT).min(31) as i32;

        r += ((a_r * l_r) >> 5) + ((((d_r * l_r) >> 5) * diff_scaled) >> 5);
        g += ((a_g * l_g) >> 5) + ((((d_g * l_g) >> 5) * diff_scaled) >> 5);
        b += ((a_b * l_b) >> 5) + ((((d_b * l_b) >> 5) * diff_scaled) >> 5);
    }

    let clamp = |c: i32| c.clamp(0, 31) as u16;
    ((clamp(b) << 10) | (clamp(g) << 5) | clamp(r)) & 0x7FFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_vector_index_and_sign() {
        let mut s = LightState::new();
        // light 2, X = -1 (Q1.9 0x200 = -512), Y = +1 (511 ≈ 0.998), Z = 0.
        let packed = (2u32 << 30) | (0x200) | (0x1FF << 10);
        s.set_vector(packed);
        assert_eq!(s.vectors[2][0], -FP_ONE); // -1.0 in Q12
        assert!(s.vectors[2][1] > 0);
        assert_eq!(s.vectors[2][2], 0);
    }

    #[test]
    fn dif_amb_sets_vertex_color_flag() {
        let mut m = MaterialState::new();
        m.set_dif_amb(0x8000 | 0x1F | (0x03E0 << 16));
        assert_eq!(m.diffuse, 0x1F);
        assert_eq!(m.ambient, 0x03E0);
        assert!(m.set_vertex_color);
    }

    #[test]
    fn no_enabled_lights_returns_emission() {
        let m = MaterialState {
            emission: 0x03E0, // pure green
            ..Default::default()
        };
        let l = LightState::new();
        let c = compute_vertex_color([0, 0, FP_ONE], 0, &m, &l);
        assert_eq!(c, 0x03E0);
    }

    #[test]
    fn frontlit_diffuse_adds_light() {
        // Normal +Z, light vector -Z (points from surface toward +Z light), so
        // -dot = +1 → full diffuse.
        let mut l = LightState::new();
        l.vectors[0] = [0, 0, -FP_ONE];
        l.colors[0] = 0x001F; // red light
        let m = MaterialState {
            diffuse: 0x001F, // red material
            ..Default::default()
        };
        let c = compute_vertex_color([0, 0, FP_ONE], 0x1, &m, &l);
        // Red channel should be lit; green/blue stay 0.
        assert!(c & 0x1F > 0);
        assert_eq!((c >> 5) & 0x1F, 0);
    }
}
