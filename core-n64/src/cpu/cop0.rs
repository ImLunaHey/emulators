//! COP0 (System Control Coprocessor) for the NEC VR4300 (MIPS R4300i).
//!
//! Built from the VR4300 user manual ("Exception Processing" / "System
//! Control Coprocessor") and the n64brew wiki. COP0 owns the exception and
//! interrupt machinery, the Count/Compare timer, and the 32-entry TLB.
//!
//! Differences from the R3000A COP0 (see the PS1 core) that matter here:
//!
//! * **64-bit registers.** EPC, BadVAddr, the TLB EntryHi/EntryLo pairs and
//!   the Context registers are 64-bit. Status/Cause are 32-bit.
//! * **Status.EXL.** The R4300 gates exception entry on the EXL bit rather
//!   than a 3-deep mode stack: on entry it sets EXL (masking further
//!   interrupts and forcing kernel mode); `ERET` clears it. Status still keeps
//!   the IE/EXL/ERL flags but there is no KU/IE shift-stack.
//! * **Count/Compare.** Count increments every other PInst cycle; when it
//!   equals Compare the timer interrupt (IP7) is asserted until Compare is
//!   written. We model this in [`Cop0::tick`].
//! * **Vector layout.** The general exception vector is 0x80000180 (BEV=0) /
//!   0xBFC00380 (BEV=1); TLB-refill uses 0x80000000 / 0xBFC00200; the reset/
//!   NMI vector is 0xBFC00000.

// ---- COP0 register numbers (rd field of MFC0/MTC0) ----
pub const R_INDEX: usize = 0;
pub const R_RANDOM: usize = 1;
pub const R_ENTRYLO0: usize = 2;
pub const R_ENTRYLO1: usize = 3;
pub const R_CONTEXT: usize = 4;
pub const R_PAGEMASK: usize = 5;
pub const R_WIRED: usize = 6;
pub const R_BADVADDR: usize = 8;
pub const R_COUNT: usize = 9;
pub const R_ENTRYHI: usize = 10;
pub const R_COMPARE: usize = 11;
pub const R_STATUS: usize = 12;
pub const R_CAUSE: usize = 13;
pub const R_EPC: usize = 14;
pub const R_PRID: usize = 15;
pub const R_CONFIG: usize = 16;
pub const R_LLADDR: usize = 17;
pub const R_WATCHLO: usize = 18;
pub const R_WATCHHI: usize = 19;
pub const R_XCONTEXT: usize = 20;
pub const R_TAGLO: usize = 28;
pub const R_TAGHI: usize = 29;
pub const R_ERROREPC: usize = 30;

// ---- Status register (r12) bit fields ----
pub const ST_IE: u32 = 1 << 0; // global interrupt enable
pub const ST_EXL: u32 = 1 << 1; // exception level
pub const ST_ERL: u32 = 1 << 2; // error level
pub const ST_IM_SHIFT: u32 = 8; // 8-bit interrupt mask (IM0..IM7)
pub const ST_IM_MASK: u32 = 0xFF << ST_IM_SHIFT;
pub const ST_BEV: u32 = 1 << 22; // boot exception vectors

// ---- Cause register (r13) layout ----
pub const CAUSE_EXCCODE_SHIFT: u32 = 2;
pub const CAUSE_EXCCODE_MASK: u32 = 0x1F << CAUSE_EXCCODE_SHIFT;
pub const CAUSE_IP_SHIFT: u32 = 8;
pub const CAUSE_IP_MASK: u32 = 0xFF << CAUSE_IP_SHIFT;
pub const CAUSE_CE_SHIFT: u32 = 28; // coprocessor unit for CpU exceptions
pub const CAUSE_BD: u32 = 1 << 31; // exception taken in a branch delay slot

/// Cause.IP bit for the MI (RCP) interrupt line — the only external hardware
/// interrupt the N64 routes to the CPU (everything funnels through MI -> IP2).
pub const IP_RCP: u32 = 1 << (CAUSE_IP_SHIFT + 2); // IP2
/// Cause.IP bit for the on-core Count/Compare timer (IP7).
pub const IP_TIMER: u32 = 1 << (CAUSE_IP_SHIFT + 7); // IP7

/// PRId for the VR4300 (revision 0x22 per the n64brew wiki).
pub const PRID_VR4300: u32 = 0x0000_0B22;

// ---- exception vectors ----
pub const VEC_RESET: u32 = 0xBFC0_0000;
pub const VEC_TLB_REFILL_BEV0: u32 = 0x8000_0000;
pub const VEC_TLB_REFILL_BEV1: u32 = 0xBFC0_0200;
pub const VEC_GENERAL_BEV0: u32 = 0x8000_0180;
pub const VEC_GENERAL_BEV1: u32 = 0xBFC0_0380;

/// VR4300 exception cause codes (Cause.ExcCode). Closed enum + exhaustive
/// match per the project's idioms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Exception {
    Interrupt = 0,
    TlbModification = 1,
    TlbLoad = 2,
    TlbStore = 3,
    AddressErrorLoad = 4,
    AddressErrorStore = 5,
    BusErrorInstruction = 6,
    BusErrorData = 7,
    Syscall = 8,
    Breakpoint = 9,
    ReservedInstruction = 10,
    CoprocessorUnusable = 11,
    Overflow = 12,
    Trap = 13,
    FloatingPoint = 15,
}

impl Exception {
    #[inline]
    pub fn code(self) -> u32 {
        self as u32
    }

    /// TLB-refill (TLBL/TLBS) exceptions use the dedicated refill vector when
    /// EXL is clear; everything else uses the general vector.
    #[inline]
    fn is_tlb_refill(self) -> bool {
        matches!(self, Exception::TlbLoad | Exception::TlbStore)
    }
}

/// A single TLB entry (one of 32). Pairs map an even/odd virtual page to two
/// physical frames. We store the raw EntryHi/EntryLo0/EntryLo1/PageMask so
/// TLBR/TLBWI round-trip them; a full software-managed lookup is provided.
#[derive(Debug, Clone, Copy, Default)]
pub struct TlbEntry {
    pub entry_hi: u64,
    pub entry_lo0: u64,
    pub entry_lo1: u64,
    pub page_mask: u32,
}

/// COP0 register file + exception entry + the TLB + the Count/Compare timer.
pub struct Cop0 {
    /// 32 raw COP0 registers (only the architected ones are meaningful). The
    /// 64-bit ones (EPC, BadVAddr, Context, EntryHi, EntryLo*, ErrorEPC) live
    /// in the wide table; Status/Cause/Count/Compare/Index/etc. use the low 32.
    pub reg: [u64; 32],

    /// The 32-entry TLB.
    pub tlb: [TlbEntry; 32],

    /// Total exceptions taken since reset — not architectural. The host watches
    /// its rate to detect a fault loop and surface a crash screen.
    pub exceptions: u64,
}

impl Default for Cop0 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cop0 {
    pub fn new() -> Self {
        let mut reg = [0u64; 32];
        // Reset state per the VR4300 manual: BEV set (exceptions vector to ROM),
        // ERL set (we're in the reset error state until IPL clears it), and the
        // PRId / Config constants populated.
        reg[R_STATUS] = (ST_BEV | ST_ERL) as u64;
        reg[R_PRID] = PRID_VR4300 as u64;
        // Config: little bit of plausible boilerplate (BE=1 big-endian, KSEG0
        // cached). Games rarely read it; we just keep MTC0/MFC0 honest.
        reg[R_CONFIG] = 0x7006_E463;
        reg[R_RANDOM] = 31; // Random counts down from 31 to Wired
        Self {
            reg,
            tlb: [TlbEntry::default(); 32],
            exceptions: 0,
        }
    }

    // ---- typed accessors for the hot registers ----
    #[inline]
    pub fn status(&self) -> u32 {
        self.reg[R_STATUS] as u32
    }
    #[inline]
    pub fn set_status(&mut self, v: u32) {
        self.reg[R_STATUS] = v as u64;
    }
    #[inline]
    pub fn cause(&self) -> u32 {
        self.reg[R_CAUSE] as u32
    }
    #[inline]
    pub fn set_cause(&mut self, v: u32) {
        self.reg[R_CAUSE] = v as u64;
    }
    #[inline]
    pub fn epc(&self) -> u64 {
        self.reg[R_EPC]
    }
    #[inline]
    pub fn bev(&self) -> bool {
        self.status() & ST_BEV != 0
    }

    /// MFC0: read a COP0 register, sign-extended from 32 bits for the 32-bit
    /// registers; the wide ones return their full 64-bit value (DMFC0 callers).
    pub fn read(&self, reg: usize) -> u64 {
        match reg {
            // 64-bit-wide registers.
            R_BADVADDR | R_CONTEXT | R_XCONTEXT | R_ENTRYHI | R_ENTRYLO0 | R_ENTRYLO1 | R_EPC
            | R_ERROREPC | R_LLADDR => self.reg[reg],
            // 32-bit registers, returned as the raw stored value.
            _ => self.reg[reg] & 0xFFFF_FFFF,
        }
    }

    /// MTC0: write a COP0 register. Read-only fields (PRId, the upper Cause
    /// bits) are masked; writing Compare clears the pending timer interrupt.
    pub fn write(&mut self, reg: usize, v: u64) {
        match reg {
            R_PRID => {} // read-only
            R_RANDOM => {} // read-only (hardware-decremented)
            R_CAUSE => {
                // Only the two software-interrupt bits (IP0/IP1) are writable.
                let cause = self.cause();
                self.set_cause((cause & !0x0000_0300) | (v as u32 & 0x0000_0300));
            }
            R_COMPARE => {
                self.reg[R_COMPARE] = v & 0xFFFF_FFFF;
                // Writing Compare acknowledges the timer interrupt.
                let cause = self.cause() & !IP_TIMER;
                self.set_cause(cause);
            }
            R_COUNT => self.reg[R_COUNT] = v & 0xFFFF_FFFF,
            R_BADVADDR | R_CONTEXT | R_XCONTEXT | R_ENTRYHI | R_ENTRYLO0 | R_ENTRYLO1 | R_EPC
            | R_ERROREPC | R_LLADDR => self.reg[reg] = v,
            _ => self.reg[reg] = v & 0xFFFF_FFFF,
        }
    }

    /// Advance the Count/Compare timer by `cycles`. Count increments at half
    /// the CPU clock; when it matches Compare the IP7 timer interrupt latches.
    /// Returns true if the timer interrupt is (now) pending.
    pub fn tick(&mut self, cycles: u64) -> bool {
        let old = self.reg[R_COUNT];
        let new = (old + cycles) & 0xFFFF_FFFF;
        self.reg[R_COUNT] = new;
        let compare = self.reg[R_COMPARE];
        // Detect Count crossing Compare during this step.
        let crossed = if new >= old {
            old <= compare && compare < new || (new == compare)
        } else {
            // wrapped
            compare >= old || compare < new
        };
        if crossed && compare != 0 {
            let cause = self.cause() | IP_TIMER;
            self.set_cause(cause);
        }
        self.cause() & IP_TIMER != 0
    }

    /// Set or clear the MI (RCP) interrupt line (Cause.IP2).
    pub fn set_rcp_interrupt(&mut self, asserted: bool) {
        let mut cause = self.cause();
        if asserted {
            cause |= IP_RCP;
        } else {
            cause &= !IP_RCP;
        }
        self.set_cause(cause);
    }

    /// True if an interrupt should be taken right now: global IE set, not
    /// already in an exception/error level, and an unmasked IP bit pending.
    pub fn interrupt_pending(&self) -> bool {
        let st = self.status();
        if st & ST_IE == 0 || st & ST_EXL != 0 || st & ST_ERL != 0 {
            return false;
        }
        let pending = self.cause() & CAUSE_IP_MASK;
        let mask = st & ST_IM_MASK;
        pending & mask != 0
    }

    /// Enter an exception. Sets Cause (ExcCode + BD + CE), saves the return PC
    /// in EPC, sets Status.EXL, and returns the handler vector. `pc` is the
    /// address of the faulting instruction; `in_delay` backs EPC up to the
    /// branch and sets Cause.BD.
    #[must_use]
    pub fn enter_exception(
        &mut self,
        cause: Exception,
        pc: u64,
        in_delay: bool,
        coprocessor: u32,
    ) -> u32 {
        self.exceptions = self.exceptions.wrapping_add(1);
        let already_exl = self.status() & ST_EXL != 0;

        // EPC and Cause.BD are only updated if we are not already at exception
        // level (a nested exception leaves the original EPC intact).
        if !already_exl {
            let epc = if in_delay { pc.wrapping_sub(4) } else { pc };
            self.reg[R_EPC] = epc;
        }

        let mut c = self.cause() & !(CAUSE_EXCCODE_MASK | (3 << CAUSE_CE_SHIFT));
        c |= cause.code() << CAUSE_EXCCODE_SHIFT;
        c |= (coprocessor & 3) << CAUSE_CE_SHIFT;
        if !already_exl {
            if in_delay {
                c |= CAUSE_BD;
            } else {
                c &= !CAUSE_BD;
            }
        }
        self.set_cause(c);

        // Set EXL (enter kernel mode, mask interrupts).
        let st = self.status() | ST_EXL;
        self.set_status(st);

        // Vector selection: TLB-refill uses its own base when EXL was clear.
        let bev = self.bev();
        if cause.is_tlb_refill() && !already_exl {
            if bev {
                VEC_TLB_REFILL_BEV1
            } else {
                VEC_TLB_REFILL_BEV0
            }
        } else if bev {
            VEC_GENERAL_BEV1
        } else {
            VEC_GENERAL_BEV0
        }
    }

    /// ERET: return from exception. Clears EXL (or ERL if it was set) and
    /// returns the PC to resume at (EPC, or ErrorEPC if ERL was set).
    #[must_use]
    pub fn eret(&mut self) -> u64 {
        let st = self.status();
        if st & ST_ERL != 0 {
            self.set_status(st & !ST_ERL);
            self.reg[R_ERROREPC]
        } else {
            self.set_status(st & !ST_EXL);
            self.reg[R_EPC]
        }
    }

    /// Software-managed TLB lookup: find an entry whose VPN2 matches `vaddr`'s
    /// (and ASID, when the global bit is clear). Returns the translated 32-bit
    /// physical address, or `None` (TLB miss). Foundation-level: assumes 4 KB
    /// pages (PageMask = 0) which covers the common case.
    pub fn tlb_translate(&self, vaddr: u32) -> Option<u32> {
        let vpn2 = (vaddr >> 13) as u64;
        let asid = self.reg[R_ENTRYHI] & 0xFF;
        for e in &self.tlb {
            let e_vpn2 = (e.entry_hi >> 13) & 0x7FF_FFFF;
            if e_vpn2 != vpn2 {
                continue;
            }
            let global = (e.entry_lo0 & e.entry_lo1 & 1) != 0;
            let e_asid = e.entry_hi & 0xFF;
            if !global && e_asid != asid {
                continue;
            }
            // Even/odd page selected by bit 12 of the virtual address.
            let lo = if vaddr & 0x1000 == 0 {
                e.entry_lo0
            } else {
                e.entry_lo1
            };
            if lo & 0b10 == 0 {
                return None; // V (valid) bit clear
            }
            let pfn = ((lo >> 6) & 0xF_FFFF) as u32;
            return Some((pfn << 12) | (vaddr & 0xFFF));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_state_has_bev_and_erl() {
        let c = Cop0::new();
        assert!(c.status() & ST_BEV != 0);
        assert!(c.status() & ST_ERL != 0);
        assert_eq!(c.read(R_PRID) as u32, PRID_VR4300);
    }

    #[test]
    fn enter_exception_sets_exl_epc_cause_and_general_vector() {
        let mut c = Cop0::new();
        c.set_status(ST_IE); // clear BEV/ERL/EXL for a clean general-vector test
        let vec = c.enter_exception(Exception::Syscall, 0x8000_1000, false, 0);
        assert_eq!(vec, VEC_GENERAL_BEV0);
        assert_eq!(c.epc(), 0x8000_1000);
        assert_eq!((c.cause() & CAUSE_EXCCODE_MASK) >> CAUSE_EXCCODE_SHIFT, 8);
        assert!(c.status() & ST_EXL != 0);
    }

    #[test]
    fn delay_slot_exception_sets_bd_and_backs_up_epc() {
        let mut c = Cop0::new();
        c.set_status(0);
        let _ = c.enter_exception(Exception::Overflow, 0x8000_1004, true, 0);
        assert_eq!(c.epc(), 0x8000_1000);
        assert!(c.cause() & CAUSE_BD != 0);
    }

    #[test]
    fn tlb_refill_uses_dedicated_vector() {
        let mut c = Cop0::new();
        c.set_status(0); // BEV clear, EXL clear
        let vec = c.enter_exception(Exception::TlbLoad, 0x100, false, 0);
        assert_eq!(vec, VEC_TLB_REFILL_BEV0);
    }

    #[test]
    fn nested_exception_keeps_original_epc() {
        let mut c = Cop0::new();
        c.set_status(0);
        let _ = c.enter_exception(Exception::Syscall, 0x1000, false, 0);
        // EXL now set; a second exception must not clobber EPC.
        let _ = c.enter_exception(Exception::Overflow, 0x2000, false, 0);
        assert_eq!(c.epc(), 0x1000);
    }

    #[test]
    fn eret_clears_exl_and_returns_epc() {
        let mut c = Cop0::new();
        c.set_status(ST_IE);
        let _ = c.enter_exception(Exception::Syscall, 0x8000_2000, false, 0);
        let resume = c.eret();
        assert_eq!(resume, 0x8000_2000);
        assert!(c.status() & ST_EXL == 0);
    }

    #[test]
    fn eret_prefers_errorepc_when_erl_set() {
        let mut c = Cop0::new();
        c.set_status(ST_ERL);
        c.write(R_ERROREPC, 0xABCD);
        let resume = c.eret();
        assert_eq!(resume, 0xABCD);
        assert!(c.status() & ST_ERL == 0);
    }

    #[test]
    fn timer_interrupt_latches_when_count_hits_compare() {
        let mut c = Cop0::new();
        c.write(R_COUNT, 0);
        c.write(R_COMPARE, 100);
        assert!(!c.tick(50));
        assert!(c.tick(60)); // crosses 100
        assert!(c.cause() & IP_TIMER != 0);
        // Writing Compare acknowledges it.
        c.write(R_COMPARE, 1000);
        assert!(c.cause() & IP_TIMER == 0);
    }

    #[test]
    fn interrupt_pending_respects_mask_and_exl() {
        let mut c = Cop0::new();
        c.set_status(ST_IE | (IP_RCP & ST_IM_MASK)); // enable, unmask IP2
        c.set_rcp_interrupt(true);
        assert!(c.interrupt_pending());
        // Setting EXL blocks it.
        c.set_status(c.status() | ST_EXL);
        assert!(!c.interrupt_pending());
    }

    #[test]
    fn compare_write_only_keeps_writable_cause_bits() {
        let mut c = Cop0::new();
        c.set_cause(0);
        c.write(R_CAUSE, 0xFFFF_FFFF);
        // Only IP0/IP1 (software interrupts) writable.
        assert_eq!(c.cause(), 0x0000_0300);
    }
}
