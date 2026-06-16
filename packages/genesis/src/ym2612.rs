//! YM2612 (OPN2) — the Genesis's 6-channel FM synthesizer.
//!
//! Built from the YM2612 register map (Sega/Yamaha documentation). The chip is
//! accessed through 4 ports on the 68000 bus ($A04000-$A04003): a pair of
//! address/data registers for bank 0 (channels 1-3) and bank 1 (channels 4-6).
//!
//! This is a BEST-EFFORT audio model (video is the priority): we faithfully
//! latch every register write and model each channel's total level / frequency
//! / key-on so a recognizable tone comes out, but we do NOT implement the full
//! 4-operator envelope/algorithm/LFO chain. `drain` returns mono f32 samples at
//! [`SAMPLE_RATE`]. The register state IS complete, so this is a clean base to
//! extend into a real operator core later.

pub const SAMPLE_RATE: u32 = 44100;
/// YM2612 master clock on the Genesis (~7.67 MHz / 1) divided down internally.
const YM_CLOCK: u32 = 7_670_453;

/// One FM channel's latched state (best-effort synthesis subset).
#[derive(Clone, Copy, Default)]
struct Channel {
    /// 11-bit F-number + 3-bit block (octave), assembled from the A0/A4 regs.
    fnum: u16,
    block: u8,
    /// Whether any operator is keyed on.
    key_on: bool,
    /// Total level of the carrier (op4), 0 = loudest, 127 = silent.
    total_level: u8,
    /// Running phase accumulator for the placeholder sine.
    phase: f32,
}

pub struct Ym2612 {
    /// Full 2x256 register file (bank 0 + bank 1), latched verbatim.
    regs: [[u8; 256]; 2],
    /// Latched register address per bank (set by the address ports).
    addr: [u8; 2],
    channels: [Channel; 6],

    /// Timer A/B state (status register reads return their overflow flags).
    timer_a: u16,
    timer_b: u8,
    status: u8,

    sample_accum: u32,
    buffer: Vec<f32>,
}

impl Default for Ym2612 {
    fn default() -> Self {
        Ym2612::new()
    }
}

impl Ym2612 {
    pub fn new() -> Ym2612 {
        Ym2612 {
            regs: [[0; 256]; 2],
            addr: [0; 2],
            channels: [Channel::default(); 6],
            timer_a: 0,
            timer_b: 0,
            status: 0,
            sample_accum: 0,
            buffer: Vec::with_capacity(2048),
        }
    }

    /// Read the status port ($A04000 / $A04001). Bit7 = busy (always 0 for us),
    /// bit1 = timer B overflow, bit0 = timer A overflow.
    pub fn read_status(&self) -> u8 {
        self.status
    }

    /// Write one of the 4 ports. `port` is the low 2 address bits:
    ///   0 -> bank0 address, 1 -> bank0 data,
    ///   2 -> bank1 address, 3 -> bank1 data.
    pub fn write(&mut self, port: u8, v: u8) {
        match port & 0x03 {
            0 => self.addr[0] = v,
            1 => self.write_reg(0, self.addr[0], v),
            2 => self.addr[1] = v,
            3 => self.write_reg(1, self.addr[1], v),
            _ => {}
        }
    }

    fn write_reg(&mut self, bank: usize, reg: u8, v: u8) {
        self.regs[bank][reg as usize] = v;
        match reg {
            0x24 => self.timer_a = (self.timer_a & 0x03) | ((v as u16) << 2),
            0x25 => self.timer_a = (self.timer_a & 0x3FC) | (v as u16 & 0x03),
            0x26 => self.timer_b = v,
            0x27 => {
                // Timer control / reset; clear overflow flags on reset bits.
                if v & 0x10 != 0 {
                    self.status &= !0x01;
                }
                if v & 0x20 != 0 {
                    self.status &= !0x02;
                }
            }
            0x28 => {
                // Key on/off. Low 3 bits select channel (with bank quirk), top
                // 4 bits are the operator key-on mask.
                let ch_sel = v & 0x07;
                let ch = match ch_sel {
                    0..=2 => ch_sel as usize,
                    4..=6 => (ch_sel - 4 + 3) as usize,
                    _ => return,
                };
                self.channels[ch].key_on = (v & 0xF0) != 0;
            }
            0xA0..=0xA2 => {
                // F-number low byte, channels (bank*3 + reg-0xA0).
                let ch = bank * 3 + (reg - 0xA0) as usize;
                if ch < 6 {
                    self.channels[ch].fnum =
                        (self.channels[ch].fnum & 0x700) | v as u16;
                }
            }
            0xA4..=0xA6 => {
                // F-number high bits + block.
                let ch = bank * 3 + (reg - 0xA4) as usize;
                if ch < 6 {
                    self.channels[ch].fnum =
                        (self.channels[ch].fnum & 0x0FF) | (((v as u16) & 0x07) << 8);
                    self.channels[ch].block = (v >> 3) & 0x07;
                }
            }
            // Total level for operators. Op4 (carrier in most algorithms) lives
            // at reg 0x4C..0x4E per bank — track it for amplitude.
            0x4C..=0x4E => {
                let ch = bank * 3 + (reg - 0x4C) as usize;
                if ch < 6 {
                    self.channels[ch].total_level = v & 0x7F;
                }
            }
            _ => {}
        }
    }

    /// Advance by `cpu_cycles` of the 68000 clock and emit host-rate samples.
    pub fn step(&mut self, cpu_cycles: u32) {
        // We emit one sample every YM_CLOCK/SAMPLE_RATE input cycles.
        for _ in 0..cpu_cycles {
            self.sample_accum += SAMPLE_RATE;
            if self.sample_accum >= YM_CLOCK {
                self.sample_accum -= YM_CLOCK;
                let s = self.mix();
                self.buffer.push(s);
            }
        }
    }

    /// Best-effort mix: sum a sine per keyed-on channel at its computed pitch,
    /// scaled by its carrier total level.
    fn mix(&mut self) -> f32 {
        let mut acc = 0.0f32;
        for ch in self.channels.iter_mut() {
            if !ch.key_on {
                continue;
            }
            // FM pitch: freq = fnum * 2^block * clock / (samplerate-ish const).
            // Use a simplified mapping that lands tones in audible range.
            let f = (ch.fnum as f32) * (1u32 << ch.block) as f32 * 0.0011;
            ch.phase += f / SAMPLE_RATE as f32;
            if ch.phase >= 1.0 {
                ch.phase -= 1.0;
            }
            let amp = 1.0 - (ch.total_level as f32 / 127.0);
            acc += (ch.phase * std::f32::consts::TAU).sin() * amp * 0.15;
        }
        acc.clamp(-1.0, 1.0)
    }

    pub fn drain(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_then_data_latches_register() {
        let mut ym = Ym2612::new();
        ym.write(0, 0x22); // bank0 address = 0x22 (LFO)
        ym.write(1, 0x08); // data
        assert_eq!(ym.regs[0][0x22], 0x08);
    }

    #[test]
    fn key_on_sets_channel() {
        let mut ym = Ym2612::new();
        ym.write(0, 0x28);
        ym.write(1, 0xF0); // all ops on, channel 0
        assert!(ym.channels[0].key_on);
        ym.write(0, 0x28);
        ym.write(1, 0x00); // key off
        assert!(!ym.channels[0].key_on);
    }

    #[test]
    fn frequency_assembles_from_two_regs() {
        let mut ym = Ym2612::new();
        // High byte first (block 3, fnum hi 0x01), then low byte 0x23.
        ym.write(0, 0xA4);
        ym.write(1, (3 << 3) | 0x01);
        ym.write(0, 0xA0);
        ym.write(1, 0x23);
        assert_eq!(ym.channels[0].fnum, 0x123);
        assert_eq!(ym.channels[0].block, 3);
    }

    #[test]
    fn bank1_addresses_channels_4_to_6() {
        let mut ym = Ym2612::new();
        ym.write(2, 0xA0); // bank1 address
        ym.write(3, 0x55);
        assert_eq!(ym.channels[3].fnum & 0xFF, 0x55);
    }

    #[test]
    fn step_emits_samples_in_range() {
        let mut ym = Ym2612::new();
        ym.write(0, 0x28);
        ym.write(1, 0xF0); // key on ch0
        ym.write(0, 0xA4);
        ym.write(1, (4 << 3) | 0x02);
        ym.step(YM_CLOCK / 100);
        let s = ym.drain();
        assert!(!s.is_empty());
        for v in &s {
            assert!(*v >= -1.0 && *v <= 1.0);
        }
    }

    #[test]
    fn timer_control_clears_overflow() {
        let mut ym = Ym2612::new();
        ym.status = 0x03;
        ym.write(0, 0x27);
        ym.write(1, 0x30); // reset both timer flags
        assert_eq!(ym.status & 0x03, 0);
    }
}
