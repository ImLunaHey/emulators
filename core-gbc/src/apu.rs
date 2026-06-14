//! The APU: 4 channels (2 square w/ sweep, wave, noise), the frame sequencer,
//! the NR10-NR52 registers, and an f32 stereo sample buffer.
//!
//! Spec: Pan Docs — Audio / Sound Controller (gbdev.io/pandocs/Audio.html and
//! the per-register pages).
//!
//! The APU is clocked at the CPU's base rate (4194304 Hz, *not* affected by CGB
//! double-speed for the audio output rate — double-speed only changes how the
//! CPU issues writes). A 512 Hz frame sequencer drives length counters (256
//! Hz), the volume envelopes (64 Hz), and channel 1's frequency sweep (128 Hz).
//!
//! We generate samples by downsampling the channel outputs to a target host
//! rate; the integration loop steps the APU by T-cycles and drains the produced
//! interleaved-stereo f32 samples once per frame.

const FRAME_SEQ_PERIOD: u32 = 8192; // 4194304 / 512 Hz
/// Host output sample rate (drained as interleaved stereo f32).
pub const SAMPLE_RATE: u32 = 48000;
const CYCLES_PER_SAMPLE_NUM: u32 = 4_194_304;

/// A volume envelope shared by the two square channels and the noise channel.
#[derive(Clone, Copy, Default)]
struct Envelope {
    initial: u8,    // initial volume (0-15)
    add_mode: bool, // true = increase
    period: u8,     // envelope period (0 = disabled)
    volume: u8,     // current volume
    timer: u8,
}

impl Envelope {
    fn trigger(&mut self) {
        self.volume = self.initial;
        self.timer = if self.period == 0 { 8 } else { self.period };
    }
    fn step(&mut self) {
        if self.period == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = self.period;
            if self.add_mode && self.volume < 15 {
                self.volume += 1;
            } else if !self.add_mode && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
    fn write(&mut self, v: u8) {
        self.initial = v >> 4;
        self.add_mode = v & 0x08 != 0;
        self.period = v & 0x07;
    }
    fn read(&self) -> u8 {
        (self.initial << 4) | ((self.add_mode as u8) << 3) | self.period
    }
    /// DAC powered = any of the upper 5 bits of NRx2 set.
    fn dac_on(&self) -> bool {
        (self.initial | (self.add_mode as u8)) != 0
    }
}

/// Square channels 1 & 2 (channel 1 adds frequency sweep).
#[derive(Clone, Copy, Default)]
struct Square {
    enabled: bool,
    dac_on: bool,
    duty: u8,         // 0-3
    duty_step: u8,    // 0-7
    freq: u16,        // 11-bit
    timer: i32,       // frequency timer (T-cycles to next duty step)
    length_counter: u16,
    length_enable: bool,
    env: Envelope,
    // sweep (channel 1 only)
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    sweep_shadow: u16,
    nrx0: u8, // raw NR10 (sweep) for readback
}

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

impl Square {
    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += ((2048 - self.freq as i32) * 4).max(1);
            self.duty_step = (self.duty_step + 1) & 7;
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        DUTY_TABLE[self.duty as usize][self.duty_step as usize] * self.env.volume
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
    fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.timer = ((2048 - self.freq as i32) * 4).max(1);
        self.env.trigger();
        // sweep init
        self.sweep_shadow = self.freq;
        self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
        self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
        if self.sweep_shift != 0 {
            self.sweep_calc(); // overflow check on trigger
        }
        if !self.dac_on {
            self.enabled = false;
        }
    }
    fn sweep_calc(&mut self) -> u16 {
        let delta = self.sweep_shadow >> self.sweep_shift;
        let new = if self.sweep_negate {
            self.sweep_shadow.wrapping_sub(delta)
        } else {
            self.sweep_shadow.wrapping_add(delta)
        };
        if new > 2047 {
            self.enabled = false;
        }
        new
    }
    fn sweep_tick(&mut self) {
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
            if self.sweep_enabled && self.sweep_period != 0 {
                let new = self.sweep_calc();
                if new <= 2047 && self.sweep_shift != 0 {
                    self.sweep_shadow = new;
                    self.freq = new;
                    self.sweep_calc();
                }
            }
        }
    }
}

/// Wave channel 3.
#[derive(Clone, Default)]
struct Wave {
    enabled: bool,
    dac_on: bool,
    freq: u16,
    timer: i32,
    position: u8, // 0-31
    length_counter: u16,
    length_enable: bool,
    volume_code: u8, // 0-3
    ram: [u8; 16],
}

impl Wave {
    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += ((2048 - self.freq as i32) * 2).max(1);
            self.position = (self.position + 1) & 31;
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        let byte = self.ram[(self.position / 2) as usize];
        let sample = if self.position & 1 == 0 { byte >> 4 } else { byte & 0x0F };
        match self.volume_code {
            0 => 0,
            1 => sample,
            2 => sample >> 1,
            _ => sample >> 2,
        }
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
    fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 256;
        }
        self.timer = ((2048 - self.freq as i32) * 2).max(1);
        self.position = 0;
        if !self.dac_on {
            self.enabled = false;
        }
    }
}

/// Noise channel 4.
#[derive(Clone, Copy, Default)]
struct Noise {
    enabled: bool,
    dac_on: bool,
    timer: i32,
    lfsr: u16,
    width_7bit: bool,
    clock_shift: u8,
    divisor_code: u8,
    length_counter: u16,
    length_enable: bool,
    env: Envelope,
}

impl Noise {
    fn divisor(code: u8) -> i32 {
        match code {
            0 => 8,
            n => (n as i32) * 16,
        }
    }
    fn step(&mut self, cycles: i32) {
        let period = Noise::divisor(self.divisor_code) << self.clock_shift;
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += period.max(1);
            let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 14);
            if self.width_7bit {
                self.lfsr = (self.lfsr & !0x40) | (bit << 6);
            }
        }
    }
    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        // Output is the inverted low bit of the LFSR.
        ((!self.lfsr & 1) as u8) * self.env.volume
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
    fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.lfsr = 0x7FFF;
        self.timer = (Noise::divisor(self.divisor_code) << self.clock_shift).max(1);
        self.env.trigger();
        if !self.dac_on {
            self.enabled = false;
        }
    }
}

pub struct Apu {
    ch1: Square,
    ch2: Square,
    ch3: Wave,
    ch4: Noise,

    /// Master enable (NR52 bit 7).
    power: bool,
    /// NR50: master volume + VIN.
    nr50: u8,
    /// NR51: channel→output panning.
    nr51: u8,

    frame_seq_timer: u32,
    frame_seq_step: u8,

    sample_accum: u32, // fixed-point accumulator for downsampling
    buffer: Vec<f32>,
}

impl Default for Apu {
    fn default() -> Self {
        Apu::new()
    }
}

impl Apu {
    pub fn new() -> Self {
        Apu {
            ch1: Square::default(),
            ch2: Square::default(),
            ch3: Wave::default(),
            ch4: Noise::default(),
            power: false,
            nr50: 0,
            nr51: 0,
            frame_seq_timer: FRAME_SEQ_PERIOD,
            frame_seq_step: 0,
            sample_accum: 0,
            buffer: Vec::with_capacity(2048),
        }
    }

    /// Advance the APU by `cycles` base T-cycles (the audio output is *not*
    /// double-speed scaled — the caller passes un-scaled cycles).
    pub fn step(&mut self, cycles: u32) {
        for _ in 0..cycles {
            // Frame sequencer.
            self.frame_seq_timer -= 1;
            if self.frame_seq_timer == 0 {
                self.frame_seq_timer = FRAME_SEQ_PERIOD;
                self.frame_sequencer_step();
            }

            if self.power {
                self.ch1.step(1);
                self.ch2.step(1);
                self.ch3.step(1);
                self.ch4.step(1);
            }

            // Downsample to the host rate.
            self.sample_accum += SAMPLE_RATE;
            if self.sample_accum >= CYCLES_PER_SAMPLE_NUM {
                self.sample_accum -= CYCLES_PER_SAMPLE_NUM;
                let (l, r) = self.mix();
                self.buffer.push(l);
                self.buffer.push(r);
            }
        }
    }

    fn frame_sequencer_step(&mut self) {
        // Step pattern: length on 0/2/4/6, sweep on 2/6, envelope on 7.
        match self.frame_seq_step {
            0 | 4 => self.length_clock(),
            2 | 6 => {
                self.length_clock();
                self.ch1.sweep_tick();
            }
            7 => {
                self.ch1.env.step();
                self.ch2.env.step();
                self.ch4.env.step();
            }
            _ => {}
        }
        self.frame_seq_step = (self.frame_seq_step + 1) & 7;
    }

    fn length_clock(&mut self) {
        self.ch1.length_tick();
        self.ch2.length_tick();
        self.ch3.length_tick();
        self.ch4.length_tick();
    }

    /// Mix the four channels into stereo f32 (-1.0..1.0-ish).
    fn mix(&self) -> (f32, f32) {
        if !self.power {
            return (0.0, 0.0);
        }
        let o1 = self.ch1.output() as f32;
        let o2 = self.ch2.output() as f32;
        let o3 = self.ch3.output() as f32;
        let o4 = self.ch4.output() as f32;

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        // NR51: bits 4-7 = left, 0-3 = right; one bit per channel.
        if self.nr51 & 0x10 != 0 { left += o1; }
        if self.nr51 & 0x20 != 0 { left += o2; }
        if self.nr51 & 0x40 != 0 { left += o3; }
        if self.nr51 & 0x80 != 0 { left += o4; }
        if self.nr51 & 0x01 != 0 { right += o1; }
        if self.nr51 & 0x02 != 0 { right += o2; }
        if self.nr51 & 0x04 != 0 { right += o3; }
        if self.nr51 & 0x08 != 0 { right += o4; }

        // Master volume (NR50 bits 6-4 left, 2-0 right), 0-7 → +1 step.
        let lvol = ((self.nr50 >> 4) & 0x07) as f32 + 1.0;
        let rvol = (self.nr50 & 0x07) as f32 + 1.0;

        // Each channel is 0..15; four channels max 60, * vol(8) = 480. Normalize.
        let norm = 1.0 / (15.0 * 4.0 * 8.0);
        (left * lvol * norm, right * rvol * norm)
    }

    /// Drain produced interleaved-stereo samples since the last call.
    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.buffer)
    }

    // ============================ Registers ============================
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            // Channel 1
            0xFF10 => self.ch1.nrx0 | 0x80,
            0xFF11 => (self.ch1.duty << 6) | 0x3F,
            0xFF12 => self.ch1.env.read(),
            0xFF13 => 0xFF, // write-only
            0xFF14 => 0xBF | ((self.ch1.length_enable as u8) << 6),
            // Channel 2
            0xFF15 => 0xFF,
            0xFF16 => (self.ch2.duty << 6) | 0x3F,
            0xFF17 => self.ch2.env.read(),
            0xFF18 => 0xFF,
            0xFF19 => 0xBF | ((self.ch2.length_enable as u8) << 6),
            // Channel 3
            0xFF1A => ((self.ch3.dac_on as u8) << 7) | 0x7F,
            0xFF1B => 0xFF,
            0xFF1C => (self.ch3.volume_code << 5) | 0x9F,
            0xFF1D => 0xFF,
            0xFF1E => 0xBF | ((self.ch3.length_enable as u8) << 6),
            // Channel 4
            0xFF1F => 0xFF,
            0xFF20 => 0xFF,
            0xFF21 => self.ch4.env.read(),
            0xFF22 => {
                (self.ch4.clock_shift << 4)
                    | ((self.ch4.width_7bit as u8) << 3)
                    | self.ch4.divisor_code
            }
            0xFF23 => 0xBF | ((self.ch4.length_enable as u8) << 6),
            // Control
            0xFF24 => self.nr50,
            0xFF25 => self.nr51,
            0xFF26 => {
                let mut v = (self.power as u8) << 7;
                v |= 0x70; // unused bits read 1
                if self.ch1.enabled { v |= 0x01; }
                if self.ch2.enabled { v |= 0x02; }
                if self.ch3.enabled { v |= 0x04; }
                if self.ch4.enabled { v |= 0x08; }
                v
            }
            // Wave RAM
            0xFF30..=0xFF3F => self.ch3.ram[(addr - 0xFF30) as usize],
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, v: u8) {
        // When powered off, only NR52 and wave RAM are writable.
        if !self.power && addr != 0xFF26 && !(0xFF30..=0xFF3F).contains(&addr) {
            return;
        }
        match addr {
            // Channel 1
            0xFF10 => {
                self.ch1.nrx0 = v;
                self.ch1.sweep_period = (v >> 4) & 0x07;
                self.ch1.sweep_negate = v & 0x08 != 0;
                self.ch1.sweep_shift = v & 0x07;
            }
            0xFF11 => {
                self.ch1.duty = v >> 6;
                self.ch1.length_counter = 64 - (v & 0x3F) as u16;
            }
            0xFF12 => {
                self.ch1.env.write(v);
                self.ch1.dac_on = self.ch1.env.dac_on();
                if !self.ch1.dac_on { self.ch1.enabled = false; }
            }
            0xFF13 => self.ch1.freq = (self.ch1.freq & 0x700) | v as u16,
            0xFF14 => {
                self.ch1.freq = (self.ch1.freq & 0xFF) | ((v as u16 & 0x07) << 8);
                self.ch1.length_enable = v & 0x40 != 0;
                if v & 0x80 != 0 { self.ch1.trigger(); }
            }
            // Channel 2
            0xFF16 => {
                self.ch2.duty = v >> 6;
                self.ch2.length_counter = 64 - (v & 0x3F) as u16;
            }
            0xFF17 => {
                self.ch2.env.write(v);
                self.ch2.dac_on = self.ch2.env.dac_on();
                if !self.ch2.dac_on { self.ch2.enabled = false; }
            }
            0xFF18 => self.ch2.freq = (self.ch2.freq & 0x700) | v as u16,
            0xFF19 => {
                self.ch2.freq = (self.ch2.freq & 0xFF) | ((v as u16 & 0x07) << 8);
                self.ch2.length_enable = v & 0x40 != 0;
                if v & 0x80 != 0 { self.ch2.trigger(); }
            }
            // Channel 3
            0xFF1A => {
                self.ch3.dac_on = v & 0x80 != 0;
                if !self.ch3.dac_on { self.ch3.enabled = false; }
            }
            0xFF1B => self.ch3.length_counter = 256 - v as u16,
            0xFF1C => self.ch3.volume_code = (v >> 5) & 0x03,
            0xFF1D => self.ch3.freq = (self.ch3.freq & 0x700) | v as u16,
            0xFF1E => {
                self.ch3.freq = (self.ch3.freq & 0xFF) | ((v as u16 & 0x07) << 8);
                self.ch3.length_enable = v & 0x40 != 0;
                if v & 0x80 != 0 { self.ch3.trigger(); }
            }
            // Channel 4
            0xFF20 => self.ch4.length_counter = 64 - (v & 0x3F) as u16,
            0xFF21 => {
                self.ch4.env.write(v);
                self.ch4.dac_on = self.ch4.env.dac_on();
                if !self.ch4.dac_on { self.ch4.enabled = false; }
            }
            0xFF22 => {
                self.ch4.clock_shift = v >> 4;
                self.ch4.width_7bit = v & 0x08 != 0;
                self.ch4.divisor_code = v & 0x07;
            }
            0xFF23 => {
                self.ch4.length_enable = v & 0x40 != 0;
                if v & 0x80 != 0 { self.ch4.trigger(); }
            }
            // Control
            0xFF24 => self.nr50 = v,
            0xFF25 => self.nr51 = v,
            0xFF26 => {
                let on = v & 0x80 != 0;
                if !on && self.power {
                    // Power off: clear all registers.
                    self.power_off();
                } else if on && !self.power {
                    self.power = true;
                    self.frame_seq_step = 0;
                    self.frame_seq_timer = FRAME_SEQ_PERIOD;
                }
            }
            0xFF30..=0xFF3F => self.ch3.ram[(addr - 0xFF30) as usize] = v,
            _ => {}
        }
    }

    fn power_off(&mut self) {
        let wave_ram = self.ch3.ram;
        self.ch1 = Square::default();
        self.ch2 = Square::default();
        self.ch4 = Noise::default();
        self.ch3 = Wave::default();
        self.ch3.ram = wave_ram; // wave RAM is preserved across power-off
        self.nr50 = 0;
        self.nr51 = 0;
        self.power = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_gates_register_writes() {
        let mut apu = Apu::new();
        // APU off at boot: writing NR50 is ignored.
        apu.write(0xFF24, 0x77);
        assert_eq!(apu.read(0xFF24), 0x00);
        // Power on, then it sticks.
        apu.write(0xFF26, 0x80);
        apu.write(0xFF24, 0x77);
        assert_eq!(apu.read(0xFF24), 0x77);
    }

    #[test]
    fn channel1_trigger_enables() {
        let mut apu = Apu::new();
        apu.write(0xFF26, 0x80); // power on
        apu.write(0xFF12, 0xF0); // envelope, DAC on
        apu.write(0xFF14, 0x80); // trigger
        assert_eq!(apu.read(0xFF26) & 0x01, 0x01);
    }

    #[test]
    fn produces_samples() {
        let mut apu = Apu::new();
        apu.write(0xFF26, 0x80);
        apu.step(4_194_304 / 60); // ~one frame of cycles
        let s = apu.drain();
        assert!(!s.is_empty());
        assert_eq!(s.len() % 2, 0); // interleaved stereo
    }

    #[test]
    fn wave_ram_survives_power_off() {
        let mut apu = Apu::new();
        apu.write(0xFF26, 0x80);
        apu.write(0xFF30, 0xAB);
        apu.write(0xFF26, 0x00); // power off
        assert_eq!(apu.read(0xFF30), 0xAB);
    }
}
