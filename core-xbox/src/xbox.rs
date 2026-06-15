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
use crate::cpu::state::{CR0_PE, ESP};
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

    /// A mounted game disc, if [`Xbox::load_rom`] was given an XDVDFS image. When
    /// present, [`Xbox::run_frame`] shows a "disc mounted" info screen instead of
    /// stepping the CPU (the foundation can't boot the game, but it can identify
    /// it — see [`crate::xiso`]).
    disc: Option<crate::xiso::DiscInfo>,
    disc_shown: bool,

    /// True once a game XBE is loaded and the CPU is executing it. `run_frame`
    /// then steps the interpreter instead of showing the identify screen.
    booted: bool,
    /// Title shown on the boot diagnostic.
    boot_title: String,
    /// The XBE entry point (for the diagnostic).
    boot_entry: u32,
    /// Kernel-import ordinals indexed by thunk slot — parallel to the HLE stub
    /// addresses patched into the thunk table. A CALL into the HLE region is a
    /// call to the ordinal at that slot.
    kernel_ordinals: Vec<u32>,
    /// The ordinal of the first kernel import the game called (the seam where the
    /// foundation stops: it has no HLE kernel).
    last_kernel_ordinal: Option<u32>,

    /// NV2A GPU (pushbuffer + scanout).
    nv2a: crate::nv2a::Nv2a,
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
            disc: None,
            disc_shown: false,
            booted: false,
            boot_title: String::new(),
            boot_entry: 0,
            kernel_ordinals: Vec::new(),
            last_kernel_ordinal: None,
            nv2a: crate::nv2a::Nv2a::new(),
        }
    }

    /// Linear base of the HLE kernel-import trap region (in the unmapped band).
    /// Each thunk slot is mapped to `HLE_BASE + slot*4`; a CALL there means the
    /// game invoked the kernel import recorded for that slot.
    const HLE_BASE: u32 = 0x8000_0000;
    const HLE_SPAN: u32 = 0x4000;
    /// Initial stack pointer — high in RAM, clear of the loaded image.
    const STACK_TOP: u32 = 0x0300_0000;

    /// Load a flash/BIOS image (256 KB retail, or a larger mirrored dump — the
    /// tail is used) and reset to the x86 reset vector (`0xFFFF_FFF0`, inside the
    /// flash mirror). A real Xbox needs the BIOS to boot; without one the CPU
    /// fetches open-bus zeros and traps — expected for this foundation.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.mem.load_bios(bytes);
        self.reset();
    }

    /// Load a game image: a raw `.xbe`, or an XDVDFS disc (we extract its
    /// `default.xbe`). The XBE is mapped into RAM and the CPU starts at its entry
    /// point — [`Xbox::run_frame`] then executes the game's real x86 code. It will
    /// run the CRT/startup until it calls a kernel import (no HLE kernel yet) or
    /// hits an unimplemented instruction; the boot diagnostic reports how far.
    pub fn load_rom(&mut self, bytes: Vec<u8>) {
        // Raw XBE.
        if bytes.get(0..4) == Some(&b"XBEH"[..]) {
            self.boot_xbe(&bytes, "DEFAULT.XBE");
            return;
        }
        // Disc image — identify, then extract + boot default.xbe.
        if let Some(info) = crate::xiso::probe(&bytes) {
            let title = if info.title.is_empty() { "XBOX".into() } else { info.title.clone() };
            if info.xbe_size > 0 {
                let start = info.xbe_offset;
                let end = start.saturating_add(info.xbe_size).min(bytes.len());
                if start < end {
                    let xbe = bytes[start..end].to_vec();
                    if self.boot_xbe(&xbe, &title) {
                        self.disc = Some(info);
                        // Retain the disc image so the HLE filesystem can serve
                        // the game's file reads (moves the bytes into the kernel).
                        crate::hle::set_disc(bytes);
                        return;
                    }
                }
            }
            // Couldn't boot: fall back to the identify-only screen.
            self.disc = Some(info);
            self.disc_shown = false;
        }
    }

    /// Map an XBE into RAM, patch its kernel-import thunks to HLE trap stubs, and
    /// point the CPU at the entry point in flat 32-bit protected mode. Returns
    /// false if the bytes aren't a valid XBE.
    pub fn boot_xbe(&mut self, xbe: &[u8], title: &str) -> bool {
        let img = match crate::xbe::parse(xbe) {
            Some(i) => i,
            None => return false,
        };
        // Fresh RAM + HLE state (allocator/scheduler/files/kernel-data globals).
        crate::hle::reset();
        self.mem = Mem::new();
        crate::xbe::load_into(&img, xbe, &mut self.mem.ram[..]);

        // Patch the kernel-import thunk table. Each entry is `0x8000_0000|ordinal`:
        // - a FUNCTION export becomes a unique HLE call-stub address (a CALL there
        //   traps to our dispatch);
        // - a DATA export becomes the address of a real backing variable (the game
        //   reads it as memory, e.g. KeTickCount).
        self.kernel_ordinals.clear();
        let mut addr = img.kernel_thunk;
        while addr != 0 {
            let v = self.mem.ram_read32(addr);
            if v == 0 {
                break;
            }
            let ordinal = v & 0x7FFF_FFFF;
            if crate::hle_table::is_data_export(ordinal) {
                let data_addr = crate::hle::data_export_addr(ordinal, &mut self.mem);
                self.mem.ram_write32(addr, data_addr);
            } else {
                let idx = self.kernel_ordinals.len() as u32;
                self.kernel_ordinals.push(ordinal);
                self.mem.ram_write32(addr, Self::HLE_BASE + idx * 4);
                if idx >= Self::HLE_SPAN / 4 - 1 {
                    break;
                }
            }
            addr = addr.wrapping_add(4);
        }

        // CPU: flat 32-bit protected mode (PE set, segment bases 0), entry point,
        // a high stack clear of the image.
        self.cpu = Cpu::new();
        self.cpu.cr[0] |= CR0_PE;
        for s in 0..6 {
            self.cpu.seg_sel[s] = 0x08;
            self.cpu.seg_base[s] = 0;
        }
        // Push a thread-exit sentinel as the entry's return address, so when the
        // entry routine returns (after init) it terminates cleanly instead of
        // popping off the empty stack into garbage.
        let sp = Self::STACK_TOP - 4;
        self.mem.ram_write32(sp, crate::hle::THREAD_EXIT_SENTINEL);
        self.cpu.eip = img.entry;
        self.cpu.set_reg32(ESP, sp);

        self.booted = true;
        self.boot_title = title.to_string();
        self.boot_entry = img.entry;
        self.last_kernel_ordinal = None;
        self.crash_shown = false;
        self.disc = None;
        true
    }

    /// Reset the CPU to the x86 reset vector.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        self.crash_shown = false;
        self.booted = false;
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

        // A booted game: step the CPU and show a live boot diagnostic.
        if self.booted {
            // Signal one vblank per frame so the game's interrupt-service loop
            // advances its frame/timer bookkeeping.
            self.nv2a.raise_vblank();
            crate::hle::tick_clock(&mut self.mem); // advance KeTickCount
            // Deliver the connected device ISR (vblank) — runs the game's
            // interrupt handler, which acks the GPU and signals its vblank event.
            crate::hle::deliver_isr(&mut self.cpu, &mut self.mem);
            if self.cpu.fault.is_none() {
                let mut quantum = 0u32;
                let mut steps = 0u32;
                while steps < Self::STEP_BUDGET {
                    steps += 1;
                    let eip = self.cpu.eip;
                    trace_eip(eip);

                    // A delivered ISR returned: restore the interrupted context.
                    if eip == crate::hle::ISR_RETURN_SENTINEL {
                        crate::hle::isr_return(&mut self.cpu);
                        continue;
                    }
                    // A thread returned from its entry routine: terminate it and
                    // schedule another.
                    if eip == crate::hle::THREAD_EXIT_SENTINEL {
                        crate::hle::terminate_current(&mut self.cpu);
                        continue;
                    }
                    // Kernel-import boundary: route the call through the HLE kernel
                    // (which may also switch threads). Unhandled -> stop + report.
                    if (Self::HLE_BASE..Self::HLE_BASE + Self::HLE_SPAN).contains(&eip) {
                        let idx = ((eip - Self::HLE_BASE) / 4) as usize;
                        let ord = self.kernel_ordinals.get(idx).copied().unwrap_or(0);
                        match crate::hle::dispatch(&mut self.cpu, &mut self.mem, ord) {
                            crate::hle::Dispatch::Handled(_) => {
                                trace_kernel(ord);
                                continue;
                            }
                            crate::hle::Dispatch::Unhandled(_) => {
                                self.last_kernel_ordinal = Some(ord);
                                break;
                            }
                        }
                    }
                    if self.cpu.fault.is_some() {
                        break;
                    }
                    // A thread idled (HLT): hand the slice to another thread.
                    if self.cpu.halted {
                        self.cpu.halted = false;
                        crate::hle::preempt(&mut self.cpu);
                        continue;
                    }
                    self.step_cpu();
                    // Round-robin preemption so a busy-waiting thread can't starve
                    // the loaders/workers it's waiting on.
                    quantum += 1;
                    if quantum >= 8192 {
                        quantum = 0;
                        crate::hle::preempt(&mut self.cpu);
                    }
                }
            }
            // If the GPU has produced a color surface, scan it out to the screen;
            // otherwise show the live boot diagnostic.
            if let Some((w, h)) = self.nv2a.scanout(&self.mem.ram[..], &mut self.gpu.framebuffer) {
                self.gpu.display_w = w;
                self.gpu.display_h = h;
            } else {
                let lines = self.boot_lines();
                crash::render(&mut self.gpu, &lines);
            }
            self.gpu.frames = self.gpu.frames.wrapping_add(1);
            self.frames = self.frames.wrapping_add(1);
            return;
        }

        // A mounted disc takes priority: identify it on screen. The foundation
        // can't boot it, so there's no CPU to step here.
        if self.disc.is_some() {
            if !self.disc_shown {
                let lines = disc_lines(self.disc.as_ref().unwrap());
                crash::render(&mut self.gpu, &lines);
                self.disc_shown = true;
            }
            self.gpu.frames = self.gpu.frames.wrapping_add(1);
            self.frames = self.frames.wrapping_add(1);
            return;
        }

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
            // MMIO band: the NV2A GPU window lives at the bottom (0xFD00_0000);
            // route it there. Other devices are still open-bus.
            Region::Mmio(off) => {
                trace_mmio(addr);
                self.nv2a.mmio_read(off, size)
            }
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
            Region::Mmio(off) => self.nv2a.mmio_write(off, v, size, &mut self.mem.ram[..]),
            Region::Unmapped => {}
            Region::Ram(_) | Region::Flash(_) => {}
        }
    }
}

impl Xbox {
    /// Public view of the boot diagnostic (for headless harnesses).
    pub fn boot_diagnostic(&self) -> Vec<String> {
        self.boot_lines()
    }

    /// The kernel-import ordinals the loaded XBE links against (intel for an HLE
    /// kernel: these are the OS functions the game will call).
    pub fn kernel_imports(&self) -> &[u32] {
        &self.kernel_ordinals
    }

    /// Build the live boot-diagnostic text: title, entry, how far the CPU got,
    /// and where it stopped (kernel import, fault, HLT, or still running).
    fn boot_lines(&self) -> Vec<String> {
        let status = if let Some(f) = self.cpu.fault {
            let what = match f.vector {
                0 => "DIV ERROR",
                6 => "BAD OPCODE",
                13 => "GEN PROTECT",
                14 => "PAGE FAULT",
                _ => "EXCEPTION",
            };
            format!("STOP  {} OP {:02X} EIP {:08X}", what, f.opcode, f.eip)
        } else if let Some(ord) = self.last_kernel_ordinal {
            match crate::hle_table::lookup(ord) {
                Some((name, _)) => format!("STOP  KERNEL {} ({})", ord, name.to_uppercase()),
                None => format!("STOP  KERNEL IMPORT ORD {}", ord),
            }
        } else if self.cpu.halted {
            "STOP  HLT".to_string()
        } else {
            "RUNNING".to_string()
        };
        vec![
            format!("BOOTING {}", self.boot_title.to_uppercase()),
            format!("ENTRY {:08X}  EIP {:08X}", self.boot_entry, self.cpu.eip),
            format!("INSTRS {}", self.cpu.instret),
            format!("IMPORTS {}", self.kernel_ordinals.len()),
            status,
        ]
    }
}

/// Debug aid: count EIP visits (gated on `XBOX_TRACE_EIP`) and surface the
/// hottest instruction — pinpoints a spin loop's PC.
use std::collections::HashMap as EipMap;
static EIP_HIST: std::sync::Mutex<Option<EipMap<u32, u64>>> = std::sync::Mutex::new(None);

fn trace_eip(eip: u32) {
    if std::env::var_os("XBOX_TRACE_EIP").is_none() {
        return;
    }
    let mut g = EIP_HIST.lock().unwrap();
    *g.get_or_insert_with(EipMap::new).entry(eip).or_insert(0) += 1;
}

/// Print the hottest instruction addresses (the spin loop). Call after running.
pub fn dump_eip_hist() {
    let g = EIP_HIST.lock().unwrap();
    if let Some(m) = g.as_ref() {
        let mut v: Vec<(u32, u64)> = m.iter().map(|(&k, &c)| (k, c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("[eip top]");
        for (eip, c) in v.into_iter().take(10) {
            eprintln!("  {eip:#010X}  {c}");
        }
    }
}

/// Debug aid: count kernel-import calls per ordinal (gated on `XBOX_TRACE_KERNEL`)
/// and surface the "hot" ones a boot loop hammers — what the game is waiting on.
fn trace_kernel(ord: u32) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static COUNTS: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);
    if std::env::var_os("XBOX_TRACE_KERNEL").is_none() {
        return;
    }
    let mut g = COUNTS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    let c = m.entry(ord).or_insert(0);
    *c += 1;
    if *c == 1 {
        let name = crate::hle_table::lookup(ord).map(|(n, _)| n).unwrap_or("?");
        eprintln!("[krn] ord {ord} {name}");
    }
}

/// Debug aid: log each distinct MMIO address the guest reads (gated on the
/// `XBOX_TRACE_MMIO` env var) — used to find what device a boot spin-loop polls.
fn trace_mmio(addr: u32) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static COUNTS: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);
    if std::env::var_os("XBOX_TRACE_MMIO").is_none() {
        return;
    }
    let mut g = COUNTS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    let c = m.entry(addr).or_insert(0);
    *c += 1;
    // Surface the "hot" register a spin loop hammers (printed once at 50k reads).
    if *c == 50_000 {
        eprintln!("[mmio HOT] {addr:#010X} (spin-polled)");
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

/// Build the "disc mounted" info screen text from a parsed disc.
fn disc_lines(d: &crate::xiso::DiscInfo) -> Vec<String> {
    let title = if d.title.is_empty() {
        "UNKNOWN".to_string()
    } else {
        d.title.to_uppercase()
    };
    vec![
        "XBOX DISC MOUNTED".to_string(),
        format!("TITLE  {}", title),
        format!(
            "ID     {} {}",
            d.publisher().to_uppercase(),
            d.game_number()
        ),
        format!("ENTRY  {:08X} {}", d.entry, d.entry_key.to_uppercase()),
        format!("FILES  {}", d.files.len()),
        "NO EXECUTION - FOUNDATION CORE".to_string(),
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
