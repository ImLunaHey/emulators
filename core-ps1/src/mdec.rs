//! MDEC — Macroblock Decoder (FMV decompressor).
//!
//! Built from psx-spx "Macroblock Decoder (MDEC)". The MDEC decompresses
//! JPEG-like macroblocks (run-level + dequantise + IDCT + YUV→RGB colour
//! conversion) used by the BIOS/games to play full-motion video. Two 32-bit
//! ports (`off` here is relative to the MDEC window base 0x1F80_1820):
//!
//! | off | write              | read               |
//! |-----|--------------------|--------------------|
//! | 0x0 | MDEC0 command/data | MDEC data out      |
//! | 0x4 | MDEC1 control      | MDEC1 status       |
//!
//! Both data paths are normally fed/drained by DMA channels 0 (MDECin, RAM→MDEC
//! command + compressed data) and 1 (MDECout, decoded pixels MDEC→RAM), but the
//! exact same words can be pushed/pulled through MDEC0 by the CPU directly. This
//! module models the full pipeline:
//!
//! * a command FSM (`Idle` → collect parameter words → process),
//! * `MDEC(2)` quant-table upload and `MDEC(3)` scale (IDCT) table upload,
//! * `MDEC(1)` macroblock decode: run-level decode → dequantise → IDCT →
//!   YUV→RGB (colour, 16×16) or Y→mono (monochrome, 8×8),
//! * a word-oriented output FIFO drained by MDEC0 reads / DMA1, packed to the
//!   command's output depth (4/8/24/15 bit).
//!
//! The decode math follows the psx-spx reference ("Old" algorithm): the
//! dequantised coefficients are stored into the natural-order block via the
//! `ZAGZIG` scan table, then the fixed-point IDCT (using the host scale table)
//! and colour conversion produce signed -128..127 samples, optionally biased to
//! unsigned by XOR-ing 0x80.

/// Number of coefficients in one 8×8 block.
const BLOCK_LEN: usize = 64;

/// End-of-block / padding marker word emitted between macroblocks.
const EOB_MARKER: u16 = 0xFE00;

/// MDEC command FSM: what the next MDEC0 write words mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for a command word.
    Idle,
    /// `MDEC(1)`: collecting `remaining` halfword-pairs of compressed block
    /// data (each MDEC0 write is two 16-bit run-level words).
    DecodeMacroblock { remaining: u32 },
    /// `MDEC(2)`: collecting the 16 (luma only) or 32 (luma+colour) words of
    /// quant-table bytes.
    LoadQuant { color: bool, remaining: u32 },
    /// `MDEC(3)`: collecting the 32 words (64 signed halfwords) of scale table.
    LoadScale { remaining: u32 },
}

/// Output colour depth (command bits 28..27 / status bits 26..25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Bit4 = 0,
    Bit8 = 1,
    Bit24 = 2,
    Bit15 = 3,
}

impl Depth {
    #[inline]
    fn from_bits(b: u32) -> Depth {
        match b & 3 {
            0 => Depth::Bit4,
            1 => Depth::Bit8,
            2 => Depth::Bit24,
            _ => Depth::Bit15,
        }
    }
    /// `true` for the colour modes (16×16 macroblock, 6 blocks per MB).
    #[inline]
    fn is_color(self) -> bool {
        matches!(self, Depth::Bit24 | Depth::Bit15)
    }
}

/// MDEC register/decoder state.
pub struct Mdec {
    /// Last command word latched on MDEC0 (exposed for the orchestrator / tests).
    pub command: u32,
    /// MDEC1 status register backing value (the live status word is composed in
    /// [`Mdec::status_word`]; this holds the persistent depth/sign/bit15 flags).
    pub status: u32,

    // ---- decoder state ----
    state: State,
    depth: Depth,
    signed: bool,
    bit15: bool,
    /// DMA0 (data-in) request enable — control bit 30.
    dma_in_enable: bool,
    /// DMA1 (data-out) request enable — control bit 29.
    dma_out_enable: bool,

    /// Luminance / monochrome quant table (64 bytes).
    quant_y: [u8; BLOCK_LEN],
    /// Colour (Cr/Cb) quant table (64 bytes).
    quant_uv: [u8; BLOCK_LEN],
    /// IDCT scale table (64 signed 14-bit-fractional halfwords).
    scale: [i16; BLOCK_LEN],

    /// Staging buffer for the current macroblock's compressed words (for the
    /// colour path we must collect 6 blocks before producing the 16×16 output).
    in_words: Vec<u16>,

    /// Decoded colour planes for the current 16×16 macroblock (colour mode).
    blk_cr: [i16; BLOCK_LEN],
    blk_cb: [i16; BLOCK_LEN],
    /// 16×16 scratch plane of 0xRRGGBB pixels for the colour path, filled per Y
    /// quadrant then packed to the output depth.
    rgb: [u32; 256],
    /// Which block of the macroblock we are decoding next (colour: Cr,Cb,Y1..Y4).
    block_index: u32,

    /// Output FIFO, already packed to the command's depth, drained word-at-a-time
    /// by MDEC0 reads / DMA1.
    out_fifo: std::collections::VecDeque<u32>,
}

impl Default for Mdec {
    fn default() -> Self {
        Mdec::new()
    }
}

impl Mdec {
    pub fn new() -> Self {
        Mdec {
            command: 0,
            // Reset status: command busy clear, FIFOs empty, depth 0, 0xFFFF
            // params-remaining. Matches the post-reset 0x8004_0000 value once the
            // dynamic bits are composed in `status_word`.
            status: 0,
            state: State::Idle,
            depth: Depth::Bit4,
            signed: false,
            bit15: false,
            dma_in_enable: false,
            dma_out_enable: false,
            quant_y: [0; BLOCK_LEN],
            quant_uv: [0; BLOCK_LEN],
            scale: [0; BLOCK_LEN],
            in_words: Vec::new(),
            blk_cr: [0; BLOCK_LEN],
            blk_cb: [0; BLOCK_LEN],
            rgb: [0; 256],
            block_index: 0,
            out_fifo: std::collections::VecDeque::new(),
        }
    }

    // ================================ I/O ================================

    /// Read an MDEC port. `off` is relative to 0x1F80_1820: 0x0 = data out FIFO,
    /// 0x4 = status. Reading 0x0 pops one word from the output FIFO.
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.out_fifo.pop_front().unwrap_or(0),
            0x4 => self.status_word(),
            _ => 0,
        }
    }

    /// Write an MDEC port. `off` is relative to 0x1F80_1820: 0x0 = command/data,
    /// 0x4 = control (bit 31 = reset).
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0 => self.write_command(v),
            0x4 => self.write_control(v),
            _ => {}
        }
    }

    /// Compose the live MDEC1 status word (psx-spx bit layout).
    fn status_word(&self) -> u32 {
        let mut s = 0u32;
        // bit31: data-out FIFO empty.
        if self.out_fifo.is_empty() {
            s |= 1 << 31;
        }
        // bit29: command busy (processing parameters / not idle).
        if self.state != State::Idle {
            s |= 1 << 29;
        }
        // bit28: data-in request (DMA0 enabled and waiting for input).
        if self.dma_in_enable && self.wants_input() {
            s |= 1 << 28;
        }
        // bit27: data-out request (DMA1 enabled and output available).
        if self.dma_out_enable && !self.out_fifo.is_empty() {
            s |= 1 << 27;
        }
        // bits26..25: output depth, bit24: signed, bit23: bit15.
        s |= (self.depth as u32) << 25;
        if self.signed {
            s |= 1 << 24;
        }
        if self.bit15 {
            s |= 1 << 23;
        }
        // bits18..16: current block (colour decode progress).
        s |= (self.block_index & 7) << 16;
        // bits15..0: remaining parameter words minus 1 (0xFFFF = none).
        s |= self.params_remaining().wrapping_sub(1) & 0xFFFF;
        s
    }

    /// Parameter words still expected for the in-flight command.
    fn params_remaining(&self) -> u32 {
        match self.state {
            State::Idle => 0,
            State::DecodeMacroblock { remaining }
            | State::LoadQuant { remaining, .. }
            | State::LoadScale { remaining } => remaining,
        }
    }

    /// `true` while the decoder is collecting input words for a command.
    fn wants_input(&self) -> bool {
        self.state != State::Idle
    }

    /// Control register write (MDEC1). bit31=reset, bit30=DMA0 enable,
    /// bit29=DMA1 enable.
    fn write_control(&mut self, v: u32) {
        if v & (1 << 31) != 0 {
            self.reset();
            return;
        }
        self.dma_in_enable = v & (1 << 30) != 0;
        self.dma_out_enable = v & (1 << 29) != 0;
    }

    /// Abort any command and return to the reset state (status = 0x8004_0000).
    fn reset(&mut self) {
        self.state = State::Idle;
        self.command = 0;
        self.status = 0;
        self.depth = Depth::Bit4;
        self.signed = false;
        self.bit15 = false;
        self.dma_in_enable = false;
        self.dma_out_enable = false;
        self.block_index = 0;
        self.in_words.clear();
        self.out_fifo.clear();
    }

    // ============================ command FSM ============================

    /// MDEC0 write — either a new command (when idle) or a parameter word.
    fn write_command(&mut self, v: u32) {
        match self.state {
            State::Idle => self.start_command(v),
            State::DecodeMacroblock { .. } => self.feed_macroblock(v),
            State::LoadQuant { .. } => self.feed_quant(v),
            State::LoadScale { .. } => self.feed_scale(v),
        }
    }

    /// Decode and begin a freshly received command word.
    fn start_command(&mut self, v: u32) {
        self.command = v;
        let cmd = (v >> 29) & 7;
        // Output format flags (apply to every command's status mirror).
        self.depth = Depth::from_bits(v >> 27);
        self.signed = v & (1 << 26) != 0;
        self.bit15 = v & (1 << 25) != 0;

        match cmd {
            1 => {
                // Decode macroblock(s): bits15..0 = number of parameter words.
                let words = v & 0xFFFF;
                self.in_words.clear();
                self.block_index = 0;
                self.state = State::DecodeMacroblock { remaining: words };
            }
            2 => {
                // Set quant table(s): bit0 = also colour table.
                let color = v & 1 != 0;
                // 64 luma bytes (= 16 words), +64 colour bytes (= 16 words).
                let remaining = if color { 32 } else { 16 };
                self.in_words.clear();
                self.state = State::LoadQuant { color, remaining };
            }
            3 => {
                // Set scale table: 64 signed halfwords = 32 words.
                self.in_words.clear();
                self.state = State::LoadScale { remaining: 32 };
            }
            // Command 0 / 4..7: no-op (treated as idle), as on hardware.
            _ => self.state = State::Idle,
        }
    }

    /// Feed one parameter word to `MDEC(2)` (quant tables).
    fn feed_quant(&mut self, v: u32) {
        let State::LoadQuant { color, remaining } = self.state else {
            return;
        };
        self.in_words.push(v as u16);
        self.in_words.push((v >> 16) as u16);
        let remaining = remaining - 1;
        if remaining == 0 {
            // Reassemble the byte tables from the collected words.
            let mut bytes = Vec::with_capacity(self.in_words.len() * 2);
            for w in &self.in_words {
                bytes.push(*w as u8);
                bytes.push((*w >> 8) as u8);
            }
            self.quant_y.copy_from_slice(&bytes[0..BLOCK_LEN]);
            if color {
                self.quant_uv
                    .copy_from_slice(&bytes[BLOCK_LEN..2 * BLOCK_LEN]);
            }
            self.in_words.clear();
            self.state = State::Idle;
        } else {
            self.state = State::LoadQuant { color, remaining };
        }
    }

    /// Feed one parameter word to `MDEC(3)` (scale / IDCT table).
    fn feed_scale(&mut self, v: u32) {
        let State::LoadScale { remaining } = self.state else {
            return;
        };
        self.in_words.push(v as u16);
        self.in_words.push((v >> 16) as u16);
        let remaining = remaining - 1;
        if remaining == 0 {
            for (i, w) in self.in_words.iter().enumerate() {
                self.scale[i] = *w as i16;
            }
            self.in_words.clear();
            self.state = State::Idle;
        } else {
            self.state = State::LoadScale { remaining };
        }
    }

    /// Feed one parameter word to `MDEC(1)` (compressed macroblock data).
    fn feed_macroblock(&mut self, v: u32) {
        let State::DecodeMacroblock { remaining } = self.state else {
            return;
        };
        self.in_words.push(v as u16);
        self.in_words.push((v >> 16) as u16);
        let remaining = remaining.saturating_sub(1);

        // Each block ends at an EOB marker; decode greedily so the staging buffer
        // never grows beyond the data of blocks not yet consumed.
        self.try_consume_blocks();

        if remaining == 0 {
            // Flush any trailing block, then return to idle.
            self.try_consume_blocks();
            self.in_words.clear();
            self.block_index = 0;
            self.state = State::Idle;
        } else {
            self.state = State::DecodeMacroblock { remaining };
        }
    }

    /// Consume complete blocks (terminated by an EOB marker) from `in_words`.
    fn try_consume_blocks(&mut self) {
        loop {
            // Find the end of the next block (first EOB marker after coefficients).
            let Some(end) = self.find_block_end() else {
                return;
            };
            // Drain the block words (including the trailing EOB marker).
            let block: Vec<u16> = self.in_words.drain(0..=end).collect();
            self.decode_block(&block);
        }
    }

    /// Index of the EOB marker that closes the first staged block, or `None` if
    /// the block is not fully buffered yet. The first word carries q_scale+DC, so
    /// a leading EOB marker (no coefficients) is skipped.
    fn find_block_end(&self) -> Option<usize> {
        if self.in_words.is_empty() {
            return None;
        }
        // A block must have at least a header word before its EOB.
        for (i, &w) in self.in_words.iter().enumerate() {
            if w == EOB_MARKER && i > 0 {
                return Some(i);
            }
        }
        None
    }

    // ============================ decode pipeline ============================

    /// Decode one 8×8 block (run-level → dequantise → IDCT) and, depending on the
    /// output depth, emit a monochrome block or stage a colour plane and emit the
    /// completed 16×16 macroblock.
    fn decode_block(&mut self, words: &[u16]) {
        if self.depth.is_color() {
            // Colour macroblock order: Cr, Cb, Y1, Y2, Y3, Y4.
            match self.block_index {
                0 => {
                    let mut blk = self.rl_decode(words, &self.quant_uv.clone());
                    self.idct(&mut blk);
                    self.blk_cr = blk;
                }
                1 => {
                    let mut blk = self.rl_decode(words, &self.quant_uv.clone());
                    self.idct(&mut blk);
                    self.blk_cb = blk;
                }
                2..=5 => {
                    let mut y = self.rl_decode(words, &self.quant_y.clone());
                    self.idct(&mut y);
                    // Y1..Y4 occupy the four 8×8 quadrants of the 16×16 output.
                    let (xx, yy) = match self.block_index {
                        2 => (0u32, 0u32),
                        3 => (8, 0),
                        4 => (0, 8),
                        _ => (8, 8),
                    };
                    self.yuv_to_rgb(&y, xx, yy);
                }
                _ => {}
            }
            self.block_index += 1;
            if self.block_index >= 6 {
                self.block_index = 0;
                self.flush_color_macroblock();
            }
        } else {
            // Monochrome: one 8×8 luminance block per call.
            let mut y = self.rl_decode(words, &self.quant_y.clone());
            self.idct(&mut y);
            self.flush_mono_block(&y);
        }
    }

    /// Run-level decode + dequantise into a natural-order 8×8 block (psx-spx
    /// `rl_decode_block`, "Old" algorithm). `words` is one block including its
    /// trailing EOB marker.
    fn rl_decode(&self, words: &[u16], qt: &[u8; BLOCK_LEN]) -> [i16; BLOCK_LEN] {
        let mut blk = [0i16; BLOCK_LEN];
        if words.is_empty() {
            return blk;
        }
        // First word: q_scale (bits 15..10) + DC value (bits 9..0).
        let n0 = words[0];
        let q_scale = ((n0 >> 10) & 0x3F) as i32;
        let mut k: usize = 0;
        // DC coefficient: dc * qt[0]  (q_scale==0 ⇒ dc*2, no quant).
        let dc = sign_extend_10(n0 & 0x3FF);
        let mut val = if q_scale == 0 {
            dc * 2
        } else {
            dc * qt[0] as i32
        };

        let mut wi = 1usize; // next coefficient word index
        loop {
            let val_c = val.clamp(-0x400, 0x3FF) as i16;
            if q_scale == 0 {
                // Linear store (no zigzag) when quantisation is bypassed.
                if k < BLOCK_LEN {
                    blk[k] = val_c;
                }
            } else if k < BLOCK_LEN {
                blk[ZAGZIG[k] as usize] = val_c;
            }

            // Fetch the next run-level word.
            let Some(&n) = words.get(wi) else { break };
            wi += 1;
            if n == EOB_MARKER {
                break;
            }
            let run = ((n >> 10) & 0x3F) as usize;
            let level = sign_extend_10(n & 0x3FF);
            k = k + run + 1;
            if k > 63 {
                break;
            }
            // AC coefficient: (level * qt[k] * q_scale + 4) / 8.
            val = if q_scale == 0 {
                level * 2
            } else {
                (level * qt[k] as i32 * q_scale + 4) / 8
            };
        }
        blk
    }

    /// Fixed-point IDCT (psx-spx `idct_core`, "Old" algorithm): two passes of an
    /// 8×8 matrix multiply against the host scale table, rounding to signed
    /// -128..127 samples. Operates in place on a natural-order block.
    fn idct(&self, blk: &mut [i16; BLOCK_LEN]) {
        let mut tmp = [0i64; BLOCK_LEN];
        // Pass 1: columns of `blk` → rows of `tmp` using scale[x*8+u].
        for x in 0..8 {
            for y in 0..8 {
                let mut sum: i64 = 0;
                for u in 0..8 {
                    sum += blk[u * 8 + x] as i64 * self.scale[y * 8 + u] as i64;
                }
                tmp[x * 8 + y] = sum;
            }
        }
        // Pass 2: tmp → blk, with the >>32 rounding and saturation.
        for x in 0..8 {
            for y in 0..8 {
                let mut sum: i64 = 0;
                for u in 0..8 {
                    sum += (tmp[u * 8 + x] >> 8) * self.scale[y * 8 + u] as i64;
                }
                let rounded = (sum >> 24) + ((sum >> 23) & 1);
                blk[x * 8 + y] = rounded.clamp(-128, 127) as i16;
            }
        }
    }

    // ============================ colour conversion ============================

    /// YUV→RGB for one 8×8 luminance block written into the (`xx`,`yy`) quadrant
    /// of the 16×16 output, using the current Cr/Cb planes (psx-spx
    /// `yuv_to_rgb`). Results are stored in [`Self::rgb`] as 0xRRGGBB.
    fn yuv_to_rgb(&mut self, y_blk: &[i16; BLOCK_LEN], xx: u32, yy: u32) {
        for y in 0..8u32 {
            for x in 0..8u32 {
                // Cr/Cb are subsampled 2:1 over the 16×16 macroblock.
                let cx = ((x + xx) / 2) as usize;
                let cy = ((y + yy) / 2) as usize;
                let cr = self.blk_cr[cx + cy * 8] as f32;
                let cb = self.blk_cb[cx + cy * 8] as f32;
                let g_off = -0.3437 * cb - 0.7143 * cr;
                let r_off = 1.402 * cr;
                let b_off = 1.772 * cb;
                let yv = y_blk[(x + y * 8) as usize] as f32;
                let mut r = clamp_i8((yv + r_off).round() as i32);
                let mut g = clamp_i8((yv + g_off).round() as i32);
                let mut b = clamp_i8((yv + b_off).round() as i32);
                if !self.signed {
                    r ^= 0x80;
                    g ^= 0x80;
                    b ^= 0x80;
                }
                let dst = ((x + xx) + (y + yy) * 16) as usize;
                self.rgb[dst] = ((r as u32 & 0xFF) << 16)
                    | ((g as u32 & 0xFF) << 8)
                    | (b as u32 & 0xFF);
            }
        }
    }

    /// Emit the staged 16×16 colour macroblock to the output FIFO, packed to the
    /// command's depth (24bpp or 15bpp).
    fn flush_color_macroblock(&mut self) {
        match self.depth {
            Depth::Bit24 => self.pack_24(),
            Depth::Bit15 => self.pack_15(),
            _ => {}
        }
    }

    /// Emit a monochrome 8×8 block (psx-spx `y_to_mono`) packed to 4/8 bit depth.
    fn flush_mono_block(&mut self, y_blk: &[i16; BLOCK_LEN]) {
        let mut px = [0u8; BLOCK_LEN];
        for (i, &y) in y_blk.iter().enumerate() {
            let mut v = clamp_i8((y & 0x1FF) as i16 as i32);
            if !self.signed {
                v ^= 0x80;
            }
            px[i] = v as u8;
        }
        match self.depth {
            Depth::Bit8 => {
                // Four 8-bit samples per word.
                for chunk in px.chunks(4) {
                    let w = chunk[0] as u32
                        | (chunk[1] as u32) << 8
                        | (chunk[2] as u32) << 16
                        | (chunk[3] as u32) << 24;
                    self.out_fifo.push_back(w);
                }
            }
            Depth::Bit4 => {
                // Eight 4-bit nibbles per word (top nibble of each byte dropped).
                for chunk in px.chunks(8) {
                    let mut w = 0u32;
                    for (j, &p) in chunk.iter().enumerate() {
                        w |= ((p >> 4) as u32 & 0xF) << (j * 4);
                    }
                    self.out_fifo.push_back(w);
                }
            }
            _ => {}
        }
    }

    /// Pack the 16×16 0xRRGGBB plane as tightly-packed 24bpp (3 bytes/pixel).
    fn pack_24(&mut self) {
        let mut bytes = Vec::with_capacity(256 * 3);
        for &rgb in self.rgb.iter() {
            bytes.push((rgb >> 16) as u8); // R
            bytes.push((rgb >> 8) as u8); // G
            bytes.push(rgb as u8); // B
        }
        for chunk in bytes.chunks(4) {
            let mut w = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                w |= (b as u32) << (i * 8);
            }
            self.out_fifo.push_back(w);
        }
    }

    /// Pack the 16×16 0xRRGGBB plane as 15bpp (two BGR555 pixels per word).
    fn pack_15(&mut self) {
        let mb = self.bit15;
        let mut halfwords = Vec::with_capacity(256);
        for &rgb in self.rgb.iter() {
            let r = ((rgb >> 16) & 0xFF) as u16 >> 3;
            let g = ((rgb >> 8) & 0xFF) as u16 >> 3;
            let b = (rgb & 0xFF) as u16 >> 3;
            let mut p = r | (g << 5) | (b << 10);
            if mb {
                p |= 0x8000;
            }
            halfwords.push(p);
        }
        for chunk in halfwords.chunks(2) {
            let lo = chunk[0] as u32;
            let hi = *chunk.get(1).unwrap_or(&0) as u32;
            self.out_fifo.push_back(lo | (hi << 16));
        }
    }
}

/// Sign-extend a 10-bit value to i32.
#[inline]
fn sign_extend_10(v: u16) -> i32 {
    let v = (v & 0x3FF) as i32;
    if v & 0x200 != 0 {
        v - 0x400
    } else {
        v
    }
}

/// Clamp to the signed 8-bit range -128..127.
#[inline]
fn clamp_i8(v: i32) -> i32 {
    v.clamp(-128, 127)
}

/// Natural-order destination for the k-th run-level coefficient (psx-spx
/// `zagzig`): coefficients arrive in zig-zag order and are scattered back into
/// the 8×8 grid through this table.
#[rustfmt::skip]
const ZAGZIG: [u8; BLOCK_LEN] = [
    0,  1,  5,  6,  14, 15, 27, 28,
    2,  4,  7,  13, 16, 26, 29, 42,
    3,  8,  12, 17, 25, 30, 41, 43,
    9,  11, 18, 24, 31, 40, 44, 53,
    10, 19, 23, 32, 39, 45, 52, 54,
    20, 22, 33, 38, 46, 51, 55, 60,
    21, 34, 37, 47, 50, 56, 59, 61,
    35, 36, 48, 49, 57, 58, 62, 63,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_idle_ready() {
        let mut mdec = Mdec::new();
        let s = mdec.read(0x4);
        assert_ne!(s & (1 << 31), 0, "data-out FIFO empty");
        assert_eq!(s & (1 << 29), 0, "command not busy when idle");
        assert_eq!(s & 0xFFFF, 0xFFFF, "no params remaining");
    }

    #[test]
    fn reset_clears_command_and_state() {
        let mut mdec = Mdec::new();
        mdec.write(0x0, 0x6000_0010); // start a decode command
        assert_ne!(mdec.command, 0);
        mdec.write(0x4, 0x8000_0000); // reset
        assert_eq!(mdec.command, 0);
        assert_eq!(mdec.read(0x4) & (1 << 29), 0, "back to idle");
    }

    #[test]
    fn control_enables_dma_request_bits() {
        let mut mdec = Mdec::new();
        // Begin a decode so the data-in path actually wants input.
        mdec.write(0x0, (1 << 29) | 4); // cmd1, 4 param words
        mdec.write(0x4, (1 << 30) | (1 << 29)); // enable DMA0+DMA1
        let s = mdec.read(0x4);
        assert_ne!(s & (1 << 28), 0, "data-in request set");
    }

    #[test]
    fn command_decode_sets_busy_and_param_count() {
        let mut mdec = Mdec::new();
        // MDEC(1), 24bpp (depth=2), 8 parameter words.
        let cmd = (1 << 29) | (2 << 27) | 8;
        mdec.write(0x0, cmd);
        let s = mdec.read(0x4);
        assert_ne!(s & (1 << 29), 0, "command busy");
        // depth bits (26..25) reflect 24bpp.
        assert_eq!((s >> 25) & 3, 2);
        // params-remaining minus 1 = 7.
        assert_eq!(s & 0xFFFF, 7);
    }

    #[test]
    fn quant_table_upload_returns_to_idle() {
        let mut mdec = Mdec::new();
        // MDEC(2), luma only → 16 words.
        mdec.write(0x0, 2 << 29);
        for i in 0..16u32 {
            assert_ne!(mdec.read(0x4) & (1 << 29), 0, "busy while loading");
            mdec.write(0x0, 0x0101_0101 * (i + 1));
        }
        assert_eq!(mdec.read(0x4) & (1 << 29), 0, "idle after 16 words");
        // First two bytes of the luma table came from the first word.
        assert_eq!(mdec.quant_y[0], 0x01);
        assert_eq!(mdec.quant_y[1], 0x01);
    }

    #[test]
    fn scale_table_upload_stores_signed_halfwords() {
        let mut mdec = Mdec::new();
        mdec.write(0x0, 3 << 29); // MDEC(3)
        // Feed 0xFFFF_0001 patterns: low half 0x0001 (=1), high half 0xFFFF (=-1).
        for _ in 0..32 {
            mdec.write(0x0, 0xFFFF_0001);
        }
        assert_eq!(mdec.read(0x4) & (1 << 29), 0, "idle after scale upload");
        assert_eq!(mdec.scale[0], 1);
        assert_eq!(mdec.scale[1], -1);
    }

    #[test]
    fn zagzig_table_is_a_permutation() {
        let mut seen = [false; BLOCK_LEN];
        for &z in ZAGZIG.iter() {
            assert!(!seen[z as usize], "duplicate index {z}");
            seen[z as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "covers all 64 positions");
    }

    #[test]
    fn sign_extend_10_handles_negatives() {
        assert_eq!(sign_extend_10(0x001), 1);
        assert_eq!(sign_extend_10(0x3FF), -1);
        assert_eq!(sign_extend_10(0x200), -512);
        assert_eq!(sign_extend_10(0x1FF), 511);
    }

    #[test]
    fn mono_dc_only_block_decodes_to_flat_output() {
        let mut mdec = Mdec::new();
        // Identity-ish scale so DC propagates; set quant[0]=1, scale = identity
        // diagonal won't be flat, so instead test the run-level/IDCT plumbing by
        // checking the output FIFO fills with the right number of words.
        // Upload a quant table of all 1s (luma).
        mdec.write(0x0, 2 << 29);
        for _ in 0..16 {
            mdec.write(0x0, 0x0101_0101);
        }
        // Upload a scale table of all zero (IDCT → 0 everywhere) just to drive the
        // pipeline; output should be 16 words of 8bpp (64 px / 4 per word).
        mdec.write(0x0, 3 << 29);
        for _ in 0..32 {
            mdec.write(0x0, 0);
        }
        // MDEC(1), 8bpp (depth=1), unsigned, 1 parameter word (DC + EOB).
        let cmd = (1 << 29) | (1 << 27) | 1;
        mdec.write(0x0, cmd);
        // One word: header (q_scale=1, dc=0) low, EOB high.
        let header = 1u32 << 10; // q_scale=1, dc=0
        let eob = EOB_MARKER as u32;
        mdec.write(0x0, header | (eob << 16));
        // 64 px at 8bpp → 16 output words.
        let mut count = 0;
        while mdec.read(0x4) & (1 << 31) == 0 {
            let _ = mdec.read(0x0);
            count += 1;
            if count > 100 {
                break;
            }
        }
        assert_eq!(count, 16, "8bpp mono block = 16 output words");
    }
}
