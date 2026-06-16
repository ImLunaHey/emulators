//! Four 16-bit timers. Ported 1:1 from src/io/timers.ts.

use crate::irq::{Irq, IRQ_TIMER0};
use crate::sound::Sound;

// Four 16-bit timers. Each has:
//   reload (TMxCNT_L on write), counter (TMxCNT_L on read), control bits
// Control TMxCNT_H bits:
//   0..1 prescaler (1, 64, 256, 1024)
//   2    count-up timing (chained to previous timer's overflow)
//   6    IRQ enable
//   7    start

const PRESCALES: [u32; 4] = [1, 64, 256, 1024];

#[derive(Clone)]
pub struct TimerChannel {
    pub reload: u32,
    pub counter: u32,
    pub control: u32,
    // Cycles accumulated toward next tick (only used when not in count-up).
    pub sub_cycles: u32,
    pub enabled: bool,
    pub count_up: bool,
    pub irq_enable: bool,
    pub prescale: u32,
}

impl Default for TimerChannel {
    fn default() -> Self {
        Self {
            reload: 0,
            counter: 0,
            control: 0,
            sub_cycles: 0,
            enabled: false,
            count_up: false,
            irq_enable: false,
            prescale: 1,
        }
    }
}

impl TimerChannel {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct Timers {
    pub ch: [TimerChannel; 4],
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            ch: [
                TimerChannel::new(),
                TimerChannel::new(),
                TimerChannel::new(),
                TimerChannel::new(),
            ],
        }
    }
}

impl Timers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_reload(&mut self, i: usize, v: u32) {
        self.ch[i].reload = v & 0xFFFF;
    }

    pub fn read_counter(&self, i: usize) -> u32 {
        self.ch[i].counter & 0xFFFF
    }

    pub fn read_control(&self, i: usize) -> u32 {
        self.ch[i].control
    }

    pub fn write_control(&mut self, i: usize, v: u32) {
        let c = &mut self.ch[i];
        let was_enabled = c.enabled;
        c.control = v & 0xFFFF;
        c.prescale = PRESCALES[(v & 3) as usize];
        c.count_up = i > 0 && (v & 0x04) != 0;
        c.irq_enable = (v & 0x40) != 0;
        c.enabled = (v & 0x80) != 0;
        if !was_enabled && c.enabled {
            c.counter = c.reload;
            c.sub_cycles = 0;
        }
    }

    // Step all timers by `cycles` CPU cycles. Returns `(refill_a, refill_b)`
    // — whether a Direct Sound FIFO needs a special-timing DMA refill (the
    // orchestrator runs the DMA after this returns; see `Sound::on_timer_overflow`).
    pub fn step(&mut self, cycles: u32, irq: &mut Irq, sound: &mut Sound) -> (bool, bool) {
        let mut refill = (false, false);
        for i in 0..4 {
            let c = &mut self.ch[i];
            if !c.enabled || c.count_up {
                continue;
            }
            c.sub_cycles += cycles;
            while self.ch[i].sub_cycles >= self.ch[i].prescale {
                self.ch[i].sub_cycles -= self.ch[i].prescale;
                self.ch[i].counter = (self.ch[i].counter + 1) & 0xFFFF;
                if self.ch[i].counter == 0 {
                    let (ra, rb) = self.overflow(i, irq, sound);
                    refill.0 |= ra;
                    refill.1 |= rb;
                }
            }
        }
        refill
    }

    fn overflow(&mut self, i: usize, irq: &mut Irq, sound: &mut Sound) -> (bool, bool) {
        self.ch[i].counter = self.ch[i].reload;
        if self.ch[i].irq_enable {
            irq.raise(IRQ_TIMER0 << i);
        }
        // Direct Sound A/B are driven by Timer 0 or Timer 1 overflow.
        let mut refill = (false, false);
        if i == 0 || i == 1 {
            refill = sound.on_timer_overflow(i as u32);
        }
        // Cascade to next channel if it is count-up.
        if i < 3 {
            let next_enabled = self.ch[i + 1].enabled;
            let next_count_up = self.ch[i + 1].count_up;
            if next_enabled && next_count_up {
                self.ch[i + 1].counter = (self.ch[i + 1].counter + 1) & 0xFFFF;
                if self.ch[i + 1].counter == 0 {
                    let (ra, rb) = self.overflow(i + 1, irq, sound);
                    refill.0 |= ra;
                    refill.1 |= rb;
                }
            }
        }
        refill
    }
}

// Tests ported from the (deleted) TypeScript suite src/test/timers.test.ts.
// Harness style (B): construct `Timers` directly and drive `step` with mock
// `Irq`/`Sound` collaborators (the TS `Timers` stored `irq`; per the port
// contract it's a `&mut` parameter here).
#[cfg(test)]
mod tests {
    use super::*;

    // ---- prescaler -----------------------------------------------------

    #[test]
    fn prescale_1_ticks_every_cycle() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0);
        t.write_control(0, 0x80); // enable, prescale 1
        t.step(100, &mut irq, &mut snd);
        assert_eq!(t.read_counter(0), 100);
    }

    #[test]
    fn prescale_64() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0);
        t.write_control(0, 0x81); // enable, prescale 64
        t.step(128, &mut irq, &mut snd);
        assert_eq!(t.read_counter(0), 2);
    }

    #[test]
    fn prescale_1024() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0);
        t.write_control(0, 0x83); // enable, prescale 1024
        t.step(3072, &mut irq, &mut snd);
        assert_eq!(t.read_counter(0), 3);
    }

    // ---- overflow + reload ---------------------------------------------

    #[test]
    fn overflow_restores_reload_value() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFD);
        t.write_control(0, 0x80); // enable, prescale 1
        t.step(5, &mut irq, &mut snd);
        // FFFD →(1) FFFE →(2) FFFF →(3) reload FFFD →(4) FFFE →(5) FFFF
        assert_eq!(t.read_counter(0), 0xFFFF);
    }

    #[test]
    fn overflow_raises_irq_when_enabled() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFF);
        t.write_control(0, 0xC0); // enable, IRQ
        t.step(1, &mut irq, &mut snd);
        assert_ne!(irq.iflag & (1 << 3), 0);
    }

    #[test]
    fn overflow_no_irq_when_disabled() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFF);
        t.write_control(0, 0x80); // enable, NO IRQ
        t.step(1, &mut irq, &mut snd);
        assert_eq!(irq.iflag, 0);
    }

    // ---- count-up cascade ----------------------------------------------

    #[test]
    fn timer1_countup_ticks_on_timer0_overflow() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFF);
        t.write_control(0, 0x80); // T0: enable, prescale 1
        t.write_reload(1, 0);
        t.write_control(1, 0x84); // T1: enable, count-up
        t.step(3, &mut irq, &mut snd);
        assert_eq!(t.read_counter(1), 3);
    }

    #[test]
    fn timer2_cascade_from_timer1() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFF);
        t.write_control(0, 0x80);
        t.write_reload(1, 0xFFFF);
        t.write_control(1, 0x84);
        t.write_reload(2, 0);
        t.write_control(2, 0x84);
        t.step(1, &mut irq, &mut snd);
        // T0 overflow → T1 tick → T1 overflow → T2 tick.
        assert_eq!(t.read_counter(2), 1);
    }

    // ---- enable starts from reload -------------------------------------

    #[test]
    fn enable_reloads_counter() {
        let mut t = Timers::new();
        t.write_reload(0, 0x1234);
        assert_eq!(t.read_counter(0), 0); // not yet enabled
        t.write_control(0, 0x80);
        assert_eq!(t.read_counter(0), 0x1234);
    }

    #[test]
    fn control_rewrite_same_enable_does_not_reload() {
        let mut irq = Irq::new();
        let mut snd = Sound::new();
        let mut t = Timers::new();
        t.write_reload(0, 0x1234);
        t.write_control(0, 0x80);
        t.step(10, &mut irq, &mut snd);
        assert_eq!(t.read_counter(0), 0x1234 + 10);
        // Rewrite control with the same enable bit → counter must not reset.
        t.write_reload(0, 0x9999);
        t.write_control(0, 0x80);
        assert_eq!(t.read_counter(0), 0x1234 + 10);
    }
}
