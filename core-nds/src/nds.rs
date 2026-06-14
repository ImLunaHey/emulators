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

use crate::cpu::{Cp15, CpuState};
use crate::memory::bus7::Resolved as Resolved7;
use crate::memory::bus9::Resolved as Resolved9;
use crate::memory::{Bus7, Bus9, SharedMemory, VramRouter};

/// Which CPU a bus access is for. The two cores see different memory maps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Core {
    Arm9,
    Arm7,
}

pub struct Nds {
    /// Single backing copy of every block both CPUs can touch.
    pub mem: SharedMemory,

    /// ARM9 bus state (TCMs + their CP15 config).
    pub bus9: Bus9,
    /// ARM7 bus state (touch-struct HLE flags).
    pub bus7: Bus7,

    /// VRAM bank router. Stateless w.r.t. VRAMCNT — those registers live on
    /// `vramcnt` here for now (they belong to the PPU once it lands).
    pub vram: VramRouter,
    /// VRAMCNT_A..I (bank-control registers). Placeholder owner until the PPU
    /// module is ported; the bus routing reads these on every VRAM access.
    pub vramcnt: [u8; 9],

    /// ARM9 register file (ARMv5TE). Shares the `CpuState` type with ARM7.
    pub state9: CpuState,
    /// ARM7 register file (ARMv4T).
    pub state7: CpuState,

    /// ARM9 CP15 system-control coprocessor (caches/MPU/TCM config).
    pub cp15: Cp15,
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
            vramcnt: [0; 9],
            state9: CpuState::new(),
            state7: CpuState::new(),
            cp15: Cp15::new(),
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
            .resolve(addr, for_write, &mut self.mem, &self.vram, &self.vramcnt)
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
            .resolve(addr, &mut self.mem, &self.vram, &self.vramcnt)
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

    // ─── IO / WiFi routing (deferred to the unported subsystems) ─────────
    //
    // These are the seams the IO module will fill. Until then any access that
    // resolves to the IO/WiFi windows lands here.

    fn io_read9(&mut self, _addr: u32, _size: u8) -> u32 {
        todo!("port from ds-recomp src/io/io.ts (ARM9 IO read dispatch)")
    }
    fn io_write9(&mut self, _addr: u32, _v: u32, _size: u8) {
        todo!("port from ds-recomp src/io/io.ts (ARM9 IO write dispatch)")
    }
    fn io_read7(&mut self, _addr: u32, _size: u8) -> u32 {
        todo!("port from ds-recomp src/io/io.ts (ARM7 IO read dispatch)")
    }
    fn io_write7(&mut self, _addr: u32, _v: u32, _size: u8) {
        todo!("port from ds-recomp src/io/io.ts (ARM7 IO write dispatch)")
    }
    fn wifi_read7(&mut self, _addr: u32, _size: u8) -> u32 {
        todo!("port from ds-recomp src/io/wifi.ts (ARM7 WiFi MMIO read)")
    }
    fn wifi_write7(&mut self, _addr: u32, _v: u32, _size: u8) {
        todo!("port from ds-recomp src/io/wifi.ts (ARM7 WiFi MMIO write)")
    }

    // ─── Frame loop (deferred — needs CPU exec + every subsystem) ────────
    pub fn run_frame(&mut self) {
        todo!("port from ds-recomp src/emulator.ts (needs CPU exec + subsystems)")
    }

    /// Load a `.nds` cartridge image (deferred — needs the cart loader).
    pub fn load_rom(&mut self, _bytes: &[u8]) {
        todo!("port from ds-recomp src/cart/loader.ts")
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
        nds.vramcnt[0] = 0x80; // bank A enabled, MST=0 (LCDC)
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
}
