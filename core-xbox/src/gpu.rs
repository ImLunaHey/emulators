//! Nvidia NV2A GPU — framebuffer stub.
//!
//! Built (as a stub) from the XboxDevWiki "NV2A" notes and the nouveau project's
//! reverse-engineered register documentation. The real NV2A is a GeForce3/4-class
//! part: PFIFO pulls a pushbuffer of methods from unified RAM, PGRAPH runs the
//! programmable vertex pipeline + register-combiner pixel pipeline, and PCRTC /
//! PRAMDAC scan a surface out of unified RAM to the video encoder. NONE of that
//! pipeline exists yet.
//!
//! For now this owns only an RGBA8888 host framebuffer so the wasm surface
//! compiles and the orchestrator's `run_frame` has something to present.
//! `render_frame` is a no-op clear. The Xbox's standard NTSC output is 640x480,
//! which we use as the default display size. The crash screen ([`crate::crash`])
//! draws straight into this framebuffer.

/// Default display geometry — Xbox NTSC 640x480. The host re-reads these each
/// present and sizes its canvas accordingly.
pub const DEFAULT_W: u16 = 640;
pub const DEFAULT_H: u16 = 480;

/// Maximum framebuffer the crash screen / display may use. Sized to the default
/// geometry; `render_frame` resizes the backing `Vec` if the display changes.
pub const MAX_PIXELS: usize = DEFAULT_W as usize * DEFAULT_H as usize;

/// The NV2A GPU stub: a host-facing RGBA8888 framebuffer plus the display
/// geometry. Real PFIFO/PGRAPH/PCRTC state lands in future modules.
pub struct Gpu {
    /// RGBA8888 framebuffer, `display_w * display_h` pixels (one `u32` each,
    /// packed so a little-endian byte view is R,G,B,A — see [`crate::crash`]).
    pub framebuffer: Vec<u32>,
    /// Current display width in pixels.
    pub display_w: u16,
    /// Current display height in pixels.
    pub display_h: u16,
    /// Completed frames rendered (one per [`Gpu::render_frame`]).
    pub frames: u32,
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpu {
    pub fn new() -> Self {
        let w = DEFAULT_W as usize;
        let h = DEFAULT_H as usize;
        Gpu {
            framebuffer: vec![0xFF00_0000; w * h], // opaque black (A=FF)
            display_w: DEFAULT_W,
            display_h: DEFAULT_H,
            frames: 0,
        }
    }

    /// Present a frame. A real NV2A would have PCRTC scan out the surface PGRAPH
    /// rendered into unified RAM; here we just clear to opaque black and bump the
    /// counter, keeping the framebuffer sized to the display geometry.
    pub fn render_frame(&mut self) {
        let want = (self.display_w as usize) * (self.display_h as usize);
        if self.framebuffer.len() != want {
            self.framebuffer.resize(want, 0xFF00_0000);
        }
        for px in self.framebuffer.iter_mut() {
            *px = 0xFF00_0000; // opaque black
        }
        self.frames = self.frames.wrapping_add(1);
    }

    /// The host-facing framebuffer slice (RGBA8888, `display_w * display_h`).
    pub fn frame(&self) -> &[u32] {
        &self.framebuffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_geometry_is_ntsc_640x480() {
        let gpu = Gpu::new();
        assert_eq!(gpu.display_w, 640);
        assert_eq!(gpu.display_h, 480);
        assert_eq!(gpu.frame().len(), 640 * 480);
    }

    #[test]
    fn render_frame_clears_and_counts() {
        let mut gpu = Gpu::new();
        gpu.framebuffer[0] = 0xDEAD_BEEF;
        gpu.render_frame();
        assert_eq!(gpu.frame()[0], 0xFF00_0000, "cleared to opaque black");
        assert_eq!(gpu.frames, 1);
    }

    #[test]
    fn render_frame_resizes_to_display_geometry() {
        let mut gpu = Gpu::new();
        gpu.display_w = 320;
        gpu.display_h = 240;
        gpu.render_frame();
        assert_eq!(gpu.frame().len(), 320 * 240);
    }
}
