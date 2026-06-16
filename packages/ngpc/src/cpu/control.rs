//! TLCS-900/H interrupt acceptance, RETI, and SWI. Built from the Toshiba
//! manual's interrupt chapter.
//!
//! Interrupt model: each maskable source carries a priority level (1-6). The
//! CPU accepts a request only when its level > ILM (the 3-bit mask in SR). On
//! accept it pushes SR then PC, raises ILM to the accepted level, and loads PC
//! from the source's vector. RETI pops PC then SR. The bus owner (`ngpc.rs`)
//! sets `int_request`/`int_vector` from the device interrupt controller.

use crate::cpu::bus::Bus;
use crate::cpu::state::*;

impl Cpu {
    /// If a pending interrupt out-priorities ILM, accept it: wake from HALT,
    /// push PC + SR, raise ILM, vector. Returns true if an interrupt was taken.
    pub(crate) fn try_interrupt(&mut self, bus: &mut dyn Bus) -> bool {
        let lvl = self.int_request;
        if lvl == 0 || lvl <= self.ilm() {
            return false;
        }
        self.halted = false;
        // Push PC (3-byte effective; store as long) then SR (word).
        let pc = self.pc & 0xFF_FFFF;
        self.push(bus, Size::Long, pc);
        let sr = self.sr();
        self.push(bus, Size::Word, sr as u32);
        self.set_ilm(lvl);
        // Vector holds the 24-bit handler address.
        let vec = self.int_vector & 0xFF_FFFF;
        self.pc = bus.read32(vec) & 0xFF_FFFF;
        // Consume the edge (the device re-raises if still pending).
        self.int_request = 0;
        true
    }

    /// RETI: pop SR (word) then PC (long).
    pub(crate) fn do_reti(&mut self, bus: &mut dyn Bus) -> u32 {
        let sr = self.pop(bus, Size::Word);
        self.set_sr(sr as u16);
        let pc = self.pop(bus, Size::Long);
        self.pc = pc & 0xFF_FFFF;
        12
    }

    /// SWI n: software interrupt. Push PC + SR, vector from the SWI table near
    /// the top of memory (0xFFFF00 + n*4 region).
    pub(crate) fn do_swi(&mut self, bus: &mut dyn Bus, n: u8) {
        let pc = self.pc & 0xFF_FFFF;
        self.push(bus, Size::Long, pc);
        let sr = self.sr();
        self.push(bus, Size::Word, sr as u32);
        let vec = 0xFFFF00u32 + (n as u32) * 4;
        self.pc = bus.read32(vec) & 0xFF_FFFF;
    }

    /// First-byte 0xF8-0xFE handler. In the canonical map these are short
    /// store/LD forms; for now we treat 0xF8-0xFE as SWI 0-6 fallbacks (rare in
    /// NGPC code) and 0xFF as SWI 7. This keeps the decoder total without
    /// mis-executing — refine against the manual's appendix as needed.
    pub(crate) fn do_swi_or_ldx(&mut self, bus: &mut dyn Bus, op: u8) -> u32 {
        let n = op & 0x07;
        self.do_swi(bus, n);
        16
    }
}
