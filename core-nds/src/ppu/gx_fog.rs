//! DS 3D engine per-pixel fog + edge marking (GBATEK §"3D Display Engine — Fog"
//! and §"— Edge Marking"). Ported from ../../ds-recomp/src/ppu/gx_fog.ts.
//!
//! Both passes happen during 2D composition, AFTER the GX framebuffer is built
//! but BEFORE it is promoted onto BG0.
//!
//! Fog: a pixel's depth Z maps via `FOG_TABLE` (32 × 7-bit density) into a 0..127
//! density; the polygon color is blended toward `FOG_COLOR` by that density.
//! Until a real Z buffer exists the caller passes Z = 0 per pixel.
//!
//! Edge marking: where a drawn pixel borders an undrawn pixel (4-neighbour), the
//! center is replaced with an edge color. The reference operates on a binary
//! "is drawn" mask (no polygon-IDs yet).

/// Apply fog to one BGR555 pixel. `color`'s bit 15 (drawn flag) is preserved.
/// `z` is a 0..0xFFFF depth (0 = nearest). `fog_table` is 32 × 7-bit density.
pub fn apply_fog(
    color: u16,
    z: u32,
    fog_table: &[u8; 32],
    fog_offset: u32,
    fog_color: u16,
) -> u16 {
    // Bucket (z - fogOffset) into one of 32 slots (16-bit z → >>11). Saturate
    // below the offset to slot 0 and above to slot 31.
    let rel_z = (z as i64) - (fog_offset as i64);
    let idx = if rel_z <= 0 {
        0
    } else {
        ((rel_z as u32) >> 11).min(31) as usize
    };
    let density = (fog_table[idx] & 0x7F) as u32;
    if density == 0 {
        return color;
    }

    let drawn_bit = color & 0x8000;
    let cr = (color & 0x1F) as u32;
    let cg = ((color >> 5) & 0x1F) as u32;
    let cb = ((color >> 10) & 0x1F) as u32;
    let fr = (fog_color & 0x1F) as u32;
    let fg = ((fog_color >> 5) & 0x1F) as u32;
    let fb = ((fog_color >> 10) & 0x1F) as u32;

    // density 0..127 → mix factor / 128.
    let d = density;
    let inv = 128 - d;
    let r = (cr * inv + fr * d) >> 7;
    let g = (cg * inv + fg * d) >> 7;
    let b = (cb * inv + fb * d) >> 7;
    drawn_bit | (((b << 10) | (g << 5) | r) as u16)
}

/// Apply edge marking to one pixel by sampling its 4-connected neighbours in a
/// flat `W×H` drawn mask (1 = drawn). If the center is drawn but any neighbour
/// is undrawn, replace the color with `edge_color` (drawn bit preserved).
#[allow(clippy::too_many_arguments)]
pub fn apply_edge_mark(
    color: u16,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    drawn_mask: &[u8],
    edge_color: u16,
) -> u16 {
    let idx = y * w + x;
    if drawn_mask[idx] == 0 {
        return color;
    }
    let left = x > 0 && drawn_mask[idx - 1] != 0;
    let right = x < w - 1 && drawn_mask[idx + 1] != 0;
    let up = y > 0 && drawn_mask[idx - w] != 0;
    let down = y < h - 1 && drawn_mask[idx + w] != 0;
    if left && right && up && down {
        return color;
    }
    (color & 0x8000) | (edge_color & 0x7FFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_zero_density_is_noop() {
        let table = [0u8; 32];
        let c = apply_fog(0x8000 | 0x1F, 0, &table, 0, 0x7C00);
        assert_eq!(c, 0x8000 | 0x1F);
    }

    #[test]
    fn fog_full_density_blends_toward_fog_color() {
        let mut table = [0u8; 32];
        table[0] = 127; // near-max density
        // color pure red, fog pure blue.
        let c = apply_fog(0x001F, 0, &table, 0, 0x7C00);
        // heavily fogged → blue dominates, red shrinks.
        assert!((c & 0x1F) < 0x1F);
        assert!((c >> 10) & 0x1F > 0);
    }

    #[test]
    fn edge_mark_interior_pixel_unchanged() {
        // 3x3 all drawn → center (1,1) has all neighbours drawn.
        let mask = [1u8; 9];
        let c = apply_edge_mark(0x1234, 1, 1, 3, 3, &mask, 0x7FFF);
        assert_eq!(c, 0x1234);
    }

    #[test]
    fn edge_mark_boundary_pixel_recolored() {
        // center drawn, left neighbour undrawn.
        let mut mask = [1u8; 9];
        mask[1 * 3 + 0] = 0; // (0,1) undrawn
        let c = apply_edge_mark(0x8000 | 0x1234, 1, 1, 3, 3, &mask, 0x03E0);
        assert_eq!(c, 0x8000 | 0x03E0);
    }
}
