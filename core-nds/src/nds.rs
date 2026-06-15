//! The `Nds` god-struct: sibling of the GBA core's `Gba`. Owns the DS memory
//! foundation — shared RAM, the ARM9 + ARM7 buses, the VRAM bank router, both
//! CPU register files, and the ARM9 CP15 — and exposes per-CPU `read*`/
//! `write*` bus entry points.
//!
//! This is the FOUNDATION phase: CPU instruction execution and every IO/PPU/
//! cart/BIOS subsystem are NOT ported yet. Methods that would need them call
//! `todo!("port from ds-recomp ...")`.
//!
//! Borrow strategy (mirrors `Gba`): everything reachable via a bus stays
//! owned by `Nds`. The bus accessors borrow the shared blocks + VRAM router
//! out of `self` and hand them to `Bus9`/`Bus7::resolve` (the TS stored those
//! as bus fields; we pass them in). IO routing — the TS `Bus`↔`Io` cycle —
//! lives here in `Nds` because it needs every device at once; for now those
//! paths are `todo!()` until the IO modules land.

use crate::bios::hle::BiosHle;
use crate::bios::nitro_os::NitroOsAssist;
use crate::cart::cart::{Cart, TransferEvent};
use crate::cpu::exec::Cpu;
use crate::cpu::{Cp15, CpuState};
use crate::io::dma::{Dma, DmaTiming};
use crate::io::ds_math::DsMath;
use crate::io::ipc::Ipc;
use crate::io::irq::Irq;
use crate::io::rtc::Rtc;
use crate::io::sound::Sound;
use crate::io::spi::Spi;
use crate::io::timers::Timers;
use crate::io::touch::TouchDriver;
use crate::memory::bus7::Resolved as Resolved7;
use crate::memory::bus9::Resolved as Resolved9;
use crate::memory::{Bus7, Bus9, SharedMemory, VramRouter};
use crate::ppu::gx::Gpu3d;
use crate::ppu::ppu::Ppu;

/// Which CPU a bus access is for. The two cores see different memory maps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Core {
    Arm9,
    Arm7,
}

/// A detected ARM9 fault loop (an unhandled-exception storm), captured for the
/// crash screen. Once set, [`Nds::run_frame`] freezes both CPUs and keeps the
/// crash panel presented.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    /// Exception cause of the storm (a `crate::cpu::exec::exc::*` code).
    pub code: u32,
    /// ARM9 PC at the most recent exception entry.
    pub pc: u32,
    /// Frame number when the fault was detected.
    pub frame: u32,
}

pub struct Nds {
    /// Single backing copy of every block both CPUs can touch.
    pub mem: SharedMemory,

    /// ARM9 bus state (TCMs + their CP15 config).
    pub bus9: Bus9,
    /// ARM7 bus state (touch-struct HLE flags).
    pub bus7: Bus7,

    /// VRAM bank router. Stateless w.r.t. VRAMCNT — those registers now live on
    /// the PPU (`ppu.vramcnt`), which the bus routing reads on every VRAM
    /// access.
    pub vram: VramRouter,

    /// 2D PPU coordinator. Owns VRAMCNT_A..I, both 2D engines (A/B), the
    /// scanline clock, DISPSTAT/VCOUNT, POWCNT1's graphics bits, and the two
    /// 256x192 RGBA8888 framebuffers. The bus VRAM router borrows
    /// `ppu.vramcnt` on every VRAM access.
    pub ppu: Ppu,

    /// 3D geometry engine (the "GX"). ARM9-only. Owns the four matrix stacks,
    /// the GXFIFO command interpreter, the vertex/polygon assembly, per-vertex
    /// lighting, the software rasterizer's two BGR555 framebuffers + drawn masks,
    /// and the 3D control register block (DISP3DCNT/CLEAR_*/FOG_*/EDGE_*). Engine
    /// A composites its output as BG0 when DISPCNT bit 3 (3D) is set.
    pub gpu3d: Gpu3d,

    /// ARM9 BIOS-facing register file (ARMv5TE). This is the CANONICAL register
    /// file the BIOS HLE, `bios_service_wait`, `nitro_os_tick`, and `hle_boot`
    /// read/write. While `run_frame` is actively stepping a core, the live
    /// register file lives on `cpu9.state`; the frame loop syncs it back into
    /// `state9` at the boundaries where BIOS code runs (and the SWI seam in
    /// `Cpu::step` swaps it in/out for the duration of each HLE call).
    pub state9: CpuState,
    /// ARM7 BIOS-facing register file (ARMv4T). Same role as `state9`.
    pub state7: CpuState,

    /// ARM9 executor (ARMv5TE decode + pipeline/IRQ tracking). Owns the live
    /// register file while `run_frame` is stepping; otherwise mirrors `state9`.
    pub cpu9: Cpu,
    /// ARM7 executor (ARMv4T).
    pub cpu7: Cpu,

    /// ARM9 CP15 system-control coprocessor (caches/MPU/TCM config).
    pub cp15: Cp15,

    // ─── IO devices ──────────────────────────────────────────────────────
    //
    // The DS has TWO of several blocks (one per core) and a handful shared.
    // Per-core: IRQ controller, DMA, timers. ARM9-only: the math accelerator.
    // ARM7-only: SPI bus, RTC, sound. Shared: IPC (one instance both cores
    // poke) and the touch driver (an HLE helper, ARM9-side memory writer).

    /// ARM9 interrupt controller (IE/IF/IME).
    pub irq9: Irq,
    /// ARM7 interrupt controller.
    pub irq7: Irq,

    /// ARM9 DMA (4 channels, 3-bit timing).
    pub dma9: Dma,
    /// ARM7 DMA (4 channels, 2-bit timing).
    pub dma7: Dma,

    /// ARM9 timers (4).
    pub timers9: Timers,
    /// ARM7 timers (4).
    pub timers7: Timers,

    /// ARM9 math accelerator (divider + sqrt). No ARM7 equivalent.
    pub ds_math: DsMath,

    /// ARM7 SPI bus (firmware / touchscreen / power-management).
    pub spi: Spi,
    /// ARM7 real-time clock.
    pub rtc: Rtc,
    /// ARM7 sound chip (16 channels).
    pub sound: Sound,
    /// Per-frame mixed audio (interleaved stereo f32 @ 44.1 kHz), host-drained.
    audio_buf: Vec<f32>,

    /// Inter-processor communication (IPCSYNC + the two FIFO queues). Single
    /// instance shared by both cores — the IO dispatch passes each accessing
    /// core's `is_arm9` perspective.
    pub ipc: Ipc,

    /// HLE touch driver — writes the cooked NitroSDK touch sample into main RAM
    /// each VBlank (skips the ARM7 PXI roundtrip we don't model).
    pub touch: TouchDriver,

    // ─── Cartridge + BIOS HLE ────────────────────────────────────────────

    /// Game cartridge: ROM image, the ROMCTRL/ROMCMD/ROMDATA command state
    /// machine, AUXSPI save chip, and KEY1/KEY2 phase tracking. `None` until a
    /// ROM is mounted via `load_rom` / `hle_boot`.
    pub cart: Option<Cart>,

    /// Per-core BIOS HLE state (IntrWait latches + sound tables). The CPU
    /// executor consults `bios_swi` first on a SWI; these hold the wait masks
    /// the frame loop services.
    pub bios9: BiosHle,
    pub bios7: BiosHle,

    /// NitroSDK OS-thread deadlock assist (once-per-frame heuristic wake).
    pub nitro_os: NitroOsAssist,

    // ─── Misc per-core IO latches the dispatch reads/writes directly ─────

    /// POSTFLG (0x04000300) bit 0 = boot completed. We HLE the handoff → 1.
    pub postflg9: u32,
    pub postflg7: u32,
    /// POWCNT1 (ARM9, 0x04000304). Typical post-BIOS default.
    pub powcnt1: u32,
    /// HALTCNT (0x04000301) latch.
    pub haltcnt7: u32,
    /// KEYINPUT (0x04000130) — bits LOW = pressed. Default all-up.
    pub keyinput: u32,
    /// EXTKEYIN (0x04000136) — X, Y, lid; low = active.
    pub ext_keyinput: u32,

    /// EXMEMCNT (0x04000204, ARM9) / EXMEMSTAT (ARM7 read mirror). Bit 11 =
    /// cart-slot owner: 0 → ARM9 owns the cart bus, 1 → ARM7 owns it. Both
    /// CPUs' cart IO routes through the same `Cart`, but the slot-owning core
    /// is the one whose DMA/IRQ the transfer drives, and the non-owner's cart
    /// register accesses read open-bus. Default 0 = ARM9 owns (boot default).
    pub exmemcnt: u32,

    /// Set once an ARM9 fault loop is detected (an unhandled-exception storm).
    /// While set, `run_frame` runs no CPU code and re-presents the crash screen.
    pub fault: Option<Fault>,
}

impl Default for Nds {
    fn default() -> Self {
        Self::new()
    }
}

impl Nds {
    pub fn new() -> Self {
        let mut nds = Nds {
            mem: SharedMemory::new(),
            bus9: Bus9::new(),
            bus7: Bus7::new(),
            vram: VramRouter::new(),
            ppu: Ppu::new(),
            gpu3d: Gpu3d::new(),
            state9: CpuState::new(),
            state7: CpuState::new(),
            cpu9: Cpu::new(Core::Arm9),
            cpu7: Cpu::new(Core::Arm7),
            cp15: Cp15::new(),

            irq9: Irq::new(),
            irq7: Irq::new(),
            dma9: Dma::new(true),
            dma7: Dma::new(false),
            timers9: Timers::new(),
            timers7: Timers::new(),
            ds_math: DsMath::new(),
            spi: Spi::new(),
            rtc: Rtc::new(),
            sound: Sound::new(),
            audio_buf: Vec::new(),
            ipc: Ipc::new(),
            touch: TouchDriver::new(),

            cart: None,
            bios9: BiosHle::new(Core::Arm9),
            bios7: BiosHle::new(Core::Arm7),
            nitro_os: NitroOsAssist::new(),

            postflg9: 1,
            postflg7: 1,
            powcnt1: 0x820F,
            haltcnt7: 0,
            keyinput: 0x03FF,
            ext_keyinput: 0x007F,
            exmemcnt: 0,
            fault: None,
        };
        // Seed the BIOS IRQ-handler-pointer literal from CP15's reset DTCM
        // placement (matches the TS Cp15 constructor calling it). `cp15`,
        // `bus9` and `mem` are disjoint fields, so the split borrow is fine.
        nds.cp15
            .update_irq_handler_ptr_literal(&nds.bus9, &mut nds.mem);
        nds
    }

    // ─── ARM9 bus accessors ──────────────────────────────────────────────
    //
    // Little-endian byte assembly (the DS is LE), matching the TS DataView
    // access. IO (region 0x4) routing is deferred to the IO module.

    pub fn read8_arm9(&mut self, addr: u32) -> u32 {
        match self.resolve9(addr, false) {
            Resolved9::Mem(arr, idx) => arr[idx] as u32,
            Resolved9::Io => self.io_read9(addr, 1),
            Resolved9::None => 0,
        }
    }
    pub fn read16_arm9(&mut self, addr: u32) -> u32 {
        match self.resolve9(addr, false) {
            Resolved9::Mem(arr, idx) => (arr[idx] as u32) | ((arr[idx + 1] as u32) << 8),
            Resolved9::Io => self.io_read9(addr, 2),
            Resolved9::None => 0,
        }
    }
    pub fn read32_arm9(&mut self, addr: u32) -> u32 {
        match self.resolve9(addr, false) {
            Resolved9::Mem(arr, idx) => {
                (arr[idx] as u32)
                    | ((arr[idx + 1] as u32) << 8)
                    | ((arr[idx + 2] as u32) << 16)
                    | ((arr[idx + 3] as u32) << 24)
            }
            Resolved9::Io => self.io_read9(addr, 4),
            Resolved9::None => 0,
        }
    }
    pub fn write8_arm9(&mut self, addr: u32, v: u32) {
        match self.resolve9(addr, true) {
            Resolved9::Mem(arr, idx) => arr[idx] = (v & 0xFF) as u8,
            Resolved9::Io => self.io_write9(addr, v, 1),
            Resolved9::None => {}
        }
    }
    pub fn write16_arm9(&mut self, addr: u32, v: u32) {
        match self.resolve9(addr, true) {
            Resolved9::Mem(arr, idx) => {
                arr[idx] = (v & 0xFF) as u8;
                arr[idx + 1] = ((v >> 8) & 0xFF) as u8;
            }
            Resolved9::Io => self.io_write9(addr, v, 2),
            Resolved9::None => {}
        }
    }
    pub fn write32_arm9(&mut self, addr: u32, v: u32) {
        match self.resolve9(addr, true) {
            Resolved9::Mem(arr, idx) => {
                arr[idx] = (v & 0xFF) as u8;
                arr[idx + 1] = ((v >> 8) & 0xFF) as u8;
                arr[idx + 2] = ((v >> 16) & 0xFF) as u8;
                arr[idx + 3] = ((v >> 24) & 0xFF) as u8;
            }
            Resolved9::Io => self.io_write9(addr, v, 4),
            Resolved9::None => {}
        }
    }

    /// Borrow the shared blocks + router out of `self` and resolve an ARM9
    /// address. Split-borrow: `bus9`, `mem`, `vram`, `vramcnt` are distinct
    /// fields so the borrow checker accepts the simultaneous `&mut`/`&`.
    #[inline]
    fn resolve9(&mut self, addr: u32, for_write: bool) -> Resolved9<'_> {
        self.bus9
            .resolve(addr, for_write, &mut self.mem, &self.vram, &self.ppu.vramcnt)
    }

    // ─── ARM7 bus accessors ──────────────────────────────────────────────

    pub fn read8_arm7(&mut self, addr: u32) -> u32 {
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => arr[idx] as u32,
            Resolved7::Io => self.io_read7(addr, 1),
            Resolved7::Wifi => self.wifi_read7(addr, 1),
            Resolved7::None => 0,
        }
    }
    pub fn read16_arm7(&mut self, addr: u32) -> u32 {
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => (arr[idx] as u32) | ((arr[idx + 1] as u32) << 8),
            Resolved7::Io => self.io_read7(addr, 2),
            Resolved7::Wifi => self.wifi_read7(addr, 2),
            Resolved7::None => 0,
        }
    }
    pub fn read32_arm7(&mut self, addr: u32) -> u32 {
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => {
                (arr[idx] as u32)
                    | ((arr[idx + 1] as u32) << 8)
                    | ((arr[idx + 2] as u32) << 16)
                    | ((arr[idx + 3] as u32) << 24)
            }
            Resolved7::Io => self.io_read7(addr, 4),
            Resolved7::Wifi => self.wifi_read7(addr, 4),
            Resolved7::None => 0,
        }
    }
    pub fn write8_arm7(&mut self, addr: u32, v: u32) {
        let v = self.bus7.munge_write8(addr, v);
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => arr[idx] = (v & 0xFF) as u8,
            Resolved7::Io => self.io_write7(addr, v, 1),
            Resolved7::Wifi => self.wifi_write7(addr, v, 1),
            Resolved7::None => {}
        }
    }
    pub fn write16_arm7(&mut self, addr: u32, v: u32) {
        let v = self.bus7.munge_write16(addr, v);
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => {
                arr[idx] = (v & 0xFF) as u8;
                arr[idx + 1] = ((v >> 8) & 0xFF) as u8;
            }
            Resolved7::Io => self.io_write7(addr, v, 2),
            Resolved7::Wifi => self.wifi_write7(addr, v, 2),
            Resolved7::None => {}
        }
    }
    pub fn write32_arm7(&mut self, addr: u32, v: u32) {
        let v = self.bus7.munge_write32(addr, v);
        match self.resolve7(addr) {
            Resolved7::Mem(arr, idx) => {
                arr[idx] = (v & 0xFF) as u8;
                arr[idx + 1] = ((v >> 8) & 0xFF) as u8;
                arr[idx + 2] = ((v >> 16) & 0xFF) as u8;
                arr[idx + 3] = ((v >> 24) & 0xFF) as u8;
            }
            Resolved7::Io => self.io_write7(addr, v, 4),
            Resolved7::Wifi => self.wifi_write7(addr, v, 4),
            Resolved7::None => {}
        }
    }

    #[inline]
    fn resolve7(&mut self, addr: u32) -> Resolved7<'_> {
        self.bus7
            .resolve(addr, &mut self.mem, &self.vram, &self.ppu.vramcnt)
    }

    // ─── CP15 access (ARM9 MCR/MRC) ──────────────────────────────────────

    /// ARM9 `MRC p15` — read a CP15 register.
    pub fn cp15_read(&self, opc1: u32, crn: u32, crm: u32, opc2: u32) -> u32 {
        self.cp15.read(opc1, crn, crm, opc2)
    }
    /// ARM9 `MCR p15` — write a CP15 register, applying TCM/control/WFI side
    /// effects to the ARM9 bus + CPU state.
    pub fn cp15_write(&mut self, opc1: u32, crn: u32, crm: u32, opc2: u32, value: u32) {
        // `cp15`, `bus9`, `mem`, `state9` are disjoint fields; the compiler
        // accepts borrowing each `&mut` at once (no `mem::take` needed).
        self.cp15.write(
            opc1,
            crn,
            crm,
            opc2,
            value,
            &mut self.bus9,
            &mut self.mem,
            &mut self.state9,
        );
    }

    // ─── IO register dispatch (the TS `Bus`↔`Io` cycle, resolved here) ───
    //
    // The two cores see DIFFERENT IO maps (ds-recomp src/io/io.ts gated on
    // `isArm9`): the math accelerator + GX ports are ARM9-only; the SPI bus,
    // RTC, and sound chip are ARM7-only; everything else (IRQ, IPC, DMA,
    // timers, keypad, cart) is shared with per-core register banks. These are
    // the seams the device wave fills — each `addr`-range arm calls a device
    // method on `self`. `size` is the access width in bytes (1/2/4); the
    // device methods are byte-granular and the dispatch composes wider access.
    //
    // The PPU register block (DISPCNT/BGxCNT/VRAMCNT/etc.) and the cart block
    // are NOT in scope for this IO-skeleton wave — those land with the PPU and
    // cart modules and slot into the match here.

    /// ARM9 IO read dispatch (0x04000000-0x04FFFFFF, region resolved by Bus9).
    pub fn read_io_arm9(&mut self, addr: u32, size: u8) -> u32 {
        match size {
            4 => self.io_read32(addr, true),
            2 => self.io_read16(addr, true),
            _ => self.io_read8(addr, true),
        }
    }
    /// ARM9 IO write dispatch.
    pub fn write_io_arm9(&mut self, addr: u32, size: u8, v: u32) {
        match size {
            4 => self.io_write32(addr, v, true),
            2 => self.io_write16(addr, v, true),
            _ => self.io_write8(addr, v, true),
        }
    }
    /// ARM7 IO read dispatch.
    pub fn read_io_arm7(&mut self, addr: u32, size: u8) -> u32 {
        match size {
            4 => self.io_read32(addr, false),
            2 => self.io_read16(addr, false),
            _ => self.io_read8(addr, false),
        }
    }
    /// ARM7 IO write dispatch.
    pub fn write_io_arm7(&mut self, addr: u32, size: u8, v: u32) {
        match size {
            4 => self.io_write32(addr, v, false),
            2 => self.io_write16(addr, v, false),
            _ => self.io_write8(addr, v, false),
        }
    }

    // ─── Width dispatch (ports from ds-recomp io.ts IoBus.read*/write*) ──
    //
    // PPU/cart/GX/3D register blocks are NOT routed yet — they land with the
    // PPU and cart modules (those branches in io.ts touch `this.ppu.*` /
    // `this.cart.*`, which `Nds` doesn't own this wave). The seam: unmatched
    // reads return 0, writes are swallowed, exactly like a real DS open-bus IO
    // port. The blocks we DO own — DMA, math, IPC, IRQ, timers, keypad,
    // POSTFLG/POWCNT/HALTCNT, VRAMCNT/WRAMCNT, SPI/RTC/sound — are wired.

    #[inline]
    fn is_dma_addr(addr: u32) -> bool {
        let lo = addr & 0xFF;
        (addr & 0x0FFF_FF00) == 0x0400_0000 && (0xB0..0xE0).contains(&lo)
    }
    #[inline]
    fn is_math_addr(addr: u32) -> bool {
        let m = addr & 0x0FFF_FFFF;
        (0x0400_0280..0x0400_02C0).contains(&m)
    }

    fn io_read32(&mut self, addr: u32, is_arm9: bool) -> u32 {
        let m = addr & 0x0FFF_FFFC;
        if Self::is_dma_addr(addr) {
            let dma = if is_arm9 { &self.dma9 } else { &self.dma7 };
            return dma.read32(addr);
        }
        if is_arm9 && Self::is_math_addr(addr) {
            return self.ds_math.read32(addr & 0x0FFF_FFFF);
        }
        // GXSTAT (0x04000600, ARM9) — geometry engine + FIFO status. Pokemon
        // D/P/Pt spin on the "FIFO less than half full" bit before each list.
        if is_arm9 && (addr & 0x0FFF_FFFF) == 0x0400_0600 {
            return self.gpu3d.read_stat();
        }
        // IPC RECV FIFO (atomic word pop) at 0x04100000.
        if m == 0x0410_0000 {
            return self
                .ipc
                .read_recv(is_arm9, &mut self.irq9, &mut self.irq7);
        }
        // ROMDATA FIFO at 0x04100010 — 32-bit atomic word pop. Gated on cart
        // ownership: only the EXMEMCNT-selected core sees the cart.
        if m == 0x0410_0010 {
            return self.cart_read_romdata(is_arm9);
        }
        // ROMCTRL at 0x040001A4 — live busy/word-ready bits.
        if m == 0x0400_01A4 {
            return self.cart_read_romctrl(is_arm9);
        }
        // Default: compose from two halfwords.
        let lo = self.io_read16(addr, is_arm9);
        let hi = self.io_read16(addr.wrapping_add(2), is_arm9);
        (hi << 16) | lo
    }

    fn io_read16(&mut self, addr: u32, is_arm9: bool) -> u32 {
        let m = addr & 0x0FFF_FFFF;
        if Self::is_dma_addr(addr) {
            let dma = if is_arm9 { &self.dma9 } else { &self.dma7 };
            return dma.read16(addr);
        }
        if is_arm9 && Self::is_math_addr(addr) {
            return self.ds_math.read16(addr & 0x0FFF_FFFF);
        }
        if m == 0x0400_0180 {
            return self.ipc.read_sync(is_arm9);
        }
        if m == 0x0400_0184 {
            return self.ipc.read_cnt(is_arm9);
        }
        // AUXSPICNT at 0x040001A0 (cart save-chip SPI control).
        if m == 0x0400_01A0 {
            return self.cart_read_auxspicnt(is_arm9);
        }
        (self.io_read8(addr, is_arm9) | (self.io_read8(addr.wrapping_add(1), is_arm9) << 8)) & 0xFFFF
    }

    fn io_read8(&mut self, addr: u32, is_arm9: bool) -> u32 {
        let addr = addr & 0x0FFF_FFFF;
        if is_arm9 && (0x0400_0280..0x0400_02C0).contains(&addr) {
            return self.ds_math.read8(addr);
        }
        // GX 3D control register block (ARM9-only): DISP3DCNT (0x60/0x61),
        // EDGE_COLOR_TABLE (0x330..0x33F), CLEAR_COLOR/CLEAR_DEPTH (0x350..0x357),
        // FOG_COLOR/OFFSET/TABLE (0x358..0x37F). GXSTAT (0x600..0x603) byte reads.
        if is_arm9 {
            if let Some(b) = self.gpu3d.read_reg8(addr) {
                return b;
            }
            if (0x0400_0600..0x0400_0604).contains(&addr) {
                return (self.gpu3d.read_stat() >> ((addr & 3) * 8)) & 0xFF;
            }
        }
        // ARM7 sound chip occupies 0x04000400..0x040005FF (ARM9 sees GX → 0).
        if !is_arm9 && (0x0400_0400..0x0400_0600).contains(&addr) {
            return self.sound.read_byte(addr | 0x0400_0000);
        }
        // 2D PPU register blocks (ARM9-only). DISPSTAT/VCOUNT are shared, but
        // the TS only routed them on the ARM9 IO map; ARM7 sees open bus here.
        if is_arm9 {
            if let Some(v) = self.ppu_read_reg8(addr) {
                return v;
            }
        }
        // VRAMCNT_A..G / WRAMCNT / VRAMCNT_H..I and the ARM7 STAT mirror.
        if !is_arm9 {
            if addr == 0x0400_0240 {
                return self.vram.read_vram_stat(&self.ppu.vramcnt);
            }
            if addr == 0x0400_0241 {
                return self.mem.wramcnt.bits() & 0xFF;
            }
            if (0x0400_0242..0x0400_024A).contains(&addr) {
                return 0;
            }
        } else {
            if (0x0400_0240..0x0400_0247).contains(&addr) {
                return self.ppu.vramcnt[(addr - 0x0400_0240) as usize] as u32;
            }
            if addr == 0x0400_0247 {
                return self.mem.wramcnt.bits() & 0xFF;
            }
            if addr == 0x0400_0248 {
                return self.ppu.vramcnt[7] as u32;
            }
            if addr == 0x0400_0249 {
                return self.ppu.vramcnt[8] as u32;
            }
        }
        // Cart registers reachable byte-wise: AUXSPIDATA (0x1A2), ROMCTRL byte
        // slices (0x1A4..A7), AUXSPICNT byte slices (0x1A0..A1), and the ROMCMD
        // command latch (0x1A8..AF). All gated on cart slot ownership.
        if (0x0400_01A0..0x0400_01B0).contains(&addr) {
            return self.cart_read_reg8(is_arm9, addr);
        }
        // Timers (per-core), 0x04000100..0x0400010F.
        if (0x0400_0100..0x0400_0110).contains(&addr) {
            let timers = if is_arm9 { &self.timers9 } else { &self.timers7 };
            return timers.read8(addr - 0x0400_0100);
        }
        let irq = if is_arm9 { &self.irq9 } else { &self.irq7 };
        match addr {
            0x0400_0130 => self.keyinput & 0xFF,
            0x0400_0131 => (self.keyinput >> 8) & 0xFF,
            0x0400_0136 => {
                // EXTKEYIN: bit 6 LOW = pen down. The touch state lives on the
                // SPI module's touch latches.
                let mut v = self.ext_keyinput & 0xFF;
                if self.spi.touch_x.is_some() && self.spi.touch_y.is_some() {
                    v &= !0x40;
                }
                v
            }
            0x0400_0137 => (self.ext_keyinput >> 8) & 0xFF,
            0x0400_0138 => self.rtc.read() & 0xFF,
            0x0400_0139 => (self.rtc.read() >> 8) & 0xFF,
            0x0400_0180 => self.ipc.read_sync(is_arm9) & 0xFF,
            0x0400_0181 => (self.ipc.read_sync(is_arm9) >> 8) & 0xFF,
            0x0400_0184 => self.ipc.read_cnt(is_arm9) & 0xFF,
            0x0400_0185 => (self.ipc.read_cnt(is_arm9) >> 8) & 0xFF,
            0x0400_0208 => {
                if irq.ime {
                    1
                } else {
                    0
                }
            }
            0x0400_0209..=0x0400_020B => 0,
            0x0400_0210 => irq.ie & 0xFF,
            0x0400_0211 => (irq.ie >> 8) & 0xFF,
            0x0400_0212 => (irq.ie >> 16) & 0xFF,
            0x0400_0213 => (irq.ie >> 24) & 0xFF,
            0x0400_0214 => irq.iflag & 0xFF,
            0x0400_0215 => (irq.iflag >> 8) & 0xFF,
            0x0400_0216 => (irq.iflag >> 16) & 0xFF,
            0x0400_0217 => (irq.iflag >> 24) & 0xFF,
            0x0400_0300 => {
                if is_arm9 {
                    self.postflg9 & 0xFF
                } else {
                    self.postflg7 & 0xFF
                }
            }
            // EXMEMCNT (ARM9) / EXMEMSTAT (ARM7). Both cores read the same
            // latch; only ARM9 may write it (handled in io_write8).
            0x0400_0204 => self.exmemcnt & 0xFF,
            0x0400_0205 => (self.exmemcnt >> 8) & 0xFF,
            0x0400_0304 => self.powcnt1 & 0xFF,
            0x0400_0305 => (self.powcnt1 >> 8) & 0xFF,
            0x0400_0306 => (self.powcnt1 >> 16) & 0xFF,
            0x0400_0307 => (self.powcnt1 >> 24) & 0xFF,
            // SPI bus (ARM7 only).
            0x0400_01C0 => {
                if is_arm9 {
                    0
                } else {
                    self.spi.read_cnt() & 0xFF
                }
            }
            0x0400_01C1 => {
                if is_arm9 {
                    0
                } else {
                    (self.spi.read_cnt() >> 8) & 0xFF
                }
            }
            0x0400_01C2 => {
                if is_arm9 {
                    0
                } else {
                    self.spi.read_data() & 0xFF
                }
            }
            _ => 0,
        }
    }

    fn io_write32(&mut self, addr: u32, v: u32, is_arm9: bool) {
        let m = addr & 0x0FFF_FFFC;
        if Self::is_dma_addr(addr) {
            let armed = if is_arm9 {
                self.dma9.write32(addr, v)
            } else {
                self.dma7.write32(addr, v)
            };
            if let Some(ch) = armed {
                self.run_dma_channel(ch, is_arm9);
            }
            return;
        }
        if is_arm9 && Self::is_math_addr(addr) {
            self.ds_math.write32(addr & 0x0FFF_FFFF, v);
            return;
        }
        // GX geometry engine (ARM9-only). GXFIFO at 0x04000400 (32-bit packed
        // command word), the direct command ports at 0x04000440..0x040005FF
        // (one parameter each, opcode encoded in the offset), and GXSTAT at
        // 0x04000600. The command interpreter drains synchronously and may raise
        // IRQ_GXFIFO, so it borrows the ARM9 IRQ controller.
        if is_arm9 {
            let g = addr & 0x0FFF_FFFF;
            if g == 0x0400_0400 {
                self.gpu3d.write_fifo(v, &mut self.irq9);
                return;
            }
            if (0x0400_0440..0x0400_0600).contains(&g) {
                self.gpu3d.write_direct(g & !0x3, v, &mut self.irq9);
                return;
            }
            if g == 0x0400_0600 {
                self.gpu3d.write_stat(v, &mut self.irq9);
                return;
            }
        }
        // IPC SEND FIFO at 0x04000188.
        if m == 0x0400_0188 {
            self.ipc
                .write_send(is_arm9, v, &mut self.irq9, &mut self.irq7);
            return;
        }
        // ROMCTRL at 0x040001A4 — bit 31 (block-start) kicks off a transfer.
        if m == 0x0400_01A4 {
            self.cart_write_romctrl(is_arm9, v);
            return;
        }
        self.io_write16(addr, v & 0xFFFF, is_arm9);
        self.io_write16(addr.wrapping_add(2), (v >> 16) & 0xFFFF, is_arm9);
    }

    fn io_write16(&mut self, addr: u32, v: u32, is_arm9: bool) {
        let m = addr & 0x0FFF_FFFF;
        if Self::is_dma_addr(addr) {
            let armed = if is_arm9 {
                self.dma9.write16(addr, v)
            } else {
                self.dma7.write16(addr, v)
            };
            if let Some(ch) = armed {
                self.run_dma_channel(ch, is_arm9);
            }
            return;
        }
        if is_arm9 && Self::is_math_addr(addr) {
            self.ds_math.write16(addr & 0x0FFF_FFFF, v);
            return;
        }
        if m == 0x0400_0180 {
            self.ipc
                .write_sync(is_arm9, v & 0xFFFF, &mut self.irq9, &mut self.irq7);
            return;
        }
        if m == 0x0400_0184 {
            self.ipc
                .write_cnt(is_arm9, v & 0xFFFF, &mut self.irq9, &mut self.irq7);
            return;
        }
        // AUXSPICNT at 0x040001A0 (cart save-chip SPI control).
        if m == 0x0400_01A0 {
            self.cart_write_auxspicnt(is_arm9, v & 0xFFFF);
            return;
        }
        self.io_write8(addr, v & 0xFF, is_arm9);
        self.io_write8(addr.wrapping_add(1), (v >> 8) & 0xFF, is_arm9);
    }

    fn io_write8(&mut self, addr: u32, v: u32, is_arm9: bool) {
        let addr = addr & 0x0FFF_FFFF;
        let v = v & 0xFF;
        if Self::is_dma_addr(addr | 0x0400_0000) {
            let armed = if is_arm9 {
                self.dma9.write8(addr, v)
            } else {
                self.dma7.write8(addr, v)
            };
            if let Some(ch) = armed {
                self.run_dma_channel(ch, is_arm9);
            }
            return;
        }
        if is_arm9 && (0x0400_0280..0x0400_02C0).contains(&addr) {
            self.ds_math.write8(addr, v);
            return;
        }
        // GX 3D control register block (ARM9-only). Routed BEFORE the 2D engine
        // block because DISP3DCNT (0x60/0x61) overlaps the engine-A register
        // address window (0x00..0x6F) — the GX register owns those bytes.
        if is_arm9 && self.gpu3d.write_reg8(addr, v) {
            return;
        }
        // GXSTAT high half (0x602/0x603) — games set the FIFO IRQ mode (bits
        // 30..31) with a halfword/byte store; compose into the full word.
        if is_arm9 && (0x0400_0602..0x0400_0604).contains(&addr) {
            let sh = (addr & 3) * 8;
            let cur = self.gpu3d.read_stat();
            self.gpu3d
                .write_stat((cur & !(0xFF << sh)) | (v << sh), &mut self.irq9);
            return;
        }
        // 2D PPU register blocks (ARM9-only). POWCNT1 (0x04000304) is handled
        // below in the shared match so its non-graphics bits stay on the Nds
        // latch (and are mirrored into the PPU there).
        if is_arm9 && self.ppu_write_reg8(addr, v) {
            return;
        }
        // ARM7 sound chip.
        if !is_arm9 && (0x0400_0400..0x0400_0600).contains(&addr) {
            self.sound.write_byte(addr | 0x0400_0000, v);
            return;
        }
        // VRAMCNT / WRAMCNT (ARM9 writes only; ARM7 writes are dropped).
        if (0x0400_0240..0x0400_024A).contains(&addr) {
            if !is_arm9 {
                return;
            }
            if addr < 0x0400_0247 {
                self.ppu.vramcnt[(addr - 0x0400_0240) as usize] = v as u8;
            } else if addr == 0x0400_0247 {
                self.mem.wramcnt = crate::memory::WramCnt::from_bits(v & 0x03);
            } else if addr == 0x0400_0248 {
                self.ppu.vramcnt[7] = v as u8;
            } else if addr == 0x0400_0249 {
                self.ppu.vramcnt[8] = v as u8;
            }
            return;
        }
        // Cart registers reachable byte-wise (see io_read8). AUXSPIDATA writes
        // exchange a save-chip byte; ROMCTRL/AUXSPICNT byte slices recompose
        // the word; ROMCMD bytes latch into the command buffer.
        if (0x0400_01A0..0x0400_01B0).contains(&addr) {
            self.cart_write_reg8(is_arm9, addr, v);
            return;
        }
        // Timers (per-core).
        if (0x0400_0100..0x0400_0110).contains(&addr) {
            if is_arm9 {
                self.timers9.write8(addr - 0x0400_0100, v);
            } else {
                self.timers7.write8(addr - 0x0400_0100, v);
            }
            return;
        }
        match addr {
            // IPCSYNC byte writes — assemble + delegate.
            0x0400_0180 => {
                let cur = self.ipc.read_sync(is_arm9);
                self.ipc.write_sync(
                    is_arm9,
                    (cur & 0xFF00) | v,
                    &mut self.irq9,
                    &mut self.irq7,
                );
            }
            0x0400_0181 => {
                let cur = self.ipc.read_sync(is_arm9);
                self.ipc.write_sync(
                    is_arm9,
                    (cur & 0x00FF) | (v << 8),
                    &mut self.irq9,
                    &mut self.irq7,
                );
            }
            0x0400_0184 => {
                let cur = self.ipc.read_cnt(is_arm9);
                self.ipc
                    .write_cnt(is_arm9, (cur & 0xFF00) | v, &mut self.irq9, &mut self.irq7);
            }
            0x0400_0185 => {
                let cur = self.ipc.read_cnt(is_arm9);
                self.ipc.write_cnt(
                    is_arm9,
                    (cur & 0x00FF) | (v << 8),
                    &mut self.irq9,
                    &mut self.irq7,
                );
            }
            0x0400_0208 => {
                let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
                irq.set_ime(v & 1);
            }
            0x0400_0210..=0x0400_0213 => {
                let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
                let shift = (addr & 3) * 8;
                irq.set_ie((irq.ie & !(0xFF << shift)) | (v << shift));
            }
            0x0400_0214..=0x0400_0217 => {
                let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
                let shift = (addr & 3) * 8;
                irq.ack_if(v << shift);
            }
            0x0400_0138 => {
                let cur = self.rtc.read();
                self.rtc.write((cur & 0xFF00) | v);
            }
            0x0400_0139 => {
                let cur = self.rtc.read();
                self.rtc.write((cur & 0x00FF) | (v << 8));
            }
            // EXMEMCNT (0x04000204..205). ARM9 owns the full register; ARM7 may
            // only write bits 0..6 (its own GBA-slot timing) and never bit 11
            // (cart owner) per GBATEK. We model the cart-owner bit as ARM9-only
            // and let ARM7 update its low byte.
            0x0400_0204 => {
                if is_arm9 {
                    self.exmemcnt = (self.exmemcnt & !0xFF) | v;
                } else {
                    // ARM7 may set its own low-byte timing bits but not bit 11.
                    self.exmemcnt = (self.exmemcnt & !0x7F) | (v & 0x7F);
                }
            }
            0x0400_0205 => {
                if is_arm9 {
                    self.exmemcnt = (self.exmemcnt & !0xFF00) | (v << 8);
                }
            }
            0x0400_0300 => {
                if is_arm9 {
                    self.postflg9 = v;
                } else {
                    self.postflg7 = v;
                }
            }
            0x0400_0301 => {
                // HALTCNT — bit 7 = HALT, bit 6 = sleep. Either halts the CPU.
                // We model the halt directly on the relevant CpuState (the TS
                // routed through bios.halt(); that side-channel resolves here).
                self.haltcnt7 = v;
                if (v & 0x80) != 0 || (v & 0x40) != 0 {
                    let st = if is_arm9 {
                        &mut self.state9
                    } else {
                        &mut self.state7
                    };
                    st.halted = true;
                }
            }
            // POWCNT1: keep the full register on the Nds latch (non-graphics
            // bits) AND mirror it into the PPU, whose bit 15 (display swap)
            // selects which engine drives the top vs bottom screen.
            0x0400_0304 => {
                self.powcnt1 = (self.powcnt1 & !0xFF) | v;
                self.ppu.write_powcnt1(self.powcnt1);
            }
            0x0400_0305 => {
                self.powcnt1 = (self.powcnt1 & !0xFF00) | (v << 8);
                self.ppu.write_powcnt1(self.powcnt1);
            }
            0x0400_0306 => {
                self.powcnt1 = (self.powcnt1 & !0xFF_0000) | (v << 16);
                self.ppu.write_powcnt1(self.powcnt1);
            }
            0x0400_0307 => {
                self.powcnt1 = (self.powcnt1 & !0xFF00_0000) | (v << 24);
                self.ppu.write_powcnt1(self.powcnt1);
            }
            // SPI bus (ARM7 only).
            0x0400_01C0 => {
                if !is_arm9 {
                    let c = self.spi.read_cnt();
                    self.spi.write_cnt((c & 0xFF00) | v);
                }
            }
            0x0400_01C1 => {
                if !is_arm9 {
                    let c = self.spi.read_cnt();
                    self.spi.write_cnt((c & 0x00FF) | (v << 8));
                }
            }
            0x0400_01C2 => {
                if !is_arm9 {
                    self.spi.write_data(v);
                }
            }
            _ => {}
        }
    }

    // Bus-accessor seams (named by bus9.rs/bus7.rs) forward to the dispatch.
    #[inline]
    fn io_read9(&mut self, addr: u32, size: u8) -> u32 {
        self.read_io_arm9(addr, size)
    }
    #[inline]
    fn io_write9(&mut self, addr: u32, v: u32, size: u8) {
        self.write_io_arm9(addr, size, v)
    }
    #[inline]
    fn io_read7(&mut self, addr: u32, size: u8) -> u32 {
        self.read_io_arm7(addr, size)
    }
    #[inline]
    fn io_write7(&mut self, addr: u32, v: u32, size: u8) {
        self.write_io_arm7(addr, size, v)
    }

    // ─── 2D PPU register routing (ARM9 IO map) ────────────────────────────
    //
    // The PPU owns DISPCNT/BGxCNT/scroll/affine/WIN/MOSAIC/BLD/master-bright/
    // DISPCAPCNT (engine A at 0x040000xx, engine B mirror at 0x040010xx) plus
    // the PPU-global DISPSTAT (0x04000004) and VCOUNT (0x04000006). These
    // helpers return `Some(byte)` / `true` when the address belongs to a PPU
    // block so the surrounding dispatch can early-out, mirroring io.ts.

    /// PPU register byte read. `addr` is masked to 0x0FFFFFFF. Returns `None`
    /// for non-PPU addresses (the caller falls through to other devices).
    fn ppu_read_reg8(&self, addr: u32) -> Option<u32> {
        match addr {
            // DISPSTAT (low/high) and VCOUNT — PPU-global, recomputed live.
            0x0400_0004 => Some(self.ppu.read_dispstat() & 0xFF),
            0x0400_0005 => Some((self.ppu.read_dispstat() >> 8) & 0xFF),
            0x0400_0006 => Some(self.ppu.read_vcount() & 0xFF),
            0x0400_0007 => Some((self.ppu.read_vcount() >> 8) & 0x01),
            // Engine A register block (DISPCNT 0..3, then 0x08..0x6F). 0x04..7
            // are the DISPSTAT/VCOUNT handled above.
            0x0400_0000..=0x0400_0003 | 0x0400_0008..=0x0400_006F => {
                Some(self.ppu.read_engine_reg8(false, addr - 0x0400_0000))
            }
            // Engine B mirror at 0x04001000..0x0400106F.
            0x0400_1000..=0x0400_106F => {
                Some(self.ppu.read_engine_reg8(true, addr - 0x0400_1000))
            }
            _ => None,
        }
    }

    /// PPU register byte write. Returns `true` when the address belonged to a
    /// PPU block (and was consumed). POWCNT1 is intentionally NOT handled here
    /// (the shared match keeps its non-graphics bits on the Nds latch).
    fn ppu_write_reg8(&mut self, addr: u32, v: u32) -> bool {
        match addr {
            0x0400_0004 => {
                let cur = self.ppu.read_dispstat();
                self.ppu.write_dispstat((cur & 0xFF00) | v);
                true
            }
            0x0400_0005 => {
                let cur = self.ppu.read_dispstat();
                self.ppu.write_dispstat((cur & 0x00FF) | (v << 8));
                true
            }
            // VCOUNT (0x04000006/7) is read-only on hardware — swallow writes.
            0x0400_0006 | 0x0400_0007 => true,
            0x0400_0000..=0x0400_0003 | 0x0400_0008..=0x0400_006F => {
                self.ppu.write_engine_reg8(false, addr - 0x0400_0000, v);
                true
            }
            0x0400_1000..=0x0400_106F => {
                self.ppu.write_engine_reg8(true, addr - 0x0400_1000, v);
                true
            }
            _ => false,
        }
    }

    // ─── Cart slot IO routing (ROMCTRL / ROMCMD / ROMDATA / AUXSPI) ──────
    //
    // Both ARM9 and ARM7 see the cart registers at the same addresses, but only
    // the EXMEMCNT-bit-11-selected core OWNS the slot at any moment: the
    // non-owner reads open bus (0 / 0xFF on ROMDATA) and its writes are dropped.
    // The owner's accesses drive the single `Cart` state machine; transfer
    // events route the cart-ready DMA + cart-end IRQ to the owning core. With no
    // ROM mounted (`cart` is `None`) every cart access is open-bus.

    /// Whether the given core currently owns the cart slot. EXMEMCNT bit 11:
    /// 0 → ARM9, 1 → ARM7 (TS gated cart-ready DMA + cart-end IRQ on this).
    #[inline]
    fn cart_owned_by(&self, is_arm9: bool) -> bool {
        let arm7_owns = (self.exmemcnt & (1 << 11)) != 0;
        is_arm9 != arm7_owns
    }

    /// Apply a `TransferEvent` returned by a cart access: Ready → cart-ready DMA
    /// on the owning core (ARM9 only has the timing), TransferEnd → IRQ_CART.
    fn cart_apply_event(&mut self, is_arm9: bool, ev: TransferEvent) {
        match ev {
            TransferEvent::None => {}
            TransferEvent::Ready => {
                if is_arm9 {
                    self.dma_trigger_card_ready();
                }
            }
            TransferEvent::TransferEnd => {
                let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
                irq.raise(crate::io::irq::IRQ_CART);
            }
        }
    }

    fn cart_read_romdata(&mut self, is_arm9: bool) -> u32 {
        if !self.cart_owned_by(is_arm9) {
            return 0xFFFF_FFFF;
        }
        let Some(cart) = self.cart.as_mut() else {
            return 0xFFFF_FFFF;
        };
        let (word, ev) = cart.read_romdata();
        self.cart_apply_event(is_arm9, ev);
        word
    }

    fn cart_read_romctrl(&mut self, is_arm9: bool) -> u32 {
        if !self.cart_owned_by(is_arm9) {
            return 0;
        }
        self.cart.as_ref().map_or(0, |c| c.read_romctrl())
    }

    fn cart_write_romctrl(&mut self, is_arm9: bool, v: u32) {
        if !self.cart_owned_by(is_arm9) {
            return;
        }
        let Some(cart) = self.cart.as_mut() else {
            return;
        };
        let ev = cart.write_romctrl(v);
        self.cart_apply_event(is_arm9, ev);
    }

    fn cart_read_auxspicnt(&mut self, is_arm9: bool) -> u32 {
        if !self.cart_owned_by(is_arm9) {
            return 0;
        }
        self.cart.as_ref().map_or(0, |c| c.read_auxspicnt())
    }

    fn cart_write_auxspicnt(&mut self, is_arm9: bool, v: u32) {
        if !self.cart_owned_by(is_arm9) {
            return;
        }
        if let Some(cart) = self.cart.as_mut() {
            cart.write_auxspicnt(v);
        }
    }

    /// Byte-granular cart register read for 0x040001A0..0x040001AF.
    fn cart_read_reg8(&mut self, is_arm9: bool, addr: u32) -> u32 {
        if !self.cart_owned_by(is_arm9) {
            return 0;
        }
        let Some(cart) = self.cart.as_mut() else {
            return 0;
        };
        match addr {
            // AUXSPICNT byte slices.
            0x0400_01A0 => cart.read_auxspicnt() & 0xFF,
            0x0400_01A1 => (cart.read_auxspicnt() >> 8) & 0xFF,
            // AUXSPIDATA.
            0x0400_01A2 => cart.read_auxspidata() & 0xFF,
            // ROMCTRL byte slices.
            0x0400_01A4..=0x0400_01A7 => (cart.read_romctrl() >> ((addr & 3) * 8)) & 0xFF,
            // ROMCMD command latch.
            0x0400_01A8..=0x0400_01AF => cart.read_cmd_byte(addr - 0x0400_01A8) as u32,
            _ => 0,
        }
    }

    /// Byte-granular cart register write for 0x040001A0..0x040001AF.
    fn cart_write_reg8(&mut self, is_arm9: bool, addr: u32, v: u32) {
        if !self.cart_owned_by(is_arm9) {
            return;
        }
        // ROMCTRL byte writes recompose the word then re-run write_romctrl so
        // the block-start side effect fires; pull the current value first.
        if (0x0400_01A4..=0x0400_01A7).contains(&addr) {
            let Some(cart) = self.cart.as_mut() else {
                return;
            };
            let shift = (addr & 3) * 8;
            let cur = cart.read_romctrl();
            let next = (cur & !(0xFF << shift)) | ((v & 0xFF) << shift);
            let ev = cart.write_romctrl(next);
            self.cart_apply_event(is_arm9, ev);
            return;
        }
        let Some(cart) = self.cart.as_mut() else {
            return;
        };
        match addr {
            0x0400_01A0 => {
                let cur = cart.read_auxspicnt();
                cart.write_auxspicnt((cur & 0xFF00) | (v & 0xFF));
            }
            0x0400_01A1 => {
                let cur = cart.read_auxspicnt();
                cart.write_auxspicnt((cur & 0x00FF) | ((v & 0xFF) << 8));
            }
            0x0400_01A2 => cart.write_auxspidata(v & 0xFF),
            0x0400_01A8..=0x0400_01AF => cart.write_cmd_byte(addr - 0x0400_01A8, (v & 0xFF) as u8),
            _ => {}
        }
    }

    // ─── WiFi MMIO (ARM7 0x04800000-0x04807FFF) — minimal no-op stub ─────
    //
    // We don't emulate the WiFi link layer (there's no peer). For this wave
    // reads return 0 and writes are swallowed, which keeps the bus seams
    // typed; a faithful probe-response stub (chip-ID 0x1440, power-state ready
    // bit, baseband shadow) lands with the WiFi device port later.
    fn wifi_read7(&mut self, _addr: u32, _size: u8) -> u32 {
        0
    }
    fn wifi_write7(&mut self, _addr: u32, _v: u32, _size: u8) {}

    // ─── DMA orchestration (owns the memory walk the `Dma` struct can't) ─
    //
    // `Dma` holds only channel state; the transfer touches `self`'s bus, so it
    // lives here. The device wave calls these from the dispatch (immediate arm)
    // and from the PPU timing triggers.

    /// Walk one ARM9 DMA channel's transfer through the ARM9 bus, then run the
    /// `Dma`'s post-transfer writeback + completion IRQ. The split borrow:
    /// channel src/dst/count are copied out of `self.dma9` before the loop, the
    /// loop uses `self.read*/write*_arm9`, then `self.dma9.finish_channel` and
    /// `self.irq9.raise` apply the tail.
    pub fn run_dma_channel9(&mut self, channel: usize) {
        self.run_dma_channel(channel, true);
    }
    /// Walk one ARM7 DMA channel through the ARM7 bus (same shape).
    pub fn run_dma_channel7(&mut self, channel: usize) {
        self.run_dma_channel(channel, false);
    }

    /// Walk one DMA channel's transfer through the relevant core's bus, then
    /// run the `Dma`'s post-transfer writeback + completion IRQ. Ported from
    /// ds-recomp dma.ts `runChannel`. The split-borrow: channel src/dst/count
    /// modes are copied out of the `Dma` struct before the loop; the loop uses
    /// the core's `read*/write*` bus accessors; then `finish_channel` applies
    /// the writeback and reports whether to raise the completion IRQ.
    fn run_dma_channel(&mut self, channel: usize, is_arm9: bool) {
        let (word_count, step, word32, src_mode, dst_mode, mut src, mut dst, irq_bit) = {
            let dma = if is_arm9 { &self.dma9 } else { &self.dma7 };
            let c = &dma.channels[channel];
            (
                c.word_count(is_arm9),
                c.step(),
                c.word32,
                c.src_mode,
                c.dst_mode,
                c.src,
                c.dst,
                Dma::channel_irq_bit(channel),
            )
        };

        for _ in 0..word_count {
            if word32 {
                let v = if is_arm9 {
                    self.read32_arm9(src & !3)
                } else {
                    self.read32_arm7(src & !3)
                };
                if is_arm9 {
                    self.write32_arm9(dst & !3, v);
                } else {
                    self.write32_arm7(dst & !3, v);
                }
            } else {
                let v = if is_arm9 {
                    self.read16_arm9(src & !1)
                } else {
                    self.read16_arm7(src & !1)
                };
                if is_arm9 {
                    self.write16_arm9(dst & !1, v);
                } else {
                    self.write16_arm7(dst & !1, v);
                }
            }
            // Step source: 0=incr, 1=decr, 2/3=fixed.
            match src_mode {
                0 => src = src.wrapping_add(step),
                1 => src = src.wrapping_sub(step),
                _ => {}
            }
            // Step dest: mode 3 ("incr+reload") walks forward like mode 0, then
            // snaps back AFTER the transfer (finish_channel handles the snap).
            match dst_mode {
                0 | 3 => dst = dst.wrapping_add(step),
                1 => dst = dst.wrapping_sub(step),
                _ => {}
            }
        }

        let dma = if is_arm9 { &mut self.dma9 } else { &mut self.dma7 };
        let raise = dma.finish_channel(channel, src, dst);
        if raise {
            if is_arm9 {
                self.irq9.raise(irq_bit);
            } else {
                self.irq7.raise(irq_bit);
            }
        }
    }

    /// PPU VBlank trigger: run every armed VBlank-timed channel on both cores
    /// (ARM9 also re-evaluates GXFIFO-timed channels — see dma.ts).
    pub fn dma_trigger_vblank(&mut self) {
        for ch in self.dma9.channels_for_timing(DmaTiming::VBlank).collect::<Vec<_>>() {
            self.run_dma_channel(ch, true);
        }
        // ARM9 re-evaluates GXFIFO-timed channels every frame (our GX drains
        // synchronously so the "FIFO below half-full" condition always holds).
        for ch in self.dma9.channels_for_timing(DmaTiming::GxFifo).collect::<Vec<_>>() {
            self.run_dma_channel(ch, true);
        }
        for ch in self.dma7.channels_for_timing(DmaTiming::VBlank).collect::<Vec<_>>() {
            self.run_dma_channel(ch, false);
        }
    }
    /// PPU HBlank trigger.
    pub fn dma_trigger_hblank(&mut self) {
        for ch in self.dma9.channels_for_timing(DmaTiming::HBlank).collect::<Vec<_>>() {
            self.run_dma_channel(ch, true);
        }
        for ch in self.dma7.channels_for_timing(DmaTiming::HBlank).collect::<Vec<_>>() {
            self.run_dma_channel(ch, false);
        }
    }
    /// GXFIFO DMA trigger (ARM9 only). Fires every channel armed with timing 7
    /// (geometry-command FIFO). On hardware this asserts when the GXFIFO drops
    /// below half-full; our GX command interpreter drains synchronously, so the
    /// condition holds whenever the geometry engine has consumed a batch. The GX
    /// IO write path calls this after handing a command word to the engine so a
    /// game's GXFIFO-timed DMA keeps refilling the geometry queue. (The VBlank
    /// trigger also re-evaluates GxFifo channels as a backstop.)
    pub fn dma_trigger_gxfifo(&mut self) {
        for ch in self
            .dma9
            .channels_for_timing(DmaTiming::GxFifo)
            .collect::<Vec<_>>()
        {
            self.run_dma_channel(ch, true);
        }
    }
    /// Cart "card ready" trigger (ARM9 only).
    pub fn dma_trigger_card_ready(&mut self) {
        for ch in self.dma9.channels_for_timing(DmaTiming::CardReady).collect::<Vec<_>>() {
            self.run_dma_channel(ch, true);
        }
    }

    // ─── Touch driver (ARM9-side memory writer) ──────────────────────────

    /// Once-per-VBlank cooked-touch write. Reads the pointer latches off `spi`,
    /// cooks them via `self.touch.cook(...)`, then writes the struct into main
    /// RAM via the ARM9 bus and updates `Bus7`'s touch HLE flags.
    pub fn touch_tick_vblank(&mut self) {
        // OSTouchPanelStatus struct in main RAM (NitroSDK shared work area).
        const TOUCH_STRUCT_BASE: u32 = 0x027F_FFA8;
        const TOUCH_PRESSED_OFFSET: u32 = 0x00;
        const TOUCH_X_OFFSET: u32 = 0x02;
        const TOUCH_Y_OFFSET: u32 = 0x04;
        const TOUCH_FRAME_OFFSET: u32 = 0x06;

        let Some(cooked) =
            self.touch
                .cook(self.spi.touch_x, self.spi.touch_y, self.spi.touch_z)
        else {
            return;
        };
        let pressed = if cooked.pressed { 1 } else { 0 };
        let sx = cooked.screen_x;
        let sy = cooked.screen_y;

        self.write8_arm9(TOUCH_STRUCT_BASE + TOUCH_PRESSED_OFFSET, pressed);
        self.write16_arm9(TOUCH_STRUCT_BASE + TOUCH_X_OFFSET, sx);
        self.write16_arm9(TOUCH_STRUCT_BASE + TOUCH_Y_OFFSET, sy);
        self.write8_arm9(TOUCH_STRUCT_BASE + TOUCH_FRAME_OFFSET, cooked.update_frame);

        // Hand the cooked state to Bus7 so ARM7's mid-frame touch-struct
        // rewrites see the same pressed/coords (see bus7 munge logic).
        self.bus7.touch_pressed = cooked.pressed;
        self.bus7.touch_screen_x = sx;
    }

    // ─── Frame loop (ports ds-recomp src/emulator.ts `runFrame`) ─────────
    //
    // Timing model (per the TS): the DS dot clock is the master. Each dot the
    // ARM9 takes 2 instruction steps and the ARM7 takes 1 (a 2:1 ratio), the
    // PPU advances one dot, and the timer/sound blocks advance 6 bus cycles
    // (BUS_CYCLES_PER_DOT). We batch a whole scanline at a time (DOTS_PER_LINE)
    // so the PPU's `step` does its line-boundary bookkeeping in one call and
    // returns a `PpuTick`; the orchestrator then fires HBlank/VBlank DMA (the
    // PPU already raised the matching IRQs internally). The HBlank/VBlank IRQ
    // *enable* bits are checked inside `Ppu::step`, so here we only drive DMA.
    //
    // Register-file ownership: `state9`/`state7` are the canonical BIOS-facing
    // files. We move them into the two `Cpu` executors for the duration of the
    // frame (so `Cpu::step`'s own `self.state` is live), and sync them back
    // into `state9`/`state7` around every point where BIOS code runs
    // (`bios_service_wait` at VBlank, `nitro_os_tick` once per frame, and at
    // frame end). The SWI seam inside `Cpu::step` independently swaps the live
    // state into `state9`/`state7` for the duration of each HLE call.
    /// ARM9 exception entries within a single frame above which we declare a
    /// fault loop. A healthy frame takes only a handful (a VBlank IRQ or two); a
    /// CPU wedged re-faulting every few instructions takes tens of thousands —
    /// a crash, not a running game. Mirrors the PS1 core's exception-storm
    /// threshold (the ARM is exception-based, like the MIPS).
    const FAULT_THRESHOLD: u64 = 10_000;

    pub fn run_frame(&mut self) {
        use crate::ppu::ppu::{DOTS_PER_LINE, LINES_PER_FRAME};

        // Once faulted, freeze: keep the crash panel presented, run no CPU code.
        if self.fault.is_some() {
            self.present_crash();
            return;
        }

        /// Instruction steps per dot, ARM9 (ARM9 runs at ~2x the dot clock).
        const ARM9_STEPS_PER_DOT: u32 = 2;
        /// Bus cycles per dot — the rate the timer + sound blocks advance at.
        const BUS_CYCLES_PER_DOT: u32 = 6;
        /// How many dots we batch before ticking the PPU/timers/sound. One
        /// scanline keeps the PPU line bookkeeping atomic per call.
        const BATCH_DOTS: u32 = DOTS_PER_LINE;

        let total_dots = DOTS_PER_LINE * LINES_PER_FRAME;

        // Take the executors out of `self` so we can borrow `&mut self` for the
        // bus while stepping; restore them at the end. Move the canonical
        // register files into the live executors first.
        let mut cpu9 = std::mem::replace(&mut self.cpu9, Cpu::new(Core::Arm9));
        let mut cpu7 = std::mem::replace(&mut self.cpu7, Cpu::new(Core::Arm7));
        cpu9.state = std::mem::take(&mut self.state9);
        cpu7.state = std::mem::take(&mut self.state7);

        // Snapshot the ARM9 exception count to measure this frame's delta below.
        let exc_before = cpu9.exceptions;

        let mut dots_done = 0u32;
        let mut a7_carry = 0u32;

        while dots_done < total_dots {
            let batch = BATCH_DOTS.min(total_dots - dots_done);

            for _ in 0..batch {
                for _ in 0..ARM9_STEPS_PER_DOT {
                    // Sample the ARM9 IRQ lines fresh before each step (devices
                    // ticked since the last step may have raised an IRQ).
                    cpu9.irq_line = self.irq9.pending();
                    cpu9.wake_line = self.irq9.wake_pending();
                    // A halted CPU with no wake source can't make progress; skip
                    // its step (matches the TS guard). `Cpu::step` itself lifts
                    // halt when a wake/IRQ is present, so we still step then.
                    if !cpu9.state.halted || cpu9.wake_line {
                        cpu9.step(self);
                    }

                    // ARM7 runs at half the ARM9 rate: one ARM7 step per 2 ARM9.
                    a7_carry += 1;
                    if a7_carry >= ARM9_STEPS_PER_DOT {
                        a7_carry -= ARM9_STEPS_PER_DOT;
                        cpu7.irq_line = self.irq7.pending();
                        cpu7.wake_line = self.irq7.wake_pending();
                        if !cpu7.state.halted || cpu7.wake_line {
                            cpu7.step(self);
                        }
                    }
                }
            }

            // Just before the PPU crosses into VBlank (vcount 191 → 192) it
            // composites the whole frame, including Engine A's BG0-as-3D layer.
            // Build that 3D layer from the geometry engine's front display list
            // now (the GXFIFO interpreter already drained the back list and a
            // SWAP_BUFFERS promoted it), and hand it to Engine A so the next
            // `ppu.step` render reads it as BG0. We do this exactly once per
            // frame, on the line before the render, to avoid re-rasterizing the
            // whole 3D scene 263 times.
            if self.ppu.vcount + 1 == crate::ppu::ppu::VISIBLE_LINES {
                self.refresh_3d_layer();
            }

            // Advance the PPU one batch of dots. It raises HBlank/VBlank/VCount
            // IRQs internally and reports which DMA triggers to fire.
            let tick = self.ppu.step(
                batch,
                &mut self.irq9,
                &mut self.irq7,
                &self.mem,
                &self.vram,
            );
            if tick.hblank {
                self.dma_trigger_hblank();
            }
            if tick.vblank {
                self.dma_trigger_vblank();
                // VBlank housekeeping: refresh the cooked touch struct, then let
                // both cores' IntrWait latches observe the VBlank IRQ. These read
                // `state9`/`state7`, so sync the live files in and back out.
                self.touch_tick_vblank();
                // Hand the live files to the canonical slots, run the IntrWait
                // service (which may clear `state*.halted`), then take them back.
                std::mem::swap(&mut cpu9.state, &mut self.state9);
                std::mem::swap(&mut cpu7.state, &mut self.state7);
                self.bios_service_wait(true);
                self.bios_service_wait(false);
                std::mem::swap(&mut cpu9.state, &mut self.state9);
                std::mem::swap(&mut cpu7.state, &mut self.state7);
            }

            // Timers + sound advance in bus cycles (both cores' timer blocks tick
            // at the same bus clock; the prescalers divide further).
            let bus_cycles = batch * BUS_CYCLES_PER_DOT;
            self.timers9.step(bus_cycles, &mut self.irq9);
            self.timers7.step(bus_cycles, &mut self.irq7);
            self.sound.step(bus_cycles);

            dots_done += batch;
        }

        // Move the live register files back into the canonical slots before the
        // once-per-frame NitroSDK deadlock assist (it reads `state9`). The
        // executors return to their resting state (their `state` is overwritten
        // from `state9`/`state7` at the start of the next `run_frame`).
        self.state9 = std::mem::take(&mut cpu9.state);
        self.state7 = std::mem::take(&mut cpu7.state);
        self.nitro_os_tick();

        // Fault-loop detection: a storm of ARM9 exception entries this frame
        // means the CPU is wedged re-faulting (e.g. an undefined-instruction
        // vector with no valid handler, looping forever). Capture the cause +
        // PC and switch to the crash screen.
        if cpu9.exceptions.wrapping_sub(exc_before) > Self::FAULT_THRESHOLD {
            self.fault = Some(Fault {
                code: cpu9.last_exc,
                pc: cpu9.last_exc_pc,
                frame: self.ppu.frame_count,
            });
        }

        // Restore the executors (the assist may have raised an IRQ; the next
        // frame re-samples the lines from the IRQ controller).
        self.cpu9 = cpu9;
        self.cpu7 = cpu7;

        // On first detection, present the crash panel now (subsequent frames
        // short-circuit at the top of `run_frame`).
        if self.fault.is_some() {
            self.present_crash();
        }

        // Mix one frame of audio (~735 stereo samples @ 44.1 kHz) for the host.
        const AUDIO_FRAMES: usize = 735;
        let mut tmp = [0.0f32; AUDIO_FRAMES * 2];
        self.sound
            .mix(&mut tmp, AUDIO_FRAMES, 44_100, &self.mem.main_ram[..], &self.mem.arm7_iwram[..]);
        if self.audio_buf.len() > 44_100 {
            self.audio_buf.clear(); // host fell behind — drop the backlog
        }
        self.audio_buf.extend_from_slice(&tmp);
    }

    /// Draw the crash screen into the TOP-screen framebuffer from the latched
    /// [`Fault`]. Called once on detection and every frame thereafter so the
    /// panel stays presented while both CPUs are frozen.
    fn present_crash(&mut self) {
        use crate::cpu::exec::exc_name;
        use crate::ppu::ppu::{SCREEN_H, SCREEN_W};
        let f = match self.fault {
            Some(f) => f,
            None => return,
        };
        let lines = [
            "NDS CORE FAULT".to_string(),
            exc_name(f.code).to_string(),
            format!("PC {:08X}", f.pc),
            format!("FRAME {}", f.frame),
        ];
        let fb = self.ppu.top_framebuffer_mut();
        crate::crash::render(fb, SCREEN_W, SCREEN_H, &lines);
    }

    /// Drain mixed audio since the last call (interleaved stereo f32, 44.1 kHz).
    pub fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio_buf)
    }

    /// Rasterize the 3D geometry engine's front display list into a full
    /// 256x192 packed layer and hand it to Engine A as the BG0-in-3D-mode
    /// source. Called once per frame (just before the PPU's VBlank composite)
    /// from `run_frame`.
    ///
    /// When Engine A's DISPCNT bit 3 (BG0 = 3D) is clear, the 3D layer is not
    /// composited, so we drop any stale layer (BG0 reverts to its 2D meaning /
    /// transparent stub). When set, `Gpu3d::render_scanline` rasterizes the
    /// front scene once (its first call after a SWAP_BUFFERS) and packs each
    /// scanline into the engine_a `PX_TRANSPARENT` convention; we stitch the
    /// 192 scanlines into one `SCREEN_W * SCREEN_H` buffer the compositor then
    /// reads per pixel at BG0CNT's priority.
    fn refresh_3d_layer(&mut self) {
        use crate::ppu::ppu::{SCREEN_H, SCREEN_W};

        // BG0-as-3D only exists on Engine A, gated by DISPCNT bit 3.
        if (self.ppu.engine_a.dispcnt & 0x8) == 0 {
            self.ppu.engine_a.gx_bg0_layer = None;
            return;
        }

        let mut layer = vec![0u32; SCREEN_W * SCREEN_H].into_boxed_slice();
        for y in 0..SCREEN_H as u32 {
            let line = self
                .gpu3d
                .render_scanline(y, &self.mem, &self.vram, &self.ppu.vramcnt);
            let row = (y as usize) * SCREEN_W;
            layer[row..row + SCREEN_W].copy_from_slice(&line[..SCREEN_W]);
        }
        self.ppu.engine_a.gx_bg0_layer = Some(layer);
    }

    /// The 256x192 RGBA8888 framebuffer for the TOP screen (POWCNT1 bit 15
    /// selects which 2D engine drives it). Stable until the next `run_frame`.
    pub fn top_framebuffer(&self) -> &[u8] {
        self.ppu.top_framebuffer()
    }
    /// The 256x192 RGBA8888 framebuffer for the BOTTOM screen.
    pub fn bottom_framebuffer(&self) -> &[u8] {
        self.ppu.bottom_framebuffer()
    }

    // ─── Input API ───────────────────────────────────────────────────────

    /// Set the button state. Both masks are ACTIVE-LOW (a 0 bit = pressed),
    /// matching the hardware registers the games poll.
    ///
    /// - `keyinput` → KEYINPUT (0x04000130), low 10 bits:
    ///   A, B, Select, Start, Right, Left, Up, Down, R, L.
    /// - `ext_keyinput` → EXTKEYIN (0x04000136): bit 0 = X, bit 1 = Y
    ///   (bits 3/6/7 are debug/pen/hinge and left to the IO defaults).
    pub fn set_keys(&mut self, keyinput: u32, ext_keyinput: u32) {
        self.keyinput = keyinput & 0x03FF;
        // Preserve the IO-managed high bits (pen-down bit 6, hinge bit 7) and
        // only let X/Y (bits 0..1) through from the caller.
        self.ext_keyinput = (self.ext_keyinput & !0x0003) | (ext_keyinput & 0x0003);
    }

    /// Set the touchscreen pointer. `pressed` gates the SPI touch latches the
    /// HLE touch tick cooks into the OS shared-work struct each VBlank. When
    /// pressed, `x`/`y` are bottom-screen coordinates (0..255 / 0..191); when
    /// released the latches are cleared so EXTKEYIN reads "pen up".
    pub fn set_touch(&mut self, pressed: bool, x: u16, y: u16) {
        if pressed {
            self.spi.touch_x = Some(x as u32);
            self.spi.touch_y = Some(y as u32);
            // Pressure latch above the cook threshold (PRESSURE_THRESHOLD).
            self.spi.touch_z = 0x800;
        } else {
            self.spi.touch_x = None;
            self.spi.touch_y = None;
            self.spi.touch_z = 0;
        }
    }

    /// Load a `.nds` cartridge image and HLE-boot it (parse header, copy
    /// binaries + overlays into RAM, mount the cart, seed BIOS RAM, reset both
    /// CPUs to their entry points). Forwards to the BIOS HLE boot seam.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.hle_boot(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::WramCnt;

    // Mirrors ds-recomp src/test/bus9_tcm.test.ts (DTCM mirroring).
    #[test]
    fn dtcm_virtual_mirrors_physical() {
        let mut nds = Nds::new();
        nds.bus9.dtcm_base = 0x0080_0000;
        nds.bus9.dtcm_virtual_size = 0x4000;
        nds.write32_arm9(0x0080_0000, 0x1122_3344);
        nds.write32_arm9(0x0080_3FFC, 0xAABB_CCDD);
        // Move and double the virtual size — the 16 KB physical bank mirrors.
        nds.bus9.dtcm_base = 0x0060_0000;
        nds.bus9.dtcm_virtual_size = 0x8000;
        assert_eq!(nds.read32_arm9(0x0060_4000), 0x1122_3344);
        assert_eq!(nds.read32_arm9(0x0060_7FFC), 0xAABB_CCDD);
        assert_eq!(nds.read32_arm9(0x0060_0000), 0x1122_3344);
    }

    // Mirrors ds-recomp src/test/bus9_tcm.test.ts (priority + load mode).
    #[test]
    fn dtcm_priority_and_load_mode() {
        let mut nds = Nds::new();
        nds.mem.wramcnt = WramCnt::AllToArm9;
        nds.bus9.dtcm_enabled = false;
        nds.write32_arm9(0x0300_0000, 0xDEAD_0001);
        nds.bus9.dtcm_base = 0x0300_0000;
        nds.bus9.dtcm_virtual_size = 0x8000;
        nds.bus9.dtcm_enabled = true;

        // DTCM beats shared WRAM at the same address.
        nds.write32_arm9(0x0300_0000, 0xCAFE_BABE);
        assert_eq!(nds.read32_arm9(0x0300_0000), 0xCAFE_BABE);

        // Load-mode read bypasses DTCM (sees WRAM), write still hits DTCM.
        nds.bus9.dtcm_load_mode = true;
        assert_eq!(nds.read32_arm9(0x0300_0000), 0xDEAD_0001);
        nds.write32_arm9(0x0300_0004, 0x1234_5678);
        nds.bus9.dtcm_load_mode = false;
        assert_eq!(nds.read32_arm9(0x0300_0004), 0x1234_5678);
    }

    // WRAMCNT split + cross-CPU shared visibility. With AllToArm9, an ARM9
    // write to shared WRAM is invisible to ARM7 (it sees its IWRAM mirror);
    // with AllToArm7 both halves belong to ARM7.
    #[test]
    fn wramcnt_split_routing() {
        let mut nds = Nds::new();

        // AllToArm9: ARM9 sees the whole 32 KB at 0x03000000; ARM7's
        // 0x03000000 hits its private IWRAM, not the shared block.
        nds.mem.wramcnt = WramCnt::AllToArm9;
        nds.write32_arm9(0x0300_0000, 0x1111_2222);
        assert_eq!(nds.read32_arm9(0x0300_0000), 0x1111_2222);
        assert_eq!(nds.read32_arm7(0x0300_0000), 0); // IWRAM, untouched

        // AllToArm7: ARM9 sees nothing at 0x03000000; ARM7 sees the shared
        // block there.
        nds.mem.wramcnt = WramCnt::AllToArm7;
        nds.write32_arm7(0x0300_0000, 0x3333_4444);
        assert_eq!(nds.read32_arm7(0x0300_0000), 0x3333_4444);
        assert_eq!(nds.read32_arm9(0x0300_0000), 0); // ARM9 sees open bus
    }

    // Main RAM is shared: an ARM9 write is observable from ARM7.
    #[test]
    fn main_ram_shared_between_cores() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0200_1000, 0xABCD_1234);
        assert_eq!(nds.read32_arm7(0x0200_1000), 0xABCD_1234);
        // Mirror at 0x01000000 (ARM9 only) aliases the same byte.
        assert_eq!(nds.read32_arm9(0x0100_1000), 0xABCD_1234);
    }

    // VRAM LCDC alias routing: bank A in LCDC mode (MST=0, enabled) appears at
    // 0x06800000 and writes land in the flat vram[] at bank A's offset (0).
    #[test]
    fn vram_lcdc_bank_a_routes() {
        let mut nds = Nds::new();
        nds.ppu.vramcnt[0] = 0x80; // bank A enabled, MST=0 (LCDC)
        nds.write32_arm9(0x0680_0000, 0xFEED_BEEF);
        assert_eq!(nds.read32_arm9(0x0680_0000), 0xFEED_BEEF);
        assert_eq!(nds.mem.vram[0], 0xEF);
    }

    // CP15 DTCM relocation: writing CRn=9,CRm=1,opc2=0 moves the DTCM window.
    #[test]
    fn cp15_relocates_dtcm() {
        let mut nds = Nds::new();
        // base=0x02800000, size code 5 → virtual size 512<<5 = 16 KB.
        let value = 0x0280_0000 | (5 << 1);
        nds.cp15_write(0, 9, 1, 0, value);
        assert_eq!(nds.bus9.dtcm_base, 0x0280_0000);
        assert_eq!(nds.bus9.dtcm_virtual_size, 512 << 5);
    }

    // ════════════════════════════════════════════════════════════════════
    // IO register dispatch — ported from ds-recomp io_register_routing.test.ts
    // (the in-scope device branches: DMA, math, IPC, IRQ, keypad, POSTFLG/
    // POWCNT, VRAMCNT/WRAMCNT, sound isolation, SPI). PPU/cart/GX branches are
    // out of scope this wave and intentionally not asserted.
    // ════════════════════════════════════════════════════════════════════

    use crate::io::irq::{IRQ_DMA0, IRQ_TIMER0};

    // ─── KEYINPUT ────────────────────────────────────────────────────────
    #[test]
    fn keyinput_reads_and_updates_live() {
        let mut nds = Nds::new();
        nds.keyinput = 0x3FF;
        assert_eq!(nds.read8_arm9(0x0400_0130), 0xFF);
        assert_eq!(nds.read8_arm9(0x0400_0131), 0x03);
        nds.keyinput = 0x3FE; // press A (clear bit 0)
        assert_eq!(nds.read8_arm9(0x0400_0130), 0xFE);
    }

    // ─── IME / IE / IF byte slices ───────────────────────────────────────
    #[test]
    fn ime_round_trips() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0208, 1);
        assert!(nds.irq9.ime);
        assert_eq!(nds.read8_arm9(0x0400_0208), 1);
        nds.write8_arm9(0x0400_0208, 0);
        assert!(!nds.irq9.ime);
        assert_eq!(nds.read8_arm9(0x0400_0208), 0);
    }

    #[test]
    fn ie_round_trips_byte_by_byte() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0210, 0x11);
        nds.write8_arm9(0x0400_0211, 0x22);
        nds.write8_arm9(0x0400_0212, 0x33);
        nds.write8_arm9(0x0400_0213, 0x44);
        assert_eq!(nds.irq9.ie, 0x4433_2211);
        assert_eq!(nds.read8_arm9(0x0400_0210), 0x11);
        assert_eq!(nds.read8_arm9(0x0400_0213), 0x44);
    }

    #[test]
    fn if_acks_by_writing_one_to_clear() {
        let mut nds = Nds::new();
        nds.irq9.raise(0xFF);
        assert_eq!(nds.read8_arm9(0x0400_0214), 0xFF);
        // Write 0x0F to ack the low 4 bits.
        nds.write8_arm9(0x0400_0214, 0x0F);
        assert_eq!(nds.read8_arm9(0x0400_0214), 0xF0);
        // Second-byte ack slice.
        nds.irq9.raise(0x00FF_0000);
        nds.write8_arm9(0x0400_0216, 0xFF);
        assert_eq!(nds.read8_arm9(0x0400_0216), 0);
    }

    #[test]
    fn ie_round_trips_word() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0400_0210, 0xDEAD_BEEF);
        assert_eq!(nds.irq9.ie, 0xDEAD_BEEF);
        assert_eq!(nds.read32_arm9(0x0400_0210), 0xDEAD_BEEF);
    }

    // The two cores have INDEPENDENT IRQ controllers: an ARM9 IE write must
    // not be visible to ARM7.
    #[test]
    fn irq_controllers_are_per_core() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0400_0210, 0xFFFF_FFFF);
        assert_eq!(nds.irq9.ie, 0xFFFF_FFFF);
        assert_eq!(nds.irq7.ie, 0);
        assert_eq!(nds.read32_arm7(0x0400_0210), 0);
    }

    // ─── POSTFLG / POWCNT1 ───────────────────────────────────────────────
    #[test]
    fn postflg_per_core() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0300, 0xAA);
        assert_eq!(nds.postflg9, 0xAA);
        assert_eq!(nds.read8_arm9(0x0400_0300), 0xAA);
        // ARM7 POSTFLG is a separate latch.
        nds.write8_arm7(0x0400_0300, 0x01);
        assert_eq!(nds.postflg7, 0x01);
        assert_eq!(nds.postflg9, 0xAA);
    }

    #[test]
    fn powcnt1_round_trips_byte_by_byte() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0304, 0x11);
        nds.write8_arm9(0x0400_0305, 0x22);
        nds.write8_arm9(0x0400_0306, 0x33);
        nds.write8_arm9(0x0400_0307, 0x44);
        assert_eq!(nds.powcnt1, 0x4433_2211);
        assert_eq!(nds.read8_arm9(0x0400_0304), 0x11);
        assert_eq!(nds.read8_arm9(0x0400_0307), 0x44);
    }

    // ─── PPU engine register routing (DISPCNT/BGxCNT/DISPSTAT/VCOUNT) ────
    #[test]
    fn dispcnt_and_bg0cnt_route_through_arm9_bus() {
        let mut nds = Nds::new();
        // DISPCNT (0x04000000) — write32 lands all four bytes in engine A.
        nds.write32_arm9(0x0400_0000, 0x0001_0100);
        assert_eq!(nds.ppu.engine_a.dispcnt, 0x0001_0100);
        assert_eq!(nds.read32_arm9(0x0400_0000), 0x0001_0100);
        // BG0CNT (0x04000008) — 16-bit register.
        nds.write16_arm9(0x0400_0008, 0x0100);
        assert_eq!(nds.ppu.engine_a.bg.cnt[0], 0x0100);
        // Engine B mirror (0x04001000) is a separate latch.
        nds.write32_arm9(0x0400_1000, 0xDEAD_BEEF);
        assert_eq!(nds.ppu.engine_b.dispcnt, 0xDEAD_BEEF);
        assert_eq!(nds.ppu.engine_a.dispcnt, 0x0001_0100);
    }

    #[test]
    fn dispstat_writable_bits_and_vcount_readonly() {
        let mut nds = Nds::new();
        // DISPSTAT enable + target bits (0xFFF8) are writable; status bits aren't.
        nds.write16_arm9(0x0400_0004, 0xFFF8);
        assert_eq!(nds.read16_arm9(0x0400_0004) & 0xFFF8, 0xFFF8);
        // VCOUNT (0x04000006) is read-only — writes are swallowed.
        nds.ppu.vcount = 42;
        nds.write16_arm9(0x0400_0006, 0x0123);
        assert_eq!(nds.read16_arm9(0x0400_0006), 42);
    }

    // ─── VRAMCNT / WRAMCNT ───────────────────────────────────────────────
    #[test]
    fn vramcnt_write_arm9_read_back() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0240, 0x81);
        assert_eq!(nds.ppu.vramcnt[0], 0x81);
        assert_eq!(nds.read8_arm9(0x0400_0240), 0x81);
    }

    #[test]
    fn wramcnt_write_masks_to_two_bits() {
        let mut nds = Nds::new();
        nds.write8_arm9(0x0400_0247, 0xFF);
        assert_eq!(nds.mem.wramcnt.bits(), 0x03);
        assert_eq!(nds.read8_arm9(0x0400_0247), 0x03);
    }

    #[test]
    fn arm7_cannot_write_vramcnt() {
        let mut nds = Nds::new();
        let before = nds.ppu.vramcnt[0];
        nds.write8_arm7(0x0400_0240, 0x42);
        assert_eq!(nds.ppu.vramcnt[0], before);
    }

    // ─── ARM7 sound isolation ────────────────────────────────────────────
    // SOUNDCNT (0x04000500) is ARM7-only; ARM9 reads see GX/open-bus → 0.
    #[test]
    fn sound_port_arm7_only() {
        let mut nds = Nds::new();
        nds.write8_arm7(0x0400_0500, 0xAA);
        nds.write8_arm7(0x0400_0501, 0xBB);
        assert_eq!(nds.read8_arm7(0x0400_0500), 0xAA);
        assert_eq!(nds.read8_arm7(0x0400_0501), 0xBB);
        // ARM9 must NOT see the ARM7 sound state.
        assert_eq!(nds.read8_arm9(0x0400_0500), 0);
    }

    // ─── DMA register routing ────────────────────────────────────────────
    #[test]
    fn dma_sad_routes_through_io() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0400_00B0, 0x0200_0000);
        assert_eq!(nds.dma9.channels[0].src, 0x0200_0000);
        assert_eq!(nds.read32_arm9(0x0400_00B0), 0x0200_0000);
        // Byte write composes too.
        nds.write8_arm9(0x0400_00B0, 0xEF);
        assert_eq!(nds.dma9.channels[0].src & 0xFF, 0xEF);
    }

    // ─── DS Math routing (ARM9 only) ─────────────────────────────────────
    #[test]
    fn ds_math_routes_on_arm9_not_arm7() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0400_0290, 0xDEAD_BEEF);
        assert_eq!(nds.read32_arm9(0x0400_0290), 0xDEAD_BEEF);
        // ARM7 has no math accelerator — the write is a no-op (open bus).
        let mut nds7 = Nds::new();
        nds7.write32_arm7(0x0400_0290, 0xDEAD_BEEF);
        assert_eq!(nds7.read32_arm9(0x0400_0290), 0);
    }

    // ─── IPC SEND/RECV FIFO across cores ─────────────────────────────────
    // ARM9 writes a word to its SEND FIFO (0x04000188); ARM7 pops it from its
    // RECV FIFO (0x04100000). Mirrors io_register_routing.test.ts.
    #[test]
    fn ipc_send_word_received_by_other_core() {
        let mut nds = Nds::new();
        nds.ipc.enable9 = true;
        nds.ipc.enable7 = true;
        nds.write32_arm9(0x0400_0188, 0xDEAD_BEEF);
        let got = nds.read32_arm7(0x0410_0000);
        assert_eq!(got, 0xDEAD_BEEF);
    }

    // ═══════════════════════════════════════════════════════════════════
    // DMA memory walk — ported from ds-recomp dma.test.ts. The transfer runs
    // through the Nds bus (main RAM at 0x02000000), exercising the immediate-
    // enable → run_dma_channel path.
    // ═══════════════════════════════════════════════════════════════════

    fn encode_cnt(count: u32, word32: bool, src_mode: u32, dst_mode: u32, irq: bool) -> u32 {
        let mut ctrl = 0x8000u32; // enable
        ctrl |= dst_mode << 5;
        ctrl |= src_mode << 7;
        if word32 {
            ctrl |= 1 << 10;
        }
        if irq {
            ctrl |= 1 << 14;
        }
        (ctrl << 16) | (count & 0xFFFF)
    }

    #[test]
    fn dma_immediate_word_copy_and_clears_enable() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        for i in 0..8u32 {
            nds.write32_arm9(src + i * 4, 0xDEAD_BE00 + i);
        }
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        // Immediate, word32, enable, count 8 — fires the transfer now.
        nds.write32_arm9(0x0400_00B8, encode_cnt(8, true, 0, 0, false));
        for i in 0..8u32 {
            assert_eq!(nds.read32_arm9(dst + i * 4), 0xDEAD_BE00 + i);
        }
        // Enable bit (31) cleared, channel disabled (non-repeat immediate).
        assert_eq!((nds.read32_arm9(0x0400_00B8) >> 31) & 1, 0);
        assert!(!nds.dma9.channels[0].enabled);
    }

    #[test]
    fn dma_halfword_transfer_moves_count_times_two() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        for i in 0..4u32 {
            nds.write16_arm9(src + i * 2, 0x1100 + i);
        }
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        nds.write32_arm9(0x0400_00B8, encode_cnt(4, false, 0, 0, false));
        for i in 0..4u32 {
            assert_eq!(nds.read16_arm9(dst + i * 2), 0x1100 + i);
        }
        // One past the end untouched.
        assert_eq!(nds.read16_arm9(dst + 8), 0);
    }

    #[test]
    fn dma_src_mode_fixed_reads_same_word() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        nds.write32_arm9(src, 0x5566_7788);
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        // src_mode 2 = fixed.
        nds.write32_arm9(0x0400_00B8, encode_cnt(4, true, 2, 0, false));
        for i in 0..4u32 {
            assert_eq!(nds.read32_arm9(dst + i * 4), 0x5566_7788);
        }
    }

    #[test]
    fn dma_src_mode_decrement_walks_backwards() {
        let mut nds = Nds::new();
        let dst = 0x0200_0200;
        nds.write32_arm9(0x0200_0110, 0xA1);
        nds.write32_arm9(0x0200_010C, 0xA2);
        nds.write32_arm9(0x0200_0108, 0xA3);
        nds.write32_arm9(0x0200_0104, 0xA4);
        nds.write32_arm9(0x0400_00B0, 0x0200_0110);
        nds.write32_arm9(0x0400_00B4, dst);
        nds.write32_arm9(0x0400_00B8, encode_cnt(4, true, 1, 0, false));
        assert_eq!(nds.read32_arm9(dst), 0xA1);
        assert_eq!(nds.read32_arm9(dst + 4), 0xA2);
        assert_eq!(nds.read32_arm9(dst + 8), 0xA3);
        assert_eq!(nds.read32_arm9(dst + 12), 0xA4);
    }

    // dst mode 3 = increment+reload: walks forward during the transfer (so all
    // N words land at consecutive addresses), then the register snaps back.
    #[test]
    fn dma_dst_mode3_walks_then_register_reloads() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        for i in 0..4u32 {
            nds.write32_arm9(src + i * 4, 0xC0DE_0000 + i);
        }
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        nds.write32_arm9(0x0400_00B8, encode_cnt(4, true, 0, 3, false));
        // All four words must land at distinct, consecutive destination slots.
        for i in 0..4u32 {
            assert_eq!(nds.read32_arm9(dst + i * 4), 0xC0DE_0000 + i);
        }
        // Register snapped back to the latched start.
        assert_eq!(nds.dma9.channels[0].dst, dst);
    }

    // Completion IRQ: a finished channel with irq_on_done raises IRQ_DMA0 on
    // the owning core.
    #[test]
    fn dma_completion_raises_irq() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        nds.write32_arm9(src, 0x1234_5678);
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        nds.write32_arm9(0x0400_00B8, encode_cnt(1, true, 0, 0, true));
        assert_ne!(nds.irq9.iflag & IRQ_DMA0, 0);
    }

    // VBlank-timed channel doesn't fire on arm, but does on the trigger.
    #[test]
    fn dma_vblank_timed_runs_on_trigger() {
        let mut nds = Nds::new();
        let src = 0x0200_0100;
        let dst = 0x0200_0200;
        for i in 0..4u32 {
            nds.write32_arm9(src + i * 4, 0xBEEF_0000 + i);
        }
        nds.write32_arm9(0x0400_00B0, src);
        nds.write32_arm9(0x0400_00B4, dst);
        // VBlank timing (1 << 11), word32, enable, count 4.
        let ctrl = 0x8000 | (1 << 11) | (1 << 10);
        nds.write32_arm9(0x0400_00B8, (ctrl << 16) | 4);
        // Not yet transferred.
        assert_eq!(nds.read32_arm9(dst), 0);
        nds.dma_trigger_vblank();
        for i in 0..4u32 {
            assert_eq!(nds.read32_arm9(dst + i * 4), 0xBEEF_0000 + i);
        }
    }

    // ─── Timer overflow → IRQ ────────────────────────────────────────────
    // A timer at prescale 1, reload 0xFFFF, enabled with IRQ: one tick past
    // 0xFFFF overflows and raises IRQ_TIMER0.
    #[test]
    fn timer_overflow_raises_irq_through_core() {
        let mut nds = Nds::new();
        // reload = 0xFFFF.
        nds.write8_arm9(0x0400_0100, 0xFF);
        nds.write8_arm9(0x0400_0101, 0xFF);
        // control: enable (bit 7) + IRQ (bit 6), prescale 0 (= /1).
        nds.write8_arm9(0x0400_0102, 0xC0);
        nds.irq9.set_ie(IRQ_TIMER0);
        // Two cycles: 0xFFFF → 0x0000 (overflow) → reload.
        nds.timers9.step(2, &mut nds.irq9);
        assert_ne!(nds.irq9.iflag & IRQ_TIMER0, 0);
    }

    // ════════════════════════════════════════════════════════════════════
    // run_frame smoke test — the lockstep frame loop end-to-end.
    //
    // A synthetic cart whose ARM9 program writes DISPCNT = display-mode 1
    // (graphics, all BGs off → pure backdrop) and a red backdrop into engine-A
    // PRAM, then busy-loops. After load_rom + a few run_frame calls the top
    // framebuffer must show the red backdrop everywhere, proving both CPUs +
    // PPU + timers advanced in lockstep without panicking.
    // ════════════════════════════════════════════════════════════════════

    /// Assemble a minimal valid .nds header + two ARM binaries (mirrors the
    /// hle.rs test `synth_cart`).
    fn smoke_synth_cart(arm9: &[u8], arm9_ram: u32, arm7: &[u8], arm7_ram: u32) -> Vec<u8> {
        let arm9_off = 0x4000usize;
        let arm7_off = 0x8000usize;
        let total = (arm9_off + arm9.len()).max(arm7_off + arm7.len()).max(0x200);
        let mut rom = vec![0u8; total];
        rom[0x00..0x0B].copy_from_slice(b"SMOKEBOOT\0\0");
        rom[0x0C..0x10].copy_from_slice(b"ZZZE");
        rom[0x10..0x12].copy_from_slice(b"01");
        let w32 = |rom: &mut [u8], off: usize, v: u32| {
            rom[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        w32(&mut rom, 0x020, arm9_off as u32);
        w32(&mut rom, 0x024, arm9_ram); // ARM9 entry
        w32(&mut rom, 0x028, arm9_ram);
        w32(&mut rom, 0x02C, arm9.len() as u32);
        w32(&mut rom, 0x030, arm7_off as u32);
        w32(&mut rom, 0x034, arm7_ram); // ARM7 entry
        w32(&mut rom, 0x038, arm7_ram);
        w32(&mut rom, 0x03C, arm7.len() as u32);
        rom[arm9_off..arm9_off + arm9.len()].copy_from_slice(arm9);
        rom[arm7_off..arm7_off + arm7.len()].copy_from_slice(arm7);
        rom
    }

    #[test]
    fn run_frame_renders_backdrop_from_a_loaded_cart() {
        // ── ARM9 program (little-endian ARM words) ───────────────────────────
        //   MOV  r0, #0x04000000   ; DISPCNT (engine A)
        //   MOV  r1, #0x00010000   ; display-mode 1 (graphics), all BGs off
        //   STR  r1, [r0]
        //   MOV  r0, #0x05000000   ; PRAM (engine A palette)
        //   MOV  r1, #0x0000001F   ; BGR555 red (backdrop = palette entry 0)
        //   STRH r1, [r0]
        //   B    .                 ; busy-loop forever
        let arm9_words: [u32; 7] = [
            0xE3A0_0404, // MOV r0,#0x04000000
            0xE3A0_1801, // MOV r1,#0x00010000
            0xE580_1000, // STR r1,[r0]
            0xE3A0_0405, // MOV r0,#0x05000000
            0xE3A0_101F, // MOV r1,#0x0000001F
            0xE1C0_10B0, // STRH r1,[r0]
            0xEAFF_FFFE, // B . (self)
        ];
        let mut arm9_bytes = Vec::new();
        for w in arm9_words {
            arm9_bytes.extend_from_slice(&w.to_le_bytes());
        }
        // ARM7 program: just busy-loop (B .) so its core has something to step.
        let arm7_bytes = 0xEAFF_FFFEu32.to_le_bytes().to_vec();

        let rom = smoke_synth_cart(&arm9_bytes, 0x0200_0000, &arm7_bytes, 0x0380_0000);

        let mut nds = Nds::new();
        nds.load_rom(&rom);

        // Both cores booted to their entry points in SYS/ARM mode.
        assert_eq!(nds.state9.r[15], 0x0200_0000);
        assert_eq!(nds.state7.r[15], 0x0380_0000);

        // Run a handful of frames — must not panic and must complete frames.
        let start_frames = nds.ppu.frame_count;
        for _ in 0..3 {
            nds.run_frame();
        }
        assert!(
            nds.ppu.frame_count > start_frames,
            "run_frame should advance the PPU frame counter"
        );

        // DISPCNT was programmed by the ARM9 code through the bus.
        assert_eq!(nds.ppu.engine_a.dispcnt & 0x0003_0000, 0x0001_0000);

        // The top framebuffer (POWCNT1 default 0x820F → engine A on top) shows
        // the red backdrop the program wrote: BGR555 0x001F → RGBA (255,0,0,255).
        let fb = nds.top_framebuffer();
        assert_eq!(fb.len(), 256 * 192 * 4);
        assert_eq!(fb[3], 0xFF, "framebuffer must be opaque (rendered)");
        // Sample a few pixels — all backdrop red.
        for &(x, y) in &[(0usize, 0usize), (128, 96), (255, 191)] {
            let o = (y * 256 + x) * 4;
            assert_eq!(
                (fb[o], fb[o + 1], fb[o + 2], fb[o + 3]),
                (0xFF, 0, 0, 0xFF),
                "pixel ({x},{y}) should be backdrop red"
            );
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // 3D pipeline integration — submit a single flat triangle through the
    // GXFIFO at the Nds bus level (writes to 0x04000440+ direct command ports),
    // drive a full frame, and assert the triangle reaches BOTH the geometry
    // engine's 3D layer AND the composited top framebuffer as Engine A's BG0.
    //
    // This exercises the whole 3D path end-to-end: GXFIFO command interpreter →
    // matrix/vertex assembly → deferred display list → SWAP_BUFFERS promote →
    // `run_frame`'s once-per-frame `refresh_3d_layer` rasterize → Engine A BG0
    // composite (priority from BG0CNT) → BGR555→RGBA framebuffer.
    // ════════════════════════════════════════════════════════════════════

    /// Push one GX command + its parameters through the ARM9 GXFIFO direct
    /// command ports. The direct port for opcode `op` is
    /// `0x04000440 + (op - 0x10) * 4`; each parameter word is written there.
    /// A zero-parameter command is still "issued" by one write to its port.
    fn gx_cmd(nds: &mut Nds, op: u8, params: &[u32]) {
        let port = 0x0400_0440 + ((op as u32) - 0x10) * 4;
        if params.is_empty() {
            nds.write32_arm9(port, 0);
        } else {
            for &p in params {
                nds.write32_arm9(port, p);
            }
        }
    }

    #[test]
    fn gxfifo_triangle_reaches_3d_layer_and_top_framebuffer() {
        let mut nds = Nds::new();

        // ── Engine A: graphics display-mode 1, BG0 = 3D (DISPCNT bit 3),
        //    BG0 enabled (bit 8). Backdrop = black so the green triangle stands
        //    out. BG0CNT priority 0. ──────────────────────────────────────────
        nds.write32_arm9(0x0400_0000, (1 << 16) | (1 << 8) | (1 << 3));
        nds.write16_arm9(0x0400_0008, 0x0000); // BG0CNT priority 0
        // Backdrop (engine A PRAM entry 0) = black.
        nds.mem.pram[0] = 0x00;
        nds.mem.pram[1] = 0x00;

        // ── Submit one flat green triangle through the GXFIFO. Default matrix
        //    stacks are identity, so a Q12 vertex maps straight to NDC. The
        //    triangle's NDC corners spill past the screen edges so the center
        //    (128, 96) is solidly inside. ─────────────────────────────────────
        use crate::ppu::gx::FP_ONE;
        let q = FP_ONE as u32;

        gx_cmd(&mut nds, 0x29, &[0x001F_0000]); // POLYGON_ATTR: alpha 31 (opaque)
        gx_cmd(&mut nds, 0x20, &[0x03E0]); // COLOR: green (BGR555)
        gx_cmd(&mut nds, 0x40, &[0]); // BEGIN_VTXS: triangle list
        // VTX_16: p0 = (X | Y<<16), p1 = Z. Two's-complement 16-bit each.
        let v16 = |x: i32, y: i32| (x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16);
        gx_cmd(&mut nds, 0x23, &[v16(-2 * q as i32, -2 * q as i32), 0]);
        gx_cmd(&mut nds, 0x23, &[v16(2 * q as i32, -2 * q as i32), 0]);
        gx_cmd(&mut nds, 0x23, &[v16(0, 3 * q as i32), 0]);
        gx_cmd(&mut nds, 0x41, &[]); // END_VTXS
        gx_cmd(&mut nds, 0x50, &[0]); // SWAP_BUFFERS — promote to front list

        // One queued triangle is now in the geometry engine's FRONT list.
        // (SWAP promoted the back list; `run_frame` rasterizes it.)

        // ── Drive a full frame: run_frame builds the 3D layer just before the
        //    VBlank composite and Engine A reads it as BG0. ──────────────────
        nds.run_frame();

        // 1) The geometry engine rasterized the front scene: center pixel drawn.
        let center3d = 96 * crate::ppu::gx::GX_SCREEN_W + 128;
        assert_eq!(
            nds.gpu3d.line().len(),
            crate::ppu::gx::GX_SCREEN_W,
            "3D line buffer is one screen wide"
        );

        // 2) Engine A received a 3D layer (run_frame filled it, DISPCNT bit 3 set).
        assert!(
            nds.ppu.engine_a.gx_bg0_layer.is_some(),
            "run_frame must hand Engine A the 3D BG0 layer when DISPCNT bit 3 is set"
        );
        let layer = nds.ppu.engine_a.gx_bg0_layer.as_ref().unwrap();
        assert_eq!(layer.len(), 256 * 192);
        // The 3D layer's center pixel is a drawn green texel (bit 15 CLEAR =
        // opaque in the PX_TRANSPARENT convention; BGR555 green = 0x03E0).
        assert_eq!(
            layer[center3d] & 0x8000,
            0,
            "3D layer center pixel must be drawn (opaque)"
        );
        assert_eq!(layer[center3d] & 0x7FFF, 0x03E0, "3D layer center = green");

        // 3) The composited TOP framebuffer shows the green triangle at center
        //    (Engine A on top by the POWCNT1 default), and the black backdrop in
        //    a corner the triangle can't reach.
        let fb = nds.top_framebuffer();
        let center = (96 * 256 + 128) * 4;
        assert_eq!(
            (fb[center], fb[center + 1], fb[center + 2], fb[center + 3]),
            (0x00, 0xFF, 0x00, 0xFF),
            "composited center pixel must be the green 3D triangle"
        );
        // A top corner the (downward-pointing) triangle never covers → backdrop.
        let corner = 0usize;
        assert_eq!(
            (fb[corner], fb[corner + 1], fb[corner + 2]),
            (0x00, 0x00, 0x00),
            "corner outside the triangle must be the black backdrop"
        );
        // Genuinely non-uniform: triangle != backdrop.
        assert_ne!(&fb[center..center + 3], &fb[corner..corner + 3]);
    }

    #[test]
    fn three_d_layer_dropped_when_dispcnt_bit3_clear() {
        // With BG0-as-3D disabled (DISPCNT bit 3 clear), run_frame must NOT
        // attach a 3D layer, even if geometry was submitted.
        let mut nds = Nds::new();
        nds.write32_arm9(0x0400_0000, 1 << 16); // graphics, BG0 NOT 3D
        gx_cmd(&mut nds, 0x40, &[0]); // BEGIN_VTXS
        gx_cmd(&mut nds, 0x50, &[0]); // SWAP_BUFFERS
        nds.run_frame();
        assert!(
            nds.ppu.engine_a.gx_bg0_layer.is_none(),
            "3D layer must be absent when DISPCNT bit 3 is clear"
        );
    }

    #[test]
    fn input_api_sets_keypad_and_touch() {
        let mut nds = Nds::new();
        // Press A (bit 0) + Start (bit 3): active-low → clear those bits.
        nds.set_keys(0x03FF & !0b1001, 0x0003 & !0b01); // also press X (ext bit 0)
        assert_eq!(nds.read8_arm9(0x0400_0130) & 0b1001, 0);
        // EXTKEYIN bit 0 (X) cleared = pressed.
        assert_eq!(nds.read8_arm9(0x0400_0136) & 0x01, 0);

        // Touch: pressed feeds the SPI latches the VBlank cook reads.
        nds.set_touch(true, 100, 80);
        assert_eq!(nds.spi.touch_x, Some(100));
        assert_eq!(nds.spi.touch_y, Some(80));
        assert!(nds.spi.touch_z > crate::io::touch::PRESSURE_THRESHOLD);
        // Pen-down reflected in EXTKEYIN bit 6 (active-low).
        assert_eq!(nds.read8_arm9(0x0400_0136) & 0x40, 0);

        // Release clears the latches.
        nds.set_touch(false, 0, 0);
        assert_eq!(nds.spi.touch_x, None);
        assert_ne!(nds.read8_arm9(0x0400_0136) & 0x40, 0, "pen up");
    }

    // ════════════════════════════════════════════════════════════════════
    // Crash screen — a forced ARM9 exception storm trips the fault watcher and
    // paints the crash panel into the top framebuffer.
    //
    // The ARM9 program is a single permanently-UNDEFINED instruction (the
    // ARMv5 "UDF" encoding). Executing it takes the undefined-instruction
    // vector, which on our HLE-booted high-vector region holds no valid
    // handler — the CPU re-faults every step. Across one frame that is tens of
    // thousands of exception entries: an unhandled-exception storm = a fault
    // loop, which sets `fault` and renders the crash screen.
    // ════════════════════════════════════════════════════════════════════
    #[test]
    fn run_frame_undef_storm_trips_crash_screen() {
        // ARM9: a permanently-undefined UDF (0xE7F000F0) at the entry point. No
        // valid UND handler exists at the high vector, so each execution re-takes
        // the exception → a storm within a single frame.
        let arm9_bytes = 0xE7F0_00F0u32.to_le_bytes().to_vec();
        // ARM7: busy-loop so its core has something to step.
        let arm7_bytes = 0xEAFF_FFFEu32.to_le_bytes().to_vec();

        let rom = smoke_synth_cart(&arm9_bytes, 0x0200_0000, &arm7_bytes, 0x0380_0000);
        let mut nds = Nds::new();
        nds.load_rom(&rom);
        assert!(nds.fault.is_none(), "no fault before running");

        // Install a broken UND handler at the ARM9 high vector (0xFFFF_0004):
        // another UDF. Taking the undefined-instruction exception jumps here,
        // which is *itself* undefined, so the CPU re-faults immediately — the
        // canonical unhandled-exception fault loop. (The handler never returns;
        // it just keeps re-entering the vector, tens of thousands of times per
        // frame.)
        nds.write32_arm9(0xFFFF_0004, 0xE7F0_00F0);

        // One frame is enough to rack up the exception storm.
        nds.run_frame();

        let fault = nds.fault.expect("undef storm must set a fault");
        assert_eq!(
            fault.code,
            crate::cpu::exec::exc::UNDEF,
            "captured cause should be the undefined-instruction exception"
        );

        // The top framebuffer must hold the crash panel: every pixel is either
        // the dark-blue background or white text — and at least some are the
        // white text (proving glyphs were drawn, not just a cleared screen).
        let fb = nds.top_framebuffer();
        assert_eq!(fb.len(), 256 * 192 * 4);
        let bg = [0x10u8, 0x10, 0x60, 0xFF];
        let fg = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut text_pixels = 0usize;
        for px in fb.chunks_exact(4) {
            assert!(
                px == bg || px == fg,
                "crash framebuffer pixels must be bg or fg, got {px:?}"
            );
            if px == fg {
                text_pixels += 1;
            }
        }
        assert!(text_pixels > 0, "crash screen must have drawn white text");

        // Re-running stays faulted (both CPUs frozen) and keeps presenting it.
        nds.run_frame();
        assert!(nds.fault.is_some(), "fault is sticky once set");
    }
}
