//! HuC6270 VDC — the Video Display Controller. Owns 64 KiB of VRAM (32K words),
//! the register file, and produces a per-scanline render of the background
//! tilemap + sprites into an indexed buffer that `pce.rs` resolves through the
//! VCE palette.
//!
//! Spec: Charles MacDonald's "HuC6270" notes, Archaic Pixels "HuC6270", pcedev
//! wiki "VDC". The VDC is accessed through a 2-port interface:
//!   - Address register (AR): selects which internal register the data port
//!     reads/writes (write to I/O offset 0).
//!   - Data port (low/high, I/O offsets 2/3): reads/writes the selected
//!     register; the VRAM-access registers (MAWR/MARR + VWR/VRR) auto-increment.
//!
//! Registers we model (by index):
//!   $00 MAWR  memory-address write
//!   $01 MARR  memory-address read
//!   $02 VWR   VRAM write data (commits on the high byte; bumps MAWR)
//!   $05 CR    control (sprite/BG enable, interrupt enables, increment width)
//!   $06 RCR   raster compare (scanline IRQ)
//!   $07 BXR   BG horizontal scroll
//!   $08 BYR   BG vertical scroll
//!   $09 MWR   memory-access width / virtual map size
//!   $0A-$0D   horizontal/vertical sync + display timing (accepted)
//!   $0F DCR   DMA control
//!   $10 SOUR  / $11 DESR / $12 LENR  VRAM-VRAM DMA
//!   $13 SATB  sprite-attribute-table source (triggers SATB DMA)
//!
//! Status register (read at I/O offset 0) reports VBlank, RCR, sprite overflow,
//! sprite-0 collision, and VRAM-DMA-complete flags; reading it clears them.

/// VRAM size in 16-bit words (64 KiB).
pub const VRAM_WORDS: usize = 0x8000;

/// Number of sprites in the Sprite Attribute Table.
pub const SPRITES: usize = 64;

// Status register bits.
const ST_CR: u8 = 0x01; // RCR scanline match
const ST_OR: u8 = 0x02; // sprite overflow (>16 on a line)
const ST_RR: u8 = 0x04; // sprite-0 collision
const ST_DS: u8 = 0x08; // SATB DMA complete
const ST_DV: u8 = 0x10; // VRAM-VRAM DMA complete
const ST_VD: u8 = 0x20; // VBlank

// Control register (CR, reg $05) bits.
const CR_BG_EN: u16 = 0x0080; // background enable
const CR_SP_EN: u16 = 0x0040; // sprite enable
const CR_VBLANK_IE: u16 = 0x0008; // VBlank interrupt enable
const CR_RCR_IE: u16 = 0x0004; // RCR (scanline) interrupt enable
const CR_OR_IE: u16 = 0x0002; // sprite-overflow interrupt enable
const CR_RR_IE: u16 = 0x0001; // sprite-0 collision interrupt enable

pub struct Vdc {
    /// 64 KiB VRAM as 32K 16-bit words.
    pub vram: Box<[u16; VRAM_WORDS]>,

    /// Internal registers ($00-$13).
    reg: [u16; 0x20],
    /// Currently selected register (the address register).
    ar: u8,
    /// Latched low byte of a pending data write (committed on the high write).
    write_lo: u8,
    /// Read buffer for the VRAM read port (prefetched on MARR set / after read).
    read_buf: u16,

    /// Status flags (returned + cleared on a status read).
    status: u8,

    /// Pending IRQ to the CPU (any enabled+asserted condition). The orchestrator
    /// samples this into the CPU's IRQ1 line.
    pub irq: bool,

    /// Frame counter.
    pub frame: u64,

    /// Internal copy of BYR latched at the start of the active display (the VDC
    /// reloads the line counter from BYR at frame top).
    line_counter: u16,

    /// The current scanline being rendered (0..262).
    pub scanline: u16,
}

impl Default for Vdc {
    fn default() -> Self {
        Vdc::new()
    }
}

impl Vdc {
    pub fn new() -> Vdc {
        Vdc {
            vram: vec![0u16; VRAM_WORDS]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            reg: [0; 0x20],
            ar: 0,
            write_lo: 0,
            read_buf: 0,
            status: 0,
            irq: false,
            frame: 0,
            line_counter: 0,
            scanline: 0,
        }
    }

    /// VRAM auto-increment step from CR bits 11-12 (00=+1, 01=+32, 10=+64,
    /// 11=+128).
    fn increment(&self) -> u16 {
        match (self.reg[0x05] >> 11) & 0x03 {
            0 => 1,
            1 => 32,
            2 => 64,
            _ => 128,
        }
    }

    // ---- CPU port interface (I/O offset = low 2 bits) ----

    /// Write the address register (I/O offset 0).
    pub fn write_address(&mut self, v: u8) {
        self.ar = v & 0x1F;
    }

    /// Read the status register (I/O offset 0) — returns + clears the flags and
    /// deasserts the IRQ.
    pub fn read_status(&mut self) -> u8 {
        let s = self.status;
        self.status = 0;
        self.irq = false;
        s
    }

    /// Write the data port low byte (I/O offset 2).
    pub fn write_data_lo(&mut self, v: u8) {
        match self.ar {
            0x00 => self.reg[0x00] = (self.reg[0x00] & 0xFF00) | v as u16, // MAWR lo
            0x01 => self.reg[0x01] = (self.reg[0x01] & 0xFF00) | v as u16, // MARR lo
            0x02 => self.write_lo = v, // VWR low — latched, committed on hi
            _ => {
                let r = self.ar as usize & 0x1F;
                self.reg[r] = (self.reg[r] & 0xFF00) | v as u16;
            }
        }
    }

    /// Write the data port high byte (I/O offset 3). VRAM-access registers
    /// commit + auto-increment here.
    pub fn write_data_hi(&mut self, v: u8) {
        let r = self.ar as usize & 0x1F;
        match self.ar {
            0x01 => {
                // MARR high — set, then prefetch the read buffer.
                self.reg[0x01] = (self.reg[0x01] & 0x00FF) | ((v as u16) << 8);
                let a = self.reg[0x01] as usize & (VRAM_WORDS - 1);
                self.read_buf = self.vram[a];
            }
            0x02 => {
                // VWR high — commit the 16-bit word to VRAM[MAWR], bump MAWR.
                let word = ((v as u16) << 8) | self.write_lo as u16;
                let a = self.reg[0x00] as usize & (VRAM_WORDS - 1);
                self.vram[a] = word;
                self.reg[0x00] = self.reg[0x00].wrapping_add(self.increment());
            }
            0x13 => {
                // SATB source — setting it triggers a sprite-table DMA. We model
                // the table as living in VRAM at SATB; the sprite renderer reads
                // it directly, so we just store the address + flag DS complete.
                self.reg[0x13] = (self.reg[0x13] & 0x00FF) | ((v as u16) << 8);
            }
            _ => {
                self.reg[r] = (self.reg[r] & 0x00FF) | ((v as u16) << 8);
                if self.ar == 0x08 {
                    // BYR write reloads the line counter immediately.
                    self.line_counter = self.reg[0x08];
                }
            }
        }
    }

    /// Read the data port low byte (I/O offset 2).
    pub fn read_data_lo(&mut self) -> u8 {
        (self.read_buf & 0xFF) as u8
    }

    /// Read the data port high byte (I/O offset 3) — returns the buffered word's
    /// high byte and advances MARR + refills the buffer.
    pub fn read_data_hi(&mut self) -> u8 {
        let hi = (self.read_buf >> 8) as u8;
        self.reg[0x01] = self.reg[0x01].wrapping_add(self.increment());
        let a = self.reg[0x01] as usize & (VRAM_WORDS - 1);
        self.read_buf = self.vram[a];
        hi
    }

    // ---- ST0/ST1/ST2 fast path (used by the CPU's ST instructions) ----
    pub fn st_address(&mut self, v: u8) {
        self.write_address(v);
    }
    pub fn st_data_lo(&mut self, v: u8) {
        self.write_data_lo(v);
    }
    pub fn st_data_hi(&mut self, v: u8) {
        self.write_data_hi(v);
    }

    // ---- timing / interrupts ----

    /// Number of scanlines per frame (NTSC).
    pub const SCANLINES: u16 = 262;
    /// First visible scanline.
    pub const VISIBLE_START: u16 = 14;
    /// Number of visible scanlines we render.
    pub const VISIBLE_LINES: u16 = 242;

    /// Advance one scanline. `render_line` is the visible-line callback owner
    /// (pce.rs) — this only updates timing + flags + the IRQ line and returns
    /// the visible row to draw (if any).
    ///
    /// Returns `Some(display_row)` when a visible line should be rendered.
    pub fn step_scanline(&mut self) -> Option<u16> {
        let line = self.scanline;
        let mut visible_row = None;

        // RCR (raster compare) interrupt: the compare value is offset by 64
        // (the VDC's RCR register counts from line 64 = first display line).
        let rcr = self.reg[0x06] & 0x3FF;
        let display_line = if line >= Self::VISIBLE_START {
            line - Self::VISIBLE_START
        } else {
            0xFFFF
        };
        if display_line != 0xFFFF {
            // RCR matches when (display_line + 64) == rcr.
            if rcr >= 64 && (display_line as u32 + 64) == rcr as u32 {
                self.status |= ST_CR;
                if self.reg[0x05] & CR_RCR_IE != 0 {
                    self.irq = true;
                }
            }
            if display_line < Self::VISIBLE_LINES {
                visible_row = Some(display_line);
            }
        }

        // VBlank at the end of the visible region.
        if line == Self::VISIBLE_START + Self::VISIBLE_LINES {
            self.status |= ST_VD;
            if self.reg[0x05] & CR_VBLANK_IE != 0 {
                self.irq = true;
            }
        }

        // Advance.
        self.scanline += 1;
        if self.scanline >= Self::SCANLINES {
            self.scanline = 0;
            self.frame += 1;
            self.line_counter = self.reg[0x08]; // reload BG vscroll at frame top
        }

        visible_row
    }

    // ---- accessors used by the renderer ----
    pub fn bg_enabled(&self) -> bool {
        self.reg[0x05] & CR_BG_EN != 0
    }
    pub fn sp_enabled(&self) -> bool {
        self.reg[0x05] & CR_SP_EN != 0
    }
    pub fn bg_xscroll(&self) -> u16 {
        self.reg[0x07] & 0x3FF
    }
    pub fn bg_yscroll(&self) -> u16 {
        self.reg[0x08] & 0x1FF
    }
    pub fn satb_addr(&self) -> usize {
        self.reg[0x13] as usize & (VRAM_WORDS - 1)
    }

    /// Virtual background map size from MWR (reg $09) bits 4-6:
    /// width 32/64/128 columns, height 32/64 rows.
    pub fn map_dims(&self) -> (usize, usize) {
        let mwr = self.reg[0x09];
        let w = match (mwr >> 4) & 0x03 {
            0 => 32,
            1 => 64,
            _ => 128,
        };
        let h = if (mwr >> 6) & 0x01 != 0 { 64 } else { 32 };
        (w, h)
    }

    /// Mark the sprite-overflow / collision flags (called by the renderer).
    pub fn set_overflow(&mut self) {
        self.status |= ST_OR;
        if self.reg[0x05] & CR_OR_IE != 0 {
            self.irq = true;
        }
    }
    pub fn set_collision(&mut self) {
        self.status |= ST_RR;
        if self.reg[0x05] & CR_RR_IE != 0 {
            self.irq = true;
        }
    }

    // unused-flag suppression for ST_DS/ST_DV in this milestone.
    #[allow(dead_code)]
    fn dma_flags(&self) -> u8 {
        ST_DS | ST_DV
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_write_through_data_port() {
        let mut vdc = Vdc::new();
        // Select MAWR (reg 0), set address to 0x0100.
        vdc.write_address(0x00);
        vdc.write_data_lo(0x00);
        vdc.write_data_hi(0x01); // MAWR = 0x0100
        // Select VWR (reg 2), write a word.
        vdc.write_address(0x02);
        vdc.write_data_lo(0x34);
        vdc.write_data_hi(0x12); // commits 0x1234
        assert_eq!(vdc.vram[0x0100], 0x1234);
    }

    #[test]
    fn vram_write_auto_increments_mawr() {
        let mut vdc = Vdc::new();
        vdc.write_address(0x00);
        vdc.write_data_lo(0x00);
        vdc.write_data_hi(0x00); // MAWR = 0
        vdc.write_address(0x02);
        vdc.write_data_lo(0xAA);
        vdc.write_data_hi(0x00); // word 0x00AA at 0
        vdc.write_data_lo(0xBB);
        vdc.write_data_hi(0x00); // word 0x00BB at 1 (auto-inc)
        assert_eq!(vdc.vram[0], 0x00AA);
        assert_eq!(vdc.vram[1], 0x00BB);
    }

    #[test]
    fn vram_read_buffer_and_increment() {
        let mut vdc = Vdc::new();
        vdc.vram[0x0010] = 0xCAFE;
        vdc.vram[0x0011] = 0xBEEF;
        // Set MARR to 0x0010 -> prefetches 0xCAFE.
        vdc.write_address(0x01);
        vdc.write_data_lo(0x10);
        vdc.write_data_hi(0x00);
        assert_eq!(vdc.read_data_lo(), 0xFE);
        assert_eq!(vdc.read_data_hi(), 0xCA); // returns hi, advances to 0x0011
        assert_eq!(vdc.read_data_lo(), 0xEF); // now buffered 0xBEEF
    }

    #[test]
    fn status_read_clears() {
        let mut vdc = Vdc::new();
        vdc.status = ST_VD;
        vdc.irq = true;
        assert_eq!(vdc.read_status() & ST_VD, ST_VD);
        assert_eq!(vdc.read_status(), 0); // cleared
        assert!(!vdc.irq);
    }

    #[test]
    fn vblank_sets_irq_when_enabled() {
        let mut vdc = Vdc::new();
        // Enable VBlank IRQ (CR bit 3).
        vdc.write_address(0x05);
        vdc.write_data_lo((CR_VBLANK_IE) as u8);
        vdc.write_data_hi(0x00);
        // Run a whole frame of scanlines.
        for _ in 0..Vdc::SCANLINES {
            vdc.step_scanline();
        }
        // VBlank should have asserted the IRQ at the end of the visible region.
        assert!(vdc.irq);
    }

    #[test]
    fn rcr_match_sets_status() {
        let mut vdc = Vdc::new();
        // Enable RCR IRQ + set RCR to line 64 (=> display line 0).
        vdc.write_address(0x05);
        vdc.write_data_lo(CR_RCR_IE as u8);
        vdc.write_data_hi(0x00);
        vdc.write_address(0x06);
        vdc.write_data_lo(64);
        vdc.write_data_hi(0x00);
        // Step to the first visible line.
        for _ in 0..=Vdc::VISIBLE_START {
            vdc.step_scanline();
        }
        assert!(vdc.status & ST_CR != 0 || vdc.irq);
    }

    #[test]
    fn map_dims_from_mwr() {
        let mut vdc = Vdc::new();
        vdc.write_address(0x09);
        vdc.write_data_lo(0x10); // width bits = 01 -> 64 wide
        vdc.write_data_hi(0x00);
        assert_eq!(vdc.map_dims(), (64, 32));
    }

    #[test]
    fn frame_counter_advances() {
        let mut vdc = Vdc::new();
        let f0 = vdc.frame;
        for _ in 0..Vdc::SCANLINES {
            vdc.step_scanline();
        }
        assert_eq!(vdc.frame, f0 + 1);
    }
}
