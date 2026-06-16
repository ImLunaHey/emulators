//! The DIV/TIMA/TMA/TAC timer.
//!
//! Spec: Pan Docs — Timer and Divider Registers (gbdev.io/pandocs/Timer_and_Divider_Registers.html).
//!
//! The timer is driven by a free-running 16-bit internal counter that ticks
//! once per T-cycle (the same clock the CPU runs on). `DIV` (0xFF04) is the
//! upper 8 bits of that counter; writing any value to `DIV` resets the whole
//! counter to 0. `TIMA` (0xFF05) increments on the *falling edge* of a selected
//! bit of the internal counter (chosen by `TAC`); when it overflows it reloads
//! from `TMA` (0xFF06) and raises the Timer interrupt. `TAC` (0xFF07) bit 2
//! enables the timer; bits 1-0 pick the increment rate.
//!
//! The falling-edge model is what makes the obscure timer behaviors (writing
//! DIV/TAC mid-count nudging TIMA) emerge naturally, so we model it that way
//! rather than as a simple divided counter.

use crate::interrupts::{Interrupt, Irq};

#[derive(Clone)]
pub struct Timer {
    /// The 16-bit free-running internal counter. DIV is its high byte.
    pub counter: u16,
    /// TIMA (0xFF05) — the counting register.
    pub tima: u8,
    /// TMA (0xFF06) — the reload value on TIMA overflow.
    pub tma: u8,
    /// TAC (0xFF07) — bit 2 enable, bits 1-0 clock select.
    pub tac: u8,
    /// One-cycle delay between TIMA overflow and the reload+IRQ (hardware
    /// holds TIMA at 0x00 for one M-cycle before loading TMA).
    reload_pending: u8,
}

impl Default for Timer {
    fn default() -> Self {
        Timer {
            // Post-boot DIV is non-zero on real hardware; the exact value isn't
            // critical for our purposes, start the counter at 0.
            counter: 0,
            tima: 0,
            tma: 0,
            tac: 0,
            reload_pending: 0,
        }
    }
}

impl Timer {
    /// Which bit of the internal counter the selected TAC rate taps. A falling
    /// edge on this bit increments TIMA.
    ///   00 → bit 9 (4096 Hz), 01 → bit 3 (262144 Hz),
    ///   10 → bit 5 (65536 Hz), 11 → bit 7 (16384 Hz).
    #[inline]
    fn tac_bit(tac: u8) -> u16 {
        match tac & 0x03 {
            0 => 1 << 9,
            1 => 1 << 3,
            2 => 1 << 5,
            _ => 1 << 7,
        }
    }

    /// The current value of the "tick" line: (selected bit AND enable). TIMA
    /// counts on the line's falling edge.
    #[inline]
    fn tick_line(&self) -> bool {
        (self.counter & Self::tac_bit(self.tac)) != 0 && (self.tac & 0x04) != 0
    }

    /// Advance the timer by `cycles` T-cycles. Each T-cycle steps the internal
    /// counter and checks for falling edges on the selected tap.
    pub fn step(&mut self, cycles: u32, irq: &mut Irq) {
        for _ in 0..cycles {
            // Service a pending reload first: TIMA sat at 0 for one cycle.
            if self.reload_pending > 0 {
                self.reload_pending -= 1;
                if self.reload_pending == 0 {
                    self.tima = self.tma;
                    irq.request(Interrupt::Timer);
                }
            }

            let before = self.tick_line();
            self.counter = self.counter.wrapping_add(1);
            let after = self.tick_line();

            if before && !after {
                self.increment_tima();
            }
        }
    }

    /// Increment TIMA, scheduling the reload+IRQ on overflow.
    #[inline]
    fn increment_tima(&mut self) {
        let (v, carry) = self.tima.overflowing_add(1);
        self.tima = v;
        if carry {
            // Overflow: TIMA reads 0 for one cycle, then reloads from TMA and
            // fires the interrupt.
            self.reload_pending = 1;
        }
    }

    // ---- IO register access ----
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => 0xF8 | (self.tac & 0x07), // unused bits read 1
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0xFF04 => {
                // Writing DIV resets the counter. A falling edge can occur if
                // the tap bit was high.
                let before = self.tick_line();
                self.counter = 0;
                if before && !self.tick_line() {
                    self.increment_tima();
                }
            }
            0xFF05 => {
                // A write during the one-cycle reload window cancels the reload.
                if self.reload_pending == 0 {
                    self.tima = v;
                }
            }
            0xFF06 => self.tma = v,
            0xFF07 => {
                let before = self.tick_line();
                self.tac = v & 0x07;
                // Changing TAC can drop the tick line, producing a falling edge.
                if before && !self.tick_line() {
                    self.increment_tima();
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
    fn div_increments_and_resets() {
        let mut t = Timer::default();
        let mut irq = Irq::new();
        t.step(256, &mut irq); // 256 T-cycles -> DIV ticks once
        assert_eq!(t.read(0xFF04), 1);
        t.write(0xFF04, 0xFF); // any write resets
        assert_eq!(t.read(0xFF04), 0);
    }

    #[test]
    fn tima_counts_at_selected_rate() {
        let mut t = Timer::default();
        let mut irq = Irq::new();
        t.write(0xFF07, 0x05); // enable + rate 01 (bit 3, every 16 cycles)
        t.step(16, &mut irq);
        assert_eq!(t.read(0xFF05), 1);
        t.step(16, &mut irq);
        assert_eq!(t.read(0xFF05), 2);
    }

    #[test]
    fn tima_overflow_reloads_tma_and_irqs() {
        let mut t = Timer::default();
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        t.write(0xFF06, 0xAB); // TMA
        t.write(0xFF05, 0xFF); // TIMA about to overflow
        t.write(0xFF07, 0x05); // enable, fast rate
        t.step(16, &mut irq); // overflow
        t.step(4, &mut irq); // let the reload window elapse
        assert_eq!(t.read(0xFF05), 0xAB);
        assert_eq!(irq.pending() & Interrupt::Timer.mask(), Interrupt::Timer.mask());
    }

    #[test]
    fn disabled_timer_does_not_count() {
        let mut t = Timer::default();
        let mut irq = Irq::new();
        t.write(0xFF07, 0x01); // rate set but enable bit clear
        t.step(64, &mut irq);
        assert_eq!(t.read(0xFF05), 0);
    }
}
