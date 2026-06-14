//! NDS sound chip — ARM7-only IO at 0x04000400-0x040005FF (16 channels +
//! SOUNDCNT/SOUNDBIAS + capture units). `Nds` owns one `Sound` on the ARM7
//! side; the ARM9 sees the same address range as the 3D GXFIFO ports. Ported
//! from ../../ds-recomp/src/io/sound.ts.
//!
//! ## Ownership / borrow strategy (the device wave must keep this)
//!
//! Register reads/writes ([`Sound::read_byte`] / [`Sound::write_byte`]) and
//! the key-on auto-clear tick ([`Sound::step`]) touch ONLY chip state — no
//! memory. The mixer DOES read sample data from main RAM / ARM7 IWRAM; to
//! avoid a self-borrow we pass those blocks in as `&[u8]` slices (the TS held
//! a `SoundMemory { mainRam, arm7Iwram }` view). `Nds::sound_mix(...)` slices
//! `self.mem.main_ram`/`self.mem.arm7_iwram` and hands them to [`Sound::mix`].
//! The mixer never writes memory (capture units are stubbed), so immutable
//! borrows suffice.

pub const NUM_CHANNELS: usize = 16;
/// NDS sound clock — channel timers count up at 33.514 MHz.
pub const NDS_SOUND_CLOCK: u32 = 33_513_982;

/// SOUND_CNT format field (bits 30:29).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Signed 8-bit PCM.
    Pcm8,
    /// Signed 16-bit PCM.
    Pcm16,
    /// IMA-ADPCM.
    Adpcm,
    /// PSG square wave / noise (mixer-unimplemented, emits silence).
    Psg,
}

impl Format {
    /// Decode the 2-bit format field out of a 32-bit SOUND_CNT.
    pub fn from_cnt(cnt: u32) -> Format {
        match (cnt >> 29) & 0x3 {
            0 => Format::Pcm8,
            1 => Format::Pcm16,
            2 => Format::Adpcm,
            _ => Format::Psg,
        }
    }
}

// IMA-ADPCM step / index tables. Standard IMA-ADPCM constants (Intel "DVI
// ADPCM Wave Type" 1992, GBATEK "DS Sound Notes" §"ADPCM"). Identical across
// DeSmuME / melonDS, so decoded retail output matches.
const ADPCM_STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const ADPCM_INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// One sound channel's register + mixer state.
#[derive(Clone, Copy)]
pub struct Channel {
    /// 32-bit SOUND_CNT — bit 31 = key-on / busy.
    pub cnt: u32,
    /// 32-bit source address.
    pub sad: u32,
    /// 16-bit timer period.
    pub tmr: u32,
    /// 16-bit loop point (halfwords from start).
    pub pnt: u32,
    /// 32-bit length (halfwords).
    pub len: u32,

    /// Cycles remaining until the natural sample end (step() counts this down
    /// and clears key-on at 0).
    pub cycles_left: f64,
    /// Mixer cursor — fractional source-sample position.
    pub pos_frac: f64,
    /// IMA-ADPCM decoder state.
    pub adpcm_predictor: i32,
    pub adpcm_step_index: i32,
    /// Index of the last resolved sample (-1 = needs re-prime).
    pub adpcm_last_decoded_pos: i64,
}

impl Default for Channel {
    fn default() -> Self {
        Channel {
            cnt: 0,
            sad: 0,
            tmr: 0,
            pnt: 0,
            len: 0,
            cycles_left: 0.0,
            pos_frac: 0.0,
            adpcm_predictor: 0,
            adpcm_step_index: 0,
            adpcm_last_decoded_pos: -1,
        }
    }
}

impl Channel {
    /// SOUND_CNT bit 31 — key-on / busy.
    fn is_playing(&self) -> bool {
        (self.cnt >> 31) & 1 != 0
    }

    /// Repeat mode (SOUND_CNT bits 27..28): 1 = loop, 2 = one-shot. 0 = manual.
    fn repeat_mode(&self) -> u32 {
        (self.cnt >> 27) & 0x3
    }

    /// Per-channel volume (cnt bits 0..6, 0..127) and panning (bits 16..22,
    /// 0 = left, 127 = right) as a (left, right) linear-pan gain pair in [0,1].
    fn gains(&self) -> (f32, f32) {
        let vol = (self.cnt & 0x7F) as f32 / 127.0;
        let pan = ((self.cnt >> 16) & 0x7F) as f32 / 127.0;
        (vol * (1.0 - pan), vol * pan)
    }

    /// Total number of *source* samples (how far the cursor walks before
    /// looping / stopping).
    fn sample_count(&self) -> u32 {
        match Format::from_cnt(self.cnt) {
            Format::Pcm8 => self.len.wrapping_mul(2),
            Format::Pcm16 => self.len,
            // len is in halfwords; each halfword = 4 nibble samples, minus the
            // 4-byte (8-nibble) header.
            Format::Adpcm => (self.len.wrapping_mul(4)).saturating_sub(8),
            Format::Psg => 0,
        }
    }

    /// Loop-point in source-sample units (same scale as `sample_count`).
    fn loop_start(&self) -> u32 {
        let pnt = self.pnt & 0xFFFF;
        match Format::from_cnt(self.cnt) {
            Format::Pcm8 => pnt.wrapping_mul(2),
            Format::Pcm16 => pnt,
            Format::Adpcm => pnt.wrapping_mul(4),
            Format::Psg => 0,
        }
    }
}

pub struct Sound {
    pub channels: [Channel; NUM_CHANNELS],
    pub soundcnt: u32,
    pub soundbias: u32,
    pub sndcap0cnt: u32,
    pub sndcap1cnt: u32,
    pub sndcap0dad: u32,
    pub sndcap1dad: u32,
    pub sndcap0len: u32,
    pub sndcap1len: u32,
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Sound {
    pub fn new() -> Self {
        Sound {
            channels: [Channel::default(); NUM_CHANNELS],
            soundcnt: 0,
            soundbias: 0x200, // default mid-rail
            sndcap0cnt: 0,
            sndcap1cnt: 0,
            sndcap0dad: 0,
            sndcap1dad: 0,
            sndcap0len: 0,
            sndcap1len: 0,
        }
    }

    /// Byte read for an address in 0x04000400-0x040005FF (the IO dispatch
    /// passes the full 0x0400_04xx address).
    pub fn read_byte(&self, addr: u32) -> u32 {
        let a = addr & 0x0FFF_FFFF;
        if (0x0400_0400..0x0400_0500).contains(&a) {
            let ch = ((a - 0x0400_0400) >> 4) as usize;
            let off = (a - 0x0400_0400) & 0xF;
            let c = &self.channels[ch];
            return if off < 4 {
                (c.cnt >> ((off & 3) * 8)) & 0xFF
            } else if off < 8 {
                (c.sad >> (((off - 4) & 3) * 8)) & 0xFF
            } else if off < 0xA {
                (c.tmr >> (((off - 8) & 1) * 8)) & 0xFF
            } else if off < 0xC {
                (c.pnt >> (((off - 0xA) & 1) * 8)) & 0xFF
            } else {
                (c.len >> (((off - 0xC) & 3) * 8)) & 0xFF
            };
        }
        match a {
            0x0400_0500 => self.soundcnt & 0xFF,
            0x0400_0501 => (self.soundcnt >> 8) & 0xFF,
            0x0400_0504 => self.soundbias & 0xFF,
            0x0400_0505 => (self.soundbias >> 8) & 0xFF,
            0x0400_0508 => self.sndcap0cnt & 0xFF,
            0x0400_0509 => self.sndcap1cnt & 0xFF,
            _ => 0,
        }
    }

    /// Byte write — a key-on (SOUND_CNT bit 31 rising) primes `cycles_left` and
    /// resets the mixer cursor for that channel.
    pub fn write_byte(&mut self, addr: u32, value: u32) {
        let a = addr & 0x0FFF_FFFF;
        let v = value & 0xFF;
        if (0x0400_0400..0x0400_0500).contains(&a) {
            let ch = ((a - 0x0400_0400) >> 4) as usize;
            let off = (a - 0x0400_0400) & 0xF;
            let c = &mut self.channels[ch];
            let before = c.cnt;
            if off < 4 {
                let shift = (off & 3) * 8;
                c.cnt = (c.cnt & !(0xFFu32 << shift)) | (v << shift);
            } else if off < 8 {
                let shift = ((off - 4) & 3) * 8;
                c.sad = (c.sad & !(0xFFu32 << shift)) | (v << shift);
            } else if off < 0xA {
                let shift = ((off - 8) & 1) * 8;
                c.tmr = ((c.tmr & !(0xFFu32 << shift)) | (v << shift)) & 0xFFFF;
            } else if off < 0xC {
                let shift = ((off - 0xA) & 1) * 8;
                c.pnt = ((c.pnt & !(0xFFu32 << shift)) | (v << shift)) & 0xFFFF;
            } else {
                let shift = ((off - 0xC) & 3) * 8;
                c.len = (c.len & !(0xFFu32 << shift)) | (v << shift);
            }
            // Key-on edge — bit 31 of cnt going 0 → 1 (top byte was written).
            if off == 3 && (before >> 31) & 1 == 0 && (c.cnt >> 31) & 1 == 1 {
                Self::start_channel(c);
                // Restart the mixer cursor and reset the ADPCM decoder so the
                // next fetch re-primes from the 4-byte header at SAD.
                c.pos_frac = 0.0;
                c.adpcm_predictor = 0;
                c.adpcm_step_index = 0;
                c.adpcm_last_decoded_pos = -1;
            }
            return;
        }
        match a {
            0x0400_0500 => self.soundcnt = (self.soundcnt & 0xFF00) | v,
            0x0400_0501 => self.soundcnt = (self.soundcnt & 0x00FF) | (v << 8),
            0x0400_0504 => self.soundbias = (self.soundbias & 0xFF00) | v,
            0x0400_0505 => self.soundbias = (self.soundbias & 0x00FF) | (v << 8),
            0x0400_0508 => self.sndcap0cnt = v,
            0x0400_0509 => self.sndcap1cnt = v,
            // Capture dest/len writes — accept silently into the backing regs.
            0x0400_050C..=0x0400_050F => {
                let shift = (a - 0x0400_050C) * 8;
                self.sndcap0dad = (self.sndcap0dad & !(0xFFu32 << shift)) | (v << shift);
            }
            0x0400_0510..=0x0400_0513 => {
                let shift = (a - 0x0400_0510) * 8;
                self.sndcap1dad = (self.sndcap1dad & !(0xFFu32 << shift)) | (v << shift);
            }
            0x0400_0514..=0x0400_0515 => {
                let shift = (a - 0x0400_0514) * 8;
                self.sndcap0len = ((self.sndcap0len & !(0xFFu32 << shift)) | (v << shift)) & 0xFFFF;
            }
            0x0400_0516..=0x0400_0517 => {
                let shift = (a - 0x0400_0516) * 8;
                self.sndcap1len = ((self.sndcap1len & !(0xFFu32 << shift)) | (v << shift)) & 0xFFFF;
            }
            _ => {}
        }
    }

    /// When a channel is key-on'd, compute how many ARM7 cycles it will take
    /// before the natural end of the sample so we can auto-clear key-on when
    /// the sample finishes.
    fn start_channel(c: &mut Channel) {
        // tmr is a "negate count" — period in 33MHz cycles = (0x10000 - tmr).
        let period = (0x1_0000 - (c.tmr & 0xFFFF)).max(1) as f64;
        let length_samples = c.len as f64;
        // Cap at ~10 sec of cycles to avoid a runaway simulated total.
        let total = (length_samples * period).min(NDS_SOUND_CLOCK as f64 * 10.0);
        c.cycles_left = total;
    }

    /// Tick from the ARM7 step loop: decrement each key-on channel's
    /// cycles-left; on a looping channel re-prime, else clear key-on. No memory
    /// access.
    pub fn step(&mut self, cycles: u32) {
        let cycles = cycles as f64;
        for c in self.channels.iter_mut() {
            if !c.is_playing() {
                continue;
            }
            let repeat = c.repeat_mode();
            c.cycles_left -= cycles;
            if c.cycles_left <= 0.0 {
                if repeat == 1 {
                    // Looping — re-prime cycle counter; key-on stays.
                    Self::start_channel(c);
                } else {
                    // One-shot finished — clear key-on bit.
                    c.cnt &= 0x7FFF_FFFF;
                    c.cycles_left = 0.0;
                }
            }
        }
    }

    /// Mix `num_samples` interleaved stereo frames at `output_rate` into `out`
    /// (length >= num_samples * 2). Reads channel source data from the supplied
    /// memory blocks (`Nds` slices `main_ram` / `arm7_iwram` in). Read-only —
    /// capture is stubbed.
    pub fn mix(
        &mut self,
        out: &mut [f32],
        num_samples: usize,
        output_rate: u32,
        main_ram: &[u8],
        arm7_iwram: &[u8],
    ) {
        let used = num_samples * 2;
        out[..used].fill(0.0);
        // SOUNDCNT bit 15 = master enable; when 0 hardware mutes all channels.
        if self.soundcnt & 0x8000 == 0 {
            return;
        }
        // Master vol (SOUNDCNT bits 0..6, 0..127). Divide by ~4 (typical active
        // channel count) for headroom; we hard-clamp per sample afterwards.
        let master_vol = (self.soundcnt & 0x7F) as f32 / 127.0;
        let master_scale = master_vol / 4.0;
        let output_rate = output_rate.max(1) as f64;

        for c in self.channels.iter_mut() {
            if !c.is_playing() {
                continue;
            }
            let fmt = Format::from_cnt(c.cnt);
            // PSG square-wave decoder isn't implemented — skip those channels.
            if fmt == Format::Psg {
                continue;
            }
            let period = (0x1_0000 - (c.tmr & 0xFFFF)).max(1) as f64;
            let chan_rate = NDS_SOUND_CLOCK as f64 / period;
            // Source samples to advance per output sample.
            let step = chan_rate / output_rate;
            let total_samples = c.sample_count();
            if total_samples == 0 {
                continue;
            }
            let total_samples = total_samples as f64;
            let repeat = c.repeat_mode();
            let loop_start = c.loop_start() as f64;
            let (g_l, g_r) = c.gains();

            let mut pos = c.pos_frac;
            let mut decoder = AdpcmDecoder::from_channel(c);
            for n in 0..num_samples {
                if pos >= total_samples {
                    if repeat == 1 {
                        // Loop back to the loop-point, preserving fractional
                        // phase so the pitch doesn't glitch at the wrap.
                        let tail = pos - total_samples;
                        let span = total_samples - loop_start;
                        pos = if span > 0.0 {
                            loop_start + tail.rem_euclid(span)
                        } else {
                            loop_start
                        };
                    } else {
                        // One-shot ended mid-buffer — clear key-on and stop.
                        c.cnt &= 0x7FFF_FFFF;
                        break;
                    }
                }
                let s_idx = pos.floor() as i64;
                let s = fetch_sample(c, &mut decoder, fmt, s_idx, main_ram, arm7_iwram);
                out[n * 2] += s * g_l * master_scale;
                out[n * 2 + 1] += s * g_r * master_scale;
                pos += step;
            }
            c.pos_frac = pos;
            decoder.write_back(c);
        }

        // Final hard clamp to [-1, 1].
        for v in out[..used].iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
    }
}

/// Resolve a sound-sample address (main RAM mirrors at 0x02xxxxxx, ARM7 IWRAM
/// at 0x037Fxxxx) to a (bytes, offset) pair, or `None` if the address doesn't
/// land in a streamable region. Out-of-region reads come back as silence.
fn resolve_sample_byte<'a>(
    addr: u32,
    main_ram: &'a [u8],
    arm7_iwram: &'a [u8],
) -> Option<(&'a [u8], usize)> {
    if (0x0200_0000..0x0300_0000).contains(&addr) && !main_ram.is_empty() {
        // Main RAM (4 MB) + mirror at 0x027xxxxx — mask to the backing length.
        let off = (addr - 0x0200_0000) as usize & (main_ram.len() - 1);
        return Some((main_ram, off));
    }
    if (0x037F_8000..0x0381_0000).contains(&addr) {
        let off = (addr - 0x037F_8000) as usize;
        if off < arm7_iwram.len() {
            return Some((arm7_iwram, off));
        }
    }
    None
}

/// Fetch one decoded source sample for `c` at integer source-position `pos`,
/// in roughly [-1, 1]. PSG returns silence.
fn fetch_sample(
    c: &Channel,
    decoder: &mut AdpcmDecoder,
    fmt: Format,
    pos: i64,
    main_ram: &[u8],
    arm7_iwram: &[u8],
) -> f32 {
    match fmt {
        Format::Pcm8 => {
            // 1 byte per source sample.
            let addr = c.sad.wrapping_add(pos as u32);
            match resolve_sample_byte(addr, main_ram, arm7_iwram) {
                Some((bytes, off)) => (bytes[off] as i8 as f32) / 128.0,
                None => 0.0,
            }
        }
        Format::Pcm16 => {
            // 2 bytes per source sample.
            let addr = c.sad.wrapping_add((pos as u32).wrapping_mul(2));
            match resolve_sample_byte(addr, main_ram, arm7_iwram) {
                Some((bytes, off)) if off + 1 < bytes.len() => {
                    let u = (bytes[off] as u16) | ((bytes[off + 1] as u16) << 8);
                    (u as i16 as f32) / 32768.0
                }
                _ => 0.0,
            }
        }
        Format::Adpcm => decoder.decode(c, pos, main_ram, arm7_iwram),
        Format::Psg => 0.0,
    }
}

/// Sequential IMA-ADPCM decoder for one mix() pass over a channel. Holds the
/// running predictor / step-index locally so the inner loop avoids touching the
/// channel struct; `write_back` flushes the final state.
struct AdpcmDecoder {
    predictor: i32,
    step_index: i32,
    last_decoded_pos: i64,
}

impl AdpcmDecoder {
    fn from_channel(c: &Channel) -> Self {
        AdpcmDecoder {
            predictor: c.adpcm_predictor,
            step_index: c.adpcm_step_index,
            last_decoded_pos: c.adpcm_last_decoded_pos,
        }
    }

    fn write_back(&self, c: &mut Channel) {
        c.adpcm_predictor = self.predictor;
        c.adpcm_step_index = self.step_index;
        c.adpcm_last_decoded_pos = self.last_decoded_pos;
    }

    /// Decode the ADPCM sample at `sample_idx` (0-based, after the 4-byte
    /// header). Sequential: walks forward from `last_decoded_pos+1`. Re-primes
    /// from the header when the cursor moves backwards (loop wrap / key-on).
    fn decode(
        &mut self,
        c: &Channel,
        sample_idx: i64,
        main_ram: &[u8],
        arm7_iwram: &[u8],
    ) -> f32 {
        let (bytes, base) = match resolve_sample_byte(c.sad, main_ram, arm7_iwram) {
            Some(r) => r,
            None => return 0.0,
        };
        // Re-prime when the cursor moves BACKWARDS or hasn't been primed.
        // The `<` (not `<=`) matters: when sample_idx == last_decoded_pos the
        // predictor is already correct and the advance loop runs zero times —
        // re-priming there would make the decoder O(N^2) per buffer.
        if sample_idx < self.last_decoded_pos || self.last_decoded_pos < 0 {
            if base + 2 < bytes.len() {
                let u = (bytes[base] as u16) | ((bytes[base + 1] as u16) << 8);
                self.predictor = u as i16 as i32;
                // Step index is the high byte, & 0x7F (top bit is loop info).
                self.step_index = (bytes[base + 2] as i32 & 0x7F).min(88);
            } else {
                self.predictor = 0;
                self.step_index = 0;
            }
            self.last_decoded_pos = -1;
        }
        // Advance from one past the last-decoded sample through sample_idx.
        // First nibble of each byte is the LOW nibble; source byte index is
        // `sad + 4 + floor(n / 2)`.
        let payload = base + 4;
        let mut predictor = self.predictor;
        let mut step_index = self.step_index;
        let mut n = self.last_decoded_pos + 1;
        while n <= sample_idx {
            let byte_off = payload + (n >> 1) as usize;
            let b = if byte_off < bytes.len() {
                bytes[byte_off] as u32
            } else {
                0
            };
            let nibble = if n & 1 == 0 { b & 0xF } else { (b >> 4) & 0xF };
            let step = ADPCM_STEP_TABLE[step_index as usize];
            let mut diff = step >> 3;
            if nibble & 1 != 0 {
                diff += step >> 2;
            }
            if nibble & 2 != 0 {
                diff += step >> 1;
            }
            if nibble & 4 != 0 {
                diff += step;
            }
            if nibble & 8 != 0 {
                diff = -diff;
            }
            predictor = (predictor + diff).clamp(-32768, 32767);
            step_index = (step_index + ADPCM_INDEX_TABLE[nibble as usize]).clamp(0, 88);
            n += 1;
        }
        self.predictor = predictor;
        self.step_index = step_index;
        self.last_decoded_pos = sample_idx;
        predictor as f32 / 32768.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Address helpers for the channel register block.
    fn ch_base(ch: u32) -> u32 {
        0x0400_0400 + ch * 0x10
    }

    // Write a 32-bit value byte-by-byte to a channel field via write_byte.
    fn write32(s: &mut Sound, addr: u32, v: u32) {
        for i in 0..4 {
            s.write_byte(addr + i, (v >> (i * 8)) & 0xFF);
        }
    }

    #[test]
    fn channel_register_byte_roundtrip() {
        let mut s = Sound::new();
        let base = ch_base(5);
        // SAD at +4 is a plain 32-bit field — write it without keying on.
        write32(&mut s, base + 4, 0x0201_2345);
        for i in 0..4 {
            assert_eq!(s.read_byte(base + 4 + i), (0x0201_2345u32 >> (i * 8)) & 0xFF);
        }
        // TMR (16-bit) and PNT (16-bit) and LEN (32-bit).
        s.write_byte(base + 8, 0xCD);
        s.write_byte(base + 9, 0xAB);
        assert_eq!(s.channels[5].tmr, 0xABCD);
        s.write_byte(base + 0xA, 0x34);
        s.write_byte(base + 0xB, 0x12);
        assert_eq!(s.channels[5].pnt, 0x1234);
        write32(&mut s, base + 0xC, 0xDEAD_BEEF);
        assert_eq!(s.channels[5].len, 0xDEAD_BEEF);
    }

    #[test]
    fn key_on_edge_primes_and_clears_decoder() {
        let mut s = Sound::new();
        let base = ch_base(0);
        // Set up a PCM8 one-shot: len = 4 halfwords, tmr giving period 0x100.
        s.write_byte(base + 8, 0x00); // tmr lo
        s.write_byte(base + 9, 0xFF); // tmr = 0xFF00 → period 0x100
        write32(&mut s, base + 0xC, 4); // len = 4
                                        // Dirty the decoder to prove key-on resets it.
        s.channels[0].adpcm_last_decoded_pos = 99;
        s.channels[0].pos_frac = 7.0;
        // Key on: write cnt top byte with bit 31 set. cnt = 0x8000_0000.
        write32(&mut s, base, 0x8000_0000);
        let c = &s.channels[0];
        assert!(c.is_playing());
        assert_eq!(c.adpcm_last_decoded_pos, -1);
        assert_eq!(c.pos_frac, 0.0);
        // cycles_left = len * period = 4 * 0x100 = 0x400.
        assert_eq!(c.cycles_left, (4 * 0x100) as f64);
    }

    #[test]
    fn step_clears_keyon_for_oneshot() {
        let mut s = Sound::new();
        let base = ch_base(0);
        s.write_byte(base + 8, 0x00);
        s.write_byte(base + 9, 0xFF); // period 0x100
        write32(&mut s, base + 0xC, 1); // len 1 → cycles_left 0x100
                                        // one-shot repeat mode: bits 27..28 = 2.
        write32(&mut s, base, 0x8000_0000 | (2 << 27));
        assert!(s.channels[0].is_playing());
        s.step(0x100);
        assert!(!s.channels[0].is_playing(), "one-shot should clear key-on");
    }

    #[test]
    fn step_loops_keeps_keyon() {
        let mut s = Sound::new();
        let base = ch_base(0);
        s.write_byte(base + 8, 0x00);
        s.write_byte(base + 9, 0xFF);
        write32(&mut s, base + 0xC, 1);
        // repeat mode 1 = loop.
        write32(&mut s, base, 0x8000_0000 | (1 << 27));
        s.step(0x200); // overshoot
        assert!(s.channels[0].is_playing(), "loop keeps key-on");
        assert!(s.channels[0].cycles_left > 0.0, "loop re-primes cycle counter");
    }

    #[test]
    fn soundcnt_and_bias_byte_access() {
        let mut s = Sound::new();
        s.write_byte(0x0400_0500, 0x7F);
        s.write_byte(0x0400_0501, 0x80); // master enable bit 15
        assert_eq!(s.soundcnt, 0x807F);
        assert_eq!(s.read_byte(0x0400_0500), 0x7F);
        assert_eq!(s.read_byte(0x0400_0501), 0x80);
        // soundbias default mid-rail.
        assert_eq!(s.read_byte(0x0400_0504), 0x00);
        assert_eq!(s.read_byte(0x0400_0505), 0x02);
        s.write_byte(0x0400_0504, 0x55);
        assert_eq!(s.soundbias, 0x0255);
    }

    #[test]
    fn capture_regs_round_trip() {
        let mut s = Sound::new();
        s.write_byte(0x0400_0508, 0xAB);
        s.write_byte(0x0400_0509, 0xCD);
        assert_eq!(s.sndcap0cnt, 0xAB);
        assert_eq!(s.sndcap1cnt, 0xCD);
        assert_eq!(s.read_byte(0x0400_0508), 0xAB);
        write32(&mut s, 0x0400_050C, 0x0203_0000);
        assert_eq!(s.sndcap0dad, 0x0203_0000);
        s.write_byte(0x0400_0514, 0x10);
        s.write_byte(0x0400_0515, 0x20);
        assert_eq!(s.sndcap0len, 0x2010);
    }

    #[test]
    fn mix_master_disable_is_silent() {
        let mut s = Sound::new();
        let main = vec![0x40u8; 0x1000];
        let iwram = vec![0u8; 0x1_0000];
        let mut out = [0.0f32; 16];
        // soundcnt bit 15 (master) is clear → silence even if a channel is on.
        let base = ch_base(0);
        write32(&mut s, base, 0x8000_0000); // key on PCM8
        write32(&mut s, base + 4, 0x0200_0000); // sad → main ram
        write32(&mut s, base + 0xC, 0x100); // length
        s.write_byte(base, 0x7F); // vol max (low byte of cnt) — re-write keeps key-on
        s.mix(&mut out, 8, 32768, &main, &iwram);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn mix_pcm8_full_scale() {
        let mut s = Sound::new();
        // 0x7F = +127 signed → +127/128 ~= 0.992 at the source.
        let main = vec![0x7Fu8; 0x1000];
        let iwram = vec![0u8; 0x1_0000];
        // Master enable + full master volume.
        s.write_byte(0x0400_0500, 0x7F);
        s.write_byte(0x0400_0501, 0x80);
        let base = ch_base(0);
        write32(&mut s, base + 4, 0x0200_0000); // sad → main ram base
        write32(&mut s, base + 0xC, 0x100); // len 0x100 halfwords
                                            // tmr giving a 1:1-ish rate; pick period so step ~ 1.
                                            // chan_rate = CLOCK/period; output_rate = CLOCK/period too if we pass it.
                                            // Simpler: just confirm non-zero, hard-left/right gains.
                                            // cnt: vol 127 (bits0..6), pan 0 (left), PCM8 fmt(0), key-on.
        let cnt = 0x8000_0000u32 | 0x7F; // pan=0 → left only, vol 127
        write32(&mut s, base, cnt);
        // tmr: set so period reasonable.
        s.write_byte(base + 8, 0x00);
        s.write_byte(base + 9, 0xFF); // period 0x100
        let mut out = [0.0f32; 16];
        let rate = NDS_SOUND_CLOCK / 0x100; // chan rate ~= output rate → step ~1
        s.mix(&mut out, 8, rate, &main, &iwram);
        // Left channel (even indices) should be non-zero positive; right zero.
        assert!(out[0] > 0.0, "left should carry the sample, got {}", out[0]);
        assert_eq!(out[1], 0.0, "pan=0 → no right output");
    }

    #[test]
    fn mix_pcm16_decodes_signed() {
        let mut s = Sound::new();
        // 0x4000 little-endian = +16384 → +0.5 source.
        let mut main = vec![0u8; 0x1000];
        for i in (0..0x1000).step_by(2) {
            main[i] = 0x00;
            main[i + 1] = 0x40;
        }
        let iwram = vec![0u8; 0x1_0000];
        s.write_byte(0x0400_0500, 0x7F);
        s.write_byte(0x0400_0501, 0x80);
        let base = ch_base(0);
        write32(&mut s, base + 4, 0x0200_0000);
        write32(&mut s, base + 0xC, 0x100);
        // fmt PCM16 = 1 → cnt bits 30:29 = 01 → 0x2000_0000; pan center 64.
        let cnt = 0x8000_0000u32 | (1 << 29) | (64 << 16) | 0x7F;
        write32(&mut s, base, cnt);
        s.write_byte(base + 8, 0x00);
        s.write_byte(base + 9, 0xFF);
        let mut out = [0.0f32; 8];
        let rate = NDS_SOUND_CLOCK / 0x100;
        s.mix(&mut out, 4, rate, &main, &iwram);
        // Both sides carry roughly equal positive energy (near-center pan
        // 64/127 → left 63/127, right 64/127 — close but not identical).
        assert!(out[0] > 0.0 && out[1] > 0.0);
        assert!((out[0] - out[1]).abs() < 0.05 * out[1]);
    }

    #[test]
    fn adpcm_decoder_is_sequential_and_reprimes_on_wrap() {
        // Build a small ADPCM stream: header (predictor 0, step index 0) then
        // nibbles. Decode forward, then re-decode pos 0 to confirm re-prime.
        let mut main = vec![0u8; 0x100];
        // header: predictor lo/hi = 0, step index = 0, byte3 = 0.
        main[0] = 0;
        main[1] = 0;
        main[2] = 0;
        main[3] = 0;
        // payload nibbles: 0x04 then 0x04 ... a positive-ish ramp.
        for b in main.iter_mut().skip(4).take(8) {
            *b = 0x44;
        }
        let iwram = vec![0u8; 0x10];
        let mut c = Channel {
            sad: 0x0200_0000,
            ..Default::default()
        };
        let mut dec = AdpcmDecoder::from_channel(&c);
        let s0 = dec.decode(&c, 0, &main, &iwram);
        let s1 = dec.decode(&c, 1, &main, &iwram);
        let s2 = dec.decode(&c, 2, &main, &iwram);
        // Predictor accumulates → magnitudes grow with positive nibbles.
        assert!(s1.abs() >= s0.abs());
        assert!(s2.abs() >= s1.abs());
        assert_eq!(dec.last_decoded_pos, 2);
        // Going backwards to 0 must re-prime (predictor returns near header).
        let s0b = dec.decode(&c, 0, &main, &iwram);
        assert!((s0b - s0).abs() < 1e-6, "re-prime should reproduce sample 0");
        dec.write_back(&mut c);
        assert_eq!(c.adpcm_last_decoded_pos, 0);
    }

    #[test]
    fn format_decode() {
        assert_eq!(Format::from_cnt(0), Format::Pcm8);
        assert_eq!(Format::from_cnt(1 << 29), Format::Pcm16);
        assert_eq!(Format::from_cnt(2 << 29), Format::Adpcm);
        assert_eq!(Format::from_cnt(3 << 29), Format::Psg);
    }

    #[test]
    fn psg_channel_emits_silence_from_mixer() {
        let mut s = Sound::new();
        let main = vec![0x7Fu8; 0x1000];
        let iwram = vec![0u8; 0x10];
        s.write_byte(0x0400_0500, 0x7F);
        s.write_byte(0x0400_0501, 0x80);
        let base = ch_base(0);
        write32(&mut s, base + 4, 0x0200_0000);
        write32(&mut s, base + 0xC, 0x100);
        let cnt = 0x8000_0000u32 | (3 << 29) | 0x7F; // PSG fmt
        write32(&mut s, base, cnt);
        s.write_byte(base + 8, 0x00);
        s.write_byte(base + 9, 0xFF);
        let mut out = [0.0f32; 16];
        s.mix(&mut out, 8, 32768, &main, &iwram);
        assert!(out.iter().all(|&v| v == 0.0), "PSG mixer path is silent");
    }
}
