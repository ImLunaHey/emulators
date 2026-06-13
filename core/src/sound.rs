//! GBA sound. Ported 1:1 from src/io/sound.ts.
//!
//! GBA sound: the two FIFO-driven DirectSound PCM channels (A/B) plus
//! the four legacy GB PSG channels (tone+sweep, tone, wave, noise).
//
// How DirectSound works on the GBA:
//   1. Game sets up a Timer (0 or 1) with a reload value that makes
//      it overflow at the desired output sample rate, e.g. 32768 Hz.
//   2. Game sets up DMA1 (for FIFO A) or DMA2 (for FIFO B) with
//      timing = special, src = wave buffer in EWRAM, dst = FIFO addr,
//      count = 4 words.
//   3. Timer overflow drains 1 sample (signed 8-bit) from the FIFO.
//   4. When the FIFO has ≤16 of its 32 bytes remaining, DMA fires and
//      pushes 4 words = 16 bytes back in.
//   5. SOUNDCNT_H picks which timer drives each channel, the per-
//      channel volume (50% / 100%), and which sides (L/R) get the
//      output.
//   6. SOUNDCNT_X bit 7 is the master enable.
//
// Output strategy: we emit interleaved stereo float pairs [L, R] at a
// fixed 32768 Hz (one pair every 512 CPU cycles), driven by step()
// from the emulator's main loop. DirectSound samples are zero-order
// held between timer overflows (matching the hardware DAC, which also
// holds the last sample) and resampled to the output rate implicitly.
// The PSG generators are clocked in CPU cycles, with the GB frame
// sequencer (512 Hz) derived from the same cycle counter:
//   length counters @ 256 Hz, sweep @ 128 Hz, envelopes @ 64 Hz.

// The `dma` collaborator the TS constructor received is, per the porting
// contract, passed as a `&mut crate::dma::Dma` PARAMETER to the methods
// that need it (only `on_timer_overflow`), never stored as a field.

const FIFO_SIZE: usize = 32;
const SAMPLE_CYCLES: i32 = 512; // 16777216 / 512 = 32768 Hz output rate
const SEQ_CYCLES: i32 = 32768; // 512 Hz frame sequencer

// The four square-wave duty patterns (12.5%, 25%, 50%, 75%), 8 phase
// steps each. Phase advances at 8× the tone frequency.
const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

// SOUNDCNT_H bits 0-1: PSG mix ratio. 3 is "prohibited"; treat as 100%.
const PSG_RATIO: [f32; 4] = [0.25, 0.5, 1.0, 1.0];

// Square channel (1 and 2). Channel 2 simply never gets sweep writes.
pub struct SquareChannel {
    pub enabled: bool,
    pub dac_on: bool, // envelope initial volume 0 + decrease = DAC off
    pub duty: u32,
    pub duty_pos: u32,
    pub freq: i32,       // 11-bit; tone = 131072/(2048-freq) Hz
    pub freq_timer: i32, // CPU cycles until next duty step
    pub length_counter: i32, // 0..64, counts down at 256 Hz when enabled
    pub length_enable: bool,
    pub env_init: i32,
    pub env_dir: u32,
    pub env_period: i32,
    pub env_timer: i32,
    pub vol: i32, // current envelope volume 0..15
    // Sweep (channel 1 only).
    pub sweep_shift: u32,
    pub sweep_dir: u32,
    pub sweep_period: i32,
    pub sweep_timer: i32,
    pub sweep_enabled: bool,
    pub shadow_freq: i32,
}

impl Default for SquareChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl SquareChannel {
    pub fn new() -> Self {
        SquareChannel {
            enabled: false,
            dac_on: false,
            duty: 0,
            duty_pos: 0,
            freq: 0,
            freq_timer: 0,
            length_counter: 0,
            length_enable: false,
            env_init: 0,
            env_dir: 0,
            env_period: 0,
            env_timer: 0,
            vol: 0,
            sweep_shift: 0,
            sweep_dir: 0,
            sweep_period: 0,
            sweep_timer: 0,
            sweep_enabled: false,
            shadow_freq: 0,
        }
    }

    pub fn reset(&mut self) {
        self.enabled = false;
        self.dac_on = false;
        self.length_enable = false;
        self.sweep_enabled = false;
        self.duty = 0;
        self.duty_pos = 0;
        self.freq = 0;
        self.freq_timer = 0;
        self.length_counter = 0;
        self.env_init = 0;
        self.env_dir = 0;
        self.env_period = 0;
        self.env_timer = 0;
        self.vol = 0;
        self.sweep_shift = 0;
        self.sweep_dir = 0;
        self.sweep_period = 0;
        self.sweep_timer = 0;
        self.shadow_freq = 0;
    }

    // NR10 / SOUND1CNT_L
    pub fn write_sweep(&mut self, v: u32) {
        self.sweep_shift = v & 7;
        self.sweep_dir = (v >> 3) & 1;
        self.sweep_period = ((v >> 4) & 7) as i32;
    }
    // NRx1+NRx2 / SOUNDxCNT_H: length, duty, envelope
    pub fn write_duty_len_env(&mut self, v: u32) {
        self.length_counter = (64 - (v & 63)) as i32;
        self.duty = (v >> 6) & 3;
        self.env_period = ((v >> 8) & 7) as i32;
        self.env_dir = (v >> 11) & 1;
        self.env_init = ((v >> 12) & 15) as i32;
        self.dac_on = (v & 0xF800) != 0;
        if !self.dac_on {
            self.enabled = false;
        }
    }
    // NRx3+NRx4 / SOUNDxCNT_X: frequency, length enable, trigger
    pub fn write_freq_ctl(&mut self, v: u32) {
        self.freq = (v & 0x7FF) as i32;
        self.length_enable = (v & 0x4000) != 0;
        if v & 0x8000 != 0 {
            self.trigger();
        }
    }

    fn period(&self) -> i32 {
        (2048 - self.freq) * 16
    }

    pub fn trigger(&mut self) {
        self.enabled = self.dac_on;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.freq_timer = self.period();
        self.duty_pos = 0;
        self.vol = self.env_init;
        self.env_timer = if self.env_period != 0 { self.env_period } else { 8 };
        self.shadow_freq = self.freq;
        self.sweep_timer = if self.sweep_period != 0 { self.sweep_period } else { 8 };
        self.sweep_enabled = self.sweep_period > 0 || self.sweep_shift > 0;
        // Immediate overflow check when shift > 0 (can kill the channel
        // right at the trigger — GB/GBA hardware behavior).
        if self.sweep_shift > 0 {
            self.sweep_calc();
        }
    }

    // Compute the next sweep frequency; flags overflow by disabling.
    fn sweep_calc(&mut self) -> i32 {
        let d = self.shadow_freq >> self.sweep_shift;
        let nf = if self.sweep_dir != 0 {
            self.shadow_freq - d
        } else {
            self.shadow_freq + d
        };
        if nf > 2047 {
            self.enabled = false;
        }
        nf
    }

    pub fn clock_sweep(&mut self) {
        self.sweep_timer -= 1;
        if self.sweep_timer > 0 {
            return;
        }
        self.sweep_timer = if self.sweep_period != 0 { self.sweep_period } else { 8 };
        if !self.sweep_enabled || self.sweep_period == 0 {
            return;
        }
        let nf = self.sweep_calc();
        if nf <= 2047 && self.sweep_shift > 0 {
            self.shadow_freq = nf;
            self.freq = nf;
            self.sweep_calc(); // second overflow check with the new shadow
        }
    }
    pub fn clock_length(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
    pub fn clock_envelope(&mut self) {
        if self.env_period == 0 {
            return;
        }
        self.env_timer -= 1;
        if self.env_timer > 0 {
            return;
        }
        self.env_timer = self.env_period;
        if self.env_dir != 0 {
            if self.vol < 15 {
                self.vol += 1;
            }
        } else if self.vol > 0 {
            self.vol -= 1;
        }
    }

    pub fn advance(&mut self, cycles: i32) {
        if !self.enabled {
            return;
        }
        self.freq_timer -= cycles;
        if self.freq_timer <= 0 {
            let p = self.period();
            let n = (-self.freq_timer / p) + 1; // Math.floor(-freqTimer / p) + 1
            self.freq_timer += n * p;
            self.duty_pos = (self.duty_pos + n as u32) & 7;
        }
    }

    // Digital output in -15..15. We center the square around 0 (±vol)
    // instead of the hardware's unipolar DAC + analog highpass, so a
    // silenced/expired channel contributes exactly 0 with no DC step.
    pub fn output(&self) -> f32 {
        if !self.enabled || !self.dac_on {
            return 0.0;
        }
        if DUTY[self.duty as usize][self.duty_pos as usize] != 0 {
            self.vol as f32
        } else {
            -self.vol as f32
        }
    }
}

// Wave channel (3). GBA-specific: two 32-sample (16-byte) wave RAM
// banks; bit 5 of SOUND3CNT_L selects 32- or 64-sample dimension and
// bit 6 selects the playing bank. CPU reads/writes at 0x4000090..9F
// always access the bank NOT selected for playback (GBATEK).
pub struct WaveChannel {
    pub enabled: bool,
    pub playback: bool, // SOUND3CNT_L bit 7 (acts as the DAC enable)
    pub dimension: u32, // 0 = one 32-sample bank, 1 = 64 samples across both
    pub bank: u32,
    pub length_counter: i32, // 0..256
    pub length_enable: bool,
    pub vol_code: u32, // 0=0%, 1=100%, 2=50%, 3=25%
    pub force75: bool, // SOUND3CNT_H bit 15: force 75%
    pub freq: i32,
    pub freq_timer: i32,
    pub pos: u32,    // sample position 0..31 (or 0..63)
    pub sample: i32, // latched 4-bit sample
    pub ram: [u8; 32], // bytes 0-15 = bank 0, bytes 16-31 = bank 1
}

impl Default for WaveChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveChannel {
    pub fn new() -> Self {
        WaveChannel {
            enabled: false,
            playback: false,
            dimension: 0,
            bank: 0,
            length_counter: 0,
            length_enable: false,
            vol_code: 0,
            force75: false,
            freq: 0,
            freq_timer: 0,
            pos: 0,
            sample: 0,
            ram: [0; 32],
        }
    }

    pub fn reset(&mut self) {
        self.enabled = false;
        self.playback = false;
        self.length_enable = false;
        self.force75 = false;
        self.dimension = 0;
        self.bank = 0;
        self.vol_code = 0;
        self.length_counter = 0;
        self.freq = 0;
        self.freq_timer = 0;
        self.pos = 0;
        self.sample = 0;
        // Wave RAM contents survive a PSG master-disable on hardware.
    }

    pub fn write_ctl(&mut self, v: u32) {
        // SOUND3CNT_L
        self.dimension = (v >> 5) & 1;
        self.bank = (v >> 6) & 1;
        self.playback = (v & 0x80) != 0;
        if !self.playback {
            self.enabled = false;
        }
    }
    pub fn write_len_vol(&mut self, v: u32) {
        // SOUND3CNT_H
        self.length_counter = (256 - (v & 0xFF)) as i32;
        self.vol_code = (v >> 13) & 3;
        self.force75 = (v & 0x8000) != 0;
    }
    pub fn write_freq_ctl(&mut self, v: u32) {
        // SOUND3CNT_X
        self.freq = (v & 0x7FF) as i32;
        self.length_enable = (v & 0x4000) != 0;
        if v & 0x8000 != 0 {
            self.trigger();
        }
    }

    // CPU-visible wave RAM: the non-playing bank.
    pub fn ram_index(&self, byte_off: u32) -> usize {
        ((self.bank ^ 1) * 16 + (byte_off & 15)) as usize
    }
    pub fn read_ram8(&self, off: u32) -> u32 {
        self.ram[self.ram_index(off)] as u32
    }
    pub fn write_ram8(&mut self, off: u32, v: u32) {
        let i = self.ram_index(off);
        self.ram[i] = (v & 0xFF) as u8;
    }

    fn period(&self) -> i32 {
        (2048 - self.freq) * 8
    }

    pub fn trigger(&mut self) {
        self.enabled = self.playback;
        if self.length_counter == 0 {
            self.length_counter = 256;
        }
        self.pos = 0;
        self.freq_timer = self.period();
        self.latch_sample();
    }

    fn latch_sample(&mut self) {
        // In 64-sample mode playback starts at the selected bank and runs
        // through both; in 32-sample mode it loops the selected bank.
        let idx = if self.dimension != 0 {
            (self.bank * 32 + self.pos) & 63
        } else {
            self.bank * 32 + (self.pos & 31)
        };
        let byte = self.ram[(idx >> 1) as usize] as i32;
        self.sample = if idx & 1 != 0 { byte & 0xF } else { byte >> 4 }; // high nibble first
    }

    pub fn clock_length(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    pub fn advance(&mut self, cycles: i32) {
        if !self.enabled {
            return;
        }
        self.freq_timer -= cycles;
        if self.freq_timer <= 0 {
            let p = self.period();
            let n = (-self.freq_timer / p) + 1;
            self.freq_timer += n * p;
            let modulus = if self.dimension != 0 { 64 } else { 32 };
            self.pos = (self.pos + n as u32) % modulus;
            self.latch_sample();
        }
    }

    // Centered digital output in -15..15 (float — volume scaling can
    // produce fractions). Center-then-scale so volume 0 emits 0, not DC.
    pub fn output(&self) -> f32 {
        if !self.enabled || !self.playback {
            return 0.0;
        }
        let centered = (self.sample * 2 - 15) as f32;
        if self.force75 {
            return centered * 0.75;
        }
        match self.vol_code {
            0 => 0.0,
            1 => centered,
            2 => centered * 0.5,
            _ => centered * 0.25,
        }
    }
}

// Noise channel (4). 15-bit LFSR (optionally folded to 7-bit width).
pub struct NoiseChannel {
    pub enabled: bool,
    pub dac_on: bool,
    pub length_counter: i32,
    pub length_enable: bool,
    pub env_init: i32,
    pub env_dir: u32,
    pub env_period: i32,
    pub env_timer: i32,
    pub vol: i32,
    pub divisor: i32,
    pub width7: bool,
    pub shift: u32,
    pub lfsr: u32,
    pub freq_timer: i32,
}

impl Default for NoiseChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseChannel {
    pub fn new() -> Self {
        NoiseChannel {
            enabled: false,
            dac_on: false,
            length_counter: 0,
            length_enable: false,
            env_init: 0,
            env_dir: 0,
            env_period: 0,
            env_timer: 0,
            vol: 0,
            divisor: 0,
            width7: false,
            shift: 0,
            lfsr: 0x7FFF,
            freq_timer: 0,
        }
    }

    pub fn reset(&mut self) {
        self.enabled = false;
        self.dac_on = false;
        self.length_enable = false;
        self.width7 = false;
        self.length_counter = 0;
        self.env_init = 0;
        self.env_dir = 0;
        self.env_period = 0;
        self.env_timer = 0;
        self.vol = 0;
        self.divisor = 0;
        self.shift = 0;
        self.freq_timer = 0;
        self.lfsr = 0x7FFF;
    }

    pub fn write_len_env(&mut self, v: u32) {
        // SOUND4CNT_L
        self.length_counter = (64 - (v & 63)) as i32;
        self.env_period = ((v >> 8) & 7) as i32;
        self.env_dir = (v >> 11) & 1;
        self.env_init = ((v >> 12) & 15) as i32;
        self.dac_on = (v & 0xF800) != 0;
        if !self.dac_on {
            self.enabled = false;
        }
    }
    pub fn write_ctl(&mut self, v: u32) {
        // SOUND4CNT_H
        self.divisor = (v & 7) as i32;
        self.width7 = (v & 0x08) != 0;
        self.shift = (v >> 4) & 15;
        self.length_enable = (v & 0x4000) != 0;
        if v & 0x8000 != 0 {
            self.trigger();
        }
    }

    // LFSR step rate = 524288 / r / 2^(shift+1) Hz with r=0.5 for
    // divisor code 0. In CPU cycles: (32 or 64*divisor) << shift.
    fn period(&self) -> i32 {
        (if self.divisor == 0 { 32 } else { 64 * self.divisor }) << self.shift
    }

    pub fn trigger(&mut self) {
        self.enabled = self.dac_on;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.freq_timer = self.period();
        self.vol = self.env_init;
        self.env_timer = if self.env_period != 0 { self.env_period } else { 8 };
        self.lfsr = 0x7FFF;
    }

    pub fn clock(&mut self) {
        let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
        self.lfsr = (self.lfsr >> 1) | (bit << 14);
        if self.width7 {
            self.lfsr = (self.lfsr & !0x40) | (bit << 6);
        }
    }
    // Channel output bit: inverted LFSR bit 0.
    pub fn out_bit(&self) -> u32 {
        (!self.lfsr) & 1
    }

    pub fn clock_length(&mut self) {
        if self.length_enable && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
    pub fn clock_envelope(&mut self) {
        if self.env_period == 0 {
            return;
        }
        self.env_timer -= 1;
        if self.env_timer > 0 {
            return;
        }
        self.env_timer = self.env_period;
        if self.env_dir != 0 {
            if self.vol < 15 {
                self.vol += 1;
            }
        } else if self.vol > 0 {
            self.vol -= 1;
        }
    }

    pub fn advance(&mut self, cycles: i32) {
        if !self.enabled {
            return;
        }
        self.freq_timer -= cycles;
        let p = self.period();
        // Bounded: p >= 32 and advance() chunks are <= 512 cycles, so this
        // loops at most 17 times.
        while self.freq_timer <= 0 {
            self.freq_timer += p;
            self.clock();
        }
    }

    pub fn output(&self) -> f32 {
        if !self.enabled || !self.dac_on {
            return 0.0;
        }
        if self.out_bit() != 0 {
            self.vol as f32
        } else {
            -self.vol as f32
        }
    }
}

pub struct Sound {
    // Each FIFO is a 32-entry ring of 8-bit signed PCM samples.
    pub fifo_a: [i8; FIFO_SIZE],
    pub fifo_b: [i8; FIFO_SIZE],
    pub head_a: usize,
    pub tail_a: usize,
    pub count_a: i32,
    pub head_b: usize,
    pub tail_b: usize,
    pub count_b: i32,
    // Most-recently-drained sample value for each channel; held until the
    // next timer overflow drains the FIFO again. Real hardware does the
    // same — DAC output stays at the last sample between drains.
    pub cur_a: i32,
    pub cur_b: i32,

    pub soundcnt_l: u32, // PSG master L/R volume + per-channel L/R enables
    pub soundcnt_h: u32,
    pub soundcnt_x: u32,

    pub ch1: SquareChannel,
    pub ch2: SquareChannel,
    pub ch3: WaveChannel,
    pub ch4: NoiseChannel,

    // Raw write-latch for 0x60..0x84 (16-bit regs, index = (addr-0x60)>>1).
    // Readback applies GBATEK masks; byte writes read-modify-write against
    // THIS (not the masked readback) so e.g. a byte write to the envelope
    // half of SOUND1CNT_H can't zero the write-only length bits.
    pub reg_raw: [u16; 0x13],

    // Cycle accumulators for the 32768 Hz output sampler and the 512 Hz
    // PSG frame sequencer.
    sample_acc: i32,
    seq_acc: i32,
    seq_step: u32,

    // Per-frame INTERLEAVED STEREO sample buffer [L, R, L, R, ...] the
    // host audio sink drains each runFrame(). One GBA frame at 32768 Hz
    // is ~547 pairs = ~1094 floats; 4096 leaves margin for skipped drains.
    pub output: [f32; 4096],
    pub output_len: usize,

    // Output sample rate. Fixed: we emit one stereo pair every 512 CPU
    // cycles regardless of the DirectSound timer rate (FIFO samples are
    // zero-order held between timer overflows, like the hardware DAC).
    pub sample_rate: u32,
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Sound {
    // The TS constructor took `dma`; per the porting contract that
    // collaborator becomes a `&mut` parameter on `on_timer_overflow`
    // instead of a stored field.
    pub fn new() -> Self {
        Sound {
            fifo_a: [0; FIFO_SIZE],
            fifo_b: [0; FIFO_SIZE],
            head_a: 0,
            tail_a: 0,
            count_a: 0,
            head_b: 0,
            tail_b: 0,
            count_b: 0,
            cur_a: 0,
            cur_b: 0,
            soundcnt_l: 0,
            soundcnt_h: 0,
            soundcnt_x: 0,
            ch1: SquareChannel::new(),
            ch2: SquareChannel::new(),
            ch3: WaveChannel::new(),
            ch4: NoiseChannel::new(),
            reg_raw: [0; 0x13],
            sample_acc: 0,
            seq_acc: 0,
            seq_step: 0,
            output: [0.0; 4096],
            output_len: 0,
            sample_rate: 32768,
        }
    }

    pub fn reset(&mut self) {
        self.head_a = 0;
        self.tail_a = 0;
        self.count_a = 0;
        self.head_b = 0;
        self.tail_b = 0;
        self.count_b = 0;
        self.cur_a = 0;
        self.cur_b = 0;
        self.output_len = 0;
        self.soundcnt_l = 0;
        self.soundcnt_h = 0;
        self.soundcnt_x = 0;
        self.sample_acc = 0;
        self.seq_acc = 0;
        self.seq_step = 0;
        self.ch1.reset();
        self.ch2.reset();
        self.ch3.reset();
        self.ch4.reset();
        self.reg_raw.fill(0);
    }

    // Push one byte into the FIFO (called when the game writes to the
    // FIFO_A_L/H or FIFO_B_L/H MMIO ports, including via DMA).
    pub fn push_a(&mut self, b: u32) {
        if self.count_a >= FIFO_SIZE as i32 {
            return;
        }
        self.fifo_a[self.tail_a] = ((b << 24) as i32 >> 24) as i8; // sign-extend to int8
        self.tail_a = (self.tail_a + 1) % FIFO_SIZE;
        self.count_a += 1;
    }
    pub fn push_b(&mut self, b: u32) {
        if self.count_b >= FIFO_SIZE as i32 {
            return;
        }
        self.fifo_b[self.tail_b] = ((b << 24) as i32 >> 24) as i8;
        self.tail_b = (self.tail_b + 1) % FIFO_SIZE;
        self.count_b += 1;
    }

    // ---- MMIO ----------------------------------------------------------

    // 16-bit write to the sound block, addr = IO offset 0x60..0x9E.
    pub fn write_reg16(&mut self, addr: u32, v: u32) {
        let v = v & 0xFFFF;
        // Wave RAM is not gated by the master enable.
        if (0x90..=0x9E).contains(&addr) {
            let off = addr - 0x90;
            self.ch3.write_ram8(off, v & 0xFF);
            self.ch3.write_ram8(off + 1, v >> 8);
            return;
        }
        // While master-disabled, PSG registers 0x60..0x81 are write-
        // protected and read as zero (GBATEK). SOUNDCNT_H/X stay writable.
        if addr <= 0x80 && (self.soundcnt_x & 0x80) == 0 {
            return;
        }
        // Latch the raw value for byte-write RMW. Trigger bits are not
        // sticky — clear them so a later low-byte RMW can't re-trigger.
        let is_ctl_x = addr == 0x64 || addr == 0x6C || addr == 0x74 || addr == 0x7C;
        self.reg_raw[((addr - 0x60) >> 1) as usize] =
            if is_ctl_x { (v & 0x7FFF) as u16 } else { v as u16 };
        match addr {
            0x60 => self.ch1.write_sweep(v),
            0x62 => self.ch1.write_duty_len_env(v),
            0x64 => self.ch1.write_freq_ctl(v),
            0x68 => self.ch2.write_duty_len_env(v),
            0x6C => self.ch2.write_freq_ctl(v),
            0x70 => self.ch3.write_ctl(v),
            0x72 => self.ch3.write_len_vol(v),
            0x74 => self.ch3.write_freq_ctl(v),
            0x78 => self.ch4.write_len_env(v),
            0x7C => self.ch4.write_ctl(v),
            0x80 => self.soundcnt_l = v,
            0x82 => self.write_soundcnt_h(v),
            0x84 => self.write_soundcnt_x(v),
            _ => {}
        }
    }

    // 16-bit read, addr = IO offset 0x60..0x9E. GBATEK read masks: length
    // fields, frequencies and trigger bits are write-only and read as 0.
    pub fn read_reg16(&self, addr: u32) -> u32 {
        if (0x90..=0x9E).contains(&addr) {
            let off = addr - 0x90;
            return self.ch3.read_ram8(off) | (self.ch3.read_ram8(off + 1) << 8);
        }
        match addr {
            0x60 => self.reg_raw[0x00] as u32 & 0x007F,
            0x62 => self.reg_raw[0x01] as u32 & 0xFFC0,
            0x64 => self.reg_raw[0x02] as u32 & 0x4000,
            0x68 => self.reg_raw[0x04] as u32 & 0xFFC0,
            0x6C => self.reg_raw[0x06] as u32 & 0x4000,
            0x70 => self.reg_raw[0x08] as u32 & 0x00E0,
            0x72 => self.reg_raw[0x09] as u32 & 0xE000,
            0x74 => self.reg_raw[0x0A] as u32 & 0x4000,
            0x78 => self.reg_raw[0x0C] as u32 & 0xFF00,
            0x7C => self.reg_raw[0x0E] as u32 & 0x40FF,
            0x80 => self.soundcnt_l & 0xFF77,
            0x82 => self.soundcnt_h & 0x770F,
            // SOUNDCNT_X: master enable + READ-ONLY live channel-active flags.
            0x84 => {
                (self.soundcnt_x & 0x80)
                    | (if self.ch1.enabled { 1 } else { 0 })
                    | (if self.ch2.enabled { 2 } else { 0 })
                    | (if self.ch3.enabled { 4 } else { 0 })
                    | (if self.ch4.enabled { 8 } else { 0 })
            }
            _ => 0, // unused gaps (0x66, 0x6A, 0x6E, 0x76, 0x7A, 0x7E, 0x86)
        }
    }

    // Raw (unmasked) value for byte-write read-modify-write in Io.write8.
    pub fn raw_read16(&self, addr: u32) -> u32 {
        if addr == 0x82 {
            return self.soundcnt_h;
        }
        if addr == 0x84 {
            return self.soundcnt_x;
        }
        if (0x60..=0x80).contains(&addr) {
            return self.reg_raw[((addr - 0x60) >> 1) as usize] as u32;
        }
        0
    }

    // Bit 11 of SOUNDCNT_H is the FIFO A reset; bit 15 is FIFO B reset.
    // Writing the bit clears that FIFO.
    pub fn write_soundcnt_h(&mut self, v: u32) {
        self.soundcnt_h = v & 0xFFFF;
        if v & 0x0800 != 0 {
            self.head_a = 0;
            self.tail_a = 0;
            self.count_a = 0;
        }
        if v & 0x8000 != 0 {
            self.head_b = 0;
            self.tail_b = 0;
            self.count_b = 0;
        }
    }
    pub fn write_soundcnt_x(&mut self, v: u32) {
        let was_on = (self.soundcnt_x & 0x80) != 0;
        self.soundcnt_x = v & 0x80; // only bit 7 is writable
        if (v & 0x80) == 0 {
            self.head_a = 0;
            self.tail_a = 0;
            self.count_a = 0;
            self.head_b = 0;
            self.tail_b = 0;
            self.count_b = 0;
            self.cur_a = 0;
            self.cur_b = 0;
            // Master disable zeroes all PSG registers (0x60..0x81) — they
            // must be re-initialized after re-enabling (GBATEK).
            if was_on {
                self.ch1.reset();
                self.ch2.reset();
                self.ch3.reset();
                self.ch4.reset();
                self.soundcnt_l = 0;
                self.reg_raw[..0x11].fill(0);
            }
        } else if !was_on {
            self.sample_acc = 0;
            self.seq_acc = 0;
            self.seq_step = 0;
        }
    }

    // ---- DirectSound FIFO drain (from Timers.overflow) ------------------

    // Called from Timers.overflow(timerIdx). Drains one sample from any
    // FIFO whose timer-select bit matches this timer. (Sample EMISSION is
    // handled by step(); the drained value is held in curA/curB.)
    //
    // Returns `(refill_a, refill_b)`: whether FIFO A/B dropped below
    // half-full and needs a special-timing DMA refill. The TS triggered the
    // DMA inline via a stored `dma` ref; here the orchestrator runs it after
    // this returns (the bus can't be re-entered while `Sound` is borrowed).
    pub fn on_timer_overflow(&mut self, timer_idx: u32) -> (bool, bool) {
        if (self.soundcnt_x & 0x80) == 0 {
            return (false, false); // master disable → silence
        }
        let timer_a = (self.soundcnt_h >> 10) & 1;
        let timer_b = (self.soundcnt_h >> 14) & 1;
        let mut refill_a = false;
        let mut refill_b = false;

        if timer_a == timer_idx {
            if self.count_a > 0 {
                self.cur_a = self.fifo_a[self.head_a] as i32;
                self.head_a = (self.head_a + 1) % FIFO_SIZE;
                self.count_a -= 1;
            }
            // Refill via DMA1 special-timing if the FIFO is below half-full.
            if self.count_a <= 16 {
                refill_a = true;
            }
        }
        if timer_b == timer_idx {
            if self.count_b > 0 {
                self.cur_b = self.fifo_b[self.head_b] as i32;
                self.head_b = (self.head_b + 1) % FIFO_SIZE;
                self.count_b -= 1;
            }
            if self.count_b <= 16 {
                refill_b = true;
            }
        }
        (refill_a, refill_b)
    }

    // ---- Cycle-driven sampler + PSG clocks ------------------------------

    // Step the PSG generators and emit output samples. Called from the
    // emulator main loop with the same per-batch cycle counts Timers get.
    pub fn step(&mut self, cycles: i32) {
        if (self.soundcnt_x & 0x80) == 0 {
            return; // master off: nothing ticks
        }
        let mut rem = cycles;
        while rem > 0 {
            // Process up to the next output-sample boundary so generator
            // state is correct at the instant each sample is taken.
            let mut n = SAMPLE_CYCLES - self.sample_acc;
            if n > rem {
                n = rem;
            }
            self.ch1.advance(n);
            self.ch2.advance(n);
            self.ch3.advance(n);
            self.ch4.advance(n);
            self.seq_acc += n;
            if self.seq_acc >= SEQ_CYCLES {
                self.seq_acc -= SEQ_CYCLES;
                self.tick_sequencer();
            }
            self.sample_acc += n;
            if self.sample_acc >= SAMPLE_CYCLES {
                self.sample_acc = 0;
                self.emit_sample();
            }
            rem -= n;
        }
    }

    // 512 Hz frame sequencer: lengths at 256 Hz (even steps), sweep at
    // 128 Hz (steps 2 and 6), envelopes at 64 Hz (step 7).
    fn tick_sequencer(&mut self) {
        let s = self.seq_step;
        self.seq_step = (s + 1) & 7;
        if (s & 1) == 0 {
            self.ch1.clock_length();
            self.ch2.clock_length();
            self.ch3.clock_length();
            self.ch4.clock_length();
        }
        if s == 2 || s == 6 {
            self.ch1.clock_sweep();
        }
        if s == 7 {
            self.ch1.clock_envelope();
            self.ch2.clock_envelope();
            self.ch4.clock_envelope();
        }
    }

    fn emit_sample(&mut self) {
        // PSG digital outputs, each in -15..15.
        let o1 = self.ch1.output();
        let o2 = self.ch2.output();
        let o3 = self.ch3.output();
        let o4 = self.ch4.output();
        let cl = self.soundcnt_l;
        // Per-channel routing: SOUNDCNT_L bits 8-11 = right, 12-15 = left.
        let mut r: f32 = 0.0;
        let mut l: f32 = 0.0;
        if cl & 0x0100 != 0 {
            r += o1;
        }
        if cl & 0x0200 != 0 {
            r += o2;
        }
        if cl & 0x0400 != 0 {
            r += o3;
        }
        if cl & 0x0800 != 0 {
            r += o4;
        }
        if cl & 0x1000 != 0 {
            l += o1;
        }
        if cl & 0x2000 != 0 {
            l += o2;
        }
        if cl & 0x4000 != 0 {
            l += o3;
        }
        if cl & 0x8000 != 0 {
            l += o4;
        }
        // Master side volume (0-7 → 1/8..8/8), PSG ratio (25/50/100%), then
        // scale so all four channels at max volume sum to ±0.5 (one PSG
        // channel at full volume = ±0.125, comparable to one DirectSound
        // channel's ±0.5).
        let ratio = PSG_RATIO[(self.soundcnt_h & 3) as usize];
        r *= ((((cl >> 0) & 7) + 1) as f32) / 8.0 * ratio / 120.0;
        l *= ((((cl >> 4) & 7) + 1) as f32) / 8.0 * ratio / 120.0;
        // DirectSound: SOUNDCNT_H bit 2/3 = A/B volume ratio (50%/100%);
        // bits 8/9 = A right/left enable, bits 12/13 = B right/left enable.
        let a_gain = if self.soundcnt_h & 0x04 != 0 { 1.0 } else { 0.5 };
        let b_gain = if self.soundcnt_h & 0x08 != 0 { 1.0 } else { 0.5 };
        let a = (self.cur_a as f32 * a_gain) / 256.0; // ±0.5 at 100%
        let b = (self.cur_b as f32 * b_gain) / 256.0;
        if self.soundcnt_h & 0x0100 != 0 {
            r += a;
        }
        if self.soundcnt_h & 0x0200 != 0 {
            l += a;
        }
        if self.soundcnt_h & 0x1000 != 0 {
            r += b;
        }
        if self.soundcnt_h & 0x2000 != 0 {
            l += b;
        }
        if self.output_len + 2 <= self.output.len() {
            self.output[self.output_len] = if l < -1.0 {
                -1.0
            } else if l > 1.0 {
                1.0
            } else {
                l
            };
            self.output_len += 1;
            self.output[self.output_len] = if r < -1.0 {
                -1.0
            } else if r > 1.0 {
                1.0
            } else {
                r
            };
            self.output_len += 1;
        }
    }

    // Pop the per-frame samples for the audio sink. Returns a NEW Vec
    // (small copy) so the caller can hand it directly to the host sink.
    // Layout: interleaved stereo [L, R, L, R, ...].
    pub fn drain_output(&mut self) -> Vec<f32> {
        let out = self.output[..self.output_len].to_vec();
        self.output_len = 0;
        out
    }
}

// Tests ported from the (deleted) TypeScript suite src/test/sound.test.ts.
// Mostly harness style (B): construct `Sound` directly (its TS `dma`
// collaborator is a `&mut` parameter on `on_timer_overflow`, unused here)
// and drive `step` with CPU cycles, inspecting the interleaved stereo float
// output. The MMIO-routing block uses style (A) through a real `Gba`.
#[cfg(test)]
mod tests {
    use super::*;

    // Master on, full L/R volume, all four PSG channels both sides, PSG ratio
    // 100%. One channel at envelope volume 15 then produces samples of
    // amplitude 15 * ((7+1)/8) * 1.0 / 120 = 0.125 per side.
    fn make_sound() -> Sound {
        let mut s = Sound::new();
        s.write_reg16(0x84, 0x80); // SOUNDCNT_X: master enable
        s.write_reg16(0x80, 0xFF77); // SOUNDCNT_L: vol L=R=7, all channels L+R
        s.write_reg16(0x82, 0x0002); // SOUNDCNT_H: PSG ratio 100%
        s
    }

    const AMP: f32 = 0.125;
    const SQ_FREQ: u32 = 1792;

    fn lefts(buf: &[f32]) -> Vec<f32> {
        buf.iter().step_by(2).copied().collect()
    }
    fn rights(buf: &[f32]) -> Vec<f32> {
        buf.iter().skip(1).step_by(2).copied().collect()
    }
    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "expected {a} ~= {b}");
    }

    // ---- PSG channel 1: square wave ------------------------------------

    #[test]
    fn ch1_duty_high_sample_counts() {
        for (duty, expect_high) in [(0u32, 8usize), (1, 16), (2, 32), (3, 48)] {
            let mut s = make_sound();
            s.write_reg16(0x62, (15 << 12) | (duty << 6)); // env init 15, no envelope
            s.write_reg16(0x64, 0x8000 | SQ_FREQ); // trigger
            s.step(512 * 64);
            let l = lefts(&s.drain_output());
            assert_eq!(l.len(), 64);
            assert_eq!(l.iter().filter(|&&v| v > 0.0).count(), expect_high);
            for v in &l {
                close(v.abs(), AMP);
            }
        }
    }

    #[test]
    fn ch1_envelope_decreases() {
        let mut s = make_sound();
        s.write_reg16(0x62, (15 << 12) | (1 << 8)); // init 15, decrease, period 1
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        assert_eq!(s.ch1.vol, 15);
        s.step(8 * 32768); // envelope clocks on frame-sequencer step 7
        assert_eq!(s.ch1.vol, 14);
        s.drain_output();
        s.step(8 * 32768);
        assert_eq!(s.ch1.vol, 13);
        let l = lefts(&s.drain_output());
        close(l[l.len() - 1].abs(), AMP * 13.0 / 15.0);
    }

    #[test]
    fn ch1_envelope_increase() {
        let mut s = make_sound();
        s.write_reg16(0x62, (1 << 12) | (1 << 11) | (1 << 8)); // init 1, increase, period 1
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        s.step(8 * 32768);
        assert_eq!(s.ch1.vol, 2);
    }

    #[test]
    fn ch1_length_expiry_silences() {
        let mut s = make_sound();
        s.write_reg16(0x62, (15 << 12) | 62); // length counter = 64-62 = 2
        s.write_reg16(0x64, 0x8000 | 0x4000 | SQ_FREQ); // trigger + length enable
        assert_eq!(s.read_reg16(0x84) & 1, 1);
        s.step(3 * 32768);
        assert_eq!(s.read_reg16(0x84) & 1, 0);
        s.drain_output();
        s.step(512 * 8);
        assert!(s.drain_output().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn ch1_sweep_overflow_at_trigger_disables() {
        let mut s = make_sound();
        s.write_reg16(0x60, (1 << 4) | 1); // period 1, addition, shift 1
        s.write_reg16(0x62, 15 << 12);
        s.write_reg16(0x64, 0x8000 | 2000); // 2000 + (2000>>1) > 2047
        assert_eq!(s.read_reg16(0x84) & 1, 0);
    }

    #[test]
    fn ch1_sweep_raises_then_overflows() {
        let mut s = make_sound();
        s.write_reg16(0x60, (1 << 4) | 2); // period 1, addition, shift 2
        s.write_reg16(0x62, 15 << 12);
        s.write_reg16(0x64, 0x8000 | 1024);
        assert_eq!(s.read_reg16(0x84) & 1, 1);
        s.step(3 * 32768); // step 2 → 1024 + 256 = 1280
        assert_eq!(s.ch1.freq, 1280);
        assert_eq!(s.read_reg16(0x84) & 1, 1);
        s.step(4 * 32768); // step 6 → 1280 + 320 = 1600
        assert_eq!(s.ch1.freq, 1600);
        assert_eq!(s.read_reg16(0x84) & 1, 1);
        s.step(4 * 32768); // step 2 → 1600 + 400 = 2000, lookahead 2500 overflows
        assert_eq!(s.ch1.freq, 2000);
        assert_eq!(s.read_reg16(0x84) & 1, 0);
    }

    #[test]
    fn ch1_dac_off_stays_inactive() {
        let mut s = make_sound();
        s.write_reg16(0x62, 0);
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        assert_eq!(s.read_reg16(0x84) & 1, 0);
    }

    // ---- PSG channel 2: square (no sweep) ------------------------------

    #[test]
    fn ch2_25pct_duty() {
        let mut s = make_sound();
        s.write_reg16(0x68, (15 << 12) | (1 << 6));
        s.write_reg16(0x6C, 0x8000 | SQ_FREQ);
        assert_eq!(s.read_reg16(0x84) & 2, 2);
        s.step(512 * 64);
        let l = lefts(&s.drain_output());
        assert_eq!(l.iter().filter(|&&v| v > 0.0).count(), 16);
    }

    #[test]
    fn ch2_length_expiry_clears_bit1() {
        let mut s = make_sound();
        s.write_reg16(0x68, (15 << 12) | 63); // counter = 1
        s.write_reg16(0x6C, 0x8000 | 0x4000 | SQ_FREQ);
        s.step(32768);
        assert_eq!(s.read_reg16(0x84) & 2, 0);
    }

    // ---- PSG channel 3: wave -------------------------------------------

    #[test]
    fn ch3_cpu_wave_ram_targets_non_playing_bank() {
        let mut s = make_sound();
        s.write_reg16(0x70, 0x00); // play bank 0 → CPU sees bank 1
        s.write_reg16(0x90, 0xBEEF);
        assert_eq!(s.read_reg16(0x90), 0xBEEF);
        s.write_reg16(0x70, 0x40); // play bank 1 → CPU sees bank 0
        assert_eq!(s.read_reg16(0x90), 0x0000);
        s.write_reg16(0x90, 0x1234);
        assert_eq!(s.read_reg16(0x90), 0x1234);
        s.write_reg16(0x70, 0x00); // back: bank 1 contents preserved
        assert_eq!(s.read_reg16(0x90), 0xBEEF);
    }

    // Fill bank 0 with the nibble ramp 0,1,...,15,0,...,15 (high nibble first).
    fn fill_ramp(s: &mut Sound) {
        s.write_reg16(0x70, 0x40); // select bank 1 → CPU writes land in bank 0
        let mut off = 0u32;
        while off < 16 {
            let b0 = (((off * 2) % 16) << 4) | ((off * 2 + 1) % 16);
            let b1 = ((((off + 1) * 2) % 16) << 4) | (((off + 1) * 2 + 1) % 16);
            s.write_reg16(0x90 + off, b0 | (b1 << 8));
            off += 2;
        }
    }

    #[test]
    fn ch3_plays_high_nibble_first_full_volume() {
        let mut s = make_sound();
        fill_ramp(&mut s);
        s.write_reg16(0x70, 0x80); // playback on, bank 0, 32-sample mode
        s.write_reg16(0x72, 1 << 13); // volume code 1 = 100%
        s.write_reg16(0x74, 0x8000 | 1984); // step = 512 cycles = 1 output sample
        assert_eq!(s.read_reg16(0x84) & 4, 4);
        s.step(512 * 32);
        let l = lefts(&s.drain_output());
        for k in 0..32usize {
            let nib = ((k + 1) & 31) % 16;
            close(l[k], (nib as f32 * 2.0 - 15.0) / 120.0);
        }
    }

    #[test]
    fn ch3_volume_control() {
        for (vol_bits, factor) in [
            (2u32 << 13, 0.5f32),
            (3 << 13, 0.25),
            (0x8000, 0.75),
            (0 << 13, 0.0),
        ] {
            let mut s = make_sound();
            fill_ramp(&mut s);
            s.write_reg16(0x70, 0x80);
            s.write_reg16(0x72, vol_bits);
            s.write_reg16(0x74, 0x8000 | 1984);
            s.step(512 * 4);
            let l = lefts(&s.drain_output());
            for k in 0..4usize {
                let nib = ((k + 1) & 31) % 16;
                close(l[k], (nib as f32 * 2.0 - 15.0) * factor / 120.0);
            }
        }
    }

    #[test]
    fn ch3_64_sample_spans_both_banks() {
        let mut s = make_sound();
        fill_ramp(&mut s); // ramp in bank 0
        s.write_reg16(0x70, 0x00); // play bank 0 → CPU writes bank 1
        let mut off = 0u32;
        while off < 16 {
            s.write_reg16(0x90 + off, 0xFFFF); // bank 1 all 15s
            off += 2;
        }
        s.write_reg16(0x70, 0x80 | 0x20); // playback, bank 0, 64-sample mode
        s.write_reg16(0x72, 1 << 13);
        s.write_reg16(0x74, 0x8000 | 1984);
        s.step(512 * 64);
        let l = lefts(&s.drain_output());
        for k in 32..62usize {
            close(l[k], (15.0 * 2.0 - 15.0) / 120.0);
        }
        close(l[0], (1.0 * 2.0 - 15.0) / 120.0);
    }

    #[test]
    fn ch3_length_expiry_clears_bit2() {
        let mut s = make_sound();
        s.write_reg16(0x70, 0x80);
        s.write_reg16(0x72, (1 << 13) | 255); // counter = 1
        s.write_reg16(0x74, 0x8000 | 0x4000 | 1984);
        assert_eq!(s.read_reg16(0x84) & 4, 4);
        s.step(32768);
        assert_eq!(s.read_reg16(0x84) & 4, 0);
    }

    // ---- PSG channel 4: noise ------------------------------------------

    #[test]
    fn ch4_7bit_lfsr_period_127() {
        let mut s = make_sound();
        s.write_reg16(0x78, 15 << 12);
        s.write_reg16(0x7C, 0x8000 | 0x08); // trigger, 7-bit width
        let mut seq1 = Vec::new();
        let mut seq2 = Vec::new();
        for _ in 0..127 {
            s.ch4.clock();
            seq1.push(s.ch4.out_bit());
        }
        for _ in 0..127 {
            s.ch4.clock();
            seq2.push(s.ch4.out_bit());
        }
        assert_eq!(seq1, seq2);
        assert!(seq1.iter().any(|&b| b == 0));
        assert!(seq1.iter().any(|&b| b == 1));
    }

    #[test]
    fn ch4_15bit_lfsr_no_period_127() {
        let mut s = make_sound();
        s.write_reg16(0x78, 15 << 12);
        s.write_reg16(0x7C, 0x8000); // trigger, 15-bit width
        let mut seq1 = Vec::new();
        let mut seq2 = Vec::new();
        for _ in 0..127 {
            s.ch4.clock();
            seq1.push(s.ch4.out_bit());
        }
        for _ in 0..127 {
            s.ch4.clock();
            seq2.push(s.ch4.out_bit());
        }
        assert_ne!(seq1, seq2);
    }

    #[test]
    fn ch4_emits_pm_vol_and_envelope() {
        let mut s = make_sound();
        s.write_reg16(0x78, (15 << 12) | (1 << 8)); // init 15, decrease, period 1
        s.write_reg16(0x7C, 0x8000 | 0x01); // divisor 1, shift 0
        s.step(512 * 32);
        let l = lefts(&s.drain_output());
        for v in &l {
            close(v.abs(), AMP);
        }
        assert!(l.iter().any(|&v| v > 0.0));
        assert!(l.iter().any(|&v| v < 0.0));
        s.step(8 * 32768); // one envelope tick
        assert_eq!(s.ch4.vol, 14);
    }

    #[test]
    fn ch4_length_expiry_clears_bit3() {
        let mut s = make_sound();
        s.write_reg16(0x78, (15 << 12) | 63);
        s.write_reg16(0x7C, 0x8000 | 0x4000);
        assert_eq!(s.read_reg16(0x84) & 8, 8);
        s.step(32768);
        assert_eq!(s.read_reg16(0x84) & 8, 0);
    }

    // ---- SOUNDCNT_L/H/X ------------------------------------------------

    #[test]
    fn master_disable_write_protects_psg() {
        let mut s = Sound::new();
        s.write_reg16(0x62, 0xF040);
        assert_eq!(s.read_reg16(0x62), 0);
        s.step(512 * 4);
        assert_eq!(s.drain_output().len(), 0);
    }

    #[test]
    fn master_disable_zeroes_psg_and_kills_channels() {
        let mut s = make_sound();
        s.write_reg16(0x62, (15 << 12) | (2 << 6));
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        assert_eq!(s.read_reg16(0x62), 0xF080);
        assert_eq!(s.read_reg16(0x84) & 1, 1);
        s.write_reg16(0x84, 0);
        s.write_reg16(0x84, 0x80);
        assert_eq!(s.read_reg16(0x62), 0);
        assert_eq!(s.read_reg16(0x80), 0);
        assert_eq!(s.read_reg16(0x84) & 0xF, 0);
    }

    #[test]
    fn hard_left_pan() {
        let mut s = make_sound();
        s.write_reg16(0x80, 0x1077); // ch1 LEFT only, master vol 7/7
        s.write_reg16(0x62, 15 << 12);
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        s.step(512 * 64);
        let out = s.drain_output();
        let lsum: f32 = lefts(&out).iter().map(|v| v.abs()).sum();
        let rsum: f32 = rights(&out).iter().map(|v| v.abs()).sum();
        assert_eq!(rsum, 0.0);
        assert!(lsum > 0.0);
    }

    #[test]
    fn hard_right_pan() {
        let mut s = make_sound();
        s.write_reg16(0x80, 0x0177); // ch1 RIGHT only
        s.write_reg16(0x62, 15 << 12);
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        s.step(512 * 64);
        let out = s.drain_output();
        let lsum: f32 = lefts(&out).iter().map(|v| v.abs()).sum();
        let rsum: f32 = rights(&out).iter().map(|v| v.abs()).sum();
        assert_eq!(lsum, 0.0);
        assert!(rsum > 0.0);
    }

    #[test]
    fn per_side_master_volume() {
        let mut s = make_sound();
        // ch1 both sides; vol L=7 (full), R=3 (half).
        s.write_reg16(0x80, 0x1100 | (7 << 4) | 3);
        s.write_reg16(0x62, 15 << 12);
        s.write_reg16(0x64, 0x8000 | SQ_FREQ);
        s.step(512 * 8);
        let out = s.drain_output();
        close(out[0].abs(), AMP); // left: (7+1)/8
        close(out[1].abs(), AMP * 0.5); // right: (3+1)/8
    }

    #[test]
    fn soundcnt_h_psg_ratio() {
        for (bits, factor) in [(0u32, 0.25f32), (1, 0.5)] {
            let mut s = make_sound();
            s.write_reg16(0x82, bits);
            s.write_reg16(0x62, 15 << 12);
            s.write_reg16(0x64, 0x8000 | SQ_FREQ);
            s.step(512 * 8);
            let out = s.drain_output();
            close(out[0].abs(), AMP * factor);
        }
    }

    // ---- DirectSound stereo routing ------------------------------------

    #[test]
    fn fifo_a_left_only_100pct() {
        let mut s = Sound::new();
        s.write_reg16(0x84, 0x80);
        s.write_reg16(0x82, 0x0204); // A left enable (bit 9) + A 100% (bit 2), timer 0
        for _ in 0..4 {
            s.push_a(0x40);
        }
        s.on_timer_overflow(0);
        assert_eq!(s.cur_a, 0x40);
        s.step(512);
        let out = s.drain_output();
        close(out[0], 64.0 / 256.0); // left
        assert_eq!(out[1], 0.0); // right
    }

    #[test]
    fn fifo_b_right_only_50pct() {
        let mut s = Sound::new();
        s.write_reg16(0x84, 0x80);
        s.write_reg16(0x82, 0x1000); // B right enable (bit 12), B 50%, timer 0
        for _ in 0..4 {
            s.push_b(0x80); // -128
        }
        s.on_timer_overflow(0);
        assert_eq!(s.cur_b, -128);
        s.step(512);
        let out = s.drain_output();
        assert_eq!(out[0], 0.0); // left
        close(out[1], -128.0 * 0.5 / 256.0); // right
    }

    #[test]
    fn held_fifo_sample_repeats_between_drains() {
        let mut s = Sound::new();
        s.write_reg16(0x84, 0x80);
        s.write_reg16(0x82, 0x0304); // A both sides, 100%
        s.push_a(0x20);
        s.on_timer_overflow(0);
        s.step(512 * 4); // four output samples, no further drain
        let l = lefts(&s.drain_output());
        assert_eq!(l.len(), 4);
        for v in &l {
            close(*v, 32.0 / 256.0);
        }
    }

    // ---- MMIO routing through Gba (style A) ----------------------------

    use crate::bus::Bus;
    use crate::Gba;

    fn make_rig() -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        g
    }

    #[test]
    fn mmio_gbatek_read_masks() {
        let mut g = make_rig();
        g.write16(0x0400_0084, 0x80);
        g.write16(0x0400_0062, 0xF7BF); // length bits 0-5 write-only
        assert_eq!(g.read16(0x0400_0062), 0xF780);
        g.write16(0x0400_0064, 0x4000 | 1792); // freq + trigger read as 0
        assert_eq!(g.read16(0x0400_0064), 0x4000);
        assert_eq!(g.read16(0x0400_0066), 0); // unused gap
    }

    #[test]
    fn mmio_soundcnt_x_live_active_flags() {
        let mut g = make_rig();
        g.write16(0x0400_0084, 0x80);
        assert_eq!(g.read16(0x0400_0084), 0x80);
        g.write16(0x0400_0062, 15 << 12);
        g.write16(0x0400_0064, 0x8000 | 1792);
        assert_eq!(g.read16(0x0400_0084), 0x81);
    }

    #[test]
    fn mmio_byte_writes_rmw_raw_latch() {
        let mut g = make_rig();
        g.write16(0x0400_0084, 0x80);
        g.write16(0x0400_0062, 0x0080); // duty 2
        g.write8(0x0400_0063, 0x57); // envelope byte only
        assert_eq!(g.read16(0x0400_0062), 0x5780);
        g.write8(0x0400_0062, 0x40); // duty byte only (duty 1)
        assert_eq!(g.read16(0x0400_0062), 0x5740);
    }

    #[test]
    fn mmio_fifo_byte_writes() {
        let mut g = make_rig();
        g.write8(0x0400_00A0, 0x12);
        assert_eq!(g.sound.count_a, 1);
        g.write8(0x0400_00A4, 0x34);
        assert_eq!(g.sound.count_b, 1);
    }

    #[test]
    fn mmio_wave_ram_byte_non_playing_bank() {
        let mut g = make_rig();
        g.write16(0x0400_0084, 0x80);
        g.write16(0x0400_0070, 0x00); // play bank 0 → CPU sees bank 1
        g.write8(0x0400_0090, 0xAB);
        assert_eq!(g.sound.ch3.ram[16], 0xAB);
        assert_eq!(g.read8(0x0400_0090), 0xAB);
    }

    #[test]
    fn mmio_soundbias_round_trips() {
        let mut g = make_rig();
        g.write16(0x0400_0088, 0x0200);
        assert_eq!(g.read16(0x0400_0088), 0x0200);
    }
}
