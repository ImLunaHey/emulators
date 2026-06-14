//! The two DS 2D graphics engines (A = "main", B = "sub") as ONE parameterized
//! `Engine` struct. Ported/adapted from ../../ds-recomp/src/ppu/engine_a.ts
//! and ../../ds-recomp/src/ppu/ppu.ts (the per-engine register fields lived on
//! the TS `Ppu` as `*A` / `*B` pairs; here each engine owns its own copy).
//!
//! Both DS engines are a SUPERSET of the GBA PPU — same text/affine/bitmap BG
//! modes, OBJ sprites, windows and blending — so the register layout and the
//! packed per-layer pixel format are adapted directly from the GBA core
//! (../../core/src/ppu.rs). The DS deltas this struct carries:
//!   - DISPCNT is 32-bit (extended-palette enable, BG/OBJ char/map global
//!     base offsets, display mode 0..3, LCDC bank select);
//!   - MASTER_BRIGHT (final brightness fade);
//!   - extended palettes (resolved via the VRAM router, see `vramcnt`);
//!   - Engine A has feature bits B lacks (large-bitmap mode 6, DISPCAPCNT, the
//!     3D layer on BG0). The `kind` field gates those.
//!
//! Ownership model (CONTRACT.md): the TS render functions received the whole
//! `Ppu` (and reached `ppu.mem.vram` etc.). Here the renderers are FREE
//! functions that take the engine's register state plus borrowed VRAM / PRAM /
//! OAM / vramcnt slices — NO `&mut Nds`, mirroring the foundation/sound
//! split-borrow style. The compositor entry `render_frame` orchestrates them.
//!
//! THIS WAVE: register state + accessors are real; the per-layer RENDER bodies
//! (text/affine/bitmap/sprites) and the compositor are signature-only stubs the
//! next wave fills. Anything reachable from `Engine::new()` is a real ctor.

use super::ppu::{SCREEN_H, SCREEN_W};
use super::{affine_bg, bitmap_bg, sprites, text_bg};
use crate::memory::{SharedMemory, VramRouter};

/// Which of the two 2D engines this is. Engine A is the main engine (all
/// features); Engine B is the sub engine (no large-bitmap mode 6, no display
/// capture, no 3D layer). A closed enum so feature gating is an exhaustive
/// `match` rather than a magic `is_engine_a` bool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineKind {
    A,
    B,
}

impl EngineKind {
    /// Base byte offset of this engine's palette within `SharedMemory::pram`
    /// (engine A = 0, engine B = 0x400).
    #[inline]
    pub fn pram_base(self) -> usize {
        match self {
            EngineKind::A => 0x000,
            EngineKind::B => 0x400,
        }
    }
    /// Base byte offset of this engine's OAM within `SharedMemory::oam`.
    #[inline]
    pub fn oam_base(self) -> usize {
        match self {
            EngineKind::A => 0x000,
            EngineKind::B => 0x400,
        }
    }
}

// ─── Packed per-layer scanline pixel format ──────────────────────────────────
//
// Adapted from the GBA core (../../core/src/ppu.rs). Each per-layer line buffer
// holds one packed u32 per pixel:
//   bits  0..14   BGR555 color
//   bit   15      transparent (1 = no pixel drawn here)
//   bits 16..17   layer source (0..3 = BG0..3, 4 = OBJ, 5 = backdrop)
//   bits 18..19   priority (0..3)
//   bit   20      OBJ semi-transparent
//   bit   21      OBJ window
// The bg/sprite renderers write these; the compositor picks per pixel, applies
// windows + blend + master-bright, then converts to RGBA8888.
pub const PX_TRANSPARENT: u32 = 0x8000;

/// Width of a per-layer scanline buffer (= screen width, 256 on the DS).
pub const LINE_W: usize = SCREEN_W;

/// Per-BG control + scroll + affine register state for the four BG layers of
/// ONE engine. BG0/BG1 only use the text fields; BG2/BG3 add the affine matrix
/// + reference point. We keep all four slots uniform so a single per-BG loop
/// can index everything (mirrors the TS `bgCnt`/`bgHofs`/`bgPA` arrays).
#[derive(Clone)]
pub struct BgRegs {
    /// BGxCNT (priority / char-base / mosaic / color-mode / map-base / size).
    pub cnt: [u32; 4],
    /// BGxHOFS (9-bit horizontal scroll).
    pub hofs: [u32; 4],
    /// BGxVOFS (9-bit vertical scroll).
    pub vofs: [u32; 4],

    // Affine matrix (BG2/BG3 → indices 2/3 used; 0/1 unused but kept uniform).
    // PA..PD are 16-bit signed Q8.8.
    pub pa: [i32; 4],
    pub pb: [i32; 4],
    pub pc: [i32; 4],
    pub pd: [i32; 4],

    // Reference point (BGxX/BGxY), 28-bit signed Q20.8. `ref_*` is the live
    // register (re-latched on direct write); `ref_*_latched` is the per-frame
    // running copy the affine renderer accumulates PB/PD into per scanline.
    pub ref_x: [i32; 4],
    pub ref_y: [i32; 4],
    pub ref_x_latched: [i32; 4],
    pub ref_y_latched: [i32; 4],
}

impl Default for BgRegs {
    fn default() -> Self {
        BgRegs {
            cnt: [0; 4],
            hofs: [0; 4],
            vofs: [0; 4],
            pa: [0; 4],
            pb: [0; 4],
            pc: [0; 4],
            pd: [0; 4],
            ref_x: [0; 4],
            ref_y: [0; 4],
            ref_x_latched: [0; 4],
            ref_y_latched: [0; 4],
        }
    }
}

/// Window registers for ONE engine (WIN0/WIN1 bounds + WININ/WINOUT enables).
#[derive(Clone, Default)]
pub struct WinRegs {
    /// WINxH: bits 8..15 = left (X1), bits 0..7 = right (X2, exclusive).
    pub h: [u32; 2],
    /// WINxV: bits 8..15 = top (Y1), bits 0..7 = bottom (Y2, exclusive).
    pub v: [u32; 2],
    /// WININ: per-region BG/OBJ/special-effect enable bits.
    pub win_in: u32,
    /// WINOUT: outside + OBJ-window enable bits.
    pub win_out: u32,
}

/// The complete register state of one 2D engine plus its render scratch.
pub struct Engine {
    /// Which engine (gates A-only features).
    pub kind: EngineKind,

    /// DISPCNT — 32-bit on the DS (display mode, BG mode, layer enables,
    /// extended-palette enable, global char/map base, OBJ mapping, windows).
    pub dispcnt: u32,

    /// BG control / scroll / affine for the four BG layers.
    pub bg: BgRegs,

    /// MOSAIC (BG H/V in low byte, OBJ H/V in high byte, each N-1 encoded).
    pub mosaic: u32,

    /// Window state.
    pub win: WinRegs,

    /// BLDCNT (target-A bits 0..5, mode bits 6..7, target-B bits 8..13).
    pub bldcnt: u32,
    /// BLDALPHA (EVA bits 0..4, EVB bits 8..12).
    pub bldalpha: u32,
    /// BLDY (EVY bits 0..4).
    pub bldy: u32,

    /// MASTER_BRIGHT (factor bits 0..4, mode bits 14..15). Post-compositor.
    pub master_bright: u32,

    /// DISPCAPCNT (Engine A only; ignored on B). Display-capture control.
    pub disp_cap_cnt: u32,

    // ─── Render scratch (per scanline) ────────────────────────────────────
    /// Four per-BG packed-pixel line buffers (see PX_TRANSPARENT format).
    pub bg_line: [Box<[u32; LINE_W]>; 4],
    /// OBJ packed-pixel line buffer.
    pub obj_line: Box<[u32; LINE_W]>,

    /// 3D-layer source for the BG0-in-3D-mode path (Engine A only). When BG0 is
    /// the 3D layer (DISPCNT bit 3) this holds the GX engine's packed per-pixel
    /// output for the whole frame: `SCREEN_W * SCREEN_H` entries in the
    /// `PX_TRANSPARENT` convention (BGR555 in bits 0..14, bit 15 set where the
    /// pixel was not drawn). `None` (or absent on Engine B) → BG0 stays
    /// transparent, exactly as the pre-3D stub did. `Nds` fills this from
    /// `Gpu3d` once per frame before compositing; the GX rasterizer wave wires
    /// the actual `Gpu3d::render_scanline` source.
    pub gx_bg0_layer: Option<Box<[u32]>>,
}

impl Engine {
    pub fn new(kind: EngineKind) -> Self {
        Engine {
            kind,
            dispcnt: 0,
            bg: BgRegs::default(),
            mosaic: 0,
            win: WinRegs::default(),
            bldcnt: 0,
            bldalpha: 0,
            bldy: 0,
            master_bright: 0,
            disp_cap_cnt: 0,
            bg_line: [
                Box::new([PX_TRANSPARENT; LINE_W]),
                Box::new([PX_TRANSPARENT; LINE_W]),
                Box::new([PX_TRANSPARENT; LINE_W]),
                Box::new([PX_TRANSPARENT; LINE_W]),
            ],
            obj_line: Box::new([PX_TRANSPARENT; LINE_W]),
            gx_bg0_layer: None,
        }
    }

    // ─── Byte-granular register accessors (driven by the Nds IO dispatch) ───
    //
    // The IO dispatch in nds.rs composes wider accesses from these. Each takes
    // a register-block-relative offset (0x00 = DISPCNT for engine A; engine B's
    // mirror at 0x04001xxx subtracts 0x1000 before calling). The affine
    // BGxX/BGxY + PA..PD writes have re-latch side effects, handled here.

    /// Read one byte of this engine's register block at block-relative `off`
    /// (0x000..0x06F). `off` already has the 0x04001000 engine-B bias removed.
    pub fn read_reg8(&self, off: u32) -> u32 {
        match off {
            0x00..=0x03 => (self.dispcnt >> ((off & 3) * 8)) & 0xFF,
            0x08..=0x0F => {
                let bg = ((off - 0x08) >> 1) as usize;
                (self.bg.cnt[bg] >> ((off & 1) * 8)) & 0xFF
            }
            // BGxHOFS/VOFS (write-only on hardware; return 0 like the TS).
            0x10..=0x1F => 0,
            // Affine PA..PD / BGxX/BGxY are write-only.
            0x20..=0x3F => 0,
            0x40..=0x47 => {
                // WINxH / WINxV (0=W0H, 1=W1H, 2=W0V, 3=W1V).
                let reg = ((off - 0x40) >> 1) as usize;
                let arr = if reg < 2 { &self.win.h } else { &self.win.v };
                let i = reg & 1;
                (arr[i] >> ((off & 1) * 8)) & 0xFF
            }
            0x48 => self.win.win_in & 0xFF,
            0x49 => (self.win.win_in >> 8) & 0xFF,
            0x4A => self.win.win_out & 0xFF,
            0x4B => (self.win.win_out >> 8) & 0xFF,
            0x4C => self.mosaic & 0xFF,
            0x4D => (self.mosaic >> 8) & 0xFF,
            0x50 => self.bldcnt & 0xFF,
            0x51 => (self.bldcnt >> 8) & 0xFF,
            0x52 => self.bldalpha & 0xFF,
            0x53 => (self.bldalpha >> 8) & 0xFF,
            0x54 => self.bldy & 0xFF,
            0x55 => (self.bldy >> 8) & 0xFF,
            0x64..=0x67 => (self.disp_cap_cnt >> ((off & 3) * 8)) & 0xFF,
            0x6C => self.master_bright & 0xFF,
            0x6D => (self.master_bright >> 8) & 0xFF,
            _ => 0,
        }
    }

    /// Write one byte of this engine's register block at block-relative `off`.
    pub fn write_reg8(&mut self, off: u32, v: u32) {
        let v = v & 0xFF;
        match off {
            0x00..=0x03 => {
                let sh = (off & 3) * 8;
                self.dispcnt = (self.dispcnt & !(0xFF << sh)) | (v << sh);
            }
            0x08..=0x0F => {
                let bg = ((off - 0x08) >> 1) as usize;
                let sh = (off & 1) * 8;
                self.bg.cnt[bg] = ((self.bg.cnt[bg] & !(0xFF << sh)) | (v << sh)) & 0xFFFF;
            }
            0x10..=0x1F => {
                // BGxHOFS (sub 0,1) / BGxVOFS (sub 2,3) per BG, 9-bit each.
                let bg = ((off - 0x10) >> 2) as usize;
                let sub = (off - 0x10) & 0x3;
                let is_hofs = sub < 2;
                let arr = if is_hofs {
                    &mut self.bg.hofs
                } else {
                    &mut self.bg.vofs
                };
                let sh = (sub & 1) * 8;
                arr[bg] = ((arr[bg] & !(0xFF << sh)) | (v << sh)) & 0x1FF;
            }
            0x20..=0x3F => self.write_affine_byte(off - 0x20, v),
            0x40..=0x47 => {
                let reg = ((off - 0x40) >> 1) as usize;
                let i = reg & 1;
                let sh = (off & 1) * 8;
                let arr = if reg < 2 {
                    &mut self.win.h
                } else {
                    &mut self.win.v
                };
                arr[i] = ((arr[i] & !(0xFF << sh)) | (v << sh)) & 0xFFFF;
            }
            0x48 => self.win.win_in = (self.win.win_in & 0xFF00) | v,
            0x49 => self.win.win_in = (self.win.win_in & 0x00FF) | (v << 8),
            0x4A => self.win.win_out = (self.win.win_out & 0xFF00) | v,
            0x4B => self.win.win_out = (self.win.win_out & 0x00FF) | (v << 8),
            0x4C => self.mosaic = (self.mosaic & 0xFF00) | v,
            0x4D => self.mosaic = (self.mosaic & 0x00FF) | (v << 8),
            0x50 => self.bldcnt = (self.bldcnt & 0xFF00) | v,
            0x51 => self.bldcnt = (self.bldcnt & 0x00FF) | (v << 8),
            0x52 => self.bldalpha = (self.bldalpha & 0xFF00) | v,
            0x53 => self.bldalpha = (self.bldalpha & 0x00FF) | (v << 8),
            0x54 => self.bldy = (self.bldy & 0xFF00) | v,
            0x55 => self.bldy = (self.bldy & 0x00FF) | (v << 8),
            0x64..=0x67 => {
                // DISPCAPCNT — Engine A only; swallow on B.
                if self.kind == EngineKind::A {
                    let sh = (off & 3) * 8;
                    self.disp_cap_cnt = (self.disp_cap_cnt & !(0xFF << sh)) | (v << sh);
                }
            }
            0x6C => self.master_bright = (self.master_bright & 0xFF00) | v,
            0x6D => self.master_bright = (self.master_bright & 0x00FF) | (v << 8),
            _ => {}
        }
    }

    /// Affine BG register byte write (BG2 at rel 0x00..0x0F, BG3 at 0x10..0x1F
    /// within the affine block, i.e. IO addresses 0x04000020..0x0400003F).
    /// Ported from io.ts `writeAffineByte`: PA..PD halves (signed Q8.8) and the
    /// 28-bit signed Q20.8 BGxX/BGxY reference, which re-latches on write.
    fn write_affine_byte(&mut self, rel: u32, v: u32) {
        let bg = if (rel & 0x10) != 0 { 3 } else { 2 };
        let inner = rel & 0x0F;
        if inner < 8 {
            // PA/PB/PC/PD halves: inner 0..1 = PA lo/hi, 2..3 = PB, …
            let which = (inner >> 1) as usize;
            let is_hi = (inner & 1) == 1;
            let arr = match which {
                0 => &mut self.bg.pa,
                1 => &mut self.bg.pb,
                2 => &mut self.bg.pc,
                _ => &mut self.bg.pd,
            };
            let cur = (arr[bg] as u32) & 0xFFFF;
            let next = if is_hi {
                (cur & 0x00FF) | (v << 8)
            } else {
                (cur & 0xFF00) | v
            };
            // Sign-extend the 16-bit value into the i32 slot.
            arr[bg] = ((next << 16) as i32) >> 16;
            return;
        }
        // BGxX (inner 8..B) / BGxY (inner C..F): 28-bit signed Q20.8.
        let is_y = (inner & 0x4) != 0;
        let byte_idx = inner & 0x3;
        let cur = if is_y {
            self.bg.ref_y[bg] as u32
        } else {
            self.bg.ref_x[bg] as u32
        };
        let mask = !(0xFFu32 << (byte_idx * 8));
        let mut next = (cur & mask) | (v << (byte_idx * 8));
        next &= 0x0FFF_FFFF;
        if (next & 0x0800_0000) != 0 {
            next |= 0xF000_0000;
        }
        let signed = next as i32;
        if is_y {
            self.bg.ref_y[bg] = signed;
            self.bg.ref_y_latched[bg] = signed;
        } else {
            self.bg.ref_x[bg] = signed;
            self.bg.ref_x_latched[bg] = signed;
        }
    }

    /// Re-seed the per-frame affine reference latches from the live registers.
    /// Called by the PPU at the start of each frame (VCount=0 boundary).
    pub fn latch_affine_refs(&mut self) {
        for bg in 2..4 {
            self.bg.ref_x_latched[bg] = self.bg.ref_x[bg];
            self.bg.ref_y_latched[bg] = self.bg.ref_y[bg];
        }
    }

    // ─── Compositor entry (THE renderer-wave seam) ────────────────────────
    //
    // The next wave fills this. It renders the whole engine frame into `fb`
    // (256x192 RGBA8888), reading VRAM/PRAM/OAM through borrowed slices + the
    // VRAM router (for extended palettes / bank base resolution) — NEVER an
    // `&mut Nds`. The PPU calls it once per frame from `render_frame`.
    //
    // Signature is FROZEN for the parallel wave; only the body is provisional.

    /// Render this engine's full 256x192 frame into `fb` (RGBA8888, len
    /// SCREEN_W*SCREEN_H*4). `mem` provides VRAM/PRAM/OAM; `router` + `vramcnt`
    /// resolve bank bases + extended palettes. No `&mut Nds`.
    ///
    /// Adapted from ds-recomp engine_a.ts `renderEngine` + the GBA core
    /// `render_scanline`/`composite_scanline`. The four-way DISPCNT display
    /// mode (bits 16..17) decides the whole pipeline:
    ///   0 = display off  → forced blank (white, like the TS / GBA core),
    ///   1 = graphics     → composite BGs + OBJ per scanline,
    ///   2 = LCDC direct  → one VRAM bank blitted straight to the screen,
    ///   3 = main-mem FIFO→ deferred (treated as black; needs the capture/DMA
    ///                       FIFO that isn't wired yet).
    /// MASTER_BRIGHT is applied as a final post-pass over the whole frame.
    pub fn render_frame(
        &mut self,
        fb: &mut [u8],
        mem: &SharedMemory,
        router: &VramRouter,
        vramcnt: &[u8; 9],
    ) {
        let display_mode = (self.dispcnt >> 16) & 0x3;
        match display_mode {
            0 => {
                // Display off → forced blank (white).
                for px in fb.chunks_exact_mut(4) {
                    px[0] = 0xFF;
                    px[1] = 0xFF;
                    px[2] = 0xFF;
                    px[3] = 0xFF;
                }
            }
            2 => {
                // LCDC bank-direct display (Engine A only on hardware; B never
                // selects mode 2). One of VRAM banks A..D (selected by DISPCNT
                // bits 18..19) is shown as a raw 256x192 BGR555 framebuffer.
                let bank = ((self.dispcnt >> 18) & 0x3) as usize;
                let base = bank * 0x20000; // banks A..D are 128 KB each
                let vram: &[u8] = &mem.vram[..];
                for y in 0..SCREEN_H {
                    for x in 0..SCREEN_W {
                        let p = base + (y * SCREEN_W + x) * 2;
                        let bgr = if p + 1 < vram.len() {
                            (vram[p] as u32) | ((vram[p + 1] as u32) << 8)
                        } else {
                            0
                        };
                        bgr555_to_rgba(bgr & 0x7FFF, fb, (y * SCREEN_W + x) * 4);
                    }
                }
            }
            3 => {
                // Main-memory FIFO display — deferred (no DMA FIFO source yet).
                // Black it out so the screen is defined and reproducible.
                for px in fb.chunks_exact_mut(4) {
                    px[0] = 0;
                    px[1] = 0;
                    px[2] = 0;
                    px[3] = 0xFF;
                }
            }
            _ => {
                // display_mode == 1: graphics — the real compositor.
                self.render_graphics(fb, mem, router, vramcnt);
            }
        }

        apply_master_brightness(fb, self.master_bright);
    }

    /// Display-mode-1 graphics path: for each scanline clear the per-layer line
    /// buffers, dispatch the enabled BG slots + OBJ through the free renderers,
    /// then composite (priority + windows + blend) into `fb`.
    fn render_graphics(
        &mut self,
        fb: &mut [u8],
        mem: &SharedMemory,
        router: &VramRouter,
        vramcnt: &[u8; 9],
    ) {
        let vram: &[u8] = &mem.vram[..];
        let pram: &[u8] = &mem.pram[..];
        let oam: &[u8] = &mem.oam[..];

        let is_a = self.kind == EngineKind::A;
        let pram_base = self.kind.pram_base();
        let oam_base = self.kind.oam_base();
        let obj_pram_base = pram_base + 0x200;

        // Resolve the BG / OBJ VRAM window bases through the router. Real games
        // configure VRAMCNT before enabling the matching DISPCNT bits; the
        // fallbacks cover reset-time / unconfigured renders (see TS notes).
        let (bg_win, obj_win, bg_fallback, obj_fallback) = if is_a {
            (0x0600_0000u32, 0x0640_0000u32, 0usize, 0x20000usize)
        } else {
            (0x0620_0000u32, 0x0660_0000u32, 0x40000usize, 0x60000usize)
        };
        let bg_vram_base = router
            .resolve_arm9(bg_win, vramcnt)
            .unwrap_or(bg_fallback);
        let obj_vram_base = router
            .resolve_arm9(obj_win, vramcnt)
            .unwrap_or(obj_fallback);

        let bg_mode = self.dispcnt & 0x7;
        let obj_enabled = (self.dispcnt & 0x1000) != 0;
        let bg_enables = (self.dispcnt >> 8) & 0xF;
        let ext_pal_enabled = (self.dispcnt & (1 << 30)) != 0; // BG ext-pal
        let obj_ext_pal_enabled = (self.dispcnt & (1 << 31)) != 0; // OBJ ext-pal

        // Engine-A global char/screen base offsets (DISPCNT bits 24..26 char,
        // 27..29 screen map). Engine B has none.
        let char_extra = if is_a {
            (((self.dispcnt >> 24) & 0x7) * 0x10000) as usize
        } else {
            0
        };
        let screen_extra = if is_a {
            (((self.dispcnt >> 27) & 0x7) * 0x10000) as usize
        } else {
            0
        };

        // Backdrop = palette entry 0 of this engine.
        let backdrop = rd16(pram, pram_base) & 0x7FFF;

        // OBJ extended palette slice (256-color, when enabled + mapped).
        let obj_ext_pal: Option<&[u8]> = if obj_ext_pal_enabled {
            let resolver = if is_a {
                VramRouter::resolve_obj_ext_pal_a
            } else {
                VramRouter::resolve_obj_ext_pal_b
            };
            resolver(router, 0, vramcnt).map(|idx| &vram[idx..])
        } else {
            None
        };

        for y in 0..SCREEN_H as u32 {
            // Clear per-layer line buffers (mark transparent).
            for bg in 0..4 {
                for px in self.bg_line[bg].iter_mut() {
                    *px = PX_TRANSPARENT;
                }
            }
            for px in self.obj_line.iter_mut() {
                *px = PX_TRANSPARENT;
            }

            // Dispatch each enabled BG slot.
            for bg in 0..4 {
                if (bg_enables & (1 << bg)) == 0 {
                    continue;
                }
                match bg_slot_kind(bg, bg_mode, is_a) {
                    SlotKind::Off => {}
                    SlotKind::Text => {
                        let ext = bg_ext_pal_slice(
                            is_a,
                            bg,
                            self.bg.cnt[bg],
                            ext_pal_enabled,
                            router,
                            vramcnt,
                            vram,
                        );
                        text_bg::render_text_scanline(
                            bg,
                            y,
                            &self.bg,
                            self.mosaic,
                            vram,
                            bg_vram_base,
                            char_extra,
                            screen_extra,
                            pram,
                            pram_base,
                            ext,
                            &mut self.bg_line[bg][..],
                        );
                    }
                    SlotKind::Affine | SlotKind::Extended => {
                        let force_tile = matches!(bg_slot_kind(bg, bg_mode, is_a), SlotKind::Affine);
                        let ext = bg_ext_pal_slice(
                            is_a,
                            bg,
                            self.bg.cnt[bg],
                            ext_pal_enabled,
                            router,
                            vramcnt,
                            vram,
                        );
                        affine_bg::render_affine_bg_scanline(
                            bg,
                            y,
                            &self.bg,
                            vram,
                            bg_vram_base,
                            char_extra,
                            screen_extra,
                            pram,
                            pram_base,
                            ext,
                            force_tile,
                            &mut self.bg_line[bg][..],
                        );
                    }
                    SlotKind::LargeBitmap => {
                        bitmap_bg::render_bitmap_scanline(
                            self.bg.cnt[bg],
                            self.bg.hofs[bg],
                            self.bg.vofs[bg],
                            y,
                            vram,
                            bg_vram_base,
                            &mut self.bg_line[bg][..],
                        );
                    }
                }
            }

            // 3D engine drives BG0 (Engine A only, DISPCNT bit 3). Copy the GX
            // engine's packed scanline into BG0's line buffer so the compositor
            // treats it like any other BG layer (priority from BG0CNT). When no
            // 3D layer is supplied (`gx_bg0_layer` is `None` — e.g. the GX
            // rasterizer wave hasn't filled it yet) BG0 stays transparent,
            // exactly as the pre-3D stub did.
            if is_a && (self.dispcnt & 0x8) != 0 {
                match &self.gx_bg0_layer {
                    Some(layer) => {
                        let row = (y as usize) * SCREEN_W;
                        let pri = (self.bg.cnt[0] & 0x3) << 18; // BG0CNT priority
                        for x in 0..LINE_W {
                            let src = layer[row + x];
                            // Carry the 3D pixel's transparent bit; tag it as
                            // BG0 (layer 0) at BG0's priority for the compositor.
                            self.bg_line[0][x] = if (src & PX_TRANSPARENT) != 0 {
                                PX_TRANSPARENT
                            } else {
                                (src & 0x7FFF) | pri
                            };
                        }
                    }
                    None => {
                        for px in self.bg_line[0].iter_mut() {
                            *px = PX_TRANSPARENT;
                        }
                    }
                }
            }

            // OBJ layer.
            if obj_enabled {
                sprites::render_obj_scanline(
                    y,
                    self.dispcnt,
                    self.mosaic,
                    oam,
                    oam_base,
                    vram,
                    obj_vram_base,
                    pram,
                    obj_pram_base,
                    obj_ext_pal,
                    &mut self.obj_line[..],
                );
            }

            self.composite_scanline(y, backdrop, fb);

            // Advance the per-frame affine reference latch by one scanline's
            // worth of (PB, PD) for any slot the current mode uses as affine.
            // (Per GBATEK this bump happens every visible scanline, including
            // the last — harmless since latches reseed at the next VBlank.)
            for bg in 2..4 {
                match bg_slot_kind(bg, bg_mode, is_a) {
                    SlotKind::Affine | SlotKind::Extended => {
                        affine_bg::advance_affine_ref_for_scanline(&mut self.bg, bg);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Composite the four BG line buffers + OBJ line into one RGBA scanline of
    /// `fb`, applying windows + the BLDCNT color special effect per pixel.
    /// Adapted from the GBA core `composite_scanline` + ds-recomp engine_a.ts.
    fn composite_scanline(&self, y: u32, backdrop: u32, fb: &mut [u8]) {
        let off_base = (y as usize) * SCREEN_W * 4;

        let bldcnt = self.bldcnt;
        let effect_mode = (bldcnt >> 6) & 3;
        let target_a = bldcnt & 0x3F;
        let target_b = (bldcnt >> 8) & 0x3F;
        let eva = (self.bldalpha & 0x1F).min(16);
        let evb = ((self.bldalpha >> 8) & 0x1F).min(16);
        let evy = (self.bldy & 0x1F).min(16);

        // Window enables (DISPCNT bits 13=WIN0, 14=WIN1, 15=OBJWIN).
        let win0_en = (self.dispcnt & 0x2000) != 0;
        let win1_en = (self.dispcnt & 0x4000) != 0;
        let obj_win_en = (self.dispcnt & 0x8000) != 0;
        let any_win = win0_en || win1_en || obj_win_en;

        // WINxH: bits 8..15 = left (X1), bits 0..7 = right (X2, exclusive).
        // WINxV: bits 8..15 = top (Y1), bits 0..7 = bottom (Y2, exclusive).
        let w0_left = (self.win.h[0] >> 8) & 0xFF;
        let w0_right = self.win.h[0] & 0xFF;
        let w1_left = (self.win.h[1] >> 8) & 0xFF;
        let w1_right = self.win.h[1] & 0xFF;
        let w0_in = self.win.win_in & 0x3F;
        let w1_in = (self.win.win_in >> 8) & 0x3F;
        let obj_win_in = (self.win.win_out >> 8) & 0x3F;
        let out_mask = self.win.win_out & 0x3F;

        let w0_row = win0_en && row_inside_window(y, self.win.v[0]);
        let w1_row = win1_en && row_inside_window(y, self.win.v[1]);

        for x in 0..SCREEN_W {
            let xu = x as u32;
            // Per-pixel window mask: WIN0 > WIN1 > OBJWIN > outside. Low 6 bits
            // = BG0..3 enable, bit 4 OBJ, bit 5 special-effect enable.
            let mask = if any_win {
                let obj = self.obj_line[x];
                if w0_row && col_inside_window(xu, w0_left, w0_right) {
                    w0_in
                } else if w1_row && col_inside_window(xu, w1_left, w1_right) {
                    w1_in
                } else if obj_win_en && (obj & (1 << 21)) != 0 {
                    obj_win_in
                } else {
                    out_mask
                }
            } else {
                0x3F
            };

            // Find the top-two visible layers by priority. BGs ordered by
            // priority, ties broken by BG index (BG0 highest). OBJ sits at its
            // own priority and beats equal-priority BGs.
            let mut top_color = backdrop;
            let mut top_layer = 5u32; // backdrop
            let mut top_pri = 5u32;
            let mut second_color = backdrop;
            let mut second_layer = 5u32;
            let mut second_pri = 5u32;

            for bg in 0..4 {
                let px = self.bg_line[bg][x];
                if (px & PX_TRANSPARENT) != 0 {
                    continue;
                }
                if (mask & (1 << bg)) == 0 {
                    continue;
                }
                let pri = (px >> 18) & 3;
                let color = px & 0x7FFF;
                if pri < top_pri {
                    second_color = top_color;
                    second_layer = top_layer;
                    second_pri = top_pri;
                    top_color = color;
                    top_layer = bg as u32;
                    top_pri = pri;
                } else if pri < second_pri {
                    second_color = color;
                    second_layer = bg as u32;
                    second_pri = pri;
                }
            }

            let obj = self.obj_line[x];
            let obj_is_win = (obj & (1 << 21)) != 0;
            let obj_visible =
                (obj & PX_TRANSPARENT) == 0 && !obj_is_win && (mask & (1 << 4)) != 0;
            let obj_semi = (obj >> 20) & 1;
            if obj_visible {
                let pri = (obj >> 18) & 3;
                let color = obj & 0x7FFF;
                // OBJ wins ties against BGs at equal priority.
                if pri <= top_pri {
                    second_color = top_color;
                    second_layer = top_layer;
                    second_pri = top_pri;
                    top_color = color;
                    top_layer = 4;
                    top_pri = pri;
                } else if pri < second_pri {
                    second_color = color;
                    second_layer = 4;
                    second_pri = pri;
                }
            }
            let _ = (second_pri, top_pri);

            // Color special effect. Semi-transparent OBJ on top forces an alpha
            // blend (OBJ = target A, layer below = target B) regardless of the
            // BLDCNT effect mode. Bit 5 of the window mask gates all effects.
            let sfx_allowed = (mask & (1 << 5)) != 0;
            let mut final_color = top_color;
            if sfx_allowed {
                let semi_obj_on_top = top_layer == 4 && obj_semi != 0;
                if semi_obj_on_top && (target_b & (1 << second_layer)) != 0 {
                    final_color = bgr555_blend(top_color, second_color, eva, evb);
                } else if effect_mode == 1 {
                    if (target_a & (1 << top_layer)) != 0
                        && (target_b & (1 << second_layer)) != 0
                    {
                        final_color = bgr555_blend(top_color, second_color, eva, evb);
                    }
                } else if effect_mode == 2 {
                    if (target_a & (1 << top_layer)) != 0 {
                        final_color = fade_white(top_color, evy);
                    }
                } else if effect_mode == 3 {
                    if (target_a & (1 << top_layer)) != 0 {
                        final_color = fade_black(top_color, evy);
                    }
                }
            }

            bgr555_to_rgba(final_color, fb, off_base + x * 4);
        }
    }
}

// ─── Compositor support: slot dispatch + window geometry ─────────────────────

/// What a given BG slot is in the current DISPCNT BG mode (GBATEK §"DS Video
/// BG Modes"). An `Off` slot contributes nothing. Closed enum + exhaustive
/// `match` replaces the TS string union.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotKind {
    Text,
    Affine,
    Extended,
    LargeBitmap,
    Off,
}

/// Map (bg, bg_mode, is_engine_a) → slot kind. Mirrors ds-recomp `bgSlotKind`.
///   mode 0 — BG0..3 all text
///   mode 1 — BG0..2 text, BG3 affine
///   mode 2 — BG0..1 text, BG2/BG3 affine
///   mode 3 — BG0..2 text, BG3 extended
///   mode 4 — BG0..1 text, BG2 affine, BG3 extended
///   mode 5 — BG0..1 text, BG2/BG3 both extended
///   mode 6 — BG2 large bitmap only (Engine A; Off on B)
fn bg_slot_kind(bg: usize, bg_mode: u32, is_a: bool) -> SlotKind {
    if bg < 2 {
        if bg_mode == 6 {
            return SlotKind::Off;
        }
        return SlotKind::Text;
    }
    // BG2 / BG3.
    match bg_mode {
        0 => SlotKind::Text,
        6 => {
            if bg == 2 && is_a {
                SlotKind::LargeBitmap
            } else {
                SlotKind::Off
            }
        }
        1 => {
            if bg == 3 {
                SlotKind::Affine
            } else {
                SlotKind::Text
            }
        }
        2 => SlotKind::Affine,
        3 => {
            if bg == 2 {
                SlotKind::Text
            } else {
                SlotKind::Extended
            }
        }
        4 => {
            if bg == 2 {
                SlotKind::Affine
            } else {
                SlotKind::Extended
            }
        }
        // mode 5 (and any out-of-range, which can't happen for a 3-bit field).
        _ => SlotKind::Extended,
    }
}

/// Resolve the BG extended-palette slice for one BG layer, if ext palettes are
/// enabled. The ext-pal slot is the BG index, except text BG0/BG1 with BGxCNT
/// bit 13 set use slots 2/3 (GBATEK §"BG Extended Palettes"). Returns the flat
/// VRAM slice the renderer indexes by 256-color bank.
#[allow(clippy::too_many_arguments)]
fn bg_ext_pal_slice<'v>(
    is_a: bool,
    bg: usize,
    bg_cnt: u32,
    ext_pal_enabled: bool,
    router: &VramRouter,
    vramcnt: &[u8; 9],
    vram: &'v [u8],
) -> Option<&'v [u8]> {
    if !ext_pal_enabled {
        return None;
    }
    // BG0/BG1 can be remapped to ext-pal slots 2/3 via BGxCNT bit 13.
    let slot = if bg < 2 && (bg_cnt & 0x2000) != 0 {
        (bg as u32) + 2
    } else {
        bg as u32
    };
    let idx = if is_a {
        router.resolve_bg_ext_pal_a(slot, 0, vramcnt)
    } else {
        router.resolve_bg_ext_pal_b(slot, 0, vramcnt)
    };
    idx.map(|i| &vram[i..])
}

/// Whether scanline `y` is inside a window's vertical span. `v_reg`: bits 8..15
/// top, bits 0..7 bottom (exclusive). When bottom < top the region wraps.
#[inline]
fn row_inside_window(y: u32, v_reg: u32) -> bool {
    let bottom = v_reg & 0xFF;
    let top = (v_reg >> 8) & 0xFF;
    if top <= bottom {
        y >= top && y < bottom
    } else {
        y >= top || y < bottom
    }
}

/// Whether column `x` is inside a window's horizontal span. `left`/`right`
/// exclusive; wraps when right < left.
#[inline]
fn col_inside_window(x: u32, left: u32, right: u32) -> bool {
    if left <= right {
        x >= left && x < right
    } else {
        x >= left || x < right
    }
}

/// Read a little-endian u16 from `b` at byte offset `off`.
#[inline]
fn rd16(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8)
}

// ─── Shared color helpers (used by the compositor + renderer wave) ───────────

/// Expand a 15-bit BGR555 color into RGBA8888 at `out[off..off+4]`.
#[inline]
pub fn bgr555_to_rgba(bgr: u32, out: &mut [u8], off: usize) {
    let r = bgr & 0x1F;
    let g = (bgr >> 5) & 0x1F;
    let b = (bgr >> 10) & 0x1F;
    out[off] = ((r << 3) | (r >> 2)) as u8;
    out[off + 1] = ((g << 3) | (g >> 2)) as u8;
    out[off + 2] = ((b << 3) | (b >> 2)) as u8;
    out[off + 3] = 0xFF;
}

/// Alpha-blend two BGR555 colors with EVA/EVB coefficients (0..16), per the
/// DS BLDALPHA math. Adapted from the GBA core.
#[inline]
pub fn bgr555_blend(a: u32, b: u32, eva: u32, evb: u32) -> u32 {
    let ra = a & 0x1F;
    let ga = (a >> 5) & 0x1F;
    let ba = (a >> 10) & 0x1F;
    let rb = b & 0x1F;
    let gb = (b >> 5) & 0x1F;
    let bb = (b >> 10) & 0x1F;
    // DS hardware applies the EVA/EVB weights to the *sum* before the >>4, not
    // to each term independently (matches ds-recomp `alphaBlend`).
    let r = (((ra * eva) + (rb * evb)) >> 4).min(31);
    let g = (((ga * eva) + (gb * evb)) >> 4).min(31);
    let bl = (((ba * eva) + (bb * evb)) >> 4).min(31);
    (bl << 10) | (g << 5) | r
}

/// Fade a BGR555 color toward white. `evy` ∈ 0..16; each channel gains
/// `(31 - c) * evy / 16`. Mirrors ds-recomp `fadeWhite`.
#[inline]
fn fade_white(c: u32, evy: u32) -> u32 {
    let r = c & 0x1F;
    let g = (c >> 5) & 0x1F;
    let b = (c >> 10) & 0x1F;
    let nr = r + (((31 - r) * evy) >> 4);
    let ng = g + (((31 - g) * evy) >> 4);
    let nb = b + (((31 - b) * evy) >> 4);
    (nb << 10) | (ng << 5) | nr
}

/// Fade a BGR555 color toward black. `evy` ∈ 0..16; each channel loses
/// `c * evy / 16`. Mirrors ds-recomp `fadeBlack`.
#[inline]
fn fade_black(c: u32, evy: u32) -> u32 {
    let r = c & 0x1F;
    let g = (c >> 5) & 0x1F;
    let b = (c >> 10) & 0x1F;
    let nr = r - ((r * evy) >> 4);
    let ng = g - ((g * evy) >> 4);
    let nb = b - ((b * evy) >> 4);
    (nb << 10) | (ng << 5) | nr
}

/// Final post-process: MASTER_BRIGHT fades the whole RGBA frame toward white
/// (mode 1) or black (mode 2). Mode 0 is a no-op; mode 3 is "reserved" → forced
/// black on hardware. `reg`: factor in bits 0..4, mode in bits 14..15. Operates
/// on the 8-bit RGBA channels directly (alpha untouched). Mirrors ds-recomp
/// `applyMasterBrightness`.
fn apply_master_brightness(fb: &mut [u8], reg: u32) {
    let mode = (reg >> 14) & 0x3;
    if mode == 0 {
        return;
    }
    let factor = (reg & 0x1F).min(16);
    match mode {
        1 => {
            if factor == 0 {
                return;
            }
            for px in fb.chunks_exact_mut(4) {
                for c in px.iter_mut().take(3) {
                    let v = *c as u32;
                    *c = (v + (((255 - v) * factor) >> 4)) as u8;
                }
            }
        }
        2 => {
            if factor == 0 {
                return;
            }
            for px in fb.chunks_exact_mut(4) {
                for c in px.iter_mut().take(3) {
                    let v = *c as u32;
                    *c = (v - ((v * factor) >> 4)) as u8;
                }
            }
        }
        _ => {
            // Mode 3 reserved → black.
            for px in fb.chunks_exact_mut(4) {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────
//
// The per-layer renderers (text/affine/bitmap/sprites) are filled by parallel
// agents and currently `todo!()`, so these tests deliberately exercise only the
// COMPOSITOR + post-process pipeline: the display-mode dispatch (forced blank,
// LCDC, main-mem stub), backdrop, priority ordering, windows, BLDCNT blending,
// and MASTER_BRIGHT. They either pick display modes that never call a renderer,
// or seed the per-layer `bg_line`/`obj_line` buffers directly and call the
// private `composite_scanline` — the exact seam the renderer wave feeds. No
// test triggers a `todo!()` renderer.
#[cfg(test)]
mod tests {
    use super::*;

    // A packed BG pixel: BGR555 color | priority.
    fn bg_px(color: u32, priority: u32) -> u32 {
        (color & 0x7FFF) | (priority << 18)
    }
    // A packed OBJ pixel.
    fn obj_px(color: u32, priority: u32, semi: bool, win: bool) -> u32 {
        (color & 0x7FFF)
            | (4 << 16)
            | (priority << 18)
            | (if semi { 1 << 20 } else { 0 })
            | (if win { 1 << 21 } else { 0 })
    }

    // Decode an RGBA pixel back to BGR555 (the compositor's inverse — exact for
    // any color the compositor wrote, since both use the same 5→8 expand).
    fn rgba_to_bgr555(fb: &[u8], idx: usize) -> u32 {
        let r = (fb[idx] >> 3) as u32;
        let g = (fb[idx + 1] >> 3) as u32;
        let b = (fb[idx + 2] >> 3) as u32;
        (b << 10) | (g << 5) | r
    }

    fn blank_engine(kind: EngineKind) -> Engine {
        let mut e = Engine::new(kind);
        for bg in 0..4 {
            for px in e.bg_line[bg].iter_mut() {
                *px = PX_TRANSPARENT;
            }
        }
        for px in e.obj_line.iter_mut() {
            *px = PX_TRANSPARENT;
        }
        e
    }

    fn fresh_fb() -> Vec<u8> {
        vec![0u8; SCREEN_W * SCREEN_H * 4]
    }

    // ── display-mode dispatch ─────────────────────────────────────────────

    #[test]
    fn display_mode0_forced_blank_is_white() {
        let mut e = Engine::new(EngineKind::A);
        e.dispcnt = 0; // display mode 0 = off
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(&fb[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
        let last = fb.len() - 4;
        assert_eq!(&fb[last..], &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn display_mode3_main_mem_stub_is_black_opaque() {
        let mut e = Engine::new(EngineKind::A);
        e.dispcnt = 3 << 16;
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(&fb[0..4], &[0, 0, 0, 0xFF]);
    }

    #[test]
    fn display_mode2_lcdc_blits_vram_bank() {
        let mut e = Engine::new(EngineKind::A);
        e.dispcnt = (2 << 16) | (1 << 18); // LCDC, bank B (offset 0x20000)
        let mut mem = SharedMemory::new();
        let base = 0x20000;
        mem.vram[base] = 0x1F; // pixel (0,0) = pure red 0x001F
        mem.vram[base + 1] = 0x00;
        mem.vram[base + 2] = 0x00; // pixel (1,0) = pure blue 0x7C00
        mem.vram[base + 3] = 0x7C;
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(&fb[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(&fb[4..8], &[0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn master_bright_applies_after_lcdc() {
        let mut e = Engine::new(EngineKind::A);
        e.dispcnt = 2 << 16; // LCDC bank A
        e.master_bright = (1 << 14) | 16; // fade-to-white, full factor
        let mut mem = SharedMemory::new();
        mem.vram[0] = 0x00; // black pixel
        mem.vram[1] = 0x00;
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(&fb[0..3], &[0xFF, 0xFF, 0xFF]);
    }

    // ── graphics mode: backdrop-only (no renderer invoked) ────────────────

    #[test]
    fn graphics_mode_no_layers_shows_backdrop() {
        let mut e = Engine::new(EngineKind::A);
        e.dispcnt = 1 << 16; // graphics, no BGs/OBJ enabled
        let mut mem = SharedMemory::new();
        mem.pram[0] = 0xE0; // engine A backdrop = green 0x03E0
        mem.pram[1] = 0x03;
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x03E0);
        assert_eq!(fb[3], 0xFF);
    }

    #[test]
    fn engine_b_backdrop_reads_its_own_pram_half() {
        let mut e = Engine::new(EngineKind::B);
        e.dispcnt = 1 << 16;
        let mut mem = SharedMemory::new();
        mem.pram[0x400] = 0x00; // engine B backdrop = blue
        mem.pram[0x401] = 0x7C;
        let router = VramRouter::new();
        let mut fb = fresh_fb();
        e.render_frame(&mut fb, &mem, &router, &[0u8; 9]);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x7C00);
    }

    // ── compositor priority ordering ──────────────────────────────────────

    #[test]
    fn composite_picks_highest_priority_bg() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x001F, 2); // red, priority 2
        e.bg_line[1][0] = bg_px(0x7C00, 0); // blue, priority 0 (on top)
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0x0000, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x7C00);
    }

    #[test]
    fn composite_bg_index_breaks_priority_tie() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x001F, 1);
        e.bg_line[2][0] = bg_px(0x7C00, 1);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x001F); // BG0 wins the tie
    }

    #[test]
    fn composite_obj_beats_equal_priority_bg() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x001F, 1);
        e.obj_line[0] = obj_px(0x7C00, 1, false, false);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x7C00);
    }

    #[test]
    fn composite_transparent_layers_fall_through_to_backdrop() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0x03E0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x03E0);
    }

    // ── blending ──────────────────────────────────────────────────────────

    #[test]
    fn composite_alpha_blend_top_and_second() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x001F, 0); // red top (target A)
        e.bg_line[1][0] = bg_px(0x7C00, 1); // blue below (target B)
        e.bldcnt = (1 << 6) | 0x01 | (0x02 << 8); // mode 1, A=BG0, B=BG1
        e.bldalpha = 8 | (8 << 8);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), bgr555_blend(0x001F, 0x7C00, 8, 8));
    }

    #[test]
    fn composite_semi_transparent_obj_forces_blend() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.obj_line[0] = obj_px(0x001F, 0, true, false); // semi OBJ red on top
        e.bg_line[0][0] = bg_px(0x7C00, 2); // blue below
        e.bldcnt = 0x01 << 8; // effect mode 0; target B = BG0
        e.bldalpha = 8 | (8 << 8);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), bgr555_blend(0x001F, 0x7C00, 8, 8));
    }

    #[test]
    fn composite_brighten_and_darken() {
        // Brighten (mode 2).
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x0010, 0);
        e.bldcnt = (2 << 6) | 0x01; // mode 2, target A = BG0
        e.bldy = 16;
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), fade_white(0x0010, 16));

        // Darken (mode 3).
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x7FFF, 0);
        e.bldcnt = (3 << 6) | 0x01;
        e.bldy = 16;
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), fade_black(0x7FFF, 16));
    }

    #[test]
    fn composite_blend_requires_both_targets() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = 1 << 16;
        e.bg_line[0][0] = bg_px(0x001F, 0);
        e.bg_line[1][0] = bg_px(0x7C00, 1);
        e.bldcnt = (1 << 6) | 0x01; // target A = BG0, target B = none
        e.bldalpha = 8 | (8 << 8);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x001F); // unblended
    }

    // ── windows ───────────────────────────────────────────────────────────

    #[test]
    fn window0_masks_bg_outside_region() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = (1 << 16) | 0x2000; // graphics + WIN0
        e.win.h[0] = (4 << 8) | 8; // x in [4,8)
        e.win.v[0] = 192; // y in [0,192)
        e.win.win_in = 0x01; // inside: BG0 only
        e.win.win_out = 0x00; // outside: nothing
        for x in 0..16 {
            e.bg_line[0][x] = bg_px(0x001F, 0);
        }
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0x7C00, &mut fb); // backdrop blue
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x7C00); // outside → backdrop
        assert_eq!(rgba_to_bgr555(&fb, 5 * 4), 0x001F); // inside → BG0
        assert_eq!(rgba_to_bgr555(&fb, 9 * 4), 0x7C00); // outside again
    }

    #[test]
    fn window_blend_gate_blocks_effect() {
        let mut e = blank_engine(EngineKind::A);
        e.dispcnt = (1 << 16) | 0x2000;
        e.win.h[0] = (0 << 8) | 200; // x in [0,200) — includes x=0
        e.win.v[0] = 192;
        e.win.win_in = 0x1F; // BG0..3 + OBJ enabled, sfx (bit5) CLEAR
        e.win.win_out = 0x3F;
        e.bg_line[0][0] = bg_px(0x001F, 0);
        e.bg_line[1][0] = bg_px(0x7C00, 1);
        e.bldcnt = (1 << 6) | 0x01 | (0x02 << 8);
        e.bldalpha = 8 | (8 << 8);
        let mut fb = fresh_fb();
        e.composite_scanline(0, 0, &mut fb);
        assert_eq!(rgba_to_bgr555(&fb, 0), 0x001F); // sfx gated off → no blend
    }

    // ── slot-kind dispatch ────────────────────────────────────────────────

    #[test]
    fn slot_kinds_by_mode() {
        for bg in 0..4 {
            assert_eq!(bg_slot_kind(bg, 0, true), SlotKind::Text);
        }
        assert_eq!(bg_slot_kind(3, 1, true), SlotKind::Affine);
        assert_eq!(bg_slot_kind(2, 1, true), SlotKind::Text);
        assert_eq!(bg_slot_kind(2, 2, true), SlotKind::Affine);
        assert_eq!(bg_slot_kind(3, 2, true), SlotKind::Affine);
        assert_eq!(bg_slot_kind(2, 3, true), SlotKind::Text);
        assert_eq!(bg_slot_kind(3, 3, true), SlotKind::Extended);
        assert_eq!(bg_slot_kind(2, 4, true), SlotKind::Affine);
        assert_eq!(bg_slot_kind(3, 4, true), SlotKind::Extended);
        assert_eq!(bg_slot_kind(2, 5, true), SlotKind::Extended);
        assert_eq!(bg_slot_kind(3, 5, true), SlotKind::Extended);
        assert_eq!(bg_slot_kind(2, 6, true), SlotKind::LargeBitmap);
        assert_eq!(bg_slot_kind(2, 6, false), SlotKind::Off); // B never mode-6 bitmap
        assert_eq!(bg_slot_kind(0, 6, true), SlotKind::Off);
        assert_eq!(bg_slot_kind(3, 6, true), SlotKind::Off);
    }

    // ── window geometry helpers ───────────────────────────────────────────

    #[test]
    fn window_geometry_wrap_and_normal() {
        assert!(col_inside_window(15, 10, 20));
        assert!(!col_inside_window(20, 10, 20)); // right exclusive
        assert!(!col_inside_window(9, 10, 20));
        // wrapped: left 200 > right 10 → covers 200..255 and 0..10.
        assert!(col_inside_window(250, 200, 10));
        assert!(col_inside_window(5, 200, 10));
        assert!(!col_inside_window(100, 200, 10));
        assert!(row_inside_window(50, (10 << 8) | 100));
        assert!(!row_inside_window(150, (10 << 8) | 100));
        // wrapped row.
        assert!(row_inside_window(250, (200 << 8) | 10));
        assert!(row_inside_window(5, (200 << 8) | 10));
    }

    // ── post-process helpers ──────────────────────────────────────────────

    #[test]
    fn master_bright_modes() {
        let mut fb = vec![100u8, 100, 100, 0xFF];
        apply_master_brightness(&mut fb, 0); // mode 0 no-op
        assert_eq!(&fb[0..3], &[100, 100, 100]);

        let mut fb = vec![100u8, 50, 0, 0xFF];
        apply_master_brightness(&mut fb, (1 << 14) | 16); // white
        assert_eq!(&fb[0..3], &[255, 255, 255]);

        let mut fb = vec![200u8, 100, 50, 0xFF];
        apply_master_brightness(&mut fb, (2 << 14) | 16); // black
        assert_eq!(&fb[0..3], &[0, 0, 0]);

        let mut fb = vec![200u8, 100, 50, 0xFF];
        apply_master_brightness(&mut fb, 3 << 14); // mode 3 reserved → black
        assert_eq!(&fb[0..3], &[0, 0, 0]);
        assert_eq!(fb[3], 0xFF); // alpha untouched
    }

    #[test]
    fn alpha_blend_sums_before_shift() {
        // eva=evb=16, a=r16, b=r16 → r = min(31, (16*16+16*16)/16=32) = 31.
        assert_eq!(bgr555_blend(0x0010, 0x0010, 16, 16) & 0x1F, 31);
        // eva=evb=8, a=r31, b=r0 → r = (31*8)/16 = 15.
        assert_eq!(bgr555_blend(0x001F, 0x0000, 8, 8) & 0x1F, 15);
    }
}
