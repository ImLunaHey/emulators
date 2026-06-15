//! The `Xbox` god-struct: owns the Pentium III CPU + memory + the NV2A GPU stub,
//! and implements [`Bus`]. Sibling of the PS1 core's `Psx` and the GC core's `Gc`.
//!
//! Ownership model (see the PS1 core's CONTRACT.md): everything reachable via the
//! bus stays owned by `Xbox`; stepping the CPU `mem::take`s it out so the executor
//! can use `self` as its `&mut dyn Bus`. This phase lands the CPU state, the
//! memory map, the bus routing, and a framebuffer present only — the MMIO devices
//! (NV2A, MCPX, APU, USB, IDE), the boot decryption chain, and the frame timing
//! are stubs/seams. It is NOT a functional Xbox emulator.

use crate::bus::{self, Bus, Region};
use crate::cpu::Cpu;
use crate::gpu::Gpu;
use crate::mem::Mem;
use crate::crash;

pub struct Xbox {
    pub mem: Mem,
    pub cpu: Cpu,
    /// NV2A GPU stub (owns the host framebuffer).
    pub gpu: Gpu,

    /// Completed video frames since reset (one per [`Xbox::run_frame`]).
    pub frames: u32,
    /// Latched controller state (routed to USB later; stored so the wasm surface
    /// has somewhere to put it).
    pub keys: u32,
    /// Set once we've painted the crash screen for the current fault, so we don't
    /// rebuild it every frame.
    crash_shown: bool,
}

impl Default for Xbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Xbox {
    pub fn new() -> Self {
        Xbox {
            mem: Mem::new(),
            cpu: Cpu::new(),
            gpu: Gpu::new(),
            frames: 0,
            keys: 0,
            crash_shown: false,
        }
    }

    /// Load a flash/BIOS image (256 KB retail, or a larger mirrored dump — the
    /// tail is used) and reset to the x86 reset vector (`0xFFFF_FFF0`, inside the
    /// flash mirror). A real Xbox needs the BIOS to boot; without one the CPU
    /// fetches open-bus zeros and traps — expected for this foundation.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.mem.load_bios(bytes);
        self.reset();
    }

    /// Load a game image (an Xbox XBE/XISO). No loader exists yet — the bytes are
    /// dropped — so this is a seam that keeps the host wiring identical to the
    /// other cores. A real implementation parses the XBE/mounts the XISO and
    /// hands control to the kernel.
    pub fn load_rom(&mut self, _bytes: Vec<u8>) {
        // TODO: XBE parser / XISO mount + kernel handoff.
    }

    /// Reset the CPU to the x86 reset vector.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        self.crash_shown = false;
    }

    /// The current display framebuffer as a byte slice (RGBA8888,
    /// `width * height * 4`). Rebuilt at the end of each [`Xbox::run_frame`].
    pub fn framebuffer(&self) -> &[u8] {
        let frame = self.gpu.frame(); // &[u32]
        // SAFETY: `frame` is a contiguous `[u32]`; reinterpret as bytes (the host
        // wants a flat RGBA8888 byte view).
        unsafe { core::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.gpu.display_w as u32
    }
    #[inline]
    pub fn height(&self) -> u32 {
        self.gpu.display_h as u32
    }
    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.frames
    }

    /// Set controller input. The Xbox reads pads over USB; until that subsystem
    /// exists this just latches the bits so the wasm surface has the method.
    pub fn set_keys(&mut self, bits: u32) {
        self.keys = bits;
    }

    /// Drain queued audio samples (interleaved stereo f32). The APU isn't modelled
    /// yet, so this is always empty — present so the host audio path is uniform
    /// with the other cores.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        Vec::new()
    }

    // ============================ frame loop ============================

    /// Pentium III clocks (733 MHz) per video frame at ~60 Hz. Used only as a
    /// conceptual budget; we cap the actual step count well below it so a tight
    /// loop in unfinished boot code can't hang the browser.
    const CYCLES_PER_FRAME: u32 = 733_000_000 / 60;
    /// Instructions to step per frame (capped — see above).
    const STEP_BUDGET: u32 = 1 << 16;

    /// Run a single video frame: step the CPU a fixed instruction budget (until a
    /// fault or HLT), then present. If the CPU has faulted (e.g. it ran into an
    /// unimplemented opcode — which it will, immediately, without a full BIOS),
    /// paint the crash screen so the host shows a legible readout instead of a
    /// black hang. This proves the CPU/bus/GPU plumbing runs a frame without
    /// panicking, exactly like the other cores' first phase.
    pub fn run_frame(&mut self) {
        let _ = Self::CYCLES_PER_FRAME; // documented budget; not yet cycle-accurate
        if self.cpu.fault.is_none() {
            for _ in 0..Self::STEP_BUDGET {
                if self.cpu.fault.is_some() || self.cpu.halted {
                    break;
                }
                self.step_cpu();
            }
        }

        if let Some(fault) = self.cpu.fault {
            if !self.crash_shown {
                let lines = crash_lines(&fault);
                crash::render(&mut self.gpu, &lines);
                self.crash_shown = true;
            }
            self.gpu.frames = self.gpu.frames.wrapping_add(1);
        } else {
            self.gpu.render_frame();
        }
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

    // ---- shared read/write cores (translate → classify → route) ----
    fn read(&mut self, addr: u32, size: u8) -> u32 {
        let region = bus::translate(addr);
        if let Some(v) = self.mem.region_read(region, size) {
            return v;
        }
        match region {
            // MMIO band: open bus for now (no device behaviour).
            Region::Mmio(_) => 0,
            Region::Unmapped => 0,
            // RAM/flash handled by region_read above.
            Region::Ram(_) | Region::Flash(_) => unreachable!(),
        }
    }

    fn write(&mut self, addr: u32, size: u8, v: u32) {
        let region = bus::translate(addr);
        if self.mem.region_write(region, size, v) {
            return;
        }
        match region {
            Region::Mmio(_) => {}   // swallow MMIO writes for now
            Region::Unmapped => {}
            Region::Ram(_) | Region::Flash(_) => {}
        }
    }
}

/// Build the crash-screen text from a recorded CPU fault.
fn crash_lines(fault: &crate::cpu::Fault) -> Vec<String> {
    let name = match fault.vector {
        0 => "DIVIDE ERROR",
        6 => "INVALID OPCODE",
        8 => "DOUBLE FAULT",
        13 => "GENERAL PROTECTION",
        14 => "PAGE FAULT",
        _ => "EXCEPTION",
    };
    vec![
        "XBOX CPU FAULT".to_string(),
        format!("#{} {}", fault.vector, name),
        format!("CS-EIP {:04X}-{:08X}", fault.cs, fault.eip),
        format!("OPCODE {:02X}", fault.opcode),
        "FOUNDATION CORE - NO BIOS HLE".to_string(),
    ]
}

// ============================ Bus impl ============================
impl Bus for Xbox {
    fn read8(&mut self, addr: u32) -> u32 {
        self.read(addr, 1)
    }
    fn read16(&mut self, addr: u32) -> u32 {
        self.read(addr, 2)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.read(addr, 4)
    }
    fn write8(&mut self, addr: u32, v: u32) {
        self.write(addr, 1, v & 0xFF)
    }
    fn write16(&mut self, addr: u32, v: u32) {
        self.write(addr, 2, v & 0xFFFF)
    }
    fn write32(&mut self, addr: u32, v: u32) {
        self.write(addr, 4, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions as R;

    #[test]
    fn ram_round_trips_through_bus_little_endian() {
        let mut xb = Xbox::new();
        xb.write32(0x1000, 0x1122_3344);
        assert_eq!(xb.read32(0x1000), 0x1122_3344);
        assert_eq!(xb.mem.ram[0x1000], 0x44, "little-endian byte order");
    }

    #[test]
    fn flash_reads_back_through_bus() {
        let mut xb = Xbox::new();
        let mut img = vec![0u8; R::FLASH_SIZE];
        img[R::FLASH_SIZE - 4] = 0xEF;
        img[R::FLASH_SIZE - 3] = 0xBE;
        img[R::FLASH_SIZE - 2] = 0xAD;
        img[R::FLASH_SIZE - 1] = 0xDE;
        xb.load_bios(&img);
        // Read via the reset-vector linear address.
        assert_eq!(xb.read32(0xFFFF_FFFC), 0xDEAD_BEEF);
    }

    #[test]
    fn run_frame_without_bios_faults_and_shows_crash() {
        // No BIOS: the reset fetch reads open-bus zeros (opcode 0x00 = ADD with a
        // ModR/M; it will decode, but execution quickly hits an unmapped fetch /
        // unimplemented path). Either way run_frame must not panic and must end
        // with a presented framebuffer.
        let mut xb = Xbox::new();
        xb.run_frame();
        assert_eq!(xb.frame_count(), 1);
        assert_eq!(
            xb.framebuffer().len(),
            (xb.width() * xb.height() * 4) as usize
        );
    }

    #[test]
    fn run_frame_advances_and_presents() {
        let mut xb = Xbox::new();
        // Put an immediate HLT at the reset vector so the CPU stops cleanly.
        let mut img = vec![0u8; R::FLASH_SIZE];
        img[R::FLASH_SIZE - 16] = 0xF4; // hlt at 0xFFFFFFF0
        xb.load_bios(&img);
        let before = xb.frame_count();
        xb.run_frame();
        assert_eq!(xb.frame_count(), before + 1);
        assert!(xb.cpu.halted, "CPU executed the HLT at the reset vector");
    }
}
