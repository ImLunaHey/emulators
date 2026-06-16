//! The T6W28 PSG + DAC. Built from the SN76489 documentation (the T6W28 behaves
//! like a stereo pair of SN76489s sharing the tone/noise generators but with
//! independent left/right attenuation) and the NGPC sound notes
//! (jiggawatt.org/badc0de/ngpcsound + NeoPop docs).
//!
//! Register model (mirrors the SN76489): two write ports in the TLCS-900 I/O
//! area —
//!   0x00A1  TONE port: bytes with bit7 set latch a (channel,type); bytes with
//!           bit7 clear extend the latched tone period. Same encoding as the SMS
//!           SN76489 but the volume nibble here is the LEFT attenuation.
//!   0x00A0  NOISE/RIGHT port: on the T6W28 the right-channel attenuations are
//!           written here. We model it as the SN76489 second-channel volume set.
//! Plus two 8-bit DAC ports:
//!   0x00A2  DACL   0x00A3  DACR
//!
//! Tone channels divide (clock/16) by a 10-bit counter and toggle a square
//! output; the noise channel uses a 15-bit LFSR. Volume is a 4-bit attenuation
//! (0 = loudest, 15 = silent). We mix down to a mono f32 stream like the sibling
//! cores' `drain_audio` contract.

/// Host audio sample rate. The frame loop drains roughly `SAMPLE_RATE/60`
/// samples per frame.
pub const SAMPLE_RATE: u32 = 44100;

/// PSG clock ≈ 3.072 MHz (the NGPC sound clock), then the tone generators
/// divide it by a further 16; we fold the /16 into the period.
const PSG_CLOCK: u32 = 3_072_000;

/// 4-bit attenuation -> linear volume table (0 = full, 15 = mute), 2 dB/step.
const VOLUME_TABLE: [f32; 16] = [
    1.0, 0.794, 0.631, 0.501, 0.398, 0.316, 0.251, 0.199, 0.158, 0.126, 0.1, 0.0794, 0.0631,
    0.0501, 0.0398, 0.0,
];

pub struct Psg {
    /// 10-bit tone period per channel (index 3 = noise control).
    tone: [u16; 4],
    /// Left 4-bit attenuation per channel.
    vol_l: [u8; 4],
    /// Right 4-bit attenuation per channel.
    vol_r: [u8; 4],
    /// Internal countdown counters.
    counter: [i32; 4],
    /// Square-wave output polarity per tone channel.
    output: [bool; 3],

    /// Which (channel,type) the next data byte extends. Encodes ch*2 + type.
    latched_l: u8,
    latched_r: u8,

    /// 15-bit noise LFSR + its toggling output.
    lfsr: u16,
    noise_output: bool,

    /// 8-bit DAC outputs (signed center 0x80).
    dac_l: u8,
    dac_r: u8,

    /// Fractional accumulator for sample-rate conversion.
    sample_accum: u32,

    /// Mono sample buffer drained by the host each frame.
    buffer: Vec<f32>,
}

impl Default for Psg {
    fn default() -> Self {
        Psg::new()
    }
}

impl Psg {
    pub fn new() -> Psg {
        Psg {
            tone: [0; 4],
            vol_l: [0x0F; 4],
            vol_r: [0x0F; 4],
            counter: [0; 4],
            output: [false; 3],
            latched_l: 0,
            latched_r: 0,
            lfsr: 0x8000,
            noise_output: false,
            dac_l: 0x80,
            dac_r: 0x80,
            sample_accum: 0,
            buffer: Vec::with_capacity(2048),
        }
    }

    /// Write the LEFT/tone SN76489 port (0x00A1).
    pub fn write_left(&mut self, v: u8) {
        Self::write_chip(
            v,
            &mut self.tone,
            &mut self.vol_l,
            &mut self.latched_l,
            &mut self.lfsr,
        );
    }

    /// Write the RIGHT/noise SN76489 port (0x00A0). Right attenuations only.
    pub fn write_right(&mut self, v: u8) {
        // The right chip shares the same tone periods; we only let it update the
        // right-side volumes + the noise register (so noise isn't doubled).
        let mut scratch_tone = self.tone;
        Self::write_chip(
            v,
            &mut scratch_tone,
            &mut self.vol_r,
            &mut self.latched_r,
            &mut self.lfsr,
        );
        // Noise period writes still apply (shared generator).
        self.tone[3] = scratch_tone[3];
    }

    fn write_chip(v: u8, tone: &mut [u16; 4], vol: &mut [u8; 4], latched: &mut u8, lfsr: &mut u16) {
        if v & 0x80 != 0 {
            let ch = ((v >> 5) & 0x03) as usize;
            let is_volume = v & 0x10 != 0;
            *latched = (ch as u8) << 1 | (is_volume as u8);
            let data = (v & 0x0F) as u16;
            if is_volume {
                vol[ch] = data as u8;
            } else if ch == 3 {
                tone[3] = data & 0x07;
                *lfsr = 0x8000;
            } else {
                tone[ch] = (tone[ch] & 0x3F0) | data;
            }
        } else {
            let ch = (*latched >> 1) as usize;
            let is_volume = *latched & 1 != 0;
            let data = (v & 0x3F) as u16;
            if is_volume {
                vol[ch] = (data & 0x0F) as u8;
            } else if ch == 3 {
                tone[3] = data & 0x07;
                *lfsr = 0x8000;
            } else {
                tone[ch] = (tone[ch] & 0x00F) | (data << 4);
            }
        }
    }

    pub fn write_dac_l(&mut self, v: u8) {
        self.dac_l = v;
    }
    pub fn write_dac_r(&mut self, v: u8) {
        self.dac_r = v;
    }

    /// Advance the PSG by `cycles` PSG clocks and emit host-rate samples.
    pub fn step(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.tick();
            self.sample_accum += SAMPLE_RATE;
            if self.sample_accum >= PSG_CLOCK {
                self.sample_accum -= PSG_CLOCK;
                let s = self.mix();
                self.buffer.push(s);
            }
        }
    }

    fn tick(&mut self) {
        for ch in 0..3 {
            self.counter[ch] -= 1;
            if self.counter[ch] <= 0 {
                let period = self.tone[ch].max(1) as i32;
                self.counter[ch] = period;
                self.output[ch] = !self.output[ch];
            }
        }
        self.counter[3] -= 1;
        if self.counter[3] <= 0 {
            let nf = self.tone[3] & 0x03;
            let period: i32 = match nf {
                0 => 0x10,
                1 => 0x20,
                2 => 0x40,
                _ => self.tone[2].max(1) as i32,
            };
            self.counter[3] = period.max(1);
            let white = self.tone[3] & 0x04 != 0;
            let feedback = if white {
                ((self.lfsr & 0x0009).count_ones() & 1) as u16
            } else {
                self.lfsr & 1
            };
            self.lfsr = (self.lfsr >> 1) | (feedback << 15);
            self.noise_output = self.lfsr & 1 != 0;
        }
    }

    /// Mix the channels (averaging L/R attenuations) + the DACs into mono.
    fn mix(&self) -> f32 {
        let mut s = 0.0f32;
        for ch in 0..3 {
            let amp = (VOLUME_TABLE[self.vol_l[ch] as usize] + VOLUME_TABLE[self.vol_r[ch] as usize])
                * 0.5;
            s += if self.output[ch] { amp } else { -amp };
        }
        let namp =
            (VOLUME_TABLE[self.vol_l[3] as usize] + VOLUME_TABLE[self.vol_r[3] as usize]) * 0.5;
        s += if self.noise_output { namp } else { -namp };
        // DAC: signed-center, scaled down.
        let dac = ((self.dac_l as i32 - 0x80) + (self.dac_r as i32 - 0x80)) as f32 / 256.0;
        (s * 0.2 + dac * 0.5).clamp(-1.0, 1.0)
    }

    /// Drain accumulated mono samples (host resamples to its output rate).
    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latch_sets_tone_low_then_high() {
        let mut p = Psg::new();
        p.write_left(0x80 | 0x05); // ch0, tone, data=5
        assert_eq!(p.tone[0] & 0x0F, 0x05);
        p.write_left(0x0A);
        assert_eq!(p.tone[0], (0x0A << 4) | 0x05);
    }

    #[test]
    fn left_volume_latch() {
        let mut p = Psg::new();
        p.write_left(0x90 | 0x03); // ch0 left volume = 3
        assert_eq!(p.vol_l[0], 3);
    }

    #[test]
    fn right_volume_independent() {
        let mut p = Psg::new();
        p.write_left(0x90 | 0x00); // ch0 left = 0 (loud)
        p.write_right(0x90 | 0x0F); // ch0 right = 15 (silent)
        assert_eq!(p.vol_l[0], 0);
        assert_eq!(p.vol_r[0], 0x0F);
    }

    #[test]
    fn noise_write_resets_lfsr() {
        let mut p = Psg::new();
        p.lfsr = 0x1234;
        p.write_left(0xE0 | 0x04);
        assert_eq!(p.lfsr, 0x8000);
        assert_eq!(p.tone[3], 0x04);
    }

    #[test]
    fn step_produces_bounded_samples() {
        let mut p = Psg::new();
        p.write_left(0x80 | 0x02);
        p.write_left(0x10);
        p.write_left(0x90 | 0x00);
        p.step(PSG_CLOCK / 100);
        let s = p.drain();
        assert!(!s.is_empty());
        for v in &s {
            assert!(*v >= -1.0 && *v <= 1.0);
        }
    }

    #[test]
    fn dac_writes() {
        let mut p = Psg::new();
        p.write_dac_l(0xFF);
        p.write_dac_r(0x00);
        assert_eq!(p.dac_l, 0xFF);
        assert_eq!(p.dac_r, 0x00);
    }
}
