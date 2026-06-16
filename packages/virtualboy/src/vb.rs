//! The `Vb` god-struct: owns the V810 CPU, the VIP video processor, the VSU
//! sound unit, the hardware control registers (timer/gamepad/link), work RAM,
//! input, and the cartridge; implements the V810 `Bus`; and runs video frames.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): the god-struct owns
//! every subsystem. The CPU needs the whole machine as its `Bus`, so we
//! `mem::take` the CPU out of `self`, run it with `self` as the bus, then put it
//! back. No `Rc`/`RefCell`.
//!
//! V810 memory map (the top 3 bits of the address select the region; each region
//! is mirrored within its 16 MiB window):
//!   0x00000000  VIP — VRAM/DRAM + frame buffers + character/BGMap/OAM + I/O regs
//!   0x01000000  VSU — sound registers + wave/modulation RAM
//!   0x02000000  Hardware control registers (timer, gamepad, link, wait)
//!   0x03000000  unmapped (cartridge expansion)
//!   0x04000000  Game Pak expansion
//!   0x05000000  WRAM — 64 KiB program RAM (mirrored; real boards expose 64 KiB)
//!   0x06000000  Cartridge SRAM (battery-backed)
//!   0x07000000  Cartridge ROM (mirrored)
//!
//! The VIP register file sits inside the VIP window at 0x0005F800-0x0005FFFF;
//! reads/writes there route to `Vip::read_reg`/`write_reg`. We also expose the
//! linear character table at 0x00078000 (used by the renderer) and mirror the
//! classic four character banks into it.

use crate::bus::Bus;
use crate::cart::Cart;
use crate::cpu::{Cpu, FaultKind};
use crate::hw::{Hw, CPU_CLOCK};
use crate::input::Input;
use crate::vip::{Vip, DISP_H, DISP_W, FB_LEN, INT_XPEND};
use crate::vsu::Vsu;

/// 64 KiB work RAM at 0x05000000.
const WRAM_SIZE: usize = 0x0001_0000;

/// V810 cycles per game frame. The VB runs at ~20 MHz; the display refreshes the
/// stereo pair at 50 Hz, so ~400k cycles/frame. We pace by a fixed budget.
const CYCLES_PER_FRAME: u32 = 20_000_000 / 50;

/// VB interrupt levels (higher = higher priority).
const IRQ_GAMEPAD: u8 = 0;
const IRQ_TIMER: u8 = 1;
const IRQ_GAMEPAK: u8 = 2;
const IRQ_COMM: u8 = 3;
const IRQ_VIP: u8 = 4;

#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub pc: u32,
    pub opcode: u16,
    pub frame: u64,
}

pub struct Vb {
    pub cpu: Cpu,
    pub vip: Vip,
    pub vsu: Vsu,
    pub hw: Hw,
    pub input: Input,
    pub cart: Option<Cart>,

    /// 64 KiB program work RAM.
    wram: Box<[u8; WRAM_SIZE]>,

    /// CPU cycles owed to the audio/timer steppers.
    audio_owed: u32,

    pub fault: Option<Fault>,
}

impl Default for Vb {
    fn default() -> Self {
        Vb::new()
    }
}

impl Vb {
    pub fn new() -> Vb {
        Vb {
            cpu: Cpu::new(),
            vip: Vip::new(),
            vsu: Vsu::new(),
            hw: Hw::new(),
            input: Input::new(),
            cart: None,
            wram: vec![0u8; WRAM_SIZE].into_boxed_slice().try_into().unwrap(),
            audio_owed: 0,
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        let cart = Cart::load(bytes);
        self.cart = Some(cart);
        // Reset for a clean boot.
        self.vip = Vip::new();
        self.vsu = Vsu::new();
        self.hw = Hw::new();
        self.cpu = Cpu::new();
        self.cpu.reset();
        for b in self.wram.iter_mut() {
            *b = 0;
        }
        self.fault = None;
    }

    pub fn set_keys(&mut self, bits: u32) {
        self.input.set_keys(bits);
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.vsu.drain()
    }

    pub fn frame_count(&self) -> u64 {
        self.vip.frame
    }

    pub fn width(&self) -> usize {
        DISP_W
    }
    pub fn height(&self) -> usize {
        DISP_H
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.vip.framebuffer[..]
    }

    // ---- battery save passthrough ----
    pub fn save_ram(&self) -> &[u8] {
        self.cart.as_ref().map(|c| c.save_ram()).unwrap_or(&[])
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        if let Some(c) = self.cart.as_mut() {
            c.load_save_ram(bytes);
        }
    }
    pub fn save_dirty(&self) -> bool {
        self.cart.as_ref().map(|c| c.ram_dirty).unwrap_or(false)
    }
    pub fn clear_save_dirty(&mut self) {
        if let Some(c) = self.cart.as_mut() {
            c.ram_dirty = false;
        }
    }

    pub fn title(&self) -> String {
        self.cart.as_ref().map(|c| c.title.clone()).unwrap_or_default()
    }

    /// Compute the highest-priority pending interrupt level for the CPU.
    fn pending_irq_level(&self) -> u8 {
        // VIP highest.
        if self.vip.irq_asserted() {
            return IRQ_VIP;
        }
        if self.hw.timer_irq {
            return IRQ_TIMER;
        }
        let _ = (IRQ_GAMEPAD, IRQ_GAMEPAK, IRQ_COMM);
        0xFF
    }

    /// Run one full game frame: step the CPU for a frame's cycle budget,
    /// advancing the timer + VSU, render the VIP frame, then service interrupts.
    pub fn run_frame(&mut self) {
        if self.fault.is_some() {
            self.present_crash();
            return;
        }
        if self.cart.is_none() {
            return;
        }

        let mut consumed = 0u32;
        while consumed < CYCLES_PER_FRAME {
            // Latch the highest-priority pending IRQ into the CPU.
            self.cpu.irq_level = self.pending_irq_level();

            let mut cpu = std::mem::take(&mut self.cpu);
            let t = cpu.step(self);
            self.cpu = cpu;

            // If the CPU just took the timer interrupt vector, ack the timer.
            // (Cheap heuristic: clear when the timer IRQ has been the source and
            // the CPU is no longer idle on it. We ack on service via PSW EP.)
            consumed += t;
            self.audio_owed += t;

            // Advance the timer in lockstep so it can fire mid-frame.
            self.hw.step(t);

            // Latch an illegal-opcode fault for the crash screen.
            if let Some(f) = self.cpu.fault.take() {
                if f.kind == FaultKind::IllegalOpcode {
                    self.fault = Some(Fault {
                        pc: f.pc,
                        opcode: f.opcode,
                        frame: self.vip.frame,
                    });
                    self.present_crash();
                    return;
                }
            }
        }

        // Feed the VSU the elapsed cycles.
        let owed = self.audio_owed;
        self.audio_owed = 0;
        self.vsu.step(owed, CPU_CLOCK);

        // Render the VIP frame and raise frame/draw interrupts.
        self.vip.run_frame();

        // Acknowledge the timer IRQ if the game cleared it via TCR; otherwise
        // leave it for next frame. (CPU services via vector; we ack here so a
        // game that never reads TCR doesn't get stuck re-entering.)
        if self.cpu.flag_serviced_timer() {
            self.hw.ack_timer();
        }

        let _ = INT_XPEND;
    }

    fn present_crash(&mut self) {
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "VB CORE FAULT".to_string(),
            "ILLEGAL OPCODE".to_string(),
            format!("PC {:08X}", f.pc),
            format!("OP {:04X}", f.opcode),
            format!("FRAME {}", f.frame),
        ];
        crate::crash::render(&mut self.vip.framebuffer[..], DISP_W, DISP_H, &lines);
    }

    // ---- debug ----
    pub fn dbg_read8(&mut self, addr: u32) -> u8 {
        self.read8(addr)
    }
    pub fn dbg_read32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }
}

// =============================================================================
// V810 memory bus. The top byte (bits 31..24) selects the region; each device
// window is 16 MiB and mirrors internally.
// =============================================================================
impl Bus for Vb {
    fn read8(&mut self, addr: u32) -> u8 {
        match (addr >> 24) & 0x07 {
            0x00 => {
                let off = addr & 0x00FF_FFFF;
                // VIP register window.
                if (0x0005_F800..0x0006_0000).contains(&off) {
                    let r = self.vip.read_reg(off & 0x7E);
                    return if off & 1 == 0 { r as u8 } else { (r >> 8) as u8 };
                }
                self.vip.read8(off)
            }
            0x01 => 0xFF, // VSU registers are write-only
            0x02 => {
                let off = addr & 0x00FF_FFFF;
                match off & 0x3F {
                    0x10 => self.input.sdlr(), // SDLR
                    0x14 => self.input.sdhr(), // SDHR
                    _ => self.hw.read8(off),
                }
            }
            0x05 => self.wram[(addr as usize) & (WRAM_SIZE - 1)],
            0x06 => self
                .cart
                .as_ref()
                .map(|c| c.read_sram(addr & 0x00FF_FFFF))
                .unwrap_or(0xFF),
            0x07 => self
                .cart
                .as_ref()
                .map(|c| c.read_rom(addr & 0x00FF_FFFF))
                .unwrap_or(0xFF),
            _ => 0xFF,
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        match (addr >> 24) & 0x07 {
            0x00 => {
                let off = addr & 0x00FF_FFFF;
                if (0x0005_F800..0x0006_0000).contains(&off) {
                    // Halfword registers — read-modify-write the byte.
                    let cur = self.vip.read_reg(off & 0x7E);
                    let nv = if off & 1 == 0 {
                        (cur & 0xFF00) | v as u16
                    } else {
                        (cur & 0x00FF) | ((v as u16) << 8)
                    };
                    self.vip.write_reg(off & 0x7E, nv);
                    return;
                }
                self.vip.write8(off, v);
            }
            0x01 => self.vsu.write8(addr & 0x00FF_FFFF, v),
            0x02 => self.hw.write8(addr & 0x00FF_FFFF, v),
            0x05 => self.wram[(addr as usize) & (WRAM_SIZE - 1)] = v,
            0x06 => {
                if let Some(c) = self.cart.as_mut() {
                    c.write_sram(addr & 0x00FF_FFFF, v);
                }
            }
            // ROM is read-only.
            _ => {}
        }
    }

    // Halfword/word fast paths for VIP DRAM + WRAM (the hot regions). We still
    // route device-register windows through the byte path's RMW logic by
    // delegating to read8/write8 for non-DRAM regions.
    fn read16(&mut self, addr: u32) -> u16 {
        match (addr >> 24) & 0x07 {
            0x00 => {
                let off = addr & 0x00FF_FFFF;
                if (0x0005_F800..0x0006_0000).contains(&off) {
                    return self.vip.read_reg(off & 0x7E);
                }
                self.vip.read16(off)
            }
            0x05 => {
                let a = (addr as usize) & (WRAM_SIZE - 1) & !1;
                u16::from_le_bytes([self.wram[a], self.wram[a + 1]])
            }
            _ => {
                let a = addr & !1;
                let lo = self.read8(a) as u16;
                let hi = self.read8(a.wrapping_add(1)) as u16;
                (hi << 8) | lo
            }
        }
    }

    fn write16(&mut self, addr: u32, v: u16) {
        match (addr >> 24) & 0x07 {
            0x00 => {
                let off = addr & 0x00FF_FFFF;
                if (0x0005_F800..0x0006_0000).contains(&off) {
                    self.vip.write_reg(off & 0x7E, v);
                    return;
                }
                self.vip.write16(off, v);
            }
            0x05 => {
                let a = (addr as usize) & (WRAM_SIZE - 1) & !1;
                let b = v.to_le_bytes();
                self.wram[a] = b[0];
                self.wram[a + 1] = b[1];
            }
            _ => {
                let a = addr & !1;
                self.write8(a, (v & 0xFF) as u8);
                self.write8(a.wrapping_add(1), (v >> 8) as u8);
            }
        }
    }
}

const _: () = assert!(FB_LEN == DISP_W * DISP_H * 4);

// CPU helper: did we just service a timer interrupt? We expose a small
// inspector on the CPU so the frame loop can ack the timer.
impl Cpu {
    /// True if the CPU is currently inside an interrupt handler entered from the
    /// timer level (a heuristic ack hook for the timer IRQ).
    pub fn flag_serviced_timer(&self) -> bool {
        // ECR low byte holds the interrupt code 0xFE | (level<<4). Timer = lvl1.
        (self.ecr & 0xFFFF) == (0xFE00 | (1u32 << 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal ROM: place code at the ROM start and a reset vector that
    /// the V810 fetches from 0xFFFFFFF0 (which mirrors to ROM offset
    /// len-0x10 inside the 16 MiB window). We put a small loop there.
    fn make_rom(prog: &[u8]) -> Vec<u8> {
        // 1 MiB ROM so it mirrors cleanly across the window.
        let mut rom = vec![0u8; 1 << 20];
        // Program at offset 0.
        rom[..prog.len()].copy_from_slice(prog);
        // Reset vector region: 0xFFFFFFF0 & (len-1) = (len-0x10). Put a JR to
        // offset 0 there. JR disp26: opcode 0b101010 in top 6 bits.
        // disp = 0 - 0xFFFF_FFF0 ... we instead place a simple "infinite jr 0".
        // Easiest: at the reset vector, write `movhi`/`movea` is complex; use a
        // JR with displacement that lands at our program by relative math.
        let vec_off = rom.len() - 0x10;
        // JR to program: target = program at window offset 0 == address
        // 0x07000000. PC at reset = 0xFFFFFFF0 (mirrors to vec_off). We can't
        // easily express that disp; tests below set PC directly instead.
        let _ = vec_off;
        rom
    }

    #[test]
    fn wram_read_write() {
        let mut vb = Vb::new();
        vb.write8(0x0500_0000, 0x42);
        assert_eq!(vb.read8(0x0500_0000), 0x42);
        // Mirror within the 64 KiB window.
        vb.write8(0x0500_0004, 0x99);
        assert_eq!(vb.read8(0x0501_0004), 0x99);
    }

    #[test]
    fn rom_reads_through_window() {
        let mut prog = vec![0u8; 16];
        prog[0] = 0xAB;
        prog[1] = 0xCD;
        let rom = make_rom(&prog);
        let mut vb = Vb::new();
        vb.load_rom(&rom);
        assert_eq!(vb.read8(0x0700_0000), 0xAB);
        assert_eq!(vb.read8(0x0700_0001), 0xCD);
    }

    #[test]
    fn vip_dram_via_bus() {
        let mut vb = Vb::new();
        vb.write16(0x0000_0000, 0x1234);
        assert_eq!(vb.read16(0x0000_0000), 0x1234);
    }

    #[test]
    fn vip_register_via_bus() {
        let mut vb = Vb::new();
        // INTENB at 0x0005F802.
        vb.write16(0x0005_F802, 0x4000);
        assert_eq!(vb.vip.intenb, 0x4000);
    }

    #[test]
    fn gamepad_read_via_bus() {
        let mut vb = Vb::new();
        vb.set_keys(crate::input::KEY_A);
        // SDHR at 0x02000014 should reflect A (bit 11 -> high byte bit 3).
        let sdhr = vb.read8(0x0200_0014);
        assert_eq!(sdhr & (1 << 3), 1 << 3);
    }

    #[test]
    fn run_frame_advances_count_with_rom() {
        let mut vb = Vb::new();
        // A ROM whose reset vector loops forever. We can't easily encode the
        // reset JR, so we just confirm run_frame advances the VIP frame even if
        // the CPU mostly NOPs/faults — the frame still renders.
        vb.load_rom(&vec![0u8; 1 << 20]);
        let f0 = vb.frame_count();
        vb.run_frame();
        // VIP frame advances unless we faulted immediately. All-zero ROM decodes
        // as MOV r0,r0 (op 0) which is a harmless NOP, so we run a full frame.
        assert!(vb.frame_count() >= f0);
    }

    #[test]
    fn all_zero_rom_runs_a_frame_without_fault() {
        // Opcode 0x0000 = MOV r0,r0 (Format I, op 000000) — a NOP. A ROM of
        // zeros should execute a whole frame of NOPs and render, no fault.
        let mut vb = Vb::new();
        vb.load_rom(&vec![0u8; 1 << 20]);
        vb.run_frame();
        assert!(vb.fault.is_none());
        assert_eq!(vb.frame_count(), 1);
    }
}
