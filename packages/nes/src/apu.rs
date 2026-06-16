//! The 2A03 APU: 2 pulse, triangle, noise, and DMC channels + the frame
//! counter that clocks length/envelope/sweep units.
//!
//! Spec: NESdev wiki "APU", "APU Pulse/Triangle/Noise/DMC", "APU Frame
//! Counter". This is a functional (not cycle-perfect) mixer: each channel
//! advances its timer in CPU cycles and the frame sequencer drives the
//! envelope/length/sweep at the standard ~240 Hz / ~120 Hz steps. Samples are
//! produced by downsampling the mixed output to a fixed host rate.

const CPU_HZ: f64 = 1_789_773.0;
/// Host sample rate. The orchestrator drains f32 mono samples at this rate.
pub const SAMPLE_RATE: f64 = 44_100.0;

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

const TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

const NOISE_PERIOD: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

#[derive(Default)]
struct Envelope {
    start: bool,
    loop_flag: bool,
    constant: bool,
    volume: u8,
    divider: u8,
    decay: u8,
}
impl Envelope {
    fn clock(&mut self) {
        if self.start {
            self.start = false;
            self.decay = 15;
            self.divider = self.volume;
        } else if self.divider == 0 {
            self.divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if self.loop_flag {
                self.decay = 15;
            }
        } else {
            self.divider -= 1;
        }
    }
    fn output(&self) -> u8 {
        if self.constant {
            self.volume
        } else {
            self.decay
        }
    }
}

#[derive(Default)]
struct Pulse {
    enabled: bool,
    duty: u8,
    duty_pos: u8,
    timer: u16,
    timer_period: u16,
    length: u8,
    length_halt: bool,
    env: Envelope,
    // sweep
    sweep_enable: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_reload: bool,
    sweep_divider: u8,
    is_pulse2: bool,
}
impl Pulse {
    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            self.duty_pos = (self.duty_pos + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }
    fn target_period(&self) -> u16 {
        let change = self.timer_period >> self.sweep_shift;
        if self.sweep_negate {
            // Pulse 1 uses ones' complement (-c-1), pulse 2 twos' complement.
            self.timer_period.wrapping_sub(change + if self.is_pulse2 { 0 } else { 1 })
        } else {
            self.timer_period.wrapping_add(change)
        }
    }
    fn clock_sweep(&mut self) {
        if self.sweep_divider == 0 && self.sweep_enable && self.sweep_shift > 0 {
            let t = self.target_period();
            if t <= 0x7FF && self.timer_period >= 8 {
                self.timer_period = t;
            }
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }
    fn clock_length(&mut self) {
        if !self.length_halt && self.length > 0 {
            self.length -= 1;
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled
            || self.length == 0
            || self.timer_period < 8
            || self.target_period() > 0x7FF
            || DUTY_TABLE[self.duty as usize][self.duty_pos as usize] == 0
        {
            0
        } else {
            self.env.output()
        }
    }
}

#[derive(Default)]
struct Triangle {
    enabled: bool,
    timer: u16,
    timer_period: u16,
    length: u8,
    length_halt: bool, // also the linear-counter control flag
    linear_counter: u8,
    linear_reload_value: u8,
    linear_reload: bool,
    seq_pos: u8,
}
impl Triangle {
    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            if self.length > 0 && self.linear_counter > 0 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
        } else {
            self.timer -= 1;
        }
    }
    fn clock_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.length_halt {
            self.linear_reload = false;
        }
    }
    fn clock_length(&mut self) {
        if !self.length_halt && self.length > 0 {
            self.length -= 1;
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled || self.timer_period < 2 {
            // Very high frequencies are silenced to avoid a pop.
            return 7;
        }
        TRIANGLE_TABLE[self.seq_pos as usize]
    }
}

#[derive(Default)]
struct Noise {
    enabled: bool,
    mode: bool,
    timer: u16,
    timer_period: u16,
    shift: u16,
    length: u8,
    length_halt: bool,
    env: Envelope,
}
impl Noise {
    fn new() -> Noise {
        Noise { shift: 1, ..Default::default() }
    }
    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            let bit = if self.mode { 6 } else { 1 };
            let feedback = (self.shift & 1) ^ ((self.shift >> bit) & 1);
            self.shift = (self.shift >> 1) | (feedback << 14);
        } else {
            self.timer -= 1;
        }
    }
    fn clock_length(&mut self) {
        if !self.length_halt && self.length > 0 {
            self.length -= 1;
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled || self.length == 0 || self.shift & 1 == 1 {
            0
        } else {
            self.env.output()
        }
    }
}

/// Minimal DMC: tracks the output level and enabled flag for the mixer.
/// Sample fetch/playback timing is approximated (no DMA stalls).
#[derive(Default)]
struct Dmc {
    enabled: bool,
    level: u8,
}

pub struct Apu {
    pulse1: Pulse,
    pulse2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,

    frame_mode_5step: bool,
    frame_irq_inhibit: bool,
    pub frame_irq: bool,
    frame_counter: u32,

    // Downsampling accumulator.
    sample_accum: f64,
    samples: Vec<f32>,
}

impl Default for Apu {
    fn default() -> Self {
        Apu::new()
    }
}

impl Apu {
    pub fn new() -> Apu {
        Apu {
            pulse1: Pulse::default(),
            pulse2: Pulse { is_pulse2: true, ..Default::default() },
            triangle: Triangle::default(),
            noise: Noise::new(),
            dmc: Dmc::default(),
            frame_mode_5step: false,
            frame_irq_inhibit: false,
            frame_irq: false,
            frame_counter: 0,
            sample_accum: 0.0,
            samples: Vec::new(),
        }
    }

    /// Advance the APU by one CPU cycle. The triangle clocks every CPU cycle;
    /// pulse/noise clock every other cycle.
    pub fn step(&mut self) {
        self.triangle.clock_timer();
        if self.frame_counter & 1 == 0 {
            self.pulse1.clock_timer();
            self.pulse2.clock_timer();
            self.noise.clock_timer();
        }

        self.frame_counter += 1;
        // Frame sequencer steps at ~3729, 7457, 11186, 14915 (4-step) CPU
        // cycles. We approximate with quarter-frame ticks every 7457 cycles.
        // Period of a quarter frame ≈ 7457 cycles.
        if self.frame_counter % 7457 == 0 {
            self.frame_quarter();
            let step = (self.frame_counter / 7457) % if self.frame_mode_5step { 5 } else { 4 };
            // Half-frame on steps 2 and 4 (4-step) — clock length+sweep.
            if step == 2 || step == 0 {
                self.frame_half();
            }
            if !self.frame_mode_5step && step == 0 && !self.frame_irq_inhibit {
                self.frame_irq = true;
            }
        }

        // Downsample to the host rate.
        self.sample_accum += SAMPLE_RATE / CPU_HZ;
        if self.sample_accum >= 1.0 {
            self.sample_accum -= 1.0;
            let s = self.mix();
            self.samples.push(s);
        }
    }

    fn frame_quarter(&mut self) {
        self.pulse1.env.clock();
        self.pulse2.env.clock();
        self.noise.env.clock();
        self.triangle.clock_linear();
    }
    fn frame_half(&mut self) {
        self.pulse1.clock_length();
        self.pulse2.clock_length();
        self.triangle.clock_length();
        self.noise.clock_length();
        self.pulse1.clock_sweep();
        self.pulse2.clock_sweep();
    }

    /// Non-linear NES mixer (NESdev "APU Mixer" approximation).
    fn mix(&self) -> f32 {
        let p1 = self.pulse1.output() as f64;
        let p2 = self.pulse2.output() as f64;
        let t = self.triangle.output() as f64;
        let n = self.noise.output() as f64;
        let d = self.dmc.level as f64;

        let pulse_out = if p1 + p2 == 0.0 {
            0.0
        } else {
            95.88 / (8128.0 / (p1 + p2) + 100.0)
        };
        let tnd_out = if t + n + d == 0.0 {
            0.0
        } else {
            159.79 / (1.0 / (t / 8227.0 + n / 12241.0 + d / 22638.0) + 100.0)
        };
        (pulse_out + tnd_out) as f32
    }

    // ================= register interface ($4000-$4017) =================

    pub fn write_reg(&mut self, addr: u16, v: u8) {
        match addr {
            0x4000 => {
                self.pulse1.duty = v >> 6;
                self.pulse1.length_halt = v & 0x20 != 0;
                self.pulse1.env.loop_flag = v & 0x20 != 0;
                self.pulse1.env.constant = v & 0x10 != 0;
                self.pulse1.env.volume = v & 0x0F;
            }
            0x4001 => {
                self.pulse1.sweep_enable = v & 0x80 != 0;
                self.pulse1.sweep_period = (v >> 4) & 0x07;
                self.pulse1.sweep_negate = v & 0x08 != 0;
                self.pulse1.sweep_shift = v & 0x07;
                self.pulse1.sweep_reload = true;
            }
            0x4002 => self.pulse1.timer_period = (self.pulse1.timer_period & 0x700) | v as u16,
            0x4003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0xFF) | (((v as u16) & 0x07) << 8);
                if self.pulse1.enabled {
                    self.pulse1.length = LENGTH_TABLE[(v >> 3) as usize];
                }
                self.pulse1.env.start = true;
                self.pulse1.duty_pos = 0;
            }

            0x4004 => {
                self.pulse2.duty = v >> 6;
                self.pulse2.length_halt = v & 0x20 != 0;
                self.pulse2.env.loop_flag = v & 0x20 != 0;
                self.pulse2.env.constant = v & 0x10 != 0;
                self.pulse2.env.volume = v & 0x0F;
            }
            0x4005 => {
                self.pulse2.sweep_enable = v & 0x80 != 0;
                self.pulse2.sweep_period = (v >> 4) & 0x07;
                self.pulse2.sweep_negate = v & 0x08 != 0;
                self.pulse2.sweep_shift = v & 0x07;
                self.pulse2.sweep_reload = true;
            }
            0x4006 => self.pulse2.timer_period = (self.pulse2.timer_period & 0x700) | v as u16,
            0x4007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0xFF) | (((v as u16) & 0x07) << 8);
                if self.pulse2.enabled {
                    self.pulse2.length = LENGTH_TABLE[(v >> 3) as usize];
                }
                self.pulse2.env.start = true;
                self.pulse2.duty_pos = 0;
            }

            0x4008 => {
                self.triangle.length_halt = v & 0x80 != 0;
                self.triangle.linear_reload_value = v & 0x7F;
            }
            0x400A => self.triangle.timer_period = (self.triangle.timer_period & 0x700) | v as u16,
            0x400B => {
                self.triangle.timer_period =
                    (self.triangle.timer_period & 0xFF) | (((v as u16) & 0x07) << 8);
                if self.triangle.enabled {
                    self.triangle.length = LENGTH_TABLE[(v >> 3) as usize];
                }
                self.triangle.linear_reload = true;
            }

            0x400C => {
                self.noise.length_halt = v & 0x20 != 0;
                self.noise.env.loop_flag = v & 0x20 != 0;
                self.noise.env.constant = v & 0x10 != 0;
                self.noise.env.volume = v & 0x0F;
            }
            0x400E => {
                self.noise.mode = v & 0x80 != 0;
                self.noise.timer_period = NOISE_PERIOD[(v & 0x0F) as usize];
            }
            0x400F => {
                if self.noise.enabled {
                    self.noise.length = LENGTH_TABLE[(v >> 3) as usize];
                }
                self.noise.env.start = true;
            }

            0x4010 => { /* DMC rate/IRQ — not modelled past level */ }
            0x4011 => self.dmc.level = v & 0x7F,
            0x4012 => {}
            0x4013 => {}

            0x4015 => {
                // Channel enable. Disabling clears the length counter.
                self.pulse1.enabled = v & 0x01 != 0;
                if !self.pulse1.enabled { self.pulse1.length = 0; }
                self.pulse2.enabled = v & 0x02 != 0;
                if !self.pulse2.enabled { self.pulse2.length = 0; }
                self.triangle.enabled = v & 0x04 != 0;
                if !self.triangle.enabled { self.triangle.length = 0; }
                self.noise.enabled = v & 0x08 != 0;
                if !self.noise.enabled { self.noise.length = 0; }
                self.dmc.enabled = v & 0x10 != 0;
            }
            0x4017 => {
                self.frame_mode_5step = v & 0x80 != 0;
                self.frame_irq_inhibit = v & 0x40 != 0;
                if self.frame_irq_inhibit {
                    self.frame_irq = false;
                }
                self.frame_counter = 0;
                if self.frame_mode_5step {
                    // 5-step mode immediately clocks quarter + half frames.
                    self.frame_quarter();
                    self.frame_half();
                }
            }
            _ => {}
        }
    }

    /// $4015 read: channel length-counter status + frame IRQ flag (cleared on
    /// read).
    pub fn read_status(&mut self) -> u8 {
        let mut s = 0u8;
        if self.pulse1.length > 0 { s |= 0x01; }
        if self.pulse2.length > 0 { s |= 0x02; }
        if self.triangle.length > 0 { s |= 0x04; }
        if self.noise.length > 0 { s |= 0x08; }
        if self.frame_irq { s |= 0x40; }
        self.frame_irq = false;
        s
    }

    /// Take all samples accumulated since the last call.
    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_counter_loads_and_status() {
        let mut apu = Apu::new();
        apu.write_reg(0x4015, 0x01); // enable pulse1
        apu.write_reg(0x4003, 0x08); // length index 1 -> 254
        assert!(apu.read_status() & 0x01 != 0);
    }

    #[test]
    fn disabling_channel_clears_length() {
        let mut apu = Apu::new();
        apu.write_reg(0x4015, 0x01);
        apu.write_reg(0x4003, 0x08);
        apu.write_reg(0x4015, 0x00); // disable
        assert!(apu.read_status() & 0x01 == 0);
    }

    #[test]
    fn produces_samples_over_time() {
        let mut apu = Apu::new();
        apu.write_reg(0x4015, 0x01);
        apu.write_reg(0x4002, 0x80);
        apu.write_reg(0x4003, 0x08);
        for _ in 0..40_000 {
            apu.step();
        }
        let s = apu.drain();
        // ~44100/1789773 * 40000 ≈ 985 samples.
        assert!(s.len() > 800 && s.len() < 1100);
    }
}
