//! S-DSP — the SNES sound DSP. 128 registers, 8 voices, BRR-compressed samples
//! in ARAM. This is a PARTIAL implementation: registers are stored, KON/KOFF are
//! tracked, and a coarse mixer decodes BRR and steps voices to produce some
//! audio for `drain_audio`. Pitch is honored; ADSR envelopes, gaussian
//! interpolation, echo, and noise are simplified or omitted.
//!
//! Source: fullsnes "SNES APU DSP". The goal here is "won't deadlock + makes
//! plausible sound", not accuracy.

const NUM_VOICES: usize = 8;

#[derive(Clone, Copy, Default)]
struct Voice {
    /// BRR decode cursor (ARAM byte address of the current 9-byte block).
    cursor: u16,
    /// Position within block in samples (0..16) as 16.16-ish fixed pitch acc.
    pitch_acc: u32,
    /// Last two decoded samples (for BRR filter).
    prev: [i32; 2],
    /// 16 decoded samples of the current block.
    samples: [i16; 16],
    sample_idx: usize,
    active: bool,
    block_end: bool,
    loop_flag: bool,
}

pub struct Dsp {
    pub regs: [u8; 128],
    pub addr: u8,
    voices: [Voice; NUM_VOICES],
}

impl Default for Dsp {
    fn default() -> Self {
        Dsp::new()
    }
}

impl Dsp {
    pub fn new() -> Dsp {
        Dsp {
            regs: [0; 128],
            addr: 0,
            voices: [Voice::default(); NUM_VOICES],
        }
    }

    pub fn read(&self, addr: u8) -> u8 {
        self.regs[(addr & 0x7F) as usize]
    }

    pub fn write(&mut self, addr: u8, v: u8) {
        let a = (addr & 0x7F) as usize;
        self.regs[a] = v;
        match a {
            0x4C => self.key_on(v),  // KON
            0x5C => self.key_off(v), // KOFF
            _ => {}
        }
    }

    fn key_on(&mut self, mask: u8) {
        for i in 0..NUM_VOICES {
            if mask & (1 << i) != 0 {
                let v = &mut self.voices[i];
                v.active = true;
                v.block_end = false;
                v.sample_idx = 16; // force a block load on first step
                v.pitch_acc = 0;
                v.prev = [0, 0];
            }
        }
    }
    fn key_off(&mut self, mask: u8) {
        for i in 0..NUM_VOICES {
            if mask & (1 << i) != 0 {
                self.voices[i].active = false;
            }
        }
    }

    /// Sample directory base ($5D DIR -> page in ARAM).
    fn dir_base(&self) -> u16 {
        (self.regs[0x5D] as u16) << 8
    }

    fn voice_reg(&self, v: usize, off: usize) -> u8 {
        self.regs[v * 0x10 + off]
    }

    /// Decode the next BRR block for a voice from ARAM into `v.samples`.
    fn decode_block(v: &mut Voice, aram: &[u8; 0x10000]) {
        let base = v.cursor as usize;
        let header = aram[base];
        let shift = (header >> 4) & 0x0F;
        let filter = (header >> 2) & 0x03;
        v.block_end = header & 0x01 != 0;
        v.loop_flag = header & 0x02 != 0;
        for i in 0..16 {
            let byte = aram[(base + 1 + i / 2) & 0xFFFF];
            let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0x0F };
            let mut s = (nibble as i8) as i32;
            // sign extend 4-bit.
            if s > 7 {
                s -= 16;
            }
            s = if shift <= 12 { (s << shift) >> 1 } else { (s >> 3) << 12 };
            let p0 = v.prev[0];
            let p1 = v.prev[1];
            let filtered = match filter {
                0 => s,
                1 => s + p0 + ((-p0) >> 4),
                2 => s + (p0 << 1) + ((-((p0 << 1) + p0)) >> 5) - p1 + (p1 >> 4),
                _ => s + (p0 << 1) + ((-(p0 + (p0 << 1) + (p0 << 2))) >> 6) - p1 + (((p1 << 1) + p1) >> 4),
            };
            let clamped = filtered.clamp(-32768, 32767);
            v.prev[1] = v.prev[0];
            v.prev[0] = clamped;
            v.samples[i] = clamped as i16;
        }
        v.sample_idx = 0;
    }

    /// Generate one mixed mono sample (f32, -1..1) advancing all active voices.
    pub fn generate_sample(&mut self, aram: &[u8; 0x10000]) -> f32 {
        let dir = self.dir_base();
        let main_vol_l = self.regs[0x0C] as i8 as i32;
        let _main_vol_r = self.regs[0x1C] as i8 as i32;
        let mut mix: i32 = 0;

        for vi in 0..NUM_VOICES {
            // Read per-voice config before mutably borrowing the voice.
            let srcn = self.voice_reg(vi, 0x04);
            let pitch_lo = self.voice_reg(vi, 0x02) as u32;
            let pitch_hi = self.voice_reg(vi, 0x03) as u32;
            let pitch = ((pitch_hi << 8) | pitch_lo) & 0x3FFF;
            let vol = self.voice_reg(vi, 0x00) as i8 as i32;

            let v = &mut self.voices[vi];
            if !v.active {
                continue;
            }
            // Initialize cursor from the sample directory on first activation.
            if v.sample_idx >= 16 && v.pitch_acc == 0 && v.prev == [0, 0] && !v.block_end {
                let entry = dir.wrapping_add(srcn as u16 * 4) as usize;
                let start = (aram[entry] as u16) | ((aram[(entry + 1) & 0xFFFF] as u16) << 8);
                v.cursor = start;
                Self::decode_block(v, aram);
            }

            // Advance pitch accumulator (4096 = 1.0 sample step).
            v.pitch_acc += pitch.max(1);
            while v.pitch_acc >= 0x1000 {
                v.pitch_acc -= 0x1000;
                v.sample_idx += 1;
                if v.sample_idx >= 16 {
                    if v.block_end {
                        if v.loop_flag {
                            // loop point.
                            let entry = dir.wrapping_add(srcn as u16 * 4) as usize;
                            let loop_addr = (aram[(entry + 2) & 0xFFFF] as u16)
                                | ((aram[(entry + 3) & 0xFFFF] as u16) << 8);
                            v.cursor = loop_addr;
                        } else {
                            v.active = false;
                            break;
                        }
                    } else {
                        v.cursor = v.cursor.wrapping_add(9);
                    }
                    Self::decode_block(v, aram);
                }
            }

            if v.active {
                let s = v.samples[v.sample_idx.min(15)] as i32;
                mix += (s * vol) >> 7;
            }
        }

        // Apply main volume and normalize. Clamp to avoid clipping artifacts.
        let scaled = (mix * main_vol_l.max(1)) >> 7;
        let clamped = scaled.clamp(-32768, 32767);
        clamped as f32 / 32768.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_readback() {
        let mut dsp = Dsp::new();
        dsp.write(0x00, 0x55);
        assert_eq!(dsp.read(0x00), 0x55);
    }

    #[test]
    fn key_on_activates_voice() {
        let mut dsp = Dsp::new();
        dsp.write(0x4C, 0x01); // KON voice 0
        assert!(dsp.voices[0].active);
        dsp.write(0x5C, 0x01); // KOFF voice 0
        assert!(!dsp.voices[0].active);
    }

    #[test]
    fn silent_when_no_voices() {
        let mut dsp = Dsp::new();
        let aram = [0u8; 0x10000];
        assert_eq!(dsp.generate_sample(&aram), 0.0);
    }
}
