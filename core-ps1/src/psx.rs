//! The `Psx` god-struct: owns the R3000A CPU + memory + a slot for every
//! subsystem, and implements [`Bus`]. Sibling of the GBA core's `Gba`.
//!
//! Ownership model (see CONTRACT.md): everything reachable via the bus stays
//! owned by `Psx`; a subsystem method that itself needs `&mut dyn Bus`
//! (= `&mut Psx`) is run by `mem::take`-ing the device(s) it mutates and
//! passing `self` as the bus. This phase lands the CPU state, the memory map,
//! and the bus routing only — the sub-device reads/writes and the frame loop
//! are `todo!()` seams filled in by later agents.

use crate::bus::{self, Bus, Region};
use crate::cpu::Cpu;
use crate::memory::Mem;

pub struct Psx {
    pub mem: Mem,
    pub cpu: Cpu,

    // ---- subsystem slots (empty until their agents land) ----
    // Each is a unit placeholder so the field exists and `Psx` is a stable
    // shape to wire against; the real structs replace these types later.
    pub gte: (),
    pub gpu: (),
    pub spu: (),
    pub dma: (),
    pub timers: (),
    pub cdrom: (),
    pub mdec: (),
    pub irq: (),
    pub sio: (),

    /// Memory-control register file (0x1F80_1000..0x1F80_1024 + RAM_SIZE at
    /// 0x1F80_1060). Backing store only for now.
    pub mem_control: [u32; 9],
    /// RAM_SIZE register (0x1F80_1060).
    pub ram_size: u32,
    /// Cache-control register (KSEG2 @ 0xFFFE_0130).
    pub cache_control: u32,
}

impl Default for Psx {
    fn default() -> Self {
        Self::new()
    }
}

impl Psx {
    pub fn new() -> Self {
        Psx {
            mem: Mem::new(),
            cpu: Cpu::new(),
            gte: (),
            gpu: (),
            spu: (),
            dma: (),
            timers: (),
            cdrom: (),
            mdec: (),
            irq: (),
            sio: (),
            mem_control: [0; 9],
            // BIOS programs this to 0x0000_0B88 (2 MB mirrored in 8 MB).
            ram_size: 0x0000_0B88,
            cache_control: 0,
        }
    }

    /// Load a BIOS ROM image (≤ 512 KB).
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.mem.load_bios(bytes);
    }

    /// Run a single frame. Filled in once the CPU executor + GPU timing land.
    pub fn run_frame(&mut self) {
        todo!("frame loop: CPU dispatch + sub-device stepping not yet ported")
    }

    // ---- cache-control / memory-control register window ----
    fn cache_control_read(&self) -> u32 {
        self.cache_control
    }
    fn cache_control_write(&mut self, v: u32) {
        self.cache_control = v;
    }

    // ============================ I/O dispatch ============================
    // The 8 KB hardware I/O window (0x1F80_1000..0x1F80_3000). Sub-device
    // routing (IRQ control, DMA, timers, CDROM, GPU, MDEC, SPU, SIO, and the
    // memory-control registers) lands per-agent; until then every access is a
    // `todo!()` seam so an unimplemented port faults loudly rather than reading
    // garbage. `off` is relative to [`crate::regions::IO_BASE`].
    fn io_read(&mut self, off: u32, size: u8) -> u32 {
        let _ = (off, size);
        todo!("I/O register reads (IRQ/DMA/timers/CDROM/GPU/MDEC/SPU/SIO) not yet ported")
    }
    fn io_write(&mut self, off: u32, size: u8, v: u32) {
        let _ = (off, size, v);
        todo!("I/O register writes (IRQ/DMA/timers/CDROM/GPU/MDEC/SPU/SIO) not yet ported")
    }

    // ---- shared read/write cores (translate → classify → route) ----
    fn read(&mut self, addr: u32, size: u8) -> u32 {
        let region = bus::translate(addr);
        if let Some(v) = self.mem.region_read(region, size) {
            return v;
        }
        match region {
            Region::Io(off) => self.io_read(off, size),
            Region::CacheControl => self.cache_control_read(),
            // Expansion regions are typically unpopulated; reads float high.
            Region::Expansion1(_) | Region::Expansion2(_) | Region::Expansion3(_) => 0xFFFF_FFFF,
            Region::Unmapped => 0xFFFF_FFFF,
            // RAM/Scratchpad/BIOS were handled by region_read above.
            Region::Ram(_) | Region::Scratchpad(_) | Region::Bios(_) => unreachable!(),
        }
    }

    fn write(&mut self, addr: u32, size: u8, v: u32) {
        // Cache-isolation (SR.IsC): when set, stores hit the data cache (which
        // doubles as the scratchpad) rather than main RAM. We model this by
        // dropping RAM writes entirely — the BIOS uses it to invalidate the
        // i-cache during boot and never expects the RAM to change.
        let isolated = self.cpu.cache_isolated();
        let region = bus::translate(addr);

        if isolated {
            if let Region::Ram(_) = region {
                return;
            }
        }

        if self.mem.region_write(region, size, v) {
            return;
        }
        match region {
            Region::Io(off) => self.io_write(off, size, v),
            Region::CacheControl => self.cache_control_write(v),
            Region::Expansion1(_) | Region::Expansion2(_) | Region::Expansion3(_) => {}
            Region::Unmapped => {}
            Region::Ram(_) | Region::Scratchpad(_) | Region::Bios(_) => {}
        }
    }
}

// ============================ Bus impl ============================
impl Bus for Psx {
    fn read8(&mut self, addr: u32) -> u32 {
        self.read(addr, 1)
    }
    fn read16(&mut self, addr: u32) -> u32 {
        self.read(addr & !1, 2)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.read(addr & !3, 4)
    }
    fn write8(&mut self, addr: u32, v: u32) {
        self.write(addr, 1, v & 0xFF)
    }
    fn write16(&mut self, addr: u32, v: u32) {
        self.write(addr & !1, 2, v & 0xFFFF)
    }
    fn write32(&mut self, addr: u32, v: u32) {
        self.write(addr & !3, 4, v)
    }
}
