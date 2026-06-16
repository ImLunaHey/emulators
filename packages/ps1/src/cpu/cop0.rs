//! COP0 (System Control Coprocessor) — register file + exception entry.
//!
//! Built from psx-spx "CPU Specifications / Coprocessor 0". COP0 holds the
//! exception/interrupt state for the R3000A: the Status Register (SR, r12),
//! the CAUSE register (r13), the Exception Program Counter (EPC, r14), the
//! Bad Virtual Address (BadVaddr, r8) and the Processor Revision Id (PRId,
//! r15). The handful of debug registers (BPC/BDA/DCIC/…) are stored in a flat
//! array so MTC0/MFC0 can round-trip them, but they have no behavior here.

// ---- COP0 register indices (psx-spx COP0 register table) ----
pub const R_BPC: usize = 3; // Breakpoint Program Counter
pub const R_BDA: usize = 5; // Breakpoint Data Address
pub const R_TAR: usize = 6; // Target Address (JUMPDEST)
pub const R_DCIC: usize = 7; // Debug & Cache Invalidate Control
pub const R_BADVADDR: usize = 8; // Bad Virtual Address
pub const R_BDAM: usize = 9; // Breakpoint Data Address Mask
pub const R_BPCM: usize = 11; // Breakpoint Program Counter Mask
pub const R_SR: usize = 12; // System Status Register
pub const R_CAUSE: usize = 13; // Exception Cause
pub const R_EPC: usize = 14; // Return address from trap
pub const R_PRID: usize = 15; // Processor Revision Identifier

// ---- Status Register (SR / r12) bit fields ----
pub const SR_IEC: u32 = 1 << 0; // Current Interrupt Enable
pub const SR_KUC: u32 = 1 << 1; // Current Kernel/User mode (1=user)
pub const SR_IEP: u32 = 1 << 2; // Previous Interrupt Enable
pub const SR_KUP: u32 = 1 << 3; // Previous Kernel/User mode
pub const SR_IEO: u32 = 1 << 4; // Old Interrupt Enable
pub const SR_KUO: u32 = 1 << 5; // Old Kernel/User mode
pub const SR_IM: u32 = 0xFF << 8; // 8-bit interrupt mask (Im0..Im7)
pub const SR_ISC: u32 = 1 << 16; // Isolate Cache: stores hit cache, not RAM
pub const SR_SWC: u32 = 1 << 17; // Swap Caches
pub const SR_BEV: u32 = 1 << 22; // Boot Exception Vectors (0=RAM/KSEG0,1=ROM)
pub const SR_CU0: u32 = 1 << 28; // COP0 enable
pub const SR_CU2: u32 = 1 << 30; // COP2 (GTE) enable

/// The low 6 bits of SR form a 3-deep stack of (KU, IE) pairs:
/// current(IEc/KUc) / previous(IEp/KUp) / old(IEo/KUo).
const SR_MODE_STACK: u32 = 0x3F;

// ---- CAUSE register (r13) layout ----
pub const CAUSE_EXCCODE_SHIFT: u32 = 2;
pub const CAUSE_EXCCODE_MASK: u32 = 0x1F << CAUSE_EXCCODE_SHIFT; // bits 2..6
pub const CAUSE_IP_SHIFT: u32 = 8; // software/hardware interrupt-pending field
pub const CAUSE_IP_MASK: u32 = 0xFF << CAUSE_IP_SHIFT; // bits 8..15
pub const CAUSE_CE_SHIFT: u32 = 28; // coprocessor number for CpU exceptions
pub const CAUSE_BD: u32 = 1 << 31; // EPC points at a branch (delay slot)

/// PRId for the PSX's R3000A (CXD8530, revision 2). Value per psx-spx.
pub const PRID_R3000A: u32 = 0x0000_0002;

// ---- exception vectors (psx-spx) ----
/// General exception vector when BEV=0 (RAM / KSEG0).
pub const VECTOR_RAM: u32 = 0x8000_0080;
/// General exception vector when BEV=1 (ROM / KSEG1, i.e. the BIOS).
pub const VECTOR_ROM: u32 = 0xBFC0_0180;

/// R3000A exception cause codes (CAUSE.Excode, psx-spx). Closed enum +
/// exhaustive match per the project's idioms — no catch-all integer codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Exception {
    /// External interrupt (hardware/software IRQ line).
    Interrupt = 0x00,
    /// Address error on load or instruction fetch (misaligned / bad segment).
    AddressErrorLoad = 0x04,
    /// Address error on store.
    AddressErrorStore = 0x05,
    /// Bus error on instruction fetch.
    BusErrorInstruction = 0x06,
    /// Bus error on data load/store.
    BusErrorData = 0x07,
    /// `syscall` instruction.
    Syscall = 0x08,
    /// `break` instruction.
    Breakpoint = 0x09,
    /// Reserved / illegal instruction.
    ReservedInstruction = 0x0A,
    /// Coprocessor unusable (CU bit clear for the referenced COPn).
    CoprocessorUnusable = 0x0B,
    /// Arithmetic overflow (ADD/ADDI/SUB).
    Overflow = 0x0C,
}

impl Exception {
    #[inline]
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// COP0 register file + the exception-entry helper.
pub struct Cop0 {
    /// Status Register (r12).
    pub sr: u32,
    /// Cause of last exception (r13).
    pub cause: u32,
    /// Exception PC (r14).
    pub epc: u32,
    /// Bad virtual address (r8) — set on AddressError / bus-error exceptions.
    pub bad_vaddr: u32,
    /// Processor revision id (r15) — read-only constant.
    pub prid: u32,
    /// Backing store for the debug registers (BPC/BDA/TAR/DCIC/BDAM/BPCM).
    /// Indexed by COP0 register number; only the debug slots are meaningful.
    pub dbg: [u32; 16],
}

impl Default for Cop0 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cop0 {
    pub fn new() -> Self {
        Cop0 {
            // At reset BEV=1 so exceptions vector into the BIOS ROM; everything
            // else clear (interrupts disabled, kernel mode, cache not isolated).
            sr: SR_BEV,
            cause: 0,
            epc: 0,
            bad_vaddr: 0,
            prid: PRID_R3000A,
            dbg: [0; 16],
        }
    }

    /// Cache-isolation bit: when set, stores hit the (scratch) cache instead of
    /// main RAM. The bus consults this on every write.
    #[inline]
    pub fn cache_isolated(&self) -> bool {
        (self.sr & SR_ISC) != 0
    }

    /// BEV bit: selects the exception vector base (ROM vs RAM).
    #[inline]
    pub fn bev(&self) -> bool {
        (self.sr & SR_BEV) != 0
    }

    /// COP0 register read (MFC0). Returns the live SR/CAUSE/EPC/BadVaddr/PRId,
    /// or the stored debug-register value, by COP0 register number.
    pub fn read(&self, reg: usize) -> u32 {
        match reg {
            R_SR => self.sr,
            R_CAUSE => self.cause,
            R_EPC => self.epc,
            R_BADVADDR => self.bad_vaddr,
            R_PRID => self.prid,
            R_BPC | R_BDA | R_TAR | R_DCIC | R_BDAM | R_BPCM => self.dbg[reg],
            _ => 0,
        }
    }

    /// COP0 register write (MTC0). EPC/BadVaddr/PRId/CAUSE are largely
    /// hardware-maintained; CAUSE only lets software touch the soft-interrupt
    /// bits (8..9), and PRId is read-only.
    pub fn write(&mut self, reg: usize, v: u32) {
        match reg {
            R_SR => self.sr = v,
            R_CAUSE => {
                // Only the two software-interrupt bits (Sw0/Sw1) are writable.
                self.cause = (self.cause & !0x0000_0300) | (v & 0x0000_0300);
            }
            R_EPC => self.epc = v,
            R_BADVADDR => self.bad_vaddr = v,
            R_PRID => {} // read-only
            R_BPC | R_BDA | R_TAR | R_DCIC | R_BDAM | R_BPCM => self.dbg[reg] = v,
            _ => {}
        }
    }

    /// Enter an exception. Sets CAUSE (Excode + BD), saves the return address
    /// in EPC, pushes the SR mode/interrupt stack (entering kernel mode with
    /// interrupts disabled) and returns the handler vector (BEV-selected).
    ///
    /// `pc` is the address of the faulting instruction; if it sits in a branch
    /// delay slot, pass `in_delay = true` so EPC points at the branch and
    /// CAUSE.BD is set (the handler must re-execute the branch on return).
    #[must_use]
    pub fn enter_exception(&mut self, cause: Exception, pc: u32, in_delay: bool) -> u32 {
        // EPC: the delay-slot case points one instruction earlier (at the
        // branch) so RFE/return re-runs the branch.
        let epc = if in_delay { pc.wrapping_sub(4) } else { pc };
        self.epc = epc;

        // CAUSE: write the Excode, set/clear BD, leave IP (interrupt-pending)
        // intact — it's driven by the hardware IRQ lines, not by entry.
        let mut new_cause = self.cause & !CAUSE_EXCCODE_MASK;
        new_cause |= cause.code() << CAUSE_EXCCODE_SHIFT;
        if in_delay {
            new_cause |= CAUSE_BD;
        } else {
            new_cause &= !CAUSE_BD;
        }
        self.cause = new_cause;

        // Push the (KU,IE) stack: shift current->previous->old left by 2 bits,
        // and clear the new "current" pair (kernel mode, interrupts disabled).
        let mode = self.sr & SR_MODE_STACK;
        let pushed = (mode << 2) & SR_MODE_STACK;
        self.sr = (self.sr & !SR_MODE_STACK) | pushed;

        // Vector base selected by BEV.
        if self.bev() {
            VECTOR_ROM
        } else {
            VECTOR_RAM
        }
    }

    /// RFE (Return From Exception): pop the (KU,IE) stack — copy
    /// previous->current and old->previous, leaving "old" unchanged (this is
    /// the documented R3000A behavior; the top pair is duplicated).
    pub fn return_from_exception(&mut self) {
        let mode = self.sr & SR_MODE_STACK;
        // current <- previous, previous <- old, old stays.
        let popped = (mode >> 2) | (mode & (SR_KUO | SR_IEO));
        self.sr = (self.sr & !SR_MODE_STACK) | (popped & SR_MODE_STACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_isolation_bit() {
        let mut c = Cop0::new();
        assert!(!c.cache_isolated());
        c.write(R_SR, c.sr | SR_ISC);
        assert!(c.cache_isolated());
    }

    #[test]
    fn enter_exception_sets_cause_epc_and_vector() {
        let mut c = Cop0::new(); // BEV=1 at reset
        let vec = c.enter_exception(Exception::Syscall, 0x8000_1000, false);
        assert_eq!(vec, VECTOR_ROM);
        assert_eq!(c.epc, 0x8000_1000);
        assert_eq!((c.cause & CAUSE_EXCCODE_MASK) >> CAUSE_EXCCODE_SHIFT, 0x08);
        assert_eq!(c.cause & CAUSE_BD, 0);
    }

    #[test]
    fn delay_slot_exception_sets_bd_and_backs_up_epc() {
        let mut c = Cop0::new();
        let _ = c.enter_exception(Exception::Overflow, 0x8000_1004, true);
        assert_eq!(c.epc, 0x8000_1000); // points at the branch
        assert_ne!(c.cause & CAUSE_BD, 0);
    }

    #[test]
    fn ram_vector_when_bev_clear() {
        let mut c = Cop0::new();
        c.write(R_SR, c.sr & !SR_BEV);
        let vec = c.enter_exception(Exception::Interrupt, 0x100, false);
        assert_eq!(vec, VECTOR_RAM);
    }

    #[test]
    fn exception_push_pop_mode_stack() {
        let mut c = Cop0::new();
        // current IEc=1, KUc=1 (user, ints on).
        c.write(R_SR, (c.sr & !SR_MODE_STACK) | SR_IEC | SR_KUC);
        let _ = c.enter_exception(Exception::Syscall, 0, false);
        // After entry: current pair cleared (kernel, ints off); previous holds
        // the old current.
        assert_eq!(c.sr & SR_IEC, 0);
        assert_eq!(c.sr & SR_KUC, 0);
        assert_ne!(c.sr & SR_IEP, 0);
        assert_ne!(c.sr & SR_KUP, 0);
        c.return_from_exception();
        // After RFE: current restored from previous.
        assert_ne!(c.sr & SR_IEC, 0);
        assert_ne!(c.sr & SR_KUC, 0);
    }
}
