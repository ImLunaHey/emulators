//! Interrupt controller — `I_STAT` / `I_MASK`.
//!
//! Built from psx-spx "Interrupts". The PSX folds eleven interrupt sources into
//! a single CPU interrupt line (CAUSE.IP2). A source latches a bit in `I_STAT`
//! (0x1F80_1070); `I_MASK` (0x1F80_1074) gates which latched bits actually
//! drive the line. The CPU sees `(I_STAT & I_MASK) != 0` via
//! [`crate::cpu::Cpu::irq_pending`].
//!
//! Acknowledge semantics (psx-spx): a write to `I_STAT` *clears* the bits that
//! are written as 0 and leaves bits written as 1 unchanged — i.e.
//! `I_STAT &= value`. Hardware never lets software *set* a status bit, only the
//! devices do (via [`Irq::raise`]).

/// Interrupt source bit positions in `I_STAT` / `I_MASK` (psx-spx).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Interrupt {
    Vblank = 0,
    Gpu = 1,
    Cdrom = 2,
    Dma = 3,
    Timer0 = 4,
    Timer1 = 5,
    Timer2 = 6,
    /// Controller / memory-card byte received.
    ControllerMemcard = 7,
    Sio = 8,
    Spu = 9,
    /// Controller lightpen (shared with PIO).
    Pio = 10,
}

impl Interrupt {
    #[inline]
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// Mask of the eleven defined interrupt bits (0..10).
const IRQ_BITS: u32 = 0x7FF;

/// The interrupt controller register file.
#[derive(Debug, Clone, Default)]
pub struct Irq {
    /// `I_STAT` (0x1F80_1070): latched pending bits, bits 0..10.
    pub stat: u32,
    /// `I_MASK` (0x1F80_1074): per-source enable, bits 0..10.
    pub mask: u32,
}

impl Irq {
    pub fn new() -> Self {
        Irq { stat: 0, mask: 0 }
    }

    /// Latch an interrupt source as pending (called by the devices).
    #[inline]
    pub fn raise(&mut self, source: Interrupt) {
        self.stat |= source.bit();
    }

    /// True if any unmasked source is pending — this drives CAUSE.IP2.
    #[inline]
    pub fn pending(&self) -> bool {
        (self.stat & self.mask & IRQ_BITS) != 0
    }

    /// Read `I_STAT` (offset 0) or `I_MASK` (offset 4). `off` is relative to the
    /// IRQ window base 0x1F80_1070.
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x0 => self.stat,
            0x4 => self.mask,
            _ => 0,
        }
    }

    /// Write `I_STAT` (ack: `stat &= v`) or `I_MASK` (= v). `off` is relative to
    /// the IRQ window base 0x1F80_1070.
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            // Acknowledge: only bits written as 1 are *kept*; 0 clears them.
            0x0 => self.stat &= v & IRQ_BITS,
            0x4 => self.mask = v & IRQ_BITS,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_and_mask_gate_the_line() {
        let mut irq = Irq::new();
        assert!(!irq.pending());
        irq.raise(Interrupt::Vblank);
        assert!(!irq.pending(), "masked off");
        irq.write(0x4, Interrupt::Vblank.bit());
        assert!(irq.pending(), "now unmasked");
    }

    #[test]
    fn ack_clears_written_zero_bits() {
        let mut irq = Irq::new();
        irq.raise(Interrupt::Gpu);
        irq.raise(Interrupt::Cdrom);
        // Ack GPU only: write a value with GPU's bit = 0, CDROM's bit = 1.
        irq.write(0x0, !Interrupt::Gpu.bit());
        assert_eq!(irq.stat & Interrupt::Gpu.bit(), 0);
        assert_ne!(irq.stat & Interrupt::Cdrom.bit(), 0);
    }
}
