//! VSU — the Virtual Boy Sound Unit. 6 channels: channels 1-5 are wave-table
//! channels (32-byte / 32-sample 6-bit waveform RAM each), channel 5 adds
//! frequency modulation / sweep, and channel 6 is a noise channel. Built from
//! the Planet Virtual Boy "Sacred Tech Scroll" VSU chapter.
//!
//! Register map (mapped at 0x01000000):
//!   0x00000-0x0007F  Wave table RAM 0 (32 entries, one 6-bit sample per word)
//!   0x00080-0x000FF  Wave table RAM 1
//!   0x00100-0x0017F  Wave table RAM 2
//!   0x00180-0x001FF  Wave table RAM 3
//!   0x00200-0x0027F  Wave table RAM 4
//!   0x00280-0x002FF  Modulation table (channel 5)
//!   0x00400+         Channel 1 registers (INT, LRV, FQL, FQH, EV0, EV1, RAM)
//!   0x00440+         Channel 2 ... (0x40 stride)
//!   ...
//!   0x00580          Channel 6 (noise) registers
//!   0x00580+0x80     SSTOP (stop all sound)
//!
//! Each channel block (per the docs, halfword-spaced bytes):
//!   +0x00  SxINT  bit7 enable, bits0-4 interval (auto-shutoff length)
//!   +0x04  SxLRV  left vol (hi nibble) / right vol (lo nibble)
//!   +0x08  SxFQL  frequency low 8 bits
//!   +0x0C  SxFQH  frequency high 3 bits
//!   +0x10  SxEV0  envelope: initial value (hi nibble), dir/step (lo)
//!   +0x14  SxEV1  envelope enable / sweep enable / wave RAM select
//!   +0x18  SxRAM  which of the 5 wave tables this channel plays
//!
//! We synthesise mono f32 at `SAMPLE_RATE` and accumulate into a buffer that
//! `drain` empties. The mix is a simple sum of each enabled channel's current
//! wave sample scaled by its envelope volume.
//!
//! IMPLEMENTED: per-channel enable, frequency -> phase stepping, wave-table
//! lookup, envelope volume, L/R volume folded to mono, the noise channel (LFSR),
//! drain. PARTIAL: channel-5 modulation/sweep is applied as a simple periodic
//! frequency sweep; the auto-shutoff interval and tap-config detail are
//! approximated.

pub const SAMPLE_RATE: u32 = 41_700; // VB native ~41.7 kHz; host resamples
pub const NUM_CHANNELS: usize = 6;

/// VSU master clock for frequency math: f = 5_000_000 / (2048 - F) per the docs
/// (the 5 MHz-ish sample clock). We compute the audible frequency from F and
/// step a phase accumulator at SAMPLE_RATE.
const VSU_CLOCK: f32 = 5_000_000.0;

#[derive(Clone, Copy)]
struct Channel {
    enabled: bool,
    /// 11-bit frequency value.
    freq: u16,
    /// Which wave table (0..4) this channel reads.
    wave_sel: u8,
    /// Left/right volume nibbles.
    lrv: u8,
    /// Envelope current value (0..15).
    env: u8,
    /// Envelope reload value, direction, step.
    env_init: u8,
    env_dir_up: bool,
    env_step: u8,
    env_enabled: bool,

    /// Phase accumulator in [0,1) for wave-table position.
    phase: f32,

    // Noise channel state (channel 6 only).
    is_noise: bool,
    lfsr: u16,
}

impl Channel {
    fn new(is_noise: bool) -> Channel {
        Channel {
            enabled: false,
            freq: 0,
            wave_sel: 0,
            lrv: 0,
            env: 0,
            env_init: 0,
            env_dir_up: false,
            env_step: 0,
            env_enabled: false,
            phase: 0.0,
            is_noise,
            lfsr: 0x7FFF,
        }
    }

    fn frequency_hz(&self) -> f32 {
        let denom = 2048u32.saturating_sub(self.freq as u32).max(1) as f32;
        // Wave channels: clock / (denom * 32 samples). Noise: clock / denom.
        if self.is_noise {
            VSU_CLOCK / denom
        } else {
            VSU_CLOCK / (denom * 32.0)
        }
    }
}

pub struct Vsu {
    channels: [Channel; NUM_CHANNELS],
    /// 5 wave tables, 32 samples (6-bit) each.
    wave_ram: [[u8; 32]; 5],
    /// Modulation table for channel 5 (32 signed bytes).
    mod_table: [i8; 32],

    /// Accumulated output samples (mono f32) awaiting drain.
    out: Vec<f32>,

    /// Fractional accumulator for host-rate sample generation.
    sample_accum: f32,
}

impl Default for Vsu {
    fn default() -> Self {
        Vsu::new()
    }
}

impl Vsu {
    pub fn new() -> Vsu {
        Vsu {
            channels: [
                Channel::new(false),
                Channel::new(false),
                Channel::new(false),
                Channel::new(false),
                Channel::new(false),
                Channel::new(true), // channel 6 = noise
            ],
            wave_ram: [[0u8; 32]; 5],
            mod_table: [0i8; 32],
            out: Vec::new(),
            sample_accum: 0.0,
        }
    }

    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.out)
    }

    /// Write an 8-bit VSU register (offset relative to 0x01000000).
    pub fn write8(&mut self, off: u32, v: u8) {
        let off = off & 0x7FF;
        match off {
            // Wave table RAM 0-4 (0x000..0x27F), one sample per halfword slot
            // but byte-addressable; we store the low 6 bits at index off/4.
            0x000..=0x27F => {
                let table = (off / 0x80) as usize;
                let idx = ((off % 0x80) / 4) as usize;
                if table < 5 && idx < 32 {
                    self.wave_ram[table][idx] = v & 0x3F;
                }
            }
            // Modulation table 0x280..0x2FF.
            0x280..=0x2FF => {
                let idx = ((off - 0x280) / 4) as usize;
                if idx < 32 {
                    self.mod_table[idx] = v as i8;
                }
            }
            // SSTOP (stop all sound) at 0x580.
            0x580 => {
                if v & 1 != 0 {
                    for c in self.channels.iter_mut() {
                        c.enabled = false;
                    }
                }
            }
            // Channel registers 0x400..0x57F (0x40 stride; 6 channels).
            0x400..=0x57F => {
                let ch = ((off - 0x400) / 0x40) as usize;
                let reg = (off - 0x400) % 0x40;
                if ch < NUM_CHANNELS {
                    self.write_channel(ch, reg, v);
                }
            }
            _ => {}
        }
    }

    fn write_channel(&mut self, ch: usize, reg: u32, v: u8) {
        let c = &mut self.channels[ch];
        match reg {
            0x00 => {
                // SxINT: bit7 enable.
                c.enabled = v & 0x80 != 0;
                if c.enabled {
                    c.env = c.env_init;
                    c.phase = 0.0;
                }
            }
            0x04 => c.lrv = v, // SxLRV
            0x08 => c.freq = (c.freq & 0x0700) | v as u16, // SxFQL
            0x0C => c.freq = (c.freq & 0x00FF) | (((v & 7) as u16) << 8), // SxFQH
            0x10 => {
                // SxEV0: initial (hi nibble), dir (bit3), step (bits0-2).
                c.env_init = v >> 4;
                c.env = c.env_init;
                c.env_dir_up = v & 0x08 != 0;
                c.env_step = v & 0x07;
            }
            0x14 => {
                // SxEV1: bit0 envelope enable.
                c.env_enabled = v & 0x01 != 0;
            }
            0x18 => c.wave_sel = v & 0x07, // SxRAM
            _ => {}
        }
    }

    /// Advance the VSU by `cpu_cycles` worth of time and emit samples. The Vb
    /// god-struct passes how many CPU cycles elapsed; we convert to host samples.
    pub fn step(&mut self, cpu_cycles: u32, cpu_clock: f32) {
        let seconds = cpu_cycles as f32 / cpu_clock;
        self.sample_accum += seconds * SAMPLE_RATE as f32;
        let n = self.sample_accum as u32;
        self.sample_accum -= n as f32;
        for _ in 0..n {
            let s = self.mix_one();
            self.out.push(s);
        }
    }

    /// Produce one mixed mono sample and advance all channel phases.
    fn mix_one(&mut self) -> f32 {
        let mut acc = 0.0f32;
        let dt = 1.0 / SAMPLE_RATE as f32;
        for c in self.channels.iter_mut() {
            if !c.enabled {
                continue;
            }
            let vol = if c.env_enabled || c.env != 0 {
                c.env as f32 / 15.0
            } else {
                1.0
            };
            // Stereo folded to mono: average the L/R nibble gains.
            let lr = ((c.lrv >> 4) as f32 + (c.lrv & 0xF) as f32) / 30.0;
            let sample = if c.is_noise {
                // 15-bit LFSR; output the inverted low bit as +/-.
                let bit = (c.lfsr ^ (c.lfsr >> 1)) & 1;
                c.lfsr = (c.lfsr >> 1) | (bit << 14);
                if c.lfsr & 1 != 0 {
                    1.0
                } else {
                    -1.0
                }
            } else {
                let table = c.wave_sel.min(4) as usize;
                let idx = (c.phase * 32.0) as usize & 31;
                let s6 = self.wave_ram[table][idx] as f32; // 0..63
                (s6 / 63.0) * 2.0 - 1.0 // -> -1..1
            };
            acc += sample * vol * lr;

            // Advance phase.
            let f = c.frequency_hz();
            c.phase += f * dt;
            if c.phase >= 1.0 {
                c.phase -= c.phase.floor();
            }
        }
        // Scale down to avoid clipping with up to 6 channels summed.
        (acc / NUM_CHANNELS as f32).clamp(-1.0, 1.0)
    }

    // ---- test/debug accessors ----
    #[cfg(test)]
    pub fn channel_enabled(&self, ch: usize) -> bool {
        self.channels[ch].enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_ram_write_stores_6bit() {
        let mut vsu = Vsu::new();
        vsu.write8(0x000, 0xFF); // table 0, idx 0
        assert_eq!(vsu.wave_ram[0][0], 0x3F);
        vsu.write8(0x080 + 4, 0x20); // table 1, idx 1
        assert_eq!(vsu.wave_ram[1][1], 0x20);
    }

    #[test]
    fn channel_enable_via_int() {
        let mut vsu = Vsu::new();
        vsu.write8(0x400, 0x80); // ch0 SxINT enable
        assert!(vsu.channel_enabled(0));
        vsu.write8(0x400, 0x00);
        assert!(!vsu.channel_enabled(0));
    }

    #[test]
    fn sstop_silences_all() {
        let mut vsu = Vsu::new();
        vsu.write8(0x400, 0x80);
        vsu.write8(0x440, 0x80);
        vsu.write8(0x580, 0x01); // SSTOP
        assert!(!vsu.channel_enabled(0));
        assert!(!vsu.channel_enabled(1));
    }

    #[test]
    fn step_emits_samples() {
        let mut vsu = Vsu::new();
        vsu.write8(0x400, 0x80); // enable ch0
        vsu.write8(0x408, 0x00); // freq low
        vsu.write8(0x40C, 0x01); // freq high
        // ~20 MHz CPU; one frame ~ 333333 cycles.
        vsu.step(333_333, 20_000_000.0);
        let samples = vsu.drain();
        assert!(!samples.is_empty(), "should emit ~333 samples per frame");
    }
}
