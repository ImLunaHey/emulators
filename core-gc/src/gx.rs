//! Flipper GPU — framebuffer stub.
//!
//! Built (as a stub) from YAGCD §5.11 ("Graphics") and §8 ("GX"). The real
//! Flipper is an ATI/ArtX fixed-function + register-combiner GPU: a Command
//! Processor (CP) reads a FIFO of display lists from main RAM, the Transform
//! unit (XF) does vertex lighting/transform, the Texture/TEV stages do the
//! shading, and the Pixel Engine (PE) writes the embedded framebuffer (EFB),
//! which the Video Interface (VI) later scans out via "copy" / XFB. NONE of that
//! pipeline exists yet.
//!
//! For now this owns only an RGBA8888 host framebuffer so the wasm surface
//! compiles and the orchestrator's `run_frame` has something to present.
//! `render_frame` is a no-op clear. The GameCube's standard NTSC output is
//! 640x480 (interlaced), which we use as the default display size.

/// Default display geometry — GameCube NTSC 640x480 (YAGCD §5.6, Video
/// Interface). The host re-reads these each present and sizes its canvas.
pub const DEFAULT_W: u16 = 640;
pub const DEFAULT_H: u16 = 480;

/// The Flipper GPU stub: a host-facing RGBA8888 framebuffer plus the display
/// geometry. Real CP/XF/TEV/PE/EFB/VI state lands in future modules.
pub struct Gx {
    /// RGBA8888 framebuffer, `display_w * display_h` pixels (one `u32` each,
    /// packed `0xAABBGGRR` little-endian-in-memory so a byte view is R,G,B,A).
    framebuffer: Vec<u32>,
    /// Current display width in pixels.
    pub display_w: u16,
    /// Current display height in pixels.
    pub display_h: u16,
    /// Completed frames rendered (one per [`Gx::render_frame`]).
    pub frames: u32,
}

impl Default for Gx {
    fn default() -> Self {
        Self::new()
    }
}

impl Gx {
    pub fn new() -> Self {
        let w = DEFAULT_W as usize;
        let h = DEFAULT_H as usize;
        Gx {
            framebuffer: vec![0xFF00_0000; w * h], // opaque black (A=FF)
            display_w: DEFAULT_W,
            display_h: DEFAULT_H,
            frames: 0,
        }
    }

    /// Present a frame. A real Flipper would have the VI scan out the XFB the PE
    /// copied from the EFB; here we just clear to opaque black and bump the
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
        let gx = Gx::new();
        assert_eq!(gx.display_w, 640);
        assert_eq!(gx.display_h, 480);
        assert_eq!(gx.frame().len(), 640 * 480);
    }

    #[test]
    fn render_frame_clears_and_counts() {
        let mut gx = Gx::new();
        gx.framebuffer[0] = 0xDEAD_BEEF;
        gx.render_frame();
        assert_eq!(gx.frame()[0], 0xFF00_0000, "cleared to opaque black");
        assert_eq!(gx.frames, 1);
    }

    #[test]
    fn render_frame_resizes_to_display_geometry() {
        let mut gx = Gx::new();
        gx.display_w = 320;
        gx.display_h = 240;
        gx.render_frame();
        assert_eq!(gx.frame().len(), 320 * 240);
    }
}
