//! MI — the MIPS Interface. The RCP's interrupt aggregator and version
//! register. Every RCP subsystem (SP, SI, AI, VI, PI, DP) raises its interrupt
//! into MI's `intr` register; MI ANDs that with `mask` and, if any bit is set,
//! asserts the single CPU interrupt line (Cause.IP2). The CPU's COP0 then takes
//! an Interrupt exception if globally enabled.
//!
//! Built from n64brew "MIPS Interface".

/// MI interrupt-source bits (MI_INTR_REG / MI_INTR_MASK_REG).
pub const INTR_SP: u32 = 1 << 0; // RSP
pub const INTR_SI: u32 = 1 << 1; // Serial (PIF) DMA complete
pub const INTR_AI: u32 = 1 << 2; // Audio
pub const INTR_VI: u32 = 1 << 3; // Video (vertical interrupt)
pub const INTR_PI: u32 = 1 << 4; // Peripheral (cart) DMA complete
pub const INTR_DP: u32 = 1 << 5; // RDP

/// MI register block.
pub struct Mi {
    /// MI_MODE_REG (init mode, EBus test, etc.). Stored, mostly inert here.
    pub mode: u32,
    /// MI_VERSION_REG — fixed silicon revision word.
    pub version: u32,
    /// MI_INTR_REG — pending interrupt sources (set by subsystems, cleared by
    /// the subsystem-specific acknowledge).
    pub intr: u32,
    /// MI_INTR_MASK_REG — which sources are allowed to raise the CPU line.
    pub mask: u32,
}

impl Default for Mi {
    fn default() -> Self {
        Self::new()
    }
}

impl Mi {
    pub fn new() -> Self {
        Mi {
            mode: 0,
            // Plausible version word (RSP/RDP/RAC/IO revisions) per n64brew.
            version: 0x0202_0102,
            intr: 0,
            mask: 0,
        }
    }

    /// Raise one or more interrupt sources.
    #[inline]
    pub fn raise(&mut self, sources: u32) {
        self.intr |= sources;
    }

    /// Clear one or more interrupt sources (acknowledge).
    #[inline]
    pub fn clear(&mut self, sources: u32) {
        self.intr &= !sources;
    }

    /// True if any unmasked interrupt is pending — drives Cause.IP2.
    #[inline]
    pub fn interrupt_line(&self) -> bool {
        self.intr & self.mask != 0
    }

    /// Read an MI register by byte offset within the MI block.
    pub fn read(&self, offset: u32) -> u32 {
        match offset & 0xF {
            0x0 => self.mode,
            0x4 => self.version,
            0x8 => self.intr,
            0xC => self.mask,
            _ => 0,
        }
    }

    /// Write an MI register. MI_MODE / MI_INTR_MASK use a set/clear bit-pair
    /// encoding; we model the mask's set/clear pairs (the common path) and
    /// store mode verbatim.
    pub fn write(&mut self, offset: u32, v: u32) {
        match offset & 0xF {
            0x0 => self.mode = v,
            0xC => {
                // MI_INTR_MASK_REG write: pairs of (clear,set) bits per source.
                // bit 2k clears mask bit k, bit 2k+1 sets it.
                for k in 0..6 {
                    let clr = 1 << (k * 2);
                    let set = 1 << (k * 2 + 1);
                    if v & clr != 0 {
                        self.mask &= !(1 << k);
                    }
                    if v & set != 0 {
                        self.mask |= 1 << k;
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_interrupt_drives_line() {
        let mut mi = Mi::new();
        mi.raise(INTR_VI);
        assert!(!mi.interrupt_line()); // masked off
        mi.mask |= INTR_VI;
        assert!(mi.interrupt_line());
        mi.clear(INTR_VI);
        assert!(!mi.interrupt_line());
    }

    #[test]
    fn mask_set_clear_pairs() {
        let mut mi = Mi::new();
        // Set the VI mask bit (source 3): set bit = 1 << (3*2+1) = 1 << 7.
        mi.write(0xC, 1 << 7);
        assert_eq!(mi.mask & INTR_VI, INTR_VI);
        // Clear it: clear bit = 1 << (3*2) = 1 << 6.
        mi.write(0xC, 1 << 6);
        assert_eq!(mi.mask & INTR_VI, 0);
    }
}
