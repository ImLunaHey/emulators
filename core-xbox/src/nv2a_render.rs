//! Software 3D rasterizer for the NV2A — transform, interpolation, texturing.
//!
//! Self-contained rendering math used by the NV2A PGRAPH layer: a 4×4 transform
//! + viewport, vertex attributes (position/color/texcoord), perspective-correct
//! interpolation, texture sampling, and triangle rasterization into an RGBA
//! color surface (with an optional depth buffer). Operates on plain buffers +
//! parameters so it can be unit-tested without the rest of the GPU.
//!
//! Pixel format: the color surface holds ARGB8888 packed as a little-endian
//! `u32` (`0xAARRGGBB`), i.e. bytes B, G, R, A — matching how `nv2a.rs` stores
//! guest framebuffer pixels.

/// A render target: an RGBA8888 (ARGB-in-u32) color buffer addressed by pitch.
pub struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline. If 0, defaults to `width * 4`.
    pub pitch: u32,
}

impl<'a> Target<'a> {
    fn row_pitch(&self) -> u32 {
        if self.pitch == 0 {
            self.width * 4
        } else {
            self.pitch
        }
    }

    #[inline]
    fn put(&mut self, x: u32, y: u32, argb: u32) {
        let off = (y * self.row_pitch() + x * 4) as usize;
        if off + 4 <= self.pixels.len() {
            self.pixels[off..off + 4].copy_from_slice(&argb.to_le_bytes());
        }
    }

    #[cfg(test)]
    fn get(&self, x: u32, y: u32) -> u32 {
        let off = (y * self.row_pitch() + x * 4) as usize;
        u32::from_le_bytes([
            self.pixels[off],
            self.pixels[off + 1],
            self.pixels[off + 2],
            self.pixels[off + 3],
        ])
    }
}

/// A vertex with the attributes the rasterizer interpolates.
#[derive(Clone, Copy, Default)]
pub struct Vert {
    pub pos: [f32; 4], // clip/screen-space position (x, y, z, w)
    pub color: u32,    // ARGB diffuse
    pub uv: [f32; 2],  // texture coordinates
}

impl Vert {
    pub fn new(pos: [f32; 4], color: u32, uv: [f32; 2]) -> Self {
        Self { pos, color, uv }
    }
}

// ---------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------

/// A 4×4 matrix stored column-major (matching D3D/NV2A constant layout).
///
/// Element `m[col * 4 + row]`. A point is transformed as `M * v` (column
/// vector), so `transform_point` computes `out[row] = Σ_col m[col*4+row]*v[col]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix4 {
    /// Column-major storage: `m[c * 4 + r]`.
    pub m: [f32; 16],
}

impl Default for Matrix4 {
    fn default() -> Self {
        Self::identity()
    }
}

impl Matrix4 {
    pub fn identity() -> Self {
        let mut m = [0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        Self { m }
    }

    /// Build from a column-major array.
    pub fn from_columns(m: [f32; 16]) -> Self {
        Self { m }
    }

    /// Build from a row-major array (handy for hand-written matrices in tests).
    pub fn from_rows(r: [f32; 16]) -> Self {
        let mut m = [0.0f32; 16];
        for row in 0..4 {
            for col in 0..4 {
                m[col * 4 + row] = r[row * 4 + col];
            }
        }
        Self { m }
    }

    #[inline]
    fn at(&self, row: usize, col: usize) -> f32 {
        self.m[col * 4 + row]
    }

    /// Matrix product `self * rhs` (apply `rhs` first, then `self`).
    pub fn mul(&self, rhs: &Matrix4) -> Matrix4 {
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += self.at(row, k) * rhs.at(k, col);
                }
                out[col * 4 + row] = sum;
            }
        }
        Matrix4 { m: out }
    }

    /// Transform a 4-component point (clip space output).
    pub fn transform_point(&self, v: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for row in 0..4 {
            let mut sum = 0.0;
            for col in 0..4 {
                sum += self.at(row, col) * v[col];
            }
            out[row] = sum;
        }
        out
    }

    /// A translation matrix.
    pub fn translation(x: f32, y: f32, z: f32) -> Matrix4 {
        let mut m = Self::identity();
        m.m[12] = x;
        m.m[13] = y;
        m.m[14] = z;
        m
    }

    /// A scale matrix.
    pub fn scale(x: f32, y: f32, z: f32) -> Matrix4 {
        let mut m = Self::identity();
        m.m[0] = x;
        m.m[5] = y;
        m.m[10] = z;
        m
    }
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/// Viewport mapping from NDC to screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub znear: f32,
    pub zfar: f32,
}

impl Viewport {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            znear: 0.0,
            zfar: 1.0,
        }
    }

    /// Map an NDC point (x,y,z in [-1,1]) to screen space (pixels + depth).
    /// Note y is flipped (NDC +y up → screen +y down).
    fn ndc_to_screen(&self, ndc: [f32; 3]) -> [f32; 3] {
        let sx = self.x + (ndc[0] * 0.5 + 0.5) * self.w;
        let sy = self.y + (1.0 - (ndc[1] * 0.5 + 0.5)) * self.h;
        let sz = self.znear + (ndc[2] * 0.5 + 0.5) * (self.zfar - self.znear);
        [sx, sy, sz]
    }
}

// ---------------------------------------------------------------------------
// Texture
// ---------------------------------------------------------------------------

/// Supported source texture formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexFormat {
    /// ARGB8888 packed little-endian (bytes B,G,R,A), the native surface format.
    Argb8888,
    /// XRGB8888 — like ARGB but alpha forced opaque.
    Xrgb8888,
}

/// UV addressing mode at sample time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode {
    Wrap,
    Clamp,
}

/// A sampleable texture: decoded to ARGB8888 (`0xAARRGGBB`) texels.
#[derive(Clone, Debug)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// Row-major ARGB8888 texels, length `width * height`.
    pub texels: Vec<u32>,
    pub wrap_u: WrapMode,
    pub wrap_v: WrapMode,
}

impl Texture {
    /// Build a texture directly from ARGB texels.
    pub fn from_texels(width: u32, height: u32, texels: Vec<u32>) -> Self {
        debug_assert_eq!(texels.len(), (width * height) as usize);
        Self {
            width,
            height,
            texels,
            wrap_u: WrapMode::Wrap,
            wrap_v: WrapMode::Wrap,
        }
    }

    /// Build a texture from a guest-RAM region.
    ///
    /// `data` is the raw bytes at the texture's base; `pitch` is bytes per row
    /// (0 ⇒ `width * bpp`). `swizzled` selects NV2A swizzled (Morton/Z-order)
    /// addressing for power-of-two textures vs. linear row-major.
    pub fn from_memory(
        data: &[u8],
        width: u32,
        height: u32,
        pitch: u32,
        format: TexFormat,
        swizzled: bool,
    ) -> Self {
        let bpp: u32 = 4;
        let row_pitch = if pitch == 0 { width * bpp } else { pitch };
        let mut texels = vec![0u32; (width * height) as usize];

        for y in 0..height {
            for x in 0..width {
                let src_off = if swizzled {
                    (swizzle_offset(x, y, width, height) * bpp) as usize
                } else {
                    (y * row_pitch + x * bpp) as usize
                };
                let mut argb = if src_off + 4 <= data.len() {
                    u32::from_le_bytes([
                        data[src_off],
                        data[src_off + 1],
                        data[src_off + 2],
                        data[src_off + 3],
                    ])
                } else {
                    0
                };
                if format == TexFormat::Xrgb8888 {
                    argb |= 0xFF00_0000;
                }
                texels[(y * width + x) as usize] = argb;
            }
        }

        Self::from_texels(width, height, texels)
    }

    #[inline]
    fn addr(coord: i32, size: u32, mode: WrapMode) -> u32 {
        let s = size as i32;
        match mode {
            WrapMode::Clamp => coord.clamp(0, s - 1) as u32,
            WrapMode::Wrap => coord.rem_euclid(s) as u32,
        }
    }

    /// Nearest-neighbour sample at normalized UV.
    pub fn sample_nearest(&self, u: f32, v: f32) -> u32 {
        if self.width == 0 || self.height == 0 {
            return 0xFFFF_FFFF;
        }
        // Texel centers are at (i + 0.5)/size; floor(u*size) selects the texel.
        let tx = (u * self.width as f32).floor() as i32;
        let ty = (v * self.height as f32).floor() as i32;
        let x = Self::addr(tx, self.width, self.wrap_u);
        let y = Self::addr(ty, self.height, self.wrap_v);
        self.texels[(y * self.width + x) as usize]
    }
}

/// Compute the linear byte index (in texels) of (x,y) in an NV2A swizzled
/// (Morton / Z-order) power-of-two texture. For non-power-of-two dimensions we
/// only interleave the bits that both axes share; the remainder is appended
/// linearly, matching the hardware's handling of rectangular textures.
fn swizzle_offset(x: u32, y: u32, width: u32, height: u32) -> u32 {
    let log_w = 31 - width.max(1).leading_zeros();
    let log_h = 31 - height.max(1).leading_zeros();
    let bits = log_w.min(log_h);

    let mut offset = 0u32;
    for i in 0..bits {
        offset |= ((x >> i) & 1) << (2 * i);
        offset |= ((y >> i) & 1) << (2 * i + 1);
    }
    // Bits beyond the square region pack linearly above the interleaved block.
    let masked = (1u32 << bits) - 1;
    let hi_x = x >> bits;
    let hi_y = y >> bits;
    offset |= (hi_x | (hi_y * (width >> bits).max(1))) << (2 * bits);
    let _ = masked;
    offset
}

// ---------------------------------------------------------------------------
// Primitive types and shading
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimType {
    Triangles,
    TriangleStrip,
    TriangleFan,
    Quads,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadeMode {
    /// Interpolate per-vertex ARGB color only.
    Gouraud,
    /// Sample the bound texture and modulate by the interpolated color.
    Textured,
}

/// How vertex positions are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpaceMode {
    /// Apply transform + perspective divide + viewport mapping.
    Transformed,
    /// `pos` is already in screen space (XYZRHW). `pos[3]` is RHW (1/w); if 0,
    /// it is treated as 1 (no perspective correction).
    PassThrough,
}

// ---------------------------------------------------------------------------
// Rasterizer
// ---------------------------------------------------------------------------

/// A vertex after transform, in screen space, with `1/w` carried for
/// perspective-correct interpolation.
#[derive(Clone, Copy)]
struct ScreenVert {
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
    // Color channels premultiplied by inv_w for perspective-correct interp.
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    u: f32,
    v: f32,
}

#[inline]
fn unpack(argb: u32) -> (f32, f32, f32, f32) {
    let a = ((argb >> 24) & 0xFF) as f32;
    let r = ((argb >> 16) & 0xFF) as f32;
    let g = ((argb >> 8) & 0xFF) as f32;
    let b = (argb & 0xFF) as f32;
    (a, r, g, b)
}

#[inline]
fn pack(a: f32, r: f32, g: f32, b: f32) -> u32 {
    let c = |v: f32| (v.round().clamp(0.0, 255.0)) as u32;
    (c(a) << 24) | (c(r) << 16) | (c(g) << 8) | c(b)
}

fn project(
    v: &Vert,
    space: SpaceMode,
    transform: Option<&Matrix4>,
    viewport: &Viewport,
) -> ScreenVert {
    let (a, r, g, b) = unpack(v.color);
    match space {
        SpaceMode::PassThrough => {
            let rhw = if v.pos[3] == 0.0 { 1.0 } else { v.pos[3] };
            ScreenVert {
                x: v.pos[0],
                y: v.pos[1],
                z: v.pos[2],
                inv_w: rhw,
                r: r * rhw,
                g: g * rhw,
                b: b * rhw,
                a: a * rhw,
                u: v.uv[0] * rhw,
                v: v.uv[1] * rhw,
            }
        }
        SpaceMode::Transformed => {
            let clip = match transform {
                Some(m) => m.transform_point(v.pos),
                None => v.pos,
            };
            let w = clip[3];
            let inv_w = if w != 0.0 { 1.0 / w } else { 1.0 };
            let ndc = [clip[0] * inv_w, clip[1] * inv_w, clip[2] * inv_w];
            let s = viewport.ndc_to_screen(ndc);
            ScreenVert {
                x: s[0],
                y: s[1],
                z: s[2],
                inv_w,
                r: r * inv_w,
                g: g * inv_w,
                b: b * inv_w,
                a: a * inv_w,
                u: v.uv[0] * inv_w,
                v: v.uv[1] * inv_w,
            }
        }
    }
}

#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// Top-left fill rule: a sample exactly on an edge is covered only if the edge
/// is a top or left edge of the triangle (assuming CCW winding in screen space,
/// where the area is positive).
#[inline]
fn is_top_left(ax: f32, ay: f32, bx: f32, by: f32) -> bool {
    // Edge from a->b. Top edge: horizontal and going left (bx < ax).
    // Left edge: going down (by > ay) in screen space (y grows downward).
    (ay == by && bx < ax) || (by > ay)
}

fn raster_tri(
    target: &mut Target,
    depth: &mut Option<&mut [f32]>,
    depth_enabled: bool,
    v0: &ScreenVert,
    v1: &ScreenVert,
    v2: &ScreenVert,
    mode: ShadeMode,
    texture: Option<&Texture>,
) {
    // Ensure CCW winding (positive area). If negative, swap two vertices.
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    let (v0, v1, v2) = if area < 0.0 {
        (v0, v2, v1)
    } else {
        (v0, v1, v2)
    };
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area == 0.0 {
        return; // degenerate
    }
    let inv_area = 1.0 / area;

    let min_x = v0.x.min(v1.x).min(v2.x).floor().max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).ceil().min(target.width as f32) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).floor().max(0.0) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).ceil().min(target.height as f32) as i32;

    // Fill-rule bias: edge opposite to each vertex.
    let bias0 = if is_top_left(v1.x, v1.y, v2.x, v2.y) { 0.0 } else { -f32::EPSILON };
    let bias1 = if is_top_left(v2.x, v2.y, v0.x, v0.y) { 0.0 } else { -f32::EPSILON };
    let bias2 = if is_top_left(v0.x, v0.y, v1.x, v1.y) { 0.0 } else { -f32::EPSILON };

    let dw = target.width;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let w0 = edge(v1.x, v1.y, v2.x, v2.y, px, py);
            let w1 = edge(v2.x, v2.y, v0.x, v0.y, px, py);
            let w2 = edge(v0.x, v0.y, v1.x, v1.y, px, py);

            if w0 + bias0 < 0.0 || w1 + bias1 < 0.0 || w2 + bias2 < 0.0 {
                continue;
            }

            let l0 = w0 * inv_area;
            let l1 = w1 * inv_area;
            let l2 = w2 * inv_area;

            // Interpolate 1/w then recover w for perspective-correct attrs.
            let inv_w = l0 * v0.inv_w + l1 * v1.inv_w + l2 * v2.inv_w;
            if inv_w == 0.0 {
                continue;
            }
            let w = 1.0 / inv_w;

            // Depth: screen-space z interpolated linearly (already post-divide).
            let z = l0 * v0.z + l1 * v1.z + l2 * v2.z;
            if depth_enabled {
                if let Some(buf) = depth.as_mut() {
                    let idx = (y as u32 * dw + x as u32) as usize;
                    if idx < buf.len() {
                        if z > buf[idx] {
                            continue; // less-equal test: farther → reject
                        }
                        buf[idx] = z;
                    }
                }
            }

            let r = (l0 * v0.r + l1 * v1.r + l2 * v2.r) * w;
            let g = (l0 * v0.g + l1 * v1.g + l2 * v2.g) * w;
            let b = (l0 * v0.b + l1 * v1.b + l2 * v2.b) * w;
            let a = (l0 * v0.a + l1 * v1.a + l2 * v2.a) * w;

            let argb = match mode {
                ShadeMode::Gouraud => pack(a, r, g, b),
                ShadeMode::Textured => {
                    let u = (l0 * v0.u + l1 * v1.u + l2 * v2.u) * w;
                    let v = (l0 * v0.v + l1 * v1.v + l2 * v2.v) * w;
                    let tex = match texture {
                        Some(t) => t.sample_nearest(u, v),
                        None => 0xFFFF_FFFF,
                    };
                    let (ta, tr, tg, tb) = unpack(tex);
                    // Modulate: tex * diffuse, both in [0,255].
                    pack(
                        ta * a / 255.0,
                        tr * r / 255.0,
                        tg * g / 255.0,
                        tb * b / 255.0,
                    )
                }
            };

            target.put(x as u32, y as u32, argb);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_impl(
    target: &mut Target,
    mut depth: Option<&mut [f32]>,
    verts: &[Vert],
    prim: PrimType,
    mode: ShadeMode,
    space: SpaceMode,
    transform: Option<&Matrix4>,
    viewport: &Viewport,
    texture: Option<&Texture>,
) {
    let depth_enabled = depth.is_some();
    let p = |v: &Vert| project(v, space, transform, viewport);

    let tri = |target: &mut Target, depth: &mut Option<&mut [f32]>, a: &Vert, b: &Vert, c: &Vert| {
        let (sa, sb, sc) = (p(a), p(b), p(c));
        raster_tri(target, depth, depth_enabled, &sa, &sb, &sc, mode, texture);
    };

    match prim {
        PrimType::Triangles => {
            let n = verts.len() / 3;
            for i in 0..n {
                tri(target, &mut depth, &verts[i * 3], &verts[i * 3 + 1], &verts[i * 3 + 2]);
            }
        }
        PrimType::TriangleStrip => {
            for i in 0..verts.len().saturating_sub(2) {
                // Alternate winding to keep a consistent front face.
                if i % 2 == 0 {
                    tri(target, &mut depth, &verts[i], &verts[i + 1], &verts[i + 2]);
                } else {
                    tri(target, &mut depth, &verts[i + 1], &verts[i], &verts[i + 2]);
                }
            }
        }
        PrimType::TriangleFan => {
            if verts.len() >= 3 {
                for i in 1..verts.len() - 1 {
                    tri(target, &mut depth, &verts[0], &verts[i], &verts[i + 1]);
                }
            }
        }
        PrimType::Quads => {
            let n = verts.len() / 4;
            for i in 0..n {
                let q = &verts[i * 4..i * 4 + 4];
                tri(target, &mut depth, &q[0], &q[1], &q[2]);
                tri(target, &mut depth, &q[0], &q[2], &q[3]);
            }
        }
    }
}

/// Draw geometry with full transform (modelview*projection) + viewport mapping.
///
/// `transform` is applied to each vertex position to produce clip space, which
/// is then perspective-divided and viewport-mapped. Pass `None` to skip the
/// matrix (vertices already in clip space).
#[allow(clippy::too_many_arguments)]
pub fn draw_triangles(
    target: &mut Target,
    depth: Option<&mut [f32]>,
    verts: &[Vert],
    prim: PrimType,
    mode: ShadeMode,
    transform: Option<&Matrix4>,
    viewport: &Viewport,
    texture: Option<&Texture>,
) {
    draw_impl(
        target,
        depth,
        verts,
        prim,
        mode,
        SpaceMode::Transformed,
        transform,
        viewport,
        texture,
    );
}

/// Draw pre-transformed (XYZRHW) geometry: `pos` is screen-space pixels and
/// `pos[3]` is RHW (1/w). No matrix or viewport mapping is applied. This is the
/// path our homebrew uses.
pub fn draw_triangles_screen(
    target: &mut Target,
    depth: Option<&mut [f32]>,
    verts: &[Vert],
    prim: PrimType,
    mode: ShadeMode,
    texture: Option<&Texture>,
) {
    // A dummy viewport (unused in pass-through).
    let vp = Viewport::new(0.0, 0.0, target.width as f32, target.height as f32);
    draw_impl(
        target,
        depth,
        verts,
        prim,
        mode,
        SpaceMode::PassThrough,
        None,
        &vp,
        texture,
    );
}

/// Placeholder retained for compatibility.
pub fn version() -> &'static str {
    "nv2a-render"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32) -> Vec<u8> {
        vec![0u8; (w * h * 4) as usize]
    }

    #[test]
    fn matrix_identity_and_mul() {
        let i = Matrix4::identity();
        let p = i.transform_point([1.0, 2.0, 3.0, 1.0]);
        assert_eq!(p, [1.0, 2.0, 3.0, 1.0]);

        let t = Matrix4::translation(5.0, 6.0, 7.0);
        let p = t.transform_point([1.0, 1.0, 1.0, 1.0]);
        assert_eq!(p, [6.0, 7.0, 8.0, 1.0]);

        // (scale then translate) applied via mul ordering: T * S
        let s = Matrix4::scale(2.0, 2.0, 2.0);
        let ts = t.mul(&s);
        let p = ts.transform_point([1.0, 1.0, 1.0, 1.0]);
        assert_eq!(p, [7.0, 8.0, 9.0, 1.0]); // 1*2 + 5, etc.

        // identity * m == m
        assert_eq!(i.mul(&t), t);
    }

    #[test]
    fn transformed_triangle_lands_on_screen() {
        // A triangle in NDC mapped through a 100x100 viewport with identity
        // transform (w=1). NDC (-1,-1)→(0,100), (1,-1)→(100,100), (0,1)→(50,0).
        let (w, h) = (100u32, 100u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };
        let vp = Viewport::new(0.0, 0.0, w as f32, h as f32);

        let verts = [
            Vert::new([-1.0, -1.0, 0.0, 1.0], 0xFFFF_0000, [0.0, 0.0]),
            Vert::new([1.0, -1.0, 0.0, 1.0], 0xFFFF_0000, [0.0, 0.0]),
            Vert::new([0.0, 1.0, 0.0, 1.0], 0xFFFF_0000, [0.0, 0.0]),
        ];
        draw_triangles(&mut t, None, &verts, PrimType::Triangles, ShadeMode::Gouraud, Some(&Matrix4::identity()), &vp, None);

        // Centroid ~ (50, 66) should be red.
        assert_eq!(t.get(50, 66), 0xFFFF_0000);
        // Top-left corner is outside the triangle.
        assert_eq!(t.get(2, 2), 0x0000_0000);
    }

    #[test]
    fn gouraud_interpolation_at_centroid() {
        // Screen-space right triangle, three pure colors at the corners.
        let (w, h) = (90u32, 90u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };

        let verts = [
            Vert::new([10.0, 10.0, 0.0, 1.0], 0xFFFF_0000, [0.0, 0.0]), // red
            Vert::new([70.0, 10.0, 0.0, 1.0], 0xFF00_FF00, [0.0, 0.0]), // green
            Vert::new([40.0, 70.0, 0.0, 1.0], 0xFF00_00FF, [0.0, 0.0]), // blue
        ];
        draw_triangles_screen(&mut t, None, &verts, PrimType::Triangles, ShadeMode::Gouraud, None);

        // Centroid = (40, 30): each weight = 1/3 ⇒ ~ (85,85,85).
        let c = t.get(40, 30);
        let r = (c >> 16) & 0xFF;
        let g = (c >> 8) & 0xFF;
        let b = c & 0xFF;
        // Pixel-center sampling at (40.5,30.5) shifts weights slightly off 1/3.
        assert!((r as i32 - 85).abs() <= 5, "r={r}");
        assert!((g as i32 - 85).abs() <= 5, "g={g}");
        assert!((b as i32 - 85).abs() <= 5, "b={b}");
        assert_eq!((c >> 24) & 0xFF, 0xFF);
    }

    #[test]
    fn passthrough_matches_direct_coords() {
        let (w, h) = (20u32, 20u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };

        // Big square covering most of the buffer (two screen-space tris).
        let verts = [
            Vert::new([2.0, 2.0, 0.0, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([18.0, 2.0, 0.0, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([18.0, 18.0, 0.0, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([2.0, 18.0, 0.0, 1.0], 0xFF00_FF00, [0.0, 0.0]),
        ];
        draw_triangles_screen(&mut t, None, &verts, PrimType::Quads, ShadeMode::Gouraud, None);

        assert_eq!(t.get(10, 10), 0xFF00_FF00);
        assert_eq!(t.get(0, 0), 0x0000_0000); // outside
    }

    #[test]
    fn texture_2x2_maps_to_quadrants() {
        // 2x2 texture: TL=red, TR=green, BL=blue, BR=white.
        let tex = Texture::from_texels(
            2,
            2,
            vec![0xFFFF_0000, 0xFF00_FF00, 0xFF00_00FF, 0xFFFF_FFFF],
        );

        let (w, h) = (40u32, 40u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };

        // Screen-space quad (0,0)-(40,40), UV (0,0)-(1,1). White diffuse so
        // modulation is identity.
        let verts = [
            Vert::new([0.0, 0.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([40.0, 0.0, 0.0, 1.0], 0xFFFF_FFFF, [1.0, 0.0]),
            Vert::new([40.0, 40.0, 0.0, 1.0], 0xFFFF_FFFF, [1.0, 1.0]),
            Vert::new([0.0, 40.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 1.0]),
        ];
        draw_triangles_screen(&mut t, None, &verts, PrimType::Quads, ShadeMode::Textured, Some(&tex));

        assert_eq!(t.get(10, 10), 0xFFFF_0000); // TL red
        assert_eq!(t.get(30, 10), 0xFF00_FF00); // TR green
        assert_eq!(t.get(10, 30), 0xFF00_00FF); // BL blue
        assert_eq!(t.get(30, 30), 0xFFFF_FFFF); // BR white
    }

    #[test]
    fn texture_modulated_by_diffuse() {
        let tex = Texture::from_texels(1, 1, vec![0xFFFF_FFFF]); // white texel
        let (w, h) = (10u32, 10u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };
        // Half-intensity red diffuse modulating a white texel ⇒ half red.
        let verts = [
            Vert::new([0.0, 0.0, 0.0, 1.0], 0xFF80_0000, [0.0, 0.0]),
            Vert::new([10.0, 0.0, 0.0, 1.0], 0xFF80_0000, [1.0, 0.0]),
            Vert::new([10.0, 10.0, 0.0, 1.0], 0xFF80_0000, [1.0, 1.0]),
            Vert::new([0.0, 10.0, 0.0, 1.0], 0xFF80_0000, [0.0, 1.0]),
        ];
        draw_triangles_screen(&mut t, None, &verts, PrimType::Quads, ShadeMode::Textured, Some(&tex));
        let c = t.get(5, 5);
        assert_eq!((c >> 16) & 0xFF, 0x80);
        assert_eq!(c & 0xFFFF, 0); // no green/blue
    }

    #[test]
    fn perspective_correct_uv() {
        // A triangle with strongly differing w at vertices. Compare perspective
        // correct interpolation vs. naive linear at the centroid.
        let (w, h) = (100u32, 100u32);
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };

        // 256x1 horizontal gradient texture: texel i = gray(i).
        let mut texels = Vec::with_capacity(256);
        for i in 0..256u32 {
            texels.push(0xFF00_0000 | (i << 16) | (i << 8) | i);
        }
        let tex = Texture::from_texels(256, 1, texels);

        // Pass-through with explicit RHW (1/w). Two close, one far vertex.
        // v0 near (rhw=1), v1 far (rhw=0.25), both with u spanning 0..1.
        let verts = [
            Vert::new([10.0, 10.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.5]),
            Vert::new([90.0, 10.0, 0.0, 0.25], 0xFFFF_FFFF, [1.0, 0.5]),
            Vert::new([50.0, 90.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.5]),
        ];
        draw_triangles_screen(&mut t, None, &verts, PrimType::Triangles, ShadeMode::Textured, Some(&tex));

        // At the midpoint of the top edge (50,10) the perspective-correct u is
        // pulled toward the near vertex (u≈0) vs. linear u=0.5. So the sampled
        // gray should be well below 128.
        let c = t.get(50, 12);
        let gray = (c >> 16) & 0xFF;
        assert!(gray < 100, "expected perspective-correct (dark), got {gray}");
    }

    #[test]
    fn depth_buffer_hides_farther_triangle() {
        let (w, h) = (20u32, 20u32);
        let mut px = blank(w, h);
        let mut depth = vec![f32::INFINITY; (w * h) as usize];
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };

        let near = [
            Vert::new([0.0, 0.0, 0.2, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([20.0, 0.0, 0.2, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([20.0, 20.0, 0.2, 1.0], 0xFF00_FF00, [0.0, 0.0]),
            Vert::new([0.0, 20.0, 0.2, 1.0], 0xFF00_FF00, [0.0, 0.0]),
        ];
        let far = [
            Vert::new([0.0, 0.0, 0.8, 1.0], 0xFFFF_0000, [0.0, 0.0]),
            Vert::new([20.0, 0.0, 0.8, 1.0], 0xFFFF_0000, [0.0, 0.0]),
            Vert::new([20.0, 20.0, 0.8, 1.0], 0xFFFF_0000, [0.0, 0.0]),
            Vert::new([0.0, 20.0, 0.8, 1.0], 0xFFFF_0000, [0.0, 0.0]),
        ];

        // Draw near first, then far: far must be rejected.
        draw_triangles_screen(&mut t, Some(&mut depth), &near, PrimType::Quads, ShadeMode::Gouraud, None);
        draw_triangles_screen(&mut t, Some(&mut depth), &far, PrimType::Quads, ShadeMode::Gouraud, None);
        assert_eq!(t.get(10, 10), 0xFF00_FF00, "near should survive");

        // Reverse: far first, near second: near overwrites (less-equal passes).
        let mut px2 = blank(w, h);
        let mut depth2 = vec![f32::INFINITY; (w * h) as usize];
        let mut t2 = Target { pixels: &mut px2, width: w, height: h, pitch: 0 };
        draw_triangles_screen(&mut t2, Some(&mut depth2), &far, PrimType::Quads, ShadeMode::Gouraud, None);
        draw_triangles_screen(&mut t2, Some(&mut depth2), &near, PrimType::Quads, ShadeMode::Gouraud, None);
        assert_eq!(t2.get(10, 10), 0xFF00_FF00, "near should overwrite far");
    }

    #[test]
    fn texture_wrap_and_clamp() {
        let mut tex = Texture::from_texels(2, 1, vec![0xFF11_1111, 0xFF22_2222]);
        // Wrap: u=1.5 → texel 1; u=-0.5 → texel 1 (rem_euclid).
        tex.wrap_u = WrapMode::Wrap;
        assert_eq!(tex.sample_nearest(1.25, 0.0), 0xFF11_1111); // 1.25*2=2.5→floor2→wrap0
        assert_eq!(tex.sample_nearest(-0.25, 0.0), 0xFF22_2222); // -0.5→floor-1→wrap1

        tex.wrap_u = WrapMode::Clamp;
        assert_eq!(tex.sample_nearest(5.0, 0.0), 0xFF22_2222); // clamp to last
        assert_eq!(tex.sample_nearest(-5.0, 0.0), 0xFF11_1111); // clamp to first
    }

    #[test]
    fn texture_from_memory_linear() {
        // 2x2 ARGB linear: bytes are little-endian per texel.
        let mut data = Vec::new();
        for argb in [0xFFAA_BBCCu32, 0xFF11_2233, 0xFF44_5566, 0xFF77_8899] {
            data.extend_from_slice(&argb.to_le_bytes());
        }
        let tex = Texture::from_memory(&data, 2, 2, 0, TexFormat::Argb8888, false);
        assert_eq!(tex.texels[0], 0xFFAA_BBCC);
        assert_eq!(tex.texels[3], 0xFF77_8899);

        // Xrgb forces alpha opaque.
        let mut d2 = Vec::new();
        d2.extend_from_slice(&0x0012_3456u32.to_le_bytes());
        let t2 = Texture::from_memory(&d2, 1, 1, 0, TexFormat::Xrgb8888, false);
        assert_eq!(t2.texels[0], 0xFF12_3456);
    }

    #[test]
    fn triangle_strip_and_fan() {
        let (w, h) = (40u32, 40u32);

        // Strip: 4 verts → 2 triangles forming a quad-ish band.
        let mut px = blank(w, h);
        let mut t = Target { pixels: &mut px, width: w, height: h, pitch: 0 };
        let strip = [
            Vert::new([5.0, 5.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([5.0, 35.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([35.0, 5.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([35.0, 35.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
        ];
        draw_triangles_screen(&mut t, None, &strip, PrimType::TriangleStrip, ShadeMode::Gouraud, None);
        assert_eq!(t.get(20, 20), 0xFFFF_FFFF);

        // Fan: center + ring.
        let mut px2 = blank(w, h);
        let mut t2 = Target { pixels: &mut px2, width: w, height: h, pitch: 0 };
        let fan = [
            Vert::new([20.0, 20.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([10.0, 10.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([30.0, 10.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
            Vert::new([30.0, 30.0, 0.0, 1.0], 0xFFFF_FFFF, [0.0, 0.0]),
        ];
        draw_triangles_screen(&mut t2, None, &fan, PrimType::TriangleFan, ShadeMode::Gouraud, None);
        assert_eq!(t2.get(22, 18), 0xFFFF_FFFF);
    }

    #[test]
    fn swizzle_offset_2x2() {
        // Z-order for 2x2: (0,0)=0,(1,0)=1,(0,1)=2,(1,1)=3.
        assert_eq!(swizzle_offset(0, 0, 2, 2), 0);
        assert_eq!(swizzle_offset(1, 0, 2, 2), 1);
        assert_eq!(swizzle_offset(0, 1, 2, 2), 2);
        assert_eq!(swizzle_offset(1, 1, 2, 2), 3);
        // 4x4 sanity: (2,1) → x=10b,y=01b interleaved = x0 y0 x1 y1... bits:
        // i0: x0=0,y0=1 → bit1; i1: x1=1,y1=0 → bit2 → 0b0110 = 6.
        assert_eq!(swizzle_offset(2, 1, 4, 4), 6);
    }
}
