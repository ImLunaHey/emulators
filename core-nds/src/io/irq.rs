//! IE/IF/IME — the per-CPU interrupt controller. The DS has TWO of these (one
//! for the ARM9, one for the ARM7); each `Nds` owns an `Irq` for each core.
//! Ported from ../../ds-recomp/src/io/irq.ts.
//!
//! Ownership (see CONTRACT.md): this struct owns only its own IE/IF/IME state.
//! The line into the CPU is NOT stored here — the `Nds` god-struct samples
//! `pending()` / `wake_pending()` after every device tick and writes them onto
//! the relevant `Cpu`'s `irq_line` / `wake_line` (the executor's seams).

// IRQ bit positions (shared between ARM9 and ARM7). GXFIFO (bit 21) is ARM9
// only, but the bit constant is harmless on the ARM7 side.
pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_HBLANK: u32 = 1 << 1;
pub const IRQ_VCOUNT: u32 = 1 << 2;
pub const IRQ_TIMER0: u32 = 1 << 3;
pub const IRQ_TIMER1: u32 = 1 << 4;
pub const IRQ_TIMER2: u32 = 1 << 5;
pub const IRQ_TIMER3: u32 = 1 << 6;
pub const IRQ_DMA0: u32 = 1 << 8;
pub const IRQ_DMA1: u32 = 1 << 9;
pub const IRQ_DMA2: u32 = 1 << 10;
pub const IRQ_DMA3: u32 = 1 << 11;
pub const IRQ_KEYPAD: u32 = 1 << 12;
pub const IRQ_IPC_SYNC: u32 = 1 << 16;
pub const IRQ_IPC_FIFO_EMPTY: u32 = 1 << 17;
pub const IRQ_IPC_FIFO_NOT_EMPTY: u32 = 1 << 18;
pub const IRQ_CART: u32 = 1 << 19;
pub const IRQ_GXFIFO: u32 = 1 << 21; // ARM9 only — geometry command FIFO

#[derive(Default)]
pub struct Irq {
    /// Bitmask of enabled IRQ sources (IE).
    pub ie: u32,
    /// Bitmask of pending IRQ sources (IF). A 1-bit write clears the bit.
    pub iflag: u32,
    /// Master enable (IME bit 0).
    pub ime: bool,

    /// Pre-computed `ime && (ie & if_)` — sampled by the CPU on every step to
    /// decide whether to TAKE an IRQ (jump to the handler).
    pub cached_pending: bool,
    /// Pre-computed `(ie & if_)` — used to WAKE a halted CPU. Per GBATEK, a
    /// HALTCNT/WFI halt exits as soon as an enabled-and-pending IRQ exists,
    /// even with IME=0 or CPSR.I=1 (the CPU resumes past the halt without
    /// entering the vector). Some games run an IPC handshake with IME=0 and
    /// depend on this to wake from a SWI 0x06 idle.
    pub wake_cached: bool,
}

impl Irq {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn recache(&mut self) {
        let enabled = (self.ie & self.iflag) != 0;
        self.cached_pending = self.ime && enabled;
        self.wake_cached = enabled;
    }

    /// Raise (request) the given IRQ source bit(s).
    pub fn raise(&mut self, bits: u32) {
        self.iflag |= bits;
        self.recache();
    }

    /// Writes to IF have acknowledge semantics — a 1 bit clears the bit.
    pub fn ack_if(&mut self, value: u32) {
        self.iflag &= !value;
        self.recache();
    }

    pub fn set_ie(&mut self, value: u32) {
        self.ie = value;
        self.recache();
    }

    pub fn set_ime(&mut self, value: u32) {
        self.ime = (value & 1) != 0;
        self.recache();
    }

    /// `ime && (ie & if_)` — the CPU enters the IRQ vector when this is set
    /// and CPSR.I is clear.
    #[inline]
    pub fn pending(&self) -> bool {
        self.cached_pending
    }

    /// `(ie & if_)` — lifts a halt regardless of IME / CPSR.I.
    #[inline]
    pub fn wake_pending(&self) -> bool {
        self.wake_cached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_gated_by_ime() {
        let mut irq = Irq::new();
        irq.set_ie(IRQ_VBLANK);
        irq.raise(IRQ_VBLANK);
        // IME off: not "taken-pending", but wake-pending.
        assert!(!irq.pending());
        assert!(irq.wake_pending());
        irq.set_ime(1);
        assert!(irq.pending());
    }

    #[test]
    fn ack_clears_flag() {
        let mut irq = Irq::new();
        irq.set_ime(1);
        irq.set_ie(IRQ_VBLANK | IRQ_TIMER0);
        irq.raise(IRQ_VBLANK | IRQ_TIMER0);
        assert!(irq.pending());
        irq.ack_if(IRQ_VBLANK);
        assert_eq!(irq.iflag, IRQ_TIMER0);
        assert!(irq.pending());
        irq.ack_if(IRQ_TIMER0);
        assert!(!irq.pending());
        assert!(!irq.wake_pending());
    }
}
