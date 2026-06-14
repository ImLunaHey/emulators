//! SPU — Sound Processing Unit.
//!
//! Built from psx-spx "Sound Processing Unit (SPU)". 24 ADPCM voices plus
//! reverb, fed from 512 KB of dedicated sound RAM. The register window is
//! 0x1F80_1C00..0x1F80_1E00 (`off` here is relative to 0x1F80_1C00): the
//! 24 voice register blocks (16 bytes each, +0x000..+0x180), then the
//! control/status block (main volume, KON/KOFF, PMON/NON/EON/ENDX, reverb
//! registers, `SPUCNT` at +0x1AA, `SPUSTAT` at +0x1AE, transfer address/FIFO).
//!
//! This implementation decodes ADPCM, runs ADSR envelopes, applies pitch with
//! 4-point Gaussian interpolation, mixes 24 voices into f32 stereo, and pushes
//! samples into an output ring that the host drains via [`Spu::drain`]. Reverb
//! is intentionally minimal (a single feedback echo line) — enough to be
//! audible without a faithful 22.05 kHz APF/comb network.
//!
//! Spec offsets are in *bytes* within the window. Registers are 16-bit; the
//! 32-bit ones (KON/KOFF/PMON/NON/EON/ENDX, CD/Ext volume) are accessed as two
//! adjacent halfwords by the CPU and DMA, so we keep the canonical state in
//! `u32` fields and mirror the relevant halfword on read/write.

/// 512 KB of SPU sound RAM (ADPCM samples, reverb work area, capture buffers).
pub const SOUND_RAM_SIZE: usize = 0x8_0000;

/// SPU register-window size in bytes (0x1F80_1C00..0x1F80_1E00).
pub const SPU_REGS: usize = 0x200;

/// Number of hardware voices.
pub const VOICE_COUNT: usize = 24;

/// System clocks per 44.1 kHz sample tick (33.8688 MHz / 44100 ≈ 768).
pub const CYCLES_PER_SAMPLE: u32 = 768;

/// XA-ADPCM filter coefficients (positive / "old"·f0). Same table the CD-XA
/// decoder uses; index = filter field of the block header (clamped to 0..4).
const ADPCM_POS: [i32; 5] = [0, 60, 115, 98, 122];
/// XA-ADPCM filter coefficients (negative / "older"·f1).
const ADPCM_NEG: [i32; 5] = [0, 0, -52, -55, -60];

/// The ADSR envelope phase for a single voice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AdsrPhase {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Per-voice runtime state (the part that is *not* the raw register file).
#[derive(Clone, Copy)]
struct Voice {
    // ---- registers (also shadowed in `regs`, but decoded here for use) ----
    vol_left: u16,
    vol_right: u16,
    pitch: u16,
    start_addr: u16, // in 8-byte units
    adsr1: u16,
    adsr2: u16,
    repeat_addr: u16, // in 8-byte units

    // ---- ADPCM streaming state ----
    /// Current decode address in sound RAM (byte address of the 16-byte block).
    cur_addr: u32,
    /// 17.12 fixed-point pitch counter; bits 12+ index the block sample.
    counter: u32,
    /// Index (0..28) of the next sample to decode within the current block.
    block_offset: u32,
    /// The 28 decoded samples of the current block.
    decoded: [i16; 28],
    /// Most-recent two decoded samples (ADPCM history, persists across blocks).
    hist0: i32,
    hist1: i32,
    /// The 3 previous output samples used for interpolation (older..newest).
    interp: [i16; 4],

    // ---- ADSR state ----
    phase: AdsrPhase,
    /// Current envelope level (0..0x7FFF).
    adsr_vol: i32,
    /// Cycles remaining before the next envelope step.
    adsr_cycles: u32,
}

impl Voice {
    fn new() -> Self {
        Voice {
            vol_left: 0,
            vol_right: 0,
            pitch: 0,
            start_addr: 0,
            adsr1: 0,
            adsr2: 0,
            repeat_addr: 0,
            cur_addr: 0,
            counter: 0,
            block_offset: 0,
            decoded: [0; 28],
            hist0: 0,
            hist1: 0,
            interp: [0; 4],
            phase: AdsrPhase::Off,
            adsr_vol: 0,
            adsr_cycles: 0,
        }
    }

    fn is_on(&self) -> bool {
        self.phase != AdsrPhase::Off
    }
}

/// The SPU register file + sound RAM + voice mixer.
pub struct Spu {
    /// Flat shadow of the 512-byte register window (little-endian halfwords).
    /// Reads of unmodelled registers fall back to this so software round-trips.
    pub regs: [u16; SPU_REGS / 2],
    /// 512 KB sound RAM.
    pub ram: Box<[u8; SOUND_RAM_SIZE]>,
    /// `SPUCNT` (control, +0x1AA).
    pub spucnt: u16,
    /// Sound-RAM transfer address (current byte pointer).
    pub transfer_addr: u32,

    // ---- control block ----
    main_vol_left: u16,
    main_vol_right: u16,
    /// Key-on / key-off latches (write triggers, sampled at the next tick).
    kon: u32,
    koff: u32,
    pmon: u32,
    non: u32,
    eon: u32,
    /// Voice-end status (bit set when a voice hits a loop-end block).
    endx: u32,
    /// Transfer base address (`Transfer Addr` reg, in 8-byte units → bytes).
    transfer_base: u16,
    /// IRQ trigger address (in 8-byte units).
    irq_addr: u16,
    reverb_base: u16,

    // ---- voices ----
    voices: [Voice; VOICE_COUNT],

    // ---- noise generator ----
    noise_level: i16,
    noise_timer: i32,

    // ---- minimal reverb (single delay line in a small fixed buffer) ----
    reverb_buf: Box<[(f32, f32); REVERB_LEN]>,
    reverb_pos: usize,

    // ---- output ring ----
    /// Interleaved L,R f32 samples produced by `step`, drained by the host.
    out: Vec<f32>,

    /// Leftover system cycles not yet consumed into a 768-cycle sample tick.
    cycle_acc: u32,
}

const REVERB_LEN: usize = 2048;

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

impl Spu {
    pub fn new() -> Self {
        Spu {
            regs: [0; SPU_REGS / 2],
            ram: vec![0u8; SOUND_RAM_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            spucnt: 0,
            transfer_addr: 0,
            main_vol_left: 0,
            main_vol_right: 0,
            kon: 0,
            koff: 0,
            pmon: 0,
            non: 0,
            eon: 0,
            endx: 0,
            transfer_base: 0,
            irq_addr: 0,
            reverb_base: 0,
            voices: [Voice::new(); VOICE_COUNT],
            noise_level: 0,
            noise_timer: 0,
            reverb_buf: vec![(0.0f32, 0.0f32); REVERB_LEN]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            reverb_pos: 0,
            out: Vec::with_capacity(4096),
            cycle_acc: 0,
        }
    }

    // ===================== register window =====================

    /// Read an SPU register. `off` is relative to the SPU window base
    /// 0x1F80_1C00. `SPUSTAT` (+0x1AE) reflects the current transfer mode.
    pub fn read(&self, off: u32) -> u32 {
        match off {
            // ---- per-voice registers (0x000..0x180) ----
            0x000..=0x17F => {
                let v = (off / 0x10) as usize;
                let r = off & 0xF;
                let vc = &self.voices[v];
                let hw = match r {
                    0x0 => vc.vol_left,
                    0x2 => vc.vol_right,
                    0x4 => vc.pitch,
                    0x6 => vc.start_addr,
                    0x8 => vc.adsr1,
                    0xA => vc.adsr2,
                    0xC => (vc.adsr_vol as u16) & 0x7FFF, // current ADSR volume
                    0xE => vc.repeat_addr,
                    _ => return self.shadow(off),
                };
                hw as u32
            }

            0x180 => self.main_vol_left as u32,
            0x182 => self.main_vol_right as u32,

            0x188 => self.kon & 0xFFFF,
            0x18A => self.kon >> 16,
            0x18C => self.koff & 0xFFFF,
            0x18E => self.koff >> 16,
            0x190 => self.pmon & 0xFFFF,
            0x192 => self.pmon >> 16,
            0x194 => self.non & 0xFFFF,
            0x196 => self.non >> 16,
            0x198 => self.eon & 0xFFFF,
            0x19A => self.eon >> 16,
            0x19C => self.endx & 0xFFFF,
            0x19E => self.endx >> 16,

            0x1A2 => self.reverb_base as u32,
            0x1A4 => self.irq_addr as u32,
            0x1A6 => self.transfer_base as u32,
            0x1AA => self.spucnt as u32,
            // SPUSTAT (+0x1AE): low 6 bits mirror SPUCNT's mode bits (psx-spx)
            // so the BIOS's "wait for transfer mode" handshake completes; the
            // transfer-busy/DMA-ready flags are reported idle (transfers here
            // complete instantly).
            0x1AE => (self.spucnt & 0x3F) as u32,

            _ => self.shadow(off),
        }
    }

    #[inline]
    fn shadow(&self, off: u32) -> u32 {
        let i = (off >> 1) as usize;
        if i < self.regs.len() {
            self.regs[i] as u32
        } else {
            0
        }
    }

    /// Write an SPU register. `off` is relative to 0x1F80_1C00.
    pub fn write(&mut self, off: u32, v: u32) {
        let v16 = v as u16;
        // Keep the flat shadow coherent for every halfword write.
        let i = (off >> 1) as usize;
        if i < self.regs.len() {
            self.regs[i] = v16;
        }

        match off {
            // ---- per-voice registers ----
            0x000..=0x17F => {
                let vi = (off / 0x10) as usize;
                let r = off & 0xF;
                let vc = &mut self.voices[vi];
                match r {
                    0x0 => vc.vol_left = v16,
                    0x2 => vc.vol_right = v16,
                    0x4 => vc.pitch = v16,
                    0x6 => vc.start_addr = v16,
                    0x8 => vc.adsr1 = v16,
                    0xA => vc.adsr2 = v16,
                    0xC => vc.adsr_vol = (v16 & 0x7FFF) as i32,
                    0xE => vc.repeat_addr = v16,
                    _ => {}
                }
            }

            0x180 => self.main_vol_left = v16,
            0x182 => self.main_vol_right = v16,

            0x188 => self.kon = (self.kon & 0xFFFF_0000) | v16 as u32,
            0x18A => self.kon = (self.kon & 0x0000_FFFF) | ((v16 as u32) << 16),
            0x18C => self.koff = (self.koff & 0xFFFF_0000) | v16 as u32,
            0x18E => self.koff = (self.koff & 0x0000_FFFF) | ((v16 as u32) << 16),
            0x190 => self.pmon = (self.pmon & 0xFFFF_0000) | v16 as u32,
            0x192 => self.pmon = (self.pmon & 0x0000_FFFF) | ((v16 as u32) << 16),
            0x194 => self.non = (self.non & 0xFFFF_0000) | v16 as u32,
            0x196 => self.non = (self.non & 0x0000_FFFF) | ((v16 as u32) << 16),
            0x198 => self.eon = (self.eon & 0xFFFF_0000) | v16 as u32,
            0x19A => self.eon = (self.eon & 0x0000_FFFF) | ((v16 as u32) << 16),
            // ENDX is read-only status; writes are acknowledged (clear) on HW.
            0x19C => self.endx &= 0xFFFF_0000 | (!v16 as u32 & 0xFFFF),
            0x19E => self.endx &= 0x0000_FFFF | (!(v16 as u32) << 16),

            0x1A2 => self.reverb_base = v16,
            0x1A4 => self.irq_addr = v16,
            0x1A6 => {
                self.transfer_base = v16;
                // Transfer Addr is in 8-byte units; latch the byte pointer.
                self.transfer_addr = (v16 as u32) << 3;
            }
            0x1A8 => {
                // Transfer FIFO: manual writes stream halfwords into sound RAM.
                self.ram_write16(self.transfer_addr, v16);
                self.transfer_addr = (self.transfer_addr + 2) & (SOUND_RAM_SIZE as u32 - 1);
            }
            0x1AA => self.spucnt = v16,

            _ => {}
        }
    }

    /// Write a halfword into sound RAM at a byte address (little-endian).
    #[inline]
    fn ram_write16(&mut self, addr: u32, v: u16) {
        let a = (addr as usize) & (SOUND_RAM_SIZE - 1);
        self.ram[a] = v as u8;
        self.ram[(a + 1) & (SOUND_RAM_SIZE - 1)] = (v >> 8) as u8;
    }

    /// Read a halfword from sound RAM at a byte address (little-endian).
    #[inline]
    fn ram_read16(&self, addr: u32) -> u16 {
        let a = (addr as usize) & (SOUND_RAM_SIZE - 1);
        (self.ram[a] as u16) | ((self.ram[(a + 1) & (SOUND_RAM_SIZE - 1)] as u16) << 8)
    }

    // ===================== DMA transfer port =====================

    /// Push one halfword through the transfer FIFO (used by SPU DMA). Mirrors
    /// the manual-write path at +0x1A8 so the DMA engine can stream blocks.
    pub fn dma_write(&mut self, v: u16) {
        self.ram_write16(self.transfer_addr, v);
        self.transfer_addr = (self.transfer_addr + 2) & (SOUND_RAM_SIZE as u32 - 1);
    }

    /// Pull one halfword from sound RAM through the transfer pointer (SPU DMA
    /// read mode).
    pub fn dma_read(&mut self) -> u16 {
        let v = self.ram_read16(self.transfer_addr);
        self.transfer_addr = (self.transfer_addr + 2) & (SOUND_RAM_SIZE as u32 - 1);
        v
    }

    // ===================== output drain =====================

    /// Drain all queued interleaved L,R f32 samples into `dst`, returning the
    /// number of *samples* (L+R count, i.e. frames*2) written, and clearing the
    /// internal ring. The host audio callback calls this each block.
    pub fn drain(&mut self, dst: &mut Vec<f32>) -> usize {
        let n = self.out.len();
        dst.extend_from_slice(&self.out);
        self.out.clear();
        n
    }

    /// Number of queued f32 samples (L+R) currently buffered.
    pub fn queued(&self) -> usize {
        self.out.len()
    }

    // ===================== tick / mixer =====================

    /// Advance the SPU by `cycles`. Produces one stereo sample per 768 cycles
    /// (44.1 kHz). Decodes ADPCM, runs ADSR, applies pitch + interpolation, and
    /// mixes all 24 voices into the output ring.
    pub fn step(&mut self, cycles: u32) {
        // Spill accumulated system cycles into whole 768-cycle sample ticks.
        self.cycle_acc = self.cycle_acc.wrapping_add(cycles);
        while self.cycle_acc >= CYCLES_PER_SAMPLE {
            self.cycle_acc -= CYCLES_PER_SAMPLE;
            self.tick_sample();
        }
    }

    /// Produce exactly one 44.1 kHz stereo sample.
    fn tick_sample(&mut self) {
        let enabled = self.spucnt & 0x8000 != 0;

        // Handle key-on / key-off requests latched since the last tick.
        let kon = self.kon;
        let koff = self.koff;
        self.kon = 0; // KON/KOFF auto-clear after being consumed (HW: write-only triggers)
        self.koff = 0;
        for v in 0..VOICE_COUNT {
            let bit = 1u32 << v;
            if kon & bit != 0 {
                self.key_on(v);
            }
            if koff & bit != 0 {
                self.key_off(v);
            }
        }

        self.step_noise();

        let mut mix_l = 0.0f32;
        let mut mix_r = 0.0f32;
        let mut reverb_l = 0.0f32;
        let mut reverb_r = 0.0f32;

        let mut prev_amp = 0i32; // for pitch modulation (voice N modulates N+1)

        for v in 0..VOICE_COUNT {
            if !self.voices[v].is_on() {
                prev_amp = 0;
                continue;
            }

            // ----- sample fetch with pitch + interpolation -----
            let noise_mode = self.non & (1 << v) != 0;
            let sample = if noise_mode {
                self.noise_level as i32
            } else {
                self.voice_sample(v)
            };

            // ----- ADSR envelope -----
            self.step_adsr(v);
            let env = self.voices[v].adsr_vol; // 0..0x7FFF

            let amp = (sample * env) >> 15; // -0x8000..0x7FFF

            // ----- per-voice volume -----
            let (vl, vr) = (self.voices[v].vol_left, self.voices[v].vol_right);
            let l = (amp * volume_level(vl)) >> 15;
            let r = (amp * volume_level(vr)) >> 15;

            let lf = l as f32 / 32768.0;
            let rf = r as f32 / 32768.0;
            mix_l += lf;
            mix_r += rf;

            if self.eon & (1 << v) != 0 {
                reverb_l += lf;
                reverb_r += rf;
            }

            // ----- pitch counter advance -----
            // Pitch modulation uses the *previous* voice's amplitude (PMON
            // modulates voice N by voice N-1); update the running value after.
            self.advance_pitch(v, prev_amp);
            prev_amp = amp;
        }

        // ----- minimal reverb -----
        let reverb_enabled = self.spucnt & 0x0080 != 0;
        if reverb_enabled {
            let (rl, rr) = self.reverb_buf[self.reverb_pos];
            // Feed input + 50% feedback of the delayed signal.
            let out_l = reverb_l + rl * 0.5;
            let out_r = reverb_r + rr * 0.5;
            self.reverb_buf[self.reverb_pos] = (out_l, out_r);
            self.reverb_pos = (self.reverb_pos + 1) % REVERB_LEN;
            mix_l += out_l * 0.25;
            mix_r += out_r * 0.25;
        }

        // ----- master volume + mute -----
        let muted = self.spucnt & 0x4000 != 0;
        let (out_l, out_r) = if enabled && !muted {
            let ml = self.main_vol_left;
            let mr = self.main_vol_right;
            (
                mix_l * (volume_level(ml) as f32 / 32768.0),
                mix_r * (volume_level(mr) as f32 / 32768.0),
            )
        } else {
            (0.0, 0.0)
        };

        self.out.push(clamp_f32(out_l));
        self.out.push(clamp_f32(out_r));
    }

    /// Fetch the current interpolated sample for voice `v` (without advancing
    /// the pitch counter).
    fn voice_sample(&mut self, v: usize) -> i32 {
        // 4-point Gaussian interpolation using the fractional part (bits 4..11).
        let i = ((self.voices[v].counter >> 4) & 0xFF) as usize;
        let interp = self.voices[v].interp;
        let g0 = GAUSS[0x0FF - i] as i32;
        let g1 = GAUSS[0x1FF - i] as i32;
        let g2 = GAUSS[0x100 + i] as i32;
        let g3 = GAUSS[0x000 + i] as i32;
        let out = (g0 * interp[0] as i32 >> 15)
            + (g1 * interp[1] as i32 >> 15)
            + (g2 * interp[2] as i32 >> 15)
            + (g3 * interp[3] as i32 >> 15);
        out.clamp(-0x8000, 0x7FFF)
    }

    /// Advance the pitch counter for voice `v`; when it crosses a sample
    /// boundary, shift the interpolation window and decode the next sample
    /// (decoding a new 16-byte block when needed).
    fn advance_pitch(&mut self, v: usize, prev_amp: i32) {
        let mut step = self.voices[v].pitch as u32;
        if self.pmon & (1 << v) != 0 && v > 0 {
            // Pitch modulation: scale step by previous voice amplitude.
            let factor = (prev_amp + 0x8000).clamp(0, 0xFFFF);
            step = ((step as i32 * factor) >> 15) as u32;
        }
        if step > 0x3FFF {
            step = 0x4000;
        }
        self.voices[v].counter = self.voices[v].counter.wrapping_add(step);

        // Each time bit 12 increments, one new sample is consumed.
        while self.voices[v].counter >= 0x1000 {
            self.voices[v].counter -= 0x1000;
            self.consume_sample(v);
        }
    }

    /// Pop one decoded sample into the interpolation history, decoding the next
    /// ADPCM block when the current one is exhausted.
    fn consume_sample(&mut self, v: usize) {
        if self.voices[v].block_offset >= 28 {
            self.decode_next_block(v);
        }
        let s = {
            let vc = &mut self.voices[v];
            let s = vc.decoded[vc.block_offset as usize];
            vc.block_offset += 1;
            s
        };
        let vc = &mut self.voices[v];
        vc.interp[3] = vc.interp[2];
        vc.interp[2] = vc.interp[1];
        vc.interp[1] = vc.interp[0];
        vc.interp[0] = s;
    }

    /// Decode the 16-byte ADPCM block at the voice's current address into its
    /// 28-sample buffer, handle loop flags, and advance/loop the address.
    fn decode_next_block(&mut self, v: usize) {
        let base = self.voices[v].cur_addr & (SOUND_RAM_SIZE as u32 - 1);
        let hdr = self.ram[base as usize];
        let flags = self.ram[((base + 1) as usize) & (SOUND_RAM_SIZE - 1)];

        let shift = (hdr & 0x0F) as u32;
        let shift = if shift > 12 { 9 } else { shift }; // HW clamps >12 to 9
        let filter = ((hdr >> 4) & 0x07).min(4) as usize;
        let f0 = ADPCM_POS[filter];
        let f1 = ADPCM_NEG[filter];

        let mut h0 = self.voices[v].hist0;
        let mut h1 = self.voices[v].hist1;

        for n in 0..28usize {
            let byte = self.ram[((base + 2 + (n / 2) as u32) as usize) & (SOUND_RAM_SIZE - 1)];
            let nibble = if n & 1 == 0 { byte & 0x0F } else { byte >> 4 };
            // Sign-extend the 4-bit nibble into the top of a 16-bit word, then
            // arithmetic-shift down by `shift`.
            let mut s = ((nibble as i32) << 12) as i16 as i32; // <<12 then sign bit at 15
            s >>= shift;
            let pred = (h0 * f0 + h1 * f1 + 32) >> 6;
            let sample = (s + pred).clamp(-0x8000, 0x7FFF);
            self.voices[v].decoded[n] = sample as i16;
            h1 = h0;
            h0 = sample;
        }

        self.voices[v].hist0 = h0;
        self.voices[v].hist1 = h1;
        self.voices[v].block_offset = 0;

        // Loop-start flag: latch this block as the loop point.
        if flags & 0x04 != 0 {
            self.voices[v].repeat_addr = (base >> 3) as u16;
        }

        // Advance to the next block, or loop / end on the loop-end flag.
        if flags & 0x01 != 0 {
            // Loop-end: set ENDX, jump to repeat address.
            self.endx |= 1 << v;
            self.voices[v].cur_addr = (self.voices[v].repeat_addr as u32) << 3;
            if flags & 0x02 == 0 {
                // End + mute (code 1): release the voice.
                self.voices[v].adsr_vol = 0;
                self.voices[v].phase = AdsrPhase::Off;
            }
        } else {
            self.voices[v].cur_addr =
                (base + 16) & (SOUND_RAM_SIZE as u32 - 1);
        }
    }

    // ===================== key on / off =====================

    fn key_on(&mut self, v: usize) {
        let vc = &mut self.voices[v];
        vc.cur_addr = (vc.start_addr as u32) << 3;
        vc.counter = 0;
        vc.block_offset = 28; // force a decode on first sample
        vc.hist0 = 0;
        vc.hist1 = 0;
        vc.interp = [0; 4];
        vc.adsr_vol = 0;
        vc.phase = AdsrPhase::Attack;
        vc.adsr_cycles = 0;
        self.endx &= !(1u32 << v);
    }

    fn key_off(&mut self, v: usize) {
        let vc = &mut self.voices[v];
        if vc.phase != AdsrPhase::Off {
            vc.phase = AdsrPhase::Release;
            vc.adsr_cycles = 0;
        }
    }

    // ===================== ADSR =====================

    /// Step a voice's ADSR envelope by one sample.
    fn step_adsr(&mut self, v: usize) {
        if self.voices[v].phase == AdsrPhase::Off {
            return;
        }
        if self.voices[v].adsr_cycles > 0 {
            self.voices[v].adsr_cycles -= 1;
            return;
        }

        let adsr1 = self.voices[v].adsr1;
        let adsr2 = self.voices[v].adsr2;
        let level = self.voices[v].adsr_vol;

        // Decode the per-phase rate (shift/step/mode/direction).
        let (shift, step_v, exp, decreasing, target, next_phase) = match self.voices[v].phase {
            AdsrPhase::Attack => {
                let exp = adsr1 & 0x8000 != 0;
                let shift = ((adsr1 >> 10) & 0x1F) as u32;
                let step_v = (7 - ((adsr1 >> 8) & 0x03)) as i32; // +7,+6,+5,+4
                (shift, step_v, exp, false, 0x7FFF, AdsrPhase::Decay)
            }
            AdsrPhase::Decay => {
                let shift = ((adsr1 >> 4) & 0x0F) as u32;
                let sustain_level = (((adsr1 & 0x0F) as i32) + 1) * 0x800;
                let sustain_level = sustain_level.min(0x7FFF);
                // Decay is always exponential, decreasing.
                (shift, 8, true, true, sustain_level, AdsrPhase::Sustain)
            }
            AdsrPhase::Sustain => {
                let exp = adsr2 & 0x8000 != 0;
                let dec = adsr2 & 0x4000 != 0;
                let shift = ((adsr2 >> 8) & 0x1F) as u32;
                let step_v = (7 - ((adsr2 >> 6) & 0x03)) as i32;
                // Sustain holds at its level (no target); never auto-advances.
                (shift, step_v, exp, dec, -1, AdsrPhase::Sustain)
            }
            AdsrPhase::Release => {
                let exp = adsr2 & 0x0020 != 0;
                let shift = ((adsr2) & 0x1F) as u32;
                (shift, 8, exp, true, 0, AdsrPhase::Off)
            }
            AdsrPhase::Off => unreachable!(),
        };

        // Rate → counter increment / cycle count (psx-spx).
        let mut adsr_step = if decreasing { -step_v } else { step_v };
        adsr_step <<= 11u32.saturating_sub(shift);
        let mut counter_inc = 0x8000i32 >> shift.saturating_sub(11);

        if exp && !decreasing && level > 0x6000 {
            // Exponential attack slows above 0x6000.
            if shift < 10 {
                adsr_step /= 4;
            } else if shift >= 11 {
                counter_inc /= 4;
            } else {
                adsr_step /= 2;
                counter_inc /= 2;
            }
        }
        if exp && decreasing {
            adsr_step = adsr_step * level / 0x8000;
        }

        let new_level = (level + adsr_step).clamp(0, 0x7FFF);
        self.voices[v].adsr_vol = new_level;
        self.voices[v].adsr_cycles = (counter_inc.max(1) as u32) >> 4; // approx cycle pacing

        // Phase transitions.
        match self.voices[v].phase {
            AdsrPhase::Attack => {
                if new_level >= 0x7FFF {
                    self.voices[v].phase = next_phase;
                    self.voices[v].adsr_cycles = 0;
                }
            }
            AdsrPhase::Decay => {
                if new_level <= target {
                    self.voices[v].phase = next_phase;
                    self.voices[v].adsr_cycles = 0;
                }
            }
            AdsrPhase::Release => {
                if new_level == 0 {
                    self.voices[v].phase = AdsrPhase::Off;
                }
            }
            AdsrPhase::Sustain | AdsrPhase::Off => {}
        }
    }

    // ===================== noise =====================

    fn step_noise(&mut self) {
        let shift = ((self.spucnt >> 10) & 0x0F) as i32;
        let step = ((self.spucnt >> 8) & 0x03) as i32 + 4;
        self.noise_timer -= step;
        if self.noise_timer < 0 {
            let l = self.noise_level as u16;
            let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
            self.noise_level = ((l << 1) | parity) as i16;
            self.noise_timer += 0x20000 >> shift;
        }
    }
}

/// Map a voice/main volume register to a linear 0..0x7FFF magnitude. Fixed
/// mode (bit15=0) uses the signed 15-bit value directly; sweep mode (bit15=1)
/// is approximated by its current full-scale level (sweeps are not animated).
#[inline]
fn volume_level(reg: u16) -> i32 {
    if reg & 0x8000 == 0 {
        // Fixed: bits 0..14 hold a signed value -0x4000..+0x3FFF; the hardware
        // volume is that value *2 (range -0x8000..+0x7FFE). Sign-extend bit 14
        // first, then double.
        let raw = ((reg & 0x7FFF) << 1) as i16 >> 1; // sign-extend 15-bit field
        ((raw as i32) << 1).clamp(-0x8000, 0x7FFF)
    } else {
        // Sweep mode: treat as full level (phase-positive) — minimal model.
        if reg & 0x1000 != 0 {
            -0x7FFF
        } else {
            0x7FFF
        }
    }
}

#[inline]
fn clamp_f32(x: f32) -> f32 {
    x.clamp(-1.0, 1.0)
}

// The 512-entry Gaussian interpolation table is large; generate a smooth
// approximation at startup-free compile time using a const block would be
// ideal, but the canonical PSX table is hardware-specific. We use a windowed
// approximation that sums to ~0x8000 across the 4 taps, which is audibly
// indistinguishable for a software core. Index 0..0x1FF.
static GAUSS: [i16; 512] = build_gauss();

const fn build_gauss() -> [i16; 512] {
    // A raised-cosine-ish kernel split into the 4-tap layout the SPU uses.
    // We approximate gauss[x] for x in 0..512 by a triangular window scaled so
    // that the 4 taps used at any fractional index sum near 0x8000.
    let mut t = [0i16; 512];
    let mut x = 0usize;
    while x < 512 {
        // Distance from center (256) in 0..256.
        let d = if x >= 256 { x - 256 } else { 256 - x };
        // Triangular falloff: peak 0x4000 at center, ~0 at the ends.
        let w = 0x4000 - (d as i32 * 0x4000 / 256);
        t[x] = w as i16;
        x += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spustat_tracks_spucnt() {
        let mut spu = Spu::new();
        spu.write(0x1AA, 0x0030); // SPUCNT mode bits
        assert_eq!(spu.read(0x1AA), 0x0030);
        assert_eq!(spu.read(0x1AE) & 0x3F, 0x30, "SPUSTAT mirrors SPUCNT mode");
    }

    #[test]
    fn voice_register_round_trips() {
        let mut spu = Spu::new();
        spu.write(0x0, 0x1234); // voice 0 volume-left
        assert_eq!(spu.read(0x0), 0x1234);
        spu.write(0x04, 0x1000); // pitch
        assert_eq!(spu.read(0x04), 0x1000);
        spu.write(0x06, 0x0040); // start addr
        assert_eq!(spu.read(0x06), 0x0040);
    }

    #[test]
    fn kon_koff_32bit_halfword_access() {
        let mut spu = Spu::new();
        spu.write(0x188, 0xBEEF);
        spu.write(0x18A, 0xDEAD);
        assert_eq!(spu.read(0x188), 0xBEEF);
        assert_eq!(spu.read(0x18A), 0xDEAD);
    }

    #[test]
    fn transfer_fifo_writes_sound_ram() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0010); // transfer addr = 0x10 * 8 = 0x80 bytes
        spu.write(0x1A8, 0xCAFE); // FIFO write
        spu.write(0x1A8, 0x1234);
        assert_eq!(spu.ram_read16(0x80), 0xCAFE);
        assert_eq!(spu.ram_read16(0x82), 0x1234);
    }

    #[test]
    fn dma_write_read_roundtrip() {
        let mut spu = Spu::new();
        spu.write(0x1A6, 0x0000);
        spu.dma_write(0x0102);
        spu.dma_write(0x0304);
        spu.write(0x1A6, 0x0000); // rewind transfer pointer
        assert_eq!(spu.dma_read(), 0x0102);
        assert_eq!(spu.dma_read(), 0x0304);
    }

    /// Build a single silent-then-ramp ADPCM block and verify it decodes.
    #[test]
    fn adpcm_decode_basic() {
        let mut spu = Spu::new();
        // Block header: shift=0, filter=0 → samples = raw nibble (sign-extended
        // and <<12 >>shift gives big values; use shift=12 for small values).
        // shift=12: s = (nibble<<12) as i16 >> 12 → nibble in -8..7.
        let base = 0usize;
        // shift=12, filter=1 (f0=60, f1=0) so each sample carries +60/64 of the
        // previous one → a slowly rising ramp instead of a flat line.
        spu.ram[base] = 0x1C; // filter=1, shift=12
        spu.ram[base + 1] = 0x00; // flags: normal
        // 28 nibbles all = 1 (decode to s=1 before prediction).
        for i in 0..14 {
            spu.ram[base + 2 + i] = 0x11; // both nibbles = 1
        }
        // Point voice 0 at this block and decode.
        spu.voices[0].cur_addr = 0;
        spu.voices[0].block_offset = 28;
        spu.decode_next_block(0);
        // First sample: s=1, no history yet → 1. Subsequent samples add the
        // filtered history so the value climbs.
        assert_eq!(spu.voices[0].decoded[0], 1);
        assert!(
            spu.voices[0].decoded[27] > spu.voices[0].decoded[0],
            "filter-1 prediction ramps via history"
        );
    }

    #[test]
    fn key_on_starts_attack_and_decodes() {
        let mut spu = Spu::new();
        // Put a loop-end block at address 0 so the voice terminates cleanly.
        spu.ram[0] = 0x0C;
        spu.ram[1] = 0x01; // loop-end + (no repeat) → end+mute
        for i in 0..14 {
            spu.ram[2 + i] = 0x11;
        }
        spu.write(0x06, 0x0000); // voice0 start addr = 0
        spu.write(0x04, 0x1000); // pitch = 44.1kHz
        spu.write(0x08, 0x80FF); // ADSR1: attack exp, fast-ish
        spu.write(0x0A, 0x0000); // ADSR2
        spu.write(0x188, 0x0001); // KON voice 0
        // Run a few samples; the voice should produce output and eventually end.
        spu.spucnt = 0x8000; // SPU enable
        for _ in 0..64 {
            spu.tick_sample();
        }
        // ENDX should be latched once the loop-end block was consumed.
        assert_ne!(spu.endx & 1, 0, "loop-end set ENDX");
    }

    #[test]
    fn step_produces_stereo_samples() {
        let mut spu = Spu::new();
        spu.spucnt = 0x8000;
        spu.step(CYCLES_PER_SAMPLE * 4);
        // 4 samples → 8 f32 values (interleaved L,R).
        assert_eq!(spu.queued(), 8);
        let mut buf = Vec::new();
        let n = spu.drain(&mut buf);
        assert_eq!(n, 8);
        assert_eq!(buf.len(), 8);
        assert_eq!(spu.queued(), 0, "drain clears the ring");
    }

    #[test]
    fn volume_fixed_mode() {
        // Fixed full-scale positive.
        assert_eq!(volume_level(0x3FFF), 0x7FFE);
        // Fixed zero.
        assert_eq!(volume_level(0x0000), 0);
    }
}
