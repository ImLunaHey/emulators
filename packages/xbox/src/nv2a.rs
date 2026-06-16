//! Nvidia NV2A GPU — minimal model toward visible output.
//!
//! The Xbox GPU is driven by a DMA **pushbuffer**: the game writes a ring of
//! command words in RAM, publishes the write pointer to the channel `PUT`
//! register (USER region at `0xFD80_0000`), and busy-waits until the GPU's `GET`
//! catches up. PGRAPH executes the methods (surface setup, clear, draws) into a
//! color surface in RAM, which PCRTC scans out to video. The GPU also raises
//! interrupts (vblank via PCRTC, command completion via PGRAPH/PFIFO) that the
//! game services through PMC_INTR_0.
//!
//! This module routes the NV2A MMIO window. It currently models: the channel
//! PUT/GET (so the pushbuffer busy-wait completes), and the interrupt registers
//! with a per-frame vblank raise (so the game's interrupt-service loop makes
//! progress). Pushbuffer execution (clear/draw) + scanout are filled in next.

/// NV2A register-window offsets (relative to `0xFD00_0000`).
mod off {
    // PMC (master control) — the top-level interrupt aggregator.
    pub const PMC_INTR_0: u32 = 0x00_0100;
    pub const PMC_INTR_EN_0: u32 = 0x00_0140;
    // PFIFO (base 0x2000; register offsets below are PFIFO-base + reg).
    pub const PFIFO_INTR_0: u32 = 0x00_2100;
    pub const PFIFO_INTR_EN_0: u32 = 0x00_2140;
    /// PFIFO_CACHES (0x2000+0x080): master enable for the CACHE0/CACHE1 caches.
    pub const PFIFO_CACHES: u32 = 0x00_2080;
    /// RAMHT / RAMFC / RAMRO (RAMIN table base configuration).
    pub const PFIFO_RAMHT: u32 = 0x00_2210;
    pub const PFIFO_RAMFC: u32 = 0x00_2214;
    pub const PFIFO_RAMRO: u32 = 0x00_2218;
    /// RUNOUT ring status (0x2000+0x400): LOW_MARK=empty, RANOUT=error.
    pub const PFIFO_RUNOUT_STATUS: u32 = 0x00_2400;
    /// Per-channel push enable / mode select (0x2000+0x500/0x504/0x508).
    pub const PFIFO_REASSIGN: u32 = 0x00_2500; // a.k.a. caches-reassign toggle
    pub const PFIFO_MODE: u32 = 0x00_2504;
    pub const PFIFO_DMA: u32 = 0x00_2508;
    // PFIFO CACHE1 (the active DMA channel). Offsets are PFIFO-base + 0x1xxx.
    pub const PFIFO_CACHE1_PUSH0: u32 = 0x00_3200; // pusher access enable
    pub const PFIFO_CACHE1_PUSH1: u32 = 0x00_3204; // CHID + MODE(PIO/DMA)
    pub const PFIFO_CACHE1_PUT: u32 = 0x00_3210; // method-cache write ptr
    pub const PFIFO_CACHE1_STATUS: u32 = 0x00_3214; // LOW_MARK/HIGH_MARK
    pub const PFIFO_CACHE1_DMA_PUSH: u32 = 0x00_3220; // ACCESS/STATE/STATUS
    pub const PFIFO_CACHE1_DMA_FETCH: u32 = 0x00_3224;
    pub const PFIFO_CACHE1_DMA_STATE: u32 = 0x00_3228;
    pub const PFIFO_CACHE1_DMA_INSTANCE: u32 = 0x00_322C;
    pub const PFIFO_CACHE1_DMA_PUT: u32 = 0x00_3240;
    pub const PFIFO_CACHE1_DMA_GET: u32 = 0x00_3244;
    pub const PFIFO_CACHE1_DMA_SUBROUTINE: u32 = 0x00_324C;
    pub const PFIFO_CACHE1_PULL0: u32 = 0x00_3250; // puller access enable
    pub const PFIFO_CACHE1_PULL1: u32 = 0x00_3254; // puller engine select
    pub const PFIFO_CACHE1_GET: u32 = 0x00_3270; // method-cache read ptr
    pub const PFIFO_CACHE1_ENGINE: u32 = 0x00_3280;
    // PTIMER.
    pub const PTIMER_INTR_0: u32 = 0x00_9100;
    pub const PTIMER_INTR_EN_0: u32 = 0x00_9140;
    // PFB (framebuffer/memory config) + PRAMDAC (GPU clock PLL). The driver
    // validates these during init (computes the GPU frequency from the PLL
    // coefficient and checks the memory-partition count).
    pub const PFB_CFG0: u32 = 0x10_0200;
    pub const PRAMDAC_NVPLL_COEFF: u32 = 0x68_0500;
    // PGRAPH (3D engine).
    pub const PGRAPH_INTR: u32 = 0x40_0100;
    pub const PGRAPH_INTR_EN: u32 = 0x40_0140;
    // PCRTC (display / vblank).
    pub const PCRTC_INTR_0: u32 = 0x60_0100;
    pub const PCRTC_INTR_EN_0: u32 = 0x60_0140;
    pub const PCRTC_START: u32 = 0x60_0800;
    // Channel control ("USER").
    pub const USER: u32 = 0x80_0000;
    pub const DMA_PUT: u32 = USER + 0x40;
    pub const DMA_GET: u32 = USER + 0x44;
}

// PMC_INTR_0 per-engine pending bits (NV master-control layout).
const PMC_INTR_PFIFO: u32 = 1 << 8;
const PMC_INTR_PGRAPH: u32 = 1 << 12;
const PMC_INTR_PTIMER: u32 = 1 << 20;
const PMC_INTR_PCRTC: u32 = 1 << 24;
/// PCRTC_INTR vblank-pending bit.
const PCRTC_INTR_VBLANK: u32 = 1 << 0;
/// PTIMER alarm-pending bit.
const PTIMER_INTR_ALARM: u32 = 1 << 0;

// ---- PFIFO register bit layout (from nv2a_regs.h / envytools nv1-pfifo). ----
/// CACHE1_STATUS: method cache empty (read ptr == write ptr).
const CACHE1_STATUS_LOW_MARK: u32 = 1 << 4;
/// CACHE1_STATUS: method cache full.
const CACHE1_STATUS_HIGH_MARK: u32 = 1 << 8;
/// RUNOUT_STATUS: runout ring empty (no pending error entries) — the idle state.
const RUNOUT_STATUS_LOW_MARK: u32 = 1 << 4;
/// CACHE1_DMA_PUSH: pusher access enabled.
const DMA_PUSH_ACCESS: u32 = 1 << 0;
/// CACHE1_DMA_PUSH: pusher actively running a method run (STATE/busy).
const DMA_PUSH_STATE: u32 = 1 << 4;
/// CACHE1_DMA_PUSH: pusher suspended on error (STATUS).
const DMA_PUSH_STATUS: u32 = 1 << 12;
/// CACHE1_PUSH0/PULL0: access-enable bit.
const ACCESS: u32 = 1 << 0;

pub struct Nv2a {
    put: u32,
    get: u32,

    // Per-engine interrupt pending + enable.
    pcrtc_intr: u32,
    pcrtc_intr_en: u32,
    ptimer_intr: u32,
    ptimer_intr_en: u32,
    pgraph_intr: u32,
    pgraph_intr_en: u32,
    pfifo_intr: u32,
    pfifo_intr_en: u32,
    pmc_intr_en: u32,

    // ---- PFIFO state machine (CACHE1 DMA channel + caches/RAM config). ----
    /// Master cache enable (PFIFO_CACHES) and per-channel mode select.
    pfifo_caches: u32,
    pfifo_reassign: u32,
    pfifo_mode: u32,
    pfifo_dma: u32,
    pfifo_ramht: u32,
    pfifo_ramfc: u32,
    pfifo_ramro: u32,
    /// CACHE1 pusher: PUSH0 access enable, PUSH1 (CHID|MODE).
    cache1_push0: u32,
    cache1_push1: u32,
    /// CACHE1 puller: PULL0 access enable, PULL1 engine select, ENGINE map.
    cache1_pull0: u32,
    cache1_pull1: u32,
    cache1_engine: u32,
    /// CACHE1_STATUS (LOW_MARK/HIGH_MARK); CACHE1 method-cache GET/PUT pointers.
    cache1_status: u32,
    cache1_get: u32,
    cache1_put: u32,
    /// CACHE1 DMA pusher control + scratch.
    cache1_dma_push: u32,
    cache1_dma_fetch: u32,
    cache1_dma_state: u32,
    cache1_dma_instance: u32,
    cache1_dma_subroutine: u32,
    /// RUNOUT ring status (LOW_MARK = empty / idle).
    runout_status: u32,
    /// PCRTC scanout base (the framebuffer the display reads).
    crtc_start: u32,
    /// Frames signalled (vblank count).
    pub vblank_count: u32,

    // ---- Display mode (AvSetDisplayMode): the framebuffer the encoder scans
    // out. On real hardware the game hands this to AvSetDisplayMode (address +
    // pitch + format), independent of the PGRAPH render surface. When set, it
    // gives scanout a concrete surface to present even before PGRAPH establishes
    // one — so a game that programs the display and writes pixels directly (or
    // flips to a back buffer we didn't track as the "surface") still shows up.
    /// Display framebuffer base (guest RAM offset), or 0 if unset.
    disp_addr: u32,
    /// Display row pitch in bytes (0 ⇒ width*4).
    disp_pitch: u32,
    /// Display dimensions in pixels (0 ⇒ unset).
    disp_w: u16,
    disp_h: u16,

    // Color surface (filled by PGRAPH method handling).
    pub has_surface: bool,
    pub surface_offset: u32,
    pub surface_pitch: u32,
    clear_color: u32,
    clip_x: u16,
    clip_y: u16,
    clip_w: u16,
    clip_h: u16,
    /// Clear-rect window (NV097_SET_CLEAR_RECT_*), defaulting to "unset" so the
    /// first clear falls back to the full surface clip. Once a game programs it,
    /// CLEAR_SURFACE is bounded to this rect.
    clear_rect: Option<(u16, u16, u16, u16)>, // (x0, y0, x1, y1)
    pub width: u16,
    pub height: u16,

    // Immediate-mode drawing state.
    prim: Option<u32>,        // current primitive type (BEGIN..END), if drawing
    verts: Vec<Vertex>,       // accumulated vertices
    vcolor: u32,              // current diffuse color (D3DCOLOR ARGB)
    vx: f32,                  // pending vertex X (until Y completes the vertex)
    // Vertex-array draw state (DRAW_ARRAYS / ARRAY_ELEMENT path): per-attribute
    // RAM offset + format word (type/size/stride packed as
    // NV097_SET_VERTEX_DATA_ARRAY_FORMAT).
    va_offset: [u32; 16],
    va_format: [u32; 16],
    // Viewport transform (NV097_SET_VIEWPORT_SCALE / _OFFSET): screen.xyz =
    // ndc.xyz * scale + offset. Zero scale ⇒ unset (fall back to an NDC→surface
    // map). Lets a title drive the exact pixel mapping the GPU would.
    vp_scale: [f32; 4],
    vp_offset: [f32; 4],
    /// Set when a draw rasterized since the last present (the host presents only
    /// completed frames to avoid flashing a mid-clear surface).
    drew_since_present: bool,
    /// Vertex-program constant file (c[0..191] × 4 floats, flat). The transform
    /// matrices live here; the vertex shader multiplies POSITION by them. We
    /// don't run the shader bytecode — we apply the matrix chain c[96]·c[100]·
    /// c[104] (model·view·proj; viewport folded in), which is what every nxdk/
    /// D3D vertex program does for position.
    consts: Box<[f32; 768]>,
    /// Float cursor for SET_TRANSFORM_CONSTANT writes (set by CONSTANT_LOAD).
    const_cursor: usize,
    /// True once any transform constant was uploaded (else fall back to the
    /// viewport-register NDC map for pre-transformed geometry).
    consts_written: bool,
}

#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    color: u32,
}

// PGRAPH (NV20 "Kelvin" 3D class 0x97) method offsets we care about.
mod m {
    pub const SET_SURFACE_CLIP_HORIZONTAL: u32 = 0x0200;
    pub const SET_SURFACE_CLIP_VERTICAL: u32 = 0x0204;
    pub const SET_SURFACE_PITCH: u32 = 0x020C;
    pub const SET_SURFACE_COLOR_OFFSET: u32 = 0x0210;
    pub const SET_COLOR_CLEAR_VALUE: u32 = 0x1D90;
    pub const CLEAR_SURFACE: u32 = 0x1D94;
    // Clear-rect window (NV097_SET_CLEAR_RECT_*): bounds CLEAR_SURFACE to a
    // sub-rectangle. pbkit's on-screen text is drawn as a sequence of small
    // clear-rects, so honouring this is what keeps a clear from whitening the
    // whole screen.
    pub const SET_CLEAR_RECT_HORIZONTAL: u32 = 0x1D98; // (x1<<16)|x0
    pub const SET_CLEAR_RECT_VERTICAL: u32 = 0x1D9C; // (y1<<16)|y0
    // Vertex-array draw path (NV097_SET_VERTEX_DATA_ARRAY_*): per-attribute RAM
    // offset + format (type/size/stride), then DRAW_ARRAYS pulls vertices from
    // those arrays. attr 0 = position, attr 3 = diffuse colour.
    pub const VERTEX_DATA_ARRAY_OFFSET: u32 = 0x1720; // +attr*4 (16 attrs)
    pub const VERTEX_DATA_ARRAY_OFFSET_END: u32 = 0x175C;
    pub const VERTEX_DATA_ARRAY_FORMAT: u32 = 0x1760; // +attr*4 (16 attrs)
    pub const VERTEX_DATA_ARRAY_FORMAT_END: u32 = 0x179C;
    pub const DRAW_ARRAYS: u32 = 0x1810; // (count-1)<<24 | start_index
    pub const ARRAY_ELEMENT16: u32 = 0x1800; // two packed 16-bit vertex indices
    pub const VIEWPORT_OFFSET: u32 = 0x0A20; // +i*4 (x,y,z,w)
    pub const VIEWPORT_OFFSET_END: u32 = 0x0A2C;
    pub const VIEWPORT_SCALE: u32 = 0x0AF0; // +i*4 (x,y,z,w)
    pub const VIEWPORT_SCALE_END: u32 = 0x0AFC;
    // Vertex-program constant file. The shaders multiply POSITION by transform
    // matrices uploaded here (model/view/proj at c[96]/c[100]/c[104]); CONSTANT
    // auto-advances the cursor set by CONSTANT_LOAD.
    pub const SET_TRANSFORM_CONSTANT_LOAD: u32 = 0x1EA4; // cursor = vec4 index
    // SET_TRANSFORM_CONSTANT[0..31]: an increasing run of 32 method slots, each
    // writing one float at the auto-advancing cursor.
    pub const SET_TRANSFORM_CONSTANT: u32 = 0x0B80;
    pub const SET_TRANSFORM_CONSTANT_END: u32 = 0x0BFC;
    // Immediate-mode drawing.
    pub const SET_BEGIN_END: u32 = 0x17FC; // data 0 = end, else begin(primitive)
    pub const VERTEX_POS_X: u32 = 0x1880; // SET_VERTEX_DATA2F attr0 component0
    pub const VERTEX_POS_Y: u32 = 0x1884; // component1 — completes the vertex
    pub const VERTEX_DIFFUSE: u32 = 0x194C; // SET_VERTEX_DATA4UB attr3 (diffuse)
}

/// NV2A primitive types (NV097_SET_BEGIN_END op codes — the wire values the GPU
/// driver actually submits).
const PRIM_TRIANGLES: u32 = 5;
const PRIM_TRIANGLE_STRIP: u32 = 6;
const PRIM_TRIANGLE_FAN: u32 = 7;
const PRIM_QUADS: u32 = 8;
/// Vertex-array attribute slots we read in the DRAW_ARRAYS path.
const VA_POSITION: usize = 0;
const VA_DIFFUSE: usize = 3;

#[inline]
fn rd32(ram: &[u8], addr: u32) -> u32 {
    let i = addr as usize;
    if i + 4 <= ram.len() {
        u32::from_le_bytes([ram[i], ram[i + 1], ram[i + 2], ram[i + 3]])
    } else {
        0
    }
}
#[inline]
fn wr32(ram: &mut [u8], addr: u32, v: u32) {
    let i = addr as usize;
    if i + 4 <= ram.len() {
        ram[i..i + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// 4×4 row-major matrix product (row-vector convention): (a·b)[i][j] = Σ_k
/// a[i][k]·b[k][j].
#[inline]
fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[i * 4 + k] * b[k * 4 + j];
            }
            r[i * 4 + j] = s;
        }
    }
    r
}

/// Row vector × 4×4 matrix: result[j] = Σ_i v[i]·m[i][j] (D3D `mul(v, M)`).
#[inline]
fn vec4_mat4(v: [f32; 4], m: &[f32; 16]) -> [f32; 4] {
    let mut r = [0.0f32; 4];
    for j in 0..4 {
        for i in 0..4 {
            r[j] += v[i] * m[i * 4 + j];
        }
    }
    r
}

/// Read a diffuse-colour vertex attribute into a packed D3DCOLOR (0xAARRGGBB),
/// honouring the attribute format's element type. Type 2 = float components
/// (RGB or RGBA in 0..=1); anything else is treated as an already-packed u32.
#[inline]
fn read_vertex_color(ram: &[u8], addr: u32, format: u32) -> u32 {
    let elem_type = format & 0xF;
    let size = (format >> 4) & 0xF;
    if elem_type == 2 {
        let f = |o: u32| (f32::from_bits(rd32(ram, addr.wrapping_add(o))).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let r = f(0);
        let g = f(4);
        let b = f(8);
        let a = if size >= 4 { f(12) } else { 0xFF };
        (a << 24) | (r << 16) | (g << 8) | b
    } else {
        rd32(ram, addr) | 0xFF00_0000
    }
}

impl Default for Nv2a {
    fn default() -> Self {
        Self::new()
    }
}

impl Nv2a {
    pub fn new() -> Self {
        Nv2a {
            put: 0,
            get: 0,
            pcrtc_intr: 0,
            pcrtc_intr_en: 0,
            ptimer_intr: 0,
            ptimer_intr_en: 0,
            pgraph_intr: 0,
            pgraph_intr_en: 0,
            pfifo_intr: 0,
            pfifo_intr_en: 0,
            pmc_intr_en: 0,
            pfifo_caches: 0,
            pfifo_reassign: 0,
            pfifo_mode: 0,
            pfifo_dma: 0,
            pfifo_ramht: 0,
            pfifo_ramfc: 0,
            pfifo_ramro: 0,
            cache1_push0: 0,
            cache1_push1: 0,
            cache1_pull0: 0,
            cache1_pull1: 0,
            cache1_engine: 0,
            // The method cache starts empty (LOW_MARK set) — nothing queued.
            cache1_status: CACHE1_STATUS_LOW_MARK,
            cache1_get: 0,
            cache1_put: 0,
            cache1_dma_push: 0,
            cache1_dma_fetch: 0,
            cache1_dma_state: 0,
            cache1_dma_instance: 0,
            cache1_dma_subroutine: 0,
            // The runout ring starts empty (LOW_MARK set) — no error entries.
            runout_status: RUNOUT_STATUS_LOW_MARK,
            crtc_start: 0,
            vblank_count: 0,
            disp_addr: 0,
            disp_pitch: 0,
            disp_w: 0,
            disp_h: 0,
            has_surface: false,
            surface_offset: 0,
            surface_pitch: 0,
            clear_color: 0,
            clip_x: 0,
            clip_y: 0,
            clip_w: 640,
            clip_h: 480,
            clear_rect: None,
            // The displayed surface size is established by the first clear
            // (grown to the largest cleared extent); 0 until then.
            width: 0,
            height: 0,
            prim: None,
            verts: Vec::new(),
            vcolor: 0xFFFF_FFFF,
            vx: 0.0,
            va_offset: [0; 16],
            va_format: [0; 16],
            vp_scale: [0.0; 4],
            vp_offset: [0.0; 4],
            drew_since_present: false,
            consts: {
                // c[96]/c[100]/c[104] default to identity so a demo that uploads
                // only some matrices (e.g. the triangle's single viewport matrix)
                // leaves the rest as no-ops in the chain.
                let mut c = Box::new([0.0f32; 768]);
                for base in [96usize, 100, 104] {
                    for i in 0..4 {
                        c[base * 4 + i * 4 + i] = 1.0;
                    }
                }
                c
            },
            const_cursor: 0,
            consts_written: false,
        }
    }

    /// The aggregated PMC_INTR_0 value: one bit per engine with a pending IRQ.
    fn pmc_intr(&self) -> u32 {
        let mut v = 0;
        if self.pcrtc_intr != 0 {
            v |= PMC_INTR_PCRTC;
        }
        if self.ptimer_intr != 0 {
            v |= PMC_INTR_PTIMER;
        }
        if self.pgraph_intr != 0 {
            v |= PMC_INTR_PGRAPH;
        }
        if self.pfifo_intr != 0 {
            v |= PMC_INTR_PFIFO;
        }
        v
    }

    /// Signal a vblank (call once per presented frame): raise the PCRTC vblank
    /// IRQ and tick the PTIMER alarm, so the game's interrupt-service loop sees
    /// pending interrupts and advances its frame/timer bookkeeping.
    pub fn raise_vblank(&mut self) {
        self.pcrtc_intr |= PCRTC_INTR_VBLANK;
        self.ptimer_intr |= PTIMER_INTR_ALARM;
        self.vblank_count = self.vblank_count.wrapping_add(1);
    }

    /// Configure the display framebuffer (from `AvSetDisplayMode`): the address,
    /// pitch, and size the video encoder scans out. `addr` is a guest physical
    /// address (masked into RAM); `pitch` 0 means width*4. Adopting this lets
    /// `scanout` present the game's framebuffer even when PGRAPH never produced a
    /// surface through the methods we model.
    pub fn set_display(&mut self, addr: u32, pitch: u32, width: u16, height: u16) {
        self.disp_addr = addr & (crate::regions::RAM_SIZE as u32 - 1);
        self.disp_pitch = pitch;
        if width != 0 && height != 0 {
            self.disp_w = width;
            self.disp_h = height;
        }
        if std::env::var_os("XBOX_TRACE_GPU").is_some() {
            eprintln!(
                "[gpu] set_display addr={:#X} pitch={} {}x{}",
                self.disp_addr, pitch, self.disp_w, self.disp_h
            );
        }
    }

    /// Read an NV2A register (offset relative to `0xFD00_0000`).
    pub fn mmio_read(&mut self, offset: u32, _size: u8) -> u32 {
        match offset {
            off::PMC_INTR_0 => self.pmc_intr(),
            off::PMC_INTR_EN_0 => self.pmc_intr_en,
            off::PCRTC_INTR_0 => self.pcrtc_intr,
            off::PCRTC_INTR_EN_0 => self.pcrtc_intr_en,
            off::PTIMER_INTR_0 => self.ptimer_intr,
            off::PTIMER_INTR_EN_0 => self.ptimer_intr_en,
            off::PGRAPH_INTR => self.pgraph_intr,
            off::PGRAPH_INTR_EN => self.pgraph_intr_en,
            off::PFIFO_INTR_0 => self.pfifo_intr,
            off::PFIFO_INTR_EN_0 => self.pfifo_intr_en,
            off::PCRTC_START => self.crtc_start,
            // GPU clock PLL coefficient: mdiv=1, ndiv=14, pdiv=0 →
            // (16.666 MHz * 14) / 1 / 1 = 233.333 MHz, the frequency the driver
            // requires (it rejects any other value).
            off::PRAMDAC_NVPLL_COEFF => 0x0000_0E01,
            // Framebuffer config: low 2 bits (memory-partition count) must read 3.
            off::PFB_CFG0 => 0x0307_0003,
            off::DMA_GET => self.get,
            off::DMA_PUT => self.put,
            // ---- PFIFO state machine ----
            // The pusher/puller run synchronously on a kick (see `execute`), so
            // whenever the driver polls, the FIFO is idle: the method cache is
            // empty (LOW_MARK), the runout ring is empty (LOW_MARK), and the DMA
            // pusher is not mid-run (STATE clear). These are exactly the bits the
            // Xbox GPU driver's "wait for FIFO idle" loop checks.
            off::PFIFO_CACHES => self.pfifo_caches,
            off::PFIFO_REASSIGN => self.pfifo_reassign,
            off::PFIFO_MODE => self.pfifo_mode,
            off::PFIFO_DMA => self.pfifo_dma,
            off::PFIFO_RAMHT => self.pfifo_ramht,
            off::PFIFO_RAMFC => self.pfifo_ramfc,
            off::PFIFO_RAMRO => self.pfifo_ramro,
            // RUNOUT empty/idle (LOW_MARK set, no RANOUT error). The driver's
            // bring-up wait requires this bit set to declare the FIFO drained.
            off::PFIFO_RUNOUT_STATUS => self.runout_status,
            off::PFIFO_CACHE1_PUSH0 => self.cache1_push0,
            off::PFIFO_CACHE1_PUSH1 => self.cache1_push1,
            off::PFIFO_CACHE1_PULL0 => self.cache1_pull0,
            off::PFIFO_CACHE1_PULL1 => self.cache1_pull1,
            off::PFIFO_CACHE1_ENGINE => self.cache1_engine,
            // Method cache: empty after every kick (GET == PUT), LOW_MARK set.
            off::PFIFO_CACHE1_STATUS => self.cache1_status,
            off::PFIFO_CACHE1_GET => self.cache1_get,
            off::PFIFO_CACHE1_PUT => self.cache1_put,
            // DMA pusher control: ACCESS reflects what the driver enabled; STATE
            // (busy) is always clear because the pusher already ran to GET==PUT;
            // STATUS (suspended-on-error) is clear (no pushbuffer errors).
            off::PFIFO_CACHE1_DMA_PUSH => {
                self.cache1_dma_push & !(DMA_PUSH_STATE | DMA_PUSH_STATUS)
            }
            off::PFIFO_CACHE1_DMA_FETCH => self.cache1_dma_fetch,
            off::PFIFO_CACHE1_DMA_STATE => self.cache1_dma_state,
            off::PFIFO_CACHE1_DMA_INSTANCE => self.cache1_dma_instance,
            off::PFIFO_CACHE1_DMA_SUBROUTINE => self.cache1_dma_subroutine,
            // GET has caught up to PUT (pushbuffer fully consumed on each kick).
            off::PFIFO_CACHE1_DMA_PUT => self.put,
            off::PFIFO_CACHE1_DMA_GET => self.get,
            _ => 0,
        }
    }

    /// Write an NV2A register. Interrupt-status writes acknowledge (clear) the
    /// bits written (write-1-to-clear).
    pub fn mmio_write(&mut self, offset: u32, val: u32, _size: u8, ram: &mut [u8]) {
        if std::env::var_os("XBOX_TRACE_NVWR").is_some() {
            use std::sync::Mutex;
            static SEEN: Mutex<Option<std::collections::HashSet<u32>>> = Mutex::new(None);
            // Only log writes to real NV2A register windows (skip the PRAMIN/RAM
            // sweep band), and only the first time each distinct offset is seen.
            let interesting = offset < 0x1_0000
                || (0x40_0000..0x41_0000).contains(&offset)
                || (0x60_0000..0x61_0000).contains(&offset)
                || offset >= 0x80_0000;
            if interesting {
                let mut g = SEEN.lock().unwrap();
                let s = g.get_or_insert_with(std::collections::HashSet::new);
                if s.insert(offset) {
                    eprintln!("[nvwr] off={offset:#010X} val={val:#010X}");
                }
            }
        }
        match offset {
            off::DMA_PUT => {
                self.put = val;
                self.kick(ram);
            }
            off::DMA_GET => self.get = val,
            off::PMC_INTR_EN_0 => self.pmc_intr_en = val,
            off::PCRTC_INTR_0 => self.pcrtc_intr &= !val,
            off::PCRTC_INTR_EN_0 => self.pcrtc_intr_en = val,
            off::PTIMER_INTR_0 => self.ptimer_intr &= !val,
            off::PTIMER_INTR_EN_0 => self.ptimer_intr_en = val,
            off::PGRAPH_INTR => self.pgraph_intr &= !val,
            off::PGRAPH_INTR_EN => self.pgraph_intr_en = val,
            off::PFIFO_INTR_0 => self.pfifo_intr &= !val, // write-1-to-clear
            off::PFIFO_INTR_EN_0 => self.pfifo_intr_en = val,
            off::PCRTC_START => self.crtc_start = val,
            // ---- PFIFO state machine config (just latch what the driver sets) ----
            off::PFIFO_CACHES => self.pfifo_caches = val,
            off::PFIFO_REASSIGN => self.pfifo_reassign = val,
            off::PFIFO_MODE => self.pfifo_mode = val,
            off::PFIFO_DMA => self.pfifo_dma = val,
            off::PFIFO_RAMHT => self.pfifo_ramht = val,
            off::PFIFO_RAMFC => self.pfifo_ramfc = val,
            off::PFIFO_RAMRO => self.pfifo_ramro = val,
            off::PFIFO_RUNOUT_STATUS => self.runout_status &= !val, // w1c errors
            off::PFIFO_CACHE1_PUSH0 => self.cache1_push0 = val,
            off::PFIFO_CACHE1_PUSH1 => self.cache1_push1 = val,
            off::PFIFO_CACHE1_PULL0 => self.cache1_pull0 = val,
            off::PFIFO_CACHE1_PULL1 => self.cache1_pull1 = val,
            off::PFIFO_CACHE1_ENGINE => self.cache1_engine = val,
            off::PFIFO_CACHE1_STATUS => self.cache1_status = val,
            off::PFIFO_CACHE1_GET => self.cache1_get = val,
            off::PFIFO_CACHE1_PUT => self.cache1_put = val,
            off::PFIFO_CACHE1_DMA_PUSH => self.cache1_dma_push = val,
            off::PFIFO_CACHE1_DMA_FETCH => self.cache1_dma_fetch = val,
            off::PFIFO_CACHE1_DMA_STATE => self.cache1_dma_state = val,
            off::PFIFO_CACHE1_DMA_INSTANCE => self.cache1_dma_instance = val,
            off::PFIFO_CACHE1_DMA_SUBROUTINE => self.cache1_dma_subroutine = val,
            // The pushbuffer can also be kicked via the PFIFO CACHE1_DMA_PUT
            // register (alternative to the USER DMA_PUT alias above).
            off::PFIFO_CACHE1_DMA_PUT => {
                self.put = val;
                self.kick(ram);
            }
            off::PFIFO_CACHE1_DMA_GET => self.get = val,
            _ => {}
        }
    }

    /// Kick the PFIFO pusher/puller: run the pushbuffer (GET..PUT) to completion,
    /// then leave the engine idle — exactly the state the GPU driver busy-waits
    /// for. Faithful to the NV2A pusher/puller (xemu `pfifo_run_pusher` /
    /// `pfifo_run_puller`): the pusher fetches command words from the channel's
    /// DMA pushbuffer in RAM and feeds methods into CACHE1; the puller dispatches
    /// them to PGRAPH. Because we drain synchronously, when control returns:
    ///   - GET has advanced to PUT (pushbuffer consumed),
    ///   - CACHE1_STATUS.LOW_MARK is set (method cache empty),
    ///   - RUNOUT_STATUS.LOW_MARK is set (no error entries),
    ///   - CACHE1_DMA_PUSH.STATE is clear (pusher not mid-run).
    fn kick(&mut self, ram: &mut [u8]) {
        if std::env::var_os("XBOX_TRACE_FIFO").is_some() {
            eprintln!(
                "[fifo] kick get={:#010X} put={:#010X} push0={:#X} dma_push={:#X}",
                self.get, self.put, self.cache1_push0, self.cache1_dma_push
            );
        }
        // The pusher only runs when the driver has enabled pusher access (PUSH0
        // and DMA_PUSH ACCESS) and it isn't suspended on a prior error. If those
        // gates aren't set we still accept PUT but execute nothing — matching
        // hardware, where the pusher sleeps until access is granted. We keep the
        // legacy unconditional behavior when no gates were ever configured so the
        // existing homebrew clear/draw path (which kicks via DMA_PUT without
        // touching PUSH0) keeps working.
        let gated = self.cache1_push0 != 0 || self.cache1_dma_push != 0;
        let access = !gated
            || (self.cache1_push0 & ACCESS != 0
                && self.cache1_dma_push & DMA_PUSH_ACCESS != 0
                && self.cache1_dma_push & DMA_PUSH_STATUS == 0);
        if access && self.get != self.put {
            self.execute(ram); // pusher fetch + puller dispatch (advances GET)
        }
        // Pushbuffer fully consumed: GET == PUT, caches drained, engine idle.
        self.get = self.put;
        self.cache1_get = self.cache1_put;
        self.cache1_status |= CACHE1_STATUS_LOW_MARK; // method cache empty
        self.cache1_status &= !CACHE1_STATUS_HIGH_MARK; // not full
        self.runout_status |= RUNOUT_STATUS_LOW_MARK; // runout ring empty
        self.cache1_dma_push &= !DMA_PUSH_STATE; // pusher not mid-run
    }

    /// Walk + execute the pushbuffer (GET..PUT). Parses the NV command FIFO and
    /// dispatches PGRAPH methods. The pushbuffer lives in guest RAM (paging off,
    /// so a guest address is a RAM offset).
    fn execute(&mut self, ram: &mut [u8]) {
        let trace = std::env::var_os("XBOX_TRACE_GPU").is_some();
        let put = self.put;
        let mut get = self.get;
        let mut guard = 0u32;
        while get != put && guard < 1_000_000 {
            guard += 1;
            let word = rd32(ram, get);
            get = get.wrapping_add(4);
            if word & 3 == 1 {
                get = word & 0xFFFF_FFFC; // jump
                continue;
            }
            if (word & 0xE000_0003) == 0x2000_0000 {
                get = word & 0x1FFF_FFFF; // old-style jump
                continue;
            }
            let masked = word & 0xE003_0003;
            if masked != 0 && masked != 0x4000_0000 {
                break; // call/return/unknown — stop (we don't model these yet)
            }
            let increasing = masked == 0;
            let mut method = word & 0x1FFC;
            let count = (word >> 18) & 0x7FF;
            for _ in 0..count {
                if get == put {
                    break;
                }
                let data = rd32(ram, get);
                get = get.wrapping_add(4);
                if trace {
                    eprintln!("[gpu] method {method:#06X} = {data:#010X}");
                }
                self.method(ram, method, data);
                if increasing {
                    method += 4;
                }
            }
        }
        self.get = put;
    }

    /// Handle one PGRAPH method (surface setup + clear, for now).
    fn method(&mut self, ram: &mut [u8], method: u32, data: u32) {
        match method {
            m::SET_SURFACE_CLIP_HORIZONTAL => {
                self.clip_x = (data & 0xFFFF) as u16;
                self.clip_w = (data >> 16) as u16;
            }
            m::SET_SURFACE_CLIP_VERTICAL => {
                self.clip_y = (data & 0xFFFF) as u16;
                self.clip_h = (data >> 16) as u16;
            }
            m::SET_SURFACE_PITCH => self.surface_pitch = data & 0xFFFF,
            m::SET_SURFACE_COLOR_OFFSET => {
                self.surface_offset = data & (crate::regions::RAM_SIZE as u32 - 1)
            }
            m::SET_COLOR_CLEAR_VALUE => self.clear_color = data,
            m::SET_CLEAR_RECT_HORIZONTAL => {
                let (x0, x1) = ((data & 0xFFFF) as u16, (data >> 16) as u16);
                let (y0, y1) = self.clear_rect.map(|(_, y0, _, y1)| (y0, y1)).unwrap_or((0, 0));
                self.clear_rect = Some((x0, y0, x1, y1));
            }
            m::SET_CLEAR_RECT_VERTICAL => {
                let (y0, y1) = ((data & 0xFFFF) as u16, (data >> 16) as u16);
                let (x0, x1) = self.clear_rect.map(|(x0, _, x1, _)| (x0, x1)).unwrap_or((0, 0));
                self.clear_rect = Some((x0, y0, x1, y1));
            }
            m::CLEAR_SURFACE => {
                // bit 0x40 = clear color buffer (NV097_CLEAR_SURFACE_COLOR).
                if data & 0xF0 != 0 {
                    self.clear_color_buffer(ram);
                }
            }
            m::VERTEX_DATA_ARRAY_OFFSET..=m::VERTEX_DATA_ARRAY_OFFSET_END => {
                self.va_offset[((method - m::VERTEX_DATA_ARRAY_OFFSET) / 4) as usize] = data;
            }
            m::VERTEX_DATA_ARRAY_FORMAT..=m::VERTEX_DATA_ARRAY_FORMAT_END => {
                self.va_format[((method - m::VERTEX_DATA_ARRAY_FORMAT) / 4) as usize] = data;
            }
            m::DRAW_ARRAYS => self.draw_arrays(ram, data),
            m::ARRAY_ELEMENT16 => {
                // Two packed 16-bit indices into the vertex arrays (low half
                // first). Index 0xFFFF in the high half is a padding sentinel.
                self.array_element(ram, (data & 0xFFFF) as u16);
                let hi = (data >> 16) as u16;
                if hi != 0xFFFF {
                    self.array_element(ram, hi);
                }
            }
            m::VIEWPORT_OFFSET..=m::VIEWPORT_OFFSET_END => {
                self.vp_offset[((method - m::VIEWPORT_OFFSET) / 4) as usize] = f32::from_bits(data);
            }
            m::VIEWPORT_SCALE..=m::VIEWPORT_SCALE_END => {
                self.vp_scale[((method - m::VIEWPORT_SCALE) / 4) as usize] = f32::from_bits(data);
            }
            m::SET_TRANSFORM_CONSTANT_LOAD => self.const_cursor = (data as usize) * 4,
            m::SET_TRANSFORM_CONSTANT..=m::SET_TRANSFORM_CONSTANT_END => {
                if let Some(slot) = self.consts.get_mut(self.const_cursor) {
                    *slot = f32::from_bits(data);
                    self.consts_written = true;
                }
                self.const_cursor += 1;
            }
            m::SET_BEGIN_END => {
                if data == 0 {
                    self.draw_primitives(ram); // END: rasterize what we gathered
                    self.prim = None;
                    self.verts.clear();
                } else {
                    self.prim = Some(data); // BEGIN(primitive)
                    self.verts.clear();
                }
            }
            m::VERTEX_DIFFUSE => self.vcolor = data,
            m::VERTEX_POS_X => self.vx = f32::from_bits(data),
            m::VERTEX_POS_Y => {
                // The second position component completes a vertex.
                let v = Vertex {
                    x: self.vx,
                    y: f32::from_bits(data),
                    color: self.vcolor,
                };
                if self.verts.len() < 4096 {
                    self.verts.push(v);
                }
            }
            _ => {}
        }
    }

    /// Absolute RAM address of the color surface origin.
    ///
    /// `SET_SURFACE_COLOR_OFFSET` is an offset *relative to the color DMA object*
    /// (the framebuffer the game programmed through `AvSetDisplayMode`), not an
    /// absolute RAM address — pbkit submits offset 0 ("start of the framebuffer
    /// DMA object"). Treating it as absolute makes a clear at offset 0 zero RAM
    /// from address 0, scribbling over the game's own code. So resolve it against
    /// the display framebuffer base. The homebrew test path programs no display
    /// (`disp_addr == 0`) and submits an absolute offset, so it's used as-is.
    fn surface_base(&self) -> u32 {
        if self.disp_addr != 0 {
            self.disp_addr.wrapping_add(self.surface_offset)
        } else {
            self.surface_offset
        }
    }

    /// Viewport the DRAW_ARRAYS path maps NDC into: the display dimensions if the
    /// game programmed the encoder, else the surface clip, else 640×480.
    fn viewport_dims(&self) -> (u16, u16) {
        if self.disp_w != 0 && self.disp_h != 0 {
            (self.disp_w, self.disp_h)
        } else if self.clip_w != 0 && self.clip_h != 0 {
            (self.clip_w, self.clip_h)
        } else {
            (640, 480)
        }
    }

    /// Pull `count` vertices from the configured vertex arrays (attr 0 position,
    /// attr 3 diffuse) and append them as screen-space vertices for the current
    /// BEGIN..END primitive. Positions are read as NDC and mapped through the
    /// viewport; colours are read per the attribute's format (float RGB[A] or a
    /// packed D3DCOLOR). This is the path real nxdk/D3D8 geometry uses (vs. the
    /// immediate SET_VERTEX_DATA2F path).
    /// Project a clip/NDC position to screen pixels. With a programmed viewport
    /// (scale != 0): perspective-divide by w, then screen = ndc * scale + offset
    /// (the exact mapping the GPU applies). Otherwise fall back to mapping NDC
    /// across the surface (the pre-transformed homebrew path).
    fn project(&self, x: f32, y: f32, w: f32) -> (f32, f32) {
        let inv_w = if w != 0.0 { 1.0 / w } else { 1.0 };
        let (nx, ny) = (x * inv_w, y * inv_w);
        if self.vp_scale[0] != 0.0 || self.vp_scale[1] != 0.0 {
            (nx * self.vp_scale[0] + self.vp_offset[0], ny * self.vp_scale[1] + self.vp_offset[1])
        } else {
            let (vw, vh) = self.viewport_dims();
            ((nx + 1.0) * 0.5 * vw as f32, (1.0 - ny) * 0.5 * vh as f32)
        }
    }

    /// Read vertex `index` from the configured arrays (attr0 position, attr3
    /// diffuse) and return it as a screen-space vertex. Position may be 3- or
    /// 4-component (the 4th is w for the perspective divide); colour comes from
    /// the diffuse array if enabled, else the current diffuse register.
    /// The transform matrix the vertex program multiplies POSITION by:
    /// model·view·proj (each defaulting to identity). The viewport is folded into
    /// the projection by the demos, so the result (after perspective divide) is
    /// already in screen space.
    fn composite_matrix(&self) -> [f32; 16] {
        let mat = |base: usize| -> [f32; 16] {
            let mut m = [0.0f32; 16];
            m.copy_from_slice(&self.consts[base * 4..base * 4 + 16]);
            m
        };
        mat4_mul(&mat4_mul(&mat(96), &mat(100)), &mat(104))
    }

    fn fetch_vertex(&self, ram: &[u8], index: u32) -> Vertex {
        let pos_fmt = self.va_format[VA_POSITION];
        let pos_size = (pos_fmt >> 4) & 0xF;
        let pos_stride = (pos_fmt >> 8) & 0x00FF_FFFF;
        let pb = self.va_offset[VA_POSITION].wrapping_add(index.wrapping_mul(pos_stride));
        let x = f32::from_bits(rd32(ram, pb));
        let y = f32::from_bits(rd32(ram, pb.wrapping_add(4)));
        let z = if pos_size >= 3 { f32::from_bits(rd32(ram, pb.wrapping_add(8))) } else { 0.0 };
        let w = if pos_size >= 4 { f32::from_bits(rd32(ram, pb.wrapping_add(12))) } else { 1.0 };
        // Run the vertex transform: multiply POSITION by the model·view·proj
        // chain and perspective-divide (the demos bake the viewport into proj, so
        // this lands in screen space). Falls back to the viewport-register NDC map
        // for pre-transformed geometry that uploads no matrices.
        let (sx, sy) = if self.consts_written {
            let c = vec4_mat4([x, y, z, 1.0], &self.composite_matrix());
            let cw = if c[3].abs() > 1e-6 { c[3] } else { 1.0 };
            (c[0] / cw, c[1] / cw)
        } else {
            self.project(x, y, w)
        };
        let col_fmt = self.va_format[VA_DIFFUSE];
        let color = if self.va_offset[VA_DIFFUSE] != 0 && (col_fmt >> 4) & 0xF != 0 {
            let stride = (col_fmt >> 8) & 0x00FF_FFFF;
            read_vertex_color(ram, self.va_offset[VA_DIFFUSE].wrapping_add(index.wrapping_mul(stride)), col_fmt)
        } else {
            self.vcolor
        };
        Vertex { x: sx, y: sy, color }
    }

    /// Establish the surface so draw_primitives proceeds and scanout presents it
    /// even when the background clear was done CPU-side.
    fn mark_surface(&mut self) {
        let (vw, vh) = self.viewport_dims();
        if self.width == 0 {
            self.width = vw;
        }
        if self.height == 0 {
            self.height = vh;
        }
        self.has_surface = true;
    }

    /// NV097_DRAW_ARRAYS: pull `count` sequential vertices from the arrays.
    fn draw_arrays(&mut self, ram: &[u8], data: u32) {
        let count = ((data >> 24) & 0xFF) + 1;
        let start = data & 0x00FF_FFFF;
        for i in start..start.wrapping_add(count) {
            if self.verts.len() >= 4096 {
                break;
            }
            let v = self.fetch_vertex(ram, i);
            self.verts.push(v);
        }
        self.mark_surface();
    }

    /// NV097_ARRAY_ELEMENT16: append one indexed vertex from the arrays.
    fn array_element(&mut self, ram: &[u8], index: u16) {
        if self.verts.len() >= 4096 {
            return;
        }
        let v = self.fetch_vertex(ram, index as u32);
        self.verts.push(v);
        self.mark_surface();
    }

    /// Rasterize the gathered vertices for the current primitive into the color
    /// surface, using the shared software rasterizer in [`crate::nv2a_render`]
    /// (top-left fill rule, perspective-correct interpolation). The immediate-mode
    /// vertices are screen-space (the D3D8/homebrew path submits pre-transformed
    /// positions), so we drive the pass-through (`XYZRHW`) entry point.
    fn draw_primitives(&mut self, ram: &mut [u8]) {
        use crate::nv2a_render as r;
        let prim = match self.prim {
            Some(p) => match p {
                PRIM_TRIANGLES => r::PrimType::Triangles,
                PRIM_TRIANGLE_STRIP => r::PrimType::TriangleStrip,
                PRIM_TRIANGLE_FAN => r::PrimType::TriangleFan,
                PRIM_QUADS => r::PrimType::Quads,
                _ => return,
            },
            None => return,
        };
        if self.verts.is_empty() || self.width == 0 || self.height == 0 {
            return;
        }
        let verts: Vec<r::Vert> = self
            .verts
            .iter()
            .map(|v| r::Vert::new([v.x, v.y, 0.0, 1.0], v.color, [0.0, 0.0]))
            .collect();
        let pitch = if self.surface_pitch == 0 {
            self.width as u32 * 4
        } else {
            self.surface_pitch
        };
        // The rasterizer addresses pixels from the surface base; offset the RAM
        // slice so (0,0) maps to the surface origin.
        let base = self.surface_base() as usize;
        if base >= ram.len() {
            return;
        }
        let mut target = r::Target {
            pixels: &mut ram[base..],
            width: self.width as u32,
            height: self.height as u32,
            pitch,
        };
        r::draw_triangles_screen(&mut target, None, &verts, prim, r::ShadeMode::Gouraud, None);
        self.drew_since_present = true;
    }

    /// Whether a draw landed since the last present (consumed). The host uses
    /// this to present only completed frames (see `Xbox::run_frame`).
    pub fn take_drew(&mut self) -> bool {
        let d = self.drew_since_present;
        self.drew_since_present = false;
        d
    }

    /// Fill the color surface's clip rect with the clear value, and adopt it as
    /// the displayed surface.
    fn clear_color_buffer(&mut self, ram: &mut [u8]) {
        // The clear is bounded by the clear-rect window if the game set one
        // (pbkit draws text as many small clear-rects — without this we'd clear
        // the whole screen for each glyph and whiten everything); otherwise it
        // falls back to the full surface clip.
        let (x0, y0, x1, y1) = match self.clear_rect {
            Some((rx0, ry0, rx1, ry1)) => (rx0 as u32, ry0 as u32, rx1 as u32, ry1 as u32),
            None => {
                let x0 = self.clip_x as u32;
                let y0 = self.clip_y as u32;
                (x0, y0, x0 + self.clip_w.max(1) as u32, y0 + self.clip_h.max(1) as u32)
            }
        };
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // The row stride is the full surface pitch — NOT the rect width — so a
        // sub-rect clear writes the correct pixels.
        let (vw, _) = self.viewport_dims();
        let pitch = if self.surface_pitch == 0 {
            vw.max(x1 as u16) as u32 * 4
        } else {
            self.surface_pitch
        };
        let surface_base = self.surface_base();
        for y in y0..y1 {
            let row = surface_base.wrapping_add(y * pitch);
            for x in x0..x1 {
                wr32(ram, row.wrapping_add(x * 4), self.clear_color);
            }
        }
        // The displayed surface grows to the largest cleared extent (so a small
        // sub-rect clear after a full-screen clear doesn't shrink the screen).
        self.width = self.width.max(x1 as u16);
        self.height = self.height.max(y1 as u16);
        self.has_surface = true;
    }

    #[cfg(test)]
    fn test_get_set(&mut self, o: u32, v: u32, ram: &mut [u8]) {
        self.mmio_write(o, v, 4, ram);
    }

    /// Scan the color surface out to `fb` (converting the Xbox's ARGB/XRGB
    /// little-endian surface to the host's RGBA8888). Returns the display size,
    /// or `None` if no surface has been produced.
    pub fn scanout(&self, ram: &[u8], fb: &mut Vec<u32>) -> Option<(u16, u16)> {
        // Choose the surface to present. Priority:
        //   1. The PGRAPH render surface, once a clear/draw established it (the
        //      homebrew path — unchanged behaviour).
        //   2. The AvSetDisplayMode display framebuffer, if the game programmed
        //      the encoder (so a game that drives the display directly, or flips
        //      to a buffer we didn't track via PGRAPH, still presents).
        let (base, pitch, w, h) = if self.has_surface {
            let w = self.width as u32;
            let pitch = if self.surface_pitch == 0 { w * 4 } else { self.surface_pitch };
            (self.surface_base(), pitch, w, self.height as u32)
        } else if self.disp_w != 0 && self.disp_h != 0 {
            // PCRTC_START, when the driver set it, is the live front buffer; fall
            // back to the AvSetDisplayMode address otherwise.
            let base = if self.crtc_start != 0 {
                self.crtc_start & (crate::regions::RAM_SIZE as u32 - 1)
            } else {
                self.disp_addr
            };
            let w = self.disp_w as u32;
            let pitch = if self.disp_pitch == 0 { w * 4 } else { self.disp_pitch };
            (base, pitch, w, self.disp_h as u32)
        } else {
            return None;
        };
        let (wz, hz) = (w as usize, h as usize);
        fb.resize(wz * hz, 0xFF00_0000);
        for y in 0..hz {
            let row = base.wrapping_add(y as u32 * pitch);
            for x in 0..wz {
                let argb = rd32(ram, row.wrapping_add(x as u32 * 4));
                // ARGB (0xAARRGGBB) -> host RGBA bytes R,G,B,A (0xAABBGGRR word).
                let r = (argb >> 16) & 0xFF;
                let g = (argb >> 8) & 0xFF;
                let b = argb & 0xFF;
                fb[y * wz + x] = 0xFF00_0000 | (b << 16) | (g << 8) | r;
            }
        }
        Some((w as u16, h as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(m: u32) -> u32 {
        (1u32 << 18) | m
    }

    #[test]
    fn display_mode_scanout_presents_framebuffer() {
        // No PGRAPH surface, but the game programmed AvSetDisplayMode: scanout
        // must present that framebuffer (ARGB->RGBA converted).
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x20_0000];
        let fbaddr = 0x10_0000u32;
        // Paint a 2x2 framebuffer: pixel(0,0)=ARGB 0xFF112233.
        wr32(&mut ram, fbaddr, 0xFF11_2233);
        nv.set_display(fbaddr, 2 * 4, 2, 2);
        let mut fb = Vec::new();
        assert_eq!(nv.scanout(&ram, &mut fb), Some((2, 2)));
        // ARGB 112233 -> host RGBA word FF332211.
        assert_eq!(fb[0], 0xFF33_2211);
    }

    #[test]
    fn scanout_none_without_surface_or_display() {
        let nv = Nv2a::new();
        let ram = vec![0u8; 0x1000];
        let mut fb = Vec::new();
        assert_eq!(nv.scanout(&ram, &mut fb), None);
    }

    #[test]
    fn pcrtc_start_overrides_display_address() {
        // When PCRTC_START is set (the live front buffer), scanout reads from it
        // in preference to the AvSetDisplayMode base.
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x20_0000];
        let av = 0x04_0000u32;
        let front = 0x08_0000u32;
        wr32(&mut ram, av, 0xFFAA_AAAA);
        wr32(&mut ram, front, 0xFF12_3456);
        nv.set_display(av, 1 * 4, 1, 1);
        nv.mmio_write(off::PCRTC_START, front, 4, &mut ram);
        let mut fb = Vec::new();
        assert_eq!(nv.scanout(&ram, &mut fb), Some((1, 1)));
        // Reads the PCRTC front buffer (123456 -> RGBA 563412), not av.
        assert_eq!(fb[0], 0xFF56_3412);
    }

    #[test]
    fn pgraph_surface_takes_priority_over_display() {
        // A cleared PGRAPH surface must win over a configured display mode, so the
        // homebrew clear/draw path is unaffected by display programming.
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x20_0000];
        nv.set_display(0x04_0000, 4, 1, 1);
        let mut w: Vec<u32> = Vec::new();
        w.extend([hdr(m::SET_SURFACE_PITCH), 4]);
        w.extend([hdr(m::SET_SURFACE_COLOR_OFFSET), 0x10_0000]);
        w.extend([hdr(m::SET_SURFACE_CLIP_HORIZONTAL), 1 << 16]);
        w.extend([hdr(m::SET_SURFACE_CLIP_VERTICAL), 1 << 16]);
        w.extend([hdr(m::SET_COLOR_CLEAR_VALUE), 0xFF00_FF00]);
        w.extend([hdr(m::CLEAR_SURFACE), 0xF0]);
        let pbuf = 0x1000u32;
        for (i, &word) in w.iter().enumerate() {
            let o = pbuf as usize + i * 4;
            ram[o..o + 4].copy_from_slice(&word.to_le_bytes());
        }
        nv.test_get_set(off::DMA_GET, pbuf, &mut ram);
        nv.test_get_set(off::DMA_PUT, pbuf + w.len() as u32 * 4, &mut ram);
        let mut fb = Vec::new();
        assert_eq!(nv.scanout(&ram, &mut fb), Some((1, 1)));
        // The cleared surface (00FF00 -> RGBA 00FF00), not the display fb.
        assert_eq!(fb[0], 0xFF00_FF00);
    }

    #[test]
    fn fifo_clear_and_triangle_render() {
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x20_0000];
        let surf = 0x10_0000u32;
        // Build a pushbuffer: set surface, clear to ARGB 0xFF112233, then a
        // triangle covering the top-left in red (0xFFD00000).
        let mut w: Vec<u32> = Vec::new();
        w.extend([hdr(m::SET_SURFACE_PITCH), 64 * 4]);
        w.extend([hdr(m::SET_SURFACE_COLOR_OFFSET), surf]);
        w.extend([hdr(m::SET_SURFACE_CLIP_HORIZONTAL), 64 << 16]);
        w.extend([hdr(m::SET_SURFACE_CLIP_VERTICAL), 48 << 16]);
        w.extend([hdr(m::SET_COLOR_CLEAR_VALUE), 0xFF11_2233]);
        w.extend([hdr(m::CLEAR_SURFACE), 0xF0]);
        w.extend([hdr(m::SET_BEGIN_END), PRIM_TRIANGLES]);
        for (x, y) in [(0.0f32, 0.0f32), (40.0, 0.0), (0.0, 40.0)] {
            w.extend([hdr(m::VERTEX_DIFFUSE), 0xFFD0_0000]);
            w.extend([hdr(m::VERTEX_POS_X), x.to_bits()]);
            w.extend([hdr(m::VERTEX_POS_Y), y.to_bits()]);
        }
        w.extend([hdr(m::SET_BEGIN_END), 0]);

        let pbuf = 0x1000u32;
        for (i, &word) in w.iter().enumerate() {
            let o = pbuf as usize + i * 4;
            ram[o..o + 4].copy_from_slice(&word.to_le_bytes());
        }
        nv.test_get_set(off::DMA_GET, pbuf, &mut ram);
        nv.test_get_set(off::DMA_PUT, pbuf + w.len() as u32 * 4, &mut ram);

        let mut fb = Vec::new();
        assert_eq!(nv.scanout(&ram, &mut fb), Some((64, 48)));
        // A far corner keeps the clear color (ARGB 112233 -> RGBA word FF332211).
        assert_eq!(fb[47 * 64 + 63], 0xFF33_2211);
        // The top-left corner is inside the triangle -> red (D00000 -> FF0000D0).
        assert_eq!(fb[0], 0xFF00_00D0);
    }

    #[test]
    fn pfifo_reports_idle() {
        let mut nv = Nv2a::new();
        // CACHE1_STATUS LOW_MARK (empty) set so "wait for FIFO" loops complete.
        assert_eq!(nv.mmio_read(off::PFIFO_CACHE1_STATUS, 4) & 0x10, 0x10);
    }

    #[test]
    fn pfifo_bringup_wait_completes() {
        // The Xbox GPU driver's pushbuffer bring-up busy-wait (the loop Halo 2
        // stalls in) exits only when, after a kick, all three are true:
        //   CACHE1_STATUS.LOW_MARK && RUNOUT_STATUS.LOW_MARK && !DMA_PUSH.STATE
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x10_0000];
        // Driver enables the pusher then kicks with GET == PUT (empty ring) —
        // exactly Halo 2's first kick. This must not walk garbage and must leave
        // the engine idle.
        nv.test_get_set(off::PFIFO_CACHE1_PUSH0, ACCESS, &mut ram);
        nv.test_get_set(off::PFIFO_CACHE1_DMA_PUSH, DMA_PUSH_ACCESS, &mut ram);
        nv.test_get_set(off::PFIFO_CACHE1_DMA_GET, 0x2000, &mut ram);
        nv.test_get_set(off::DMA_PUT, 0x2000, &mut ram); // kick, ring empty
        assert_eq!(
            nv.mmio_read(off::PFIFO_CACHE1_STATUS, 4) & CACHE1_STATUS_LOW_MARK,
            CACHE1_STATUS_LOW_MARK
        );
        assert_eq!(
            nv.mmio_read(off::PFIFO_RUNOUT_STATUS, 4) & RUNOUT_STATUS_LOW_MARK,
            RUNOUT_STATUS_LOW_MARK
        );
        assert_eq!(
            nv.mmio_read(off::PFIFO_CACHE1_DMA_PUSH, 4) & DMA_PUSH_STATE,
            0
        );
        // GET caught up to PUT (pushbuffer consumed / nothing to do).
        assert_eq!(nv.mmio_read(off::PFIFO_CACHE1_DMA_GET, 4), 0x2000);
    }

    #[test]
    fn pfifo_config_registers_round_trip() {
        // The bring-up sequence latches a pile of CACHE1/RAM config; reads must
        // return what the driver wrote (it reads several back during setup).
        let mut nv = Nv2a::new();
        let mut ram = vec![0u8; 0x1000];
        for (o, v) in [
            (off::PFIFO_CACHE1_PUSH1, 0x0000_0100u32),
            (off::PFIFO_CACHE1_DMA_INSTANCE, 0x0000_011C),
            (off::PFIFO_CACHE1_DMA_FETCH, 0x0008_6078),
            (off::PFIFO_CACHE1_PULL0, 0x1),
            (off::PFIFO_MODE, 0x1),
            (off::PFIFO_RAMHT, 0x0300_0000),
            (off::PFIFO_RAMFC, 0x0009_0010),
        ] {
            nv.test_get_set(o, v, &mut ram);
            assert_eq!(nv.mmio_read(o, 4), v, "reg {o:#06X}");
        }
    }
}
