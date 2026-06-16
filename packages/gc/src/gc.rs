//! The `Gc` god-struct: owns the Gekko CPU + memory + the Flipper GPU stub, and
//! implements [`Bus`]. Sibling of the PS1 core's `Psx`.
//!
//! Ownership model (see the PS1 core's CONTRACT.md): everything reachable via
//! the bus stays owned by `Gc`; stepping the CPU `mem::take`s it out so the
//! executor can use `self` as its `&mut dyn Bus`. This phase lands the CPU
//! state, the memory map, the bus routing, and a framebuffer present only —
//! the hardware-register window, the Flipper GX pipeline, and the frame timing
//! are stubs/seams. It is NOT a functional GameCube emulator.

use crate::bus::{self, Bus, Region};
use crate::cpu::Cpu;
use crate::gx::Gx;
use crate::mem::Mem;

pub struct Gc {
    pub mem: Mem,
    pub cpu: Cpu,
    /// Flipper GPU stub (owns the host framebuffer).
    pub gx: Gx,

    /// Hardware-register window backing store (PI/MI/DSP/DI/SI/EXI/AI/CP/PE/VI).
    /// 64 KB of MMIO, modelled as a flat dump for now — no device behaviour.
    /// YAGCD §5. A real implementation routes each sub-range to its device.
    pub hw_regs: Box<[u8; crate::regions::HW_SIZE]>,

    /// Completed video frames since reset (one per [`Gc::run_frame`]).
    pub frames: u32,
}

impl Default for Gc {
    fn default() -> Self {
        Self::new()
    }
}

impl Gc {
    pub fn new() -> Self {
        Gc {
            mem: Mem::new(),
            cpu: Cpu::new(),
            gx: Gx::new(),
            hw_regs: vec![0u8; crate::regions::HW_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            frames: 0,
        }
    }

    /// Load an IPL boot-ROM image (≤ 2 MB) and reset to the Gekko reset vector
    /// (`0xFFF0_0100`, inside the IPL window). A real GameCube needs the IPL to
    /// boot a game; without one the CPU just fetches zeros and traps — that's
    /// expected for this foundation.
    pub fn load_ipl(&mut self, bytes: &[u8]) {
        self.mem.load_ipl(bytes);
        self.reset();
    }

    /// Reset the CPU to the Gekko reset vector.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new(); // pc = RESET_VECTOR (0xFFF0_0100, the IPL)
    }

    /// The current display framebuffer as a byte slice (RGBA8888,
    /// `width * height * 4`). Rebuilt at the end of each [`Gc::run_frame`].
    pub fn framebuffer(&self) -> &[u8] {
        let frame = self.gx.frame(); // &[u32]
        // SAFETY: `frame` is a contiguous `[u32]`; reinterpret as bytes (the
        // host wants a flat RGBA8888 byte view).
        unsafe { core::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) }
    }

    /// Display width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.gx.display_w as u32
    }
    /// Display height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.gx.display_h as u32
    }
    /// Completed frames since reset.
    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.frames
    }

    /// Set controller input. The GameCube reads pads through the SI; until that
    /// subsystem exists this is a stub so the wasm surface has the method.
    pub fn set_keys(&mut self, _bits: u32) {
        // TODO: route to the SI/controller subsystem when it lands.
    }

    // ============================ frame loop ============================

    /// Gekko clocks (486 MHz) per video frame at ~59.94 Hz NTSC. Used only as
    /// an instruction budget for `run_frame`; nothing observable yet.
    const CYCLES_PER_FRAME: u32 = 486_000_000 / 60;
    /// Instructions to step per frame. The Gekko is roughly 1+ IPC, but with no
    /// device timing to honour we just step a fixed budget so `run_frame` makes
    /// forward progress without spinning forever.
    const STEP_BUDGET: u32 = Self::CYCLES_PER_FRAME;

    /// Run a single video frame: step the CPU a fixed instruction budget, then
    /// present the framebuffer. There is no meaningful emulation yet — this just
    /// proves the CPU/bus/GPU plumbing runs a frame without panicking, exactly
    /// like the other cores' first phase.
    pub fn run_frame(&mut self) {
        for _ in 0..Self::STEP_BUDGET.min(1 << 16) {
            self.step_cpu();
        }
        self.gx.render_frame();
        self.frames = self.frames.wrapping_add(1);
    }

    /// Step the CPU exactly one instruction, split-borrowing it out of `self` so
    /// the executor can use `self` as its `&mut dyn Bus` (the contract's
    /// `mem::take` pattern). `Cpu: Default`.
    fn step_cpu(&mut self) {
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.step(self);
        self.cpu = cpu;
    }

    // ---- hardware-register window (flat backing store; no device behaviour) --
    fn hw_read(&self, off: u32, size: u8) -> u32 {
        let i = (off as usize) & (crate::regions::HW_SIZE - 1);
        match size {
            1 => self.hw_regs[i] as u32,
            2 if i + 1 < crate::regions::HW_SIZE => Mem::rd32_be(&self.hw_regs[..], i & !1) >> 16,
            _ if i + 3 < crate::regions::HW_SIZE => Mem::rd32_be(&self.hw_regs[..], i & !3),
            _ => 0,
        }
    }
    fn hw_write(&mut self, off: u32, size: u8, v: u32) {
        let i = (off as usize) & (crate::regions::HW_SIZE - 1);
        match size {
            1 => self.hw_regs[i] = (v & 0xFF) as u8,
            2 if i + 1 < crate::regions::HW_SIZE => {
                self.hw_regs[i] = ((v >> 8) & 0xFF) as u8;
                self.hw_regs[i + 1] = (v & 0xFF) as u8;
            }
            _ if i + 3 < crate::regions::HW_SIZE => Mem::wr32_be(&mut self.hw_regs[..], i & !3, v),
            _ => {}
        }
    }

    // ---- shared read/write cores (translate → classify → route) ----
    fn read(&mut self, addr: u32, size: u8) -> u32 {
        let region = bus::translate(addr);
        if let Some(v) = self.mem.region_read(region, size) {
            return v;
        }
        match region {
            Region::Hw(off) => self.hw_read(off, size),
            Region::Unmapped => 0,
            // RAM/IPL handled by region_read above.
            Region::Ram(_) | Region::Ipl(_) => unreachable!(),
        }
    }

    fn write(&mut self, addr: u32, size: u8, v: u32) {
        let region = bus::translate(addr);
        if self.mem.region_write(region, size, v) {
            return;
        }
        match region {
            Region::Hw(off) => self.hw_write(off, size, v),
            Region::Unmapped => {}
            Region::Ram(_) | Region::Ipl(_) => {}
        }
    }
}

// ============================ Bus impl ============================
impl Bus for Gc {
    fn read8(&mut self, addr: u32) -> u32 {
        self.read(addr, 1)
    }
    fn read16(&mut self, addr: u32) -> u32 {
        self.read(addr & !1, 2)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.read(addr & !3, 4)
    }
    fn read64(&mut self, addr: u32) -> u64 {
        let region = bus::translate(addr & !7);
        if let Some(v) = self.mem.region_read64(region) {
            return v;
        }
        // HW/unmapped: assemble from two 32-bit reads (big-endian: high word
        // first).
        let hi = self.read(addr & !7, 4) as u64;
        let lo = self.read((addr & !7).wrapping_add(4), 4) as u64;
        (hi << 32) | lo
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
    fn write64(&mut self, addr: u32, v: u64) {
        let region = bus::translate(addr & !7);
        if self.mem.region_write64(region, v) {
            return;
        }
        self.write(addr & !7, 4, (v >> 32) as u32);
        self.write((addr & !7).wrapping_add(4), 4, v as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_round_trips_through_bus_big_endian() {
        let mut gc = Gc::new();
        gc.write32(0x8000_1000, 0x1122_3344);
        assert_eq!(gc.read32(0x8000_1000), 0x1122_3344);
        // The uncached mirror sees the same DRAM.
        assert_eq!(gc.read32(0xC000_1000), 0x1122_3344);
        // Big-endian byte order in the backing store.
        assert_eq!(gc.mem.ram[0x1000], 0x11);
    }

    #[test]
    fn read64_assembles_big_endian_doubleword() {
        let mut gc = Gc::new();
        gc.write64(0x8000_2000, 0x0011_2233_4455_6677);
        assert_eq!(gc.read64(0x8000_2000), 0x0011_2233_4455_6677);
        assert_eq!(gc.read32(0x8000_2000), 0x0011_2233, "high word first");
        assert_eq!(gc.read32(0x8000_2004), 0x4455_6677);
    }

    #[test]
    fn hw_register_window_round_trips() {
        let mut gc = Gc::new();
        gc.write32(0xCC00_3000, 0xCAFE_F00D);
        assert_eq!(gc.read32(0xCC00_3000), 0xCAFE_F00D);
    }

    #[test]
    fn run_frame_advances_without_panic() {
        // A self-looping program (`b .` — branch to self) must let run_frame
        // step its whole budget, present a framebuffer, and bump the counter
        // without panicking.
        let mut gc = Gc::new();
        gc.write32(0x8000_0000, 0x4800_0000); // b 0 (infinite self-branch)
        gc.cpu.pc = 0x8000_0000;
        let before = gc.frame_count();
        gc.run_frame();
        gc.run_frame();
        assert_eq!(gc.frame_count(), before + 2);
        assert_eq!(
            gc.framebuffer().len(),
            (gc.width() * gc.height() * 4) as usize
        );
    }

    #[test]
    fn reset_points_at_ipl_vector() {
        let gc = Gc::new();
        assert_eq!(gc.cpu.pc, crate::cpu::state::RESET_VECTOR);
    }
}
