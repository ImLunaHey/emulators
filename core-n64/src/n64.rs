//! The N64 god-struct: the top-level orchestrator that owns the VR4300 CPU,
//! RDRAM, every RCP register block, and the cartridge, and implements the CPU's
//! [`Bus`] by routing a physical address (via [`regions`]) to the right device.
//!
//! Ownership model (mirrors the sibling cores): cross-subsystem calls use
//! `std::mem::take` — the CPU is taken out of `self`, stepped with `self` as the
//! `&mut dyn Bus`, then put back. The bus *is* the rest of the machine. No
//! `Rc`/`RefCell`.

use crate::boot;
use crate::bus::Bus;
use crate::cart;
use crate::cpu::cop0::Exception;
use crate::cpu::{exec, Cpu};
use crate::regions::{self, virt_to_phys};
use crate::vi::Vi;
use crate::{ai::Ai, crash, mi, pi, rdp::Rdp, rsp, si};

/// Instruction budget per emulated frame. The VR4300 runs at ~93.75 MHz; at
/// 60 fps that is ~1.56M cycles/frame. Our interpreter charges one "cycle" per
/// instruction (CPI is roughly 1.5 in practice), so we step a comparable
/// instruction count and service the vertical interrupt at the end.
const INSTRUCTIONS_PER_FRAME: u64 = 1_500_000;

/// How many faulting frames (exception storms) we tolerate before latching the
/// crash screen. A healthy game raises/handles exceptions routinely; a wedged
/// one raises the *same* exception every step with no forward progress.
const FAULT_FRAME_THRESHOLD: u32 = 4;

pub struct N64 {
    /// VR4300 CPU (taken out with `mem::take` while stepping).
    pub cpu: Cpu,

    /// 8 MB RDRAM (4 MB base + 4 MB Expansion Pak), big-endian bytes.
    pub rdram: Box<[u8]>,

    /// RCP + system subsystems.
    pub mi: mi::Mi,
    pub vi: Vi,
    pub ai: Ai,
    pub pi: pi::Pi,
    pub si: si::Si,
    pub rsp: rsp::Rsp,
    pub rdp: Rdp,

    /// Normalised (big-endian) cartridge image, mapped at `CART_ROM_BASE`.
    pub rom: Vec<u8>,
    /// True once a valid ROM has been loaded and HLE-booted.
    pub booted: bool,

    /// Presented framebuffer (RGBA8888), rebuilt each `run_frame`.
    fb: Vec<u8>,

    /// Frames presented so far.
    frames: u64,

    /// Latched fault (PC at the time) once the core wedges; once set we freeze
    /// and keep presenting the crash screen.
    fault: Option<u64>,
    /// Consecutive frames that made no forward PC progress / stormed exceptions.
    fault_frames: u32,
}

impl Default for N64 {
    fn default() -> Self {
        Self::new()
    }
}

impl N64 {
    pub fn new() -> Self {
        N64 {
            cpu: Cpu::new(),
            rdram: vec![0u8; regions::RDRAM_SIZE].into_boxed_slice(),
            mi: mi::Mi::new(),
            vi: Vi::new(),
            ai: Ai::new(),
            pi: pi::Pi::new(),
            si: si::Si::new(),
            rsp: rsp::Rsp::new(),
            rdp: Rdp::new(),
            rom: Vec::new(),
            booted: false,
            fb: Vec::new(),
            frames: 0,
            fault: None,
            fault_frames: 0,
        }
    }

    /// Load a cartridge image (any of .z64 / .n64 / .v64 byte order). Normalises
    /// to big-endian, copies the boot segment into RDRAM, and sets the CPU into
    /// the post-IPL3 state (HLE boot). A non-N64 image leaves the core unbooted.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        let Some(rom) = cart::normalize(bytes) else {
            return;
        };
        self.rom = rom;

        // HLE boot: configure the CPU and get the boot-segment copy to apply.
        let mut cpu = std::mem::take(&mut self.cpu);
        let copy = boot::setup(&mut cpu, &self.rom);
        self.cpu = cpu;

        if let Some(c) = copy {
            let dst = c.rdram_offset;
            let n = c.bytes.len().min(self.rdram.len().saturating_sub(dst));
            self.rdram[dst..dst + n].copy_from_slice(&c.bytes[..n]);
            self.booted = true;
        }

        // Reset run-time state for the freshly loaded game.
        self.frames = 0;
        self.fault = None;
        self.fault_frames = 0;
    }

    /// Step the CPU for a frame's worth of cycles, sample interrupts, service
    /// the vertical interrupt, then scan the framebuffer out.
    pub fn run_frame(&mut self) {
        if self.fault.is_some() {
            self.present_crash();
            self.frames = self.frames.wrapping_add(1);
            return;
        }
        if !self.booted {
            // Nothing loaded — present a clean black frame.
            self.scan_out();
            self.frames = self.frames.wrapping_add(1);
            return;
        }

        let start_pc = self.cpu.pc;
        let start_exceptions_seen = self.cpu.cop0.epc();

        let target = self.cpu.cycles.wrapping_add(INSTRUCTIONS_PER_FRAME);
        while self.cpu.cycles < target {
            self.step_one();
        }

        // End-of-frame vertical interrupt: VI reached its V_INTR scanline.
        self.mi.raise(mi::INTR_VI);
        self.update_irq_line();

        // Crude wedge detection: if the PC never moved and we made no exception
        // progress for several consecutive frames, the game is stuck.
        let no_progress = self.cpu.pc == start_pc && self.cpu.cop0.epc() == start_exceptions_seen;
        if no_progress {
            self.fault_frames = self.fault_frames.saturating_add(1);
            if self.fault_frames >= FAULT_FRAME_THRESHOLD {
                self.fault = Some(self.cpu.pc);
            }
        } else {
            self.fault_frames = 0;
        }

        if self.fault.is_some() {
            self.present_crash();
        } else {
            self.scan_out();
        }
        self.frames = self.frames.wrapping_add(1);
    }

    /// Execute one CPU instruction, having first folded a pending interrupt into
    /// an Interrupt exception (the interpreter does not sample interrupts).
    fn step_one(&mut self) {
        // Advance the Count/Compare timer (Count runs at ~half the CPU clock;
        // one tick per instruction is a coarse approximation).
        self.cpu.cop0.tick(1);

        // Reflect the RCP interrupt line into Cause.IP2 before checking.
        let line = self.mi.interrupt_line();
        self.cpu.cop0.set_rcp_interrupt(line);

        if self.cpu.cop0.interrupt_pending() {
            // current_pc is the address we'd resume at; raise() uses it for EPC.
            self.cpu.current_pc = self.cpu.pc;
            let mut cpu = std::mem::take(&mut self.cpu);
            cpu.raise(Exception::Interrupt, 0);
            self.cpu = cpu;
            return;
        }

        let mut cpu = std::mem::take(&mut self.cpu);
        exec::step(&mut cpu, self);
        self.cpu = cpu;
    }

    /// Recompute Cause.IP2 from MI's masked interrupt line.
    fn update_irq_line(&mut self) {
        let line = self.mi.interrupt_line();
        self.cpu.cop0.set_rcp_interrupt(line);
    }

    /// VI scanout into the presented framebuffer.
    fn scan_out(&mut self) {
        let mut fb = std::mem::take(&mut self.fb);
        self.vi.scanout(&self.rdram, &mut fb);
        self.fb = fb;
    }

    /// Draw the crash screen into the presented framebuffer at the VI's size.
    fn present_crash(&mut self) {
        let pc = self.fault.unwrap_or(self.cpu.pc);
        let w = self.width();
        let h = self.height();
        self.fb.clear();
        self.fb.resize(w * h * 4, 0);
        let lines = [
            "N64 CORE FAULT".to_string(),
            "CPU WEDGED".to_string(),
            format!("PC {:08X}", pc as u32),
            format!("FRAME {}", self.frames),
        ];
        crash::render(&mut self.fb, w, h, &lines);
    }

    /// Presented framebuffer (RGBA8888, width*height*4).
    pub fn framebuffer(&self) -> &[u8] {
        &self.fb
    }

    pub fn width(&self) -> usize {
        self.vi.width()
    }

    pub fn height(&self) -> usize {
        self.vi.height()
    }

    /// Route the host controller bitmask to controller port 0.
    pub fn set_keys(&mut self, bits: u32) {
        self.si.set_keys(bits);
    }

    /// Drain audio samples (mono f32). AI is a stub, so this is empty.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        Vec::new()
    }

    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    // ---- internal helpers shared by the Bus impl ----

    #[inline]
    fn rdram_read(&self, addr: u32, n: usize) -> u64 {
        let a = addr as usize;
        let mut v = 0u64;
        for i in 0..n {
            let byte = self.rdram.get(a + i).copied().unwrap_or(0);
            v = (v << 8) | byte as u64;
        }
        v
    }

    #[inline]
    fn rdram_write(&mut self, addr: u32, n: usize, value: u64) {
        let a = addr as usize;
        for i in 0..n {
            let shift = (n - 1 - i) * 8;
            if let Some(slot) = self.rdram.get_mut(a + i) {
                *slot = (value >> shift) as u8;
            }
        }
    }

    #[inline]
    fn rom_read(&self, off: u32, n: usize) -> u64 {
        let a = off as usize;
        let mut v = 0u64;
        for i in 0..n {
            let byte = self.rom.get(a + i).copied().unwrap_or(0);
            v = (v << 8) | byte as u64;
        }
        v
    }

    /// Read `n` bytes (1..=4) big-endian from the physical address `addr`.
    fn bus_read(&mut self, addr: u32, n: usize) -> u64 {
        match addr {
            a if a < regions::RDRAM_END => self.rdram_read(a, n),
            a if (regions::SP_DMEM_BASE..regions::SP_IMEM_END).contains(&a) => {
                // SP DMEM/IMEM: word-addressable; assemble from 32-bit reads.
                self.sp_mem_read(a, n)
            }
            a if (regions::SP_REGS_BASE..regions::SP_REGS_END).contains(&a) => {
                self.rsp.reg_read(a - regions::SP_REGS_BASE) as u64
            }
            a if (regions::DP_CMD_BASE..regions::DP_CMD_END).contains(&a) => {
                self.rdp.read(a - regions::DP_CMD_BASE) as u64
            }
            a if (regions::MI_BASE..regions::MI_END).contains(&a) => {
                self.mi.read(a - regions::MI_BASE) as u64
            }
            a if (regions::VI_BASE..regions::VI_END).contains(&a) => {
                self.vi.read(a - regions::VI_BASE) as u64
            }
            a if (regions::AI_BASE..regions::AI_END).contains(&a) => {
                self.ai.read(a - regions::AI_BASE) as u64
            }
            a if (regions::PI_BASE..regions::PI_END).contains(&a) => {
                self.pi.read(a - regions::PI_BASE) as u64
            }
            a if (regions::SI_BASE..regions::SI_END).contains(&a) => {
                self.si.read(a - regions::SI_BASE) as u64
            }
            a if (regions::CART_ROM_BASE..regions::CART_ROM_END).contains(&a) => {
                self.rom_read(a - regions::CART_ROM_BASE, n)
            }
            a if (regions::PIF_RAM_BASE..regions::PIF_RAM_END).contains(&a) => {
                self.pif_ram_read(a - regions::PIF_RAM_BASE, n)
            }
            _ => 0,
        }
    }

    /// Write `n` bytes (1..=4) big-endian to the physical address `addr`.
    fn bus_write(&mut self, addr: u32, n: usize, value: u64) {
        match addr {
            a if a < regions::RDRAM_END => self.rdram_write(a, n, value),
            a if (regions::SP_DMEM_BASE..regions::SP_IMEM_END).contains(&a) => {
                self.sp_mem_write(a, n, value)
            }
            a if (regions::SP_REGS_BASE..regions::SP_REGS_END).contains(&a) => {
                if let Some(req) = self.rsp.reg_write(a - regions::SP_REGS_BASE, value as u32) {
                    self.run_sp_dma(req);
                }
            }
            a if (regions::DP_CMD_BASE..regions::DP_CMD_END).contains(&a) => {
                self.rdp.write(a - regions::DP_CMD_BASE, value as u32);
            }
            a if (regions::MI_BASE..regions::MI_END).contains(&a) => {
                self.mi.write(a - regions::MI_BASE, value as u32);
                self.update_irq_line();
            }
            a if (regions::VI_BASE..regions::VI_END).contains(&a) => {
                let off = a - regions::VI_BASE;
                self.vi.write(off, value as u32);
                // Writing VI_V_CURRENT acks the VI interrupt.
                if off == crate::vi::VI_V_CURRENT {
                    self.mi.clear(mi::INTR_VI);
                    self.update_irq_line();
                }
            }
            a if (regions::AI_BASE..regions::AI_END).contains(&a) => {
                let off = a - regions::AI_BASE;
                self.ai.write(off, value as u32);
                if off == crate::ai::AI_STATUS {
                    self.mi.clear(mi::INTR_AI);
                    self.update_irq_line();
                }
            }
            a if (regions::PI_BASE..regions::PI_END).contains(&a) => {
                let off = a - regions::PI_BASE;
                if let Some(req) = self.pi.write(off, value as u32) {
                    self.run_pi_dma(req);
                }
                if off == crate::pi::PI_STATUS {
                    self.mi.clear(mi::INTR_PI);
                    self.update_irq_line();
                }
            }
            a if (regions::SI_BASE..regions::SI_END).contains(&a) => {
                let off = a - regions::SI_BASE;
                if let Some(dma) = self.si.write(off, value as u32) {
                    self.run_si_dma(dma);
                }
                if off == crate::si::SI_STATUS {
                    self.mi.clear(mi::INTR_SI);
                    self.update_irq_line();
                }
            }
            a if (regions::CART_ROM_BASE..regions::CART_ROM_END).contains(&a) => {
                // Cart ROM is read-only.
            }
            a if (regions::PIF_RAM_BASE..regions::PIF_RAM_END).contains(&a) => {
                self.pif_ram_write(a - regions::PIF_RAM_BASE, n, value);
            }
            _ => {}
        }
    }

    fn sp_mem_read(&self, addr: u32, n: usize) -> u64 {
        // Assemble from the RSP's big-endian 32-bit accessor.
        let word_addr = addr & !3;
        let word = self.rsp.mem_read32(word_addr);
        extract_be(word, addr & 3, n)
    }

    fn sp_mem_write(&mut self, addr: u32, n: usize, value: u64) {
        let word_addr = addr & !3;
        let old = self.rsp.mem_read32(word_addr);
        let merged = insert_be(old, addr & 3, n, value as u32);
        self.rsp.mem_write32(word_addr, merged);
    }

    fn pif_ram_read(&self, off: u32, n: usize) -> u64 {
        let a = off as usize;
        let mut v = 0u64;
        for i in 0..n {
            let byte = self.si.pif_ram.get(a + i).copied().unwrap_or(0);
            v = (v << 8) | byte as u64;
        }
        v
    }

    fn pif_ram_write(&mut self, off: u32, n: usize, value: u64) {
        let a = off as usize;
        for i in 0..n {
            let shift = (n - 1 - i) * 8;
            if let Some(slot) = self.si.pif_ram.get_mut(a + i) {
                *slot = (value >> shift) as u8;
            }
        }
    }

    /// Run a PI (cartridge) DMA: copy between the cart and RDRAM, then raise PI.
    fn run_pi_dma(&mut self, req: pi::DmaRequest) {
        let dram = (self.pi.dram_addr & 0x00FF_FFFF) as usize;
        // Cart address is a physical PI-bus address; the ROM domain starts at
        // CART_ROM_BASE.
        let cart = self
            .pi
            .cart_addr
            .wrapping_sub(regions::CART_ROM_BASE) as usize;
        let len = req.length as usize;
        if req.to_rdram {
            for i in 0..len {
                let byte = self.rom.get(cart + i).copied().unwrap_or(0);
                if let Some(slot) = self.rdram.get_mut(dram + i) {
                    *slot = byte;
                }
            }
        }
        // RDRAM->cart is a no-op (ROM is read-only).
        self.mi.raise(mi::INTR_PI);
        self.update_irq_line();
    }

    /// Run an SP DMA: copy between RDRAM and DMEM/IMEM.
    fn run_sp_dma(&mut self, req: rsp::DmaRequest) {
        let dram = (self.rsp.dram_addr & 0x00FF_FFFF) as usize;
        // mem_addr's bit 12 selects IMEM vs DMEM; the SP mem base is DMEM.
        let mem_base = if self.rsp.mem_addr & 0x1000 != 0 {
            regions::SP_IMEM_BASE
        } else {
            regions::SP_DMEM_BASE
        };
        let mem_off = self.rsp.mem_addr & 0xFFF;
        let len = req.length as usize;
        for i in 0..len {
            if req.to_rdram {
                // SP mem -> RDRAM
                let paddr = mem_base + ((mem_off + i as u32) & 0xFFF);
                let byte = self.sp_mem_read(paddr, 1) as u8;
                if let Some(slot) = self.rdram.get_mut(dram + i) {
                    *slot = byte;
                }
            } else {
                // RDRAM -> SP mem
                let byte = self.rdram.get(dram + i).copied().unwrap_or(0);
                let paddr = mem_base + ((mem_off + i as u32) & 0xFFF);
                self.sp_mem_write(paddr, 1, byte as u64);
            }
        }
    }

    /// Run an SI DMA between RDRAM and PIF RAM, executing the joybus protocol on
    /// the RDRAM->PIF direction and reading responses back on PIF->RDRAM.
    fn run_si_dma(&mut self, dma: si::SiDma) {
        let dram = (self.si.dram_addr & 0x00FF_FFFF) as usize;
        match dma {
            si::SiDma::ToPif => {
                // RDRAM -> PIF RAM (stage the joybus command block), then run it.
                for i in 0..64 {
                    let byte = self.rdram.get(dram + i).copied().unwrap_or(0);
                    self.si.pif_ram[i] = byte;
                }
                self.si.run_joybus();
            }
            si::SiDma::FromPif => {
                // PIF RAM -> RDRAM (read the responses back).
                for i in 0..64 {
                    let byte = self.si.pif_ram[i];
                    if let Some(slot) = self.rdram.get_mut(dram + i) {
                        *slot = byte;
                    }
                }
            }
        }
        self.mi.raise(mi::INTR_SI);
        self.update_irq_line();
    }
}

/// Extract `n` big-endian bytes at byte-lane `lane` (0..3) from a big-endian
/// 32-bit word. `lane` is the offset within the word of the first (MSB) byte.
#[inline]
fn extract_be(word: u32, lane: u32, n: usize) -> u64 {
    let bytes = word.to_be_bytes();
    let mut v = 0u64;
    for i in 0..n {
        let idx = (lane as usize + i).min(3);
        v = (v << 8) | bytes[idx] as u64;
    }
    v
}

/// Insert `n` big-endian bytes of `value` at byte-lane `lane` into a big-endian
/// 32-bit word, returning the merged word.
#[inline]
fn insert_be(word: u32, lane: u32, n: usize, value: u32) -> u32 {
    let mut bytes = word.to_be_bytes();
    for i in 0..n {
        let idx = (lane as usize + i).min(3);
        let shift = (n - 1 - i) * 8;
        bytes[idx] = (value >> shift) as u8;
    }
    u32::from_be_bytes(bytes)
}

// =============================================================================
// CPU memory bus: route the physical address to the owning device. The
// interpreter has already folded the virtual address to physical, but it passes
// us a physical address; we defensively re-fold in case a raw virtual address
// slips through.
// =============================================================================

impl Bus for N64 {
    fn read8(&mut self, addr: u32) -> u8 {
        self.bus_read(virt_to_phys(addr), 1) as u8
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.bus_read(virt_to_phys(addr), 2) as u16
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.bus_read(virt_to_phys(addr), 4) as u32
    }
    fn read64(&mut self, addr: u32) -> u64 {
        let a = virt_to_phys(addr);
        ((self.bus_read(a, 4) as u64) << 32) | self.bus_read(a.wrapping_add(4), 4)
    }
    fn write8(&mut self, addr: u32, v: u8) {
        self.bus_write(virt_to_phys(addr), 1, v as u64);
    }
    fn write16(&mut self, addr: u32, v: u16) {
        self.bus_write(virt_to_phys(addr), 2, v as u64);
    }
    fn write32(&mut self, addr: u32, v: u32) {
        self.bus_write(virt_to_phys(addr), 4, v as u64);
    }
    fn write64(&mut self, addr: u32, v: u64) {
        let a = virt_to_phys(addr);
        self.bus_write(a, 4, v >> 32);
        self.bus_write(a.wrapping_add(4), 4, v & 0xFFFF_FFFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid z64 ROM whose entry point loops in RDRAM.
    fn tiny_rom() -> Vec<u8> {
        // 1 MB + a bit so the boot segment copy has room.
        let mut rom = vec![0u8; 0x10_2000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // z64 magic
        // Entry point 0x8000_0400 (start of the boot segment in RDRAM).
        rom[0x08..0x0C].copy_from_slice(&0x8000_0400u32.to_be_bytes());
        // Boot segment (cart 0x1000 -> RDRAM 0x400): a tight infinite loop.
        // 0x1000: BEQ r0,r0,-1 (branch to self) ; 0x1004: NOP (delay slot)
        // BEQ r0,r0,offset: opcode 0x04, rs=0, rt=0, imm=0xFFFF (-1 word back).
        rom[0x1000..0x1004].copy_from_slice(&0x1000_FFFFu32.to_be_bytes());
        rom[0x1004..0x1008].copy_from_slice(&0x0000_0000u32.to_be_bytes()); // NOP
        rom
    }

    #[test]
    fn new_starts_at_reset_vector() {
        let n = N64::new();
        assert_eq!(n.frame_count(), 0);
        assert_eq!(n.rdram.len(), regions::RDRAM_SIZE);
    }

    #[test]
    fn load_rom_boots_and_copies_segment() {
        let mut n = N64::new();
        n.load_rom(&tiny_rom());
        assert!(n.booted);
        // The boot loop word is now in RDRAM at physical 0x400.
        assert_eq!(n.rdram_read(0x400, 4) as u32, 0x1000_FFFF);
        // CPU jumped to the entry point.
        assert_eq!(n.cpu.pc, 0xFFFF_FFFF_8000_0400);
    }

    #[test]
    fn run_frame_advances_frame_count() {
        let mut n = N64::new();
        n.load_rom(&tiny_rom());
        let before = n.frame_count();
        n.run_frame();
        assert_eq!(n.frame_count(), before + 1);
    }

    #[test]
    fn framebuffer_has_expected_length() {
        let mut n = N64::new();
        n.load_rom(&tiny_rom());
        n.run_frame();
        let w = n.width();
        let h = n.height();
        assert_eq!(n.framebuffer().len(), w * h * 4);
        // Default VI is 320x240 until the game configures it.
        assert_eq!((w, h), (320, 240));
    }

    #[test]
    fn unrecognised_rom_does_not_boot() {
        let mut n = N64::new();
        n.load_rom(&[0xAA, 0xBB, 0xCC, 0xDD, 0, 0, 0, 0]);
        assert!(!n.booted);
    }

    #[test]
    fn set_keys_routes_to_controller() {
        let mut n = N64::new();
        n.set_keys(crate::si::button::A | crate::si::button::START);
        assert_eq!(
            n.si.controllers[0].state,
            crate::si::button::A | crate::si::button::START
        );
    }

    #[test]
    fn bus_rdram_roundtrip_big_endian() {
        let mut n = N64::new();
        n.write32(0x8000_1000, 0xDEAD_BEEF);
        assert_eq!(n.read32(0x8000_1000), 0xDEAD_BEEF);
        assert_eq!(n.read8(0x8000_1000), 0xDE);
        assert_eq!(n.read8(0x8000_1003), 0xEF);
    }

    #[test]
    fn pi_dma_copies_cart_to_rdram() {
        let mut n = N64::new();
        n.load_rom(&tiny_rom());
        // Program a PI DMA: cart 0x1000_0010 -> RDRAM 0x2000, 16 bytes.
        n.write32(0xA460_0000, 0x2000); // PI_DRAM_ADDR
        n.write32(0xA460_0004, 0x1000_0010); // PI_CART_ADDR
        // Put a marker in the ROM where the cart addr points (offset 0x10).
        n.rom[0x10] = 0x99;
        n.write32(0xA460_000C, 15); // PI_WR_LEN (length-1)
        assert_eq!(n.rdram[0x2000], 0x99);
        // PI interrupt raised.
        assert_ne!(n.mi.intr & mi::INTR_PI, 0);
    }
}
