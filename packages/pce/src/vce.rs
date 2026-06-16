//! HuC6260 VCE — the Video Color Encoder. Holds the palette RAM and converts
//! the VDC's palette indices into RGB for the framebuffer.
//!
//! Spec: Archaic Pixels "HuC6260", pcedev wiki "VCE". The VCE has 512 palette
//! entries (16 "subpalettes" of 16 colors for background, plus 16 of 16 for
//! sprites = 512 total). Each entry is 9 bits: GGGRRRBBB (3 bits per channel),
//! stored in a 16-bit word. Color 0 of every background subpalette is shared
//! (the backdrop). The CPU programs the VCE via an address-latch + data port.
//!
//! Port interface (in the I/O page):
//!   $0400  control (we accept it; selects dot clock — affects display width)
//!   $0402  color-table address low
//!   $0403  color-table address high (bit0)
//!   $0404  color-table data low
//!   $0405  color-table data high (bit0)  -> writing high auto-increments addr

/// Number of 9-bit palette entries.
pub const PALETTE_ENTRIES: usize = 512;

pub struct Vce {
    /// 512 entries of 9-bit GRB color (low 9 bits used).
    pub palette: Box<[u16; PALETTE_ENTRIES]>,
    /// Current color-table address (9 bits).
    addr: u16,
    /// Latched low byte during a data-low write (committed on the high write).
    data_lo: u8,
    /// Control register (dot-clock select etc.). bits 0-1 select 5.37/7.16/10.74
    /// MHz dot clock => 256 / 341 / 512 pixel display widths.
    control: u8,
    /// Precomputed RGBA8888 for each of the 512 entries (recomputed on write).
    pub rgba: Box<[[u8; 4]; PALETTE_ENTRIES]>,
}

impl Default for Vce {
    fn default() -> Self {
        Vce::new()
    }
}

impl Vce {
    pub fn new() -> Vce {
        let mut vce = Vce {
            palette: vec![0u16; PALETTE_ENTRIES]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            addr: 0,
            data_lo: 0,
            control: 0,
            rgba: vec![[0u8; 4]; PALETTE_ENTRIES]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        };
        for i in 0..PALETTE_ENTRIES {
            vce.recompute(i);
        }
        vce
    }

    /// Display width selected by the dot clock (control bits 0-1).
    pub fn display_width(&self) -> usize {
        match self.control & 0x03 {
            0 => 256,
            1 => 341,
            _ => 512,
        }
    }

    /// Write to a VCE port (low 3 bits of the I/O address select the register).
    pub fn write(&mut self, reg: u8, v: u8) {
        match reg & 0x07 {
            0x00 => self.control = v,            // $0400 control
            0x02 => self.addr = (self.addr & 0x100) | v as u16, // addr low
            0x03 => self.addr = (self.addr & 0x0FF) | ((v as u16 & 1) << 8), // addr hi
            0x04 => self.data_lo = v,            // data low (latched)
            0x05 => {
                // data high (bit0) -> commit the 9-bit entry + auto-increment.
                let entry = ((v as u16 & 1) << 8) | self.data_lo as u16;
                let idx = (self.addr as usize) & (PALETTE_ENTRIES - 1);
                self.palette[idx] = entry;
                self.recompute(idx);
                self.addr = (self.addr + 1) & 0x1FF;
            }
            _ => {}
        }
    }

    /// Read a VCE port. Mostly the color data + a stable control read.
    pub fn read(&mut self, reg: u8) -> u8 {
        match reg & 0x07 {
            0x04 => {
                let idx = (self.addr as usize) & (PALETTE_ENTRIES - 1);
                (self.palette[idx] & 0xFF) as u8
            }
            0x05 => {
                let idx = (self.addr as usize) & (PALETTE_ENTRIES - 1);
                let hi = ((self.palette[idx] >> 8) & 1) as u8;
                self.addr = (self.addr + 1) & 0x1FF;
                hi | 0xFE // upper bits read as 1
            }
            _ => 0xFF,
        }
    }

    /// RGBA8888 for palette entry `idx`.
    #[inline]
    pub fn color(&self, idx: usize) -> [u8; 4] {
        self.rgba[idx & (PALETTE_ENTRIES - 1)]
    }

    /// Recompute the cached RGBA for entry `idx` from its 9-bit GRB value.
    fn recompute(&mut self, idx: usize) {
        let v = self.palette[idx];
        // 9-bit layout: bits 8-6 = green, 5-3 = red, 2-0 = blue (GGGRRRBBB).
        let g = ((v >> 6) & 0x07) as u8;
        let r = ((v >> 3) & 0x07) as u8;
        let b = (v & 0x07) as u8;
        // Expand 3-bit channel to 8-bit (replicate: x*255/7).
        let ex = |c: u8| ((c as u16 * 255 + 3) / 7) as u8;
        self.rgba[idx] = [ex(r), ex(g), ex(b), 0xFF];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_readback_entry() {
        let mut vce = Vce::new();
        // Set color-table address to entry 1.
        vce.write(0x02, 0x01); // addr low = 1
        vce.write(0x03, 0x00); // addr high = 0
        // Write a 9-bit value: green=7, red=0, blue=0 => 0b111_000_000 = 0x1C0.
        vce.write(0x04, 0xC0); // data low
        vce.write(0x05, 0x01); // data high (bit8)
        assert_eq!(vce.palette[1], 0x1C0);
        // Green channel should be full, red/blue zero.
        let c = vce.color(1);
        assert_eq!(c[1], 0xFF); // green
        assert_eq!(c[0], 0x00); // red
        assert_eq!(c[2], 0x00); // blue
    }

    #[test]
    fn data_high_auto_increments_address() {
        let mut vce = Vce::new();
        vce.write(0x02, 0x05);
        vce.write(0x03, 0x00);
        vce.write(0x04, 0x07); // blue=7
        vce.write(0x05, 0x00); // commit entry 5, addr -> 6
        vce.write(0x04, 0x38); // red=7 (0b111000)
        vce.write(0x05, 0x00); // commit entry 6
        assert_eq!(vce.palette[5] & 0x07, 0x07); // blue full
        assert_eq!((vce.palette[6] >> 3) & 0x07, 0x07); // red full
    }

    #[test]
    fn display_width_from_control() {
        let mut vce = Vce::new();
        vce.write(0x00, 0x00);
        assert_eq!(vce.display_width(), 256);
        vce.write(0x00, 0x01);
        assert_eq!(vce.display_width(), 341);
        vce.write(0x00, 0x02);
        assert_eq!(vce.display_width(), 512);
    }

    #[test]
    fn full_white_color() {
        let mut vce = Vce::new();
        vce.write(0x02, 0x00);
        vce.write(0x03, 0x00);
        vce.write(0x04, 0xFF); // all low 8 bits
        vce.write(0x05, 0x01); // bit 8 -> 0x1FF (all channels max)
        let c = vce.color(0);
        assert_eq!(c, [0xFF, 0xFF, 0xFF, 0xFF]);
    }
}
