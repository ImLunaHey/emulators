//! Gekko special-purpose registers (SPRs) + the exception model.
//!
//! Built from the PowerPC OEA (Operating Environment Architecture) and YAGCD
//! §2.2. PowerPC keeps its supervisor/control state in SPRs accessed via
//! `mfspr`/`mtspr` (rather than a separate coprocessor like MIPS COP0). The ones
//! that matter for the foundation:
//!
//! | SPR # | name | meaning                                              |
//! |-------|------|------------------------------------------------------|
//! | 1     | XER  | fixed-point exception (carry/overflow/byte-count)    |
//! | 8     | LR   | link register (subroutine return address)            |
//! | 9     | CTR  | count register (loop counter / indirect branch)      |
//! | 18/19 | DSISR/DAR | data-access fault status / address               |
//! | 26/27 | SRR0/SRR1 | save/restore on exception (PC & MSR snapshot)   |
//! | 272.. | SPRG0..3  | scratch registers for the OS                     |
//!
//! LR (8) and CTR (9) live directly in [`super::state::Cpu`] because the branch
//! unit touches them constantly; the rest are stored in a flat array so
//! `mfspr`/`mtspr` round-trip them. Real fault behaviour (page faults, the FP
//! unavailable trap, the decrementer) is future work.

// ---- SPR numbers (PowerPC OEA) ----
pub const SPR_XER: u32 = 1;
pub const SPR_LR: u32 = 8;
pub const SPR_CTR: u32 = 9;
pub const SPR_DSISR: u32 = 18;
pub const SPR_DAR: u32 = 19;
pub const SPR_DEC: u32 = 22;
pub const SPR_SRR0: u32 = 26;
pub const SPR_SRR1: u32 = 27;
pub const SPR_SPRG0: u32 = 272;
pub const SPR_SPRG1: u32 = 273;
pub const SPR_SPRG2: u32 = 274;
pub const SPR_SPRG3: u32 = 275;
/// Processor Version Register (read-only). The Gekko's PVR is 0x0008_3214.
pub const SPR_PVR: u32 = 287;

/// Gekko PVR value (YAGCD §2.2). Read-only via `mfspr`.
pub const PVR_GEKKO: u32 = 0x0008_3214;

// ---- XER bit fields (PowerPC, big-endian bit numbering — bit 0 is the MSB) --
/// Summary Overflow (XER[SO], bit 0 ⇒ mask 1<<31).
pub const XER_SO: u32 = 1 << 31;
/// Overflow (XER[OV], bit 1 ⇒ mask 1<<30).
pub const XER_OV: u32 = 1 << 30;
/// Carry (XER[CA], bit 2 ⇒ mask 1<<29).
pub const XER_CA: u32 = 1 << 29;

// ---- MSR (Machine State Register) bit fields (PowerPC OEA; big-endian bit#) --
/// External Interrupt Enable (MSR[EE], bit 16 ⇒ mask 1<<15).
pub const MSR_EE: u32 = 1 << 15;
/// Problem (user) state (MSR[PR], bit 17 ⇒ mask 1<<14). 1 = user.
pub const MSR_PR: u32 = 1 << 14;
/// FP Available (MSR[FP], bit 18 ⇒ mask 1<<13).
pub const MSR_FP: u32 = 1 << 13;
/// Instruction-address translation (MSR[IR], bit 26 ⇒ mask 1<<5).
pub const MSR_IR: u32 = 1 << 5;
/// Data-address translation (MSR[DR], bit 27 ⇒ mask 1<<4).
pub const MSR_DR: u32 = 1 << 4;

/// PowerPC exception vectors (OEA; offsets from the vector base, which is
/// `0xFFF0_0000` at reset when MSR[IP] is set, or `0x0000_0000` otherwise). We
/// model only the handful the foundation can raise. Values are the standard
/// vector offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Exception {
    /// System reset (`0x0100`). Power-on / hard reset entry.
    SystemReset = 0x0100,
    /// Data Storage (`0x0300`) — a data access fault (page/protection).
    DataStorage = 0x0300,
    /// Instruction Storage (`0x0400`) — an instruction-fetch fault.
    InstructionStorage = 0x0400,
    /// External Interrupt (`0x0500`) — a device IRQ via the PI.
    ExternalInterrupt = 0x0500,
    /// Alignment (`0x0600`) — a misaligned access the hardware can't fix up.
    Alignment = 0x0600,
    /// Program (`0x0700`) — illegal/unimplemented instruction, trap, FP
    /// exception. The foundation raises this for unimplemented opcodes.
    Program = 0x0700,
    /// Floating-point Unavailable (`0x0800`) — an FP op with MSR[FP] clear.
    FpUnavailable = 0x0800,
    /// Decrementer (`0x0900`) — the DEC SPR counted down through zero.
    Decrementer = 0x0900,
    /// System Call (`0x0C00`) — the `sc` instruction.
    SystemCall = 0x0C00,
}

impl Exception {
    /// The vector offset (added to the exception-vector base).
    #[inline]
    pub fn offset(self) -> u32 {
        self as u32
    }
}

/// The Gekko SPR file + exception entry. LR/CTR/XER live in [`super::state::Cpu`]
/// (the branch/ALU units hit them every instruction); this struct holds the
/// supervisor/control SPRs and the exception bookkeeping.
pub struct Spr {
    /// Machine State Register — interrupt-enable, translation, privilege.
    pub msr: u32,
    /// Save/Restore Register 0 — PC saved on exception entry (return address).
    pub srr0: u32,
    /// Save/Restore Register 1 — MSR snapshot saved on exception entry.
    pub srr1: u32,
    /// Data Address Register — faulting address of a data-storage exception.
    pub dar: u32,
    /// DSISR — data-storage interrupt status (the fault reason bits).
    pub dsisr: u32,
    /// Decrementer — a free-running down-counter that raises an exception at 0.
    pub dec: u32,
    /// OS scratch registers SPRG0..3.
    pub sprg: [u32; 4],
    /// Processor Version Register (read-only constant).
    pub pvr: u32,
    /// Total exceptions taken since reset (not architectural — the host watches
    /// the rate to detect a fault loop, mirroring the PS1 core).
    pub exceptions: u64,
}

impl Default for Spr {
    fn default() -> Self {
        Self::new()
    }
}

impl Spr {
    pub fn new() -> Self {
        Spr {
            // At reset MSR is essentially cleared (interrupts off, supervisor,
            // translation off). The IPL turns on the BATs and EE itself.
            msr: 0,
            srr0: 0,
            srr1: 0,
            dar: 0,
            dsisr: 0,
            dec: 0,
            sprg: [0; 4],
            pvr: PVR_GEKKO,
            exceptions: 0,
        }
    }

    /// MSR[EE]: external interrupts enabled.
    #[inline]
    pub fn ee(&self) -> bool {
        self.msr & MSR_EE != 0
    }

    /// `mfspr` — read a special-purpose register by SPR number. LR/CTR/XER are
    /// owned by [`super::state::Cpu`], so the executor handles those directly;
    /// this covers the supervisor/control SPRs stored here.
    pub fn read(&self, spr: u32) -> u32 {
        match spr {
            SPR_SRR0 => self.srr0,
            SPR_SRR1 => self.srr1,
            SPR_DAR => self.dar,
            SPR_DSISR => self.dsisr,
            SPR_DEC => self.dec,
            SPR_SPRG0 => self.sprg[0],
            SPR_SPRG1 => self.sprg[1],
            SPR_SPRG2 => self.sprg[2],
            SPR_SPRG3 => self.sprg[3],
            SPR_PVR => self.pvr,
            _ => 0,
        }
    }

    /// `mtspr` — write a special-purpose register. PVR is read-only.
    pub fn write(&mut self, spr: u32, v: u32) {
        match spr {
            SPR_SRR0 => self.srr0 = v,
            SPR_SRR1 => self.srr1 = v,
            SPR_DAR => self.dar = v,
            SPR_DSISR => self.dsisr = v,
            SPR_DEC => self.dec = v,
            SPR_SPRG0 => self.sprg[0] = v,
            SPR_SPRG1 => self.sprg[1] = v,
            SPR_SPRG2 => self.sprg[2] = v,
            SPR_SPRG3 => self.sprg[3] = v,
            SPR_PVR => {} // read-only
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spr_roundtrip_and_pvr_readonly() {
        let mut s = Spr::new();
        s.write(SPR_SRR0, 0x1234_5678);
        assert_eq!(s.read(SPR_SRR0), 0x1234_5678);
        s.write(SPR_PVR, 0); // ignored
        assert_eq!(s.read(SPR_PVR), PVR_GEKKO);
    }

    #[test]
    fn ee_reflects_msr() {
        let mut s = Spr::new();
        assert!(!s.ee());
        s.msr |= MSR_EE;
        assert!(s.ee());
    }

    #[test]
    fn exception_offsets() {
        assert_eq!(Exception::SystemCall.offset(), 0x0C00);
        assert_eq!(Exception::Program.offset(), 0x0700);
    }
}
