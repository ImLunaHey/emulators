//! Picture Processing Unit. Ported 1:1 from src/ppu/*.ts.
//!
//! This is the whole `src/ppu/` subsystem in one file: the main PPU
//! (dispcnt/dispstat/vcount, scanline timing, `step`, `write_reg`,
//! `read_dispstat`), the text/bitmap/affine BG renderers, the sprite
//! renderer, and the compositor. The shared per-scanline buffers
//! (`bg_line`, `obj_line`, `scanline`) live on `Ppu`, so keeping them
//! together avoids the cross-file borrow churn the TS split implied.
//!
//! Ownership model (per CONTRACT.md): the TS `Ppu` constructor received
//! `bus`, `irq`, `dma`. Those are NOT stored — they become `&mut`
//! parameters on `step`. The PPU reads VRAM/PRAM/OAM directly from `Mem`
//! (the raw `mem.vram`/`mem.pram`/`mem.oam` byte arrays are `pub`), raises
//! IRQs via `&mut Irq`, and triggers HBlank/VBlank DMA via `&mut Dma`
//! (whose trigger methods themselves take `bus`+`irq`).

use crate::bus::Mem;
use crate::irq::{Irq, IRQ_HBLANK, IRQ_VBLANK, IRQ_VCOUNT};

/// What a single `Ppu::step` produced that the orchestrator must act on:
/// whether an HBlank- and/or VBlank-timed DMA should fire this tick.
#[derive(Default, Clone, Copy)]
pub struct PpuTick {
    pub hblank: bool,
    pub vblank: bool,
}

// Cycle counts (1 dot = 4 CPU cycles).
const DOTS_VISIBLE: u32 = 240;
const DOTS_HBLANK: u32 = 68;
const DOTS_PER_LINE: u32 = DOTS_VISIBLE + DOTS_HBLANK;
#[allow(dead_code)]
const CYC_VISIBLE: u32 = DOTS_VISIBLE * 4;
const CYC_PER_LINE: u32 = DOTS_PER_LINE * 4;
const LINES_VISIBLE: u32 = 160;
const LINES_TOTAL: u32 = 228;

// Buffers per scanline — composer outputs into `frame` (RGBA8888).
// Layer pixel format (packed 32-bit):
//   bits 0..14   = BGR555 color
//   bit  15      = transparent
//   bits 16..17  = layer source (0..3 = BG0..3, 4 = OBJ, 5 = backdrop)
//   bits 18..19  = priority (0..3)
//   bit  20      = OBJ semi-transparent
//   bit  21      = OBJ window
// Render functions write a per-layer line buffer; compositor picks per pixel.

/// Read a little-endian u16 from a byte slice (mirrors the TS Uint16Array
/// views `pram16`/`vram16` which alias the underlying byte buffers).
#[inline]
fn rd16(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8)
}

/// PRAM is a flat u16 palette; index `i` gives entry `i` (TS `pram16[i]`).
#[inline]
fn pram16(pram: &[u8], i: usize) -> u32 {
    rd16(pram, i * 2)
}

pub struct Ppu {
    // PPU registers (we keep canonical copies; IO mirrors to raw too).
    pub dispcnt: u32,
    pub dispstat: u32,
    pub vcount: u32,
    pub bgcnt: [u32; 4],
    pub bg_hofs: [u32; 4],
    pub bg_vofs: [u32; 4],
    pub bg_x: [i32; 2], // BG2/3 reference X (28-bit signed)
    pub bg_y: [i32; 2], // BG2/3 reference Y
    pub bg_pa: [i32; 2],
    pub bg_pb: [i32; 2],
    pub bg_pc: [i32; 2],
    pub bg_pd: [i32; 2],
    pub win0_h: u32,
    pub win1_h: u32,
    pub win0_v: u32,
    pub win1_v: u32,
    pub win_in: u32,
    pub win_out: u32,
    pub mosaic: u32,
    pub bldcnt: u32,
    pub bldalpha: u32,
    pub bldy: u32,

    // RGBA frame buffer (240x160). Format: RGBA8888, one byte per channel.
    pub frame: Vec<u8>,
    pub scanline: Vec<u32>,
    pub bg_line: [Vec<u32>; 4],
    pub obj_line: Vec<u32>,

    pub cycles_accum: u32,
    pub in_hblank: bool,
    pub frame_done: bool,
    pub frame_count: u32,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            dispcnt: 0,
            dispstat: 0,
            vcount: 0,
            bgcnt: [0; 4],
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            bg_x: [0; 2],
            bg_y: [0; 2],
            bg_pa: [0; 2],
            bg_pb: [0; 2],
            bg_pc: [0; 2],
            bg_pd: [0; 2],
            win0_h: 0,
            win1_h: 0,
            win0_v: 0,
            win1_v: 0,
            win_in: 0,
            win_out: 0,
            mosaic: 0,
            bldcnt: 0,
            bldalpha: 0,
            bldy: 0,
            frame: vec![0; 240 * 160 * 4],
            scanline: vec![0; 240],
            bg_line: [vec![0; 240], vec![0; 240], vec![0; 240], vec![0; 240]],
            obj_line: vec![0; 240],
            cycles_accum: 0,
            in_hblank: false,
            frame_done: false,
            frame_count: 0,
        }
    }

    /// The 240x160 RGBA8888 framebuffer the host presents.
    pub fn framebuffer(&self) -> &[u8] {
        &self.frame
    }

    pub fn read_dispstat(&self) -> u32 {
        let mut v = self.dispstat & 0xFF38;
        if self.vcount >= LINES_VISIBLE && self.vcount != LINES_TOTAL - 1 {
            v |= 0x01;
        }
        if self.in_hblank {
            v |= 0x02;
        }
        if ((self.dispstat >> 8) & 0xFF) == self.vcount {
            v |= 0x04;
        }
        v
    }

    pub fn write_reg(&mut self, addr: u32, v: u32) {
        match addr {
            0x00 => {
                self.dispcnt = v;
            }
            0x04 => {
                // Only bits 3-7 and 8-15 of DISPSTAT are writable; status bits are RO.
                self.dispstat = (self.dispstat & 0x07) | (v & 0xFFF8);
            }
            0x08 => self.bgcnt[0] = v,
            0x0A => self.bgcnt[1] = v,
            0x0C => self.bgcnt[2] = v,
            0x0E => self.bgcnt[3] = v,
            0x10 => self.bg_hofs[0] = v & 0x1FF,
            0x12 => self.bg_vofs[0] = v & 0x1FF,
            0x14 => self.bg_hofs[1] = v & 0x1FF,
            0x16 => self.bg_vofs[1] = v & 0x1FF,
            0x18 => self.bg_hofs[2] = v & 0x1FF,
            0x1A => self.bg_vofs[2] = v & 0x1FF,
            0x1C => self.bg_hofs[3] = v & 0x1FF,
            0x1E => self.bg_vofs[3] = v & 0x1FF,
            0x20 => self.bg_pa[0] = ((v << 16) as i32) >> 16,
            0x22 => self.bg_pb[0] = ((v << 16) as i32) >> 16,
            0x24 => self.bg_pc[0] = ((v << 16) as i32) >> 16,
            0x26 => self.bg_pd[0] = ((v << 16) as i32) >> 16,
            0x28 => self.bg_x[0] = (self.bg_x[0] & (0xFFFF0000u32 as i32)) | (v as i32 & 0xFFFF),
            0x2A => {
                self.bg_x[0] = (self.bg_x[0] & 0xFFFF) | ((((v << 16) as i32) >> 16) << 16);
            }
            0x2C => self.bg_y[0] = (self.bg_y[0] & (0xFFFF0000u32 as i32)) | (v as i32 & 0xFFFF),
            0x2E => {
                self.bg_y[0] = (self.bg_y[0] & 0xFFFF) | ((((v << 16) as i32) >> 16) << 16);
            }
            0x30 => self.bg_pa[1] = ((v << 16) as i32) >> 16,
            0x32 => self.bg_pb[1] = ((v << 16) as i32) >> 16,
            0x34 => self.bg_pc[1] = ((v << 16) as i32) >> 16,
            0x36 => self.bg_pd[1] = ((v << 16) as i32) >> 16,
            0x38 => self.bg_x[1] = (self.bg_x[1] & (0xFFFF0000u32 as i32)) | (v as i32 & 0xFFFF),
            0x3A => {
                self.bg_x[1] = (self.bg_x[1] & 0xFFFF) | ((((v << 16) as i32) >> 16) << 16);
            }
            0x3C => self.bg_y[1] = (self.bg_y[1] & (0xFFFF0000u32 as i32)) | (v as i32 & 0xFFFF),
            0x3E => {
                self.bg_y[1] = (self.bg_y[1] & 0xFFFF) | ((((v << 16) as i32) >> 16) << 16);
            }
            0x40 => self.win0_h = v,
            0x42 => self.win1_h = v,
            0x44 => self.win0_v = v,
            0x46 => self.win1_v = v,
            0x48 => self.win_in = v,
            0x4A => self.win_out = v,
            0x4C => self.mosaic = v,
            0x50 => self.bldcnt = v,
            0x52 => self.bldalpha = v,
            0x54 => self.bldy = v & 0x1F,
            _ => {}
        }
    }

    // Advance PPU by `cycles` CPU cycles. Drives line transitions, HBlank
    // and VBlank IRQs, and renders each visible scanline.
    //
    // The TS `Ppu` held `bus`/`irq`/`dma`; here `mem` (VRAM/PRAM/OAM for
    // rendering) and `irq` are params. The HBlank/VBlank DMA triggers can't
    // run here (they re-enter the bus, which would mutably alias the PPU
    // while it's borrowed), so instead we REPORT them via the returned
    // `PpuTick`; the orchestrator fires the DMA after `step` returns. The
    // emulator batches CPU cycles to the scanline boundary, so at most one
    // HBlank and one VBlank transition occur per call.
    pub fn step(&mut self, cycles: u32, mem: &mut Mem, irq: &mut Irq) -> PpuTick {
        let mut tick = PpuTick::default();
        self.cycles_accum = self.cycles_accum.wrapping_add(cycles);
        while self.cycles_accum >= CYC_PER_LINE {
            self.cycles_accum -= CYC_PER_LINE;
            // We model the line as: render visible at start, then HBlank trigger.
            if self.vcount < LINES_VISIBLE {
                self.render_scanline(self.vcount, mem);
                self.in_hblank = true;
                if self.dispstat & 0x10 != 0 {
                    irq.raise(IRQ_HBLANK);
                }
                tick.hblank = true;
            }
            self.vcount += 1;
            if self.vcount == LINES_VISIBLE {
                self.in_hblank = false;
                self.frame_done = true;
                self.frame_count = self.frame_count.wrapping_add(1);
                if self.dispstat & 0x08 != 0 {
                    irq.raise(IRQ_VBLANK);
                }
                tick.vblank = true;
            } else if self.vcount >= LINES_TOTAL {
                self.vcount = 0;
                self.in_hblank = false;
                // Reload affine reference points at frame start.
                // (Strictly hardware reloads them at the end of VBlank.)
            } else {
                self.in_hblank = false;
            }
            // VCOUNT match.
            if ((self.dispstat >> 8) & 0xFF) == self.vcount && (self.dispstat & 0x20 != 0) {
                irq.raise(IRQ_VCOUNT);
            }
        }
        tick
    }

    fn render_scanline(&mut self, y: u32, mem: &Mem) {
        // Forced blank → white.
        if self.dispcnt & 0x80 != 0 {
            let off = (y * 240 * 4) as usize;
            for b in &mut self.frame[off..off + 240 * 4] {
                *b = 0xFF;
            }
            return;
        }
        // Backdrop from PRAM index 0.
        let backdrop = pram16(&mem.pram[..], 0) & 0x7FFF;
        let mode = self.dispcnt & 0x7;

        // Reset BG layer outputs (mark transparent).
        for b in 0..4 {
            for px in &mut self.bg_line[b] {
                *px = 0x8000;
            }
        }
        for px in &mut self.obj_line {
            *px = 0x8000;
        }

        if mode <= 2 {
            // Tile / text / affine modes — only the relevant BGs are valid.
            if mode == 0 {
                for b in 0..4 {
                    if self.dispcnt & (1 << (8 + b)) != 0 {
                        self.render_mode_text(b, y, mem);
                    }
                }
            } else if mode == 1 {
                if self.dispcnt & 0x100 != 0 {
                    self.render_mode_text(0, y, mem);
                }
                if self.dispcnt & 0x200 != 0 {
                    self.render_mode_text(1, y, mem);
                }
                // BG2 in mode 1 is AFFINE — different map layout (1 byte per
                // tile index, no palette/flip bits), always 8bpp tile data, and
                // sampled through the BGxPA..D matrix + reference point.
                if self.dispcnt & 0x400 != 0 {
                    self.render_mode_affine(2, y, mem);
                }
            } else {
                // Mode 2: BG2 and BG3 are both affine.
                if self.dispcnt & 0x400 != 0 {
                    self.render_mode_affine(2, y, mem);
                }
                if self.dispcnt & 0x800 != 0 {
                    self.render_mode_affine(3, y, mem);
                }
            }
        } else if mode == 3 {
            if self.dispcnt & 0x400 != 0 {
                self.render_mode_bitmap3(y, mem);
            }
        } else if mode == 4 {
            if self.dispcnt & 0x400 != 0 {
                self.render_mode_bitmap4(y, mem);
            }
        } else if mode == 5 {
            if self.dispcnt & 0x400 != 0 {
                self.render_mode_bitmap5(y, mem);
            }
        }

        if self.dispcnt & 0x1000 != 0 {
            self.render_sprites(y, mem);
        }

        self.composite_scanline(y, backdrop, mem);
    }

    // ---------------------------------------------------------------------
    // ---- text modes ----
    // ---------------------------------------------------------------------

    // Text-mode BG renderer for one scanline.
    // Outputs into ppu.bg_line[bg].
    fn render_mode_text(&mut self, bg: usize, y: u32, mem: &Mem) {
        const SIZE_W: [u32; 4] = [256, 512, 256, 512];
        const SIZE_H: [u32; 4] = [256, 256, 512, 512];

        let ctrl = self.bgcnt[bg];
        let priority = ctrl & 3;
        let char_base = ((ctrl >> 2) & 3) * 0x4000;
        let screen_base = ((ctrl >> 8) & 0x1F) * 0x800;
        let color_mode8 = (ctrl & 0x80) != 0;
        let mosaic_on = (ctrl & 0x40) != 0;
        let size_idx = ((ctrl >> 14) & 3) as usize;
        let map_w = SIZE_W[size_idx];
        let map_h = SIZE_H[size_idx];

        // BG mosaic: MOSAIC reg low nibble is the horizontal block size - 1,
        // second nibble is vertical block size - 1. The effect is a per-axis
        // "step" that quantizes sample coords to integer multiples of the
        // block size, producing the chunky pixelated look games use for
        // transitions / damage flashes.
        let mos_bg_h = if mosaic_on { (self.mosaic & 0x0F) + 1 } else { 1 };
        let mos_bg_v = if mosaic_on {
            ((self.mosaic >> 4) & 0x0F) + 1
        } else {
            1
        };

        let hofs = self.bg_hofs[bg];
        let vofs = self.bg_vofs[bg];
        let y_eff = ((if mosaic_on { y - (y % mos_bg_v) } else { y }) + vofs) & (map_h - 1);

        let layer_hi = ((bg as u32) << 16) | (priority << 18);
        let vram: &[u8] = &mem.vram[..];
        let pram: &[u8] = &mem.pram[..];

        for x in 0u32..240 {
            let x_mos = if mosaic_on { x - (x % mos_bg_h) } else { x };
            let x_eff = (x_mos + hofs) & (map_w - 1);

            // Map quadrant selection (32x32 tiles per quadrant).
            let mut map_off = screen_base;
            if map_w == 512 {
                map_off += if x_eff >= 256 { 0x800 } else { 0 };
            }
            if map_h == 512 {
                map_off += if y_eff >= 256 {
                    if map_w == 512 {
                        0x1000
                    } else {
                        0x800
                    }
                } else {
                    0
                };
            }

            let tile_x = (x_eff & 0xFF) >> 3;
            let tile_y = (y_eff & 0xFF) >> 3;
            let map_addr = (map_off + (tile_y * 32 + tile_x) * 2) as usize;
            let entry = (vram[map_addr] as u32) | ((vram[map_addr + 1] as u32) << 8);
            let tile_idx = entry & 0x3FF;
            let hflip = (entry & 0x400) != 0;
            let vflip = (entry & 0x800) != 0;
            let pal_bank = (entry >> 12) & 0xF;

            let mut in_tile_x = x_eff & 7;
            let mut in_tile_y = y_eff & 7;
            if hflip {
                in_tile_x = 7 - in_tile_x;
            }
            if vflip {
                in_tile_y = 7 - in_tile_y;
            }

            if color_mode8 {
                let tile_addr = char_base + tile_idx * 64 + in_tile_y * 8 + in_tile_x;
                if tile_addr >= 0x10000 {
                    self.bg_line[bg][x as usize] = 0x8000;
                    continue;
                } // BG can't access OBJ tile area
                let pix = vram[tile_addr as usize] as u32;
                if pix == 0 {
                    self.bg_line[bg][x as usize] = 0x8000;
                    continue;
                }
                self.bg_line[bg][x as usize] = (pram16(pram, pix as usize) & 0x7FFF) | layer_hi;
            } else {
                let tile_addr = char_base + tile_idx * 32 + in_tile_y * 4 + (in_tile_x >> 1);
                if tile_addr >= 0x10000 {
                    self.bg_line[bg][x as usize] = 0x8000;
                    continue;
                }
                let byte = vram[tile_addr as usize] as u32;
                let pix = if (in_tile_x & 1) != 0 {
                    byte >> 4
                } else {
                    byte & 0xF
                };
                if pix == 0 {
                    self.bg_line[bg][x as usize] = 0x8000;
                    continue;
                }
                self.bg_line[bg][x as usize] =
                    (pram16(pram, (pal_bank * 16 + pix) as usize) & 0x7FFF) | layer_hi;
            }
        }
    }

    // ---------------------------------------------------------------------
    // ---- affine BG mode ----
    // ---------------------------------------------------------------------

    // Affine BG renderer for Mode 1 BG2 and Mode 2 BG2/BG3.
    //
    // Affine BGs are ALWAYS 8bpp (1 byte per pixel = palette[0..255]).
    // The map is a flat byte array where each byte is a single tile index
    // (0..255). The map size is set by BGxCNT bits 14-15:
    //   00 = 128x128 px = 16x16 tiles
    //   01 = 256x256 px = 32x32 tiles
    //   10 = 512x512 px = 64x64 tiles
    //   11 = 1024x1024 px = 128x128 tiles
    //
    // Sampling uses the per-frame reference point (BGxX/BGxY, 28-bit signed
    // 8.8 fixed) plus the per-row affine matrix (pA/pB/pC/pD, 8.8 signed):
    //   src_x = pA * (px - 0) + pB * y + bgX
    //   src_y = pC * px         + pD * y + bgY
    // Each pixel: sample texel at (src_x >> 8, src_y >> 8) modulo map size.
    // (Real hardware updates the reference points across scanlines via the
    //  internal "current" registers, but for static affine BGs the per-line
    //  matrix application below is a close enough approximation.)
    fn render_mode_affine(&mut self, bg: usize, y: u32, mem: &Mem) {
        const AFFINE_SIZE_TILES: [i32; 4] = [16, 32, 64, 128];

        let ctrl = self.bgcnt[bg];
        let priority = ctrl & 3;
        let char_base = ((ctrl >> 2) & 3) * 0x4000;
        let screen_base = ((ctrl >> 8) & 0x1F) * 0x800;
        let size_idx = ((ctrl >> 14) & 3) as usize;
        let map_tiles = AFFINE_SIZE_TILES[size_idx];
        let map_px = map_tiles * 8;
        let wrap = (ctrl & 0x2000) != 0;

        let ref_idx = bg - 2; // 0 → BG2, 1 → BG3
        let p_a = self.bg_pa[ref_idx];
        let p_b = self.bg_pb[ref_idx];
        let p_c = self.bg_pc[ref_idx];
        let p_d = self.bg_pd[ref_idx];
        let ref_x = self.bg_x[ref_idx];
        let ref_y = self.bg_y[ref_idx];

        let layer_hi = ((bg as u32) << 16) | (priority << 18);
        let vram: &[u8] = &mem.vram[..];
        let pram: &[u8] = &mem.pram[..];

        // Compute starting source coords. Reference is 8.8 fixed-point signed.
        // src_x(0) = refX + 0*pA + y*pB
        // src_y(0) = refY + 0*pC + y*pD
        let mut src_x = ref_x.wrapping_add(p_b.wrapping_mul(y as i32));
        let mut src_y = ref_y.wrapping_add(p_d.wrapping_mul(y as i32));

        for x in 0usize..240 {
            let mut tx = src_x >> 8;
            let mut ty = src_y >> 8;
            src_x = src_x.wrapping_add(p_a);
            src_y = src_y.wrapping_add(p_c);

            if wrap {
                tx = ((tx % map_px) + map_px) % map_px;
                ty = ((ty % map_px) + map_px) % map_px;
            } else if tx < 0 || tx >= map_px || ty < 0 || ty >= map_px {
                self.bg_line[bg][x] = 0x8000;
                continue;
            }

            let tile_x = tx >> 3;
            let tile_y = ty >> 3;
            let in_tile_x = tx & 7;
            let in_tile_y = ty & 7;
            let map_addr = (screen_base as i32 + tile_y * map_tiles + tile_x) as usize;
            let tile_idx = vram[map_addr] as i32;
            // Affine BG tile data is 8bpp, 64 bytes per tile, addressed from
            // charBase. Unlike text mode there's no flip bit and palette is
            // implicitly bank 0.
            let tile_addr = char_base as i32 + tile_idx * 64 + in_tile_y * 8 + in_tile_x;
            if tile_addr >= 0x10000 {
                self.bg_line[bg][x] = 0x8000;
                continue;
            }
            let pix = vram[tile_addr as usize] as u32;
            if pix == 0 {
                self.bg_line[bg][x] = 0x8000;
                continue;
            }
            self.bg_line[bg][x] = (pram16(pram, pix as usize) & 0x7FFF) | layer_hi;
        }
    }

    // ---------------------------------------------------------------------
    // ---- bitmap modes (3/4/5) ----
    // ---------------------------------------------------------------------

    // BG2 in bitmap modes (3, 4, 5) is always AFFINE — same matrix +
    // reference-point math as Mode 1 BG2 / Mode 2 BG2-3, just with a flat
    // linear bitmap framebuffer instead of a tile+map fetch. Many homebrew
    // engines (Quake, voxel renderers, raycasters) set PA / PD to non-
    // identity values to stretch a sub-region of the bitmap across the
    // screen. Without applying the matrix, the unused regions of the
    // bitmap (which often contain leftover scratch data) leak through as
    // "noise" alongside the intended scene.
    //
    // All three modes use the same outer scaffold: walk per-pixel through
    // the affine source coords, then sample at (sx>>8, sy>>8) from the
    // mode-specific bitmap layout. The wrap bit (BG2CNT 0x2000) decides
    // whether out-of-range samples wrap or read transparent.

    // Mode 3: 240x160 BGR555 direct color, no double buffering.
    fn sample_mode3(&self, mem: &Mem, sx: i32, sy: i32, layer_hi: u32) -> u32 {
        if sx < 0 || sx >= 240 || sy < 0 || sy >= 160 {
            return 0x8000;
        }
        (rd16(&mem.vram[..],((sy * 240 + sx) as usize) * 2) & 0x7FFF) | layer_hi
    }

    // Mode 4: 240x160 paletted, double-buffered.
    fn sample_mode4(&self, mem: &Mem, sx: i32, sy: i32, layer_hi: u32) -> u32 {
        if sx < 0 || sx >= 240 || sy < 0 || sy >= 160 {
            return 0x8000;
        }
        let page = if self.dispcnt & 0x10 != 0 { 0xA000 } else { 0x0000 };
        let idx = mem.vram[(page + sy * 240 + sx) as usize] as u32;
        if idx == 0 {
            return 0x8000;
        }
        (pram16(&mem.pram[..], idx as usize) & 0x7FFF) | layer_hi
    }

    // Mode 5: 160x128 BGR555 direct, double-buffered.
    fn sample_mode5(&self, mem: &Mem, sx: i32, sy: i32, layer_hi: u32) -> u32 {
        if sx < 0 || sx >= 160 || sy < 0 || sy >= 128 {
            return 0x8000;
        }
        let page: i32 = if self.dispcnt & 0x10 != 0 { 0x5000 } else { 0x0000 };
        // TS indexes vram16 at (page>>>1) + sy*160 + sx → byte offset *2.
        (rd16(&mem.vram[..],(((page >> 1) + sy * 160 + sx) as usize) * 2) & 0x7FFF) | layer_hi
    }

    fn render_bitmap_affine(&mut self, y: u32, mem: &Mem, sampler: u8) {
        let layer_hi = (2u32 << 16) | ((self.bgcnt[2] & 3) << 18);
        let p_a = self.bg_pa[0];
        let p_b = self.bg_pb[0];
        let p_c = self.bg_pc[0];
        let p_d = self.bg_pd[0];
        let ref_x = self.bg_x[0];
        let ref_y = self.bg_y[0];
        // Per-scanline source coord: ref + Y * (pB, pD), then step by (pA, pC)
        // each pixel. Coords are 8.8 fixed, so sx >> 8 / sy >> 8 give the
        // bitmap-space integer sample position.
        let mut sx = ref_x.wrapping_add(p_b.wrapping_mul(y as i32));
        let mut sy = ref_y.wrapping_add(p_d.wrapping_mul(y as i32));
        for x in 0usize..240 {
            let v = match sampler {
                3 => self.sample_mode3(mem, sx >> 8, sy >> 8, layer_hi),
                4 => self.sample_mode4(mem, sx >> 8, sy >> 8, layer_hi),
                _ => self.sample_mode5(mem, sx >> 8, sy >> 8, layer_hi),
            };
            self.bg_line[2][x] = v;
            sx = sx.wrapping_add(p_a);
            sy = sy.wrapping_add(p_c);
        }
    }

    fn render_mode_bitmap3(&mut self, y: u32, mem: &Mem) {
        self.render_bitmap_affine(y, mem, 3);
    }
    fn render_mode_bitmap4(&mut self, y: u32, mem: &Mem) {
        self.render_bitmap_affine(y, mem, 4);
    }
    fn render_mode_bitmap5(&mut self, y: u32, mem: &Mem) {
        self.render_bitmap_affine(y, mem, 5);
    }

    // ---------------------------------------------------------------------
    // ---- sprites ----
    // ---------------------------------------------------------------------

    fn render_sprites(&mut self, y: u32, mem: &Mem) {
        // Sprite size table — indexed by (shape, size): width then height.
        const SIZE_W: [[i32; 4]; 3] =
            [[8, 16, 32, 64], [16, 32, 32, 64], [8, 8, 16, 32]];
        const SIZE_H: [[i32; 4]; 3] =
            [[8, 16, 32, 64], [8, 8, 16, 32], [16, 32, 32, 64]];

        let oam: &[u8] = &mem.oam[..];
        let vram: &[u8] = &mem.vram[..];
        let pram: &[u8] = &mem.pram[..];
        let obj_mapping_linear = (self.dispcnt & 0x40) != 0;

        let y = y as i32;

        for i in 0..128 {
            let base = i * 8;
            let a0 = (oam[base] as u32) | ((oam[base + 1] as u32) << 8);
            let a1 = (oam[base + 2] as u32) | ((oam[base + 3] as u32) << 8);
            let a2 = (oam[base + 4] as u32) | ((oam[base + 5] as u32) << 8);

            let mode = (a0 >> 10) & 3; // 0=normal, 1=semi-trans, 2=window, 3=prohibited
            let affine = (a0 & 0x100) != 0;
            let disabled_bit = !affine && (a0 & 0x200) != 0; // bit 9 only means "disabled" when bit 8 clear
            if disabled_bit {
                continue;
            }

            let shape = ((a0 >> 14) & 3) as usize;
            let size = ((a1 >> 14) & 3) as usize;
            if shape == 3 {
                continue;
            }
            let w = SIZE_W[shape][size];
            let h = SIZE_H[shape][size];

            // For affine sprites with bit 9 set ("double size"), the bounding box
            // on screen is 2x the sprite size (gives the matrix room for rotation).
            // The texel coords still range 0..w / 0..h — we just sample over a
            // wider screen window.
            let double_size = affine && (a0 & 0x200) != 0;
            let draw_w = if double_size { w * 2 } else { w };
            let draw_h = if double_size { h * 2 } else { h };

            let mut y_pos = (a0 & 0xFF) as i32;
            if y_pos >= 160 {
                y_pos -= 256;
            }
            if y < y_pos || y >= y_pos + draw_h {
                continue;
            }

            let mut x_pos = (a1 & 0x1FF) as i32;
            if x_pos >= 240 {
                x_pos -= 512;
            }
            if x_pos + draw_w <= 0 {
                continue;
            }

            let color8 = (a0 & 0x2000) != 0;
            let pal_bank = (a2 >> 12) & 0xF;
            let priority = (a2 >> 10) & 3;
            let tile_idx = (a2 & 0x3FF) as i32;
            // OAM mosaic: OBJ has its own block size in MOSAIC bits 8-15
            // (low nibble = horizontal-1, second nibble = vertical-1). When
            // a0 bit 12 is set, quantize the sample coords within the sprite
            // so the textured output looks chunky.
            let mosaic_on = (a0 & 0x1000) != 0;
            let mos_h = if mosaic_on {
                ((self.mosaic >> 8) & 0xF) as i32 + 1
            } else {
                1
            };
            let mos_v = if mosaic_on {
                ((self.mosaic >> 12) & 0xF) as i32 + 1
            } else {
                1
            };

            let semi = mode == 1;
            let obj_window = mode == 2;
            // No layer bits — OBJ pixels are identified by being in obj_line.
            // The OLD code had `4 << 16` here, but layer field is only 2 bits
            // (16-17), so the high bit of 4 spilled into bit 18 (priority's LSB)
            // and corrupted every sprite priority value: prio 0→1, prio 2→3, etc.
            // That manifested as sprites layering wrong vs BGs and other sprites.
            let layer_hi = (priority << 18)
                | (if semi { 1 << 20 } else { 0 })
                | (if obj_window { 1 << 21 } else { 0 });

            let tile_base: i32 = 0x10000;
            let tiles_per_tile = if color8 { 2 } else { 1 };
            let row_stride = if obj_mapping_linear {
                (w >> 3) * tiles_per_tile
            } else {
                32
            };

            // Affine path: bits 9-13 of a1 are the matrix index. Pull pA/pB/pC/pD
            // from the affine column bytes 6-7 of OAM entries 4*idx + [0..3].
            let mut p_a: i32 = 0x100;
            let mut p_b: i32 = 0;
            let mut p_c: i32 = 0;
            let mut p_d: i32 = 0x100; // identity (8.8 fixed)
            if affine {
                let mat_idx = ((a1 >> 9) & 0x1F) as usize;
                let mb = mat_idx * 32;
                p_a = ((((oam[mb + 6] as u32) | ((oam[mb + 7] as u32) << 8)) << 16) as i32) >> 16;
                p_b = ((((oam[mb + 14] as u32) | ((oam[mb + 15] as u32) << 8)) << 16) as i32) >> 16;
                p_c = ((((oam[mb + 22] as u32) | ((oam[mb + 23] as u32) << 8)) << 16) as i32) >> 16;
                p_d = ((((oam[mb + 30] as u32) | ((oam[mb + 31] as u32) << 8)) << 16) as i32) >> 16;
            }

            let in_sprite_y = y - y_pos;
            let cx = draw_w >> 1;
            let cy = draw_h >> 1;
            let half_w = w >> 1;
            let half_h = h >> 1;

            // Non-affine fast path: simple tile fetch with hflip/vflip.
            if !affine {
                let hflip_flag = (a1 & 0x1000) != 0;
                let vflip_flag = (a1 & 0x2000) != 0;
                // Apply OBJ mosaic by quantizing the sample coords within the
                // sprite to the configured block size before flip is applied.
                let mos_y = if mosaic_on {
                    in_sprite_y - (in_sprite_y % mos_v)
                } else {
                    in_sprite_y
                };
                let mut ty = mos_y;
                if vflip_flag {
                    ty = h - 1 - ty;
                }
                let tile_row = ty >> 3;
                let in_tile_y = ty & 7;
                for px in 0..w {
                    let screen_x = x_pos + px;
                    if screen_x < 0 || screen_x >= 240 {
                        continue;
                    }
                    let mos_x = if mosaic_on { px - (px % mos_h) } else { px };
                    let mut tx = mos_x;
                    if hflip_flag {
                        tx = w - 1 - tx;
                    }
                    let tile_col = tx >> 3;
                    let in_tile_x = tx & 7;
                    let base_tile = tile_idx + tile_row * row_stride + tile_col * tiles_per_tile;
                    let tile_addr = tile_base
                        + (base_tile & 0x3FF) * 32
                        + in_tile_y * (if color8 { 8 } else { 4 })
                        + (if color8 { in_tile_x } else { in_tile_x >> 1 });
                    if tile_addr >= 0x18000 {
                        continue;
                    }
                    let pix: u32;
                    if color8 {
                        pix = vram[tile_addr as usize] as u32;
                        if pix == 0 {
                            continue;
                        }
                    } else {
                        let byte = vram[tile_addr as usize] as u32;
                        pix = if (in_tile_x & 1) != 0 {
                            byte >> 4
                        } else {
                            byte & 0xF
                        };
                        if pix == 0 {
                            continue;
                        }
                    }
                    let cur = self.obj_line[screen_x as usize];
                    if (cur & 0x8000) == 0 && (((cur >> 18) & 3) <= priority) {
                        continue;
                    }
                    let pal_base = if color8 { 256 } else { 256 + pal_bank * 16 };
                    self.obj_line[screen_x as usize] =
                        (pram16(pram, (pal_base + pix) as usize) & 0x7FFF) | layer_hi;
                }
                continue;
            }

            // Affine path. Source coords are 8.8 fixed-point. For each screen
            // pixel (px, py) in the bounding box, compute:
            //   src_x = pA*(px - cx) + pB*(py - cy) + halfW (in 8.8)
            //   src_y = pC*(px - cx) + pD*(py - cy) + halfH
            // Then if (src_x, src_y) is in [0..w, 0..h) we sample the texel.
            let dy = in_sprite_y - cy;
            let mut src_x0 = (p_a.wrapping_mul(-cx).wrapping_add(p_b.wrapping_mul(dy)))
                .wrapping_add(half_w << 8);
            let mut src_y0 = (p_c.wrapping_mul(-cx).wrapping_add(p_d.wrapping_mul(dy)))
                .wrapping_add(half_h << 8);
            for px in 0..draw_w {
                let screen_x = x_pos + px;
                if screen_x < 0 || screen_x >= 240 {
                    src_x0 = src_x0.wrapping_add(p_a);
                    src_y0 = src_y0.wrapping_add(p_c);
                    continue;
                }
                let sx = src_x0 >> 8;
                let sy = src_y0 >> 8;
                src_x0 = src_x0.wrapping_add(p_a);
                src_y0 = src_y0.wrapping_add(p_c);
                if sx < 0 || sx >= w || sy < 0 || sy >= h {
                    continue;
                }
                let tile_col = sx >> 3;
                let tile_row = sy >> 3;
                let in_tile_x = sx & 7;
                let in_tile_y = sy & 7;
                let base_tile = tile_idx + tile_row * row_stride + tile_col * tiles_per_tile;
                let tile_addr = tile_base
                    + (base_tile & 0x3FF) * 32
                    + in_tile_y * (if color8 { 8 } else { 4 })
                    + (if color8 { in_tile_x } else { in_tile_x >> 1 });
                if tile_addr >= 0x18000 {
                    continue;
                }
                let pix: u32;
                if color8 {
                    pix = vram[tile_addr as usize] as u32;
                    if pix == 0 {
                        continue;
                    }
                } else {
                    let byte = vram[tile_addr as usize] as u32;
                    pix = if (in_tile_x & 1) != 0 {
                        byte >> 4
                    } else {
                        byte & 0xF
                    };
                    if pix == 0 {
                        continue;
                    }
                }
                let cur = self.obj_line[screen_x as usize];
                if (cur & 0x8000) == 0 && (((cur >> 18) & 3) <= priority) {
                    continue;
                }
                let pal_base = if color8 { 256 } else { 256 + pal_bank * 16 };
                self.obj_line[screen_x as usize] =
                    (pram16(pram, (pal_base + pix) as usize) & 0x7FFF) | layer_hi;
            }
        }
    }

    // ---------------------------------------------------------------------
    // ---- compositor ----
    // ---------------------------------------------------------------------

    // Compose layers into the final RGBA frame line.
    // Pixel encoding (32-bit):
    //   bits 0..14   BGR555
    //   bit 15       transparent
    //   bits 16..17  layer id (0..3 BG, 4 OBJ, 5 backdrop)
    //   bits 18..19  priority
    //   bit 20       OBJ semi-transparent
    //   bit 21       OBJ window
    fn composite_scanline(&mut self, y: u32, backdrop: u32, _mem: &Mem) {
        let off_base = (y * 240 * 4) as usize;

        let bldcnt = self.bldcnt;
        let blend_mode = (bldcnt >> 6) & 3;
        let top = bldcnt & 0x3F;
        let bot = (bldcnt >> 8) & 0x3F;
        let eva = (self.bldalpha & 0x1F).min(16);
        let evb = ((self.bldalpha >> 8) & 0x1F).min(16);
        let evy = (self.bldy & 0x1F).min(16);

        // Window enable bits in DISPCNT (13=WIN0, 14=WIN1, 15=OBJ_WIN).
        let win0_en = (self.dispcnt & 0x2000) != 0;
        let win1_en = (self.dispcnt & 0x4000) != 0;
        let obj_win_en = (self.dispcnt & 0x8000) != 0;
        let any_win_en = win0_en || win1_en || obj_win_en;
        // Window 0/1 bounds. H reg: bits 8-15 = X1, bits 0-7 = X2 (exclusive).
        // V reg: bits 8-15 = Y1, bits 0-7 = Y2 (exclusive). Hardware wraps oddly
        // for X2<X1 or Y2<Y1 cases — we approximate the common path.
        let w0x1 = (self.win0_h >> 8) & 0xFF;
        let w0x2 = self.win0_h & 0xFF;
        let w0y1 = (self.win0_v >> 8) & 0xFF;
        let w0y2 = self.win0_v & 0xFF;
        let w1x1 = (self.win1_h >> 8) & 0xFF;
        let w1x2 = self.win1_h & 0xFF;
        let w1y1 = (self.win1_v >> 8) & 0xFF;
        let w1y2 = self.win1_v & 0xFF;
        let win_in_bits = self.win_in;
        let win_out_bits = self.win_out;
        let w0_in_enable = win_in_bits & 0x3F; // layers + blend enabled inside WIN0
        let w1_in_enable = (win_in_bits >> 8) & 0x3F;
        let w_out_enable = win_out_bits & 0x3F;
        let w_obj_in_enable = (win_out_bits >> 8) & 0x3F;
        let y0 = y;
        let win0_hit = win0_en && y0 >= w0y1 && y0 < w0y2;
        let win1_hit = win1_en && y0 >= w1y1 && y0 < w1y2;

        for x in 0usize..240 {
            let xu = x as u32;
            // Determine which window region (if any) this pixel belongs to. Higher-
            // priority window: WIN0 > WIN1 > OBJ-window > outside.
            let mut allow_mask = 0x3F; // default: everything allowed
            if any_win_en {
                let in_w0 = win0_hit && xu >= w0x1 && xu < w0x2;
                let in_w1 = !in_w0 && win1_hit && xu >= w1x1 && xu < w1x2;
                let in_obj_win =
                    !in_w0 && !in_w1 && obj_win_en && (self.obj_line[x] & (1 << 21)) != 0;
                if in_w0 {
                    allow_mask = w0_in_enable;
                } else if in_w1 {
                    allow_mask = w1_in_enable;
                } else if in_obj_win {
                    allow_mask = w_obj_in_enable;
                } else {
                    allow_mask = w_out_enable;
                }
            }
            // allow_mask bits: 0..3 = BG0..3 enable, 4 = OBJ enable, 5 = blend enable.
            let bg_allow0 = (allow_mask & 0x01) != 0;
            let bg_allow1 = (allow_mask & 0x02) != 0;
            let bg_allow2 = (allow_mask & 0x04) != 0;
            let bg_allow3 = (allow_mask & 0x08) != 0;
            let obj_allow = (allow_mask & 0x10) != 0;
            let blend_allow = (allow_mask & 0x20) != 0;
            let mut best_color = backdrop;
            let mut best_prio = 4u32;
            let mut best_layer = 5u32;
            let mut best_semi = 0u32;

            for b in 0..4 {
                // Window-masked layer disable.
                if any_win_en {
                    if b == 0 && !bg_allow0 {
                        continue;
                    }
                    if b == 1 && !bg_allow1 {
                        continue;
                    }
                    if b == 2 && !bg_allow2 {
                        continue;
                    }
                    if b == 3 && !bg_allow3 {
                        continue;
                    }
                }
                let px = self.bg_line[b][x];
                if px & 0x8000 != 0 {
                    continue;
                }
                let prio = (px >> 18) & 3;
                if prio < best_prio || (prio == best_prio && (b as u32) < best_layer) {
                    best_prio = prio;
                    best_color = px & 0x7FFF;
                    best_layer = b as u32;
                    best_semi = 0;
                }
            }
            let obj = self.obj_line[x];
            let obj_is_obj_win = (obj & (1 << 21)) != 0;
            if (obj & 0x8000) == 0 && !obj_is_obj_win && (!any_win_en || obj_allow) {
                let prio = (obj >> 18) & 3;
                if prio <= best_prio {
                    // OBJ wins the top pixel; best_prio isn't read past this
                    // point, so we only record the color/layer/semi.
                    best_color = obj & 0x7FFF;
                    best_layer = 4;
                    best_semi = (obj >> 20) & 1;
                }
            }

            // Find next-best for blending.
            let mut bot1_color = backdrop;
            let mut bot1_layer = 5u32;
            let mut bot1_prio = 4u32;
            for b in 0..4 {
                if b as u32 == best_layer {
                    continue;
                }
                if any_win_en {
                    if b == 0 && !bg_allow0 {
                        continue;
                    }
                    if b == 1 && !bg_allow1 {
                        continue;
                    }
                    if b == 2 && !bg_allow2 {
                        continue;
                    }
                    if b == 3 && !bg_allow3 {
                        continue;
                    }
                }
                let px = self.bg_line[b][x];
                if px & 0x8000 != 0 {
                    continue;
                }
                let prio = (px >> 18) & 3;
                if prio < bot1_prio || (prio == bot1_prio && (b as u32) < bot1_layer) {
                    bot1_prio = prio;
                    bot1_color = px & 0x7FFF;
                    bot1_layer = b as u32;
                }
            }
            if best_layer != 4
                && (obj & 0x8000) == 0
                && !obj_is_obj_win
                && (!any_win_en || obj_allow)
            {
                let prio = (obj >> 18) & 3;
                if prio < bot1_prio || (prio == bot1_prio && 4 < bot1_layer) {
                    // bot1_prio isn't read past this point; record color/layer.
                    bot1_color = obj & 0x7FFF;
                    bot1_layer = 4;
                }
            }

            let mut color = best_color;
            let top_mask = 1u32 << best_layer;
            let bot_mask = 1u32 << bot1_layer;
            let top_set = (top & top_mask) != 0;
            let bot_set = (bot & bot_mask) != 0;

            // Inside a window, blending is gated by the window's blend-enable bit.
            if any_win_en && !blend_allow {
                // Skip blending — just emit the top color.
            } else if best_semi != 0 && bot_set {
                color = bgr555_blend(best_color, bot1_color, eva, evb);
            } else if blend_mode == 1 && top_set && bot_set {
                color = bgr555_blend(best_color, bot1_color, eva, evb);
            } else if blend_mode == 2 && top_set {
                // Brighten toward white.
                let r = best_color & 0x1F;
                let g = (best_color >> 5) & 0x1F;
                let b = (best_color >> 10) & 0x1F;
                let r2 = r + (((31 - r) * evy) >> 4);
                let g2 = g + (((31 - g) * evy) >> 4);
                let b2 = b + (((31 - b) * evy) >> 4);
                color = (b2 << 10) | (g2 << 5) | r2;
            } else if blend_mode == 3 && top_set {
                // Darken toward black.
                let r = best_color & 0x1F;
                let g = (best_color >> 5) & 0x1F;
                let b = (best_color >> 10) & 0x1F;
                let r2 = r - ((r * evy) >> 4);
                let g2 = g - ((g * evy) >> 4);
                let b2 = b - ((b * evy) >> 4);
                color = (b2 << 10) | (g2 << 5) | r2;
            }

            bgr555_to_rgba(color, &mut self.frame, off_base + x * 4);
        }
    }
}

// ---- compositor helpers ----

fn bgr555_to_rgba(bgr: u32, out: &mut [u8], off: usize) {
    let r = bgr & 0x1F;
    let g = (bgr >> 5) & 0x1F;
    let b = (bgr >> 10) & 0x1F;
    out[off] = ((r << 3) | (r >> 2)) as u8;
    out[off + 1] = ((g << 3) | (g >> 2)) as u8;
    out[off + 2] = ((b << 3) | (b >> 2)) as u8;
    out[off + 3] = 0xFF;
}

fn bgr555_blend(a: u32, b: u32, eva: u32, evb: u32) -> u32 {
    let ra = a & 0x1F;
    let ga = (a >> 5) & 0x1F;
    let ba = (a >> 10) & 0x1F;
    let rb = b & 0x1F;
    let gb = (b >> 5) & 0x1F;
    let bb = (b >> 10) & 0x1F;
    let r = (((ra * eva) >> 4) + ((rb * evb) >> 4)).min(31);
    let g = (((ga * eva) >> 4) + ((gb * evb) >> 4)).min(31);
    let bl = (((ba * eva) >> 4) + ((bb * evb) >> 4)).min(31);
    (bl << 10) | (g << 5) | r
}

// =====================================================================
// Tests — ported from the (deleted) TypeScript PPU suite:
//   src/test/bg.test.ts, sprites.test.ts, composite.test.ts
//
// The TS tests poked PPU internals directly (`ppu.bgLine[b]`,
// `renderModeText`, `fillTile4bpp` writing `ppu.bus.vram`) and called
// the per-layer render functions in isolation. Here the render helpers
// are private methods on `Ppu`, but in-module `#[cfg(test)]` code can
// reach them. Each scanline-level test builds a bare `Ppu` + `Mem`,
// seeds VRAM/PRAM/OAM + registers, calls the matching private renderer,
// and asserts the same per-layer / framebuffer values the TS test did.
//
// Two fully self-contained GOLDEN-FRAME tests at the end drive the real
// `Gba::run_frame()` (no external ROM) and lock framebuffer pixels.
// =====================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::Gba;

    // --- helpers mirroring the TS test harness ------------------------

    // Write a u16 into a little-endian byte buffer at logical index `i`
    // (i.e. byte offset `i*2`). Mirrors the TS `Uint16Array` views
    // (`pram16[i]`, `vram16[i]`).
    fn put16(buf: &mut [u8], i: usize, v: u16) {
        buf[i * 2] = (v & 0xFF) as u8;
        buf[i * 2 + 1] = (v >> 8) as u8;
    }

    // Expand a 5-bit BGR555 component to 8-bit exactly as `bgr555_to_rgba`.
    fn expand5(c: u32) -> u8 {
        ((c << 3) | (c >> 2)) as u8
    }
    // Expected RGB for a BGR555 color.
    fn rgb(bgr: u32) -> (u8, u8, u8) {
        (
            expand5(bgr & 0x1F),
            expand5((bgr >> 5) & 0x1F),
            expand5((bgr >> 10) & 0x1F),
        )
    }

    // A fresh PPU whose per-layer buffers are pre-filled transparent —
    // the real `render_scanline` does this before invoking layer
    // renderers; the TS tests mirrored it (`ppu.bgLine[b].fill(0x8000)`).
    fn fresh() -> (Ppu, Mem) {
        let mut ppu = Ppu::new();
        for b in 0..4 {
            for px in &mut ppu.bg_line[b] {
                *px = 0x8000;
            }
        }
        for px in &mut ppu.obj_line {
            *px = 0x8000;
        }
        (ppu, Mem::new())
    }

    // Fill a 4bpp BG tile (32 bytes, charBase 0) with one pixel value.
    fn fill_tile4bpp_bg(mem: &mut Mem, tile_slot: usize, v: u8) {
        let base = tile_slot * 32;
        let byte = v | (v << 4);
        for i in 0..32 {
            mem.vram[base + i] = byte;
        }
    }

    // Fill a 4bpp OBJ tile (OBJ tile area starts at VRAM 0x10000).
    fn fill_tile4bpp_obj(mem: &mut Mem, tile_slot: usize, v: u8) {
        let base = 0x10000 + tile_slot * 32;
        let byte = v | (v << 4);
        for i in 0..32 {
            mem.vram[base + i] = byte;
        }
    }

    fn set_oam(mem: &mut Mem, idx: usize, a0: u16, a1: u16, a2: u16) {
        let off = idx * 8;
        mem.oam[off] = (a0 & 0xFF) as u8;
        mem.oam[off + 1] = (a0 >> 8) as u8;
        mem.oam[off + 2] = (a1 & 0xFF) as u8;
        mem.oam[off + 3] = (a1 >> 8) as u8;
        mem.oam[off + 4] = (a2 & 0xFF) as u8;
        mem.oam[off + 5] = (a2 >> 8) as u8;
    }

    // Bitmap modes need the BIOS identity affine matrix; the scanline-
    // level tests skip BIOS init so we seed PA/PD = 0x100 like the TS
    // `setBitmapIdentity` helper.
    fn set_bitmap_identity(ppu: &mut Ppu) {
        ppu.bg_pa[0] = 0x100;
        ppu.bg_pb[0] = 0;
        ppu.bg_pc[0] = 0;
        ppu.bg_pd[0] = 0x100;
        ppu.bg_x[0] = 0;
        ppu.bg_y[0] = 0;
    }

    // ============ BG text-mode tests (bg.test.ts) ==================

    #[test]
    fn bg_text_renders_single_tile_at_offset0() {
        let (mut ppu, mut mem) = fresh();
        ppu.bgcnt[0] = 1 << 8; // mapBase = 1 (= 0x800)
        fill_tile4bpp_bg(&mut mem, 0, 1);
        mem.vram[0x800] = 0;
        mem.vram[0x801] = 0;
        put16(&mut mem.pram[..], 1, 0x7FFF); // BG palette entry 1 = white

        ppu.render_mode_text(0, 0, &mem);

        for x in 0..8 {
            assert_eq!(ppu.bg_line[0][x] & 0x8000, 0, "pixel {x} should be opaque");
        }
    }

    #[test]
    fn bg_text_hflip_swaps_halves() {
        let (mut ppu, mut mem) = fresh();
        ppu.bgcnt[0] = 1 << 8;
        // Left half (pixels 0-3) = value 1, right half = value 2.
        for row in 0..8 {
            mem.vram[row * 4] = 0x11;
            mem.vram[row * 4 + 1] = 0x11;
            mem.vram[row * 4 + 2] = 0x22;
            mem.vram[row * 4 + 3] = 0x22;
        }
        put16(&mut mem.pram[..], 1, 0x7C00); // blue
        put16(&mut mem.pram[..], 2, 0x03E0); // green
        // Map entry: tile 0, hflip=1 (bit 10 → byte1 bit 2 = 0x04).
        mem.vram[0x800] = 0;
        mem.vram[0x801] = 0x04;
        ppu.render_mode_text(0, 0, &mem);
        // hflip: pixel 0 reads original pixel 7 = green.
        assert_eq!(ppu.bg_line[0][0] & 0x7FFF, 0x03E0);
        assert_eq!(ppu.bg_line[0][7] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn bg_text_respects_hofs() {
        let (mut ppu, mut mem) = fresh();
        ppu.bgcnt[0] = 1 << 8;
        ppu.bg_hofs[0] = 4;
        fill_tile4bpp_bg(&mut mem, 0, 1);
        fill_tile4bpp_bg(&mut mem, 1, 2);
        put16(&mut mem.pram[..], 1, 0x7C00);
        put16(&mut mem.pram[..], 2, 0x03E0);
        mem.vram[0x800] = 0;
        mem.vram[0x801] = 0;
        mem.vram[0x802] = 1;
        mem.vram[0x803] = 0;
        ppu.render_mode_text(0, 0, &mem);
        assert_eq!(ppu.bg_line[0][0] & 0x7FFF, 0x7C00);
        assert_eq!(ppu.bg_line[0][3] & 0x7FFF, 0x7C00);
        assert_eq!(ppu.bg_line[0][4] & 0x7FFF, 0x03E0);
        assert_eq!(ppu.bg_line[0][11] & 0x7FFF, 0x03E0);
    }

    #[test]
    fn bg_text_respects_vofs() {
        let (mut ppu, mut mem) = fresh();
        ppu.bgcnt[0] = 1 << 8;
        ppu.bg_vofs[0] = 8;
        // Map entry at the second row (mapBase 0x800, row stride 64 bytes).
        mem.vram[0x800 + 64] = 5;
        mem.vram[0x800 + 65] = 0;
        fill_tile4bpp_bg(&mut mem, 5, 3);
        put16(&mut mem.pram[..], 3, 0x7FFF);
        ppu.render_mode_text(0, 0, &mem);
        assert_eq!(ppu.bg_line[0][0] & 0x7FFF, 0x7FFF);
    }

    #[test]
    fn bg_text_palette_bank_4bpp() {
        let (mut ppu, mut mem) = fresh();
        ppu.bgcnt[0] = 1 << 8;
        fill_tile4bpp_bg(&mut mem, 0, 1);
        put16(&mut mem.pram[..], 1, 0x001F); // bank 0 entry 1 = red
        put16(&mut mem.pram[..], 3 * 16 + 1, 0x7C00); // bank 3 entry 1 = blue
        // Map entry: tile 0, pal bank 3 (high nibble of byte1 = 0x30).
        mem.vram[0x800] = 0;
        mem.vram[0x801] = 0x30;
        ppu.render_mode_text(0, 0, &mem);
        assert_eq!(ppu.bg_line[0][0] & 0x7FFF, 0x7C00);
    }

    // ============ Bitmap mode tests (bg.test.ts) ===================

    #[test]
    fn bitmap4_renders_palette_indexed() {
        let (mut ppu, mut mem) = fresh();
        set_bitmap_identity(&mut ppu);
        put16(&mut mem.pram[..], 5, 0x03E0); // green
        ppu.dispcnt = 0;
        mem.vram[30 * 240 + 10] = 5;
        ppu.render_mode_bitmap4(30, &mem);
        assert_eq!(ppu.bg_line[2][10] & 0x7FFF, 0x03E0);
    }

    #[test]
    fn bitmap4_page_select() {
        let (mut ppu, mut mem) = fresh();
        set_bitmap_identity(&mut ppu);
        put16(&mut mem.pram[..], 7, 0x7C00); // blue
        ppu.dispcnt = 0x10; // page 1 active
        mem.vram[0xA000 + 50 * 240 + 5] = 7;
        ppu.render_mode_bitmap4(50, &mem);
        assert_eq!(ppu.bg_line[2][5] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn bitmap4_affine_pa_scaling() {
        // PA=0x80 → half-step: bitmap pixel x=5 lands at screen x=10.
        let (mut ppu, mut mem) = fresh();
        set_bitmap_identity(&mut ppu);
        ppu.bg_pa[0] = 0x80;
        put16(&mut mem.pram[..], 3, 0x7C00);
        mem.vram[30 * 240 + 5] = 3;
        ppu.render_mode_bitmap4(30, &mem);
        assert_eq!(ppu.bg_line[2][10] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn bitmap3_direct_color() {
        let (mut ppu, mut mem) = fresh();
        set_bitmap_identity(&mut ppu);
        // Pixel (10,30) = green direct BGR555.
        mem.vram[(30 * 240 + 10) * 2] = 0xE0;
        mem.vram[(30 * 240 + 10) * 2 + 1] = 0x03;
        ppu.render_mode_bitmap3(30, &mem);
        assert_eq!(ppu.bg_line[2][10] & 0x7FFF, 0x03E0);
    }

    // ============ Sprite tests (sprites.test.ts) ===================
    //
    // The TS harness reset objLine then called renderSprites; we do the
    // same. dispcnt 0x40 = 1D OBJ mapping (no BG/OBJ enable bits needed —
    // render_sprites doesn't check the OBJ-enable bit, that's in
    // render_scanline).

    fn render_sprites_at(ppu: &mut Ppu, mem: &Mem, y: u32) {
        for px in &mut ppu.obj_line {
            *px = 0x8000;
        }
        ppu.render_sprites(y, mem);
    }

    #[test]
    fn sprite_16x16_contiguous() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..4 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        set_oam(&mut mem, 0, 0x0032, 0x400A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 58);
        for x in 10..26 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
        assert_ne!(ppu.obj_line[9] & 0x8000, 0);
        assert_ne!(ppu.obj_line[26] & 0x8000, 0);
    }

    #[test]
    fn sprite_32x32_no_gaps() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..16 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        set_oam(&mut mem, 0, 0x0032, 0x800A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 60);
        for x in 10..42 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
    }

    #[test]
    fn sprite_32x32_8bpp_contiguous() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        // 8bpp: every byte = pixel value 1; 16 tiles = 32 4bpp slots.
        for t in 0..32 {
            let base = 0x10000 + t * 32;
            for i in 0..32 {
                mem.vram[base + i] = 1;
            }
        }
        set_oam(&mut mem, 0, 0x2032, 0x800A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 60);
        for x in 10..42 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
    }

    #[test]
    fn sprite_hflip_mirrors() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7C00); // red (pix 1)
        put16(&mut mem.pram[..], 256 + 2, 0x03E0); // green (pix 2)
        // 8x8 tile: left half = 1, right half = 2.
        let base = 0x10000;
        for row in 0..8 {
            mem.vram[base + row * 4] = 0x11;
            mem.vram[base + row * 4 + 1] = 0x11;
            mem.vram[base + row * 4 + 2] = 0x22;
            mem.vram[base + row * 4 + 3] = 0x22;
        }
        set_oam(&mut mem, 0, 0x0032, 0x000A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 54);
        assert_eq!(ppu.obj_line[10] & 0x7FFF, 0x7C00);
        assert_eq!(ppu.obj_line[17] & 0x7FFF, 0x03E0);
        // Flip horizontally (a1 bit 12).
        set_oam(&mut mem, 0, 0x0032, 0x100A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 54);
        assert_eq!(ppu.obj_line[10] & 0x7FFF, 0x03E0);
        assert_eq!(ppu.obj_line[17] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn sprite_affine_identity_matches_nonaffine() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..4 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        // Identity matrix 0 (pA=pD=0x100).
        put16(&mut mem.oam[..], 0 * 4 + 3, 0x0100);
        put16(&mut mem.oam[..], 1 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 2 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 3 * 4 + 3, 0x0100);
        set_oam(&mut mem, 0, 0x0132, 0x400A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 58);
        for x in 10..26 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
    }

    #[test]
    fn sprite_affine_double_size_full() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..4 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        put16(&mut mem.oam[..], 0 * 4 + 3, 0x0100);
        put16(&mut mem.oam[..], 1 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 2 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 3 * 4 + 3, 0x0100);
        set_oam(&mut mem, 0, 0x0332, 0x400A, 0x0000); // double-size affine
        render_sprites_at(&mut ppu, &mem, 66);
        // Center 16 px opaque; corners sample outside → transparent.
        for x in 18..34 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
        assert_ne!(ppu.obj_line[10] & 0x8000, 0);
        assert_ne!(ppu.obj_line[42] & 0x8000, 0);
    }

    #[test]
    fn sprite_affine_2x_scale() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..4 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        // pA=pD=0x80 → 2x zoom.
        put16(&mut mem.oam[..], 0 * 4 + 3, 0x0080);
        put16(&mut mem.oam[..], 1 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 2 * 4 + 3, 0x0000);
        put16(&mut mem.oam[..], 3 * 4 + 3, 0x0080);
        set_oam(&mut mem, 0, 0x0332, 0x400A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 66);
        let mut count = 0;
        for x in 10..42 {
            if ppu.obj_line[x] & 0x8000 == 0 {
                count += 1;
            }
        }
        assert!(count >= 30, "expected >=30 opaque pixels, got {count}");
    }

    #[test]
    fn sprite_64x64() {
        let (mut ppu, mut mem) = fresh();
        ppu.dispcnt = 0x40;
        put16(&mut mem.pram[..], 256 + 1, 0x7FFF);
        for t in 0..64 {
            fill_tile4bpp_obj(&mut mem, t, 1);
        }
        set_oam(&mut mem, 0, 0x0032, 0xC00A, 0x0000);
        render_sprites_at(&mut ppu, &mem, 70);
        for x in 10..74 {
            assert_eq!(ppu.obj_line[x] & 0x8000, 0, "x={x} should be opaque");
        }
    }

    // ============ Compositor tests (composite.test.ts) =============
    //
    // The TS tests set bgLine/objLine pixels directly then called
    // compositeScanline. We replicate. Pixel encoding:
    //   color (0..14) | (prio << 18); OBJ pixels add (semi << 20).

    const RED: u32 = 0x001F;
    const GREEN: u32 = 0x03E0;
    const BLUE: u32 = 0x7C00;
    const WHITE: u32 = 0x7FFF;
    const BLACK: u32 = 0x0000;

    fn bg_pixel(color: u32, prio: u32) -> u32 {
        (color & 0x7FFF) | (prio << 18)
    }
    fn obj_pixel(color: u32, prio: u32, semi: u32) -> u32 {
        (color & 0x7FFF) | (prio << 18) | (semi << 20)
    }
    fn pixel_at(ppu: &Ppu, x: usize, y: usize) -> (u8, u8, u8) {
        let off = (y * 240 + x) * 4;
        (ppu.frame[off], ppu.frame[off + 1], ppu.frame[off + 2])
    }

    #[test]
    fn comp_bg0_prio0_over_bg1_prio1() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 0);
        ppu.bg_line[1][50] = bg_pixel(GREEN, 1);
        ppu.composite_scanline(0, BLACK, &mem);
        let (r, g, _) = pixel_at(&ppu, 50, 0);
        assert!(r > 200);
        assert!(g < 20);
    }

    #[test]
    fn comp_same_prio_lower_bg_wins() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 1);
        ppu.bg_line[2][50] = bg_pixel(GREEN, 1);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).0 > 200);
    }

    #[test]
    fn comp_priority_beats_bg_index() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 1);
        ppu.bg_line[2][50] = bg_pixel(GREEN, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).1 > 200);
    }

    #[test]
    fn comp_transparent_bg_falls_to_backdrop() {
        let (mut ppu, mut mem) = fresh();
        put16(&mut mem.pram[..], 0, WHITE as u16);
        ppu.composite_scanline(0, WHITE, &mem);
        let (r, g, b) = pixel_at(&ppu, 50, 0);
        assert!(r > 240 && g > 240 && b > 240);
    }

    #[test]
    fn comp_obj_ties_win_over_bg() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 1);
        ppu.obj_line[50] = obj_pixel(GREEN, 1, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).1 > 200);
    }

    #[test]
    fn comp_obj_prio2_loses_to_bg_prio1() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 1);
        ppu.obj_line[50] = obj_pixel(GREEN, 2, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        let (r, g, _) = pixel_at(&ppu, 50, 0);
        assert!(r > 200);
        assert!(g < 20);
    }

    #[test]
    fn comp_obj_prio0_wins_all() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 0);
        ppu.bg_line[1][50] = bg_pixel(GREEN, 0);
        ppu.obj_line[50] = obj_pixel(BLUE, 0, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).2 > 200);
    }

    #[test]
    fn comp_obj_prio3_loses_to_bg3_prio2() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[3][50] = bg_pixel(RED, 2);
        ppu.obj_line[50] = obj_pixel(GREEN, 3, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).0 > 200);
    }

    #[test]
    fn comp_transparent_obj_falls_to_bg() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(RED, 1);
        ppu.composite_scanline(0, BLACK, &mem);
        assert!(pixel_at(&ppu, 50, 0).0 > 200);
    }

    #[test]
    fn comp_semi_transparent_obj_blends() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[2][50] = bg_pixel(RED, 2);
        ppu.obj_line[50] = obj_pixel(BLUE, 1, 1);
        ppu.bldcnt = 0x10 | (0x4 << 8);
        ppu.bldalpha = 8 | (8 << 8);
        ppu.composite_scanline(0, BLACK, &mem);
        let (r, _, b) = pixel_at(&ppu, 50, 0);
        assert!(r > 50);
        assert!(b > 50);
    }

    #[test]
    fn comp_brighten_mode2() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(0x000F, 0);
        ppu.bldcnt = (2 << 6) | 0x01;
        ppu.bldy = 8;
        ppu.composite_scanline(0, BLACK, &mem);
        let r = pixel_at(&ppu, 50, 0).0;
        assert!(r > 170 && r < 220, "brighten r={r}");
    }

    #[test]
    fn comp_darken_mode3() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][50] = bg_pixel(WHITE, 0);
        ppu.bldcnt = (3 << 6) | 0x01;
        ppu.bldy = 8;
        ppu.composite_scanline(0, BLACK, &mem);
        let r = pixel_at(&ppu, 50, 0).0;
        assert!(r > 100 && r < 150, "darken r={r}");
    }

    #[test]
    fn comp_win0_inside_outside() {
        let (mut ppu, mut mem) = fresh();
        for x in 0..240 {
            ppu.bg_line[0][x] = bg_pixel(RED, 0);
        }
        ppu.dispcnt = 0x2000; // WIN0 on
        ppu.win0_h = (50 << 8) | 100;
        ppu.win0_v = 160; // y [0,160)
        ppu.win_in = 0x01;
        ppu.win_out = 0x00;
        put16(&mut mem.pram[..], 0, WHITE as u16);
        ppu.composite_scanline(10, WHITE, &mem);
        let inside = pixel_at(&ppu, 75, 10);
        assert!(inside.0 > 240 && inside.1 < 20);
        let outside = pixel_at(&ppu, 25, 10);
        assert!(outside.0 > 240 && outside.1 > 240 && outside.2 > 240);
    }

    #[test]
    fn comp_win0_vertical_clip() {
        let (mut ppu, mut mem) = fresh();
        for x in 0..240 {
            ppu.bg_line[0][x] = bg_pixel(RED, 0);
        }
        ppu.dispcnt = 0x2000;
        ppu.win0_h = 240; // full X
        ppu.win0_v = (50 << 8) | 100; // y [50,100)
        ppu.win_in = 0x01;
        ppu.win_out = 0x00;
        put16(&mut mem.pram[..], 0, WHITE as u16);
        ppu.composite_scanline(75, WHITE, &mem);
        assert!(pixel_at(&ppu, 100, 75).0 > 240);
        ppu.composite_scanline(110, WHITE, &mem);
        let p = pixel_at(&ppu, 100, 110);
        assert!(p.0 > 240 && p.1 > 240);
    }

    #[test]
    fn comp_win0_priority_over_win1() {
        let (mut ppu, mut mem) = fresh();
        for x in 0..240 {
            ppu.bg_line[0][x] = bg_pixel(RED, 0);
        }
        ppu.dispcnt = 0x6000; // WIN0 + WIN1
        ppu.win0_h = (10 << 8) | 50;
        ppu.win0_v = 160;
        ppu.win1_h = (40 << 8) | 100;
        ppu.win1_v = 160;
        ppu.win_in = 0x01; // WIN0 allows BG0, WIN1 allows nothing
        ppu.win_out = 0x00;
        put16(&mut mem.pram[..], 0, WHITE as u16);
        ppu.composite_scanline(10, WHITE, &mem);
        // x=45 in both → WIN0 wins, BG0 visible (red).
        let a = pixel_at(&ppu, 45, 10);
        assert!(a.0 > 240 && a.1 < 20);
        // x=75 only in WIN1 → backdrop (white).
        assert!(pixel_at(&ppu, 75, 10).1 > 240);
    }

    #[test]
    fn comp_bgr555_red() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][0] = bg_pixel(0x001F, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        let p = pixel_at(&ppu, 0, 0);
        assert!(p.0 > 240 && p.1 == 0 && p.2 == 0);
    }

    #[test]
    fn comp_bgr555_green() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][0] = bg_pixel(0x03E0, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        let p = pixel_at(&ppu, 0, 0);
        assert!(p.0 == 0 && p.1 > 240 && p.2 == 0);
    }

    #[test]
    fn comp_bgr555_blue() {
        let (mut ppu, mem) = fresh();
        ppu.bg_line[0][0] = bg_pixel(0x7C00, 0);
        ppu.composite_scanline(0, BLACK, &mem);
        let p = pixel_at(&ppu, 0, 0);
        assert!(p.0 == 0 && p.1 == 0 && p.2 > 240);
    }

    // ============ GOLDEN-FRAME regression tests ====================
    // Fully self-contained: no external ROM. Drive the real `Gba` end
    // to end through `run_frame()` and lock framebuffer pixels.

    fn setup_gba() -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        g
    }

    #[test]
    fn golden_frame_mode3() {
        let mut g = setup_gba();
        // DISPCNT = Mode 3 + BG2 enable.
        Bus::write16(&mut g, 0x0400_0000, 0x0403);
        // Deterministic BGR555 pattern at known offsets.
        let pixels: [(usize, u32); 4] = [
            (0, 0x7FFF),                 // (0,0) white
            (10, 0x001F),                // (10,0) red
            (240 + 20, 0x03E0),          // (20,1) green
            (2 * 240 + 30, 0x7C00),      // (30,2) blue
        ];
        for &(idx, color) in &pixels {
            put16(&mut g.mem.vram[..], idx, color as u16);
        }
        g.run_frame();
        let fb = g.framebuffer();
        for &(idx, color) in &pixels {
            let o = idx * 4;
            let (er, eg, eb) = rgb(color);
            assert_eq!(
                (fb[o], fb[o + 1], fb[o + 2]),
                (er, eg, eb),
                "mode3 pixel at vram idx {idx} (color {color:#06x})"
            );
        }
    }

    #[test]
    fn golden_frame_mode0() {
        let mut g = setup_gba();
        // DISPCNT = Mode 0 + BG0 enable (bit 8).
        Bus::write16(&mut g, 0x0400_0000, 0x0100);
        // BG0CNT: charBase 0, mapBase 1 (= 0x800), prio 0, 4bpp.
        Bus::write16(&mut g, 0x0400_0008, 1 << 8);
        // One 4bpp tile (slot 0) entirely pixel value 1.
        {
            let byte = 0x11u8;
            for i in 0..32 {
                g.mem.vram[i] = byte;
            }
        }
        // Map entry 0: tile 0, no flip, pal bank 0.
        g.mem.vram[0x800] = 0;
        g.mem.vram[0x801] = 0;
        // BG palette entry 1 = green.
        put16(&mut g.mem.pram[..], 1, 0x03E0);
        g.run_frame();
        let fb = g.framebuffer();
        let (er, eg, eb) = rgb(0x03E0);
        // First tile covers screen pixels (0..8, 0..8).
        for &x in &[0usize, 3, 7] {
            let o = x * 4;
            assert_eq!(
                (fb[o], fb[o + 1], fb[o + 2]),
                (er, eg, eb),
                "mode0 pixel x={x}"
            );
        }
        // Pixel (0,7) — last row of the tile — also green.
        let o = (7 * 240) * 4;
        assert_eq!((fb[o], fb[o + 1], fb[o + 2]), (er, eg, eb), "mode0 (0,7)");
    }
}
