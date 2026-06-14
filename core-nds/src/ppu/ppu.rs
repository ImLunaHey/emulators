//! DS 2D PPU coordinator. Ported/adapted from ../../ds-recomp/src/ppu/ppu.ts
//! and the GBA core's `Ppu` (../../core/src/ppu.rs).
//!
//! This is the scanline-timing + IO + framebuffer SKELETON: it owns the VRAMCNT
//! bank-control registers (moved off `Nds`), both 2D `Engine`s (A = main, B =
//! sub), the dot/scanline clock, DISPSTAT/VCOUNT, the vblank/hblank/vcount IRQ
//! raising (via the core `Irq`), and the TWO 256x192 RGBA8888 framebuffers.
//! POWCNT1 bit 15 decides which engine drives the TOP vs BOTTOM screen.
//!
//! The geometry (3D) engine is DEFERRED: where Engine A would composite the 3D
//! layer onto BG0 in display mode 1 with a 3D BG, the renderer wave leaves a
//! transparent/black stub (TODO in engine_a.rs / the BG0 path).
//!
//! Ownership model (CONTRACT.md): the TS `Ppu` held `irq9`/`irq7`/`dma9`/`dma7`
//! and reached `this.mem.*`. Here those are PARAMETERS. `step` takes `&mut Irq`
//! (ARM9) + `&mut Irq` (ARM7) and reports HBlank/VBlank DMA triggers back via
//! `PpuTick` (the orchestrator fires DMA after `step` returns, exactly like the
//! GBA core, to avoid re-entering the bus while the PPU is borrowed). Frame
//! rendering reads VRAM/PRAM/OAM through borrowed `&SharedMemory` + the VRAM
//! `&VramRouter` + `vramcnt` — never an `&mut Nds`.

use super::engine_a::{Engine, EngineKind};
use crate::io::irq::{Irq, IRQ_HBLANK, IRQ_VBLANK, IRQ_VCOUNT};
use crate::memory::{SharedMemory, VramRouter};

// ─── DS scanline timing (GBATEK §"DS Video") ─────────────────────────────────
/// Dots per scanline (256 visible + 99 H-blank).
pub const DOTS_PER_LINE: u32 = 355;
/// Scanlines per frame (192 visible + 71 V-blank).
pub const LINES_PER_FRAME: u32 = 263;
/// Visible scanlines.
pub const VISIBLE_LINES: u32 = 192;
/// Visible dots before the H-blank flag asserts.
pub const VISIBLE_DOTS: u32 = 256;

/// Screen dimensions (each of the two screens is 256x192).
pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 192;
/// Bytes in one RGBA8888 framebuffer.
pub const FB_BYTES: usize = SCREEN_W * SCREEN_H * 4;

/// What one `Ppu::step` produced that the orchestrator must act on: whether an
/// HBlank- and/or VBlank-timed DMA should fire this tick (the PPU can't re-enter
/// the bus to run DMA itself — it's borrowed). Mirrors the GBA core's tick.
#[derive(Default, Clone, Copy)]
pub struct PpuTick {
    pub hblank: bool,
    pub vblank: bool,
}

pub struct Ppu {
    /// VRAMCNT_A..I bank-control registers (moved here from `Nds` — they are
    /// PPU state). The bus VRAM router takes these as `&[u8; 9]` on every
    /// access; the renderer consults them for bank bases + extended palettes.
    pub vramcnt: [u8; 9],

    /// Engine A (main) — all features.
    pub engine_a: Engine,
    /// Engine B (sub) — no large-bitmap mode 6, no display capture, no 3D.
    pub engine_b: Engine,

    /// POWCNT1 (0x04000304). Bit 15 = "display swap" (which engine → top
    /// screen). Owned by the PPU because it selects the framebuffer mapping;
    /// the rest of POWCNT1's graphics-power bits also live here.
    pub powcnt1: u32,

    // ─── Scanline state ───────────────────────────────────────────────────
    /// Dots elapsed into the current scanline.
    pub dots_accum: u32,
    /// Current scanline (0..LINES_PER_FRAME).
    pub vcount: u32,
    /// DISPSTAT: bit0 VBlank, bit1 HBlank, bit2 VCount-match, bit3 VBlank-IRQ
    /// enable, bit4 HBlank-IRQ enable, bit5 VCount-IRQ enable, bits 8/7..15
    /// VCount target (DS has a 9-bit target: bit 7 of byte 1 is the high bit).
    pub dispstat: u32,
    /// Total frames rendered.
    pub frame_count: u32,
    /// Set when a frame completed this `step` (the host samples + clears it).
    pub frame_done: bool,

    // ─── Framebuffers (RGBA8888, 256x192 each) ───────────────────────────
    /// Engine A's composited output.
    fb_a: Box<[u8; FB_BYTES]>,
    /// Engine B's composited output.
    fb_b: Box<[u8; FB_BYTES]>,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn boxed_fb() -> Box<[u8; FB_BYTES]> {
    vec![0u8; FB_BYTES].into_boxed_slice().try_into().unwrap()
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            vramcnt: [0; 9],
            engine_a: Engine::new(EngineKind::A),
            engine_b: Engine::new(EngineKind::B),
            powcnt1: 0x820F, // typical post-BIOS default (LCD + both engines on)
            dots_accum: 0,
            vcount: 0,
            dispstat: 0,
            frame_count: 0,
            frame_done: false,
            fb_a: boxed_fb(),
            fb_b: boxed_fb(),
        }
    }

    // ─── Framebuffer API (what the host FrameSource blits) ────────────────
    //
    // POWCNT1 bit 15 swaps which engine drives which physical screen. When
    // clear, Engine A → top, Engine B → bottom; when set, they swap. The host
    // calls `top_framebuffer()` / `bottom_framebuffer()`; each returns a
    // 256x192 RGBA8888 slice (FB_BYTES long).

    /// The 256x192 RGBA8888 framebuffer for the TOP screen.
    ///
    /// POWCNT1 bit 15 (GBATEK "Display Swap"): when SET, Engine A → top screen /
    /// Engine B → bottom; when CLEAR, Engine A → bottom / Engine B → top.
    pub fn top_framebuffer(&self) -> &[u8] {
        if (self.powcnt1 & 0x8000) != 0 {
            &self.fb_a[..]
        } else {
            &self.fb_b[..]
        }
    }

    /// The 256x192 RGBA8888 framebuffer for the BOTTOM screen (see
    /// `top_framebuffer` for the POWCNT1 bit-15 swap semantics).
    pub fn bottom_framebuffer(&self) -> &[u8] {
        if (self.powcnt1 & 0x8000) != 0 {
            &self.fb_b[..]
        } else {
            &self.fb_a[..]
        }
    }

    /// Direct access to engine A's raw framebuffer (debug / capture).
    pub fn engine_a_framebuffer(&self) -> &[u8] {
        &self.fb_a[..]
    }
    /// Direct access to engine B's raw framebuffer (debug / capture).
    pub fn engine_b_framebuffer(&self) -> &[u8] {
        &self.fb_b[..]
    }

    // ─── DISPSTAT / VCOUNT IO ─────────────────────────────────────────────

    /// Read DISPSTAT (0x04000004). The status bits (0..2) are recomputed from
    /// the live scanline state; the enable + target bits (3..15) read back what
    /// was written. (The TS kept them all in `dispstat`; we mirror that — the
    /// `step` path already maintains bits 0..2 in `self.dispstat`.)
    pub fn read_dispstat(&self) -> u32 {
        self.dispstat & 0xFFFF
    }
    /// Write DISPSTAT — only the enable bits (3..5) and the VCount target
    /// (bits 7..15) are writable; status bits 0..2 are read-only.
    pub fn write_dispstat(&mut self, v: u32) {
        self.dispstat = (self.dispstat & 0x0007) | (v & 0xFFF8);
    }
    /// Read VCOUNT (0x04000006) — the current scanline (9-bit).
    pub fn read_vcount(&self) -> u32 {
        self.vcount & 0x1FF
    }

    // ─── POWCNT1 IO ───────────────────────────────────────────────────────
    pub fn read_powcnt1(&self) -> u32 {
        self.powcnt1
    }
    pub fn write_powcnt1(&mut self, v: u32) {
        self.powcnt1 = v;
    }

    // ─── Engine register routing (0x04000xxx engine A, 0x04001xxx engine B) ─
    //
    // The Nds IO dispatch funnels the PPU register bytes here. `off` is the
    // 0x04000000-relative offset; engine B's mirror at 0x04001000..0x0400106F
    // is dispatched to `engine_b` after subtracting 0x1000.

    /// Read one byte of an engine register block. `engine_b` selects the sub
    /// engine; `block_off` is the block-relative offset (0x00 = DISPCNT).
    pub fn read_engine_reg8(&self, engine_b: bool, block_off: u32) -> u32 {
        if engine_b {
            self.engine_b.read_reg8(block_off)
        } else {
            self.engine_a.read_reg8(block_off)
        }
    }
    /// Write one byte of an engine register block.
    pub fn write_engine_reg8(&mut self, engine_b: bool, block_off: u32, v: u32) {
        if engine_b {
            self.engine_b.write_reg8(block_off, v)
        } else {
            self.engine_a.write_reg8(block_off, v)
        }
    }

    // ─── Scanline clock ───────────────────────────────────────────────────
    //
    // Advance the PPU by `cycles` ARM9 cycles (1 cycle = 1 dot, matching the
    // TS's simplified dot clock). Drives line transitions, the HBlank flag at
    // dot 256, the HBlank/VBlank/VCount IRQs, the per-frame affine re-latch,
    // and (once per VBlank) the full frame render. Returns a `PpuTick` telling
    // the orchestrator which DMA triggers to fire (it can't run DMA here —
    // the bus would alias the borrowed PPU). At most one HBlank + one VBlank
    // transition occur per call when the emulator batches to the line boundary.
    //
    // `irq9`/`irq7` are the two cores' interrupt controllers (the TS held them
    // as `this.irq9`/`this.irq7`). `mem`/`router` are needed for the frame
    // render at VBlank.
    pub fn step(
        &mut self,
        cycles: u32,
        irq9: &mut Irq,
        irq7: &mut Irq,
        mem: &SharedMemory,
        router: &VramRouter,
    ) -> PpuTick {
        let mut tick = PpuTick::default();
        self.dots_accum = self.dots_accum.wrapping_add(cycles);
        while self.dots_accum >= DOTS_PER_LINE {
            self.dots_accum -= DOTS_PER_LINE;
            self.end_line(irq9, irq7, mem, router, &mut tick);
        }
        // Mid-line HBlank flag: asserts from dot 256 onward.
        if self.dots_accum >= VISIBLE_DOTS {
            if (self.dispstat & 0x02) == 0 {
                self.dispstat |= 0x02;
                if (self.dispstat & 0x10) != 0 {
                    irq9.raise(IRQ_HBLANK);
                }
                tick.hblank = true;
            }
        } else {
            self.dispstat &= !0x02;
        }
        tick
    }

    /// End-of-scanline bookkeeping: advance VCOUNT, raise VBlank at line 192,
    /// re-latch affine refs + render the frame at VBlank start, and update the
    /// VCount-match flag/IRQ. Ported from ppu.ts `endLine`.
    fn end_line(
        &mut self,
        irq9: &mut Irq,
        irq7: &mut Irq,
        mem: &SharedMemory,
        router: &VramRouter,
        tick: &mut PpuTick,
    ) {
        self.vcount = (self.vcount + 1) % LINES_PER_FRAME;

        if self.vcount == VISIBLE_LINES {
            // Enter VBlank.
            self.dispstat |= 0x01;
            if (self.dispstat & 0x08) != 0 {
                irq9.raise(IRQ_VBLANK);
            }
            // ARM7 typically wants VBlank unconditionally (its handler drives
            // the per-frame housekeeping; see ppu.ts comment).
            irq7.raise(IRQ_VBLANK);
            tick.vblank = true;

            // Latch affine references for the new frame, then composite both
            // engines into their framebuffers. (Real hardware latches at the
            // start of each frame; we composite the whole frame in one shot at
            // VBlank, so latch immediately before rendering.)
            self.engine_a.latch_affine_refs();
            self.engine_b.latch_affine_refs();
            self.render_frame(mem, router);

            self.frame_count = self.frame_count.wrapping_add(1);
            self.frame_done = true;
        } else if self.vcount == 0 {
            // VBlank ends at wrap to line 0.
            self.dispstat &= !0x01;
        }

        // VCount match. DS target is 9-bit: bits 8..15 of DISPSTAT plus bit 7
        // (= the DISPSTAT "byte 0 bit 7" high bit) — but the common low-8-bit
        // path matches the TS, which used `(dispstat >> 8) & 0xFF`. Include the
        // 9th bit for completeness.
        let target = ((self.dispstat >> 8) & 0xFF) | (((self.dispstat >> 7) & 1) << 8);
        if self.vcount == target {
            self.dispstat |= 0x04;
            if (self.dispstat & 0x20) != 0 {
                irq9.raise(IRQ_VCOUNT);
            }
        } else {
            self.dispstat &= !0x04;
        }
    }

    /// Composite both engines into their framebuffers. Split-borrow: `engine_a`
    /// and `engine_b` are distinct fields, `fb_a`/`fb_b` distinct, and `mem`/
    /// `router`/`vramcnt` are shared `&`. The 3D layer is a stub (black) inside
    /// the engine renderer until the GX wave lands.
    fn render_frame(&mut self, mem: &SharedMemory, router: &VramRouter) {
        self.engine_a
            .render_frame(&mut self.fb_a[..], mem, router, &self.vramcnt);
        self.engine_b
            .render_frame(&mut self.fb_b[..], mem, router, &self.vramcnt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framebuffers_are_256x192_rgba() {
        let ppu = Ppu::new();
        assert_eq!(ppu.top_framebuffer().len(), FB_BYTES);
        assert_eq!(ppu.bottom_framebuffer().len(), FB_BYTES);
        assert_eq!(FB_BYTES, 256 * 192 * 4);
    }

    #[test]
    fn dispcnt_byte_round_trip_engine_a() {
        let mut ppu = Ppu::new();
        ppu.write_engine_reg8(false, 0x00, 0x11);
        ppu.write_engine_reg8(false, 0x01, 0x22);
        ppu.write_engine_reg8(false, 0x02, 0x33);
        ppu.write_engine_reg8(false, 0x03, 0x44);
        assert_eq!(ppu.engine_a.dispcnt, 0x4433_2211);
        assert_eq!(ppu.read_engine_reg8(false, 0x00), 0x11);
        assert_eq!(ppu.read_engine_reg8(false, 0x03), 0x44);
        // Engine B is a separate latch.
        assert_eq!(ppu.engine_b.dispcnt, 0);
    }

    #[test]
    fn bgcnt_and_scroll_round_trip() {
        let mut ppu = Ppu::new();
        // BG1CNT at block offset 0x0A.
        ppu.write_engine_reg8(false, 0x0A, 0x55);
        ppu.write_engine_reg8(false, 0x0B, 0x66);
        assert_eq!(ppu.engine_a.bg.cnt[1], 0x6655);
        // BG2HOFS at 0x18 — 9-bit masked.
        ppu.write_engine_reg8(false, 0x18, 0xFF);
        ppu.write_engine_reg8(false, 0x19, 0x01);
        assert_eq!(ppu.engine_a.bg.hofs[2], 0x1FF);
    }

    #[test]
    fn affine_pa_sign_extends() {
        let mut ppu = Ppu::new();
        // BG2 PA at block offset 0x20 (affine rel 0x00), value 0xFFFF → -1.
        ppu.write_engine_reg8(false, 0x20, 0xFF);
        ppu.write_engine_reg8(false, 0x21, 0xFF);
        assert_eq!(ppu.engine_a.bg.pa[2], -1);
    }

    #[test]
    fn affine_ref_sign_extends_28bit_and_latches() {
        let mut ppu = Ppu::new();
        // BG2X at affine rel 0x08 → block offset 0x28. Write 0x0800_0000
        // (bit 27 set) → sign-extends negative.
        ppu.write_engine_reg8(false, 0x28, 0x00);
        ppu.write_engine_reg8(false, 0x29, 0x00);
        ppu.write_engine_reg8(false, 0x2A, 0x00);
        ppu.write_engine_reg8(false, 0x2B, 0x08);
        assert!(ppu.engine_a.bg.ref_x[2] < 0);
        // Write re-latches immediately.
        assert_eq!(ppu.engine_a.bg.ref_x_latched[2], ppu.engine_a.bg.ref_x[2]);
    }

    #[test]
    fn dispstat_write_preserves_status_bits() {
        let mut ppu = Ppu::new();
        ppu.dispstat = 0x07; // all three status bits set
        ppu.write_dispstat(0xFFF8);
        assert_eq!(ppu.dispstat & 0x07, 0x07);
        assert_eq!(ppu.dispstat & 0xFFF8, 0xFFF8);
    }

    #[test]
    fn vblank_raises_irq_and_renders_frame() {
        let mut ppu = Ppu::new();
        let mut irq9 = Irq::new();
        let mut irq7 = Irq::new();
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        ppu.dispstat |= 0x08; // enable VBlank IRQ on ARM9
        // Step through exactly 192 scanlines worth of dots to hit VBlank.
        let mut saw_vblank = false;
        for _ in 0..(VISIBLE_LINES + 1) {
            let tick = ppu.step(DOTS_PER_LINE, &mut irq9, &mut irq7, &mem, &router);
            if tick.vblank {
                saw_vblank = true;
            }
        }
        assert!(saw_vblank);
        assert_eq!(ppu.vcount, VISIBLE_LINES + 1);
        assert_ne!(irq9.iflag & IRQ_VBLANK, 0);
        assert_ne!(irq7.iflag & IRQ_VBLANK, 0);
        assert!(ppu.frame_done);
        assert_eq!(ppu.frame_count, 1);
        // Frame render produced an opaque framebuffer (alpha = 0xFF).
        assert_eq!(ppu.top_framebuffer()[3], 0xFF);
    }

    #[test]
    fn hblank_flag_asserts_past_dot_256() {
        let mut ppu = Ppu::new();
        let mut irq9 = Irq::new();
        let mut irq7 = Irq::new();
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        // Advance into the visible region but past dot 256.
        ppu.step(300, &mut irq9, &mut irq7, &mem, &router);
        assert_ne!(ppu.dispstat & 0x02, 0);
    }

    #[inline]
    fn rgba_pixel(fb: &[u8], x: usize, y: usize) -> (u8, u8, u8, u8) {
        let o = (y * SCREEN_W + x) * 4;
        (fb[o], fb[o + 1], fb[o + 2], fb[o + 3])
    }

    /// End-to-end: drive the PPU through one full frame on a trivial scene — a
    /// solid red backdrop plus a single 4bpp text-BG tile (8x8 blue block) at the
    /// top-left — and assert the composited TOP framebuffer is (a) non-blank,
    /// (b) shows the tile color inside the tile and (c) the backdrop outside it.
    /// This exercises the whole pipeline: IO-configured engine state → the
    /// scanline clock → text_bg render → compositor → BGR555→RGBA → framebuffer.
    #[test]
    fn renders_backdrop_and_one_text_tile_through_a_frame() {
        let mut ppu = Ppu::new();
        let mut irq9 = Irq::new();
        let mut irq7 = Irq::new();
        let mut mem = SharedMemory::new();
        let router = VramRouter::new();

        // ── Engine A configuration (as the IO writes would land it) ──────────
        // DISPCNT: display-mode 1 (graphics, bit16), BG mode 0, BG0 enabled
        // (bit 8). All BG VRAM resolves to flat offset 0 here (vramcnt all-zero
        // → engine-A BG fallback base 0).
        let e = &mut ppu.engine_a;
        e.dispcnt = (1 << 16) | (1 << 8);
        // BG0CNT: 4bpp text, char_base block 0 (bits 2..5 = 0), screen_base
        // block 1 (bits 8..12 = 1 → byte offset 0x800), priority 0.
        e.bg.cnt[0] = 0x0100;
        e.bg.hofs[0] = 0;
        e.bg.vofs[0] = 0;

        // ── PRAM (engine A starts at 0) ──────────────────────────────────────
        // Backdrop = palette entry 0 = red (BGR555 0x001F).
        mem.pram[0] = 0x1F;
        mem.pram[1] = 0x00;
        // 4bpp palette bank 0, index 1 = blue (0x7C00).
        mem.pram[2] = 0x00;
        mem.pram[3] = 0x7C;

        // ── VRAM tile data (char_base 0): tile 0, every pixel = index 1 ──────
        // 4bpp packs two pixels/byte; 0x11 → both nibbles = 1. 32 bytes/tile.
        for b in 0..32usize {
            mem.vram[b] = 0x11;
        }
        // ── VRAM tilemap (screen_base byte offset 0x800): entry (0,0) = tile 0,
        // palette bank 0, no flip. Leave the rest zero (also tile 0, but those
        // map cells render the same opaque tile; we only assert (0,0) here).
        // Write a distinct backdrop-only cell to the right by pointing a far map
        // entry at a blank tile is unnecessary — index-0 pixels are transparent,
        // so we instead assert the backdrop at a pixel the tile cannot cover by
        // making only map cell (0,0) reference the opaque tile and clearing the
        // tile referenced elsewhere. Simplest: only fill map cell (0,0); all
        // other cells reference tile 0 too, so to see the backdrop we read a
        // scanline row where the tile's own pixels are index-0. We arranged the
        // whole tile opaque, so instead verify backdrop via a transparent tile
        // cell: point map cell (1,0) at tile 1 (left blank → all index 0).
        let map = 0x800usize;
        // cell (0,0) → tile 0 (opaque blue block)
        mem.vram[map] = 0x00;
        mem.vram[map + 1] = 0x00;
        // cell (1,0) → tile 1 (untouched VRAM → all index 0 → transparent →
        // backdrop shows through)
        mem.vram[map + 2] = 0x01;
        mem.vram[map + 3] = 0x00;

        // ── Drive a whole frame: 263 scanlines worth of dots ─────────────────
        for _ in 0..LINES_PER_FRAME {
            ppu.step(DOTS_PER_LINE, &mut irq9, &mut irq7, &mem, &router);
        }
        assert!(ppu.frame_done, "a frame should have completed");

        let fb = ppu.top_framebuffer(); // POWCNT1 default 0x820F → Engine A → top

        // Framebuffer is non-blank: alpha is forced opaque everywhere.
        assert_eq!(fb[3], 0xFF, "framebuffer must be opaque (rendered)");

        // Inside tile (0,0): blue 0x7C00 → (R,G,B) = (0, 0, 255).
        let (r, g, b, a) = rgba_pixel(fb, 3, 3);
        assert_eq!((r, g, b, a), (0, 0, 0xFF, 0xFF), "tile pixel should be blue");

        // Tile cell (1,0) references blank tile 1 → transparent → backdrop red
        // (0x001F) shows: (R,G,B) = (255, 0, 0).
        let (r, g, b, _) = rgba_pixel(fb, 10, 3);
        assert_eq!((r, g, b), (0xFF, 0, 0), "outside-tile pixel = red backdrop");

        // Sanity: the frame is genuinely non-uniform (tile != backdrop).
        assert_ne!(rgba_pixel(fb, 3, 3), rgba_pixel(fb, 10, 3));
    }

    /// POWCNT1 bit 15 swaps which engine drives which physical screen.
    #[test]
    fn powcnt1_bit15_swaps_top_and_bottom() {
        let mut ppu = Ppu::new();
        // Distinguish the two engine framebuffers by their alpha-less content:
        // paint fb_a red-ish, fb_b blue-ish by hand (bypassing render).
        ppu.fb_a[0] = 0xAA;
        ppu.fb_b[0] = 0xBB;

        ppu.write_powcnt1(0x8000); // bit15 set → Engine A → top
        assert_eq!(ppu.top_framebuffer()[0], 0xAA);
        assert_eq!(ppu.bottom_framebuffer()[0], 0xBB);

        ppu.write_powcnt1(0x0000); // bit15 clear → Engine A → bottom
        assert_eq!(ppu.top_framebuffer()[0], 0xBB);
        assert_eq!(ppu.bottom_framebuffer()[0], 0xAA);
    }

    // ── Full-pipeline BLD* tests, ported from ds-recomp/src/test/blend.test.ts.
    //    `setup_solid_bg0` builds a frame where BG0 is one solid text tile of
    //    palette color 1 over backdrop color 0, then a whole frame is driven and
    //    the BLDCNT/BLDALPHA/BLDY color-special-effect is read back from the FB.

    fn set_palette(mem: &mut SharedMemory, idx: usize, bgr555: u16) {
        mem.pram[idx * 2] = bgr555 as u8;
        mem.pram[idx * 2 + 1] = (bgr555 >> 8) as u8;
    }

    fn setup_solid_bg0(ppu: &mut Ppu, mem: &mut SharedMemory) {
        // Tile 0: all index-1 nibbles, 4bpp (32 bytes).
        for i in 0..32usize {
            mem.vram[i] = 0x11;
        }
        // Screen map (32x32) at screen base 0x800 — every entry tile 0/bank 0.
        for i in 0..(32 * 32) {
            mem.vram[0x800 + i * 2] = 0;
            mem.vram[0x800 + i * 2 + 1] = 0;
        }
        let e = &mut ppu.engine_a;
        e.bg.cnt[0] = 1 << 8; // priority 0, char_base 0, screen_base block 1
        e.dispcnt = (1 << 16) | (1 << 8); // graphics mode, BG0 enabled
    }

    fn run_one_frame(ppu: &mut Ppu, mem: &SharedMemory) {
        let mut irq9 = Irq::new();
        let mut irq7 = Irq::new();
        let router = VramRouter::new();
        for _ in 0..LINES_PER_FRAME {
            ppu.step(DOTS_PER_LINE, &mut irq9, &mut irq7, mem, &router);
        }
    }

    fn pixel_bgr555(fb: &[u8], x: usize, y: usize) -> u32 {
        let i = (y * SCREEN_W + x) * 4;
        let r = (fb[i] >> 3) as u32;
        let g = (fb[i + 1] >> 3) as u32;
        let b = (fb[i + 2] >> 3) as u32;
        (b << 10) | (g << 5) | r
    }

    #[test]
    fn blend_mode2_fade_to_white_half() {
        let mut ppu = Ppu::new();
        let mut mem = SharedMemory::new();
        setup_solid_bg0(&mut ppu, &mut mem);
        set_palette(&mut mem, 0, 0x0000); // backdrop black
        set_palette(&mut mem, 1, 0x0010); // R=16 only
        ppu.engine_a.bldcnt = (2 << 6) | 0x01; // mode 2 (fade-white), target BG0
        ppu.engine_a.bldy = 8; // half fade
        run_one_frame(&mut ppu, &mem);
        // r' = 16 + (31-16)*8/16 = 23; g'=b' = 0 + 31*8/16 = 15.
        let c = pixel_bgr555(ppu.engine_a_framebuffer(), 100, 100);
        assert_eq!(c & 0x1F, 23, "R");
        assert_eq!((c >> 5) & 0x1F, 15, "G");
        assert_eq!((c >> 10) & 0x1F, 15, "B");
    }

    #[test]
    fn blend_mode3_fade_to_black_half() {
        let mut ppu = Ppu::new();
        let mut mem = SharedMemory::new();
        setup_solid_bg0(&mut ppu, &mut mem);
        set_palette(&mut mem, 0, 0x0000);
        set_palette(&mut mem, 1, 0x4210); // R=G=B=16 mid grey
        ppu.engine_a.bldcnt = (3 << 6) | 0x01; // mode 3 (fade-black)
        ppu.engine_a.bldy = 8;
        run_one_frame(&mut ppu, &mem);
        // Each channel: 16 - 16*8/16 = 8.
        let c = pixel_bgr555(ppu.engine_a_framebuffer(), 100, 100);
        assert_eq!(c & 0x1F, 8);
        assert_eq!((c >> 5) & 0x1F, 8);
        assert_eq!((c >> 10) & 0x1F, 8);
    }

    #[test]
    fn blend_mode1_alpha_bg0_over_backdrop() {
        let mut ppu = Ppu::new();
        let mut mem = SharedMemory::new();
        setup_solid_bg0(&mut ppu, &mut mem);
        set_palette(&mut mem, 0, 16 << 10); // backdrop blue B=16
        set_palette(&mut mem, 1, 16); // BG0 red R=16
        // mode 1 (alpha), target A = BG0 (bit0), target B = backdrop (bit13).
        ppu.engine_a.bldcnt = (1 << 6) | (1 << 0) | (1 << 13);
        ppu.engine_a.bldalpha = (4 << 8) | 8; // EVA=8, EVB=4
        run_one_frame(&mut ppu, &mem);
        // R = (16*8 + 0*4)/16 = 8; G = 0; B = (0*8 + 16*4)/16 = 4.
        let c = pixel_bgr555(ppu.engine_a_framebuffer(), 100, 100);
        assert_eq!(c & 0x1F, 8);
        assert_eq!((c >> 5) & 0x1F, 0);
        assert_eq!((c >> 10) & 0x1F, 4);
    }

    #[test]
    fn blend_mode0_no_effect_even_with_targets() {
        let mut ppu = Ppu::new();
        let mut mem = SharedMemory::new();
        setup_solid_bg0(&mut ppu, &mut mem);
        set_palette(&mut mem, 0, 0x7FFF); // bright backdrop
        set_palette(&mut mem, 1, 16); // BG0 red R=16
        ppu.engine_a.bldcnt = 0x01 | (1 << 13); // mode 0, targets set
        ppu.engine_a.bldalpha = (16 << 8) | 16;
        ppu.engine_a.bldy = 16;
        run_one_frame(&mut ppu, &mem);
        let c = pixel_bgr555(ppu.engine_a_framebuffer(), 100, 100);
        assert_eq!(c & 0x1F, 16);
        assert_eq!((c >> 5) & 0x1F, 0);
        assert_eq!((c >> 10) & 0x1F, 0);
    }
}
