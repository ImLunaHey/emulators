//! TIA audio — two independent tone/noise channels.
//!
//! Spec: Stella Programmer's Guide §"Sound" and Eckhard Stolberg's TIA sound
//! notes. Each channel has three registers: AUDC (waveform/control, 0-15),
//! AUDF (frequency divider, 0-31), and AUDV (volume, 0-15). A channel runs a
//! 5-bit and a 4-bit polynomial counter clocked at ~30 KHz / (AUDF+1); the AUDC
//! mode selects how those polynomial outputs combine into a 1-bit waveform,
//! which is scaled by AUDV.
//!
//! This implements the documented AUDC modes (pure divided tones, the 4-bit /
//! 5-bit / 9-bit polynomial noises, and the divide-by-6/31 variants) closely
//! enough to produce recognizable game audio; it is not bit-exact against every
//! silicon corner.

/// Host sample rate. The TIA sampler in `tia.rs` emits at this rate.
pub const SAMPLE_RATE: u32 = 44100;

#[derive(Clone, Copy)]
pub struct AudioChannel {
    audc: u8,
    audf: u8,
    audv: u8,

    /// Frequency divider counter (counts down from AUDF to 0).
    div: u8,
    /// 4-bit polynomial counter state.
    poly4: u8,
    /// 5-bit polynomial counter state.
    poly5: u8,
    /// 9-bit polynomial state (used by some modes).
    poly9: u16,
    /// Current 1-bit output level.
    out: bool,
}

impl AudioChannel {
    pub fn new() -> AudioChannel {
        AudioChannel {
            audc: 0,
            audf: 0,
            audv: 0,
            div: 0,
            poly4: 1,
            poly5: 1,
            poly9: 1,
            out: false,
        }
    }

    pub fn set_control(&mut self, v: u8) {
        self.audc = v & 0x0F;
    }
    pub fn set_freq(&mut self, v: u8) {
        self.audf = v & 0x1F;
        // Reload the divider so the first full period after a frequency change
        // is audf+1 ticks long.
        self.div = self.audf;
    }
    pub fn set_volume(&mut self, v: u8) {
        self.audv = v & 0x0F;
    }

    /// Clock the channel one audio tick (called at the ~30 KHz divider rate by
    /// the TIA). Advances the frequency divider and, on its underflow, the
    /// polynomial counters per the AUDC mode.
    pub fn clock(&mut self) {
        if self.div == 0 {
            self.div = self.audf;
            self.tick_poly();
        } else {
            self.div -= 1;
        }
    }

    fn tick_poly(&mut self) {
        // Advance the 5-bit poly (used as a clock gate in several modes).
        let p5_out = self.poly5 & 1 != 0;
        let new5 = ((self.poly5 >> 3) ^ self.poly5) & 1;
        self.poly5 = (self.poly5 >> 1) | (new5 << 4);

        // The 5-bit poly gates whether the waveform generator advances this tick
        // for the "div by 31"/"poly5" modes. For pure-tone modes the gate is
        // always open.
        let gate = match self.audc {
            // Modes that use the 5-bit poly as a pre-divider.
            0x03 | 0x07 | 0x0F | 0x08..=0x0A => p5_out,
            _ => true,
        };
        if !gate {
            return;
        }

        match self.audc {
            // Silence.
            0x00 | 0x0B => self.out = false,
            // Pure tones (square wave: toggle every tick).
            0x04 | 0x05 | 0x0C | 0x0D | 0x0E => self.out = !self.out,
            // 4-bit poly noise.
            0x01 | 0x02 | 0x06 | 0x07 | 0x0F => {
                self.out = self.poly4 & 1 != 0;
                let new4 = ((self.poly4 >> 1) ^ self.poly4) & 1;
                self.poly4 = (self.poly4 >> 1) | (new4 << 3);
            }
            // 9-bit poly noise (white-ish).
            0x08 | 0x09 | 0x0A | 0x03 => {
                self.out = self.poly9 & 1 != 0;
                let new9 = ((self.poly9 >> 4) ^ self.poly9) & 1;
                self.poly9 = (self.poly9 >> 1) | (new9 << 8);
            }
            _ => self.out = !self.out,
        }
    }

    /// Current sample contribution as a small signed float.
    pub fn output(&self) -> f32 {
        if self.out {
            // Scale by volume; keep both channels summed well under 1.0.
            (self.audv as f32 / 15.0) * 0.25
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_zero_is_silent() {
        let mut ch = AudioChannel::new();
        ch.set_control(0x04); // pure tone
        ch.set_freq(0);
        ch.set_volume(0);
        for _ in 0..100 {
            ch.clock();
        }
        assert_eq!(ch.output(), 0.0);
    }

    #[test]
    fn pure_tone_toggles() {
        let mut ch = AudioChannel::new();
        ch.set_control(0x04);
        ch.set_freq(0); // divide by 1 -> toggles every clock
        ch.set_volume(15);
        let a = ch.out;
        ch.clock();
        let b = ch.out;
        assert_ne!(a, b);
    }

    #[test]
    fn freq_divider_slows_toggle() {
        let mut ch = AudioChannel::new();
        ch.set_control(0x04);
        ch.set_freq(3); // divide by 4
        ch.set_volume(15);
        let start = ch.out;
        // First few clocks just decrement the divider.
        ch.clock();
        ch.clock();
        ch.clock();
        assert_eq!(ch.out, start); // not toggled yet
        ch.clock(); // divider underflow -> toggle
        assert_ne!(ch.out, start);
    }
}
