//! Virtual Boy hardware control registers (the "VB control" block at
//! 0x02000000): the game-pad serial interface, the programmable interval timer,
//! the wait-control / cartridge config, and the link/communication port. Built
//! from the Planet Virtual Boy "Sacred Tech Scroll" hardware-register chapter.
//!
//! Register map (byte-addressed, halfword-spaced):
//!   0x02000000  CCR   link control
//!   0x02000004  CCSR  link status
//!   0x02000008  CDTR  link transmit
//!   0x0200000C  CDRR  link receive
//!   0x02000010  SDLR  controller serial data low  (read)
//!   0x02000014  SDHR  controller serial data high (read)
//!   0x02000018  TLR   timer counter low
//!   0x0200001C  THR   timer counter high
//!   0x02000020  TCR   timer control
//!   0x02000024  WCR   wait-state control
//!   0x02000028  SCR   serial/gamepad control
//!
//! The timer counts down from its reload at one of two rates (20 us or 100 us
//! tick) and raises a timer interrupt at zero when enabled. We model it in CPU
//! cycles. The gamepad SCR drives a serial read of the 16-bit controller word
//! (handled by the `input` module via `Vb`).

pub struct Hw {
    // ---- Timer ----
    pub timer_reload: u16,
    pub timer_counter: u16,
    pub timer_enabled: bool,
    pub timer_interval_short: bool, // true: 20us tick, false: 100us tick
    pub timer_z_status: bool,       // zero-reached flag (TCR bit1)
    pub timer_int_enable: bool,     // TCR bit3
    pub timer_irq: bool,            // latched timer interrupt pending

    /// Cycle accumulator for timer ticks.
    accum: u32,

    // ---- Serial/gamepad control (SCR @ 0x28) ----
    pub scr: u8,

    // ---- Wait control ----
    pub wcr: u8,
}

/// CPU clock (Hz) used to convert the timer's microsecond ticks into cycles.
pub const CPU_CLOCK: f32 = 20_000_000.0;

impl Default for Hw {
    fn default() -> Self {
        Hw::new()
    }
}

impl Hw {
    pub fn new() -> Hw {
        Hw {
            timer_reload: 0,
            timer_counter: 0,
            timer_enabled: false,
            timer_interval_short: false,
            timer_z_status: false,
            timer_int_enable: false,
            timer_irq: false,
            accum: 0,
            scr: 0,
            wcr: 0,
        }
    }

    /// Cycles per timer tick: 20 us or 100 us at CPU_CLOCK.
    fn cycles_per_tick(&self) -> u32 {
        if self.timer_interval_short {
            (CPU_CLOCK * 20e-6) as u32 // 400 cycles
        } else {
            (CPU_CLOCK * 100e-6) as u32 // 2000 cycles
        }
    }

    /// Advance the timer by `cycles` CPU cycles. Sets `timer_irq` when it
    /// underflows past zero while enabled.
    pub fn step(&mut self, cycles: u32) {
        if !self.timer_enabled {
            return;
        }
        self.accum += cycles;
        let per = self.cycles_per_tick().max(1);
        while self.accum >= per {
            self.accum -= per;
            if self.timer_counter == 0 {
                // Reload and signal zero.
                self.timer_counter = self.timer_reload;
                self.timer_z_status = true;
                if self.timer_int_enable {
                    self.timer_irq = true;
                }
            } else {
                self.timer_counter = self.timer_counter.wrapping_sub(1);
                if self.timer_counter == 0 {
                    self.timer_z_status = true;
                    if self.timer_int_enable {
                        self.timer_irq = true;
                    }
                }
            }
        }
    }

    pub fn read8(&self, off: u32) -> u8 {
        match off & 0x3F {
            0x18 => (self.timer_counter & 0xFF) as u8, // TLR
            0x1C => (self.timer_counter >> 8) as u8,   // THR
            0x20 => self.tcr(),                        // TCR
            0x24 => self.wcr,                          // WCR
            0x28 => self.scr,                          // SCR
            // Link registers: open / zero.
            _ => 0x00,
        }
    }

    /// Returns true if the write touched a controller-strobe register, so the
    /// god-struct can latch input (left to Vb; here we just store control bits).
    pub fn write8(&mut self, off: u32, v: u8) {
        match off & 0x3F {
            0x18 => self.timer_reload = (self.timer_reload & 0xFF00) | v as u16, // TLR
            0x1C => self.timer_reload = (self.timer_reload & 0x00FF) | ((v as u16) << 8), // THR
            0x20 => self.write_tcr(v),
            0x24 => self.wcr = v & 0x03,
            0x28 => self.scr = v,
            _ => {}
        }
    }

    fn tcr(&self) -> u8 {
        let mut v = 0u8;
        if self.timer_enabled {
            v |= 0x01; // T-Enb
        }
        if self.timer_z_status {
            v |= 0x02; // Z-Stat
        }
        if self.timer_interval_short {
            v |= 0x10; // T-Clk-Sel
        }
        if self.timer_int_enable {
            v |= 0x08; // Tim-Z-Int
        }
        v
    }

    fn write_tcr(&mut self, v: u8) {
        self.timer_enabled = v & 0x01 != 0;
        // Z-Stat-Clr (bit2): writing 1 clears the zero status + the IRQ.
        if v & 0x04 != 0 {
            self.timer_z_status = false;
            self.timer_irq = false;
        }
        self.timer_int_enable = v & 0x08 != 0;
        self.timer_interval_short = v & 0x10 != 0;
        if self.timer_enabled && self.timer_counter == 0 {
            self.timer_counter = self.timer_reload;
        }
    }

    /// Acknowledge/clear the timer interrupt (called when the CPU services it).
    pub fn ack_timer(&mut self) {
        self.timer_irq = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_down_and_fires() {
        let mut hw = Hw::new();
        hw.write8(0x18, 0x02); // reload low = 2
        hw.write8(0x1C, 0x00);
        hw.write8(0x20, 0x01 | 0x08 | 0x10); // enable + int + short tick
        assert_eq!(hw.timer_counter, 2);
        // Each tick = 400 cycles. Run enough cycles for 3 ticks.
        hw.step(400 * 3);
        // counter 2 -> 1 -> 0 (fires) on the way down.
        assert!(hw.timer_irq, "timer should have fired at zero");
    }

    #[test]
    fn tcr_clear_clears_status() {
        let mut hw = Hw::new();
        hw.timer_z_status = true;
        hw.timer_irq = true;
        hw.write8(0x20, 0x04); // Z-Stat-Clr
        assert!(!hw.timer_z_status);
        assert!(!hw.timer_irq);
    }

    #[test]
    fn disabled_timer_does_not_tick() {
        let mut hw = Hw::new();
        hw.timer_reload = 1;
        hw.timer_counter = 1;
        hw.step(100_000);
        assert_eq!(hw.timer_counter, 1);
    }
}
