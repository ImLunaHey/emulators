//! DS 2D PPU (engines A/B, background and sprite renderers) plus the 3D GPU
//! geometry engine (`gx`, with `gx_lighting` + `gx_fog`).

pub mod affine_bg;
pub mod bitmap_bg;
pub mod engine_a;
pub mod gx;
pub mod gx_fog;
pub mod gx_lighting;
pub mod ppu;
pub mod sprites;
pub mod text_bg;
