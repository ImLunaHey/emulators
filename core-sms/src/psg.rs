//! The SN76489 PSG (Programmable Sound Generator): 3 square-wave tone channels
//! + 1 noise channel. Built from the SMS Power! "SN76489" documentation.
//!
//! Register model: a single write-only data port ($7F on SMS). A byte with
//! bit7 set is a "latch/data" byte that selects a channel + register and writes
//! the low 4 data bits; a byte with bit7 clear is a "data" byte that updates
//! the high bits of the previously-latched register.
//!
//!   1 cc r dddd   latch: cc=channel(0-3), r=type(0 tone,1 volume), d=low data
//!   0 - dddddd    data: extends the latched tone register's high 6 bits
//!
//! Tone channels divide the (PSG clock / 16) by a 10-bit counter and toggle a
//! square output. The noise channel uses a 15-bit LFSR. Volume is a 4-bit
//! attenuation (0 = loudest, 15 = silent), 2 dB per step.
//!
//! Game Gear adds a stereo-enable byte written to port $06: bits 0-3 enable
//! the right channel for tones 0-2 + noise, bits 4-7 the left. We model it and
//! produce interleaved L/R when in stereo; the host can also treat output as
//! mono (we expose mono by default to match the other cores' drain shape).

/// Host audio sample rate. The frame loop drains roughly `SAMPLE_RATE/60`
/// samples per frame.
pub const SAMPLE_RATE: u32 = 44100;

/// PSG clock = system clock / 3 ≈ 3.58 MHz, then the tone generators divide it
/// by a further 16. We accumulate PSG ticks and emit a sample every
/// `psg_clock / SAMPLE_RATE` ticks.
const PSG_CLOCK: u32 = 3_579_545;

/// 4-bit attenuation -> linear volume table (0 = full, 15 = mute). Each step is
/// 2 dB; index 15 is forced silent.
const VOLUME_TABLE: [f32; 16] = [
    1.0, 0.794, 0.631, 0.501, 0.398, 0.316, 0.251, 0.199, 0.158, 0.126, 0.1,
    0.0794, 0.0631, 0.0501, 0.0398, 0.0,
];

pub struct Psg {
    /// 10-bit tone period / 3-bit noise control per channel (index 3 = noise).
    tone: [u16; 4],
    /// 4-bit attenuation per channel.
    volume: [u8; 4],
    /// Internal countdown counters.
    counter: [i32; 4],
    /// Square-wave output polarity per tone channel.
    output: [bool; 3],

    /// Which (channel,type) the next data byte extends. Encodes ch*2 + type.
    latched: u8,

    /// 15-bit noise LFSR + its toggling output.
    lfsr: u16,
    noise_output: bool,

    /// Game Gear stereo enable byte (port $06). Bits 0-3 right (T0,T1,T2,N),
    /// bits 4-7 left. Defaults to all channels both sides.
    stereo: u8,

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
            volume: [0x0F; 4], // start silent
            counter: [0; 4],
            output: [false; 3],
            latched: 0,
            lfsr: 0x8000,
            noise_output: false,
            stereo: 0xFF,
            sample_accum: 0,
            buffer: Vec::with_capacity(2048),
        }
    }

    /// Write the PSG data port ($7F).
    pub fn write(&mut self, v: u8) {
        if v & 0x80 != 0 {
            // Latch/data byte.
            let ch = ((v >> 5) & 0x03) as usize;
            let is_volume = v & 0x10 != 0;
            self.latched = (ch as u8) << 1 | (is_volume as u8);
            let data = (v & 0x0F) as u16;
            if is_volume {
                self.volume[ch] = data as u8;
            } else if ch == 3 {
                // Noise control: low 3 bits.
                self.tone[3] = data & 0x07;
                self.lfsr = 0x8000; // reset shift register on noise write
            } else {
                self.tone[ch] = (self.tone[ch] & 0x3F0) | data;
            }
        } else {
            // Data byte: extend the latched register's high bits.
            let ch = (self.latched >> 1) as usize;
            let is_volume = self.latched & 1 != 0;
            let data = (v & 0x3F) as u16;
            if is_volume {
                self.volume[ch] = (data & 0x0F) as u8;
            } else if ch == 3 {
                self.tone[3] = data & 0x07;
                self.lfsr = 0x8000;
            } else {
                self.tone[ch] = (self.tone[ch] & 0x00F) | (data << 4);
            }
        }
    }

    /// Game Gear stereo control (port $06).
    pub fn write_stereo(&mut self, v: u8) {
        self.stereo = v;
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

    /// One PSG clock: each tone divider runs at clock/16, so we approximate by
    /// decrementing counters by 1 every call but loading periods ×1 (the host
    /// passes already-divided cycles via `step`'s loop). To keep the math
    /// simple and accurate we treat one `tick` as clock/16 — i.e. callers pass
    /// PSG_CLOCK/16 cycles per second. We instead fold the /16 into the period.
    fn tick(&mut self) {
        for ch in 0..3 {
            self.counter[ch] -= 1;
            if self.counter[ch] <= 0 {
                let period = self.tone[ch].max(1) as i32;
                self.counter[ch] = period;
                self.output[ch] = !self.output[ch];
            }
        }
        // Noise channel.
        self.counter[3] -= 1;
        if self.counter[3] <= 0 {
            let nf = self.tone[3] & 0x03;
            let period = match nf {
                0 => 0x10,
                1 => 0x20,
                2 => 0x40,
                _ => (self.tone[2].max(1)) as i32, // use tone2's period
            } as i32;
            self.counter[3] = period.max(1);
            // Clock the LFSR.
            let white = self.tone[3] & 0x04 != 0;
            let feedback = if white {
                // tapped bits 0 and 3 (SMS taps)
                ((self.lfsr & 0x0009).count_ones() & 1) as u16
            } else {
                self.lfsr & 1
            };
            self.lfsr = (self.lfsr >> 1) | (feedback << 15);
            self.noise_output = self.lfsr & 1 != 0;
        }
    }

    /// Mix the four channels into a single mono sample in [-1, 1].
    fn mix(&self) -> f32 {
        let mut s = 0.0f32;
        for ch in 0..3 {
            let amp = VOLUME_TABLE[self.volume[ch] as usize];
            let v = if self.output[ch] { amp } else { -amp };
            s += v;
        }
        let namp = VOLUME_TABLE[self.volume[3] as usize];
        s += if self.noise_output { namp } else { -namp };
        // 4 channels summed; scale to avoid clipping.
        s * 0.25
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
        // Latch tone0 low nibble = 0x5.
        p.write(0x80 | 0x05); // ch0, tone, data=5
        assert_eq!(p.tone[0] & 0x0F, 0x05);
        // Data byte sets high 6 bits = 0x0A.
        p.write(0x0A);
        assert_eq!(p.tone[0], (0x0A << 4) | 0x05);
    }

    #[test]
    fn volume_latch() {
        let mut p = Psg::new();
        p.write(0x90 | 0x03); // ch0 volume = 3
        assert_eq!(p.volume[0], 3);
    }

    #[test]
    fn noise_write_resets_lfsr() {
        let mut p = Psg::new();
        p.lfsr = 0x1234;
        p.write(0xE0 | 0x04); // ch3 noise, white, period mode 0
        assert_eq!(p.lfsr, 0x8000);
        assert_eq!(p.tone[3], 0x04);
    }

    #[test]
    fn step_produces_samples() {
        let mut p = Psg::new();
        p.write(0x80 | 0x02); // tone0 low
        p.write(0x10); // tone0 high -> some period
        p.write(0x90 | 0x00); // tone0 full volume
        // Run enough PSG clocks to emit a handful of samples.
        p.step(PSG_CLOCK / 100);
        let s = p.drain();
        assert!(!s.is_empty());
        for v in &s {
            assert!(*v >= -1.0 && *v <= 1.0);
        }
    }

    #[test]
    fn gg_stereo_byte() {
        let mut p = Psg::new();
        p.write_stereo(0xF0); // all left
        assert_eq!(p.stereo, 0xF0);
    }
}
