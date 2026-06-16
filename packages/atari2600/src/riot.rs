//! RIOT (6532) — RAM, Interval timer, and I/O Ports.
//!
//! Spec: Stella Programmer's Guide §"6532 Timer" and §"I/O ports", and the
//! 6532 datasheet. The RIOT provides three things to the 2600:
//!
//!   1. **128 bytes of RAM** — the machine's only writable scratch + stack,
//!      at CPU $0080-$00FF (mirrored, and again at $0180-$01FF for the stack).
//!   2. **The interval timer** — a down-counter pre-scaled by 1, 8, 64, or
//!      1024 CPU cycles. Software writes the divider+start value to one of
//!      TIM1T/TIM8T/TIM64T/T1024T and polls INTIM to time game events. When the
//!      counter underflows past 0 it switches to a 1-cycle decrement and sets
//!      the timer-interrupt flag (readable in INSTAT bit 7).
//!   3. **The I/O ports** — SWCHA (joysticks) and SWCHB (console switches).
//!      The 2600 wires these as inputs; we expose latches the host drives.
//!
//! Register addresses below are the *decoded* offsets the bus hands us (it has
//! already masked the chip-select bits). We decode reads/writes by the bits the
//! Programmer's Guide specifies.

/// CPU cycles between timer decrements for each divider setting.
const DIVIDERS: [u32; 4] = [1, 8, 64, 1024];

pub struct Riot {
    /// 128 bytes of zero-page RAM.
    pub ram: Box<[u8; 128]>,

    /// Current divider (1/8/64/1024).
    interval: u32,
    /// CPU cycles accumulated toward the next decrement.
    prescale: u32,
    /// The visible timer value (INTIM).
    timer: u8,
    /// Set when the timer has underflowed past 0; cleared by reading INTIM.
    /// After underflow the timer decrements every cycle until read.
    underflowed: bool,
    /// Latched timer-interrupt flag (INSTAT bit 7), set on underflow.
    timer_irq: bool,

    /// SWCHA — port A input latch (joysticks). Bit = 0 means pressed.
    pub swcha: u8,
    /// SWCHB — port B input latch (console switches). Bit = 0 means
    /// pressed/active for the momentary switches.
    pub swchb: u8,
}

impl Default for Riot {
    fn default() -> Self {
        Riot::new()
    }
}

impl Riot {
    pub fn new() -> Riot {
        Riot {
            ram: vec![0u8; 128].into_boxed_slice().try_into().unwrap(),
            interval: 1024,
            prescale: 0,
            timer: 0,
            underflowed: false,
            timer_irq: false,
            // All joystick lines released (high).
            swcha: 0xFF,
            // SWCHB: bit0 reset (1=released), bit1 select (1=released),
            // bit3 colour/BW (1=colour), bits 6/7 difficulty (1=B/amateur).
            // Unused bits read 1.
            swchb: 0b1100_1011,
        }
    }

    /// Advance the timer by `cycles` CPU cycles.
    pub fn step(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.tick_one();
        }
    }

    fn tick_one(&mut self) {
        if self.underflowed {
            // After underflow the counter decrements every cycle (it wrapped to
            // 0xFF and keeps going) until software reads INTIM.
            self.timer = self.timer.wrapping_sub(1);
            return;
        }
        self.prescale += 1;
        if self.prescale >= self.interval {
            self.prescale = 0;
            if self.timer == 0 {
                self.timer = 0xFF;
                self.underflowed = true;
                self.timer_irq = true;
            } else {
                self.timer -= 1;
            }
        }
    }

    /// Read a RIOT register. `addr` is the full CPU address; the RIOT decodes
    /// A2 (timer/IO select), A0-A1, and A9 (mirror) per the Programmer's Guide.
    pub fn read(&mut self, addr: u16) -> u8 {
        // A2 (0x04) distinguishes the I/O ports (low) from the timer (high).
        if addr & 0x04 == 0 {
            // I/O ports: SWCHA / SWACNT / SWCHB / SWBCNT by A0,A1.
            match addr & 0x03 {
                0x00 => self.swcha,
                0x01 => 0x00, // SWACNT (data direction) — we treat ports as inputs
                0x02 => self.swchb,
                _ => 0x00,    // SWBCNT
            }
        } else {
            // Timer area: A0 selects INTIM vs INSTAT.
            if addr & 0x01 == 0 {
                // INTIM — read the timer; reading clears the underflow "fast
                // decrement" mode and the IRQ flag.
                let v = self.timer;
                self.underflowed = false;
                self.timer_irq = false;
                v
            } else {
                // INSTAT — timer interrupt flag in bit 7.
                let v = if self.timer_irq { 0x80 } else { 0x00 };
                v
            }
        }
    }

    /// Write a RIOT register. Writes with A4 set and A2 set select the timer
    /// (the divider is chosen by A0-A1: 1/8/64/1024).
    pub fn write(&mut self, addr: u16, v: u8) {
        // The timer-set registers are at $14-$17 (A2 and A4 set). We accept the
        // common decode: A2 set => timer write, divider from A0-A1.
        if addr & 0x14 == 0x14 {
            let div_idx = (addr & 0x03) as usize;
            self.interval = DIVIDERS[div_idx];
            self.timer = v;
            self.prescale = 0;
            self.underflowed = false;
            self.timer_irq = false;
        }
        // Writes to the I/O data-direction registers are accepted and ignored
        // (we model the ports as pure inputs driven by the host).
    }

    /// Current INTIM value without side effects (for tests/debug).
    pub fn peek_timer(&self) -> u8 {
        self.timer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tim1t_counts_every_cycle() {
        let mut r = Riot::new();
        r.write(0x14, 0x05); // TIM1T = 5
        assert_eq!(r.peek_timer(), 5);
        r.step(1);
        assert_eq!(r.peek_timer(), 4);
        r.step(4);
        assert_eq!(r.peek_timer(), 0);
    }

    #[test]
    fn tim64t_prescales() {
        let mut r = Riot::new();
        r.write(0x16, 0x02); // TIM64T = 2 (divider 64)
        assert_eq!(r.peek_timer(), 2);
        r.step(63);
        assert_eq!(r.peek_timer(), 2); // not yet
        r.step(1);
        assert_eq!(r.peek_timer(), 1); // 64 cycles -> one decrement
        r.step(64);
        assert_eq!(r.peek_timer(), 0);
    }

    #[test]
    fn underflow_sets_irq_and_fast_counts() {
        let mut r = Riot::new();
        r.write(0x14, 0x00); // TIM1T = 0
        // Next tick underflows -> timer becomes 0xFF, irq set.
        r.step(1);
        assert_eq!(r.peek_timer(), 0xFF);
        // INSTAT bit 7 set.
        assert_eq!(r.read(0x05) & 0x80, 0x80);
        // After underflow it decrements every cycle.
        r.step(1);
        assert_eq!(r.peek_timer(), 0xFE);
    }

    #[test]
    fn reading_intim_clears_irq() {
        let mut r = Riot::new();
        r.write(0x14, 0x00);
        r.step(1); // underflow
        assert!(r.read(0x05) & 0x80 != 0); // INSTAT shows flag
        let _ = r.read(0x04); // INTIM read clears it
        assert_eq!(r.read(0x05) & 0x80, 0x00);
    }

    #[test]
    fn swcha_reads_input_latch() {
        let mut r = Riot::new();
        r.swcha = 0xEF; // some joystick line pulled low
        assert_eq!(r.read(0x00), 0xEF);
    }
}
