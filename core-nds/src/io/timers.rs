//! Four hardware timers per CPU — each `Nds` owns one `Timers` for the ARM9
//! and one for the ARM7. Ported from ../../ds-recomp/src/io/timers.ts.
//!
//! Each timer has a 16-bit counter that ticks (from a programmable reload, via
//! a per-timer prescaler) up to 0x10000, overflows back to the reload, and may
//! raise an IRQ. Cascade mode counts a predecessor's overflows instead of
//! clock ticks.
//!
//! Ownership (see CONTRACT.md): the TS `Timers` held an `irq` ref and raised
//! `IRQ_TIMER0..3` on overflow. We pass the core's `Irq` as a parameter to the
//! only method that can overflow — [`Timers::step`]. Register writes never
//! raise (the rising-edge enable just snapshots the reload), so [`Timers::
//! write8`] needs no `Irq`.

use super::irq::{Irq, IRQ_TIMER0, IRQ_TIMER1, IRQ_TIMER2, IRQ_TIMER3};

/// Prescaler shifts for CNT bits 0..1: /1, /64, /256, /1024.
const PRESCALER_SHIFTS: [u32; 4] = [0, 6, 8, 10];

/// IRQ bit raised on overflow of each timer index.
const TIMER_IRQ_BITS: [u32; 4] = [IRQ_TIMER0, IRQ_TIMER1, IRQ_TIMER2, IRQ_TIMER3];

/// CNT control bit masks.
const CNT_PRESCALER: u16 = 0x0003;
const CNT_CASCADE: u16 = 0x0004; // count-up: tick on predecessor overflow
const CNT_IRQ: u16 = 0x0040; // raise IRQ on overflow
const CNT_ENABLE: u16 = 0x0080; // start bit

pub struct Timers {
    /// 16-bit counters (stored wide for headroom).
    pub counter: [u32; 4],
    pub reload: [u32; 4],
    pub cnt: [u16; 4],
    /// Fractional cycles toward the next tick for each non-cascade timer.
    pub frac: [f64; 4],
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

impl Timers {
    pub fn new() -> Self {
        Timers {
            counter: [0; 4],
            reload: [0; 4],
            cnt: [0; 4],
            frac: [0.0; 4],
        }
    }

    /// `reg` is the byte offset within the 16-byte timer block (0x100..0x110 →
    /// 0..16): `(reg >> 2)` selects the timer, `reg & 3` the sub-register
    /// (0/1 = counter read, 2/3 = control).
    pub fn read8(&self, reg: u32) -> u32 {
        let t = ((reg >> 2) & 0x3) as usize;
        match reg & 0x3 {
            0 => self.counter[t] & 0xFF,
            1 => (self.counter[t] >> 8) & 0xFF,
            2 => (self.cnt[t] & 0xFF) as u32,
            _ => ((self.cnt[t] >> 8) & 0xFF) as u32,
        }
    }

    /// Writing the control byte with the enable bit rising snapshots the reload
    /// into the counter — that's internal, so no `Irq` needed here.
    pub fn write8(&mut self, reg: u32, value: u32) {
        let t = ((reg >> 2) & 0x3) as usize;
        let v = (value & 0xFF) as u16;
        match reg & 0x3 {
            // Reload low / high bytes. The reload register is write-only; reads
            // return the live counter. We keep reload as a u32 holding a 16-bit
            // value.
            0 => self.reload[t] = (self.reload[t] & 0xFF00) | (v as u32),
            1 => self.reload[t] = (self.reload[t] & 0x00FF) | ((v as u32) << 8),
            2 => {
                let was_enabled = (self.cnt[t] & CNT_ENABLE) != 0;
                let new_cnt = (self.cnt[t] & 0xFF00) | v;
                self.cnt[t] = new_cnt;
                // Rising-edge enable: snapshot the reload into the counter.
                if !was_enabled && (new_cnt & CNT_ENABLE) != 0 {
                    self.counter[t] = self.reload[t] & 0xFFFF;
                    self.frac[t] = 0.0;
                }
            }
            _ => self.cnt[t] = (self.cnt[t] & 0x00FF) | (v << 8),
        }
    }

    /// Advance all enabled (non-cascade) timers by `cycles` ARM cycles; cascade
    /// timers are driven by their predecessor's overflow. Overflows raise the
    /// matching `IRQ_TIMER0..3` on the supplied core `Irq`.
    pub fn step(&mut self, cycles: u32, irq: &mut Irq) {
        for t in 0..4 {
            // Disabled timers don't tick.
            if (self.cnt[t] & CNT_ENABLE) == 0 {
                continue;
            }
            // Cascade timers (t > 0 with count-up set) are driven only by their
            // predecessor's overflow, never directly by the clock.
            if t > 0 && (self.cnt[t] & CNT_CASCADE) != 0 {
                continue;
            }
            let shift = PRESCALER_SHIFTS[(self.cnt[t] & CNT_PRESCALER) as usize];
            let ticks_frac = self.frac[t] + (cycles as f64) / ((1u32 << shift) as f64);
            let ticks = ticks_frac.floor();
            self.frac[t] = ticks_frac - ticks;
            self.advance(t, ticks as u32, irq);
        }
    }

    /// Advance timer `t` by `ticks` ticks, handling overflow/reload, IRQ, and
    /// cascade into the next timer.
    fn advance(&mut self, t: usize, ticks: u32, irq: &mut Irq) {
        let mut c = self.counter[t].wrapping_add(ticks);
        while c >= 0x10000 {
            // On overflow the counter wraps back through the reload value, not
            // through 0 — preserve any extra ticks past the boundary.
            c = c - 0x10000 + (self.reload[t] & 0xFFFF);
            // IRQ on overflow if enabled.
            if (self.cnt[t] & CNT_IRQ) != 0 {
                irq.raise(TIMER_IRQ_BITS[t]);
            }
            // Cascade: bump the next timer's counter by 1 if it is an enabled
            // count-up timer.
            if t < 3 && (self.cnt[t + 1] & (CNT_ENABLE | CNT_CASCADE)) == (CNT_ENABLE | CNT_CASCADE)
            {
                self.advance(t + 1, 1, irq);
            }
        }
        self.counter[t] = c;
    }
}

// Tests mirror the GBA core's timer suite, adapted to the DS skeleton API
// (byte-granular register IO, `frac` prescaler, `Irq`-only `step`).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::irq::IRQ_TIMER1 as IRQ_T1;

    /// Write a full 16-bit reload via the two byte registers for timer `t`.
    fn set_reload(t: &mut Timers, i: u32, v: u32) {
        let base = i * 4;
        t.write8(base, v & 0xFF);
        t.write8(base + 1, (v >> 8) & 0xFF);
    }

    /// Write the control low byte for timer `t`.
    fn set_cnt(t: &mut Timers, i: u32, v: u32) {
        t.write8(i * 4 + 2, v);
    }

    /// Read the full 16-bit counter for timer `t`.
    fn read_counter(t: &Timers, i: u32) -> u32 {
        let base = i * 4;
        t.read8(base) | (t.read8(base + 1) << 8)
    }

    // ---- prescaler -----------------------------------------------------

    #[test]
    fn prescale_1_ticks_every_cycle() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0);
        set_cnt(&mut t, 0, 0x80); // enable, prescale 1
        t.step(100, &mut irq);
        assert_eq!(read_counter(&t, 0), 100);
    }

    #[test]
    fn prescale_64() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0);
        set_cnt(&mut t, 0, 0x81); // enable, prescale 64
        t.step(128, &mut irq);
        assert_eq!(read_counter(&t, 0), 2);
    }

    #[test]
    fn prescale_1024() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0);
        set_cnt(&mut t, 0, 0x83); // enable, prescale 1024
        t.step(3072, &mut irq);
        assert_eq!(read_counter(&t, 0), 3);
    }

    #[test]
    fn frac_accumulates_across_steps() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0);
        set_cnt(&mut t, 0, 0x81); // enable, prescale 64
        // 32 cycles → 0.5 tick, then another 32 → 1 tick total.
        t.step(32, &mut irq);
        assert_eq!(read_counter(&t, 0), 0);
        t.step(32, &mut irq);
        assert_eq!(read_counter(&t, 0), 1);
    }

    // ---- overflow + reload ---------------------------------------------

    #[test]
    fn overflow_restores_reload_value() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFD);
        set_cnt(&mut t, 0, 0x80); // enable, prescale 1
        t.step(5, &mut irq);
        // FFFD →(1) FFFE →(2) FFFF →(3) overflow→reload FFFD →(4) FFFE →(5) FFFF
        assert_eq!(read_counter(&t, 0), 0xFFFF);
    }

    #[test]
    fn overflow_raises_irq_when_enabled() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFF);
        set_cnt(&mut t, 0, 0xC0); // enable, IRQ
        t.step(1, &mut irq);
        assert_ne!(irq.iflag & IRQ_TIMER0, 0);
    }

    #[test]
    fn overflow_no_irq_when_disabled() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFF);
        set_cnt(&mut t, 0, 0x80); // enable, NO IRQ
        t.step(1, &mut irq);
        assert_eq!(irq.iflag, 0);
    }

    // ---- count-up cascade ----------------------------------------------

    #[test]
    fn timer1_countup_ticks_on_timer0_overflow() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFF);
        set_cnt(&mut t, 0, 0x80); // T0: enable, prescale 1
        set_reload(&mut t, 1, 0);
        set_cnt(&mut t, 1, 0x84); // T1: enable, count-up
        t.step(3, &mut irq);
        assert_eq!(read_counter(&t, 1), 3);
    }

    #[test]
    fn timer2_cascade_from_timer1() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFF);
        set_cnt(&mut t, 0, 0x80);
        set_reload(&mut t, 1, 0xFFFF);
        set_cnt(&mut t, 1, 0x84);
        set_reload(&mut t, 2, 0);
        set_cnt(&mut t, 2, 0x84);
        t.step(1, &mut irq);
        // T0 overflow → T1 tick → T1 overflow → T2 tick.
        assert_eq!(read_counter(&t, 2), 1);
    }

    #[test]
    fn cascade_overflow_raises_predecessor_chain_irq() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0xFFFF);
        set_cnt(&mut t, 0, 0x80); // T0 no IRQ
        set_reload(&mut t, 1, 0xFFFF);
        set_cnt(&mut t, 1, 0xC4); // T1: enable, count-up, IRQ
        t.step(1, &mut irq);
        // T0 overflows → T1 ticks from FFFF → overflow → IRQ_TIMER1.
        assert_ne!(irq.iflag & IRQ_T1, 0);
    }

    // ---- enable starts from reload -------------------------------------

    #[test]
    fn enable_reloads_counter() {
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0x1234);
        assert_eq!(read_counter(&t, 0), 0); // not yet enabled
        set_cnt(&mut t, 0, 0x80);
        assert_eq!(read_counter(&t, 0), 0x1234);
    }

    #[test]
    fn control_rewrite_same_enable_does_not_reload() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0x1234);
        set_cnt(&mut t, 0, 0x80);
        t.step(10, &mut irq);
        assert_eq!(read_counter(&t, 0), 0x1234 + 10);
        // Rewrite control with the same enable bit → counter must not reset.
        set_reload(&mut t, 0, 0x9999);
        set_cnt(&mut t, 0, 0x80);
        assert_eq!(read_counter(&t, 0), 0x1234 + 10);
    }

    // ---- control read-back ---------------------------------------------

    #[test]
    fn control_byte_reads_back() {
        let mut t = Timers::new();
        set_cnt(&mut t, 2, 0xC3);
        assert_eq!(t.read8(2 * 4 + 2), 0xC3);
        // High control byte is unused on the DS but must read its stored bits.
        t.write8(2 * 4 + 3, 0x00);
        assert_eq!(t.read8(2 * 4 + 3), 0x00);
    }

    #[test]
    fn disabled_timer_does_not_advance() {
        let mut irq = Irq::new();
        let mut t = Timers::new();
        set_reload(&mut t, 0, 0);
        set_cnt(&mut t, 0, 0x00); // not enabled
        t.step(1000, &mut irq);
        assert_eq!(read_counter(&t, 0), 0);
    }
}
