//! WonderSwan sound: 4 channels driven by the audio I/O registers, mixed to a
//! mono f32 stream at the host sample rate. Built from the WonderSwan dev wiki
//! ("Sound").
//!
//! Each channel has an 11-bit period register and a 4-bit-per-side volume:
//!   * Channel 1: square/tone.
//!   * Channel 2: tone, or 4-bit PCM "voice" when voice mode is enabled.
//!   * Channel 3: tone with an optional frequency sweep.
//!   * Channel 4: tone, or LFSR noise when noise mode is enabled.
//!
//! We synthesize at the host sample rate directly: each channel keeps a phase
//! accumulator advanced by its frequency. This is an approximation of the real
//! hardware's sample-rate generator but is faithful enough for recognizable
//! audio. `drain` returns and clears the accumulated sample buffer.

/// Host output sample rate.
pub const SAMPLE_RATE: u32 = 44100;
/// WonderSwan master sound clock used to convert period -> frequency. The sound
/// generator runs at the CPU clock / 128; tone frequency = clock / (2048-period).
const SOUND_CLOCK: f32 = 3_072_000.0 / 128.0;

#[derive(Clone, Copy, Default)]
struct Channel {
    period: u16,    // 11-bit
    vol_left: u8,   // 0..15
    vol_right: u8,  // 0..15
    enabled: bool,
    phase: f32,     // 0.0..1.0 accumulator
    // Channel-specific extras.
    noise_lfsr: u16,
    // Channel-3 sweep registers are latched but the sweep step is not yet
    // applied (see lib.rs "stubbed" notes); kept for the register interface.
    #[allow(dead_code)]
    sweep_value: i8,
    #[allow(dead_code)]
    sweep_time: u8,
    voice_sample: u8, // channel 2 PCM level
}

pub struct Audio {
    ch: [Channel; 4],
    /// Master sound control ($90): per-channel enable bits.
    pub ctrl: u8,
    /// Output control ($91): master enable + volume shift.
    pub output: u8,
    /// Voice/noise/sweep mode selector ($92-$8F region collapsed): bit per ch2
    /// voice, ch3 sweep, ch4 noise.
    pub mode: u8,

    samples: Vec<f32>,
    /// Fractional sample accumulator: how many host samples are owed per CPU
    /// cycle fed in.
    sample_accum: f32,
}

impl Default for Audio {
    fn default() -> Self {
        Audio::new()
    }
}

impl Audio {
    pub fn new() -> Audio {
        Audio {
            ch: [Channel {
                noise_lfsr: 0x7FFF,
                ..Default::default()
            }; 4],
            ctrl: 0,
            output: 0,
            mode: 0,
            samples: Vec::new(),
            sample_accum: 0.0,
        }
    }

    /// Write a sound period register (low/high). `idx` is the channel, `high`
    /// selects the high (period bits 8-10) vs low byte.
    pub fn write_period(&mut self, idx: usize, high: bool, v: u8) {
        let c = &mut self.ch[idx & 3];
        if high {
            c.period = (c.period & 0x00FF) | (((v as u16) & 0x07) << 8);
        } else {
            c.period = (c.period & 0x0700) | v as u16;
        }
    }

    /// Write a per-channel volume register ($88-$8B): high nibble left, low right.
    pub fn write_volume(&mut self, idx: usize, v: u8) {
        let c = &mut self.ch[idx & 3];
        c.vol_left = v >> 4;
        c.vol_right = v & 0x0F;
    }

    /// Write the master sound control ($90).
    pub fn write_ctrl(&mut self, v: u8) {
        self.ctrl = v;
        for i in 0..4 {
            self.ch[i].enabled = v & (1 << i) != 0;
        }
    }

    pub fn write_output(&mut self, v: u8) {
        self.output = v;
    }
    pub fn write_mode(&mut self, v: u8) {
        self.mode = v;
    }
    pub fn write_voice(&mut self, level: u8) {
        self.ch[1].voice_sample = level & 0x0F;
    }

    #[inline]
    fn channel_freq(period: u16) -> f32 {
        let p = 2048u16.saturating_sub(period & 0x7FF).max(1);
        SOUND_CLOCK / p as f32
    }

    /// Advance the sound generator by `cpu_cycles` and emit host samples.
    pub fn step(&mut self, cpu_cycles: u32) {
        // CPU runs ~3.072 MHz; host wants SAMPLE_RATE samples/sec.
        let samples_per_cycle = SAMPLE_RATE as f32 / 3_072_000.0;
        self.sample_accum += cpu_cycles as f32 * samples_per_cycle;
        let n = self.sample_accum as u32;
        self.sample_accum -= n as f32;
        for _ in 0..n {
            let s = self.mix_one();
            self.samples.push(s);
        }
    }

    fn mix_one(&mut self) -> f32 {
        let mut acc = 0.0f32;
        let dt = 1.0 / SAMPLE_RATE as f32;
        let master_on = self.output & 0x80 != 0 || self.output == 0; // tolerate 0
        for i in 0..4 {
            let c = &mut self.ch[i];
            if !c.enabled {
                continue;
            }
            let freq = Self::channel_freq(c.period);
            c.phase += freq * dt;
            if c.phase >= 1.0 {
                c.phase -= c.phase.floor();
                // Advance the noise LFSR once per period for channel 4.
                if i == 3 {
                    let bit = (c.noise_lfsr ^ (c.noise_lfsr >> 1)) & 1;
                    c.noise_lfsr = (c.noise_lfsr >> 1) | (bit << 14);
                }
            }
            let vol = (c.vol_left as f32 + c.vol_right as f32) / 30.0; // 0..1
            let sample = if i == 3 && (self.mode & 0x80 != 0) {
                // Noise.
                if c.noise_lfsr & 1 != 0 {
                    1.0
                } else {
                    -1.0
                }
            } else if i == 1 && (self.mode & 0x04 != 0) {
                // Voice (PCM): centered level.
                (c.voice_sample as f32 / 7.5) - 1.0
            } else {
                // Square tone, 50% duty.
                if c.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            };
            acc += sample * vol;
        }
        if !master_on {
            return 0.0;
        }
        // Scale down to avoid clipping with 4 channels at full volume.
        (acc * 0.25).clamp(-1.0, 1.0)
    }

    /// Take and clear the accumulated samples.
    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_low_high_compose() {
        let mut a = Audio::new();
        a.write_period(0, false, 0x34);
        a.write_period(0, true, 0x05);
        assert_eq!(a.ch[0].period, 0x534);
    }

    #[test]
    fn volume_split() {
        let mut a = Audio::new();
        a.write_volume(2, 0xA3);
        assert_eq!(a.ch[2].vol_left, 0xA);
        assert_eq!(a.ch[2].vol_right, 0x3);
    }

    #[test]
    fn enable_bits() {
        let mut a = Audio::new();
        a.write_ctrl(0b0101);
        assert!(a.ch[0].enabled);
        assert!(!a.ch[1].enabled);
        assert!(a.ch[2].enabled);
    }

    #[test]
    fn step_produces_samples() {
        let mut a = Audio::new();
        a.write_ctrl(0x01);
        a.write_volume(0, 0xFF);
        a.write_period(0, false, 0x00);
        a.write_period(0, true, 0x04); // period 0x400
        a.step(3_072_000 / 60); // ~one frame of cycles
        let s = a.drain();
        assert!(s.len() > 600, "should emit ~735 samples/frame, got {}", s.len());
        // A tone with non-zero volume must produce some non-zero output.
        assert!(s.iter().any(|&v| v != 0.0));
    }

    #[test]
    fn drain_clears() {
        let mut a = Audio::new();
        a.write_ctrl(0x01);
        a.write_volume(0, 0xFF);
        a.step(100000);
        let _ = a.drain();
        assert!(a.drain().is_empty());
    }
}
