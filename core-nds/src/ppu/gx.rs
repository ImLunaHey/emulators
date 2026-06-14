//! Minimal-viable DS 3D geometry engine (the "GX"). Ported/adapted from
//! ../../ds-recomp/src/ppu/gx.ts.
//!
//! It parses the GXFIFO command stream (writes to 0x04000400 are packed-command
//! words; writes to 0x04000440+ are individual command parameters), maintains
//! the four 4×4 matrix stacks (projection / position / vector / texture),
//! transforms incoming vertices through the position+projection chain, runs
//! per-vertex lighting, and software-rasterizes triangles into a 256×192 BGR555
//! framebuffer that Engine A's BG0 layer pulls from in DISPCNT-bit-3 (3D) mode.
//!
//! ## Ownership model (CONTRACT.md)
//!
//! The TS `Gx` stored `mem` + `irq9` references and reached
//! `getActiveVramRouter()`. Here those are PARAMETERS: the command interpreter
//! that needs to raise the GXFIFO IRQ takes `&mut Irq`; the rasterizer that
//! reads texture VRAM takes `&SharedMemory` + `&VramRouter` + `&[u8;9]`. `Gpu3d`
//! owns only its own state (matrices, stacks, the FIFO, framebuffers, the 3D
//! register block).
//!
//! ## Fixed-point
//!
//! The DS geometry pipeline is fixed-point. Matrix entries are stored as `i32`
//! in Q12 (4096 = 1.0); matrix multiply accumulates in `i64` and shifts back.
//! Vertex coordinates from VTX_16 (4.12) / VTX_10 (6.4) are normalised to Q12.
//! This replaces the TS floats (`/4096`, `Float64Array`).
//!
//! ## What is a `todo!()` stub this wave
//!
//! The register/FIFO plumbing, matrix stack, vertex assembly, lighting hookup,
//! and the public interface are REAL. The heavy pixel work — `draw_triangle`
//! (the scanline rasterizer with texture mapping / alpha / wireframe / edge) and
//! `render_scanline` (the fog + edge-mark + pack pass the 2D engine reads) — are
//! `todo!()` bodies the next wave fills.

use super::gx_lighting::{LightState, MaterialState};
use crate::io::irq::{Irq, IRQ_GXFIFO};
use crate::memory::{SharedMemory, VramRouter};

/// 3D layer width (one DS screen).
pub const GX_SCREEN_W: usize = 256;
/// 3D layer height.
pub const GX_SCREEN_H: usize = 192;

/// Fixed-point fractional bits for matrices + transformed coords (Q12).
pub const FP_SHIFT: u32 = 12;
/// 1.0 in Q12.
pub const FP_ONE: i32 = 1 << FP_SHIFT;

// ─── Closed enums (replace the TS magic numbers / string unions) ─────────────

/// Which matrix the MTX_* commands target (`MTX_MODE`, cmd 0x10).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatrixMode {
    /// Projection matrix (stack depth 1).
    Projection,
    /// Position (model-view) matrix (stack depth 31).
    Position,
    /// Position AND vector — writes both stacks together.
    PositionVector,
    /// Texture matrix.
    Texture,
}

impl MatrixMode {
    /// Decode the 2-bit MTX_MODE parameter.
    #[inline]
    pub fn from_bits(v: u32) -> MatrixMode {
        match v & 0x3 {
            0 => MatrixMode::Projection,
            1 => MatrixMode::Position,
            2 => MatrixMode::PositionVector,
            _ => MatrixMode::Texture,
        }
    }
}

/// Primitive being assembled between BEGIN_VTXS (0x40) and END_VTXS (0x41).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimType {
    /// Not inside a BEGIN_VTXS block.
    None,
    TriangleList,
    QuadList,
    TriangleStrip,
    QuadStrip,
}

impl PrimType {
    /// Decode the 2-bit BEGIN_VTXS parameter.
    #[inline]
    pub fn from_bits(v: u32) -> PrimType {
        match v & 0x3 {
            0 => PrimType::TriangleList,
            1 => PrimType::QuadList,
            2 => PrimType::TriangleStrip,
            _ => PrimType::QuadStrip,
        }
    }
}

// ─── 4×4 fixed-point matrix (Q12, column-major as the TS Float64Array) ──────

/// A 4×4 matrix in Q12 fixed-point, column-major (`m[col*4 + row]`), matching
/// the TS layout where `m[12..15]` is the translation column.
pub type Mat4 = [i32; 16];

/// Identity matrix.
#[inline]
pub fn mat4_identity() -> Mat4 {
    let mut m = [0i32; 16];
    m[0] = FP_ONE;
    m[5] = FP_ONE;
    m[10] = FP_ONE;
    m[15] = FP_ONE;
    m
}

/// `out = a * b` in Q12. Computed into a temporary so `out` may alias `a`/`b`.
pub fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut t = [0i32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let mut s: i64 = 0;
            for k in 0..4 {
                s += (a[i + k * 4] as i64) * (b[k + j * 4] as i64);
            }
            t[i + j * 4] = (s >> FP_SHIFT) as i32;
        }
    }
    t
}

/// A homogeneous 4-vector in Q12.
#[derive(Clone, Copy, Default)]
pub struct Vec4 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub w: i32,
}

/// Apply `m` to `(x, y, z, w)` (Q12 in, Q12 out).
pub fn mat4_apply(m: &Mat4, x: i32, y: i32, z: i32, w: i32) -> Vec4 {
    let dot = |c0: usize, c1: usize, c2: usize, c3: usize| -> i32 {
        let s = (m[c0] as i64) * (x as i64)
            + (m[c1] as i64) * (y as i64)
            + (m[c2] as i64) * (z as i64)
            + (m[c3] as i64) * (w as i64);
        (s >> FP_SHIFT) as i32
    };
    Vec4 {
        x: dot(0, 4, 8, 12),
        y: dot(1, 5, 9, 13),
        z: dot(2, 6, 10, 14),
        w: dot(3, 7, 11, 15),
    }
}

/// A post-transform vertex ready for the rasterizer.
#[derive(Clone, Copy, Default)]
pub struct Vertex {
    /// Clip-space coordinates (Q12).
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub w: i32,
    /// Packed BGR555 vertex color.
    pub color: u16,
    /// Texture coords in texels (Q12; pre-divided by 16 from the 1/16-texel
    /// command units).
    pub s: i32,
    pub t: i32,
}

/// One screen-space vertex of a queued polygon. The geometry stage (which runs
/// at GXFIFO-command time and has no VRAM access) computes the perspective
/// divide + viewport transform up front; the values are stored as `f32` because
/// the rasterizer's barycentric weights and perspective-correct interpolation
/// are fundamentally floating-point on real hardware emulation. Texture VRAM is
/// only touched later, in `render_scanline`, which holds the borrows.
#[derive(Clone, Copy)]
struct ScreenVert {
    /// Screen-space pixel position.
    sx: f32,
    sy: f32,
    /// 1/w for perspective-correct interpolation.
    inv_w: f32,
    /// BGR555 vertex color channels (0..31).
    r: f32,
    g: f32,
    b: f32,
    /// Texture coords in texels.
    s: f32,
    t: f32,
}

/// A queued, screen-space triangle plus the texture binding it was emitted with.
/// The geometry stage appends these to the back list; `swap_buffers` promotes
/// the back list to the front one, and `render_scanline` rasterizes the front
/// list (it owns the VRAM borrows) before reading out the requested line.
#[derive(Clone, Copy)]
struct ScreenTri {
    v: [ScreenVert; 3],
    /// TEXIMAGE_PARAM snapshot for this polygon.
    tex_param: u32,
    /// PLTT_BASE snapshot for this polygon.
    tex_pltt_base: u32,
    /// POLYGON_ATTR snapshot (bit 4 = wireframe when alpha == 0; alpha bits
    /// 16..20).
    polygon_attr: u32,
}

/// Number of parameter words each GX command consumes. Indexed by opcode.
/// Mirrors the TS `CMD_PARAMS`.
#[inline]
fn cmd_params(op: u8) -> Option<usize> {
    Some(match op {
        0x10 => 1,
        0x11 => 0,
        0x12 => 1,
        0x13 => 1,
        0x14 => 1,
        0x15 => 0,
        0x16 => 16,
        0x17 => 12,
        0x18 => 16,
        0x19 => 12,
        0x1A => 9,
        0x1B => 3,
        0x1C => 3,
        0x20 => 1,
        0x21 => 1,
        0x22 => 1,
        0x23 => 2,
        0x24 => 1,
        0x25 => 1,
        0x26 => 1,
        0x27 => 1,
        0x28 => 1,
        0x29 => 1,
        0x2A => 1,
        0x2B => 1,
        0x30 => 1,
        0x31 => 1,
        0x32 => 1,
        0x33 => 1,
        0x34 => 32,
        0x40 => 1,
        0x41 => 0,
        0x50 => 1,
        0x60 => 1,
        0x70 => 3,
        0x71 => 2,
        0x72 => 1,
        _ => return None,
    })
}

/// Viewport rectangle (VIEWPORT cmd 0x60), in screen pixels.
#[derive(Clone, Copy)]
pub struct Viewport {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            x0: 0,
            y0: 0,
            x1: (GX_SCREEN_W - 1) as u32,
            y1: (GX_SCREEN_H - 1) as u32,
        }
    }
}

#[inline]
fn boxed_u16(n: usize) -> Box<[u16]> {
    vec![0u16; n].into_boxed_slice()
}
#[inline]
fn boxed_u8(n: usize) -> Box<[u8]> {
    vec![0u8; n].into_boxed_slice()
}

/// The DS geometry engine + software rasterizer.
pub struct Gpu3d {
    // ─── Framebuffers (BGR555, bit 15 = drawn) + drawn masks ─────────────
    /// Back buffer: the in-progress scene the rasterizer writes.
    pub fb_back: Box<[u16]>,
    /// Front buffer: the stable scene SWAP_BUFFERS promoted; what the 2D engine
    /// reads through `render_scanline`.
    pub fb_front: Box<[u16]>,
    /// Companion 1-byte/pixel "drawn this frame" mask for edge marking.
    pub drawn_mask_back: Box<[u8]>,
    pub drawn_mask_front: Box<[u8]>,

    /// Packed per-pixel scanline the 2D compositor reads (BGR555 in bits 0..14,
    /// bit 15 = transparent, in the engine_a `PX_TRANSPARENT` convention). Filled
    /// by `render_scanline`.
    line_buf: Box<[u32]>,

    /// Per-pixel depth buffer for the front scene (Q-something 1/w "smaller =
    /// nearer" key; we use the interpolated 1/w so larger wins). One slot per
    /// pixel, valid for `fb_front` after a rasterize.
    depth_front: Box<[f32]>,

    // ─── Deferred display lists ───────────────────────────────────────────
    /// Screen-space triangles accumulated for the in-progress (back) scene.
    tris_back: Vec<ScreenTri>,
    /// Screen-space triangles for the stable (front) scene SWAP_BUFFERS promoted.
    tris_front: Vec<ScreenTri>,
    /// Whether `tris_front` has been rasterized into `fb_front` since the last
    /// SWAP_BUFFERS. Reset on swap; set on the first `render_scanline` of a frame.
    front_rasterized: bool,

    // ─── 3D control register block (GBATEK §"3D Display Engine Registers") ─
    /// DISP3DCNT (0x04000060, 16-bit). Bit 5 = edge marking, bit 7 = fog.
    pub disp3dcnt: u32,
    /// CLEAR_COLOR (0x04000350) — BGR555 backdrop the rasterizer clears to.
    pub clear_color: u32,
    /// CLEAR_DEPTH (0x04000354) — 15-bit clear Z.
    pub clear_depth: u32,
    /// Viewport (VIEWPORT cmd 0x60).
    pub viewport: Viewport,
    /// FOG_COLOR (0x04000358), BGR555.
    pub fog_color: u16,
    /// FOG_OFFSET (0x0400035C), 15-bit Z reference.
    pub fog_offset: u32,
    /// FOG_TABLE (0x04000360..0x0400037F), 32 × 7-bit density.
    pub fog_table: [u8; 32],
    /// EDGE_COLOR_TABLE (0x04000330..0x0400033F), 8 × BGR555.
    pub edge_color_table: [u16; 8],

    // ─── Matrix stacks ────────────────────────────────────────────────────
    pub mat_proj: Mat4,
    pub mat_pos: Mat4,
    pub mat_vec: Mat4,
    pub mat_tex: Mat4,
    pub pos_stack: Vec<Mat4>,
    pub proj_stack: Vec<Mat4>,
    pub vec_stack: Vec<Mat4>,
    pub mat_mode: MatrixMode,

    // ─── Vertex assembly ──────────────────────────────────────────────────
    pub prim_type: PrimType,
    pub vertex_buf: Vec<Vertex>,
    pub current_color: u16,
    pub last_vtx: [i32; 3], // Q12, for VTX_DIFF + partial coords
    /// Current texcoord in texels (Q12).
    pub cur_s: i32,
    pub cur_t: i32,
    pub raw_s: i32,
    pub raw_t: i32,

    // ─── Texture state ────────────────────────────────────────────────────
    /// TEXIMAGE_PARAM (cmd 0x2A).
    pub tex_param: u32,
    /// PLTT_BASE (cmd 0x2B).
    pub tex_pltt_base: u32,

    // ─── Lighting ─────────────────────────────────────────────────────────
    pub lights: LightState,
    pub material: MaterialState,
    pub polygon_attr: u32,

    // ─── FIFO command interpreter state ──────────────────────────────────
    /// Opcodes unpacked from a packed FIFO word, awaiting parameters; the head
    /// is the op currently accumulating params.
    pending_ops: Vec<u8>,
    /// Params accumulated for `pending_ops[0]`.
    pending_params: Vec<u32>,
    /// GXSTAT bits 30..31 — FIFO IRQ condition: 0 = never, 1 = < half full,
    /// 2 = empty, 3 = reserved.
    pub gxstat_irq_mode: u32,
}

impl Default for Gpu3d {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpu3d {
    pub fn new() -> Self {
        let n = GX_SCREEN_W * GX_SCREEN_H;
        Gpu3d {
            fb_back: boxed_u16(n),
            fb_front: boxed_u16(n),
            drawn_mask_back: boxed_u8(n),
            drawn_mask_front: boxed_u8(n),
            line_buf: vec![0u32; GX_SCREEN_W].into_boxed_slice(),
            depth_front: vec![0.0f32; n].into_boxed_slice(),
            tris_back: Vec::new(),
            tris_front: Vec::new(),
            front_rasterized: false,

            disp3dcnt: 0,
            clear_color: 0,
            clear_depth: 0x7FFF,
            viewport: Viewport::default(),
            fog_color: 0,
            fog_offset: 0,
            fog_table: [0; 32],
            edge_color_table: [0; 8],

            mat_proj: mat4_identity(),
            mat_pos: mat4_identity(),
            mat_vec: mat4_identity(),
            mat_tex: mat4_identity(),
            pos_stack: Vec::new(),
            proj_stack: Vec::new(),
            vec_stack: Vec::new(),
            mat_mode: MatrixMode::Projection,

            prim_type: PrimType::None,
            vertex_buf: Vec::new(),
            current_color: 0x7FFF,
            last_vtx: [0; 3],
            cur_s: 0,
            cur_t: 0,
            raw_s: 0,
            raw_t: 0,

            tex_param: 0,
            tex_pltt_base: 0,

            lights: LightState::new(),
            material: MaterialState::new(),
            polygon_attr: 0,

            pending_ops: Vec::new(),
            pending_params: Vec::new(),
            gxstat_irq_mode: 0,
        }
    }

    // ─── GXSTAT (0x04000600) ─────────────────────────────────────────────
    //
    // Our interpreter drains synchronously inside `write_fifo`/`write_direct`,
    // so by the time the CPU can read GXSTAT the FIFO is always empty: count 0,
    // "less than half full" (bit 25) and "empty" (bit 26) set, geometry engine
    // idle (bit 27 clear). Pokemon D/P/Pt spins on bit 25 before each list.

    /// Read GXSTAT — the live FIFO/matrix-stack status word.
    pub fn read_stat(&self) -> u32 {
        let mut v = 0u32;
        // Bits 8..12: position/vector stack level (0..31).
        v |= ((self.pos_stack.len() as u32) & 0x1F) << 8;
        // Bit 13: projection stack level (0..1).
        if !self.proj_stack.is_empty() {
            v |= 1 << 13;
        }
        // FIFO count (bits 16..24) = 0; bit 25 under-half; bit 26 empty.
        v |= (1 << 25) | (1 << 26);
        v |= self.gxstat_irq_mode << 30;
        v
    }

    /// Write GXSTAT. Bit 15 acks the matrix-stack error (we don't latch one);
    /// bits 30..31 select the FIFO IRQ condition. With a permanently-drained
    /// FIFO both "< half" and "empty" hold, so arming a firing mode raises the
    /// IRQ immediately (level-ish hardware behavior).
    pub fn write_stat(&mut self, value: u32, irq9: &mut Irq) {
        self.gxstat_irq_mode = (value >> 30) & 0x3;
        if self.gxstat_irq_mode == 1 || self.gxstat_irq_mode == 2 {
            irq9.raise(IRQ_GXFIFO);
        }
    }

    // ─── GXFIFO / direct command ports ───────────────────────────────────

    /// GXFIFO at 0x04000400 — a packed command word. The low byte is the first
    /// opcode; subsequent non-zero bytes are additional opcodes. After each
    /// opcode its `cmd_params(op)` parameter words follow as later writes.
    pub fn write_fifo(&mut self, cmd: u32, irq9: &mut Irq) {
        if self.pending_ops.is_empty() {
            for i in 0..4 {
                let op = ((cmd >> (i * 8)) & 0xFF) as u8;
                if op != 0 {
                    self.pending_ops.push(op);
                }
            }
            self.try_drain(irq9);
            return;
        }
        self.pending_params.push(cmd);
        self.try_drain(irq9);
    }

    /// Direct command ports at 0x04000440+. The register offset encodes the
    /// opcode: `op = (regAddr - 0x04000440) / 4 + 0x10`. Each port write is a
    /// single parameter, accumulated like the FIFO path.
    pub fn write_direct(&mut self, reg_addr: u32, value: u32, irq9: &mut Irq) {
        let op = (((reg_addr - 0x0400_0440) >> 2) + 0x10) as u8;
        if !self.pending_ops.is_empty() && self.pending_ops[0] == op {
            self.pending_params.push(value);
        } else {
            self.pending_ops = vec![op];
            self.pending_params = vec![value];
        }
        self.try_drain(irq9);
    }

    /// Drain every command whose parameters are fully buffered. Re-raises the
    /// GXFIFO IRQ once per drain when an IRQ condition is armed (games pump their
    /// display list inside the IRQ-21 handler).
    fn try_drain(&mut self, irq9: &mut Irq) {
        let mut executed = false;
        while !self.pending_ops.is_empty() {
            let op = self.pending_ops[0];
            let need = cmd_params(op).unwrap_or(0);
            if self.pending_params.len() < need {
                return;
            }
            let params: Vec<u32> = self.pending_params.drain(0..need).collect();
            self.execute_command(op, &params, irq9);
            executed = true;
            self.pending_ops.remove(0);
        }
        if executed && (self.gxstat_irq_mode == 1 || self.gxstat_irq_mode == 2) {
            irq9.raise(IRQ_GXFIFO);
        }
    }

    /// Execute one decoded GX command. Unhandled opcodes are silently consumed.
    fn execute_command(&mut self, op: u8, p: &[u32], irq9: &mut Irq) {
        match op {
            0x10 => self.mat_mode = MatrixMode::from_bits(p[0]),
            0x11 => self.matrix_push(),
            0x12 => self.matrix_pop(p[0] & 0x3F),
            0x15 => self.matrix_load(mat4_identity()),
            0x16 => {
                let m = self.unpack_4x4(p);
                self.matrix_load(m);
            }
            0x17 => {
                let m = self.unpack_4x3(p);
                self.matrix_load(m);
            }
            0x18 => {
                let m = self.unpack_4x4(p);
                self.matrix_mult(&m);
            }
            0x19 => {
                let m = self.unpack_4x3(p);
                self.matrix_mult(&m);
            }
            0x1A => {
                let m = self.unpack_3x3(p);
                self.matrix_mult(&m);
            }
            0x1B => {
                let m = make_scale(p[0], p[1], p[2]);
                self.matrix_mult(&m);
            }
            0x1C => {
                let m = make_translate(p[0], p[1], p[2]);
                self.matrix_mult(&m);
            }
            0x20 => self.current_color = (p[0] & 0x7FFF) as u16,
            0x21 => self.normal(p[0]),
            0x22 => self.tex_coord(p[0]),
            0x29 => self.polygon_attr = p[0],
            0x2A => self.tex_param = p[0],
            0x2B => self.tex_pltt_base = p[0],
            0x30 => self.dif_amb(p[0]),
            0x31 => self.material.set_spe_emi(p[0]),
            0x32 => self.lights.set_vector(p[0]),
            0x33 => self.lights.set_color(p[0]),
            0x23 => self.vertex16(p[0], p[1]),
            0x24 => self.vertex10(p[0]),
            0x25 => self.vertex_partial(p[0], PartialAxes::Xy),
            0x26 => self.vertex_partial(p[0], PartialAxes::Xz),
            0x27 => self.vertex_partial(p[0], PartialAxes::Yz),
            0x28 => self.vertex_diff(p[0]),
            0x40 => self.begin_vertices(PrimType::from_bits(p[0])),
            0x41 => self.end_vertices(),
            0x50 => self.swap_buffers(),
            0x60 => self.set_viewport(p[0]),
            _ => {
                let _ = irq9;
            }
        }
    }

    // ─── Matrix command helpers ──────────────────────────────────────────

    fn current_matrix(&mut self) -> &mut Mat4 {
        match self.mat_mode {
            MatrixMode::Projection => &mut self.mat_proj,
            // POSVEC transforms vertices through the position matrix.
            MatrixMode::Position | MatrixMode::PositionVector => &mut self.mat_pos,
            MatrixMode::Texture => &mut self.mat_tex,
        }
    }

    fn matrix_push(&mut self) {
        match self.mat_mode {
            MatrixMode::Projection => self.proj_stack.push(self.mat_proj),
            MatrixMode::Texture => { /* single-slot texture stack */ }
            MatrixMode::Position => self.pos_stack.push(self.mat_pos),
            MatrixMode::PositionVector => {
                self.pos_stack.push(self.mat_pos);
                self.vec_stack.push(self.mat_vec);
            }
        }
    }

    fn matrix_pop(&mut self, n: u32) {
        // n is 6-bit signed (5-bit magnitude + sign); treat as a count.
        let count = (n & 0x1F) as usize;
        match self.mat_mode {
            MatrixMode::Projection => {
                for _ in 0..count {
                    if let Some(m) = self.proj_stack.pop() {
                        self.mat_proj = m;
                    }
                }
            }
            MatrixMode::Texture => {}
            MatrixMode::Position | MatrixMode::PositionVector => {
                let posvec = self.mat_mode == MatrixMode::PositionVector;
                for _ in 0..count {
                    if let Some(m) = self.pos_stack.pop() {
                        self.mat_pos = m;
                    }
                    if posvec {
                        if let Some(m) = self.vec_stack.pop() {
                            self.mat_vec = m;
                        }
                    }
                }
            }
        }
    }

    fn matrix_load(&mut self, m: Mat4) {
        match self.mat_mode {
            MatrixMode::Projection => self.mat_proj = m,
            MatrixMode::Texture => self.mat_tex = m,
            MatrixMode::Position => self.mat_pos = m,
            MatrixMode::PositionVector => {
                // Mode 2 sets both position AND vector (GBATEK §"3D Matrix
                // Stack"), so the lighting normal transform tracks too.
                self.mat_pos = m;
                self.mat_vec = m;
            }
        }
    }

    fn matrix_mult(&mut self, m: &Mat4) {
        let posvec = self.mat_mode == MatrixMode::PositionVector;
        {
            let cur = self.current_matrix();
            *cur = mat4_mul(cur, m);
        }
        if posvec {
            self.mat_vec = mat4_mul(&self.mat_vec, m);
        }
    }

    /// Unpack 16 Q4.12 words into a column-major Q12 matrix.
    fn unpack_4x4(&self, p: &[u32]) -> Mat4 {
        let mut m = [0i32; 16];
        for (i, slot) in m.iter_mut().enumerate() {
            *slot = p[i] as i32;
        }
        m
    }

    /// Unpack 12 Q4.12 words (4 rows × 3 cols + implicit (0,0,0,1) column) into a
    /// column-major Q12 matrix.
    fn unpack_4x3(&self, p: &[u32]) -> Mat4 {
        // GBATEK lays this out as 4 rows × 3 columns; the TS builds it row-major
        // then transposes to column-major. Here we write directly into the
        // column-major slots: row r, col c → out[c*4 + r].
        let mut out = mat4_identity();
        for r in 0..4 {
            for c in 0..3 {
                out[c * 4 + r] = p[r * 3 + c] as i32;
            }
        }
        // Translation column is rows 0..2 of the 4th input row (out[12..14]);
        // identity already set out[15] = 1.
        out[12] = p[9] as i32;
        out[13] = p[10] as i32;
        out[14] = p[11] as i32;
        out[3] = 0;
        out[7] = 0;
        out[11] = 0;
        out[15] = FP_ONE;
        out
    }

    /// Unpack a 3×3 Q4.12 rotation into a column-major Q12 matrix.
    fn unpack_3x3(&self, p: &[u32]) -> Mat4 {
        let mut m = mat4_identity();
        for c in 0..3 {
            for r in 0..3 {
                m[c * 4 + r] = p[c * 3 + r] as i32;
            }
        }
        m
    }

    // ─── Lighting / material commands ────────────────────────────────────

    /// Cmd 0x30 DIF_AMB. When bit 15 is set, latch diffuse as the current vertex
    /// color (SDK base-color stamp even when no NORMAL follows).
    fn dif_amb(&mut self, packed: u32) {
        self.material.set_dif_amb(packed);
        if self.material.set_vertex_color {
            self.current_color = self.material.diffuse;
        }
    }

    /// Cmd 0x21 NORMAL — the vertex-shader entry. Only overrides the current
    /// color when at least one POLYGON_ATTR light bit is set (matches the TS:
    /// unlit ROMs keep their COLOR-set value).
    fn normal(&mut self, packed: u32) {
        if (self.polygon_attr & 0xF) == 0 {
            return;
        }
        let n = super::gx_lighting::unpack_normal(packed);
        // Transform by the vector matrix's upper-left 3×3 (no translation).
        let m = &self.mat_vec;
        let tx = |c0: usize, c1: usize, c2: usize| -> i32 {
            let s = (m[c0] as i64) * (n[0] as i64)
                + (m[c1] as i64) * (n[1] as i64)
                + (m[c2] as i64) * (n[2] as i64);
            (s >> FP_SHIFT) as i32
        };
        let nt = [tx(0, 4, 8), tx(1, 5, 9), tx(2, 6, 10)];
        self.current_color = super::gx_lighting::compute_vertex_color(
            nt,
            self.polygon_attr,
            &self.material,
            &self.lights,
        );
    }

    // ─── Vertex assembly ─────────────────────────────────────────────────

    fn begin_vertices(&mut self, prim: PrimType) {
        self.prim_type = prim;
        self.vertex_buf.clear();
    }

    fn end_vertices(&mut self) {
        self.prim_type = PrimType::None;
    }

    fn vertex16(&mut self, p0: u32, p1: u32) {
        // p0 lo = X, p0 hi = Y, p1 lo = Z (signed 16, 4.12 → already Q12).
        let x = sign_extend(p0, 16);
        let y = sign_extend(p0 >> 16, 16);
        let z = sign_extend(p1, 16);
        self.vertex_at(x, y, z);
    }

    fn vertex10(&mut self, p0: u32) {
        // 3 × 10-bit signed in 6.4 → scale to Q12 (×(4096/64) = ×64 = <<6).
        let x = sign_extend(p0 & 0x3FF, 10) << 6;
        let y = sign_extend((p0 >> 10) & 0x3FF, 10) << 6;
        let z = sign_extend((p0 >> 20) & 0x3FF, 10) << 6;
        self.vertex_at(x, y, z);
    }

    fn vertex_partial(&mut self, p0: u32, axes: PartialAxes) {
        // Two 4.12 halves → already Q12.
        let a = sign_extend(p0, 16);
        let b = sign_extend(p0 >> 16, 16);
        match axes {
            PartialAxes::Xy => self.vertex_at(a, b, self.last_vtx[2]),
            PartialAxes::Xz => self.vertex_at(a, self.last_vtx[1], b),
            PartialAxes::Yz => self.vertex_at(self.last_vtx[0], a, b),
        }
    }

    fn vertex_diff(&mut self, p0: u32) {
        // 3 × 10-bit signed deltas in 6.4 → Q12 (<<6).
        let dx = sign_extend(p0 & 0x3FF, 10) << 6;
        let dy = sign_extend((p0 >> 10) & 0x3FF, 10) << 6;
        let dz = sign_extend((p0 >> 20) & 0x3FF, 10) << 6;
        self.vertex_at(
            self.last_vtx[0] + dx,
            self.last_vtx[1] + dy,
            self.last_vtx[2] + dz,
        );
    }

    /// Push one transformed vertex (position then projection matrix) and emit
    /// any primitive that just completed.
    fn vertex_at(&mut self, x: i32, y: i32, z: i32) {
        self.last_vtx = [x, y, z];
        let pos_view = mat4_apply(&self.mat_pos, x, y, z, FP_ONE);
        let clip = mat4_apply(
            &self.mat_proj,
            pos_view.x,
            pos_view.y,
            pos_view.z,
            pos_view.w,
        );
        self.vertex_buf.push(Vertex {
            x: clip.x,
            y: clip.y,
            z: clip.z,
            w: clip.w,
            color: self.current_color,
            s: self.cur_s,
            t: self.cur_t,
        });
        self.emit_if_ready();
    }

    /// Cmd 0x22 TEXCOORD: two s16 in 1/16-texel units. Mode 1 multiplies by the
    /// texture matrix; other modes use the raw value. We normalise to Q12 texels
    /// (raw 1/16-texel value × (4096/16) = ×256 = <<8).
    fn tex_coord(&mut self, packed: u32) {
        let s = sign_extend(packed, 16) << 8; // Q12 texels
        let t = sign_extend(packed >> 16, 16) << 8;
        self.raw_s = s;
        self.raw_t = t;
        let mode = (self.tex_param >> 30) & 0x3;
        if mode == 1 {
            let m = &self.mat_tex;
            // (s, t, 1/16, 1/16) · texMatrix; the 1/16 constants are Q12 = 256.
            let k = FP_ONE >> 4; // 1/16 in Q12
            let comp = |c0: usize, c1: usize, c2: usize, c3: usize| -> i32 {
                let acc = (s as i64) * (m[c0] as i64)
                    + (t as i64) * (m[c1] as i64)
                    + (k as i64) * (m[c2] as i64)
                    + (k as i64) * (m[c3] as i64);
                (acc >> FP_SHIFT) as i32
            };
            self.cur_s = comp(0, 4, 8, 12);
            self.cur_t = comp(1, 5, 9, 13);
        } else {
            self.cur_s = s;
            self.cur_t = t;
        }
    }

    /// Flush any primitive completed by the last vertex.
    fn emit_if_ready(&mut self) {
        let n = self.vertex_buf.len();
        match self.prim_type {
            PrimType::TriangleList if n >= 3 => {
                self.draw_triangle(n - 3, n - 2, n - 1);
                self.vertex_buf.clear();
            }
            PrimType::QuadList if n >= 4 => {
                self.draw_triangle(n - 4, n - 3, n - 2);
                self.draw_triangle(n - 4, n - 2, n - 1);
                self.vertex_buf.clear();
            }
            PrimType::TriangleStrip if n >= 3 => {
                if (n & 1) == 1 {
                    self.draw_triangle(n - 3, n - 2, n - 1);
                } else {
                    self.draw_triangle(n - 2, n - 3, n - 1);
                }
            }
            PrimType::QuadStrip if n >= 4 && (n & 1) == 0 => {
                self.draw_triangle(n - 4, n - 3, n - 1);
                self.draw_triangle(n - 3, n - 2, n - 1);
            }
            _ => {}
        }
    }

    // ─── SWAP_BUFFERS + viewport ─────────────────────────────────────────

    /// Cmd 0x50 SWAP_BUFFERS — promote the back display list to the front and
    /// start a fresh back list. The actual pixel rasterization is deferred to
    /// `render_scanline` (the only place with the texture-VRAM borrows), so here
    /// we just swap the queued geometry and flag the front scene as needing a
    /// re-raster. `fb_back` is no longer rasterized eagerly, but we keep the
    /// buffers + masks cleared so any external reader sees a defined state.
    fn swap_buffers(&mut self) {
        std::mem::swap(&mut self.tris_back, &mut self.tris_front);
        self.tris_back.clear();
        self.front_rasterized = false;
        self.fb_back.fill(0);
        self.drawn_mask_back.fill(0);
    }

    /// Cmd 0x60 VIEWPORT — packed (x0, y0, x1, y1) bytes.
    fn set_viewport(&mut self, packed: u32) {
        self.viewport = Viewport {
            x0: packed & 0xFF,
            y0: (packed >> 8) & 0xFF,
            x1: (packed >> 16) & 0xFF,
            y1: (packed >> 24) & 0xFF,
        };
    }

    // ─── 3D control register block (the IO dispatch routes bytes here) ──

    /// Byte read of the 3D control register block at masked address `addr`
    /// (0x0FFFFFFF-masked). Returns `Some(byte)` for an owned register.
    pub fn read_reg8(&self, addr: u32) -> Option<u32> {
        match addr {
            0x0400_0060 => Some(self.disp3dcnt & 0xFF),
            0x0400_0061 => Some((self.disp3dcnt >> 8) & 0xFF),
            0x0400_0320..=0x0400_032F => Some(0), // EDGE/poly status — write-mostly
            0x0400_0330..=0x0400_033F => {
                let idx = ((addr - 0x0400_0330) >> 1) as usize;
                Some((self.edge_color_table[idx] as u32 >> ((addr & 1) * 8)) & 0xFF)
            }
            0x0400_0350..=0x0400_0353 => Some((self.clear_color >> ((addr & 3) * 8)) & 0xFF),
            0x0400_0354..=0x0400_0357 => Some((self.clear_depth >> ((addr & 3) * 8)) & 0xFF),
            0x0400_0358..=0x0400_035B => {
                Some((self.fog_color as u32 >> ((addr & 3) * 8)) & 0xFF)
            }
            0x0400_035C => Some(self.fog_offset & 0xFF),
            0x0400_035D => Some((self.fog_offset >> 8) & 0xFF),
            0x0400_0360..=0x0400_037F => Some(self.fog_table[(addr - 0x0400_0360) as usize] as u32),
            _ => None,
        }
    }

    /// Byte write of the 3D control register block. Returns `true` when consumed.
    pub fn write_reg8(&mut self, addr: u32, v: u32) -> bool {
        let v = v & 0xFF;
        match addr {
            0x0400_0060 => self.disp3dcnt = (self.disp3dcnt & 0xFF00) | v,
            0x0400_0061 => self.disp3dcnt = (self.disp3dcnt & 0x00FF) | (v << 8),
            0x0400_0330..=0x0400_033F => {
                let idx = ((addr - 0x0400_0330) >> 1) as usize;
                let sh = (addr & 1) * 8;
                self.edge_color_table[idx] =
                    (((self.edge_color_table[idx] as u32 & !(0xFF << sh)) | (v << sh)) & 0xFFFF) as u16;
            }
            0x0400_0350..=0x0400_0353 => {
                let sh = (addr & 3) * 8;
                self.clear_color = (self.clear_color & !(0xFF << sh)) | (v << sh);
            }
            0x0400_0354..=0x0400_0357 => {
                let sh = (addr & 3) * 8;
                self.clear_depth = (self.clear_depth & !(0xFF << sh)) | (v << sh);
            }
            0x0400_0358..=0x0400_035B => {
                let sh = (addr & 3) * 8;
                self.fog_color =
                    (((self.fog_color as u32 & !(0xFF << sh)) | (v << sh)) & 0x7FFF) as u16;
            }
            0x0400_035C => self.fog_offset = (self.fog_offset & 0xFF00) | v,
            0x0400_035D => self.fog_offset = (self.fog_offset & 0x00FF) | (v << 8),
            0x0400_0360..=0x0400_037F => {
                self.fog_table[(addr - 0x0400_0360) as usize] = (v & 0x7F) as u8;
            }
            _ => return false,
        }
        true
    }

    // ─── Rasterizer + scanline read (the NEXT WAVE fills these) ─────────

    /// Queue the triangle formed by `vertex_buf[a/b/c]` for rasterization.
    ///
    /// The geometry stage runs at GXFIFO-command time and so has no texture-VRAM
    /// borrows; it does the clip→NDC→viewport transform here and appends a
    /// screen-space `ScreenTri` (with a snapshot of the active texture binding +
    /// polygon attributes) to the back display list. `render_scanline` later
    /// rasterizes the front list (ported from the TS `drawTriangle` +
    /// `sampleTexel`) once it holds `&SharedMemory`/`&VramRouter`.
    fn draw_triangle(&mut self, a: usize, b: usize, c: usize) {
        let va = self.vertex_buf[a];
        let vb = self.vertex_buf[b];
        let vc = self.vertex_buf[c];
        // Skip vertices behind/at the eye plane (w <= 0); the TS guarded w == 0
        // only, but a non-positive w is just as degenerate for the divide.
        if va.w <= 0 || vb.w <= 0 || vc.w <= 0 {
            return;
        }
        let sv = |v: &Vertex| -> ScreenVert {
            let inv_w = 1.0 / (v.w as f32);
            // NDC → viewport. The hardware maps NDC [-1,1] into the active
            // VIEWPORT rectangle; default viewport is the full screen, matching
            // the old `(x/w+1)*0.5*W` math.
            let ndc_x = (v.x as f32) * inv_w;
            let ndc_y = (v.y as f32) * inv_w;
            let vp_x0 = self.viewport.x0 as f32;
            let vp_x1 = self.viewport.x1 as f32;
            let vp_y0 = self.viewport.y0 as f32;
            let vp_y1 = self.viewport.y1 as f32;
            let vw = (vp_x1 - vp_x0 + 1.0).max(1.0);
            let vh = (vp_y1 - vp_y0 + 1.0).max(1.0);
            let sx = vp_x0 + (ndc_x + 1.0) * 0.5 * vw;
            // Y is flipped (screen +Y is down, NDC +Y is up).
            let sy = vp_y0 + (1.0 - ndc_y) * 0.5 * vh;
            ScreenVert {
                sx,
                sy,
                inv_w,
                r: (v.color & 0x1F) as f32,
                g: ((v.color >> 5) & 0x1F) as f32,
                b: ((v.color >> 10) & 0x1F) as f32,
                // Texture coords are stored Q12 texels; convert to texel floats.
                s: (v.s as f32) / (FP_ONE as f32),
                t: (v.t as f32) / (FP_ONE as f32),
            }
        };
        self.tris_back.push(ScreenTri {
            v: [sv(&va), sv(&vb), sv(&vc)],
            tex_param: self.tex_param,
            tex_pltt_base: self.tex_pltt_base,
            polygon_attr: self.polygon_attr,
        });
    }

    /// Produce the packed 3D-layer scanline `y` that Engine A composites as BG0.
    ///
    /// On the first call after a SWAP_BUFFERS this rasterizes the whole front
    /// display list into `fb_front`/`drawn_mask_front` (it now holds the texture
    /// VRAM borrows), then for the requested `y` reads each pixel back, applies
    /// fog (DISP3DCNT bit 7) and edge marking (bit 5), and packs into the
    /// engine_a `PX_TRANSPARENT` convention (BGR555 in bits 0..14, bit 15 set
    /// when the pixel was NOT drawn so the 2D compositor skips it).
    pub fn render_scanline(
        &mut self,
        y: u32,
        mem: &SharedMemory,
        router: &VramRouter,
        vramcnt: &[u8; 9],
    ) -> &[u32] {
        if !self.front_rasterized {
            self.rasterize_front(mem, router, vramcnt);
            self.front_rasterized = true;
        }

        let y = (y as usize).min(GX_SCREEN_H - 1);
        let row = y * GX_SCREEN_W;
        let fog_on = (self.disp3dcnt & (1 << 7)) != 0;
        let edge_on = (self.disp3dcnt & (1 << 5)) != 0;

        for x in 0..GX_SCREEN_W {
            let drawn = self.drawn_mask_front[row + x] != 0;
            if !drawn {
                self.line_buf[x] = super::engine_a::PX_TRANSPARENT;
                continue;
            }
            // The framebuffer stores BGR555 with bit 15 = drawn.
            let mut color = self.fb_front[row + x];
            if edge_on {
                // Index the edge-color table by a coarse class. Without polygon
                // IDs we have only a binary drawn mask, so all edges use slot 0.
                color = super::gx_fog::apply_edge_mark(
                    color,
                    x,
                    y,
                    GX_SCREEN_W,
                    GX_SCREEN_H,
                    &self.drawn_mask_front,
                    self.edge_color_table[0],
                );
            }
            if fog_on {
                // No per-pixel Z buffer is exposed to the fog table yet; pass
                // z = 0 (slot 0) exactly like the TS caller did.
                color =
                    super::gx_fog::apply_fog(color, 0, &self.fog_table, self.fog_offset, self.fog_color);
            }
            // Pack into the engine_a convention: BGR555 in 0..14, bit 15 CLEAR
            // means "drawn / opaque" (PX_TRANSPARENT is the inverse sense).
            self.line_buf[x] = (color & 0x7FFF) as u32;
        }
        &self.line_buf
    }

    /// Rasterize every triangle in the front display list into `fb_front` +
    /// `drawn_mask_front`, with a 1/w depth test, per-vertex Gouraud color, and
    /// perspective-correct texture mapping. Ported from the TS `drawTriangle` +
    /// `sampleTexel`; texture VRAM is resolved once per triangle.
    fn rasterize_front(&mut self, mem: &SharedMemory, router: &VramRouter, vramcnt: &[u8; 9]) {
        self.fb_front.fill(0);
        self.drawn_mask_front.fill(0);
        // Clear depth: anything is nearer than -inf (we keep the LARGEST 1/w).
        for d in self.depth_front.iter_mut() {
            *d = f32::NEG_INFINITY;
        }
        // Clone the front list so the pixel loop can borrow `self` mutably.
        let tris = std::mem::take(&mut self.tris_front);
        let vram: &[u8] = &mem.vram[..];
        for tri in &tris {
            let binding = TexBinding::prepare(tri.tex_param, tri.tex_pltt_base, router, vramcnt);
            self.raster_one(tri, &binding, vram);
        }
        self.tris_front = tris;
    }

    /// Scanline-rasterize a single screen-space triangle. Wireframe polygons
    /// (POLYGON_ATTR mode 3, i.e. alpha == 0 with the wireframe flag) fall back
    /// to filled here — a documented simplification.
    fn raster_one(&mut self, tri: &ScreenTri, binding: &TexBinding, vram: &[u8]) {
        let a = tri.v[0];
        let b = tri.v[1];
        let c = tri.v[2];

        // Edge function (signed area × 2). Skip degenerate triangles.
        let area = (b.sx - a.sx) * (c.sy - a.sy) - (b.sy - a.sy) * (c.sx - a.sx);
        if area == 0.0 {
            return;
        }
        let inv_area = 1.0 / area;

        let min_x = a.sx.min(b.sx).min(c.sx).floor().max(0.0) as i32;
        let max_x = (a.sx.max(b.sx).max(c.sx).ceil() as i32).min(GX_SCREEN_W as i32 - 1);
        let min_y = a.sy.min(b.sy).min(c.sy).floor().max(0.0) as i32;
        let max_y = (a.sy.max(b.sy).max(c.sy).ceil() as i32).min(GX_SCREEN_H as i32 - 1);
        if min_x > max_x || min_y > max_y {
            return;
        }

        // Barycentric weights are linear in screen (x, y); precompute the
        // per-pixel + per-row increments so the inner loop only adds.
        let a0 = inv_area * (b.sy - c.sy);
        let b0 = inv_area * (c.sx - b.sx);
        let c0 = inv_area * (b.sx * c.sy - b.sy * c.sx);
        let a1 = inv_area * (c.sy - a.sy);
        let b1 = inv_area * (a.sx - c.sx);
        let c1 = inv_area * (c.sx * a.sy - c.sy * a.sx);

        // Per-vertex premultiplied attributes (× 1/w for perspective-correct).
        let car = a.r * a.inv_w;
        let cag = a.g * a.inv_w;
        let cab = a.b * a.inv_w;
        let cbr = b.r * b.inv_w;
        let cbg = b.g * b.inv_w;
        let cbb = b.b * b.inv_w;
        let ccr = c.r * c.inv_w;
        let ccg = c.g * c.inv_w;
        let ccb = c.b * c.inv_w;
        let sas = a.s * a.inv_w;
        let sat = a.t * a.inv_w;
        let sbs = b.s * b.inv_w;
        let sbt = b.t * b.inv_w;
        let scs = c.s * c.inv_w;
        let sct = c.t * c.inv_w;

        // Polygon alpha (POLYGON_ATTR bits 16..20, 0..31; 0 = opaque "wireframe
        // outline" on hardware, but we treat 0 as fully opaque fill).
        let poly_alpha = (tri.polygon_attr >> 16) & 0x1F;

        for yy in min_y..=max_y {
            let py = yy as f32 + 0.5;
            let fb_row = yy as usize * GX_SCREEN_W;
            let mut w0 = a0 * (min_x as f32 + 0.5) + b0 * py + c0;
            let mut w1 = a1 * (min_x as f32 + 0.5) + b1 * py + c1;
            for xx in min_x..=max_x {
                let w2 = 1.0 - w0 - w1;
                let inside = w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0;
                if inside {
                    let iw = w0 * a.inv_w + w1 * b.inv_w + w2 * c.inv_w;
                    if iw > 0.0 {
                        let inv_iw = 1.0 / iw;
                        // Depth key: larger 1/w = nearer the eye.
                        let idx = fb_row + xx as usize;
                        if iw > self.depth_front[idx] {
                            let vr = (w0 * car + w1 * cbr + w2 * ccr) * inv_iw;
                            let vg = (w0 * cag + w1 * cbg + w2 * ccg) * inv_iw;
                            let vbl = (w0 * cab + w1 * cbb + w2 * ccb) * inv_iw;

                            let mut emit = true;
                            let out_color = if binding.textured {
                                let s = (w0 * sas + w1 * sbs + w2 * scs) * inv_iw;
                                let t = (w0 * sat + w1 * sbt + w2 * sct) * inv_iw;
                                match binding.sample(vram, s, t) {
                                    TexSample::Transparent => {
                                        emit = false;
                                        0
                                    }
                                    TexSample::Untextured => {
                                        clamp31(vr) | (clamp31(vg) << 5) | (clamp31(vbl) << 10)
                                    }
                                    TexSample::Color(c) => {
                                        // Modulate texel by vertex color (+1 to
                                        // avoid the texel vanishing on a dark vtx).
                                        let mr = clamp31(((c & 0x1F) as f32) * (vr + 1.0) / 32.0);
                                        let mg =
                                            clamp31((((c >> 5) & 0x1F) as f32) * (vg + 1.0) / 32.0);
                                        let mb =
                                            clamp31((((c >> 10) & 0x1F) as f32) * (vbl + 1.0) / 32.0);
                                        mr | (mg << 5) | (mb << 10)
                                    }
                                }
                            } else {
                                clamp31(vr) | (clamp31(vg) << 5) | (clamp31(vbl) << 10)
                            };

                            if emit {
                                let final_color = if poly_alpha != 0 && poly_alpha < 31 {
                                    // Alpha blend against whatever is already in
                                    // the framebuffer at this pixel.
                                    let dst = self.fb_front[idx];
                                    blend555(out_color as u16, dst, poly_alpha)
                                } else {
                                    (out_color as u16) | 0x8000
                                };
                                self.fb_front[idx] = final_color | 0x8000;
                                self.drawn_mask_front[idx] = 1;
                                self.depth_front[idx] = iw;
                            }
                        }
                    }
                }
                w0 += a0;
                w1 += a1;
            }
        }
    }

    /// Stable view of the packed 3D-layer scanline most recently produced by
    /// `render_scanline`. Engine A copies this into its BG0 line buffer.
    pub fn line(&self) -> &[u32] {
        &self.line_buf
    }
}

/// Which two axes a partial-vertex command (VTX_XY/XZ/YZ) carries.
#[derive(Clone, Copy)]
enum PartialAxes {
    Xy,
    Xz,
    Yz,
}

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sign_extend(v: u32, bits: u32) -> i32 {
    let m = 1u32 << (bits - 1);
    let masked = v & ((1u32 << bits) - 1);
    (masked as i32) - ((masked & m) as i32) * 2
}

/// Clamp a float channel into the BGR555 0..31 range, returning a `u32` so the
/// caller can shift it into place. Mirrors the TS `clamp31` (which floored).
#[inline]
fn clamp31(v: f32) -> u32 {
    if v <= 0.0 {
        0
    } else if v >= 31.0 {
        31
    } else {
        v as u32
    }
}

/// Alpha-blend `src` BGR555 over `dst` BGR555 with `alpha` ∈ 1..30 (DS 5-bit
/// poly alpha). `(src*a + dst*(31-a)) / 31` per channel. The result has bit 15
/// (drawn) set by the caller.
#[inline]
fn blend555(src: u16, dst: u16, alpha: u32) -> u16 {
    let inv = 31 - alpha;
    let sr = (src & 0x1F) as u32;
    let sg = ((src >> 5) & 0x1F) as u32;
    let sb = ((src >> 10) & 0x1F) as u32;
    let dr = (dst & 0x1F) as u32;
    let dg = ((dst >> 5) & 0x1F) as u32;
    let db = ((dst >> 10) & 0x1F) as u32;
    let r = ((sr * alpha + dr * inv) / 31).min(31);
    let g = ((sg * alpha + dg * inv) / 31).min(31);
    let b = ((sb * alpha + db * inv) / 31).min(31);
    ((b << 10) | (g << 5) | r) as u16
}

/// Result of sampling one texel.
enum TexSample {
    /// A drawable BGR555 color (0..0x7FFF).
    Color(u32),
    /// Transparent texel — skip the pixel.
    Transparent,
    /// Undecoded format (e.g. the 4×4 compressed fallback) — use the Gouraud
    /// vertex color instead of a texel.
    Untextured,
}

/// Per-triangle texture binding, resolved ONCE per polygon (the bank-walking
/// VRAM router lookups must stay out of the pixel loop). Mirrors the TS
/// `prepareTexBinding` + `sampleTexel`.
struct TexBinding {
    /// Whether the polygon is textured AND its texture VRAM is mapped.
    textured: bool,
    /// Flat `vram[]` index of texel (0,0).
    tex_phys: usize,
    /// Flat `vram[]` index of palette entry 0, or `None` for direct-color.
    pal_phys: Option<usize>,
    size_s: i32,
    size_t: i32,
    fmt: u32,
    repeat_s: bool,
    repeat_t: bool,
    flip_s: bool,
    flip_t: bool,
    color0_trans: bool,
}

impl TexBinding {
    fn prepare(tex_param: u32, tex_pltt_base: u32, router: &VramRouter, vramcnt: &[u8; 9]) -> Self {
        let mut tb = TexBinding {
            textured: false,
            tex_phys: 0,
            pal_phys: None,
            size_s: 0,
            size_t: 0,
            fmt: 0,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
            color0_trans: false,
        };
        let fmt = (tex_param >> 26) & 0x7;
        tb.fmt = fmt;
        if fmt == 0 {
            return tb;
        }
        let base_off = (tex_param & 0xFFFF) << 3;
        let tex_phys = match router.resolve_tex_image(base_off, vramcnt) {
            Some(p) => p,
            None => return tb,
        };
        tb.tex_phys = tex_phys;
        // Palette base: 4-color (fmt 2) uses ×8 byte granularity, others ×16.
        let pal_byte_base = if fmt == 2 {
            tex_pltt_base * 8
        } else {
            tex_pltt_base * 16
        };
        tb.pal_phys = if fmt == 7 {
            None // direct-color: no palette
        } else {
            router.resolve_tex_palette(pal_byte_base, vramcnt)
        };
        tb.size_s = 8 << ((tex_param >> 20) & 0x7);
        tb.size_t = 8 << ((tex_param >> 23) & 0x7);
        tb.repeat_s = (tex_param >> 16) & 1 != 0;
        tb.repeat_t = (tex_param >> 17) & 1 != 0;
        tb.flip_s = (tex_param >> 18) & 1 != 0;
        tb.flip_t = (tex_param >> 19) & 1 != 0;
        tb.color0_trans = (tex_param >> 29) & 1 != 0;
        tb.textured = true;
        tb
    }

    /// Sample the bound texture at texel coords `(s, t)`. No router calls.
    fn sample(&self, vram: &[u8], s: f32, t: f32) -> TexSample {
        let u = wrap_coord(s.floor() as i32, self.size_s, self.repeat_s, self.flip_s);
        let v = wrap_coord(t.floor() as i32, self.size_t, self.repeat_t, self.flip_t);
        let texel = (u + v * self.size_s) as usize;
        let base = self.tex_phys;
        // Read a palette entry safely (BGR555).
        let pal_color = |idx: usize| -> u32 {
            match self.pal_phys {
                None => 0,
                Some(pal) => {
                    let o = pal + idx * 2;
                    if o + 1 < vram.len() {
                        ((vram[o] as u32) | ((vram[o + 1] as u32) << 8)) & 0x7FFF
                    } else {
                        0
                    }
                }
            }
        };
        let rd = |o: usize| -> u8 { if o < vram.len() { vram[o] } else { 0 } };

        match self.fmt {
            1 => {
                // A3I5: 3-bit alpha + 5-bit index.
                let byte = rd(base + texel);
                if (byte & 0xE0) == 0 {
                    return TexSample::Transparent;
                }
                TexSample::Color(pal_color((byte & 0x1F) as usize))
            }
            2 => {
                // 4-color (2bpp).
                let idx = (rd(base + (texel >> 2)) >> ((texel & 3) * 2)) & 0x3;
                if idx == 0 && self.color0_trans {
                    return TexSample::Transparent;
                }
                TexSample::Color(pal_color(idx as usize))
            }
            3 => {
                // 16-color (4bpp).
                let byte = rd(base + (texel >> 1));
                let idx = if texel & 1 != 0 { (byte >> 4) & 0xF } else { byte & 0xF };
                if idx == 0 && self.color0_trans {
                    return TexSample::Transparent;
                }
                TexSample::Color(pal_color(idx as usize))
            }
            4 => {
                // 256-color (8bpp).
                let idx = rd(base + texel);
                if idx == 0 && self.color0_trans {
                    return TexSample::Transparent;
                }
                TexSample::Color(pal_color(idx as usize))
            }
            5 => {
                // 4×4-block compressed. Decode the 2-bit index + per-block
                // palette pointer. Texels are grouped into 4×4 blocks; the index
                // data is one byte per 4 texels (a row), and a parallel table
                // (32 KB further into the same 128 KB slot) holds the 16-bit
                // palette/mode word per block.
                self.sample_compressed(vram, u, v)
            }
            6 => {
                // A5I3: 5-bit alpha + 3-bit index.
                let byte = rd(base + texel);
                if (byte & 0xF8) == 0 {
                    return TexSample::Transparent;
                }
                TexSample::Color(pal_color((byte & 0x7) as usize))
            }
            7 => {
                // Direct color (BGR555 + alpha bit).
                let o = base + texel * 2;
                let c = (rd(o) as u32) | ((rd(o + 1) as u32) << 8);
                if (c & 0x8000) == 0 {
                    return TexSample::Transparent;
                }
                TexSample::Color(c & 0x7FFF)
            }
            _ => TexSample::Untextured,
        }
    }

    /// Decode one texel of the 4×4-block compressed (format 5) texture.
    ///
    /// Layout (GBATEK §"Texture format 5"): the texel index data is 2 bits per
    /// texel packed row-major (one byte per 4 horizontal texels). 32 KB into the
    /// same 128 KB texture slot lives a 16-bit word per 4×4 block: bits 0..13 =
    /// palette base offset (in 2-byte units, relative to PLTT_BASE), bit 14 =
    /// "interpolate" mode, bit 15 = "transparent index 3" mode.
    fn sample_compressed(&self, vram: &[u8], u: i32, v: i32) -> TexSample {
        let base = self.tex_phys;
        let blocks_w = (self.size_s / 4).max(1);
        let block_x = u / 4;
        let block_y = v / 4;
        let in_x = (u & 3) as usize;
        let in_y = (v & 3) as usize;
        // Index data: 4 bytes per 4×4 block (one byte per row of 4 texels).
        let block_index = (block_y * blocks_w + block_x) as usize;
        let idx_byte_off = base + block_index * 4 + in_y;
        let idx_byte = if idx_byte_off < vram.len() { vram[idx_byte_off] } else { 0 };
        let pal_idx = ((idx_byte >> (in_x * 2)) & 0x3) as u32;

        // The block's palette-info word lives 0x20000 bytes after the texel data
        // base, one 16-bit word per block.
        let info_off = base + 0x20000 + block_index * 2;
        if info_off + 1 >= vram.len() {
            return TexSample::Untextured;
        }
        let info = (vram[info_off] as u32) | ((vram[info_off + 1] as u32) << 8);
        let pal_base_words = info & 0x3FFF;
        let mode = (info >> 14) & 0x3;

        // The palette for compressed textures lives in the texture-palette space
        // at PLTT_BASE; `pal_phys` already points at entry 0 there.
        let pal = match self.pal_phys {
            Some(p) => p,
            None => return TexSample::Untextured,
        };
        let pal_entry = |i: u32| -> u32 {
            let o = pal + (pal_base_words as usize) * 4 + (i as usize) * 2;
            if o + 1 < vram.len() {
                ((vram[o] as u32) | ((vram[o + 1] as u32) << 8)) & 0x7FFF
            } else {
                0
            }
        };

        // mode bit 0 (info bit 15) = transparent on index 3; mode bit 1
        // (info bit 14) = interpolate colors 0/1.
        let transparent_3 = (mode & 0x2) == 0; // GBATEK: bit15=0 → idx3 transparent
        match pal_idx {
            0 => TexSample::Color(pal_entry(0)),
            1 => TexSample::Color(pal_entry(1)),
            2 => {
                if (mode & 0x1) != 0 {
                    // interpolate (5/8 c0 + 3/8 c1) — approximate as midpoint avg.
                    TexSample::Color(avg555(pal_entry(0), pal_entry(1)))
                } else {
                    TexSample::Color(pal_entry(2))
                }
            }
            _ => {
                if transparent_3 {
                    TexSample::Transparent
                } else if (mode & 0x1) != 0 {
                    TexSample::Color(avg555(pal_entry(0), pal_entry(1)))
                } else {
                    TexSample::Color(pal_entry(3))
                }
            }
        }
    }
}

/// Average two BGR555 colors per channel.
#[inline]
fn avg555(a: u32, b: u32) -> u32 {
    let r = (((a & 0x1F) + (b & 0x1F)) / 2) & 0x1F;
    let g = ((((a >> 5) & 0x1F) + ((b >> 5) & 0x1F)) / 2) & 0x1F;
    let bl = ((((a >> 10) & 0x1F) + ((b >> 10) & 0x1F)) / 2) & 0x1F;
    (bl << 10) | (g << 5) | r
}

/// Map a texel coordinate into `[0, size)` honoring the DS repeat/flip flags.
/// Without repeat the coord is clamped to the edge; with repeat it wraps, and
/// with flip it mirrors every other tile. Ported from the TS `wrapCoord`.
#[inline]
fn wrap_coord(c: i32, size: i32, repeat: bool, flip: bool) -> i32 {
    if size <= 0 {
        return 0;
    }
    if !repeat {
        return c.clamp(0, size - 1);
    }
    let period = size * 2;
    let mut m = c % period;
    if m < 0 {
        m += period;
    }
    if flip && m >= size {
        return period - 1 - m;
    }
    if m >= size {
        m - size
    } else {
        m
    }
}

/// MTX_SCALE (cmd 0x1B): three Q4.12 scale factors → a column-major scale matrix.
fn make_scale(sx: u32, sy: u32, sz: u32) -> Mat4 {
    let mut m = mat4_identity();
    m[0] = sx as i32;
    m[5] = sy as i32;
    m[10] = sz as i32;
    m
}

/// MTX_TRANS (cmd 0x1C): three Q4.12 translations → a column-major translate.
fn make_translate(tx: u32, ty: u32, tz: u32) -> Mat4 {
    let mut m = mat4_identity();
    m[12] = tx as i32;
    m[13] = ty as i32;
    m[14] = tz as i32;
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::irq::Irq;

    #[test]
    fn identity_matrix_apply_is_passthrough() {
        let m = mat4_identity();
        let v = mat4_apply(&m, 3 * FP_ONE, 5 * FP_ONE, 7 * FP_ONE, FP_ONE);
        assert_eq!(v.x, 3 * FP_ONE);
        assert_eq!(v.y, 5 * FP_ONE);
        assert_eq!(v.z, 7 * FP_ONE);
        assert_eq!(v.w, FP_ONE);
    }

    #[test]
    fn mat4_mul_identity_is_identity() {
        let a = mat4_identity();
        let b = make_translate(2 * FP_ONE as u32, 0, 0);
        let r = mat4_mul(&a, &b);
        assert_eq!(r, b);
    }

    #[test]
    fn translate_then_apply_offsets_point() {
        let m = make_translate(2 * FP_ONE as u32, 3 * FP_ONE as u32, 0);
        let v = mat4_apply(&m, FP_ONE, FP_ONE, 0, FP_ONE);
        assert_eq!(v.x, 3 * FP_ONE); // 1 + 2
        assert_eq!(v.y, 4 * FP_ONE); // 1 + 3
    }

    #[test]
    fn mtx_mode_command_selects_matrix() {
        let mut gx = Gpu3d::new();
        let mut irq = Irq::new();
        // MTX_MODE = position (cmd 0x10, param 1) via direct port.
        gx.write_direct(0x0400_0440, 1, &mut irq); // op 0x10
        assert_eq!(gx.mat_mode, MatrixMode::Position);
    }

    #[test]
    fn mtx_push_increments_stack_level_in_gxstat() {
        let mut gx = Gpu3d::new();
        let mut irq = Irq::new();
        gx.mat_mode = MatrixMode::Position;
        gx.matrix_push();
        // GXSTAT bits 8..12 reflect the position stack level.
        assert_eq!((gx.read_stat() >> 8) & 0x1F, 1);
        let _ = &mut irq;
    }

    #[test]
    fn gxstat_reports_empty_and_under_half() {
        let gx = Gpu3d::new();
        let v = gx.read_stat();
        assert_ne!(v & (1 << 25), 0); // < half full
        assert_ne!(v & (1 << 26), 0); // empty
    }

    #[test]
    fn write_stat_arms_irq_when_empty_mode_selected() {
        let mut gx = Gpu3d::new();
        let mut irq = Irq::new();
        // Mode 2 (empty) in bits 30..31.
        gx.write_stat(2 << 30, &mut irq);
        assert_eq!(gx.gxstat_irq_mode, 2);
        assert_ne!(irq.iflag & IRQ_GXFIFO, 0);
    }

    #[test]
    fn fifo_unpacks_and_consumes_zero_param_command() {
        let mut gx = Gpu3d::new();
        let mut irq = Irq::new();
        // Packed word: op 0x15 (MTX_IDENTITY, 0 params) then 0x11 (PUSH, 0).
        // First set mode to position so PUSH targets pos stack.
        gx.mat_mode = MatrixMode::Position;
        gx.write_fifo(0x0000_1115, &mut irq);
        // Both zero-param ops drained immediately; pending empty.
        assert!(gx.pending_ops.is_empty());
        assert_eq!(gx.pos_stack.len(), 1);
    }

    #[test]
    fn color_command_sets_current_color() {
        let mut gx = Gpu3d::new();
        let mut irq = Irq::new();
        // FIFO: op 0x20 (COLOR, 1 param) then param 0x03E0 (green).
        gx.write_fifo(0x0000_0020, &mut irq);
        gx.write_fifo(0x03E0, &mut irq);
        assert_eq!(gx.current_color, 0x03E0);
    }

    #[test]
    fn viewport_register_default_is_full_screen() {
        let gx = Gpu3d::new();
        assert_eq!(gx.viewport.x0, 0);
        assert_eq!(gx.viewport.x1, (GX_SCREEN_W - 1) as u32);
        assert_eq!(gx.viewport.y1, (GX_SCREEN_H - 1) as u32);
    }

    #[test]
    fn disp3dcnt_byte_round_trip() {
        let mut gx = Gpu3d::new();
        assert!(gx.write_reg8(0x0400_0060, 0xA0)); // bit 5 + bit 7
        assert!(gx.write_reg8(0x0400_0061, 0x01));
        assert_eq!(gx.disp3dcnt, 0x01A0);
        assert_eq!(gx.read_reg8(0x0400_0060), Some(0xA0));
        assert_eq!(gx.read_reg8(0x0400_0061), Some(0x01));
    }

    #[test]
    fn fog_table_and_clear_color_route() {
        let mut gx = Gpu3d::new();
        gx.write_reg8(0x0400_0360, 0xFF); // fog density (7-bit masked)
        assert_eq!(gx.fog_table[0], 0x7F);
        gx.write_reg8(0x0400_0350, 0x1F);
        gx.write_reg8(0x0400_0351, 0x00);
        assert_eq!(gx.clear_color & 0xFFFF, 0x001F);
    }

    #[test]
    fn edge_color_table_routes() {
        let mut gx = Gpu3d::new();
        gx.write_reg8(0x0400_0330, 0x00);
        gx.write_reg8(0x0400_0331, 0x7C); // 0x7C00 blue
        assert_eq!(gx.edge_color_table[0], 0x7C00);
    }

    #[test]
    fn vertex16_assembles_into_buffer() {
        let mut gx = Gpu3d::new();
        gx.begin_vertices(PrimType::TriangleList);
        // X = 1.0 (0x1000 in 4.12), Y = 0, Z = 0.
        gx.vertex16(0x0000_1000, 0x0000);
        assert_eq!(gx.vertex_buf.len(), 1);
        assert_eq!(gx.vertex_buf[0].x, FP_ONE);
    }

    #[test]
    fn unpack_4x4_reads_q12_words() {
        let gx = Gpu3d::new();
        let mut p = [0u32; 16];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = (i as u32) * 16;
        }
        let m = gx.unpack_4x4(&p);
        assert_eq!(m[5], 5 * 16);
    }

    // ── Rasterizer + scanline pipeline ────────────────────────────────────

    use crate::memory::{SharedMemory, VramRouter};

    /// Push a clip-space vertex directly into the back display list as a screen
    /// triangle by going through `draw_triangle`. We bypass the matrix pipeline
    /// (identity matrices) so a coordinate of ±FP_ONE maps to NDC ±1 → screen
    /// edges. Returns nothing; appends one ScreenTri to `tris_back`.
    fn push_clip_tri(gx: &mut Gpu3d, verts: [(i32, i32, u16); 3]) {
        gx.prim_type = PrimType::TriangleList;
        gx.vertex_buf.clear();
        for (x, y, color) in verts {
            gx.vertex_buf.push(Vertex {
                x,
                y,
                z: 0,
                w: FP_ONE,
                color,
                s: 0,
                t: 0,
            });
        }
        gx.draw_triangle(0, 1, 2);
    }

    fn swap_and_render(gx: &mut Gpu3d) {
        // Promote the back list and rasterize the whole front scene.
        let mut irq = Irq::new();
        gx.write_fifo(0x0000_0050, &mut irq); // SWAP_BUFFERS (cmd 0x50, 1 param)
        gx.write_fifo(0x0000_0000, &mut irq); // its (ignored) param word
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        for y in 0..GX_SCREEN_H as u32 {
            gx.render_scanline(y, &mem, &router, &[0u8; 9]);
        }
    }

    #[test]
    fn fullscreen_triangle_fills_center_pixel() {
        let mut gx = Gpu3d::new();
        // A big triangle covering the screen center: NDC corners well past the
        // edges so the center (128, 96) is solidly inside.
        push_clip_tri(
            &mut gx,
            [
                (-2 * FP_ONE, -2 * FP_ONE, 0x001F), // red
                (2 * FP_ONE, -2 * FP_ONE, 0x001F),
                (0, 3 * FP_ONE, 0x001F),
            ],
        );
        assert_eq!(gx.tris_back.len(), 1);
        swap_and_render(&mut gx);
        let center = 96 * GX_SCREEN_W + 128;
        assert_eq!(gx.drawn_mask_front[center], 1);
        assert_eq!(gx.fb_front[center] & 0x7FFF, 0x001F);
        assert_ne!(gx.fb_front[center] & 0x8000, 0); // drawn bit
    }

    #[test]
    fn render_scanline_packs_transparent_and_drawn() {
        let mut gx = Gpu3d::new();
        // A small triangle near screen center, leaving the line edges undrawn.
        // NDC ±0.25 → screen x ≈ 96..160, so x=128 is inside, x=0 is outside.
        push_clip_tri(
            &mut gx,
            [
                (-FP_ONE / 4, -FP_ONE / 4, 0x03E0),
                (FP_ONE / 4, -FP_ONE / 4, 0x03E0),
                (0, FP_ONE / 4, 0x03E0),
            ],
        );
        let mut irq = Irq::new();
        gx.write_fifo(0x0000_0050, &mut irq);
        gx.write_fifo(0x0000_0000, &mut irq);
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        // Scanline 100 sits just below NDC y=0; the small triangle still covers
        // x near 128 there. Edges of the line are well outside → transparent.
        let line = gx.render_scanline(100, &mem, &router, &[0u8; 9]).to_vec();
        assert_eq!(line[128] & 0x8000, 0); // drawn
        assert_eq!(line[128] & 0x7FFF, 0x03E0);
        assert_eq!(line[0], super::super::engine_a::PX_TRANSPARENT);
        assert_eq!(line[255], super::super::engine_a::PX_TRANSPARENT);
    }

    #[test]
    fn depth_test_keeps_nearer_triangle() {
        let mut gx = Gpu3d::new();
        // Far triangle (small inv_w via large w) blue, then near triangle red.
        // Both cover the center. Larger inv_w (smaller w) = nearer = wins.
        let big = |color: u16, w: i32| Vertex {
            x: 0,
            y: 0,
            z: 0,
            w,
            color,
            s: 0,
            t: 0,
        };
        // Far blue
        gx.prim_type = PrimType::TriangleList;
        gx.vertex_buf = vec![
            Vertex { x: -2 * FP_ONE, y: -2 * FP_ONE, ..big(0x7C00, 4 * FP_ONE) },
            Vertex { x: 2 * FP_ONE, y: -2 * FP_ONE, ..big(0x7C00, 4 * FP_ONE) },
            Vertex { x: 0, y: 3 * FP_ONE, ..big(0x7C00, 4 * FP_ONE) },
        ];
        gx.draw_triangle(0, 1, 2);
        // Near red
        gx.vertex_buf = vec![
            Vertex { x: -2 * FP_ONE, y: -2 * FP_ONE, ..big(0x001F, FP_ONE) },
            Vertex { x: 2 * FP_ONE, y: -2 * FP_ONE, ..big(0x001F, FP_ONE) },
            Vertex { x: 0, y: 3 * FP_ONE, ..big(0x001F, FP_ONE) },
        ];
        gx.draw_triangle(0, 1, 2);
        swap_and_render(&mut gx);
        let center = 96 * GX_SCREEN_W + 128;
        assert_eq!(gx.fb_front[center] & 0x7FFF, 0x001F); // red (nearer) wins
    }

    #[test]
    fn direct_color_texture_samples_vram() {
        // Bank A (offset 0 in vram[]) mapped as texture slot 0, format 7
        // (direct color). Fill texel (0,0) with opaque green.
        let mut gx = Gpu3d::new();
        let mut mem = SharedMemory::new();
        // green 0x03E0 with alpha bit 15 set.
        let texel: u16 = 0x8000 | 0x03E0;
        mem.vram[0] = (texel & 0xFF) as u8;
        mem.vram[1] = (texel >> 8) as u8;
        // VRAMCNT bank A: enabled + MST=3 (texture), offset 0.
        let mut vramcnt = [0u8; 9];
        vramcnt[0] = 0x83;
        // TEXIMAGE_PARAM: fmt 7 (bits 26..28 = 7), size-S/T = 8 (shift 0), base 0.
        gx.tex_param = 7 << 26;
        gx.tex_pltt_base = 0;
        // Triangle with all texcoords at (0,0) so it samples texel (0,0).
        gx.prim_type = PrimType::TriangleList;
        gx.vertex_buf = vec![
            Vertex { x: -2 * FP_ONE, y: -2 * FP_ONE, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: 2 * FP_ONE, y: -2 * FP_ONE, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: 0, y: 3 * FP_ONE, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
        ];
        gx.draw_triangle(0, 1, 2);
        let mut irq = Irq::new();
        gx.write_fifo(0x0000_0050, &mut irq);
        gx.write_fifo(0x0000_0000, &mut irq);
        let router = VramRouter::new();
        for y in 0..GX_SCREEN_H as u32 {
            gx.render_scanline(y, &mem, &router, &vramcnt);
        }
        let center = 96 * GX_SCREEN_W + 128;
        assert_eq!(gx.drawn_mask_front[center], 1);
        // Modulated by white vertex color → stays green-ish (green channel high).
        assert!((gx.fb_front[center] >> 5) & 0x1F > 0);
        assert_eq!(gx.fb_front[center] & 0x1F, 0); // no red
    }

    #[test]
    fn fog_blends_drawn_line_toward_fog_color() {
        let mut gx = Gpu3d::new();
        push_clip_tri(
            &mut gx,
            [
                (-2 * FP_ONE, -2 * FP_ONE, 0x001F), // red
                (2 * FP_ONE, -2 * FP_ONE, 0x001F),
                (0, 3 * FP_ONE, 0x001F),
            ],
        );
        gx.disp3dcnt = 1 << 7; // enable fog
        gx.fog_color = 0x7C00; // blue
        gx.fog_offset = 0;
        gx.fog_table = [127u8; 32]; // max density everywhere
        let mut irq = Irq::new();
        gx.write_fifo(0x0000_0050, &mut irq);
        gx.write_fifo(0x0000_0000, &mut irq);
        let mem = SharedMemory::new();
        let router = VramRouter::new();
        let line = gx.render_scanline(96, &mem, &router, &[0u8; 9]).to_vec();
        let px = line[128];
        assert_eq!(px & 0x8000, 0); // drawn
        // Heavily fogged → blue rises, red falls below its original 0x1F.
        assert!((px >> 10) & 0x1F > 0);
        assert!(px & 0x1F < 0x1F);
    }

    #[test]
    fn wrap_coord_clamp_and_repeat() {
        // Clamp (no repeat).
        assert_eq!(wrap_coord(-5, 8, false, false), 0);
        assert_eq!(wrap_coord(20, 8, false, false), 7);
        // Repeat wraps modulo size.
        assert_eq!(wrap_coord(9, 8, true, false), 1);
        // Flip mirrors in the second tile.
        assert_eq!(wrap_coord(8, 8, true, true), 7);
        assert_eq!(wrap_coord(15, 8, true, true), 0);
    }

    #[test]
    fn back_facing_degenerate_triangle_is_skipped() {
        let mut gx = Gpu3d::new();
        // All three vertices identical → zero area → no pixels.
        gx.prim_type = PrimType::TriangleList;
        gx.vertex_buf = vec![
            Vertex { x: 0, y: 0, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: 0, y: 0, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: 0, y: 0, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
        ];
        gx.draw_triangle(0, 1, 2);
        swap_and_render(&mut gx);
        assert!(gx.drawn_mask_front.iter().all(|&d| d == 0));
    }

    #[test]
    fn vertex_behind_eye_plane_is_culled() {
        let mut gx = Gpu3d::new();
        gx.prim_type = PrimType::TriangleList;
        gx.vertex_buf = vec![
            Vertex { x: 0, y: 0, z: 0, w: -FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: FP_ONE, y: 0, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
            Vertex { x: 0, y: FP_ONE, z: 0, w: FP_ONE, color: 0x7FFF, s: 0, t: 0 },
        ];
        gx.draw_triangle(0, 1, 2);
        assert_eq!(gx.tris_back.len(), 0); // w <= 0 → not queued
    }
}
