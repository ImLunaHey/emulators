//! The `Gba` god-struct: owns every subsystem + memory, implements [`Bus`],
//! and runs frames. Ported from src/emulator.ts (orchestration) and
//! src/io/io.ts (the IO-register dispatch, which lives here in the `Bus`
//! impl because it needs every device at once — the TS `Bus`↔`Io` cycle).
//!
//! Borrow strategy (see CONTRACT.md): everything reachable via the bus stays
//! owned by `Gba`. To call a subsystem method that itself needs `&mut dyn
//! Bus` (= `&mut Gba`), we `mem::take` only the device(s) that method
//! mutates and pass `self` as the bus — safe because a DMA never re-enters
//! its own registers, the CPU isn't reachable through the bus (HALTCNT is
//! deferred via `halt_requested`), and PPU/timer DMA triggers run *after*
//! those borrows release (PPU/timers report the trigger; we fire it here).

use crate::bios::BiosHle;
use crate::bus::{Bus, Mem};
use crate::cheats::{apply_cheats, Cheat};
use crate::cpu::Cpu;
use crate::dma::Dma;
use crate::eeprom::Eeprom;
use crate::flash::Flash128;
use crate::irq::Irq;
use crate::keypad::Keypad;
use crate::ppu::Ppu;
use crate::regions as R;
use crate::rtc::Rtc;
use crate::save_detect::{detect_save_type, SaveType};
use crate::sio::Sio;
use crate::sound::Sound;
use crate::sram::Sram32;
use crate::timers::Timers;
use crate::Save;

const CYCLES_PER_FRAME: u32 = 280896;
const CYC_PER_LINE: i64 = 1232;

/// Exceptions taken within a single frame above which we declare a fault loop.
/// A real frame takes a handful (IRQs, SWIs); a CPU re-faulting every few
/// instructions on undefined opcodes takes tens of thousands — a crash, not a
/// running game. Matches the PS1 core's threshold.
const FAULT_THRESHOLD: u64 = 10_000;

/// A detected fault loop (an undefined-instruction / abort exception storm),
/// captured for the crash screen.
#[derive(Debug, Clone)]
pub struct Fault {
    /// Decode PC of the faulting instruction at detection time.
    pub pc: u32,
    /// Short uppercase mnemonic of the exception kind (crash-font safe).
    pub kind: &'static str,
}

pub struct Gba {
    pub mem: Mem,
    pub cpu: Cpu,
    pub ppu: Ppu,
    pub dma: Dma,
    pub timers: Timers,
    pub irq: Irq,
    pub keypad: Keypad,
    pub sound: Sound,
    pub sio: Sio,
    pub bios: BiosHle,

    // Save backends; the active one is selected by `save_type` at access time
    // (we can't hold a reference to one of our own fields, so we dispatch).
    pub flash: Flash128,
    pub sram: Sram32,
    pub eeprom: Eeprom,
    pub save_type: SaveType,
    pub eeprom_mode: bool,
    pub rtc: Rtc,

    pub cheats: Vec<Cheat>,

    // Generic IO-register backing store (regs without side effects mirror here).
    // `pub` so the savestate module (separate file) can snapshot/restore it.
    pub io_raw: [u8; 0x400],
    postflg: u32,
    haltcnt: u32,
    waitcnt: u32,
    // HALTCNT write during a CPU step sets this; the CPU is `mem::take`-n out
    // of `self` while stepping, so we apply `halted` once the step returns.
    halt_requested: bool,

    // ---- debug write-watch (LinkPanel's IwramWatch) ----
    // When `Some((lo, hi))`, every bus write whose access range overlaps the
    // inclusive byte range [lo, hi] is appended to `watch_log` as
    // (pc, addr, size, val). The log is capped; oldest entries drop first.
    pub watch: Option<(u32, u32)>,
    pub watch_log: Vec<(u32, u32, u8, u32)>,

    /// Set once a fault loop is detected (an undefined-instruction / abort
    /// exception storm). While set, [`Gba::run_frame`] freezes the CPU and
    /// re-presents the crash screen every frame.
    pub fault: Option<Fault>,
}

impl Default for Gba {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn rtc_range(a: u32) -> bool {
    let a = a & 0xFFFF_FFFE;
    a == 0x0800_00C4 || a == 0x0800_00C6 || a == 0x0800_00C8
}

impl Gba {
    pub fn new() -> Self {
        Gba {
            mem: Mem::new(),
            cpu: Cpu::new(),
            ppu: Ppu::default(),
            dma: Dma::new(),
            timers: Timers::new(),
            irq: Irq::new(),
            keypad: Keypad::new(),
            sound: Sound::new(),
            sio: Sio::new(),
            bios: BiosHle::new(),
            flash: Flash128::new(),
            sram: Sram32::new(),
            eeprom: Eeprom::new(8192),
            save_type: SaveType::Flash128,
            eeprom_mode: false,
            rtc: Rtc::new(),
            cheats: Vec::new(),
            io_raw: [0; 0x400],
            postflg: 0,
            haltcnt: 0,
            waitcnt: 0,
            halt_requested: false,
            watch: None,
            watch_log: Vec::new(),
            fault: None,
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.mem.load_rom(bytes);
        // Pick the save backend from the ROM signature. Default 128 KB Flash.
        self.save_type = detect_save_type(bytes);
        self.eeprom_mode = matches!(self.save_type, SaveType::Eeprom512 | SaveType::Eeprom8k);
        self.cpu.reset(&mut self.mem);
        // Cartridge-bypass boot: enable VBlank/HBlank/VCount IRQ defaults.
        self.ppu.dispstat = 0x38;
        // Apply the BIOS affine-register defaults (PA=PD=0x100). Needs the bus.
        let mut bios = std::mem::replace(&mut self.bios, BiosHle::new());
        bios.reset_affine_defaults(self);
        self.bios = bios;
    }

    // ---- input / output accessors for the host ----
    pub fn framebuffer(&self) -> &[u8] {
        self.ppu.framebuffer()
    }
    pub fn keypad_mut(&mut self) -> &mut Keypad {
        &mut self.keypad
    }
    /// Set the raw pressed-button bitmask (bit layout per `keypad::Key`).
    pub fn set_keys(&mut self, bits: u32) {
        self.keypad.pressed = bits & 0x3FF;
    }
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.sound.drain_output()
    }
    pub fn frame_count(&self) -> u32 {
        self.ppu.frame_count
    }

    // ---- battery save (Flash/SRAM/EEPROM) — dispatches to the active chip ----
    /// Current contents of the cartridge save chip (for writing a `.sav`).
    pub fn save_ram(&self) -> &[u8] {
        match self.save_type {
            SaveType::Sram => self.sram.data(),
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.data(),
            _ => self.flash.data(),
        }
    }
    /// Load a previously-saved `.sav` into the active save chip.
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        match self.save_type {
            SaveType::Sram => self.sram.load_save(bytes),
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.load_save(bytes),
            _ => self.flash.load_save(bytes),
        }
    }
    /// True if the save chip was written since the last `clear_save_dirty`.
    pub fn save_dirty(&self) -> bool {
        match self.save_type {
            SaveType::Sram => self.sram.dirty,
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.dirty,
            _ => self.flash.dirty,
        }
    }
    pub fn clear_save_dirty(&mut self) {
        match self.save_type {
            SaveType::Sram => self.sram.dirty = false,
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.dirty = false,
            _ => self.flash.dirty = false,
        }
    }
    /// Erase the active save chip (fill 0xFF, the blank-flash state) and mark
    /// it dirty so the host persists the cleared image.
    pub fn reset_save(&mut self) {
        match self.save_type {
            SaveType::Sram => {
                self.sram.data.fill(0xFF);
                self.sram.dirty = true;
            }
            SaveType::Eeprom512 | SaveType::Eeprom8k => {
                self.eeprom.data.fill(0xFF);
                self.eeprom.dirty = true;
            }
            _ => {
                self.flash.data.fill(0xFF);
                self.flash.dirty = true;
            }
        }
    }
    /// The detected save type as the UI's display string.
    pub fn save_type_str(&self) -> &'static str {
        match self.save_type {
            SaveType::Flash128 => "flash128",
            SaveType::Flash64 => "flash64",
            SaveType::Sram => "sram",
            SaveType::Eeprom512 => "eeprom512",
            SaveType::Eeprom8k => "eeprom8k",
            SaveType::None => "none",
        }
    }

    // ---- the frame loop (ported from emulator.ts runFrame, interpreter-only) ----
    pub fn run_frame(&mut self) {
        // Once faulted, freeze: keep presenting the crash screen, run no code.
        if self.fault.is_some() {
            self.present_crash();
            return;
        }

        self.keypad.tick_turbo();
        let exc_before = self.cpu.exceptions;
        // Take the CPU out once for the whole frame so `self` can serve as its
        // bus inside the batch loop. Nothing reachable between batches (PPU /
        // timers / DMA / SIO / cheats) touches `self.cpu`, so a single
        // take/restore replaces the per-batch swap (~4400 struct copies/frame).
        let mut cpu = std::mem::take(&mut self.cpu);
        let mut executed: u32 = 0;
        while executed < CYCLES_PER_FRAME {
            let line_remaining = CYC_PER_LINE - self.ppu.cycles_accum as i64;
            let mut batch = if line_remaining < 64 { line_remaining } else { 64 };
            let rem = (CYCLES_PER_FRAME - executed) as i64;
            if batch > rem {
                batch = rem;
            }
            if batch <= 0 {
                batch = 1;
            }
            let batch = batch as u32;

            // --- CPU batch. `cpu` is the frame-scoped local; `self` is its bus.
            let mut i: u32 = 0;
            while i < batch {
                cpu.irq_line = self.irq.cached_pending;
                cpu.step(self);
                i += 1;
                if self.halt_requested {
                    cpu.state.halted = true;
                    self.halt_requested = false;
                }
                // Halted: the rest of the batch's cycles still elapse so the
                // PPU/timers can reach the IRQ that wakes us (matches TS).
                if cpu.state.halted {
                    i = batch;
                    break;
                }
            }

            // --- Advance the rest by the cycles we ran (`i`).
            let tick = Ppu::step(&mut self.ppu, i, &mut self.mem, &mut self.irq);
            if tick.hblank {
                self.dma_trigger_hblank();
            }
            if tick.vblank {
                self.dma_trigger_vblank();
            }

            let (refill_a, refill_b) = Timers::step(&mut self.timers, i, &mut self.irq, &mut self.sound);
            if refill_a {
                self.dma_trigger_sound_fifo(1);
            }
            if refill_b {
                self.dma_trigger_sound_fifo(2);
            }

            self.sound.step(i as i32);
            self.sio.step(i as i32, &mut self.irq);

            executed += i;
            if self.ppu.frame_done {
                self.ppu.frame_done = false;
                break;
            }
        }
        // Restore the frame-scoped CPU before any post-frame work that reads it.
        self.cpu = cpu;

        // BIOS IntrCheck flag: set bit 0 of *(u16*)0x03007FF8 each VBlank.
        self.mem.iwram[0x7FF8] |= 0x01;

        // Re-apply enabled cheats once per frame (after the game updated RAM).
        if !self.cheats.is_empty() {
            let cheats = std::mem::take(&mut self.cheats);
            apply_cheats(self, &cheats);
            self.cheats = cheats;
        }

        // Fault-loop detection: a storm of undefined-instruction / abort
        // exceptions this frame means the CPU is wedged re-faulting; capture the
        // cause and switch to the crash screen (presented now and every frame
        // hereafter).
        if self.cpu.exceptions.wrapping_sub(exc_before) > FAULT_THRESHOLD {
            self.fault = Some(Fault {
                pc: self.cpu.last_exc_pc,
                kind: self.cpu.last_exc_kind,
            });
            self.present_crash();
        }
    }

    /// Draw the crash screen into the PPU framebuffer from the latched
    /// [`Fault`]. Called on first detection and every frame thereafter so the
    /// panel stays presented (the host blits `framebuffer()` each frame).
    fn present_crash(&mut self) {
        let f = match &self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "GBA CORE FAULT".to_string(),
            f.kind.to_string(),
            format!("PC {:08X}", f.pc),
        ];
        crate::crash::render(&mut self.ppu.frame, 240, 160, &lines);
    }

    // ---- DMA triggers: take dma+irq out, run with `self` as the bus ----
    fn dma_trigger_hblank(&mut self) {
        let mut dma = std::mem::take(&mut self.dma);
        let mut irq = std::mem::take(&mut self.irq);
        dma.trigger_hblank(self, &mut irq);
        self.dma = dma;
        self.irq = irq;
    }
    fn dma_trigger_vblank(&mut self) {
        let mut dma = std::mem::take(&mut self.dma);
        let mut irq = std::mem::take(&mut self.irq);
        dma.trigger_vblank(self, &mut irq);
        self.dma = dma;
        self.irq = irq;
    }
    fn dma_trigger_sound_fifo(&mut self, channel: usize) {
        let mut dma = std::mem::take(&mut self.dma);
        let mut irq = std::mem::take(&mut self.irq);
        dma.trigger_sound_fifo(channel, self, &mut irq);
        self.dma = dma;
        self.irq = irq;
    }

    // ---- active save backend dispatch ----
    fn save_read(&mut self, addr: u32) -> u32 {
        match self.save_type {
            SaveType::Sram => self.sram.read(addr),
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.read(addr),
            _ => self.flash.read(addr),
        }
    }
    fn save_write(&mut self, addr: u32, v: u32) {
        match self.save_type {
            SaveType::Sram => self.sram.write(addr, v),
            SaveType::Eeprom512 | SaveType::Eeprom8k => self.eeprom.write(addr, v),
            _ => self.flash.write(addr, v),
        }
    }

    // ---- debug write-watch capture (LinkPanel IwramWatch) ----
    // Called from the `Bus` write paths below after the access address is
    // masked. If the watch is armed and the access range overlaps the watched
    // byte range, append (pc, addr, size, val). The PC is the CPU's last decode
    // PC (`crate::cpu::LAST_PC`, tracked in all builds). Capped at 4096 entries
    // (oldest dropped). The public arm/clear/dump API lives in `debug.rs`.
    #[inline]
    fn dbg_watch_capture(&mut self, addr: u32, size: u8, val: u32) {
        if let Some((lo, hi)) = self.watch {
            let a_hi = addr.wrapping_add(size as u32 - 1);
            if a_hi >= lo && addr <= hi {
                let pc = crate::cpu::LAST_PC.load(std::sync::atomic::Ordering::Relaxed);
                self.watch_log.push((pc, addr, size, val));
                if self.watch_log.len() > 4096 {
                    self.watch_log.remove(0);
                }
            }
        }
    }

    // ---- IO register backing-store helpers ----
    #[inline]
    fn raw16_read(&self, addr: u32) -> u32 {
        let o = (addr & 0x3FE) as usize;
        (self.io_raw[o] as u32) | ((self.io_raw[o + 1] as u32) << 8)
    }
    #[inline]
    fn raw16_write(&mut self, addr: u32, v: u32) {
        let o = (addr & 0x3FE) as usize;
        self.io_raw[o] = (v & 0xFF) as u8;
        self.io_raw[o + 1] = ((v >> 8) & 0xFF) as u8;
    }

    // ============================ IO dispatch (io.ts) ============================
    fn io_read16(&mut self, addr: u32) -> u32 {
        let addr = addr & 0x3FE;
        // Sound block: PSG + control 0x060-0x086 and wave RAM 0x090-0x09E.
        if (0x060..=0x086).contains(&addr) || (0x090..=0x09E).contains(&addr) {
            return self.sound.read_reg16(addr);
        }
        match addr {
            0x000 => self.ppu.dispcnt,
            0x004 => self.ppu.read_dispstat(),
            0x006 => self.ppu.vcount & 0xFF,

            0x100 | 0x104 | 0x108 | 0x10C => self.timers.read_counter(((addr - 0x100) >> 2) as usize),
            0x102 | 0x106 | 0x10A | 0x10E => self.timers.read_control(((addr - 0x102) >> 2) as usize),

            0x120 | 0x122 | 0x124 | 0x126 | 0x128 | 0x12A | 0x134 | 0x140 | 0x150 | 0x152
            | 0x154 | 0x156 | 0x158 => self.sio.read16(addr),

            0x130 => self.keypad.read16(),

            0x200 => self.irq.ie & 0x3FFF,
            0x202 => self.irq.iflag & 0x3FFF,
            0x204 => self.waitcnt,
            0x208 => self.irq.ime & 1,

            // DMA CNT_H reads return live channel control (poll for completion).
            0x0BA => self.dma.ch[0].control,
            0x0C6 => self.dma.ch[1].control,
            0x0D2 => self.dma.ch[2].control,
            0x0DE => self.dma.ch[3].control,

            _ => self.raw16_read(addr),
        }
    }

    fn io_read8(&mut self, addr: u32) -> u32 {
        let addr = addr & 0x3FF;
        let v16 = self.io_read16(addr & !1);
        if addr & 1 != 0 {
            (v16 >> 8) & 0xFF
        } else {
            v16 & 0xFF
        }
    }

    fn io_read32(&mut self, addr: u32) -> u32 {
        self.io_read16(addr) | (self.io_read16(addr + 2) << 16)
    }

    // Sound-block byte writes (FIFO ports, wave RAM, PSG RMW against the raw
    // write-latch). Returns true if it consumed the write.
    fn sound_write8(&mut self, addr: u32, v: u32) -> bool {
        if !(0x060..=0x0A7).contains(&addr) {
            return false;
        }
        if addr >= 0x0A0 {
            if addr < 0x0A4 {
                self.sound.push_a(v);
            } else {
                self.sound.push_b(v);
            }
            return true;
        }
        if addr >= 0x090 {
            self.sound.ch3.write_ram8(addr - 0x090, v);
            return true;
        }
        if (0x086..=0x08F).contains(&addr) {
            return false; // SOUNDBIAS / gaps → raw
        }
        let cur = self.sound.raw_read16(addr & !1);
        let nv = if addr & 1 != 0 {
            (cur & 0x00FF) | (v << 8)
        } else {
            (cur & 0xFF00) | v
        };
        self.sound.write_reg16(addr & !1, nv);
        true
    }

    fn io_write8(&mut self, addr: u32, v: u32) {
        let addr = addr & 0x3FF;
        let v = v & 0xFF;
        if addr == 0x300 {
            self.postflg = v;
            return;
        }
        if addr == 0x301 {
            self.haltcnt = v;
            // HALTCNT bit 7: halt/stop — both treated as halt (deferred).
            self.halt_requested = true;
            return;
        }
        if self.sound_write8(addr, v) {
            return;
        }
        let cur = self.io_read16(addr & !1);
        let nv = if addr & 1 != 0 {
            (cur & 0x00FF) | (v << 8)
        } else {
            (cur & 0xFF00) | v
        };
        self.io_write16(addr & !1, nv);
    }

    fn io_write32(&mut self, addr: u32, v: u32) {
        self.io_write16(addr, v & 0xFFFF);
        self.io_write16(addr + 2, (v >> 16) & 0xFFFF);
    }

    fn io_write16(&mut self, addr: u32, v: u32) {
        let addr = addr & 0x3FE;
        let v = v & 0xFFFF;

        // PPU register block 0x000-0x056.
        if addr <= 0x056 {
            self.ppu.write_reg(addr, v);
            self.raw16_write(addr, v);
            return;
        }

        // DMA block 0x0B0-0x0DE.
        if (0x0B0..=0x0DE).contains(&addr) {
            let ch = ((addr - 0x0B0) / 12) as usize;
            let off = (addr - 0x0B0) - (ch as u32) * 12;
            let mut dma = std::mem::take(&mut self.dma);
            let mut irq = std::mem::take(&mut self.irq);
            match off {
                0x0 => dma.write_src(ch, (dma.ch[ch].src & 0xFFFF_0000) | v),
                0x2 => dma.write_src(ch, (dma.ch[ch].src & 0x0000_FFFF) | (v << 16)),
                0x4 => dma.write_dst(ch, (dma.ch[ch].dst & 0xFFFF_0000) | v),
                0x6 => dma.write_dst(ch, (dma.ch[ch].dst & 0x0000_FFFF) | (v << 16)),
                0x8 => dma.write_count(ch, v),
                0xA => dma.write_control(ch, v, self, &mut irq),
                _ => {}
            }
            self.dma = dma;
            self.irq = irq;
            self.raw16_write(addr, v);
            return;
        }

        // Sound block 0x060-0x0AF.
        if (0x060..=0x084).contains(&addr) || (0x090..=0x09E).contains(&addr) {
            self.sound.write_reg16(addr, v);
            return;
        }
        if addr == 0x0A0 || addr == 0x0A2 {
            self.sound.push_a(v & 0xFF);
            self.sound.push_a((v >> 8) & 0xFF);
            return;
        }
        if addr == 0x0A4 || addr == 0x0A6 {
            self.sound.push_b(v & 0xFF);
            self.sound.push_b((v >> 8) & 0xFF);
            return;
        }

        // Timers 0x100-0x10E.
        if (0x100..=0x10E).contains(&addr) {
            let i = ((addr - 0x100) >> 2) as usize;
            let is_reload = (addr & 2) == 0;
            if is_reload {
                self.timers.write_reload(i, v);
            } else {
                self.timers.write_control(i, v);
            }
            self.raw16_write(addr, v);
            return;
        }

        // Serial / link cable.
        if (0x120..=0x12A).contains(&addr)
            || addr == 0x134
            || addr == 0x140
            || (0x150..=0x158).contains(&addr)
        {
            self.sio.write16(addr, v, &mut self.irq);
            self.raw16_write(addr, v);
            return;
        }

        // Interrupt + system.
        match addr {
            0x200 => {
                self.irq.set_ie(v);
                let ie = self.irq.ie;
                self.raw16_write(0x200, ie);
                return;
            }
            0x202 => {
                self.irq.ack_write16(v);
                let iflag = self.irq.iflag;
                self.raw16_write(0x202, iflag);
                return;
            }
            0x204 => {
                self.waitcnt = v;
                self.raw16_write(0x204, v);
                return;
            }
            0x208 => {
                self.irq.set_ime(v);
                let ime = self.irq.ime;
                self.raw16_write(0x208, ime);
                return;
            }
            _ => {}
        }
        self.raw16_write(addr, v);
    }
}

// ============================ Bus impl ============================
//
// Routes the IO region (0x4), SRAM/Flash (0xE/0xF), EEPROM (0xD in eeprom
// mode), and the cart-GPIO/RTC window before delegating to `Mem`. The RTC
// interposer (src/emulator.ts installRtcInterposer) wraps 8/16-bit accesses
// only — 32-bit GPIO reads fall through to ROM, matching the TS.
impl Bus for Gba {
    fn read8(&mut self, addr: u32) -> u32 {
        if rtc_range(addr) {
            return self.rtc.read(addr & 0xFF);
        }
        match (addr >> 24) & 0xF {
            R::REGION_IO => self.io_read8(addr & 0x3FF_FFFF),
            R::REGION_ROM_5 if self.eeprom_mode => self.save_read(addr) & 0xFF,
            R::REGION_SRAM | R::REGION_SRAM2 => self.save_read(addr & 0xFFFF) & 0xFF,
            _ => self.mem.read8(addr),
        }
    }

    fn read16(&mut self, addr: u32) -> u32 {
        let a = addr & !1;
        if rtc_range(a) {
            return self.rtc.read(a & 0xFF);
        }
        match (a >> 24) & 0xF {
            R::REGION_IO => self.io_read16(a & 0x3FF_FFFF),
            R::REGION_ROM_5 if self.eeprom_mode => self.save_read(a) & 1,
            R::REGION_SRAM | R::REGION_SRAM2 => {
                let b = self.save_read(a & 0xFFFF) & 0xFF;
                (b | (b << 8)) & 0xFFFF
            }
            _ => self.mem.read16(a),
        }
    }

    fn read32(&mut self, addr: u32) -> u32 {
        let a = addr & !3;
        match (a >> 24) & 0xF {
            R::REGION_IO => self.io_read32(a & 0x3FF_FFFF),
            R::REGION_ROM_5 if self.eeprom_mode => {
                let lo = self.save_read(a) & 1;
                let hi = self.save_read(a + 2) & 1;
                lo | (hi << 16)
            }
            R::REGION_SRAM | R::REGION_SRAM2 => {
                let b = self.save_read(a & 0xFFFF) & 0xFF;
                (b << 24) | (b << 16) | (b << 8) | b
            }
            _ => self.mem.read32(a),
        }
    }

    fn write8(&mut self, addr: u32, v: u32) {
        let v = v & 0xFF;
        self.dbg_watch_capture(addr, 1, v);
        if rtc_range(addr) {
            self.rtc.write(addr & 0xFF, v);
            return;
        }
        match (addr >> 24) & 0xF {
            R::REGION_IO => self.io_write8(addr & 0x3FF_FFFF, v),
            R::REGION_ROM_5 if self.eeprom_mode => self.save_write(addr, v),
            R::REGION_SRAM | R::REGION_SRAM2 => self.save_write(addr & 0xFFFF, v),
            _ => self.mem.write8(addr, v),
        }
    }

    fn write16(&mut self, addr: u32, v: u32) {
        let a = addr & !1;
        let v = v & 0xFFFF;
        self.dbg_watch_capture(a, 2, v);
        if rtc_range(a) {
            self.rtc.write(a & 0xFF, v);
            return;
        }
        match (a >> 24) & 0xF {
            R::REGION_IO => self.io_write16(a & 0x3FF_FFFF, v),
            R::REGION_ROM_5 if self.eeprom_mode => self.save_write(a, v),
            R::REGION_SRAM | R::REGION_SRAM2 => self.save_write(a & 0xFFFF, v & 0xFF),
            _ => self.mem.write16(a, v),
        }
    }

    fn write32(&mut self, addr: u32, v: u32) {
        let a = addr & !3;
        self.dbg_watch_capture(a, 4, v);
        match (a >> 24) & 0xF {
            R::REGION_IO => self.io_write32(a & 0x3FF_FFFF, v),
            R::REGION_ROM_5 if self.eeprom_mode => {
                self.save_write(a, v & 0xFFFF);
                self.save_write(a + 2, (v >> 16) & 0xFFFF);
            }
            R::REGION_SRAM | R::REGION_SRAM2 => {
                let rot = (v >> ((a & 3) << 3)) & 0xFF;
                self.save_write(a & 0xFFFF, rot);
            }
            _ => self.mem.write32(a, v),
        }
    }

    fn try_hle_swi(&mut self, cpu: &mut Cpu, comment: u32) -> bool {
        let mut bios = std::mem::replace(&mut self.bios, BiosHle::new());
        let handled = bios.handle_swi(comment, cpu, self);
        self.bios = bios;
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::mode;

    /// Install a returning UNDEFINED-instruction vector in the BIOS so the guest
    /// re-faults forever: at 0x04 (the UND vector) place `SUBS PC, LR, #4`, which
    /// returns to the faulting instruction (UND LR = faulting_addr + 4), so the
    /// undefined opcode is re-executed every time → an exception storm.
    fn install_returning_und_vector(g: &mut Gba) {
        let wr32 = |bios: &mut [u8], off: usize, v: u32| {
            bios[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        wr32(&mut g.mem.bios[..], 0x04, 0xE25E_F004); // SUBS PC, LR, #4
    }

    #[test]
    fn undefined_instruction_storm_raises_crash_screen() {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        install_returning_und_vector(&mut g);

        // Run an undefined ARM instruction at IWRAM in System mode (so the UND
        // exception's banked SPSR/return is clean). Encoding 0xE6000010 matches
        // the architectural undefined slot: cond=E, bits27-25=011, bit4=1.
        let und = 0xE600_0010u32;
        Bus::write32(&mut g, 0x0300_0000, und);
        g.cpu.state.cpsr = mode::SYS;
        g.cpu.state.r[15] = 0x0300_0000;
        g.cpu.state.r[13] = 0x0300_7F00;
        g.cpu.branched = false;

        assert!(g.fault.is_none(), "no fault before running");
        g.run_frame();

        // The storm tripped the detector and latched a fault.
        assert!(g.fault.is_some(), "undefined-instruction storm must fault");
        let f = g.fault.as_ref().unwrap();
        assert_eq!(f.kind, "UNDEF INSTR");
        assert_eq!(f.pc, 0x0300_0000, "faulting PC captured");

        // The crash screen was rendered: the framebuffer holds the dark-blue
        // panel (non-zero) plus white text pixels (not the default black/zero).
        let fb = g.framebuffer();
        assert_eq!(fb.len(), 240 * 160 * 4);
        let bg = fb[0..4] == [0x10, 0x10, 0x60, 0xFF];
        assert!(bg, "framebuffer cleared to crash background");
        let has_white = fb.chunks_exact(4).any(|p| p == [0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(has_white, "crash screen drew white text pixels");

        // Once faulted the CPU is frozen: another frame just re-presents.
        let exc = g.cpu.exceptions;
        g.run_frame();
        assert_eq!(g.cpu.exceptions, exc, "CPU frozen after fault");
    }
}
