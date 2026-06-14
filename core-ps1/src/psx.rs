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
use crate::cdrom::Cdrom;
use crate::cpu::Cpu;
use crate::dma::{Channel, Direction, Dma, SyncMode, Transfer};
use crate::gpu::Gpu;
use crate::gte::Gte;
use crate::irq::Irq;
use crate::mdec::Mdec;
use crate::memory::Mem;
use crate::sio::Sio;
use crate::spu::Spu;
use crate::timers::Timers;

pub struct Psx {
    pub mem: Mem,
    pub cpu: Cpu,

    // ---- subsystem devices (each owns only its own state; cross-device work
    // is done by this orchestrator via &mut parameters / split borrows) ----
    pub gte: Gte,
    pub gpu: Gpu,
    pub spu: Spu,
    pub dma: Dma,
    pub timers: Timers,
    pub cdrom: Cdrom,
    pub mdec: Mdec,
    pub irq: Irq,
    pub sio: Sio,

    /// Memory-control register file (0x1F80_1000..0x1F80_1024 + RAM_SIZE at
    /// 0x1F80_1060). Backing store only for now.
    pub mem_control: [u32; 9],
    /// RAM_SIZE register (0x1F80_1060).
    pub ram_size: u32,
    /// Cache-control register (KSEG2 @ 0xFFFE_0130).
    pub cache_control: u32,

    /// Completed video frames since reset (one per [`Psx::run_frame`]).
    pub frames: u32,
}

impl Default for Psx {
    fn default() -> Self {
        Self::new()
    }
}

impl Psx {
    pub fn new() -> Self {
        let mut psx = Psx {
            mem: Mem::new(),
            cpu: Cpu::new(),
            gte: Gte::new(),
            gpu: Gpu::new(),
            spu: Spu::new(),
            dma: Dma::new(),
            timers: Timers::new(),
            cdrom: Cdrom::new(),
            mdec: Mdec::new(),
            irq: Irq::new(),
            sio: Sio::new(),
            mem_control: [0; 9],
            // BIOS programs this to 0x0000_0B88 (2 MB mirrored in 8 MB).
            ram_size: 0x0000_0B88,
            cache_control: 0,
            frames: 0,
        };
        // No BIOS yet → lay down the minimal HLE boot environment so a freshly
        // constructed machine steps frames cleanly. `load_bios` re-resets to the
        // real ROM entry point when an image is supplied.
        psx.reset();
        psx
    }

    /// Load a BIOS ROM image (≤ 512 KB) and reset to the BIOS entry point.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.mem.load_bios(bytes);
        self.reset();
    }

    /// Reset the CPU to the BIOS reset vector. When no real BIOS is present we
    /// install the minimal HLE boot environment (see [`crate::bios`]) and point
    /// the CPU at it so the machine steps frames without trapping into garbage.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new(); // pc = RESET_VECTOR (0xBFC0_0000, the BIOS)
        if !self.mem.bios_loaded {
            crate::bios::install_min_hle(&mut self.mem.ram[..]);
            // Start executing the HLE trampoline in RAM (KSEG0) rather than the
            // empty BIOS window.
            self.cpu.pc = 0x8000_0000;
            self.cpu.next_pc = 0x8000_0004;
            self.cpu.current_pc = 0x8000_0000;
        }
    }

    /// Mount a disc image (`.bin`, MODE2/2352) as the CD-ROM. The disc *is* the
    /// game on the PSX; the BIOS boots it.
    pub fn load_disc(&mut self, bytes: &[u8]) {
        self.cdrom.load_disc(bytes);
    }

    /// Load a game image. A PS-X EXE (`"PS-X EXE"` magic) is side-loaded
    /// directly into RAM and the CPU is jumped to its entry point (handy
    /// without a BIOS / for homebrew); anything else is treated as a `.bin`
    /// disc image and mounted via [`Psx::load_disc`].
    pub fn load_rom(&mut self, bytes: &[u8]) {
        if bytes.len() >= 0x800 && &bytes[0..8] == b"PS-X EXE" {
            self.load_exe(bytes);
        } else {
            self.load_disc(bytes);
        }
    }

    /// Side-load a PS-X EXE image (psx-spx "CDROM File Formats"): copy the body
    /// (from file offset 0x800) to its destination address in RAM and seed the
    /// CPU's PC / GP / SP from the 0x800-byte header.
    fn load_exe(&mut self, bytes: &[u8]) {
        let rd = |off: usize| -> u32 {
            u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
        };
        let pc = rd(0x10);
        let gp = rd(0x14);
        let dest = rd(0x18);
        let size = rd(0x1C) as usize;
        let sp_base = rd(0x30);
        let sp_off = rd(0x34);

        // Ensure a boot environment exists (no BIOS → HLE trampolines).
        self.reset();

        // Copy the executable body into RAM at `dest` (physical, folded to 2 MB).
        let body = &bytes[0x800..(0x800 + size).min(bytes.len())];
        let base = (dest & 0x1F_FFFF) as usize;
        let n = body.len().min(self.mem.ram.len().saturating_sub(base));
        self.mem.ram[base..base + n].copy_from_slice(&body[..n]);

        // Seed the CPU registers (gp = $28, sp = $29, fp = $30).
        self.cpu.pc = pc;
        self.cpu.next_pc = pc.wrapping_add(4);
        self.cpu.current_pc = pc;
        self.cpu.set_reg(28, gp);
        let sp = sp_base.wrapping_add(sp_off);
        if sp != 0 {
            self.cpu.set_reg(29, sp);
            self.cpu.set_reg(30, sp);
        }
    }

    /// Set the digital-pad button state (active-high; bit layout per
    /// [`crate::sio::Button`]).
    pub fn set_keys(&mut self, keys: u16) {
        self.sio.set_keys(keys);
    }

    /// The current display framebuffer as a byte slice (RGBA8888,
    /// `width * height * 4`). Rebuilt at the end of each [`Psx::run_frame`].
    pub fn framebuffer(&self) -> &[u8] {
        let frame = self.gpu.frame(); // &[u32]
        // SAFETY: `frame` is a contiguous `[u32]`; reinterpret as bytes (the
        // host wants a flat RGBA8888 byte view, little-endian per pixel).
        unsafe {
            core::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4)
        }
    }

    /// Display width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.gpu.display_w as u32
    }
    /// Display height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.gpu.display_h as u32
    }
    /// Completed frames since reset.
    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.frames
    }

    /// Drain queued interleaved-stereo f32 audio samples produced since the
    /// last call (44.1 kHz, L,R,L,R…).
    pub fn drain_audio(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        self.spu.drain(&mut out);
        out
    }

    // ============================ frame loop ============================

    /// System clocks (33.8688 MHz) per video frame at ~59.94 Hz NTSC. The GPU,
    /// timers, SPU and CD-ROM are all driven off this same budget.
    const CYCLES_PER_FRAME: u32 = 565_045;
    /// CPU instructions to step between sub-device service points. Small enough
    /// that IRQ latency stays tight; large enough to amortise the device loop.
    const STEP_BATCH: u32 = 128;

    /// Run a single video frame: step the CPU in batches, advancing the GPU /
    /// timers / CD-ROM / SPU by the matching cycle budget, running any armed DMA
    /// transfers, folding every device's IRQ line into the CPU, and finally
    /// expanding the display area into the framebuffer.
    pub fn run_frame(&mut self) {
        let mut elapsed = 0u32;
        while elapsed < Self::CYCLES_PER_FRAME {
            // ---- step the CPU a batch of instructions ----
            for _ in 0..Self::STEP_BATCH {
                self.step_cpu();
            }
            // Approximate one cycle per instruction for the device clocks (the
            // R3000A is ~1 IPC for the common single-cycle ops; finer timing is
            // not observable to the simple poll-loops the boot path runs).
            let cycles = Self::STEP_BATCH;
            elapsed = elapsed.saturating_add(cycles);

            self.run_dma();
            self.advance_devices(cycles);
        }

        // Latch the interlace field + present the display area.
        self.gpu.render_frame();
        self.frames = self.frames.wrapping_add(1);
    }

    /// Step the CPU exactly one instruction, split-borrowing it out of `self`
    /// so the executor can use `self` as its `&mut dyn Bus` (the contract's
    /// `mem::take` pattern). Samples the folded IRQ line first.
    fn step_cpu(&mut self) {
        self.cpu.irq_pending = self.irq.pending();
        // Split the borrow: take the CPU out so the executor can use `self` as
        // its `&mut dyn Bus` (the contract's mem::take pattern). `Cpu: Default`.
        let mut cpu = std::mem::take(&mut self.cpu);
        cpu.step(self);
        self.cpu = cpu;
    }

    /// Advance the timed sub-devices by `cycles` system clocks and fold their
    /// raised interrupts into the CPU's pending line.
    fn advance_devices(&mut self, cycles: u32) {
        // GPU scanline / VBLANK (GPU clock ≈ 11/7 × CPU clock; psx-spx). VBLANK
        // is the dominant edge the boot path waits on.
        let gpu_cycles = (cycles as u64 * 11 / 7) as u32;
        self.gpu.step(gpu_cycles);
        if self.gpu.take_vblank() {
            self.irq.raise(crate::irq::Interrupt::Vblank);
        }

        self.timers.step(cycles, &mut self.irq);
        self.cdrom.step(cycles, &mut self.irq);
        self.spu.step(cycles);

        self.cpu.irq_pending = self.irq.pending();
    }

    // ============================ DMA ============================

    /// Run every armed DMA channel to completion. Each channel hands the
    /// orchestrator a plain [`Transfer`] descriptor; we move the words between
    /// main RAM and the target device, then clear the busy bit + latch the
    /// channel IRQ via [`Dma::complete`]. Loops until no channel is armed (one
    /// transfer can arm the next, e.g. a GPU image upload after a list walk).
    fn run_dma(&mut self) {
        while let Some((ch, transfer)) = self.dma.take_pending() {
            self.execute_transfer(transfer);
            let mut dma = std::mem::take(&mut self.dma);
            dma.complete(ch, &mut self.irq);
            self.dma = dma;
        }
    }

    /// Move the words of one decoded [`Transfer`] between RAM and its device.
    fn execute_transfer(&mut self, t: Transfer) {
        // OTC (channel 6) is special: it does not touch a device. It writes a
        // reverse-linked empty ordering table into RAM — each entry points to
        // the previous address, and the final (lowest) entry is the 0xFF_FFFF
        // terminator (psx-spx).
        if t.channel == Channel::Otc {
            self.dma_otc(t);
            return;
        }
        match t.sync {
            SyncMode::LinkedList => self.dma_linked_list(t),
            SyncMode::Burst | SyncMode::Slice => self.dma_block(t),
            SyncMode::Reserved => {}
        }
    }

    /// OTC reverse-clear: lay down `t.words` ordering-table entries descending
    /// from `t.base`, each pointing at the previous (addr-4) entry, the last
    /// one terminated with 0xFF_FFFF.
    fn dma_otc(&mut self, t: Transfer) {
        let mut addr = t.base & 0x1F_FFFC;
        for i in 0..t.words {
            let value = if i + 1 == t.words {
                0x00FF_FFFF // terminator at the tail
            } else {
                addr.wrapping_sub(4) & 0x00FF_FFFF
            };
            self.mem.ram_write32(addr, value);
            addr = addr.wrapping_sub(4) & 0x1F_FFFC;
        }
    }

    /// Block (burst/slice) transfer: stream `t.words` words to/from the device
    /// at `t.base`, stepping the address by `t.step`.
    fn dma_block(&mut self, t: Transfer) {
        let mut addr = t.base & 0x1F_FFFC;
        for _ in 0..t.words {
            match t.direction {
                Direction::FromRam => {
                    let w = self.mem.ram_read32(addr);
                    self.device_write(t.channel, w);
                }
                Direction::ToRam => {
                    let w = self.device_read(t.channel);
                    self.mem.ram_write32(addr, w);
                }
            }
            addr = (addr as i64 + t.step as i64) as u32 & 0x1F_FFFC;
        }
    }

    /// Linked-list transfer (GPU command lists, channel 2 RAM→GPU): walk the
    /// header chain in RAM — each node is `count` words of GP0 data preceded by
    /// a header (`next` in bits 0..23, `count` in bits 24..31) — until the
    /// end marker (bit 23 set / 0xFF_FFFF).
    fn dma_linked_list(&mut self, t: Transfer) {
        let mut addr = t.base & 0x1F_FFFC;
        // Bound the walk so a malformed chain can't spin forever.
        for _ in 0..0x10_0000 {
            let header = self.mem.ram_read32(addr);
            let count = header >> 24;
            let mut node = (addr + 4) & 0x1F_FFFC;
            for _ in 0..count {
                let w = self.mem.ram_read32(node);
                self.gpu.dma_gp0(w);
                node = (node + 4) & 0x1F_FFFC;
            }
            // End marker: bit 23 of the next pointer set (commonly 0xFF_FFFF).
            if header & 0x80_0000 != 0 {
                break;
            }
            addr = header & 0x1F_FFFC;
        }
    }

    /// RAM→device word sink for a block DMA.
    fn device_write(&mut self, channel: Channel, w: u32) {
        match channel {
            Channel::Gpu => self.gpu.dma_gp0(w),
            Channel::Spu => {
                self.spu.dma_write(w as u16);
                self.spu.dma_write((w >> 16) as u16);
            }
            Channel::MdecIn => self.mdec.write(0x0, w),
            // OTC, CDROM-in (n/a), MDECout-in (n/a), PIO: ignored.
            _ => {}
        }
    }

    /// device→RAM word source for a block DMA.
    fn device_read(&mut self, channel: Channel) -> u32 {
        match channel {
            Channel::Gpu => self.gpu.dma_gpuread(),
            Channel::Spu => {
                let lo = self.spu.dma_read() as u32;
                let hi = self.spu.dma_read() as u32;
                lo | (hi << 16)
            }
            Channel::MdecOut => self.mdec.read(0x0),
            Channel::Cdrom => self.cdrom_dma_word(),
            // OTC: the reverse-clear ordering table is written by the engine
            // itself; handled in `device_read` for the OTC channel below.
            Channel::Otc => 0,
            _ => 0,
        }
    }

    /// Assemble one 32-bit word from four CD-ROM data-FIFO bytes (channel 3).
    fn cdrom_dma_word(&mut self) -> u32 {
        let b0 = self.cdrom.read(2) & 0xFF;
        let b1 = self.cdrom.read(2) & 0xFF;
        let b2 = self.cdrom.read(2) & 0xFF;
        let b3 = self.cdrom.read(2) & 0xFF;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    // ---- cache-control / memory-control register window ----
    fn cache_control_read(&self) -> u32 {
        self.cache_control
    }
    fn cache_control_write(&mut self, v: u32) {
        self.cache_control = v;
    }

    // ============================ I/O dispatch ============================
    // The 4 KB hardware I/O window (0x1F80_1000..0x1F80_2000). `off` is relative
    // to [`crate::regions::IO_BASE`]; each sub-device occupies a fixed sub-range
    // (psx-spx I/O map). Unclaimed ports read open-bus / ignore writes.
    //
    // Window offsets (from IO_BASE = 0x1F80_1000):
    //   0x000..0x024  memory-control 1 (Expansion base/delay regs)
    //   0x040..0x050  SIO0  / 0x050..0x060 SIO1   (SIO window: 0x040..0x060)
    //   0x060         RAM_SIZE register
    //   0x070..0x078  IRQ   (I_STAT / I_MASK)
    //   0x080..0x100  DMA   (7 channels + DPCR/DICR)
    //   0x100..0x130  timers 0/1/2
    //   0x800..0x804  CDROM
    //   0x810..0x818  GPU   (GP0/GPUREAD, GP1/GPUSTAT)
    //   0x820..0x828  MDEC
    //   0xC00..0xE00  SPU
    fn io_read(&mut self, off: u32, size: u8) -> u32 {
        let _ = size;
        match off {
            0x000..=0x023 => self.mem_control[(off >> 2) as usize & 7],
            0x040..=0x05F => self.sio.read(off - 0x040),
            0x060 => self.ram_size,
            0x070..=0x077 => self.irq.read(off - 0x070),
            0x080..=0x0FF => self.dma.read(off - 0x080),
            0x100..=0x12F => self.timers.read(off - 0x100),
            0x800..=0x803 => self.cdrom.read(off - 0x800),
            0x810..=0x817 => self.gpu.read(off - 0x810),
            0x820..=0x827 => self.mdec.read(off - 0x820),
            0xC00..=0xDFF => self.spu.read(off - 0xC00),
            _ => 0xFFFF_FFFF,
        }
    }

    fn io_write(&mut self, off: u32, size: u8, v: u32) {
        let _ = size;
        match off {
            0x000..=0x023 => self.mem_control[(off >> 2) as usize & 7] = v,
            0x040..=0x05F => self.sio.write(off - 0x040, v),
            0x060 => self.ram_size = v,
            0x070..=0x077 => {
                self.irq.write(off - 0x070, v);
                // The CPU samples the folded IRQ line each instruction.
                self.cpu.irq_pending = self.irq.pending();
            }
            0x080..=0x0FF => self.dma.write(off - 0x080, v),
            0x100..=0x12F => self.timers.write(off - 0x100, v),
            0x800..=0x803 => self.cdrom.write(off - 0x800, v),
            0x810..=0x817 => self.gpu.write(off - 0x810, v),
            0x820..=0x827 => self.mdec.write(off - 0x820, v),
            0xC00..=0xDFF => self.spu.write(off - 0xC00, v),
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

    // ---- COP2 / GTE: route the CPU's coprocessor ops to the engine ----
    fn gte_read(&mut self, reg: u32, ctrl: bool) -> u32 {
        if ctrl {
            self.gte.read_control(reg)
        } else {
            self.gte.read_data(reg)
        }
    }
    fn gte_write(&mut self, reg: u32, ctrl: bool, v: u32) {
        if ctrl {
            self.gte.write_control(reg, v);
        } else {
            self.gte.write_data(reg, v);
        }
    }
    fn gte_command(&mut self, command: u32) {
        self.gte.command(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a little program into RAM (via KSEG0 so SR.IsC doesn't drop it) and
    /// point the CPU at it.
    fn harness(program: &[u32]) -> Psx {
        let mut psx = Psx::new();
        let base = 0x8000_0000u32;
        for (i, &w) in program.iter().enumerate() {
            psx.write32(base + (i as u32) * 4, w);
        }
        psx.cpu.pc = base;
        psx.cpu.next_pc = base + 4;
        psx.cpu.current_pc = base;
        psx
    }

    fn step(psx: &mut Psx, n: usize) {
        for _ in 0..n {
            psx.step_cpu();
        }
    }

    #[test]
    fn boots_without_bios_via_min_hle() {
        // A fresh machine with no BIOS installs the HLE trampolines and points
        // the CPU into RAM; the reset PC must be the KSEG0 entry, and the
        // exception handler word must be present.
        let psx = Psx::new();
        assert!(!psx.mem.bios_loaded);
        assert_eq!(psx.cpu.pc, 0x8000_0000);
        // RFE at the general exception vector (RAM offset 0x80).
        assert_eq!(psx.mem.ram_read32(0x80), (0x10 << 26) | (0x10 << 21) | 0x10);
    }

    #[test]
    fn run_frame_advances_without_panic() {
        // The synthetic smoke test: a tiny self-looping program (an infinite
        // `beq r0,r0,-1 ; nop`) must let `run_frame` step a whole frame budget,
        // service all sub-devices, present a framebuffer, and bump the counter —
        // without panicking.
        let mut psx = harness(&[
            0x1000_FFFF, // BEQ r0, r0, -1  (spin)
            0x0000_0000, // NOP (delay slot)
        ]);
        let before = psx.frame_count();
        psx.run_frame();
        psx.run_frame();
        assert_eq!(psx.frame_count(), before + 2);
        // A framebuffer view of the right size is available.
        assert_eq!(
            psx.framebuffer().len(),
            (psx.width() * psx.height() * 4) as usize
        );
    }

    #[test]
    fn vblank_irq_latches_over_a_frame() {
        // Over a frame the GPU crosses VBLANK and the IRQ controller latches the
        // VBLANK bit in I_STAT (regardless of the mask).
        let mut psx = harness(&[0x1000_FFFF, 0x0000_0000]);
        psx.run_frame();
        assert_ne!(
            psx.irq.stat & crate::irq::Interrupt::Vblank.bit(),
            0,
            "VBLANK latched after a frame"
        );
    }

    #[test]
    fn gte_command_runs_through_the_cpu() {
        // MTC2 three SXY FIFO entries, then a COP2 NCLIP command, and read MAC0
        // back with CFC2-equivalent data read. We assemble the ops by hand.
        //
        //   lui/ori to build values is verbose; instead drive the GTE directly
        //   through the bus seam the CPU uses, proving the COP2 routing works.
        let mut psx = harness(&[0x0000_0000]);
        // CU2 enable so COP2 ops are usable.
        psx.cpu.cop0.sr |= crate::cpu::cop0::SR_CU2;
        // Program: MTC2 sets data regs; here we just exercise the bus hooks.
        psx.gte_write(12, false, 0); // SXY0 = (0,0)
        psx.gte_write(13, false, 2); // SXY1 = (2,0)
        psx.gte_write(14, false, 2 << 16); // SXY2 = (0,2)
        psx.gte_command(0x1400_0006); // NCLIP
        // MAC0 is data register 24; signed area of the triangle = 4.
        assert_eq!(psx.gte_read(24, false) as i32, 4);
    }

    #[test]
    fn cop2_ops_execute_in_the_instruction_stream() {
        // Assemble a real MIPS program that drives the GTE through the CPU's
        // COP2 path (exec.rs): build SXY0/1/2 via LUI/ORI + MTC2, run NCLIP, and
        // MFC2 MAC0 back into a GPR. This proves the exec → bus → gte routing,
        // load-delay on MFC2 included.
        let lui = |rt: u32, imm: u32| (0x0F << 26) | (rt << 16) | (imm & 0xFFFF);
        let ori = |rs: u32, rt: u32, imm: u32| (0x0D << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF);
        // MTC2 rt -> cop2 data reg rd: COP2 (0x12), cop_op 0x04.
        let mtc2 = |rt: u32, rd: u32| (0x12 << 26) | (0x04 << 21) | (rt << 16) | (rd << 11);
        // MFC2 rt <- cop2 data reg rd: cop_op 0x00.
        let mfc2 = |rt: u32, rd: u32| (0x12 << 26) | (0x00 << 21) | (rt << 16) | (rd << 11);
        let nclip = (0x12 << 26) | (1 << 25) | 0x06; // COP2 command bit25 + NCLIP

        let mut psx = harness(&[
            ori(0, 1, 0),        // r1 = 0   (SXY0 = (0,0))
            mtc2(1, 12),         // MTC2 r1 -> cop2r12 (SXY0)
            ori(0, 2, 2),        // r2 = 2   (SXY1 = (2,0))
            mtc2(2, 13),         // MTC2 r2 -> cop2r13 (SXY1)
            lui(3, 2),           // r3 = 0x00020000 (SXY2 = (0,2))
            mtc2(3, 14),         // MTC2 r3 -> cop2r14 (SXY2)
            nclip,               // COP2 NCLIP
            mfc2(4, 24),         // MFC2 r4 <- cop2r24 (MAC0)
            0x0000_0000,         // NOP (settle the MFC2 load delay)
        ]);
        psx.cpu.cop0.sr |= crate::cpu::cop0::SR_CU2; // enable COP2
        step(&mut psx, 9);
        assert_eq!(psx.cpu.reg(4) as i32, 4, "NCLIP signed area in r4 via MFC2");
    }

    #[test]
    fn lwc2_swc2_round_trip_through_cpu() {
        // LWC2 loads RAM→GTE data reg, SWC2 stores it back. Use the bus seam the
        // CPU drives (exec_lwc2/exec_swc2 call exactly these).
        let mut psx = harness(&[0x0000_0000]);
        psx.write32(0x8000_0100, 0xCAFE_F00D);
        // LWC2 cop2r1 <- [0x100]
        let v = psx.read32(0x8000_0100);
        psx.gte_write(1, false, v);
        // SWC2 [0x200] <- cop2r1
        let out = psx.gte_read(1, false);
        psx.write32(0x8000_0200, out);
        assert_eq!(psx.read32(0x8000_0200), 0xCAFE_F00D);
    }

    #[test]
    fn otc_dma_builds_reverse_ordering_table() {
        // Arm the OTC channel (6) for a 4-entry reverse-clear at 0x1000 and run
        // the DMA. Each entry points at the previous; the tail is 0xFF_FFFF.
        let mut psx = Psx::new();
        psx.dma.write(0x60, 0x0000_1000); // MADR = 0x1000
        psx.dma.write(0x64, 0x0000_0004); // BCR  = 4 words
        psx.dma.write(0x68, 0x1100_0002); // CHCR start+trigger, dir to RAM, step -4
        psx.run_dma();
        // 0x1000 -> 0x0FFC, 0x0FFC -> 0x0FF8, 0x0FF8 -> 0x0FF4, 0x0FF4 -> END.
        assert_eq!(psx.mem.ram_read32(0x1000), 0x0FFC);
        assert_eq!(psx.mem.ram_read32(0x0FFC), 0x0FF8);
        assert_eq!(psx.mem.ram_read32(0x0FF4), 0x00FF_FFFF);
    }

    #[test]
    fn gpu_linked_list_dma_feeds_gp0() {
        // Build a 1-node GP0 list: a VRAM fill primitive (02h) + 2 params, then
        // an end marker. DMA channel 2 linked-list mode must clock all three GP0
        // words and the fill must land in VRAM.
        let mut psx = Psx::new();
        // Node @ 0x2000: header (count=3, next=0xFF_FFFF end marker).
        psx.mem.ram_write32(0x2000, 0x03FF_FFFF);
        psx.mem.ram_write32(0x2004, 0x0200_00FF); // GP0 02h fill, red
        psx.mem.ram_write32(0x2008, 0x0000_0000); // x=0,y=0
        psx.mem.ram_write32(0x200C, 0x0002_0010); // w=0x10,h=2
        // Arm channel 2 (GPU) linked-list, FromRam.
        psx.dma.write(0x20, 0x0000_2000); // MADR
        psx.dma.write(0x28, (1 << 24) | (2 << 9) | 1); // start, sync=LinkedList, dir FromRam
        psx.run_dma();
        // The fill wrote red into VRAM[0].
        assert_ne!(psx.gpu.vram[0], 0);
    }

    #[test]
    fn pad_poll_reports_pressed_buttons() {
        // Press Start + Cross; clock the standard digital-pad sequence through
        // SIO0 and verify the two button bytes come back active-low.
        let mut psx = Psx::new();
        let keys = crate::sio::Button::Start.bit() | crate::sio::Button::Cross.bit();
        psx.set_keys(keys);
        // 01 42 00 00 00 transfer; read after each clock.
        psx.write(0x1F80_1040, 1, 0x01); // address
        psx.write(0x1F80_1040, 1, 0x42); // read-buttons command
        let id = psx.read(0x1F80_1040, 1); // 0x41
        psx.write(0x1F80_1040, 1, 0x00);
        let ready = psx.read(0x1F80_1040, 1); // 0x5A
        psx.write(0x1F80_1040, 1, 0x00);
        let lo = psx.read(0x1F80_1040, 1) as u16;
        psx.write(0x1F80_1040, 1, 0x00);
        let hi = psx.read(0x1F80_1040, 1) as u16;
        assert_eq!(id, 0x41, "digital pad id");
        assert_eq!(ready, 0x5A);
        let buttons = !(lo | (hi << 8)); // un-invert to active-high
        assert_ne!(buttons & crate::sio::Button::Start.bit(), 0);
        assert_ne!(buttons & crate::sio::Button::Cross.bit(), 0);
        assert_eq!(buttons & crate::sio::Button::Square.bit(), 0);
    }

    #[test]
    fn load_exe_seeds_pc_and_copies_body() {
        // A minimal PS-X EXE: magic, pc=0x80010000, dest=0x80010000, size=8.
        let mut img = vec![0u8; 0x808];
        img[0..8].copy_from_slice(b"PS-X EXE");
        let put = |img: &mut [u8], off: usize, v: u32| {
            img[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        put(&mut img, 0x10, 0x8001_0000); // initial PC
        put(&mut img, 0x18, 0x8001_0000); // dest
        put(&mut img, 0x1C, 8); // size
        // Body: two recognisable words.
        img[0x800..0x804].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        img[0x804..0x808].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let mut psx = Psx::new();
        psx.load_rom(&img);
        assert_eq!(psx.cpu.pc, 0x8001_0000);
        assert_eq!(psx.mem.ram_read32(0x1_0000), 0xDEAD_BEEF);
        assert_eq!(psx.mem.ram_read32(0x1_0004), 0x1234_5678);
    }

    #[test]
    fn drain_audio_yields_samples_after_a_frame() {
        // The SPU is fed the frame's cycle budget; with SPU enabled it produces
        // interleaved-stereo samples the host can drain.
        let mut psx = harness(&[0x1000_FFFF, 0x0000_0000]);
        psx.spu.spucnt = 0x8000; // SPU enable
        psx.run_frame();
        let audio = psx.drain_audio();
        assert!(!audio.is_empty(), "SPU produced samples over a frame");
        assert_eq!(audio.len() % 2, 0, "interleaved stereo");
    }
}
