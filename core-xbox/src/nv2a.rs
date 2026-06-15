//! Nvidia NV2A GPU — minimal model toward visible output.
//!
//! The Xbox GPU is driven by a DMA **pushbuffer**: the game writes a ring of
//! command words in RAM, publishes the write pointer to the channel `PUT`
//! register (in the USER MMIO region at `0xFD80_0000`), and busy-waits until the
//! GPU's `GET` pointer catches up. PGRAPH executes the methods (surface setup,
//! clear, draws) into a color surface in RAM, which PCRTC scans out to video.
//!
//! This module routes the NV2A MMIO window, walks the pushbuffer, executes the
//! handful of PGRAPH methods needed for a visible image (surface setup + clear,
//! then draws), and scans the color surface out to the host framebuffer.
//!
//! STAGE 1 (here): model the channel PUT/GET so the boot's GPU busy-wait
//! completes (GET advances to PUT), unblocking the game. Pushbuffer execution +
//! clear + scanout are filled in next.

/// NV2A register-window offsets (relative to `0xFD00_0000`).
mod off {
    /// Channel control ("USER") region base.
    pub const USER: u32 = 0x80_0000;
    /// DMA put pointer (USER + 0x40).
    pub const DMA_PUT: u32 = USER + 0x40;
    /// DMA get pointer (USER + 0x44).
    pub const DMA_GET: u32 = USER + 0x44;
}

pub struct Nv2a {
    /// Channel DMA put pointer (last value the game published).
    put: u32,
    /// Channel DMA get pointer (how far the GPU has consumed).
    get: u32,
    /// True once PGRAPH has a valid color surface to scan out.
    pub has_surface: bool,
    /// Color-surface parameters (filled by PGRAPH method handling).
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
            has_surface: false,
            surface_offset: 0,
            surface_pitch: 0,
            width: 640,
            height: 480,
        }
    }

    /// Read an NV2A register (offset relative to `0xFD00_0000`).
    pub fn mmio_read(&mut self, offset: u32, _size: u8) -> u32 {
        match offset {
            off::DMA_GET => self.get,
            off::DMA_PUT => self.put,
            _ => 0,
        }
    }

    /// Write an NV2A register. `ram` is guest RAM (for pushbuffer execution).
    pub fn mmio_write(&mut self, offset: u32, val: u32, _size: u8, ram: &mut [u8]) {
        match offset {
            off::DMA_PUT => {
                self.put = val;
                // STAGE 1: pretend the GPU instantly consumed the pushbuffer so
                // the game's busy-wait (GET == PUT) completes. Real execution of
                // the commands between GET..PUT lands next.
                self.execute(ram);
                self.get = val;
            }
            off::DMA_GET => self.get = val,
            _ => {}
        }
    }

    /// Walk + execute the pushbuffer from GET to PUT. STAGE 1: no-op (just makes
    /// progress); PGRAPH method execution is added next.
    fn execute(&mut self, _ram: &mut [u8]) {}

    /// Scan the color surface out to `fb` (RGBA8888). Returns the display size if
    /// there is a surface to show, else `None` (caller keeps the prior screen).
    pub fn scanout(&self, _ram: &[u8], _fb: &mut Vec<u32>) -> Option<(u16, u16)> {
        if !self.has_surface {
            return None;
        }
        None
    }
}
