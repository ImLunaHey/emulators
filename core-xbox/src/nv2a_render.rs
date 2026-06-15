//! Software 3D rasterizer for the NV2A — transform, interpolation, texturing.
//!
//! Self-contained rendering math used by the NV2A PGRAPH layer: a 4×4 transform
//! + viewport, vertex attributes (position/color/texcoord), perspective-correct
//! interpolation, texture sampling, and triangle rasterization into an RGBA
//! color surface (with an optional depth buffer). Operates on plain buffers +
//! parameters so it can be unit-tested without the rest of the GPU.
//!
//! STUB: the implementation lands separately. The surface here is the contract
//! the PGRAPH layer renders through.

/// A render target: an RGBA8888 (ARGB-in-u32) color buffer addressed by pitch.
pub struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

/// A vertex with the attributes the rasterizer interpolates.
#[derive(Clone, Copy, Default)]
pub struct Vert {
    pub pos: [f32; 4],   // clip/screen-space position (x, y, z, w)
    pub color: u32,      // ARGB diffuse
    pub uv: [f32; 2],    // texture coordinates
}

/// Placeholder so the module compiles before the rasterizer lands.
pub fn version() -> &'static str {
    "nv2a-render stub"
}
