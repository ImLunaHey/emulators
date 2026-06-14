//! Internal memory regions: VRAM (2 CGB banks), WRAM (8 CGB banks), OAM, HRAM,
//! plus the CGB bank-select state (VBK / SVBK) and the CGB palette RAM.
//!
//! Spec: Pan Docs — Memory Map + CGB Registers. This owns no IO devices and no
//! cartridge bytes; the `Gbc` god-struct owns this plus the `Cart` and routes
//! the full address space across them in its `Bus` impl.

use crate::regions as R;

/// Heap-allocate a zeroed fixed-size array without putting `N` bytes on the
/// stack first (`Box::new([0; N])` would).
#[inline]
fn boxed<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

/// All internal RAM regions + CGB bank state.
pub struct Memory {
    /// VRAM: two 8 KiB banks on CGB. Bank selected by VBK (bit 0 of 0xFF4F).
    pub vram: Box<[u8; R::VRAM_BANK_SIZE * R::VRAM_BANKS]>,
    /// Currently selected VRAM bank (0 or 1).
    pub vram_bank: u8,

    /// WRAM: eight 4 KiB banks on CGB. 0xC000-0xCFFF is always bank 0;
    /// 0xD000-0xDFFF maps the bank selected by SVBK (1-7; 0 reads as 1).
    pub wram: Box<[u8; R::WRAM_BANK_SIZE * R::WRAM_BANKS]>,
    /// Currently selected high-WRAM bank (1-7) from SVBK.
    pub wram_bank: u8,

    /// Object Attribute Memory (40 sprites x 4 bytes).
    pub oam: Box<[u8; R::OAM_SIZE]>,
    /// High RAM (0xFF80-0xFFFE).
    pub hram: Box<[u8; R::HRAM_SIZE]>,

    /// CGB background palette RAM (8 palettes x 4 colors x 2 bytes).
    pub bg_palette: Box<[u8; R::CRAM_SIZE]>,
    /// CGB object palette RAM (same layout).
    pub obj_palette: Box<[u8; R::CRAM_SIZE]>,
    /// BCPS/BGPI (0xFF68): index + auto-increment for `bg_palette`.
    pub bcps: u8,
    /// OCPS/OBPI (0xFF6A): index + auto-increment for `obj_palette`.
    pub ocps: u8,

    /// KEY1 (0xFF4D): CGB double-speed. Bit 7 = current speed (0 normal, 1
    /// double), bit 0 = switch armed. Timing isn't modeled yet; state reserved.
    pub key1: u8,
}

impl Default for Memory {
    fn default() -> Self {
        Memory::new()
    }
}

impl Memory {
    pub fn new() -> Self {
        Memory {
            vram: boxed(),
            vram_bank: 0,
            wram: boxed(),
            wram_bank: 1,
            oam: boxed(),
            hram: boxed(),
            bg_palette: boxed(),
            obj_palette: boxed(),
            bcps: 0,
            ocps: 0,
            key1: 0,
        }
    }

    // ---- VRAM (0x8000-0x9FFF), banked by VBK ----
    #[inline]
    pub fn read_vram(&self, addr: u16) -> u8 {
        let off = (self.vram_bank as usize) * R::VRAM_BANK_SIZE
            + ((addr as usize) & (R::VRAM_BANK_SIZE - 1));
        self.vram[off]
    }
    #[inline]
    pub fn write_vram(&mut self, addr: u16, v: u8) {
        let off = (self.vram_bank as usize) * R::VRAM_BANK_SIZE
            + ((addr as usize) & (R::VRAM_BANK_SIZE - 1));
        self.vram[off] = v;
    }

    // ---- WRAM (0xC000-0xDFFF + echo), banked by SVBK in the high window ----
    /// Resolve a WRAM address (0xC000-0xDFFF, echo already folded) to a flat
    /// offset across the 8 banks. The low window (0xC000-0xCFFF) is bank 0;
    /// the high window (0xD000-0xDFFF) uses SVBK (1-7).
    #[inline]
    fn wram_off(&self, addr: u16) -> usize {
        let within = (addr as usize) & (R::WRAM_BANK_SIZE - 1);
        if addr < R::WRAMN_START {
            within // bank 0
        } else {
            let bank = self.wram_bank.max(1) as usize; // 0 maps to 1
            bank * R::WRAM_BANK_SIZE + within
        }
    }
    #[inline]
    pub fn read_wram(&self, addr: u16) -> u8 {
        self.wram[self.wram_off(addr)]
    }
    #[inline]
    pub fn write_wram(&mut self, addr: u16, v: u8) {
        let off = self.wram_off(addr);
        self.wram[off] = v;
    }

    // ---- OAM (0xFE00-0xFE9F) ----
    #[inline]
    pub fn read_oam(&self, addr: u16) -> u8 {
        self.oam[(addr as usize) - R::OAM_START as usize]
    }
    #[inline]
    pub fn write_oam(&mut self, addr: u16, v: u8) {
        self.oam[(addr as usize) - R::OAM_START as usize] = v;
    }

    // ---- HRAM (0xFF80-0xFFFE) ----
    #[inline]
    pub fn read_hram(&self, addr: u16) -> u8 {
        self.hram[(addr as usize) - R::HRAM_START as usize]
    }
    #[inline]
    pub fn write_hram(&mut self, addr: u16, v: u8) {
        self.hram[(addr as usize) - R::HRAM_START as usize] = v;
    }

    // ---- CGB bank-select registers ----
    /// VBK (0xFF4F): only bit 0 matters; unused bits read as 1.
    #[inline]
    pub fn read_vbk(&self) -> u8 {
        0xFE | (self.vram_bank & 1)
    }
    #[inline]
    pub fn write_vbk(&mut self, v: u8) {
        self.vram_bank = v & 1;
    }

    /// SVBK (0xFF70): bits 2-0 select the high-WRAM bank (0 maps to 1). Unused
    /// bits read as 1.
    #[inline]
    pub fn read_svbk(&self) -> u8 {
        0xF8 | (self.wram_bank & 0x07)
    }
    #[inline]
    pub fn write_svbk(&mut self, v: u8) {
        let b = v & 0x07;
        self.wram_bank = if b == 0 { 1 } else { b };
    }

    // ---- CGB palette RAM (auto-incrementing index/data ports) ----
    /// BCPD/BGPD (0xFF69) read: byte at the BCPS index.
    #[inline]
    pub fn read_bg_palette_data(&self) -> u8 {
        self.bg_palette[(self.bcps & 0x3F) as usize]
    }
    /// BCPD/BGPD (0xFF69) write: byte at the index, then auto-increment if
    /// BCPS bit 7 is set.
    #[inline]
    pub fn write_bg_palette_data(&mut self, v: u8) {
        let idx = (self.bcps & 0x3F) as usize;
        self.bg_palette[idx] = v;
        if self.bcps & 0x80 != 0 {
            self.bcps = 0x80 | ((self.bcps + 1) & 0x3F);
        }
    }
    /// OCPD/OBPD (0xFF6B) read.
    #[inline]
    pub fn read_obj_palette_data(&self) -> u8 {
        self.obj_palette[(self.ocps & 0x3F) as usize]
    }
    /// OCPD/OBPD (0xFF6B) write + auto-increment.
    #[inline]
    pub fn write_obj_palette_data(&mut self, v: u8) {
        let idx = (self.ocps & 0x3F) as usize;
        self.obj_palette[idx] = v;
        if self.ocps & 0x80 != 0 {
            self.ocps = 0x80 | ((self.ocps + 1) & 0x3F);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wram_high_window_banks() {
        let mut m = Memory::new();
        // bank 0 (low window)
        m.write_wram(0xC000, 0x10);
        // high window, default SVBK bank 1
        m.write_wram(0xD000, 0x20);
        m.write_svbk(2);
        m.write_wram(0xD000, 0x30); // bank 2
        m.write_svbk(1);
        assert_eq!(m.read_wram(0xC000), 0x10);
        assert_eq!(m.read_wram(0xD000), 0x20);
        m.write_svbk(2);
        assert_eq!(m.read_wram(0xD000), 0x30);
    }

    #[test]
    fn svbk_zero_maps_to_one() {
        let mut m = Memory::new();
        m.write_svbk(0);
        assert_eq!(m.wram_bank, 1);
        assert_eq!(m.read_svbk(), 0xF8 | 1);
    }

    #[test]
    fn vram_bank_select() {
        let mut m = Memory::new();
        m.write_vram(0x8000, 0xAB);
        m.write_vbk(1);
        m.write_vram(0x8000, 0xCD);
        assert_eq!(m.read_vram(0x8000), 0xCD);
        m.write_vbk(0);
        assert_eq!(m.read_vram(0x8000), 0xAB);
        assert_eq!(m.read_vbk(), 0xFE);
    }

    #[test]
    fn bg_palette_auto_increment() {
        let mut m = Memory::new();
        m.bcps = 0x80; // index 0, auto-increment on
        m.write_bg_palette_data(0x11);
        m.write_bg_palette_data(0x22);
        assert_eq!(m.bg_palette[0], 0x11);
        assert_eq!(m.bg_palette[1], 0x22);
        assert_eq!(m.bcps & 0x3F, 2);
    }
}
