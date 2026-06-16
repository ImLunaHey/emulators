//! CP15 system control coprocessor for the ARM9. Ported from
//! ../../ds-recomp/src/cpu/cp15.ts.
//!
//! The DS uses CP15 to configure caches, the MPU (protection regions), and
//! the TCMs. We keep a minimal model: handle the writes the official boot
//! code makes, and update ITCM/DTCM base/size on `Bus9` when they're
//! reconfigured.
//!
//! Real CP15 has dozens of registers. We store everything in a flat map and
//! route the TCM-control writes (CRn=9, opc2=0/1) and the control register
//! (CRn=1) to `Bus9` so the TCM windows move when the kernel re-maps them.
//!
//! Ownership (see CONTRACT.md): unlike the TS `Cp15`, this struct does NOT
//! hold `bus9`/`mem`/`cpu` references. The side-effecting `write` takes a
//! `&mut Bus9`, `&mut SharedMemory`, and `&mut CpuState` as parameters — the
//! collaborators the TS constructor received become method arguments.

use crate::memory::{Bus9, SharedMemory};
use crate::state::CpuState;
use std::collections::HashMap;

#[inline]
fn key(opc1: u32, crn: u32, crm: u32, opc2: u32) -> u32 {
    (opc1 << 16) | (crn << 8) | (crm << 4) | opc2
}

pub struct Cp15 {
    regs: HashMap<u32, u32>,
}

impl Default for Cp15 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cp15 {
    pub fn new() -> Self {
        let mut regs = HashMap::new();
        regs.insert(key(0, 0, 0, 0), 0x4105_9461); // Main ID
        regs.insert(key(0, 1, 0, 0), 0x0F0D_2112); // Cache type
        Cp15 { regs }
    }

    /// Install the BIOS IRQ-handler-pointer literal that the BIOS IRQ stub at
    /// 0x18 reads (offset 0x34) — it holds the ADDRESS of the user IRQ
    /// handler ptr, which DS games store at DTCM_END - 4. So the literal must
    /// move whenever CP15 relocates DTCM. Call after `new()` and whenever the
    /// DTCM base/size changes.
    pub fn update_irq_handler_ptr_literal(&self, bus9: &Bus9, mem: &mut SharedMemory) {
        let dtcm_end = bus9.dtcm_base.wrapping_add(bus9.dtcm_virtual_size);
        let ptr_addr = dtcm_end.wrapping_sub(4);
        let bios = &mut mem.bios_arm9[..];
        bios[0x34] = (ptr_addr & 0xFF) as u8;
        bios[0x35] = ((ptr_addr >> 8) & 0xFF) as u8;
        bios[0x36] = ((ptr_addr >> 16) & 0xFF) as u8;
        bios[0x37] = ((ptr_addr >> 24) & 0xFF) as u8;
    }

    pub fn read(&self, opc1: u32, crn: u32, crm: u32, opc2: u32) -> u32 {
        *self.regs.get(&key(opc1, crn, crm, opc2)).unwrap_or(&0)
    }

    /// Write a CP15 register and apply its side effects to the ARM9 bus and
    /// (for Wait-For-Interrupt) the CPU state.
    pub fn write(
        &mut self,
        opc1: u32,
        crn: u32,
        crm: u32,
        opc2: u32,
        value: u32,
        bus9: &mut Bus9,
        mem: &mut SharedMemory,
        cpu: &mut CpuState,
    ) {
        self.regs.insert(key(opc1, crn, crm, opc2), value);

        // CRn=7 CRm=0 opc2=4 → "Wait For Interrupt". ARM946E-S halts in
        // low-power state until an IRQ becomes pending (regardless of CPSR.I).
        // We model it by setting `halted` and counting on the CPU step's
        // halt-wake check. We ALSO unmask IRQs here: Pokemon Platinum's ARM9
        // idle loop spins DisableIrq → MCR WFI → B back with no CPSR.I clear,
        // and the spin never gets a chance to re-enable on its own, so
        // mirroring the unmask gets the IRQ to fire on the next wake.
        if crn == 7 && crm == 0 && opc2 == 4 {
            cpu.cpsr &= !0x80;
            cpu.halted = true;
        }

        // TCM region/size — CRn=9, CRm=1, opc2=0 (DTCM) or 1 (ITCM). Bits
        // 31:12 = base address, bits 5:1 = size code (virtual size =
        // 512 << code). Physical TCM size is fixed; when virtual > physical
        // the bus mirrors via (addr & (physical-1)).
        if crn == 9 && crm == 1 {
            let base = value & 0xFFFF_F000;
            let size_code = (value >> 1) & 0x1F;
            let virt_size = 512u32 << size_code;
            if opc2 == 0 {
                bus9.dtcm_base = base;
                bus9.dtcm_virtual_size = virt_size;
                self.update_irq_handler_ptr_literal(bus9, mem);
            } else if opc2 == 1 {
                // ITCM ignores the base field on real hardware (always at
                // 0x00000000 from the CPU's perspective), but the size code
                // still matters.
                bus9.itcm_base = 0;
                bus9.itcm_virtual_size = virt_size;
            }
        }

        // Control register CRn=1, CRm=0, opc2=0. Bits 16/18 enable, bits
        // 17/19 = load mode for DTCM/ITCM respectively.
        if crn == 1 && crm == 0 && opc2 == 0 {
            bus9.dtcm_enabled = (value & (1 << 16)) != 0;
            bus9.dtcm_load_mode = (value & (1 << 17)) != 0;
            bus9.itcm_enabled = (value & (1 << 18)) != 0;
            bus9.itcm_load_mode = (value & (1 << 19)) != 0;
        }
    }
}
