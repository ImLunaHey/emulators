//! TIA — Television Interface Adaptor. Video + audio, and the heart of the
//! 2600.
//!
//! Spec: Stella Programmer's Guide (the canonical TIA reference) + the AtariAge
//! TIA hardware notes. The TIA has no frame buffer: it generates one composite
//! video pixel ("colour clock") at a time, and the 6507 races alongside it
//! (1 CPU cycle = 3 colour clocks), rewriting the object registers between and
//! within scanlines. This module models the beam one colour clock at a time and
//! composites the five movable objects + playfield in hardware priority order.
//!
//! Geometry (NTSC):
//!   - 228 colour clocks per scanline: 68 of horizontal blank, then 160 visible
//!     pixels (the rendered width).
//!   - 262 scanlines per frame: programs assert VSYNC for the first 3, then a
//!     VBLANK region, ~192 visible lines, and an overscan region. We render a
//!     fixed [`VISIBLE_LINES`]-line window and let the program place its picture
//!     within it by toggling VBLANK.
//!
//! Objects, in TIA priority order (when PF/BL priority bit is clear):
//!   P0/M0 > P1/M1 > PF/BL > background. PF/BL can be promoted above the players
//!   by CTRLPF bit 2 (PFP). Each object owns a horizontal position counter that
//!   wraps modulo 160; the RESPx strobes reset it to "the current beam
//!   position", and HMOVE applies a signed fine offset from the HMxx registers.
//!
//! Implemented: PF0/PF1/PF2 (reflect + score), P0/P1 with NUSIZ copies + size,
//! M0/M1, BL, RESPx positioning, HMOVE fine motion + the left-edge blanking
//! "comb", per-object reflect (REFPx), the colour/luma registers → NTSC RGBA,
//! the 15 collision latches (CXxxxx), VSYNC/VBLANK, WSYNC, and the 2-channel
//! AUDxx audio.

mod audio;
pub use audio::SAMPLE_RATE;
use audio::AudioChannel;

/// Visible pixels per scanline (the rendered framebuffer width).
pub const VISIBLE_W: usize = 160;
/// Colour clocks of horizontal blank before the visible region.
const HBLANK: u16 = 68;
/// Total colour clocks per scanline.
const CLOCKS_PER_LINE: u16 = 228;
/// Rendered scanlines (the visible window we expose as the framebuffer height).
pub const VISIBLE_LINES: usize = 192;
/// Total scanlines per NTSC frame.
const LINES_PER_FRAME: u16 = 262;
/// Scanline at which the visible window starts (after VSYNC + part of VBLANK).
/// Most NTSC games place their picture around line 40; rendering from 40 lets
/// a 192-line window capture the playfield.
const VISIBLE_START_LINE: u16 = 40;

/// Framebuffer length in bytes (RGBA8888).
pub const FB_LEN: usize = VISIBLE_W * VISIBLE_LINES * 4;

// ---- TIA write registers (addresses are the low 6 bits the bus hands us) ----
const VSYNC: u16 = 0x00;
const VBLANK: u16 = 0x01;
const WSYNC: u16 = 0x02;
const RSYNC: u16 = 0x03;
const NUSIZ0: u16 = 0x04;
const NUSIZ1: u16 = 0x05;
const COLUP0: u16 = 0x06;
const COLUP1: u16 = 0x07;
const COLUPF: u16 = 0x08;
const COLUBK: u16 = 0x09;
const CTRLPF: u16 = 0x0A;
const REFP0: u16 = 0x0B;
const REFP1: u16 = 0x0C;
const PF0: u16 = 0x0D;
const PF1: u16 = 0x0E;
const PF2: u16 = 0x0F;
const RESP0: u16 = 0x10;
const RESP1: u16 = 0x11;
const RESM0: u16 = 0x12;
const RESM1: u16 = 0x13;
const RESBL: u16 = 0x14;
const AUDC0: u16 = 0x15;
const AUDC1: u16 = 0x16;
const AUDF0: u16 = 0x17;
const AUDF1: u16 = 0x18;
const AUDV0: u16 = 0x19;
const AUDV1: u16 = 0x1A;
const GRP0: u16 = 0x1B;
const GRP1: u16 = 0x1C;
const ENAM0: u16 = 0x1D;
const ENAM1: u16 = 0x1E;
const ENABL: u16 = 0x1F;
const HMP0: u16 = 0x20;
const HMP1: u16 = 0x21;
const HMM0: u16 = 0x22;
const HMM1: u16 = 0x23;
const HMBL: u16 = 0x24;
const VDELP0: u16 = 0x25;
const VDELP1: u16 = 0x26;
const VDELBL: u16 = 0x27;
const RESMP0: u16 = 0x28;
const RESMP1: u16 = 0x29;
const HMOVE: u16 = 0x2A;
const HMCLR: u16 = 0x2B;
const CXCLR: u16 = 0x2C;

// ---- TIA read registers (collision + input latches) ----
const CXM0P: u16 = 0x00;
const CXM1P: u16 = 0x01;
const CXP0FB: u16 = 0x02;
const CXP1FB: u16 = 0x03;
const CXM0FB: u16 = 0x04;
const CXM1FB: u16 = 0x05;
const CXBLPF: u16 = 0x06;
const CXPPMM: u16 = 0x07;
const INPT4: u16 = 0x0C;
const INPT5: u16 = 0x0D;

/// One movable object's horizontal counter + graphics state. Players, missiles,
/// and the ball all share the same positional machinery.
#[derive(Clone, Copy)]
struct Player {
    /// Position counter (0..159). The object draws when this matches a copy
    /// start point given by NUSIZ.
    pos: u16,
    /// Graphics byte (GRPx). Bit 7 is the leftmost pixel.
    grp: u8,
    /// Delayed (VDEL) graphics byte.
    grp_old: u8,
    vdel: bool,
    /// Reflect the 8-pixel sprite horizontally (REFPx).
    reflect: bool,
    /// NUSIZ low 3 bits: number/size of copies.
    nusiz: u8,
    /// Fine-motion nibble (HMPx), stored as the raw 4-bit value.
    hm: u8,
    color: u8,
}

impl Player {
    fn new() -> Player {
        Player {
            pos: 0,
            grp: 0,
            grp_old: 0,
            vdel: false,
            reflect: false,
            nusiz: 0,
            hm: 0,
            color: 0,
        }
    }
    fn gfx(&self) -> u8 {
        if self.vdel {
            self.grp_old
        } else {
            self.grp
        }
    }
}

/// Missile / ball: a 1-bit object with a size and an enable.
#[derive(Clone, Copy)]
struct Mball {
    pos: u16,
    enabled: bool,
    enabled_old: bool,
    vdel: bool,
    /// Width in pixels: 1/2/4/8 (decoded from NUSIZ bits 4-5 or CTRLPF bits 4-5).
    size: u8,
    hm: u8,
    color: u8,
}

impl Mball {
    fn new() -> Mball {
        Mball {
            pos: 0,
            enabled: false,
            enabled_old: false,
            vdel: false,
            size: 1,
            hm: 0,
            color: 0,
        }
    }
    fn ena(&self) -> bool {
        if self.vdel {
            self.enabled_old
        } else {
            self.enabled
        }
    }
}

pub struct Tia {
    /// RGBA8888 visible framebuffer.
    pub framebuffer: Box<[u8; FB_LEN]>,

    /// Current beam position within the scanline (0..227).
    clock: u16,
    /// Current scanline (0..261).
    line: u16,
    /// Completed-frame counter.
    pub frame: u64,
    /// Set true for one `run_frame` boundary when a full frame finishes.
    pub frame_done: bool,

    /// WSYNC latch: when set, the CPU is stalled until the next scanline start.
    pub wsync: bool,

    // ---- video control ----
    vsync: bool,
    vblank: bool,
    /// Background colour (COLUBK).
    colubk: u8,
    /// Playfield colour (COLUPF).
    colupf: u8,
    /// CTRLPF: bit0 reflect, bit1 score, bit2 PF/BL priority, bits4-5 ball size.
    ctrlpf: u8,
    /// 20-bit playfield assembled from PF0/PF1/PF2.
    pf: u32,
    pf0: u8,
    pf1: u8,
    pf2: u8,

    p0: Player,
    p1: Player,
    m0: Mball,
    m1: Mball,
    bl: Mball,

    /// HMOVE was strobed this line: blank the first 8 visible pixels (the comb).
    hmove_applied: bool,

    /// Collision latches: one byte each, bits 6/7 used per the read map.
    cx: [u8; 8],

    /// Input port latches for the fire buttons (INPT4/INPT5). Bit 7 = released.
    inpt4: u8,
    inpt5: u8,

    audio: [AudioChannel; 2],
    sample_accum: u32,
    samples: Vec<f32>,
}

impl Default for Tia {
    fn default() -> Self {
        Tia::new()
    }
}

impl Tia {
    pub fn new() -> Tia {
        Tia {
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
            clock: 0,
            line: 0,
            frame: 0,
            frame_done: false,
            wsync: false,
            vsync: false,
            vblank: false,
            colubk: 0,
            colupf: 0,
            ctrlpf: 0,
            pf: 0,
            pf0: 0,
            pf1: 0,
            pf2: 0,
            p0: Player::new(),
            p1: Player::new(),
            m0: Mball::new(),
            m1: Mball::new(),
            bl: Mball::new(),
            hmove_applied: false,
            cx: [0; 8],
            inpt4: 0x80,
            inpt5: 0x80,
            audio: [AudioChannel::new(), AudioChannel::new()],
            sample_accum: 0,
            samples: Vec::new(),
        }
    }

    /// Set the fire-button latches (bit 7 = 1 means released).
    pub fn set_fire(&mut self, p0_pressed: bool, p1_pressed: bool) {
        self.inpt4 = if p0_pressed { 0x00 } else { 0x80 };
        self.inpt5 = if p1_pressed { 0x00 } else { 0x80 };
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer[..]
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }

    /// Advance the TIA by one colour clock: composite the current pixel (if
    /// visible) and step the beam. Audio is clocked at the colour-clock rate /
    /// 114 (≈ the 30 KHz AUD divider) folded into the sampler.
    pub fn tick(&mut self) {
        // Audio runs off the horizontal-sync rate (~31.4 KHz): clock both
        // channels twice per scanline. We approximate by clocking every 114
        // colour clocks.
        if self.clock == 0 || self.clock == 114 {
            self.audio[0].clock();
            self.audio[1].clock();
        }
        self.sample_one();

        // Composite the visible pixel.
        if self.clock >= HBLANK {
            let px = self.clock - HBLANK; // 0..159
            self.render_pixel(px);
        }

        // Advance the beam.
        self.clock += 1;
        if self.clock >= CLOCKS_PER_LINE {
            self.clock = 0;
            self.hmove_applied = false;
            self.wsync = false; // WSYNC releases at the start of a new line
            self.line += 1;
            if self.line >= LINES_PER_FRAME {
                self.line = 0;
                self.frame += 1;
                self.frame_done = true;
            }
        }
    }

    /// Generate one audio sample when enough colour clocks have elapsed for the
    /// host sample rate.
    fn sample_one(&mut self) {
        // Colour-clock rate ≈ 3.58 MHz. Accumulate and emit at SAMPLE_RATE.
        self.sample_accum += SAMPLE_RATE;
        if self.sample_accum >= 3_579_545 {
            self.sample_accum -= 3_579_545;
            let s = self.audio[0].output() + self.audio[1].output();
            self.samples.push(s);
        }
    }

    /// Composite + write one visible pixel `px` (0..159) on the current line.
    fn render_pixel(&mut self, px: u16) {
        let line = self.line;
        // Only store pixels that fall inside our exposed window.
        let in_window = line >= VISIBLE_START_LINE
            && (line - VISIBLE_START_LINE) < VISIBLE_LINES as u16
            && (px as usize) < VISIBLE_W;

        // During VBLANK the beam outputs black (blanked).
        if self.vblank {
            if in_window {
                self.put(px, line, 0);
            }
            return;
        }

        // HMOVE comb: the first 8 visible pixels are blanked to background after
        // an HMOVE strobe on this line.
        let comb = self.hmove_applied && px < 8;

        // Evaluate each object's pixel presence.
        let pf_on = self.playfield_pixel(px);
        let bl_on = !comb && self.ball_pixel(px);
        let p0_on = !comb && self.player_pixel(&self.p0, px);
        let p1_on = !comb && self.player_pixel(&self.p1, px);
        let m0_on = !comb && self.missile_pixel(&self.m0, px);
        let m1_on = !comb && self.missile_pixel(&self.m1, px);

        self.update_collisions(p0_on, p1_on, m0_on, m1_on, bl_on, pf_on);

        // Playfield colour: in score mode the left half uses P0 colour and the
        // right half P1 colour.
        let score = self.ctrlpf & 0x02 != 0;
        let pf_color = if score {
            if px < 80 {
                self.p0.color
            } else {
                self.p1.color
            }
        } else {
            self.colupf
        };
        let pf_priority = self.ctrlpf & 0x04 != 0;

        // Priority resolution.
        let color = if pf_priority {
            // PF/BL above players.
            if pf_on {
                pf_color
            } else if bl_on {
                self.bl.color
            } else if p0_on || m0_on {
                self.p0.color
            } else if p1_on || m1_on {
                self.p1.color
            } else {
                self.colubk
            }
        } else if p0_on || m0_on {
            self.p0.color
        } else if p1_on || m1_on {
            self.p1.color
        } else if pf_on {
            pf_color
        } else if bl_on {
            self.bl.color
        } else {
            self.colubk
        };

        if in_window {
            self.put(px, line, color);
        }
    }

    fn put(&mut self, px: u16, line: u16, color: u8) {
        let y = (line - VISIBLE_START_LINE) as usize;
        let x = px as usize;
        let i = (y * VISIBLE_W + x) * 4;
        let rgb = ntsc_rgb(color);
        self.framebuffer[i] = rgb[0];
        self.framebuffer[i + 1] = rgb[1];
        self.framebuffer[i + 2] = rgb[2];
        self.framebuffer[i + 3] = 0xFF;
    }

    /// Is the playfield lit at visible pixel `px`?
    fn playfield_pixel(&self, px: u16) -> bool {
        // The playfield is 20 bits across the 160-pixel line, each bit 4 pixels
        // wide. The left half is PF bit order 0..19; the right half repeats or
        // reflects per CTRLPF bit 0.
        let reflect = self.ctrlpf & 0x01 != 0;
        let half = px / 80; // 0 = left, 1 = right
        let local = (px % 80) / 4; // 0..19 within a half
        let bit = if half == 0 {
            local
        } else if reflect {
            19 - local
        } else {
            local
        };
        self.pf & (1 << bit) != 0
    }

    fn ball_pixel(&self, px: u16) -> bool {
        if !self.bl.ena() {
            return false;
        }
        let size = self.bl.size as u16;
        let start = self.bl.pos;
        in_span(px, start, size)
    }

    fn missile_pixel(&self, m: &Mball, px: u16) -> bool {
        if !m.enabled {
            return false;
        }
        let size = m.size as u16;
        let start = m.pos;
        in_span(px, start, size)
    }

    /// Is player `p` lit at pixel `px`? Handles NUSIZ copies, size, and reflect.
    fn player_pixel(&self, p: &Player, px: u16) -> bool {
        let gfx = p.gfx();
        if gfx == 0 {
            return false;
        }
        let (copies, spacing, width) = nusiz_player(p.nusiz);
        for c in 0..copies {
            let start = (p.pos + c as u16 * spacing) % 160;
            // Each player pixel is `width` colour clocks wide (1, 2, or 4).
            // Wrap-around handling for objects near the right edge.
            let rel = if px >= start { px - start } else { 160 + px - start };
            if rel < 8 * width {
                let bit_index = rel / width; // 0..7
                let bit = if p.reflect {
                    bit_index
                } else {
                    7 - bit_index
                };
                if gfx & (1 << bit) != 0 {
                    return true;
                }
            }
        }
        false
    }

    fn update_collisions(
        &mut self,
        p0: bool,
        p1: bool,
        m0: bool,
        m1: bool,
        bl: bool,
        pf: bool,
    ) {
        // Each CXxxxx register reports two collision pairs in bits 6 and 7.
        if m0 && p1 {
            self.cx[CXM0P as usize] |= 0x80;
        }
        if m0 && p0 {
            self.cx[CXM0P as usize] |= 0x40;
        }
        if m1 && p0 {
            self.cx[CXM1P as usize] |= 0x80;
        }
        if m1 && p1 {
            self.cx[CXM1P as usize] |= 0x40;
        }
        if p0 && pf {
            self.cx[CXP0FB as usize] |= 0x80;
        }
        if p0 && bl {
            self.cx[CXP0FB as usize] |= 0x40;
        }
        if p1 && pf {
            self.cx[CXP1FB as usize] |= 0x80;
        }
        if p1 && bl {
            self.cx[CXP1FB as usize] |= 0x40;
        }
        if m0 && pf {
            self.cx[CXM0FB as usize] |= 0x80;
        }
        if m0 && bl {
            self.cx[CXM0FB as usize] |= 0x40;
        }
        if m1 && pf {
            self.cx[CXM1FB as usize] |= 0x80;
        }
        if m1 && bl {
            self.cx[CXM1FB as usize] |= 0x40;
        }
        if bl && pf {
            self.cx[CXBLPF as usize] |= 0x80;
        }
        if p0 && p1 {
            self.cx[CXPPMM as usize] |= 0x80;
        }
        if m0 && m1 {
            self.cx[CXPPMM as usize] |= 0x40;
        }
    }

    // ---- register interface ----

    /// Read a TIA register (collision latches + input ports). `addr` is the
    /// already-masked low 4 bits.
    pub fn read(&mut self, addr: u16) -> u8 {
        match addr & 0x0F {
            CXM0P => self.cx[CXM0P as usize],
            CXM1P => self.cx[CXM1P as usize],
            CXP0FB => self.cx[CXP0FB as usize],
            CXP1FB => self.cx[CXP1FB as usize],
            CXM0FB => self.cx[CXM0FB as usize],
            CXM1FB => self.cx[CXM1FB as usize],
            CXBLPF => self.cx[CXBLPF as usize],
            CXPPMM => self.cx[CXPPMM as usize],
            INPT4 => self.inpt4,
            INPT5 => self.inpt5,
            _ => 0,
        }
    }

    /// Write a TIA register. `addr` is the already-masked low 6 bits.
    pub fn write(&mut self, addr: u16, v: u8) {
        match addr & 0x3F {
            VSYNC => self.vsync = v & 0x02 != 0,
            VBLANK => self.vblank = v & 0x02 != 0,
            WSYNC => self.wsync = true,
            RSYNC => self.clock = 0,
            NUSIZ0 => {
                self.p0.nusiz = v & 0x07;
                self.m0.size = 1 << ((v >> 4) & 0x03);
            }
            NUSIZ1 => {
                self.p1.nusiz = v & 0x07;
                self.m1.size = 1 << ((v >> 4) & 0x03);
            }
            COLUP0 => {
                self.p0.color = v;
                self.m0.color = v;
            }
            COLUP1 => {
                self.p1.color = v;
                self.m1.color = v;
            }
            COLUPF => {
                self.colupf = v;
                self.bl.color = v;
            }
            COLUBK => self.colubk = v,
            CTRLPF => {
                self.ctrlpf = v;
                self.bl.size = 1 << ((v >> 4) & 0x03);
            }
            REFP0 => self.p0.reflect = v & 0x08 != 0,
            REFP1 => self.p1.reflect = v & 0x08 != 0,
            PF0 => {
                self.pf0 = v;
                self.rebuild_pf();
            }
            PF1 => {
                self.pf1 = v;
                self.rebuild_pf();
            }
            PF2 => {
                self.pf2 = v;
                self.rebuild_pf();
            }
            RESP0 => self.p0.pos = self.strobe_pos(),
            RESP1 => self.p1.pos = self.strobe_pos(),
            RESM0 => self.m0.pos = self.strobe_pos(),
            RESM1 => self.m1.pos = self.strobe_pos(),
            RESBL => self.bl.pos = self.strobe_pos(),
            AUDC0 => self.audio[0].set_control(v),
            AUDC1 => self.audio[1].set_control(v),
            AUDF0 => self.audio[0].set_freq(v),
            AUDF1 => self.audio[1].set_freq(v),
            AUDV0 => self.audio[0].set_volume(v),
            AUDV1 => self.audio[1].set_volume(v),
            GRP0 => {
                self.p0.grp = v;
                // Writing GRP0 latches the *old* GRP1 into its delayed copy.
                self.p1.grp_old = self.p1.grp;
            }
            GRP1 => {
                self.p1.grp = v;
                self.p0.grp_old = self.p0.grp;
                self.bl.enabled_old = self.bl.enabled;
            }
            ENAM0 => self.m0.enabled = v & 0x02 != 0,
            ENAM1 => self.m1.enabled = v & 0x02 != 0,
            ENABL => self.bl.enabled = v & 0x02 != 0,
            HMP0 => self.p0.hm = v >> 4,
            HMP1 => self.p1.hm = v >> 4,
            HMM0 => self.m0.hm = v >> 4,
            HMM1 => self.m1.hm = v >> 4,
            HMBL => self.bl.hm = v >> 4,
            VDELP0 => self.p0.vdel = v & 0x01 != 0,
            VDELP1 => self.p1.vdel = v & 0x01 != 0,
            VDELBL => self.bl.vdel = v & 0x01 != 0,
            RESMP0 => {
                if v & 0x02 != 0 {
                    self.m0.pos = self.p0.pos;
                }
            }
            RESMP1 => {
                if v & 0x02 != 0 {
                    self.m1.pos = self.p1.pos;
                }
            }
            HMOVE => self.apply_hmove(),
            HMCLR => {
                self.p0.hm = 0;
                self.p1.hm = 0;
                self.m0.hm = 0;
                self.m1.hm = 0;
                self.bl.hm = 0;
            }
            CXCLR => self.cx = [0; 8],
            _ => {}
        }
    }

    /// Assemble the 20-bit playfield from PF0/PF1/PF2. PF0 contributes its high
    /// nibble (bits 4-7) as PF bits 0-3 (reversed), PF1 contributes bits 4-11
    /// (reversed), PF2 contributes bits 12-19 (in order). This matches the TIA's
    /// odd bit ordering documented in the Programmer's Guide.
    fn rebuild_pf(&mut self) {
        let mut pf: u32 = 0;
        // PF0: only bits 4-7 are used, drawn MSB→LSB as the leftmost 4 PF bits.
        for i in 0..4u32 {
            if self.pf0 & (0x10 << i) != 0 {
                pf |= 1 << i;
            }
        }
        // PF1: bits 7..0 map to PF bits 4..11 (reversed).
        for i in 0..8u32 {
            if self.pf1 & (0x80 >> i) != 0 {
                pf |= 1 << (4 + i);
            }
        }
        // PF2: bits 0..7 map to PF bits 12..19 (in order).
        for i in 0..8u32 {
            if self.pf2 & (1 << i) != 0 {
                pf |= 1 << (12 + i);
            }
        }
        self.pf = pf;
    }

    /// Position value when an RESPx/RESMx/RESBL strobe lands. The object's
    /// counter is set so it starts drawing a few clocks after the strobe (the
    /// TIA has a fixed pipeline delay). We map the current beam position into
    /// the 0..159 object space.
    fn strobe_pos(&self) -> u16 {
        // During HBLANK the strobe sets the object to pixel 0; in the visible
        // region it sets it to the current visible pixel (+ a small hardware
        // delay).
        if self.clock < HBLANK {
            0
        } else {
            let px = self.clock - HBLANK;
            (px + 5) % 160
        }
    }

    /// Apply HMOVE: add each object's signed fine-motion nibble to its position
    /// and blank the left-edge comb for this line.
    fn apply_hmove(&mut self) {
        self.p0.pos = hmove_pos(self.p0.pos, self.p0.hm);
        self.p1.pos = hmove_pos(self.p1.pos, self.p1.hm);
        self.m0.pos = hmove_pos(self.m0.pos, self.m0.hm);
        self.m1.pos = hmove_pos(self.m1.pos, self.m1.hm);
        self.bl.pos = hmove_pos(self.bl.pos, self.bl.hm);
        self.hmove_applied = true;
    }
}

/// Is `px` within a `width`-wide span starting at `start` (mod 160)?
fn in_span(px: u16, start: u16, width: u16) -> bool {
    let rel = if px >= start { px - start } else { 160 + px - start };
    rel < width
}

/// Decode a player's NUSIZ low-3 bits into (copies, spacing, pixel width).
/// Spacing is in colour clocks between copy start points.
fn nusiz_player(nusiz: u8) -> (u8, u16, u16) {
    match nusiz & 0x07 {
        0 => (1, 0, 1),   // one copy
        1 => (2, 16, 1),  // two copies, close
        2 => (2, 32, 1),  // two copies, medium
        3 => (3, 16, 1),  // three copies, close
        4 => (2, 64, 1),  // two copies, wide
        5 => (1, 0, 2),   // one copy, double size
        6 => (3, 32, 1),  // three copies, medium
        7 => (1, 0, 4),   // one copy, quad size
        _ => (1, 0, 1),
    }
}

/// Apply a 4-bit HMOVE nibble to a position. The nibble is a signed value:
/// 0..7 move left (subtract), 8..15 move right (the TIA treats it as
/// two's-complement of the top bit), per the Programmer's Guide HMOVE table.
fn hmove_pos(pos: u16, hm: u8) -> u16 {
    // The nibble is interpreted as signed -8..+7 where positive moves the
    // object LEFT (earlier). Stella's table: $70 = +7 (max left), $80 = -8 (max
    // right). We stored only the high nibble, so `hm` is 0..15.
    let signed = ((hm as i8) << 4 >> 4) as i16; // sign-extend the 4-bit value
    let np = (pos as i16 - signed).rem_euclid(160);
    np as u16
}

/// NTSC TIA colour byte → RGB. Bits 7-4 select the hue (0..15), bits 3-1 the
/// luminance (0..7); bit 0 is ignored. Derived from the standard Stella NTSC
/// palette.
fn ntsc_rgb(color: u8) -> [u8; 3] {
    let idx = (color >> 1) as usize & 0x7F;
    NTSC_PALETTE[idx]
}

/// The 128-entry NTSC palette (16 hues × 8 luminances), RGB888. Values match
/// the widely-published Stella NTSC palette.
static NTSC_PALETTE: [[u8; 3]; 128] = [
    // Hue 0 (grey)
    [0x00, 0x00, 0x00], [0x40, 0x40, 0x40], [0x6C, 0x6C, 0x6C], [0x90, 0x90, 0x90],
    [0xB0, 0xB0, 0xB0], [0xC8, 0xC8, 0xC8], [0xDC, 0xDC, 0xDC], [0xEC, 0xEC, 0xEC],
    // Hue 1 (gold)
    [0x44, 0x44, 0x00], [0x64, 0x64, 0x10], [0x84, 0x84, 0x24], [0xA0, 0xA0, 0x34],
    [0xB8, 0xB8, 0x40], [0xD0, 0xD0, 0x50], [0xE8, 0xE8, 0x5C], [0xFC, 0xFC, 0x68],
    // Hue 2 (orange)
    [0x70, 0x28, 0x00], [0x84, 0x44, 0x14], [0x98, 0x5C, 0x28], [0xAC, 0x78, 0x3C],
    [0xBC, 0x8C, 0x4C], [0xCC, 0xA0, 0x5C], [0xDC, 0xB4, 0x68], [0xEC, 0xC8, 0x78],
    // Hue 3 (red-orange)
    [0x84, 0x18, 0x00], [0x98, 0x34, 0x18], [0xAC, 0x50, 0x30], [0xC0, 0x68, 0x48],
    [0xD0, 0x80, 0x5C], [0xE0, 0x94, 0x70], [0xEC, 0xA8, 0x80], [0xFC, 0xBC, 0x94],
    // Hue 4 (pink)
    [0x88, 0x00, 0x00], [0x9C, 0x20, 0x20], [0xB0, 0x3C, 0x3C], [0xC0, 0x58, 0x58],
    [0xD0, 0x70, 0x70], [0xE0, 0x88, 0x88], [0xEC, 0xA0, 0xA0], [0xFC, 0xB4, 0xB4],
    // Hue 5 (purple)
    [0x78, 0x00, 0x5C], [0x8C, 0x20, 0x74], [0xA0, 0x3C, 0x88], [0xB0, 0x58, 0x9C],
    [0xC0, 0x70, 0xB0], [0xD0, 0x84, 0xC0], [0xDC, 0x9C, 0xD0], [0xEC, 0xB0, 0xE0],
    // Hue 6 (purple-blue)
    [0x48, 0x00, 0x78], [0x60, 0x20, 0x90], [0x78, 0x3C, 0xA4], [0x8C, 0x58, 0xB8],
    [0xA0, 0x70, 0xCC], [0xB4, 0x84, 0xDC], [0xC4, 0x9C, 0xEC], [0xD4, 0xB0, 0xFC],
    // Hue 7 (blue)
    [0x14, 0x00, 0x84], [0x30, 0x20, 0x98], [0x4C, 0x3C, 0xAC], [0x68, 0x58, 0xC0],
    [0x7C, 0x70, 0xD0], [0x94, 0x88, 0xE0], [0xA8, 0xA0, 0xEC], [0xBC, 0xB4, 0xFC],
    // Hue 8 (blue)
    [0x00, 0x00, 0x88], [0x1C, 0x20, 0x9C], [0x38, 0x40, 0xB0], [0x50, 0x5C, 0xC0],
    [0x68, 0x74, 0xD0], [0x7C, 0x8C, 0xE0], [0x90, 0xA4, 0xEC], [0xA4, 0xB8, 0xFC],
    // Hue 9 (light-blue)
    [0x00, 0x18, 0x7C], [0x1C, 0x38, 0x90], [0x38, 0x54, 0xA8], [0x50, 0x70, 0xBC],
    [0x68, 0x88, 0xCC], [0x7C, 0x9C, 0xDC], [0x90, 0xB4, 0xEC], [0xA4, 0xC8, 0xFC],
    // Hue 10 (turquoise)
    [0x00, 0x2C, 0x5C], [0x1C, 0x4C, 0x78], [0x38, 0x68, 0x90], [0x50, 0x84, 0xAC],
    [0x68, 0x9C, 0xC0], [0x7C, 0xB4, 0xD4], [0x90, 0xCC, 0xE8], [0xA4, 0xE0, 0xFC],
    // Hue 11 (green-blue)
    [0x00, 0x3C, 0x2C], [0x1C, 0x5C, 0x48], [0x38, 0x7C, 0x64], [0x50, 0x9C, 0x80],
    [0x68, 0xB4, 0x94], [0x7C, 0xD0, 0xAC], [0x90, 0xE4, 0xC0], [0xA4, 0xFC, 0xD4],
    // Hue 12 (green)
    [0x00, 0x3C, 0x00], [0x20, 0x5C, 0x20], [0x40, 0x7C, 0x40], [0x5C, 0x9C, 0x5C],
    [0x74, 0xB4, 0x74], [0x8C, 0xD0, 0x8C], [0xA4, 0xE4, 0xA4], [0xB8, 0xFC, 0xB8],
    // Hue 13 (yellow-green)
    [0x14, 0x38, 0x00], [0x34, 0x5C, 0x1C], [0x50, 0x7C, 0x38], [0x6C, 0x98, 0x50],
    [0x84, 0xB4, 0x68], [0x9C, 0xCC, 0x7C], [0xB4, 0xE4, 0x90], [0xC8, 0xFC, 0xA4],
    // Hue 14 (orange-green)
    [0x2C, 0x30, 0x00], [0x4C, 0x50, 0x1C], [0x68, 0x70, 0x34], [0x84, 0x8C, 0x4C],
    [0x9C, 0xA8, 0x64], [0xB4, 0xC0, 0x78], [0xCC, 0xD4, 0x88], [0xE0, 0xEC, 0x9C],
    // Hue 15 (light-orange)
    [0x44, 0x28, 0x00], [0x64, 0x48, 0x18], [0x84, 0x68, 0x30], [0xA0, 0x84, 0x44],
    [0xB8, 0x9C, 0x58], [0xD0, 0xB4, 0x6C], [0xE8, 0xCC, 0x7C], [0xFC, 0xE0, 0x8C],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_completes_after_full_grid() {
        let mut tia = Tia::new();
        let total = CLOCKS_PER_LINE as u64 * LINES_PER_FRAME as u64;
        for _ in 0..total {
            tia.tick();
        }
        assert_eq!(tia.frame, 1);
    }

    #[test]
    fn playfield_left_half_bit0() {
        let mut tia = Tia::new();
        // PF0 high nibble bit 4 set -> PF bit 0 -> leftmost 4 pixels lit.
        tia.write(PF0, 0x10);
        assert!(tia.playfield_pixel(0));
        assert!(tia.playfield_pixel(3));
        assert!(!tia.playfield_pixel(4));
    }

    #[test]
    fn playfield_reflect_right_half() {
        let mut tia = Tia::new();
        tia.write(CTRLPF, 0x01); // reflect
        tia.write(PF0, 0x10); // PF bit 0 -> leftmost on the left half
        // On the reflected right half, PF bit 0 appears at the far right.
        assert!(tia.playfield_pixel(156));
        assert!(tia.playfield_pixel(159));
        assert!(!tia.playfield_pixel(80));
    }

    #[test]
    fn player_single_pixel() {
        let mut tia = Tia::new();
        tia.p0.pos = 10;
        tia.write(GRP0, 0x80); // leftmost bit
        tia.write(COLUP0, 0x0E);
        assert!(tia.player_pixel(&tia.p0, 10));
        assert!(!tia.player_pixel(&tia.p0, 11));
    }

    #[test]
    fn player_reflect() {
        let mut tia = Tia::new();
        tia.p0.pos = 20;
        tia.write(REFP0, 0x08); // reflect on
        tia.write(GRP0, 0x80); // bit 7
        // Reflected: bit 7 now appears at the rightmost (offset 7).
        assert!(tia.player_pixel(&tia.p0, 27));
        assert!(!tia.player_pixel(&tia.p0, 20));
    }

    #[test]
    fn hmove_moves_left() {
        let mut tia = Tia::new();
        tia.bl.pos = 50;
        tia.write(HMBL, 0x70); // +7 -> move 7 left
        tia.write(HMOVE, 0);
        assert_eq!(tia.bl.pos, 43);
        assert!(tia.hmove_applied);
    }

    #[test]
    fn hmove_moves_right() {
        let mut tia = Tia::new();
        tia.bl.pos = 50;
        tia.write(HMBL, 0x80); // -8 -> move 8 right
        tia.write(HMOVE, 0);
        assert_eq!(tia.bl.pos, 58);
    }

    #[test]
    fn hmclr_resets_motion() {
        let mut tia = Tia::new();
        tia.write(HMP0, 0x70);
        tia.write(HMM1, 0x80);
        tia.write(HMCLR, 0);
        assert_eq!(tia.p0.hm, 0);
        assert_eq!(tia.m1.hm, 0);
    }

    #[test]
    fn wsync_latches_and_clears_next_line() {
        let mut tia = Tia::new();
        tia.write(WSYNC, 0);
        assert!(tia.wsync);
        // Run to the end of the current line.
        for _ in 0..CLOCKS_PER_LINE {
            tia.tick();
        }
        assert!(!tia.wsync);
    }

    #[test]
    fn collision_p0_pf() {
        let mut tia = Tia::new();
        tia.update_collisions(true, false, false, false, false, true);
        assert_eq!(tia.read(CXP0FB) & 0x80, 0x80);
        tia.write(CXCLR, 0);
        assert_eq!(tia.read(CXP0FB), 0x00);
    }

    #[test]
    fn ball_size_from_ctrlpf() {
        let mut tia = Tia::new();
        tia.write(CTRLPF, 0x10); // ball size bits = 1 -> 2 px
        assert_eq!(tia.bl.size, 2);
        tia.write(CTRLPF, 0x30); // ball size bits = 3 -> 8 px
        assert_eq!(tia.bl.size, 8);
    }

    #[test]
    fn vblank_blanks_pixel() {
        let mut tia = Tia::new();
        tia.write(COLUBK, 0x1E);
        tia.vblank = true;
        // Position the beam inside the visible window.
        tia.line = VISIBLE_START_LINE;
        tia.clock = HBLANK; // visible pixel 0
        tia.tick();
        let i = 0;
        // VBLANK forces colour 0 (black).
        assert_eq!(&tia.framebuffer[i..i + 3], &[0, 0, 0]);
    }

    #[test]
    fn background_color_drawn() {
        let mut tia = Tia::new();
        tia.write(COLUBK, 0x1E); // some hue
        tia.line = VISIBLE_START_LINE;
        tia.clock = HBLANK;
        tia.tick();
        let rgb = ntsc_rgb(0x1E);
        assert_eq!(&tia.framebuffer[0..3], &rgb);
    }
}
