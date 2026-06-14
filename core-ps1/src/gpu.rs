//! GPU — software triangle rasterizer over the 1 MB VRAM.
//!
//! Built from psx-spx "Graphics Processing Unit (GPU)". The web target has no
//! hardware GPU, so this is a pure-software rasterizer: GP0 rendering commands
//! draw into a 1024x512 16bpp ("15-bit RGB", bit 15 = mask) VRAM, and the
//! configured display area is exposed to the host as an RGBA8888 framebuffer.
//!
//! Four memory-mapped ports (psx-spx), addressed from the GPU window base
//! 0x1F80_1810 — `off` here is relative to that base:
//!
//! | off | write       | read              |
//! |-----|-------------|-------------------|
//! | 0x0 | GP0  (draw / VRAM-transfer commands) | GPUREAD (VRAM→CPU / register replies) |
//! | 0x4 | GP1  (display control) | GPUSTAT (status register) |
//!
//! ## GP0 command machine
//!
//! GP0 words arrive one at a time. The first word of a primitive carries the
//! opcode in bits 24..31; the rasterizer counts how many parameter words the
//! primitive needs, buffers them, then draws. The variable-length primitives
//! (gouraud/textured polygons, polylines, CPU↔VRAM image transfers) are decoded
//! as the words stream in.
//!
//! ## Rasterization
//!
//! Triangles use the standard edge-function half-space test with barycentric
//! interpolation for gouraud colors and texture coordinates. Quads are split
//! into two triangles. Everything is clipped to the drawing area
//! (E3h..E4h) and offset by the drawing offset (E5h). Colors live in VRAM as
//! 1555 (BGR555 + mask bit); the framebuffer expander converts the display
//! window to RGBA8888.

use crate::irq::{Interrupt, Irq};

/// VRAM geometry: 1024 halfwords (2048 bytes) per line, 512 lines = 1 MB.
pub const VRAM_W: usize = 1024;
pub const VRAM_H: usize = 512;
pub const VRAM_HALFWORDS: usize = VRAM_W * VRAM_H;

/// GPUSTAT value after a GPU reset (`GP1(00h)`), per psx-spx. Bits 26/27/28
/// ("ready to receive command / send VRAM / receive DMA") are set so the BIOS
/// boot poll-loops see an idle, ready GPU and proceed.
const GPUSTAT_RESET: u32 = 0x1480_2000;

/// GPUSTAT bits we keep recomputing / toggling.
const STAT_IRQ1: u32 = 1 << 24; // GP0(1Fh) IRQ request pending
const STAT_VRAM_READY: u32 = 1 << 27; // VRAM→CPU data ready
const STAT_INTERLACE_FIELD: u32 = 1 << 31; // drawing even/odd line (toggles per frame)

/// A side of the GP0 transfer FIFO state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gp0Mode {
    /// Waiting for the next command word.
    Command,
    /// Streaming the words of a multi-word rendering primitive.
    Params,
    /// Streaming pixel data into VRAM (CPU→VRAM, GP0(A0h)).
    CpuToVram,
}

/// A pending CPU↔VRAM image transfer rectangle, in halfword coordinates.
#[derive(Debug, Clone, Copy, Default)]
struct Transfer {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    /// Current write/read cursor within the rectangle.
    cx: u16,
    cy: u16,
}

/// Decoded draw-mode (texpage) attribute — GP0(E1h) / per-poly texpage word.
#[derive(Debug, Clone, Copy, Default)]
struct DrawMode {
    /// Texture page X base in halfword units (`field * 64`).
    page_x: u16,
    /// Texture page Y base in line units (`bit4*256 (+ bit11*512)`).
    page_y: u16,
    /// Semi-transparency mode 0..3.
    semi: u8,
    /// Texture color depth: 0 = 4bpp, 1 = 8bpp, 2 = 15bpp, 3 = reserved.
    tex_depth: u8,
}

/// A rasterizer vertex: signed screen position, packed BGR color, and UV.
#[derive(Debug, Clone, Copy, Default)]
struct Vertex {
    x: i32,
    y: i32,
    /// 8-bit per channel, packed 0x00BBGGRR (the GP0 color word, low 24 bits).
    color: u32,
    u: u8,
    v: u8,
}

/// The software GPU: VRAM plus the command/display register state.
pub struct Gpu {
    /// 1 MB VRAM as 16bpp halfwords (1024x512). Heap-boxed — never on the stack.
    pub vram: Box<[u16; VRAM_HALFWORDS]>,
    /// Latched GPUSTAT (0x1F80_1814 read). Drives the BIOS ready/idle polls.
    pub gpustat: u32,
    /// Last GPUREAD reply value (0x1F80_1810 read), e.g. a GP1(10h) info reply
    /// or a word streamed out of a VRAM→CPU transfer.
    pub gpuread: u32,

    // ---- display area (programmed by GP1 commands) ----
    /// Display VRAM start, in halfword columns / lines.
    pub display_x: u16,
    pub display_y: u16,
    /// Display area width/height in pixels (used to size the framebuffer).
    pub display_w: u16,
    pub display_h: u16,
    /// True for 24bpp display mode (else 15bpp). The framebuffer expander reads
    /// VRAM accordingly.
    pub display_24bpp: bool,

    /// RGBA8888 expansion of the display area, rebuilt by [`Gpu::render_frame`].
    /// `VRAM_W * VRAM_H` capacity so any display window fits without realloc.
    pub framebuffer: Box<[u32; VRAM_HALFWORDS]>,

    // ---- GP0 command machine ----
    mode: Gp0Mode,
    /// Buffered parameter words of the in-flight primitive (word 0 = command).
    fifo: Vec<u32>,
    /// How many words the in-flight primitive still expects (incl. the command).
    expected: usize,
    /// True while consuming a polyline (variable length, ends on 0x55..5/...).
    polyline: bool,
    transfer: Transfer,

    // ---- drawing state (GP0 attribute commands) ----
    draw_mode: DrawMode,
    /// CLUT base (set by texpage Y reuse) — kept per-primitive instead.
    /// Texture window mask/offset (8-pixel units), GP0(E2h).
    tw_mask_x: u8,
    tw_mask_y: u8,
    tw_off_x: u8,
    tw_off_y: u8,
    /// Drawing area clip rectangle (E3h top-left, E4h bottom-right), inclusive.
    draw_x0: i32,
    draw_y0: i32,
    draw_x1: i32,
    draw_y1: i32,
    /// Drawing offset (E5h), added to every vertex.
    off_x: i32,
    off_y: i32,
    /// Mask-bit behaviour (E6h): force bit15 set, and skip masked pixels.
    set_mask: bool,
    check_mask: bool,

    // ---- scanline/VBLANK timing ----
    /// Free-running scanline cycle accumulator (GPU clocks).
    scanline_cycle: u32,
    /// Current scanline within the frame.
    scanline: u16,
    /// True when a VBLANK edge occurred since the last `step` poll — the
    /// orchestrator drains this to raise the VBLANK IRQ.
    vblank_pending: bool,
}

/// GPU clocks per scanline / scanlines per frame (NTSC, non-interlaced).
const CYCLES_PER_SCANLINE: u32 = 3413;
const SCANLINES_PER_FRAME: u16 = 263;
const VBLANK_START_LINE: u16 = 240;

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpu {
    pub fn new() -> Self {
        Gpu {
            vram: vec![0u16; VRAM_HALFWORDS]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            gpustat: GPUSTAT_RESET,
            gpuread: 0,
            display_x: 0,
            display_y: 0,
            display_w: 320,
            display_h: 240,
            display_24bpp: false,
            framebuffer: vec![0u32; VRAM_HALFWORDS]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            mode: Gp0Mode::Command,
            fifo: Vec::with_capacity(16),
            expected: 0,
            polyline: false,
            transfer: Transfer::default(),
            draw_mode: DrawMode::default(),
            tw_mask_x: 0,
            tw_mask_y: 0,
            tw_off_x: 0,
            tw_off_y: 0,
            draw_x0: 0,
            draw_y0: 0,
            draw_x1: VRAM_W as i32 - 1,
            draw_y1: VRAM_H as i32 - 1,
            off_x: 0,
            off_y: 0,
            set_mask: false,
            check_mask: false,
            scanline_cycle: 0,
            scanline: 0,
            vblank_pending: false,
        }
    }

    // ============================ port I/O ============================

    /// Read a GPU port. `off` is relative to the GPU window base 0x1F80_1810:
    /// 0x0 = GPUREAD, 0x4 = GPUSTAT.
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.read_gpuread(),
            0x4 => self.gpustat,
            _ => 0,
        }
    }

    /// Write a GPU port. `off` is relative to 0x1F80_1810: 0x0 = GP0 (draw /
    /// VRAM transfer), 0x4 = GP1 (display control).
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0 => self.gp0(v),
            0x4 => self.gp1(v),
            _ => {}
        }
    }

    /// GPUREAD: either replies a latched register (GP1(10h)) or streams the next
    /// halfword pair out of a VRAM→CPU transfer.
    fn read_gpuread(&mut self) -> u32 {
        if self.mode == Gp0Mode::Command && self.gpustat & STAT_VRAM_READY != 0 {
            // No active VRAM→CPU read; just hand back the latched reply.
            return self.gpuread;
        }
        self.gpuread
    }

    // ============================ GP0 ============================

    /// GP0 — rendering and VRAM-access command/parameter words. Feeds the
    /// command FIFO state machine; complete primitives are rasterized.
    pub fn gp0(&mut self, word: u32) {
        match self.mode {
            Gp0Mode::CpuToVram => self.push_cpu_to_vram(word),
            Gp0Mode::Command => self.gp0_command(word),
            Gp0Mode::Params => {
                self.fifo.push(word);
                if self.polyline {
                    // Polyline ends when a vertex word matches 0x5000_5000 in
                    // its top nibbles (and we already have the first vertex).
                    if self.fifo.len() >= 2 && (word & 0xF000_F000) == 0x5000_5000 {
                        self.polyline = false;
                        self.draw_polyline();
                        self.finish_command();
                    }
                } else if self.fifo.len() >= self.expected {
                    self.execute_primitive();
                    self.finish_command();
                }
            }
        }
    }

    /// Fully reset the GP0 machine back to idle command mode (GP1(01h), reset).
    fn reset_command(&mut self) {
        self.mode = Gp0Mode::Command;
        self.fifo.clear();
        self.expected = 0;
        self.polyline = false;
    }

    /// Finish the in-flight primitive: clear the FIFO bookkeeping but DON'T force
    /// command mode — a transfer primitive (CPU↔VRAM) may have switched the mode
    /// to keep streaming data words.
    fn finish_command(&mut self) {
        self.fifo.clear();
        self.expected = 0;
        self.polyline = false;
        if self.mode == Gp0Mode::Params {
            self.mode = Gp0Mode::Command;
        }
    }

    /// First word of a GP0 primitive: classify by opcode (bits 24..31), set up
    /// the expected parameter count, or execute zero-param attribute commands.
    fn gp0_command(&mut self, word: u32) {
        let op = word >> 24;
        match op {
            0x00 => {} // NOP
            0x01 => {} // clear texture cache — no cache modeled
            0x02 => self.begin_params(word, 3), // VRAM fill
            0x1F => self.gpustat |= STAT_IRQ1,  // IRQ1 request
            0x20..=0x3F => self.begin_polygon(word),
            0x40..=0x5F => self.begin_line(word),
            0x60..=0x7F => self.begin_rect(word),
            0x80..=0x9F => self.begin_params(word, 4), // VRAM→VRAM copy
            0xA0..=0xBF => self.begin_params(word, 3), // CPU→VRAM
            0xC0..=0xDF => self.begin_params(word, 3), // VRAM→CPU
            0xE1 => self.set_draw_mode(word),
            0xE2 => self.set_texture_window(word),
            0xE3 => self.set_draw_area_tl(word),
            0xE4 => self.set_draw_area_br(word),
            0xE5 => self.set_draw_offset(word),
            0xE6 => self.set_mask_bits(word),
            _ => {} // unknown / unimplemented attribute -> ignore
        }
    }

    /// Begin a fixed-length primitive, buffering the command word.
    fn begin_params(&mut self, word: u32, words: usize) {
        self.fifo.clear();
        self.fifo.push(word);
        self.expected = words;
        if words <= 1 {
            self.execute_primitive();
            self.finish_command();
        } else {
            self.mode = Gp0Mode::Params;
        }
    }

    /// Polygon (0x20..0x3F): word count depends on shading/quad/texture bits.
    fn begin_polygon(&mut self, word: u32) {
        let gouraud = word & (1 << 28) != 0;
        let quad = word & (1 << 27) != 0;
        let textured = word & (1 << 26) != 0;
        let verts = if quad { 4 } else { 3 };
        // Per vertex: position (1) + color (gouraud, but first uses command
        // word's color) + uv (textured).
        let mut words = 1; // command/color word
        for i in 0..verts {
            if gouraud && i > 0 {
                words += 1; // color word
            }
            words += 1; // vertex word
            if textured {
                words += 1; // uv/clut/page word
            }
        }
        self.begin_params(word, words);
    }

    /// Line (0x40..0x5F): 2 verts for a single line; polylines are open-ended.
    fn begin_line(&mut self, word: u32) {
        let gouraud = word & (1 << 28) != 0;
        let polyline = word & (1 << 27) != 0;
        self.fifo.clear();
        self.fifo.push(word);
        self.mode = Gp0Mode::Params;
        if polyline {
            self.polyline = true;
            self.expected = 0;
        } else {
            // single line: v0 (+c1) v1 — first color from command word.
            self.expected = if gouraud { 4 } else { 3 };
        }
    }

    /// Rectangle (0x60..0x7F): vertex always; size word only for variable size;
    /// uv word only when textured.
    fn begin_rect(&mut self, word: u32) {
        let size = (word >> 27) & 3;
        let textured = word & (1 << 26) != 0;
        let mut words = 1; // command/color
        words += 1; // vertex
        if textured {
            words += 1; // uv/clut
        }
        if size == 0 {
            words += 1; // width/height
        }
        self.begin_params(word, words);
    }

    // ---- attribute commands ----

    fn set_draw_mode(&mut self, word: u32) {
        self.draw_mode.page_x = ((word & 0xF) * 64) as u16;
        let mut py = if word & (1 << 4) != 0 { 256u16 } else { 0 };
        if word & (1 << 11) != 0 {
            py += 512;
        }
        self.draw_mode.page_y = py & (VRAM_H as u16 - 1);
        self.draw_mode.semi = ((word >> 5) & 3) as u8;
        self.draw_mode.tex_depth = ((word >> 7) & 3) as u8;
        // GPUSTAT bits 0..10 mirror E1h (bits 0..9) + bit 15 = E1h.11.
        self.gpustat = (self.gpustat & !0x0000_07FF) | (word & 0x0000_07FF);
        self.gpustat = (self.gpustat & !(1 << 15)) | (((word >> 11) & 1) << 15);
    }

    fn set_texture_window(&mut self, word: u32) {
        self.tw_mask_x = (word & 0x1F) as u8;
        self.tw_mask_y = ((word >> 5) & 0x1F) as u8;
        self.tw_off_x = ((word >> 10) & 0x1F) as u8;
        self.tw_off_y = ((word >> 15) & 0x1F) as u8;
    }

    fn set_draw_area_tl(&mut self, word: u32) {
        self.draw_x0 = (word & 0x3FF) as i32;
        self.draw_y0 = ((word >> 10) & 0x1FF) as i32;
    }

    fn set_draw_area_br(&mut self, word: u32) {
        self.draw_x1 = (word & 0x3FF) as i32;
        self.draw_y1 = ((word >> 10) & 0x1FF) as i32;
    }

    fn set_draw_offset(&mut self, word: u32) {
        // 11-bit signed each.
        self.off_x = sign_extend(word & 0x7FF, 11);
        self.off_y = sign_extend((word >> 11) & 0x7FF, 11);
    }

    fn set_mask_bits(&mut self, word: u32) {
        self.set_mask = word & 1 != 0;
        self.check_mask = word & 2 != 0;
        self.gpustat = (self.gpustat & !(3 << 11)) | ((word & 3) << 11);
    }

    // ============================ primitive dispatch ============================

    fn execute_primitive(&mut self) {
        let op = self.fifo[0] >> 24;
        match op {
            0x02 => self.fill_rect(),
            0x20..=0x3F => self.draw_polygon(),
            0x40..=0x5F => self.draw_line(),
            0x60..=0x7F => self.draw_rect(),
            0x80..=0x9F => self.vram_to_vram(),
            0xA0..=0xBF => self.begin_cpu_to_vram(),
            0xC0..=0xDF => self.begin_vram_to_cpu(),
            _ => {}
        }
    }

    // ---- VRAM fill (02h) — ignores mask/clip, raw 24bpp→15bpp ----
    fn fill_rect(&mut self) {
        let color = self.fifo[0] & 0x00FF_FFFF;
        let xy = self.fifo[1];
        let wh = self.fifo[2];
        let x0 = (xy & 0x3F0) as usize; // steps of 0x10
        let y0 = ((xy >> 16) & 0x1FF) as usize;
        let w = (((wh & 0x3FF) + 0xF) & !0xF) as usize; // round up to 0x10
        let h = ((wh >> 16) & 0x1FF) as usize;
        let c = rgb24_to_555(color);
        for dy in 0..h {
            let y = (y0 + dy) & (VRAM_H - 1);
            for dx in 0..w {
                let x = (x0 + dx) & (VRAM_W - 1);
                self.vram[y * VRAM_W + x] = c;
            }
        }
    }

    // ---- polygons (20h..3Fh) ----
    fn draw_polygon(&mut self) {
        let cmd = self.fifo[0];
        let gouraud = cmd & (1 << 28) != 0;
        let quad = cmd & (1 << 27) != 0;
        let textured = cmd & (1 << 26) != 0;
        let raw = cmd & (1 << 24) != 0;
        let semi = cmd & (1 << 25) != 0;
        let verts = if quad { 4 } else { 3 };

        let mut v = [Vertex::default(); 4];
        let mut idx = 0usize; // word cursor
        let mut clut: u32 = 0;
        let mut page: u32 = 0;
        let base_color = cmd & 0x00FF_FFFF;

        for i in 0..verts {
            let color = if gouraud {
                if i == 0 {
                    base_color
                } else {
                    idx += 1;
                    self.fifo[idx] & 0x00FF_FFFF
                }
            } else {
                base_color
            };
            idx += 1;
            let pos = self.fifo[idx];
            let x = sign_extend(pos & 0xFFFF, 16) + self.off_x;
            let y = sign_extend((pos >> 16) & 0xFFFF, 16) + self.off_y;
            let (mut u, mut tv) = (0u8, 0u8);
            if textured {
                idx += 1;
                let w = self.fifo[idx];
                u = (w & 0xFF) as u8;
                tv = ((w >> 8) & 0xFF) as u8;
                match i {
                    0 => clut = (w >> 16) & 0xFFFF,
                    1 => page = (w >> 16) & 0xFFFF,
                    _ => {}
                }
            }
            v[i] = Vertex {
                x,
                y,
                color,
                u,
                v: tv,
            };
        }

        if textured {
            self.apply_texpage(page);
        }

        let tex = if textured { Some((clut, raw)) } else { None };
        self.raster_triangle(v[0], v[1], v[2], gouraud, tex, semi);
        if quad {
            self.raster_triangle(v[1], v[2], v[3], gouraud, tex, semi);
        }
    }

    /// Override the active texpage from a per-polygon texpage word (poly's
    /// second-vertex high halfword).
    fn apply_texpage(&mut self, page: u32) {
        self.draw_mode.page_x = ((page & 0xF) * 64) as u16;
        let mut py = if page & (1 << 4) != 0 { 256u16 } else { 0 };
        if page & (1 << 11) != 0 {
            py += 512;
        }
        self.draw_mode.page_y = py & (VRAM_H as u16 - 1);
        self.draw_mode.semi = ((page >> 5) & 3) as u8;
        self.draw_mode.tex_depth = ((page >> 7) & 3) as u8;
    }

    // ---- rectangles / sprites (60h..7Fh) ----
    fn draw_rect(&mut self) {
        let cmd = self.fifo[0];
        let size = (cmd >> 27) & 3;
        let textured = cmd & (1 << 26) != 0;
        let raw = cmd & (1 << 24) != 0;
        let semi = cmd & (1 << 25) != 0;
        let color = cmd & 0x00FF_FFFF;

        let mut idx = 1;
        let pos = self.fifo[idx];
        let x = sign_extend(pos & 0xFFFF, 16) + self.off_x;
        let y = sign_extend((pos >> 16) & 0xFFFF, 16) + self.off_y;

        let (mut u0, mut v0, mut clut) = (0u8, 0u8, 0u32);
        if textured {
            idx += 1;
            let w = self.fifo[idx];
            u0 = (w & 0xFF) as u8;
            v0 = ((w >> 8) & 0xFF) as u8;
            clut = (w >> 16) & 0xFFFF;
        }
        let (rw, rh) = match size {
            1 => (1i32, 1i32),
            2 => (8, 8),
            3 => (16, 16),
            _ => {
                idx += 1;
                let wh = self.fifo[idx];
                ((wh & 0xFFFF) as i32, ((wh >> 16) & 0xFFFF) as i32)
            }
        };

        for dy in 0..rh {
            for dx in 0..rw {
                let px = x + dx;
                let py = y + dy;
                if !self.in_draw_area(px, py) {
                    continue;
                }
                let final_color;
                if textured {
                    let tu = (u0 as i32 + dx) as u8;
                    let tvv = (v0 as i32 + dy) as u8;
                    let texel = self.sample_texture(tu, tvv, clut);
                    match texel {
                        None => continue, // fully transparent (0x0000)
                        Some(t) => {
                            final_color = if raw {
                                t
                            } else {
                                modulate(t, color)
                            };
                        }
                    }
                } else {
                    final_color = rgb24_to_555(color);
                }
                self.put_pixel(px, py, final_color, semi);
            }
        }
    }

    // ---- lines (40h..5Fh) ----
    fn draw_line(&mut self) {
        let cmd = self.fifo[0];
        let gouraud = cmd & (1 << 28) != 0;
        let semi = cmd & (1 << 25) != 0;
        let base = cmd & 0x00FF_FFFF;

        // single line: [cmd] [v0] (c1) [v1]
        let mut idx = 1;
        let p0 = self.fifo[idx];
        let c0 = base;
        idx += 1;
        let (c1, p1);
        if gouraud {
            c1 = self.fifo[idx] & 0x00FF_FFFF;
            idx += 1;
            p1 = self.fifo[idx];
        } else {
            c1 = base;
            p1 = self.fifo[idx];
        }
        let x0 = sign_extend(p0 & 0xFFFF, 16) + self.off_x;
        let y0 = sign_extend((p0 >> 16) & 0xFFFF, 16) + self.off_y;
        let x1 = sign_extend(p1 & 0xFFFF, 16) + self.off_x;
        let y1 = sign_extend((p1 >> 16) & 0xFFFF, 16) + self.off_y;
        self.raster_line(x0, y0, c0, x1, y1, c1, gouraud, semi);
    }

    fn draw_polyline(&mut self) {
        let cmd = self.fifo[0];
        let gouraud = cmd & (1 << 28) != 0;
        let semi = cmd & (1 << 25) != 0;
        let base = cmd & 0x00FF_FFFF;

        // Words after cmd: [v0] then ([c] [v])* with gouraud, or [v]* flat,
        // terminated by a 0x5000_5000-style word (already dropped from fifo? no —
        // it was pushed). Build a vertex list.
        let mut verts: Vec<(i32, i32, u32)> = Vec::new();
        let mut idx = 1;
        // first color = command color
        if gouraud {
            // first vertex uses cmd color
            if idx < self.fifo.len() {
                let p = self.fifo[idx];
                idx += 1;
                verts.push((vx(p, self.off_x), vy(p, self.off_y), base));
            }
            while idx + 1 < self.fifo.len() {
                let c = self.fifo[idx] & 0x00FF_FFFF;
                let p = self.fifo[idx + 1];
                if (p & 0xF000_F000) == 0x5000_5000 && (c & 0xF000_F000) == 0x5000_5000
                {
                    break;
                }
                idx += 2;
                verts.push((vx(p, self.off_x), vy(p, self.off_y), c));
            }
        } else {
            while idx < self.fifo.len() {
                let p = self.fifo[idx];
                if (p & 0xF000_F000) == 0x5000_5000 {
                    break;
                }
                idx += 1;
                verts.push((vx(p, self.off_x), vy(p, self.off_y), base));
            }
        }

        for pair in verts.windows(2) {
            let (x0, y0, c0) = pair[0];
            let (x1, y1, c1) = pair[1];
            self.raster_line(x0, y0, c0, x1, y1, c1, gouraud, semi);
        }
    }

    // ---- VRAM↔VRAM copy (80h..9Fh) ----
    fn vram_to_vram(&mut self) {
        let src = self.fifo[1];
        let dst = self.fifo[2];
        let wh = self.fifo[3];
        let sx = (src & 0x3FF) as usize;
        let sy = ((src >> 16) & 0x1FF) as usize;
        let dx = (dst & 0x3FF) as usize;
        let dy = ((dst >> 16) & 0x1FF) as usize;
        let w = (((wh & 0x3FF).wrapping_sub(1) & 0x3FF) + 1) as usize;
        let h = ((((wh >> 16) & 0x1FF).wrapping_sub(1) & 0x1FF) + 1) as usize;
        for ry in 0..h {
            for rx in 0..w {
                let s = ((sy + ry) & (VRAM_H - 1)) * VRAM_W + ((sx + rx) & (VRAM_W - 1));
                let d = ((dy + ry) & (VRAM_H - 1)) * VRAM_W + ((dx + rx) & (VRAM_W - 1));
                let px = self.vram[s];
                if self.check_mask && self.vram[d] & 0x8000 != 0 {
                    continue;
                }
                self.vram[d] = if self.set_mask { px | 0x8000 } else { px };
            }
        }
    }

    // ---- CPU→VRAM image transfer (A0h..BFh) ----
    fn begin_cpu_to_vram(&mut self) {
        let dst = self.fifo[1];
        let wh = self.fifo[2];
        self.transfer = Transfer {
            x: (dst & 0x3FF) as u16,
            y: ((dst >> 16) & 0x1FF) as u16,
            w: ((((wh & 0x3FF).wrapping_sub(1) & 0x3FF) + 1)) as u16,
            h: (((((wh >> 16) & 0x1FF).wrapping_sub(1) & 0x1FF) + 1)) as u16,
            cx: 0,
            cy: 0,
        };
        if self.transfer.w == 0 || self.transfer.h == 0 {
            return; // nothing to receive
        }
        self.mode = Gp0Mode::CpuToVram;
    }

    /// Consume one data word (two 16bpp pixels) of a CPU→VRAM transfer.
    fn push_cpu_to_vram(&mut self, word: u32) {
        for half in 0..2 {
            let px = (word >> (half * 16)) as u16;
            let t = &mut self.transfer;
            let x = (t.x as usize + t.cx as usize) & (VRAM_W - 1);
            let y = (t.y as usize + t.cy as usize) & (VRAM_H - 1);
            self.vram[y * VRAM_W + x] = px;
            let t = &mut self.transfer;
            t.cx += 1;
            if t.cx >= t.w {
                t.cx = 0;
                t.cy += 1;
                if t.cy >= t.h {
                    self.reset_command();
                    return;
                }
            }
        }
    }

    // ---- VRAM→CPU image transfer (C0h..DFh) ----
    fn begin_vram_to_cpu(&mut self) {
        let src = self.fifo[1];
        let wh = self.fifo[2];
        self.transfer = Transfer {
            x: (src & 0x3FF) as u16,
            y: ((src >> 16) & 0x1FF) as u16,
            w: ((((wh & 0x3FF).wrapping_sub(1) & 0x3FF) + 1)) as u16,
            h: (((((wh >> 16) & 0x1FF).wrapping_sub(1) & 0x1FF) + 1)) as u16,
            cx: 0,
            cy: 0,
        };
        // Mark VRAM-read ready; GPUREAD streams the pixels out (latched word).
        self.gpustat |= STAT_VRAM_READY;
        self.gpuread = self.next_vram_word();
    }

    /// Pull the next 32-bit word (two halfwords) from the VRAM→CPU rectangle.
    fn next_vram_word(&mut self) -> u32 {
        let mut word = 0u32;
        for half in 0..2 {
            let t = self.transfer;
            if t.cy >= t.h {
                self.gpustat &= !STAT_VRAM_READY;
                break;
            }
            let x = (t.x as usize + t.cx as usize) & (VRAM_W - 1);
            let y = (t.y as usize + t.cy as usize) & (VRAM_H - 1);
            let px = self.vram[y * VRAM_W + x] as u32;
            word |= px << (half * 16);
            let t = &mut self.transfer;
            t.cx += 1;
            if t.cx >= t.w {
                t.cx = 0;
                t.cy += 1;
            }
        }
        word
    }

    // ============================ rasterizer ============================

    /// Half-space triangle rasterizer with barycentric interpolation.
    fn raster_triangle(
        &mut self,
        a: Vertex,
        b: Vertex,
        c: Vertex,
        gouraud: bool,
        tex: Option<(u32, bool)>,
        semi: bool,
    ) {
        // Bounding box clamped to the drawing area.
        let min_x = a.x.min(b.x).min(c.x).max(self.draw_x0);
        let max_x = a.x.max(b.x).max(c.x).min(self.draw_x1);
        let min_y = a.y.min(b.y).min(c.y).max(self.draw_y0);
        let max_y = a.y.max(b.y).max(c.y).min(self.draw_y1);
        if min_x > max_x || min_y > max_y {
            return;
        }

        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area == 0 {
            return; // degenerate
        }
        // Orient so the area is positive (handle both winding orders).
        let (a, b, c) = if area < 0 { (a, c, b) } else { (a, b, c) };
        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y) as f32;

        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let w0 = edge(b.x, b.y, c.x, c.y, px, py);
                let w1 = edge(c.x, c.y, a.x, a.y, px, py);
                let w2 = edge(a.x, a.y, b.x, b.y, px, py);
                if w0 < 0 || w1 < 0 || w2 < 0 {
                    continue;
                }
                let (l0, l1, l2) = (
                    w0 as f32 / area,
                    w1 as f32 / area,
                    w2 as f32 / area,
                );

                let raw_color;
                if let Some((clut, raw)) = tex {
                    let u = interp_u8(a.u, b.u, c.u, l0, l1, l2);
                    let v = interp_u8(a.v, b.v, c.v, l0, l1, l2);
                    match self.sample_texture(u, v, clut) {
                        None => continue, // transparent texel
                        Some(t) => {
                            raw_color = if raw {
                                t
                            } else {
                                let modc = if gouraud {
                                    interp_color(a.color, b.color, c.color, l0, l1, l2)
                                } else {
                                    a.color
                                };
                                modulate(t, modc)
                            };
                        }
                    }
                } else if gouraud {
                    let c24 = interp_color(a.color, b.color, c.color, l0, l1, l2);
                    raw_color = rgb24_to_555(c24);
                } else {
                    raw_color = rgb24_to_555(a.color);
                }
                self.put_pixel(px, py, raw_color, semi);
            }
        }
    }

    /// Bresenham line with optional gouraud color interpolation.
    fn raster_line(
        &mut self,
        x0: i32,
        y0: i32,
        c0: u32,
        x1: i32,
        y1: i32,
        c1: u32,
        gouraud: bool,
        semi: bool,
    ) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        let steps = dx.max(-dy).max(1);
        let mut i = 0;
        loop {
            if self.in_draw_area(x, y) {
                let color = if gouraud {
                    let t = i as f32 / steps as f32;
                    lerp_color(c0, c1, t)
                } else {
                    c0
                };
                self.put_pixel(x, y, rgb24_to_555(color), semi);
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
            i += 1;
        }
    }

    /// Sample a texel through the active texpage / texture window / CLUT.
    /// Returns `None` for the fully-transparent index (raw 0x0000).
    fn sample_texture(&self, u: u8, v: u8, clut: u32) -> Option<u16> {
        // Texture window wrap: tex = (tex & ~(mask*8)) | ((off & mask)*8).
        let u = ((u & !(self.tw_mask_x << 3)) | ((self.tw_off_x & self.tw_mask_x) << 3)) as usize;
        let v = ((v & !(self.tw_mask_y << 3)) | ((self.tw_off_y & self.tw_mask_y) << 3)) as usize;
        let page_x = self.draw_mode.page_x as usize;
        let page_y = self.draw_mode.page_y as usize;

        let texel = match self.draw_mode.tex_depth {
            0 => {
                // 4bpp: 4 texels per halfword; CLUT lookup.
                let halfword = self.vram_at(page_x + (u / 4), page_y + v);
                let shift = (u & 3) * 4;
                let index = (halfword >> shift) & 0xF;
                self.clut_lookup(clut, index)
            }
            1 => {
                // 8bpp: 2 texels per halfword; CLUT lookup.
                let halfword = self.vram_at(page_x + (u / 2), page_y + v);
                let shift = (u & 1) * 8;
                let index = (halfword >> shift) & 0xFF;
                self.clut_lookup(clut, index)
            }
            _ => {
                // 15bpp direct.
                self.vram_at(page_x + u, page_y + v)
            }
        };
        if texel == 0 {
            None
        } else {
            Some(texel)
        }
    }

    #[inline]
    fn clut_lookup(&self, clut: u32, index: u16) -> u16 {
        let cx = ((clut & 0x3F) * 16) as usize;
        let cy = ((clut >> 6) & 0x1FF) as usize;
        self.vram_at(cx + index as usize, cy)
    }

    #[inline]
    fn vram_at(&self, x: usize, y: usize) -> u16 {
        self.vram[(y & (VRAM_H - 1)) * VRAM_W + (x & (VRAM_W - 1))]
    }

    #[inline]
    fn in_draw_area(&self, x: i32, y: i32) -> bool {
        x >= self.draw_x0 && x <= self.draw_x1 && y >= self.draw_y0 && y <= self.draw_y1
    }

    /// Write a 15-bit pixel, honoring mask check / set semantics.
    #[inline]
    fn put_pixel(&mut self, x: i32, y: i32, color: u16, _semi: bool) {
        if x < 0 || y < 0 || x >= VRAM_W as i32 || y >= VRAM_H as i32 {
            return;
        }
        let idx = y as usize * VRAM_W + x as usize;
        if self.check_mask && self.vram[idx] & 0x8000 != 0 {
            return;
        }
        let c = if self.set_mask { color | 0x8000 } else { color };
        self.vram[idx] = c;
    }

    /// DMA word path into GP0 (DMA channel 2, RAM→GPU): identical to a CPU GP0
    /// write. The orchestrator feeds linked-list / image-upload words here.
    #[inline]
    pub fn dma_gp0(&mut self, word: u32) {
        self.gp0(word);
    }

    /// DMA word path out of the GPU (DMA channel 2, GPU→RAM / VRAM-read):
    /// streams the next 32-bit word (two halfwords) of an active VRAM→CPU
    /// transfer, advancing the read cursor. Returns the latched GPUREAD value
    /// when no transfer is active.
    #[inline]
    pub fn dma_gpuread(&mut self) -> u32 {
        if self.gpustat & STAT_VRAM_READY != 0 {
            let w = self.next_vram_word();
            self.gpuread = w;
            w
        } else {
            self.gpuread
        }
    }

    // ============================ GP1 ============================

    /// GP1 — display-control commands (reset, display enable, DMA direction,
    /// display area / mode, register info reply).
    pub fn gp1(&mut self, word: u32) {
        let cmd = word >> 24;
        let p = word & 0x00FF_FFFF;
        match cmd {
            0x00 => self.reset(),
            0x01 => {
                // Reset command buffer / FIFO.
                self.reset_command();
            }
            0x02 => self.gpustat &= !STAT_IRQ1, // ack IRQ1
            0x03 => {
                // Display enable: bit0 (0=on, 1=off) -> GPUSTAT.23.
                self.gpustat = (self.gpustat & !(1 << 23)) | ((p & 1) << 23);
            }
            0x04 => {
                // DMA direction -> GPUSTAT.29-30.
                self.gpustat = (self.gpustat & !(3 << 29)) | ((p & 3) << 29);
            }
            0x05 => {
                self.display_x = (p & 0x3FF) as u16;
                self.display_y = ((p >> 10) & 0x1FF) as u16;
            }
            0x06 => { /* horizontal display range — affects timing only */ }
            0x07 => { /* vertical display range — affects timing only */ }
            0x08 => self.set_display_mode(p),
            0x10..=0x1F => self.gpu_info(p),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.gpustat = GPUSTAT_RESET;
        self.reset_command();
        self.draw_mode = DrawMode::default();
        self.tw_mask_x = 0;
        self.tw_mask_y = 0;
        self.tw_off_x = 0;
        self.tw_off_y = 0;
        self.draw_x0 = 0;
        self.draw_y0 = 0;
        self.draw_x1 = VRAM_W as i32 - 1;
        self.draw_y1 = VRAM_H as i32 - 1;
        self.off_x = 0;
        self.off_y = 0;
        self.set_mask = false;
        self.check_mask = false;
        self.display_x = 0;
        self.display_y = 0;
        self.display_24bpp = false;
    }

    fn set_display_mode(&mut self, p: u32) {
        // H-res: bit6 (368px) else bits0-1 -> 256/320/512/640.
        let hres = if p & (1 << 6) != 0 {
            368
        } else {
            match p & 3 {
                0 => 256,
                1 => 320,
                2 => 512,
                _ => 640,
            }
        };
        let vres = if p & (1 << 2) != 0 && p & (1 << 5) != 0 {
            480
        } else {
            240
        };
        self.display_w = hres;
        self.display_h = vres;
        self.display_24bpp = p & (1 << 4) != 0;
        // GPUSTAT bits 16..22 mirror GP1(08h).
        let stat = ((p & 0x3F) << 17) | (((p >> 6) & 1) << 16) | (((p >> 7) & 1) << 14);
        self.gpustat = (self.gpustat & !0x007F_4000) | stat;
    }

    /// GP1(10h..1Fh): latch a register value into GPUREAD.
    fn gpu_info(&mut self, p: u32) {
        let reg = p & 0xFF;
        self.gpuread = match reg {
            0x02 => {
                (self.tw_mask_x as u32)
                    | ((self.tw_mask_y as u32) << 5)
                    | ((self.tw_off_x as u32) << 10)
                    | ((self.tw_off_y as u32) << 15)
            }
            0x03 => (self.draw_x0 as u32 & 0x3FF) | ((self.draw_y0 as u32 & 0x1FF) << 10),
            0x04 => (self.draw_x1 as u32 & 0x3FF) | ((self.draw_y1 as u32 & 0x1FF) << 10),
            0x05 => {
                (self.off_x as u32 & 0x7FF) | ((self.off_y as u32 & 0x7FF) << 11)
            }
            0x07 => 0x0000_0002, // GPU version
            _ => self.gpuread,
        };
    }

    // ============================ timing / VBLANK ============================

    /// Advance the GPU by `cycles` GPU clocks: walk the scanline counter, and
    /// raise the VBLANK pending flag on the falling edge into VBLANK.
    pub fn step(&mut self, cycles: u32) {
        self.scanline_cycle += cycles;
        while self.scanline_cycle >= CYCLES_PER_SCANLINE {
            self.scanline_cycle -= CYCLES_PER_SCANLINE;
            self.scanline += 1;
            if self.scanline == VBLANK_START_LINE {
                self.vblank_pending = true;
            }
            if self.scanline >= SCANLINES_PER_FRAME {
                self.scanline = 0;
                // Toggle the interlace "drawing odd/even line" bit each frame.
                self.gpustat ^= STAT_INTERLACE_FIELD;
            }
        }
    }

    /// Drain the latched VBLANK edge, raising the VBLANK interrupt. The
    /// orchestrator calls this after [`Gpu::step`] (the device owns no IRQ ref).
    pub fn service_irq(&mut self, irq: &mut Irq) {
        if self.vblank_pending {
            self.vblank_pending = false;
            irq.raise(Interrupt::Vblank);
        }
    }

    /// True if a VBLANK edge is pending (for orchestrators that drive the IRQ
    /// controller directly rather than via [`Gpu::service_irq`]).
    #[inline]
    pub fn take_vblank(&mut self) -> bool {
        let v = self.vblank_pending;
        self.vblank_pending = false;
        v
    }

    // ============================ framebuffer ============================

    /// Expand the current display window from VRAM into [`Gpu::framebuffer`] as
    /// RGBA8888.
    pub fn render_frame(&mut self) {
        let w = self.display_w as usize;
        let h = self.display_h as usize;
        let dx = self.display_x as usize;
        let dy = self.display_y as usize;

        for row in 0..h {
            let vy = (dy + row) & (VRAM_H - 1);
            if self.display_24bpp {
                // 24bpp: 3 bytes per pixel packed across halfwords. Each VRAM
                // line holds (w*3/2) halfwords; reconstruct RGB888 directly.
                for col in 0..w {
                    let byte = col * 3;
                    let hx0 = dx + byte / 2;
                    let lo = self.vram_at(hx0, vy);
                    let hi = self.vram_at(hx0 + 1, vy);
                    let bytes = [
                        (lo & 0xFF) as u8,
                        (lo >> 8) as u8,
                        (hi & 0xFF) as u8,
                        (hi >> 8) as u8,
                    ];
                    let (r, g, b) = if byte & 1 == 0 {
                        (bytes[0], bytes[1], bytes[2])
                    } else {
                        (bytes[1], bytes[2], bytes[3])
                    };
                    self.framebuffer[row * w + col] =
                        0xFF00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | (r as u32);
                }
            } else {
                for col in 0..w {
                    let px = self.vram_at(dx + col, vy);
                    self.framebuffer[row * w + col] = rgb555_to_8888(px);
                }
            }
        }
    }

    /// The host-facing framebuffer slice (RGBA8888, `display_w * display_h`).
    pub fn frame(&self) -> &[u32] {
        let len = (self.display_w as usize) * (self.display_h as usize);
        &self.framebuffer[..len.min(VRAM_HALFWORDS)]
    }
}

// ============================ helpers ============================

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sign_extend(v: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v << shift) as i32) >> shift
}

#[inline]
fn vx(word: u32, off: i32) -> i32 {
    sign_extend(word & 0xFFFF, 16) + off
}

#[inline]
fn vy(word: u32, off: i32) -> i32 {
    sign_extend((word >> 16) & 0xFFFF, 16) + off
}

/// Twice the signed area of triangle (ax,ay)(bx,by)(px,py) — the edge function.
#[inline]
fn edge(ax: i32, ay: i32, bx: i32, by: i32, px: i32, py: i32) -> i32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// Pack a 24-bit BGR color word (0x00BBGGRR) to BGR555 (PSX VRAM layout).
#[inline]
fn rgb24_to_555(c: u32) -> u16 {
    let r = ((c & 0xFF) >> 3) as u16;
    let g = (((c >> 8) & 0xFF) >> 3) as u16;
    let b = (((c >> 16) & 0xFF) >> 3) as u16;
    r | (g << 5) | (b << 10)
}

/// Expand a BGR555 VRAM halfword to RGBA8888 (0xFFRRGGBB → little-endian RGBA).
#[inline]
fn rgb555_to_8888(px: u16) -> u32 {
    let r = ((px & 0x1F) as u32) << 3;
    let g = (((px >> 5) & 0x1F) as u32) << 3;
    let b = (((px >> 10) & 0x1F) as u32) << 3;
    // Replicate the top 3 bits into the low bits for a fuller range.
    let r = r | (r >> 5);
    let g = g | (g >> 5);
    let b = b | (b >> 5);
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

/// Modulate a textured BGR555 texel by a 24-bit BGR vertex color (0x80 = 1.0).
#[inline]
fn modulate(texel: u16, color: u32) -> u16 {
    let tr = (texel & 0x1F) as u32;
    let tg = ((texel >> 5) & 0x1F) as u32;
    let tb = ((texel >> 10) & 0x1F) as u32;
    let cr = color & 0xFF;
    let cg = (color >> 8) & 0xFF;
    let cb = (color >> 16) & 0xFF;
    let r = ((tr * cr) >> 7).min(0x1F);
    let g = ((tg * cg) >> 7).min(0x1F);
    let b = ((tb * cb) >> 7).min(0x1F);
    (r | (g << 5) | (b << 10) | ((texel & 0x8000) as u32)) as u16
}

/// Barycentric interpolation of three 24-bit BGR colors -> packed 24-bit BGR.
#[inline]
fn interp_color(a: u32, b: u32, c: u32, l0: f32, l1: f32, l2: f32) -> u32 {
    let ch = |sh: u32| -> u32 {
        let av = ((a >> sh) & 0xFF) as f32;
        let bv = ((b >> sh) & 0xFF) as f32;
        let cv = ((c >> sh) & 0xFF) as f32;
        (av * l0 + bv * l1 + cv * l2).round().clamp(0.0, 255.0) as u32
    };
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

#[inline]
fn interp_u8(a: u8, b: u8, c: u8, l0: f32, l1: f32, l2: f32) -> u8 {
    (a as f32 * l0 + b as f32 * l1 + c as f32 * l2)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Linear interpolation of two 24-bit BGR colors by `t` in [0,1].
#[inline]
fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let ch = |sh: u32| -> u32 {
        let av = ((a >> sh) & 0xFF) as f32;
        let bv = ((b >> sh) & 0xFF) as f32;
        (av + (bv - av) * t).round().clamp(0.0, 255.0) as u32
    };
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_status_is_idle_ready() {
        let mut gpu = Gpu::new();
        assert_eq!(gpu.read(0x4), GPUSTAT_RESET);
        gpu.write(0x4, 0x0000_0000); // GP1(00h) reset
        assert_eq!(gpu.read(0x4), GPUSTAT_RESET);
    }

    #[test]
    fn vram_is_one_megabyte() {
        let gpu = Gpu::new();
        assert_eq!(gpu.vram.len() * 2, 0x10_0000);
    }

    #[test]
    fn color_round_trips_through_555() {
        // White 24-bit -> 555 -> 8888 should be near-white opaque.
        let c = rgb24_to_555(0x00FF_FFFF);
        assert_eq!(c, 0x7FFF);
        let rgba = rgb555_to_8888(c);
        assert_eq!(rgba & 0xFF00_0000, 0xFF00_0000);
        assert_eq!(rgba & 0x00FF_FFFF, 0x00FF_FFFF);
    }

    #[test]
    fn fill_writes_vram_block() {
        let mut gpu = Gpu::new();
        // GP0(02h) fill: red (R=0xFF), at (0,0), 16x2.
        gpu.gp0(0x0200_00FF);
        gpu.gp0(0x0000_0000); // x=0,y=0
        gpu.gp0(0x0002_0010); // w=0x10, h=2
        assert_eq!(gpu.vram[0], rgb24_to_555(0x0000_00FF));
        assert_eq!(gpu.vram[VRAM_W + 0xF], rgb24_to_555(0x0000_00FF));
    }

    #[test]
    fn flat_triangle_fills_interior() {
        let mut gpu = Gpu::new();
        // Set a generous drawing area.
        gpu.gp0(0xE300_0000); // top-left (0,0)
        gpu.gp0(0xE400_0000 | (511 << 10) | 1023); // bottom-right
                                                    // Flat green triangle (0,0)(100,0)(0,100).
        gpu.gp0(0x2000_FF00); // color = green (G=0xFF)
        gpu.gp0(0x0000_0000); // v0 (0,0)
        gpu.gp0(0x0000_0064); // v1 (100,0)
        gpu.gp0(0x0064_0000); // v2 (0,100)
        let green555 = rgb24_to_555(0x0000_FF00);
        // A point clearly inside the triangle.
        assert_eq!(gpu.vram[10 * VRAM_W + 10], green555);
        // A point outside (far corner).
        assert_eq!(gpu.vram[100 * VRAM_W + 100], 0);
    }

    #[test]
    fn cpu_to_vram_transfer_loads_pixels() {
        let mut gpu = Gpu::new();
        // GP0(A0h): dest (2,3), size 2x1, then one data word (two pixels).
        gpu.gp0(0xA000_0000);
        gpu.gp0(0x0003_0002); // dest x=2, y=3
        gpu.gp0(0x0001_0002); // w=2, h=1
        gpu.gp0(0xBEEF_DEAD); // px0=0xDEAD, px1=0xBEEF
        assert_eq!(gpu.vram[3 * VRAM_W + 2], 0xDEAD);
        assert_eq!(gpu.vram[3 * VRAM_W + 3], 0xBEEF);
        // Transfer complete -> back to command mode.
        gpu.gp0(0x0000_0000); // NOP, must not panic / write
    }

    #[test]
    fn vram_to_cpu_reads_pixels() {
        let mut gpu = Gpu::new();
        gpu.vram[5 * VRAM_W + 4] = 0x1234;
        gpu.vram[5 * VRAM_W + 5] = 0x5678;
        gpu.gp0(0xC000_0000);
        gpu.gp0(0x0005_0004); // src x=4, y=5
        gpu.gp0(0x0001_0002); // w=2, h=1
        let word = gpu.read(0x0); // GPUREAD
        assert_eq!(word & 0xFFFF, 0x1234);
        assert_eq!(word >> 16, 0x5678);
    }

    #[test]
    fn display_mode_sets_resolution() {
        let mut gpu = Gpu::new();
        // GP1(08h): hres1=320 (bit0-1=1), 24bpp (bit4).
        gpu.gp1(0x0800_0011);
        assert_eq!(gpu.display_w, 320);
        assert!(gpu.display_24bpp);
        assert_ne!(gpu.gpustat & (1 << 21), 0); // color depth bit
    }

    #[test]
    fn display_area_start_latched() {
        let mut gpu = Gpu::new();
        gpu.gp1(0x0500_0000 | (5 << 10) | 7); // x=7, y=5
        assert_eq!(gpu.display_x, 7);
        assert_eq!(gpu.display_y, 5);
    }

    #[test]
    fn draw_offset_sign_extends() {
        let mut gpu = Gpu::new();
        // off_x = -1 (0x7FF), off_y = +2.
        gpu.gp0(0xE500_0000 | (2 << 11) | 0x7FF);
        assert_eq!(gpu.off_x, -1);
        assert_eq!(gpu.off_y, 2);
    }

    #[test]
    fn vblank_edge_after_a_frame() {
        let mut gpu = Gpu::new();
        let mut irq = Irq::new();
        // Step through a full frame worth of cycles.
        gpu.step(CYCLES_PER_SCANLINE * (VBLANK_START_LINE as u32 + 1));
        gpu.service_irq(&mut irq);
        assert_ne!(irq.stat & Interrupt::Vblank.bit(), 0);
    }

    #[test]
    fn render_frame_expands_display_window() {
        let mut gpu = Gpu::new();
        gpu.display_x = 0;
        gpu.display_y = 0;
        gpu.display_w = 4;
        gpu.display_h = 2;
        gpu.display_24bpp = false;
        gpu.vram[0] = 0x7FFF; // white
        gpu.render_frame();
        assert_eq!(gpu.framebuffer[0] & 0xFF00_0000, 0xFF00_0000);
        assert_eq!(gpu.framebuffer[0] & 0x00FF_FFFF, 0x00FF_FFFF);
        assert_eq!(gpu.frame().len(), 8);
    }

    #[test]
    fn textured_rect_samples_clut() {
        let mut gpu = Gpu::new();
        gpu.gp0(0xE300_0000);
        gpu.gp0(0xE400_0000 | (511 << 10) | 1023);
        // 8bpp texpage at (0,0); CLUT at (0,1) -> entry 1 = white.
        gpu.draw_mode.tex_depth = 1;
        gpu.draw_mode.page_x = 0;
        gpu.draw_mode.page_y = 0;
        // texpage row 0 col 0 low byte = index 1 (the texel at u=0).
        gpu.vram[0] = 0x0001;
        // CLUT at vram y=1 (clut word: y=1<<6, x=0) entry 1 -> white 0x7FFF.
        gpu.vram[1 * VRAM_W + 1] = 0x7FFF;
        let clut = 1u32 << 6; // y=1, x/16=0
                              // Textured raw 1x1 rect at (50,50), u=0 v=0.
                              // opcode 0x6D = base 0x60 | size1(bit27) | tex(bit26) | raw(bit24)
        gpu.gp0(0x6D00_0000);
        gpu.gp0(0x0032_0032); // x=50, y=50
        gpu.gp0((clut << 16) | 0x0000); // clut, u=0, v=0
        assert_eq!(gpu.vram[50 * VRAM_W + 50], 0x7FFF);
    }
}
