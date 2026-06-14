//! Root counters (Timers 0/1/2).
//!
//! Built from psx-spx "Timers (Root Counters)". Three identical 16-bit timers,
//! each a 3-register block at 0x1F80_1100 + N*0x10:
//!
//! | offset | register                |
//! |--------|-------------------------|
//! | +0x0   | current counter (16-bit)|
//! | +0x4   | counter mode            |
//! | +0x8   | counter target (16-bit) |
//!
//! Each timer counts a selectable source (system clock, dot clock, HBLANK, or
//! /8), and can reset at the target or at 0xFFFF, raising its IRQ (one-shot or
//! repeat). The register file round-trips so BIOS reads/writes behave; [`step`]
//! advances the counters and raises the configured IRQ edge.
//!
//! Counter mode register (psx-spx 0x1F801104+N*10h) bit fields:
//!  - bit 0      Synchronization Enable (0=Free Run, 1=Synchronize via bit1-2)
//!  - bits 1-2   Synchronization Mode (0-3; timer/HBlank/VBlank specific)
//!  - bit 3      Reset counter to 0 (0=after Counter=FFFFh, 1=after =Target)
//!  - bit 4      IRQ when Counter=Target (0=Disable, 1=Enable)
//!  - bit 5      IRQ when Counter=FFFFh (0=Disable, 1=Enable)
//!  - bit 6      IRQ Once/Repeat (0=One-shot, 1=Repeatedly)
//!  - bit 7      IRQ Pulse/Toggle (0=short bit10=0 pulse, 1=toggle bit10)
//!  - bits 8-9   Clock Source (0-3; per-timer)
//!  - bit 10     Interrupt Request (0=Yes, 1=No; inverted, set after mode write)
//!  - bit 11     Reached Target Value (reset after reading mode)
//!  - bit 12     Reached FFFFh Value  (reset after reading mode)
//!  - bits 13-15 always zero
//!
//! [`step`]: Timers::step

use crate::irq::{Interrupt, Irq};

// ---- counter-mode bit positions (psx-spx) ----
const MODE_SYNC_ENABLE: u16 = 1 << 0;
#[allow(dead_code)]
const MODE_SYNC_MODE: u16 = 0b11 << 1;
const MODE_RESET_ON_TARGET: u16 = 1 << 3;
const MODE_IRQ_AT_TARGET: u16 = 1 << 4;
const MODE_IRQ_AT_FFFF: u16 = 1 << 5;
const MODE_IRQ_REPEAT: u16 = 1 << 6;
const MODE_IRQ_TOGGLE: u16 = 1 << 7;
const MODE_CLOCK_SOURCE: u16 = 0b11 << 8;
const MODE_IRQ_REQUEST: u16 = 1 << 10; // 0=request asserted (inverted)
const MODE_REACHED_TARGET: u16 = 1 << 11;
const MODE_REACHED_FFFF: u16 = 1 << 12;

/// Writable bits of the mode register (psx-spx: bits 13-15 always zero; the
/// status bits 10-12 are hardware-driven, not set by software writes).
const MODE_WRITE_MASK: u16 = 0x03FF;

/// One root counter's register block.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timer {
    /// Current counter value (16-bit; the high half-word reads as 0).
    pub counter: u16,
    /// Counter mode register. Bit 10 = IRQ request (inverted), bit 11 = reached
    /// target, bit 12 = reached 0xFFFF (psx-spx).
    pub mode: u16,
    /// Counter target value (16-bit).
    pub target: u16,
}

impl Timer {
    /// Which IRQ line this timer (0/1/2) raises.
    fn irq_source(index: usize) -> Interrupt {
        match index {
            0 => Interrupt::Timer0,
            1 => Interrupt::Timer1,
            _ => Interrupt::Timer2,
        }
    }

    #[inline]
    fn clock_source(&self) -> u16 {
        (self.mode & MODE_CLOCK_SOURCE) >> 8
    }
}

/// The three root counters.
#[derive(Debug, Clone, Default)]
pub struct Timers {
    pub timers: [Timer; 3],
}

impl Timers {
    pub fn new() -> Self {
        Timers::default()
    }

    /// Read a timer register. `off` is relative to the timers window base
    /// 0x1F80_1100; the timer index is `off >> 4`, the register `off & 0xF`.
    ///
    /// Reading the mode register clears the "reached target"/"reached FFFFh"
    /// latches (psx-spx: bits 11-12 reset after reading).
    pub fn read(&self, off: u32) -> u32 {
        let idx = (off >> 4) as usize;
        if idx >= 3 {
            return 0;
        }
        let t = &self.timers[idx];
        match off & 0xF {
            0x0 => t.counter as u32,
            0x4 => t.mode as u32,
            0x8 => t.target as u32,
            _ => 0,
        }
    }

    /// Like [`read`] but with the side effect of reading: the mode register's
    /// "reached" status latches (bits 11-12) clear on read. `read` is kept
    /// pure for the orchestrator's `&self` I/O path; the side-effecting variant
    /// is here for callers that want hardware-accurate clear-on-read.
    ///
    /// [`read`]: Timers::read
    pub fn read_mut(&mut self, off: u32) -> u32 {
        let idx = (off >> 4) as usize;
        if idx >= 3 {
            return 0;
        }
        if off & 0xF == 0x4 {
            let v = self.timers[idx].mode as u32;
            self.timers[idx].mode &= !(MODE_REACHED_TARGET | MODE_REACHED_FFFF);
            return v;
        }
        self.read(off)
    }

    /// Write a timer register. `off` is relative to 0x1F80_1100.
    ///
    /// Writing the mode register resets the counter to 0 and re-arms the IRQ
    /// request bit (psx-spx: bit 10 is set to 1 / not-requesting after a mode
    /// write, and the counter forcefully resets to 0000h).
    pub fn write(&mut self, off: u32, v: u32) {
        let idx = (off >> 4) as usize;
        if idx >= 3 {
            return;
        }
        let t = &mut self.timers[idx];
        match off & 0xF {
            0x0 => t.counter = v as u16,
            0x4 => {
                // Only the low 10 bits are software-writable; the status bits
                // (10-12) are hardware-driven. Bit 10 (IRQ request, inverted)
                // is re-asserted to "no request" (=1) on a mode write.
                t.mode = (v as u16 & MODE_WRITE_MASK) | MODE_IRQ_REQUEST;
                // Writing the mode forcefully resets the counter (psx-spx).
                t.counter = 0;
            }
            0x8 => t.target = v as u16,
            _ => {}
        }
    }

    /// Advance all three timers by `cycles` (in their respective source clocks)
    /// and raise IRQs on the configured edge.
    ///
    /// `cycles` is treated as system-clock ticks; sources that divide the
    /// system clock (Timer2 /8) scale accordingly. The dotclock/HBLANK/VBLANK
    /// sources don't have an exact system-clock ratio without GPU video timing,
    /// so they're approximated by the system clock here — the GPU's own timing
    /// will drive HBLANK/VBLANK-synced counting once it lands. `irq` is the
    /// interrupt controller borrowed in by the orchestrator.
    pub fn step(&mut self, cycles: u32, irq: &mut Irq) {
        for i in 0..3 {
            let ticks = Self::scale_cycles(i, cycles, &self.timers[i]);
            if ticks == 0 {
                continue;
            }
            Self::advance(i, ticks, &mut self.timers[i], irq);
        }
    }

    /// Convert system-clock `cycles` into source-clock ticks for timer `i`.
    fn scale_cycles(i: usize, cycles: u32, t: &Timer) -> u32 {
        match i {
            // Timer2: clock source 2/3 = system clock / 8.
            2 if t.clock_source() >= 2 => cycles / 8,
            // Timer0/1 dotclock/HBLANK and the system-clock sources are all
            // approximated 1:1 against the system clock for now.
            _ => cycles,
        }
    }

    /// Increment one timer by `ticks`, applying target/wrap resets and the
    /// configured IRQ edge.
    ///
    /// The loop advances to the next *event* boundary in one chunk (so it is
    /// O(number of events), not O(ticks)) — the nearest of "reach target" and
    /// "reach 0xFFFF" as the counter increments.
    fn advance(index: usize, ticks: u32, t: &mut Timer, irq: &mut Irq) {
        // Synchronization modes that pause counting are not modeled yet; when
        // sync is enabled we still free-run (safe over-count) rather than stall.
        let _ = MODE_SYNC_ENABLE;

        let target = t.target;
        let reset_on_target = t.mode & MODE_RESET_ON_TARGET != 0;
        let irq_at_target = t.mode & MODE_IRQ_AT_TARGET != 0;
        let irq_at_ffff = t.mode & MODE_IRQ_AT_FFFF != 0;
        let mut fire = false;

        let mut remaining = ticks;
        while remaining > 0 {
            let count = t.counter;

            // Ticks needed to land exactly on each boundary value (1..=0x10000).
            // 0 would mean "already there"; we want the *next* arrival, so a
            // count already sitting on a boundary takes a full lap (0x10000).
            let to_value = |val: u16| -> u32 {
                let d = val.wrapping_sub(count) as u32;
                if d == 0 {
                    0x1_0000
                } else {
                    d
                }
            };
            let to_target = if target != 0xFFFF {
                to_value(target)
            } else {
                // Target == FFFF coincides with the wrap boundary.
                u32::MAX
            };
            let to_ffff = to_value(0xFFFF);

            // Nearest boundary within this step budget.
            let next = to_target.min(to_ffff);
            if next > remaining {
                // No boundary reached; just advance.
                t.counter = count.wrapping_add(remaining as u16);
                break;
            }

            t.counter = count.wrapping_add(next as u16);
            remaining -= next;

            // Resolve the boundary(ies) we landed on.
            if t.counter == target {
                t.mode |= MODE_REACHED_TARGET;
                if irq_at_target {
                    fire = true;
                }
                if reset_on_target {
                    t.counter = 0;
                    continue;
                }
            }
            if t.counter == 0xFFFF {
                t.mode |= MODE_REACHED_FFFF;
                if irq_at_ffff {
                    fire = true;
                }
                // 0xFFFF -> 0 wrap consumes one more tick.
                t.counter = 0;
                if remaining > 0 {
                    remaining -= 1;
                }
            }
        }

        if fire {
            Self::trigger_irq(index, t, irq);
        }
    }

    /// Apply the pulse/toggle IRQ semantics for one firing edge and raise the
    /// source on the interrupt controller.
    fn trigger_irq(index: usize, t: &mut Timer, irq: &mut Irq) {
        let repeat = t.mode & MODE_IRQ_REPEAT != 0;
        let toggle = t.mode & MODE_IRQ_TOGGLE != 0;

        // bit10 (inverted): 0 = a request has been asserted, 1 = none. In
        // one-shot mode the IRQ pulses/toggles only once: once bit10 reads 0 the
        // timer won't raise again until a mode write re-arms it (=1).
        let already_requested = t.mode & MODE_IRQ_REQUEST == 0;
        if !repeat && already_requested {
            return;
        }

        if toggle {
            // Toggle bit10 on each IRQ (psx-spx).
            t.mode ^= MODE_IRQ_REQUEST;
        } else if repeat {
            // Repeat pulse: bit10 reads back as 1 ("no request") between the
            // few-cycle pulses we don't sub-model — leave it asserted high.
            t.mode |= MODE_IRQ_REQUEST;
        } else {
            // One-shot pulse: latch bit10 = 0 so a second edge can't re-fire.
            t.mode &= !MODE_IRQ_REQUEST;
        }

        irq.raise(Timer::irq_source(index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_block_round_trips() {
        let mut t = Timers::new();
        t.write(0x18, 0x1234); // Timer1 target (0x10 + 0x8)
        assert_eq!(t.read(0x18), 0x1234);
        t.write(0x10, 0x00AB); // Timer1 counter (0x10 + 0x0)
        assert_eq!(t.read(0x10), 0x00AB);
    }

    #[test]
    fn writing_mode_clears_counter_and_arms_irq() {
        let mut t = Timers::new();
        t.write(0x0, 0x0055); // Timer0 counter = 0x55
        t.write(0x4, 0x0000); // Timer0 mode write resets the counter
        assert_eq!(t.read(0x0), 0);
        // Bit 10 (IRQ request, inverted) is set after a mode write.
        assert_ne!(t.read(0x4) & MODE_IRQ_REQUEST as u32, 0);
    }

    #[test]
    fn counts_system_clock_one_to_one() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        t.write(0x4, 0); // Timer0: free run, system clock, no IRQ
        t.step(100, &mut irq);
        assert_eq!(t.read(0x0), 100);
    }

    #[test]
    fn timer2_div8_clock_source() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        // Timer2 (idx 2 -> off 0x20) clock source 2 = system clock / 8.
        t.write(0x24, 2 << 8);
        t.step(80, &mut irq);
        assert_eq!(t.read(0x20), 10);
    }

    #[test]
    fn irq_at_target_with_reset_repeat() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        irq.write(0x4, Interrupt::Timer0.bit()); // unmask Timer0
                                                 // Timer0: IRQ at target, reset on target, repeat.
        let mode = MODE_IRQ_AT_TARGET | MODE_RESET_ON_TARGET | MODE_IRQ_REPEAT;
        t.write(0x8, 9); // target = 9
        t.write(0x4, mode as u32);
        t.step(10, &mut irq); // counts 0..9 -> hits target, resets to 0
        assert!(irq.pending(), "target IRQ should be pending");
        assert_ne!(t.read(0x4) & MODE_REACHED_TARGET as u32, 0);
        // Reset on target wrapped it back to ~0.
        assert!(t.read(0x0) <= 1);
    }

    #[test]
    fn irq_at_ffff_wrap() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        irq.write(0x4, Interrupt::Timer1.bit()); // unmask Timer1
        let mode = MODE_IRQ_AT_FFFF | MODE_IRQ_REPEAT;
        t.write(0x14, mode as u32); // Timer1 mode (off 0x10 + 0x4)
        t.write(0x10, 0xFFFE); // counter near wrap
        t.step(3, &mut irq); // FFFE -> FFFF (fire) -> 0 -> 1
        assert!(irq.pending(), "0xFFFF wrap IRQ should be pending");
        assert_ne!(t.read(0x14) & MODE_REACHED_FFFF as u32, 0);
    }

    #[test]
    fn one_shot_irq_fires_once() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        irq.write(0x4, Interrupt::Timer0.bit());
        // One-shot (bit6=0), IRQ at target, no reset (wraps at FFFF).
        let mode = MODE_IRQ_AT_TARGET;
        t.write(0x8, 5);
        t.write(0x4, mode as u32);
        t.step(6, &mut irq); // passes target once
        assert!(irq.pending());
        // Ack and re-step past target again: one-shot must not re-raise.
        irq.write(0x0, !Interrupt::Timer0.bit());
        assert!(!irq.pending());
        // Counter wraps all the way around to hit target a second time.
        t.step(0x1_0000, &mut irq);
        assert!(!irq.pending(), "one-shot must not fire twice");
    }

    #[test]
    fn read_mut_clears_reached_latches() {
        let mut t = Timers::new();
        let mut irq = Irq::new();
        let mode = MODE_IRQ_AT_TARGET | MODE_IRQ_REPEAT;
        t.write(0x8, 4);
        t.write(0x4, mode as u32);
        t.step(5, &mut irq);
        assert_ne!(t.read(0x4) & MODE_REACHED_TARGET as u32, 0);
        // read_mut returns the latch set, then clears it.
        let v = t.read_mut(0x4);
        assert_ne!(v & MODE_REACHED_TARGET as u32, 0);
        assert_eq!(t.read(0x4) & MODE_REACHED_TARGET as u32, 0);
    }
}
