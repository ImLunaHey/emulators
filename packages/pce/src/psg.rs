//! The HuC6280's built-in 6-channel wavetable PSG.
//!
//! Spec: Archaic Pixels "PSG", pcedev wiki "PSG". Each of the 6 channels has a
//! 32-byte (5-bit samples) waveform RAM, a 12-bit frequency divider, a 5-bit
//! per-channel volume, and L/R balance. A global volume + an LFO (driven by
//! channels 1↔0) round it out; channels 4 and 5 can switch to noise. We model
//! the register interface faithfully and synthesise best-effort samples at the
//! host rate.
//!
//! Register interface (in the I/O page, addresses $0800-$0809):
//!   $0800 channel select (low 3 bits)
//!   $0801 main amplitude (global L/R volume)
//!   $0802 frequency low byte (current channel)
//!   $0803 frequency high (4 bits)
//!   $0804 control: bit7 channel on, bit6 DDA mode, bits0-4 channel volume
//!   $0805 channel L/R balance (4 bits each)
//!   $0806 waveform data (writes one 5-bit sample, advances the index)
//!   $0807 noise control (channels 4/5): bit7 enable, bits0-4 frequency
//!   $0808 LFO frequency
//!   $0809 LFO control

/// Host audio sample rate.
pub const SAMPLE_RATE: u32 = 44100;

/// PSG master clock = ~3.58 MHz (the HuC6280's base clock / 2).
const PSG_CLOCK: u32 = 3_579_545;

#[derive(Clone, Copy)]
struct Channel {
    /// 12-bit frequency divider.
    freq: u16,
    /// bit7 on, bit6 DDA (direct), bits0-4 volume.
    control: u8,
    /// L/R balance (high nibble L, low nibble R).
    balance: u8,
    /// 32-byte waveform RAM (5-bit samples).
    wave: [u8; 32],
    /// Current waveform write/playback index.
    wave_idx: usize,
    /// DDA latched sample (when in direct mode).
    dda: u8,
    /// Noise enable + frequency (channels 4/5 only).
    noise_ctrl: u8,

    // synthesis state
    phase: u32,    // fixed-point phase accumulator
    noise_lfsr: u32,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            freq: 0,
            control: 0,
            balance: 0xFF,
            wave: [0; 32],
            wave_idx: 0,
            dda: 0,
            noise_ctrl: 0,
            phase: 0,
            noise_lfsr: 1,
        }
    }
    fn enabled(&self) -> bool {
        self.control & 0x80 != 0
    }
    fn volume(&self) -> u8 {
        self.control & 0x1F
    }
    fn dda_mode(&self) -> bool {
        self.control & 0x40 != 0
    }
    fn noise_enabled(&self) -> bool {
        self.noise_ctrl & 0x80 != 0
    }
}

pub struct Psg {
    ch: [Channel; 6],
    /// Currently selected channel (0-5).
    sel: usize,
    /// Global L/R amplitude ($0801).
    main_amp: u8,
    /// LFO frequency / control.
    lfo_freq: u8,
    lfo_ctrl: u8,

    /// Sample-rate conversion accumulator.
    sample_accum: u32,
    /// Mono sample buffer drained by the host.
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
            ch: [Channel::new(); 6],
            sel: 0,
            main_amp: 0,
            lfo_freq: 0,
            lfo_ctrl: 0,
            sample_accum: 0,
            buffer: Vec::with_capacity(2048),
        }
    }

    /// Write a PSG register (reg = low 4 bits of the I/O address, 0..=9).
    pub fn write(&mut self, reg: u8, v: u8) {
        match reg & 0x0F {
            0x00 => self.sel = (v & 0x07) as usize % 6,
            0x01 => self.main_amp = v,
            0x02 => {
                let c = &mut self.ch[self.sel];
                c.freq = (c.freq & 0x0F00) | v as u16;
            }
            0x03 => {
                let c = &mut self.ch[self.sel];
                c.freq = (c.freq & 0x00FF) | ((v as u16 & 0x0F) << 8);
            }
            0x04 => {
                let c = &mut self.ch[self.sel];
                let was_dda = c.dda_mode();
                c.control = v;
                // Writing control with DDA off resets the waveform index.
                if !c.dda_mode() && was_dda {
                    c.wave_idx = 0;
                }
            }
            0x05 => self.ch[self.sel].balance = v,
            0x06 => {
                let c = &mut self.ch[self.sel];
                if c.dda_mode() {
                    c.dda = v & 0x1F;
                } else {
                    c.wave[c.wave_idx & 0x1F] = v & 0x1F;
                    c.wave_idx = (c.wave_idx + 1) & 0x1F;
                }
            }
            0x07 => self.ch[self.sel].noise_ctrl = v,
            0x08 => self.lfo_freq = v,
            0x09 => self.lfo_ctrl = v,
            _ => {}
        }
    }

    /// Read back a PSG register (mostly write-only; returns the selected
    /// channel index for $0800 and 0xFF elsewhere).
    pub fn read(&self, reg: u8) -> u8 {
        match reg & 0x0F {
            0x00 => self.sel as u8,
            _ => 0xFF,
        }
    }

    /// Advance the PSG by `cpu_cycles` and emit host-rate samples.
    pub fn step(&mut self, cpu_cycles: u32) {
        for _ in 0..cpu_cycles {
            self.tick();
            self.sample_accum += SAMPLE_RATE;
            if self.sample_accum >= PSG_CLOCK {
                self.sample_accum -= PSG_CLOCK;
                let s = self.mix();
                self.buffer.push(s);
            }
        }
    }

    /// One PSG master clock: advance each channel's phase.
    fn tick(&mut self) {
        for i in 0..6 {
            let c = &mut self.ch[i];
            if !c.enabled() {
                continue;
            }
            // Noise channels (4,5) advance an LFSR.
            if i >= 4 && c.noise_enabled() {
                let nperiod = ((c.noise_ctrl & 0x1F) as u32 + 1) * 64;
                c.phase = c.phase.wrapping_add(0x1000);
                if c.phase >= nperiod << 12 {
                    c.phase -= nperiod << 12;
                    let bit = (c.noise_lfsr ^ (c.noise_lfsr >> 1)) & 1;
                    c.noise_lfsr = (c.noise_lfsr >> 1) | (bit << 17);
                }
                continue;
            }
            // Tone channel: phase advances; period is the 12-bit freq (0 => 4096).
            let period = if c.freq == 0 { 4096u32 } else { c.freq as u32 };
            c.phase = c.phase.wrapping_add(0x1000);
            if c.phase >= period << 12 {
                c.phase -= period << 12;
                if !c.dda_mode() {
                    c.wave_idx = (c.wave_idx + 1) & 0x1F;
                }
            }
        }
    }

    /// Mix all six channels into one mono sample in [-1, 1].
    fn mix(&self) -> f32 {
        let mut acc = 0.0f32;
        for i in 0..6 {
            let c = &self.ch[i];
            if !c.enabled() {
                continue;
            }
            // Sample value 0..31, centred around 16.
            let sample = if i >= 4 && c.noise_enabled() {
                if c.noise_lfsr & 1 != 0 { 31 } else { 0 }
            } else if c.dda_mode() {
                c.dda as i32
            } else {
                c.wave[c.wave_idx & 0x1F] as i32
            };
            // Per-channel volume (0..31) and global amplitude.
            let chvol = c.volume() as f32 / 31.0;
            let lvol = (c.balance >> 4) as f32 / 15.0;
            let rvol = (c.balance & 0x0F) as f32 / 15.0;
            let bal = (lvol + rvol) * 0.5;
            let gl = (self.main_amp >> 4) as f32 / 15.0;
            let gr = (self.main_amp & 0x0F) as f32 / 15.0;
            let gain = (gl + gr) * 0.5;
            let centered = (sample - 16) as f32 / 16.0;
            acc += centered * chvol * bal * gain;
        }
        // 6 channels summed; scale to keep within range.
        (acc / 6.0).clamp(-1.0, 1.0)
    }

    /// Drain accumulated mono samples.
    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_select_and_freq() {
        let mut p = Psg::new();
        p.write(0x00, 0x02); // select channel 2
        p.write(0x02, 0x34); // freq low
        p.write(0x03, 0x02); // freq high
        assert_eq!(p.ch[2].freq, 0x234);
    }

    #[test]
    fn waveform_write_advances_index() {
        let mut p = Psg::new();
        p.write(0x00, 0x00);
        p.write(0x04, 0x00); // control: DDA off, channel off
        p.write(0x06, 0x05);
        p.write(0x06, 0x0A);
        assert_eq!(p.ch[0].wave[0], 0x05);
        assert_eq!(p.ch[0].wave[1], 0x0A);
        assert_eq!(p.ch[0].wave_idx, 2);
    }

    #[test]
    fn control_enables_channel_and_volume() {
        let mut p = Psg::new();
        p.write(0x00, 0x01);
        p.write(0x04, 0x80 | 0x1F); // on, volume 31
        assert!(p.ch[1].enabled());
        assert_eq!(p.ch[1].volume(), 0x1F);
    }

    #[test]
    fn dda_mode_latches_sample() {
        let mut p = Psg::new();
        p.write(0x00, 0x00);
        p.write(0x04, 0x80 | 0x40); // on + DDA
        p.write(0x06, 0x1F);
        assert_eq!(p.ch[0].dda, 0x1F);
        assert!(p.ch[0].dda_mode());
    }

    #[test]
    fn step_produces_samples() {
        let mut p = Psg::new();
        p.write(0x01, 0xFF); // main amplitude full
        p.write(0x00, 0x00);
        p.write(0x02, 0x10);
        p.write(0x03, 0x00);
        p.write(0x05, 0xFF); // balance full
        p.write(0x04, 0x80 | 0x1F); // on, vol 31
        // Load a square waveform.
        for i in 0..32 {
            p.write(0x06, if i < 16 { 0x1F } else { 0x00 });
        }
        p.step(PSG_CLOCK / 100);
        let s = p.drain();
        assert!(!s.is_empty());
        for v in &s {
            assert!(*v >= -1.0 && *v <= 1.0);
        }
    }

    #[test]
    fn noise_channel_enable() {
        let mut p = Psg::new();
        p.write(0x00, 0x04); // channel 4
        p.write(0x07, 0x80 | 0x05); // noise on, freq 5
        assert!(p.ch[4].noise_enabled());
    }
}
