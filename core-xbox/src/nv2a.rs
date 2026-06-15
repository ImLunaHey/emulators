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

    // Color surface (filled by PGRAPH method handling).
    pub has_surface: bool,
    pub surface_offset: u32,
    pub surface_pitch: u32,
    clear_color: u32,
    clip_x: u16,
    clip_y: u16,
    clip_w: u16,
    clip_h: u16,
    pub width: u16,
    pub height: u16,

    // Immediate-mode drawing state.
    prim: Option<u32>,        // current primitive type (BEGIN..END), if drawing
    verts: Vec<Vertex>,       // accumulated vertices
    vcolor: u32,              // current diffuse color (D3DCOLOR ARGB)
    vx: f32,                  // pending vertex X (until Y completes the vertex)
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
    // Immediate-mode drawing.
    pub const SET_BEGIN_END: u32 = 0x17FC; // data 0 = end, else begin(primitive)
    pub const VERTEX_POS_X: u32 = 0x1880; // SET_VERTEX_DATA2F attr0 component0
    pub const VERTEX_POS_Y: u32 = 0x1884; // component1 — completes the vertex
    pub const VERTEX_DIFFUSE: u32 = 0x194C; // SET_VERTEX_DATA4UB attr3 (diffuse)
}

/// NV2A primitive types (the ones we rasterize).
const PRIM_TRIANGLES: u32 = 4;
const PRIM_TRIANGLE_STRIP: u32 = 5;
const PRIM_TRIANGLE_FAN: u32 = 6;
const PRIM_QUADS: u32 = 8;

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
            has_surface: false,
            surface_offset: 0,
            surface_pitch: 0,
            clear_color: 0,
            clip_x: 0,
            clip_y: 0,
            clip_w: 640,
            clip_h: 480,
            // The displayed surface size is established by the first clear
            // (grown to the largest cleared extent); 0 until then.
            width: 0,
            height: 0,
            prim: None,
            verts: Vec::new(),
            vcolor: 0xFFFF_FFFF,
            vx: 0.0,
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
            m::CLEAR_SURFACE => {
                // bit 0x40 = clear color buffer (NV097_CLEAR_SURFACE_COLOR).
                if data & 0xF0 != 0 {
                    self.clear_color_buffer(ram);
                }
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

    /// Rasterize the gathered vertices for the current primitive into the color
    /// surface. Vertices are taken as screen-space (the homebrew submits
    /// pre-transformed positions); flat-shaded with the first vertex's color.
    fn draw_primitives(&mut self, ram: &mut [u8]) {
        let prim = match self.prim {
            Some(p) => p,
            None => return,
        };
        let v = self.verts.clone();
        match prim {
            PRIM_TRIANGLES => {
                for t in v.chunks_exact(3) {
                    self.fill_triangle(ram, t[0], t[1], t[2]);
                }
            }
            PRIM_TRIANGLE_FAN | PRIM_QUADS if v.len() >= 3 => {
                for i in 1..v.len() - 1 {
                    self.fill_triangle(ram, v[0], v[i], v[i + 1]);
                }
            }
            PRIM_TRIANGLE_STRIP if v.len() >= 3 => {
                for i in 0..v.len() - 2 {
                    self.fill_triangle(ram, v[i], v[i + 1], v[i + 2]);
                }
            }
            _ => {}
        }
    }

    /// Flat-fill a triangle into the surface (edge-function rasterizer).
    fn fill_triangle(&mut self, ram: &mut [u8], a: Vertex, b: Vertex, c: Vertex) {
        let pitch = if self.surface_pitch == 0 {
            self.width as u32 * 4
        } else {
            self.surface_pitch
        };
        let (sw, sh) = (self.width as i32, self.height as i32);
        let minx = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
        let maxx = (a.x.max(b.x).max(c.x).ceil() as i32).min(sw - 1);
        let miny = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
        let maxy = (a.y.max(b.y).max(c.y).ceil() as i32).min(sh - 1);
        let edge = |ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32| {
            (px - ax) * (by - ay) - (py - ay) * (bx - ax)
        };
        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area.abs() < 1e-3 {
            return;
        }
        // Per-channel barycentric interpolation of the three vertex colors.
        let chan = |color: u32, sh: u32| ((color >> sh) & 0xFF) as f32;
        for y in miny..=maxy {
            for x in minx..=maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = edge(b.x, b.y, c.x, c.y, px, py);
                let w1 = edge(c.x, c.y, a.x, a.y, px, py);
                let w2 = edge(a.x, a.y, b.x, b.y, px, py);
                let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                    || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                let (l0, l1, l2) = (w0 / area, w1 / area, w2 / area);
                let mix = |sh: u32| {
                    (l0 * chan(a.color, sh) + l1 * chan(b.color, sh) + l2 * chan(c.color, sh))
                        .clamp(0.0, 255.0) as u32
                };
                let color = 0xFF00_0000 | (mix(16) << 16) | (mix(8) << 8) | mix(0);
                let off = self.surface_offset.wrapping_add(y as u32 * pitch + x as u32 * 4);
                wr32(ram, off, color);
            }
        }
    }

    /// Fill the color surface's clip rect with the clear value, and adopt it as
    /// the displayed surface.
    fn clear_color_buffer(&mut self, ram: &mut [u8]) {
        let x0 = self.clip_x as u32;
        let y0 = self.clip_y as u32;
        let w = self.clip_w.max(1) as u32;
        let h = self.clip_h.max(1) as u32;
        let pitch = if self.surface_pitch == 0 {
            (x0 + w) * 4
        } else {
            self.surface_pitch
        };
        for y in y0..y0 + h {
            let row = self.surface_offset.wrapping_add(y * pitch);
            for x in x0..x0 + w {
                wr32(ram, row.wrapping_add(x * 4), self.clear_color);
            }
        }
        // The displayed surface grows to the largest cleared extent (so a small
        // sub-rect clear after a full-screen clear doesn't shrink the screen).
        self.width = self.width.max((x0 + w) as u16);
        self.height = self.height.max((y0 + h) as u16);
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
        if !self.has_surface {
            return None;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let pitch = if self.surface_pitch == 0 {
            (w * 4) as u32
        } else {
            self.surface_pitch
        };
        fb.resize(w * h, 0xFF00_0000);
        for y in 0..h {
            let row = self.surface_offset.wrapping_add(y as u32 * pitch);
            for x in 0..w {
                let argb = rd32(ram, row.wrapping_add(x as u32 * 4));
                // ARGB (0xAARRGGBB) -> host RGBA bytes R,G,B,A (0xAABBGGRR word).
                let r = (argb >> 16) & 0xFF;
                let g = (argb >> 8) & 0xFF;
                let b = argb & 0xFF;
                fb[y * w + x] = 0xFF00_0000 | (b << 16) | (g << 8) | r;
            }
        }
        Some((self.width, self.height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(m: u32) -> u32 {
        (1u32 << 18) | m
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
