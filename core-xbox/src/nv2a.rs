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
    // PFIFO.
    pub const PFIFO_INTR_0: u32 = 0x00_2100;
    pub const PFIFO_INTR_EN_0: u32 = 0x00_2140;
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
    /// PCRTC scanout base (the framebuffer the display reads).
    crtc_start: u32,
    /// Frames signalled (vblank count).
    pub vblank_count: u32,

    // Color surface (filled by PGRAPH later).
    pub has_surface: bool,
    pub surface_offset: u32,
    pub surface_pitch: u32,
    pub width: u16,
    pub height: u16,
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
            crtc_start: 0,
            vblank_count: 0,
            has_surface: false,
            surface_offset: 0,
            surface_pitch: 0,
            width: 640,
            height: 480,
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
            _ => 0,
        }
    }

    /// Write an NV2A register. Interrupt-status writes acknowledge (clear) the
    /// bits written (write-1-to-clear).
    pub fn mmio_write(&mut self, offset: u32, val: u32, _size: u8, ram: &mut [u8]) {
        match offset {
            off::DMA_PUT => {
                self.put = val;
                self.execute(ram);
                self.get = val; // pretend the pushbuffer was consumed
            }
            off::DMA_GET => self.get = val,
            off::PMC_INTR_EN_0 => self.pmc_intr_en = val,
            off::PCRTC_INTR_0 => self.pcrtc_intr &= !val,
            off::PCRTC_INTR_EN_0 => self.pcrtc_intr_en = val,
            off::PTIMER_INTR_0 => self.ptimer_intr &= !val,
            off::PTIMER_INTR_EN_0 => self.ptimer_intr_en = val,
            off::PGRAPH_INTR => self.pgraph_intr &= !val,
            off::PGRAPH_INTR_EN => self.pgraph_intr_en = val,
            off::PFIFO_INTR_0 => self.pfifo_intr &= !val,
            off::PFIFO_INTR_EN_0 => self.pfifo_intr_en = val,
            off::PCRTC_START => self.crtc_start = val,
            _ => {}
        }
    }

    /// Walk + execute the pushbuffer (GET..PUT). Not yet implemented — PGRAPH
    /// method execution lands next.
    fn execute(&mut self, _ram: &mut [u8]) {}

    /// Scan the color surface out to `fb`. Returns the display size if there's a
    /// surface to show, else `None`.
    pub fn scanout(&self, _ram: &[u8], _fb: &mut Vec<u32>) -> Option<(u16, u16)> {
        if !self.has_surface {
            return None;
        }
        None
    }
}
