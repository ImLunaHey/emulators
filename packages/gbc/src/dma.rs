//! DMA controllers: OAM DMA (0xFF46) and the CGB VRAM DMA (HDMA/GDMA,
//! 0xFF51-0xFF55).
//!
//! Spec: Pan Docs — OAM DMA Transfer + CGB Registers / VRAM DMA Transfers.
//!
//! * **OAM DMA** copies 160 bytes from `XX00-XX9F` (source high byte = the
//!   value written to 0xFF46) into OAM (0xFE00-0xFE9F). On hardware it runs over
//!   ~160 M-cycles with the CPU restricted to HRAM; we perform the copy
//!   immediately and just account for the cycles in the integration loop. That
//!   is accurate enough for the games we target.
//! * **GDMA (general-purpose)** copies a block from ROM/RAM to VRAM all at once
//!   when 0xFF55 is written with bit 7 = 0.
//! * **HDMA (H-Blank)** copies 0x10 bytes per H-Blank when 0xFF55 is written
//!   with bit 7 = 1; the integration loop calls `hdma_hblank_step` each time the
//!   PPU enters mode 0.
//!
//! Both VRAM-DMA forms transfer `((len & 0x7F) + 1) * 0x10` bytes. The DMA
//! routines need the bus (to read source / write destination), so they take
//! `&mut dyn Bus` as a parameter, matching the foundation's ownership model.

use crate::bus::Bus;

#[derive(Default, Clone)]
pub struct Dma {
    /// Last value written to 0xFF46 (OAM DMA source high byte).
    pub oam_src: u8,

    /// HDMA source address (0xFF51/0xFF52), lower 4 bits forced to 0.
    pub hdma_src: u16,
    /// HDMA destination offset into VRAM (0xFF53/0xFF54), 0x0000-0x1FF0.
    pub hdma_dst: u16,
    /// Remaining 0x10-byte blocks for an active H-Blank transfer, minus one
    /// (matches the hardware length register). `active` gates whether it runs.
    pub hdma_blocks: u8,
    /// Whether an H-Blank DMA is currently in progress.
    pub hdma_active: bool,
}

impl Dma {
    // ---- OAM DMA (0xFF46) ----

    /// Read the OAM DMA register (returns the last written source-high byte).
    pub fn read_oam_reg(&self) -> u8 {
        self.oam_src
    }

    /// Start an OAM DMA: copy 0xA0 bytes from `value*0x100` into OAM. Performed
    /// immediately; the caller charges ~640 T-cycles.
    pub fn start_oam(&mut self, value: u8, bus: &mut dyn Bus) {
        self.oam_src = value;
        let base = (value as u16) << 8;
        for i in 0..0xA0u16 {
            let b = bus.read8(base.wrapping_add(i));
            bus.write8(0xFE00 + i, b);
        }
    }

    // ---- CGB VRAM DMA (0xFF51-0xFF55) ----

    pub fn write_hdma_src_hi(&mut self, v: u8) {
        self.hdma_src = (self.hdma_src & 0x00FF) | ((v as u16) << 8);
    }
    pub fn write_hdma_src_lo(&mut self, v: u8) {
        // Lower 4 bits ignored (16-byte aligned).
        self.hdma_src = (self.hdma_src & 0xFF00) | (v as u16 & 0xF0);
    }
    pub fn write_hdma_dst_hi(&mut self, v: u8) {
        // Destination is within VRAM; only bits 12-8 of the offset matter.
        self.hdma_dst = (self.hdma_dst & 0x00FF) | (((v as u16) & 0x1F) << 8);
    }
    pub fn write_hdma_dst_lo(&mut self, v: u8) {
        self.hdma_dst = (self.hdma_dst & 0xFF00) | (v as u16 & 0xF0);
    }

    /// Write 0xFF55: starts a GDMA (bit7=0) or HDMA (bit7=1) transfer, or
    /// (bit7=0 while an HDMA is active) cancels the running HDMA. Returns the
    /// number of 0x10-byte blocks transferred immediately (for cycle charging);
    /// 0 for an HDMA which transfers incrementally.
    pub fn write_hdma_ctrl(&mut self, v: u8, bus: &mut dyn Bus) -> u32 {
        let blocks = (v & 0x7F) + 1;
        if v & 0x80 != 0 {
            // Start H-Blank DMA.
            self.hdma_blocks = v & 0x7F;
            self.hdma_active = true;
            0
        } else if self.hdma_active {
            // Bit 7 = 0 while active: terminate the H-Blank DMA.
            self.hdma_active = false;
            0
        } else {
            // General-purpose DMA: transfer the whole block immediately.
            for _ in 0..blocks {
                self.transfer_block(bus);
            }
            blocks as u32
        }
    }

    /// Read 0xFF55: bit 7 = 1 when no HDMA is active (transfer complete /
    /// inactive), low 7 bits = remaining length register.
    pub fn read_hdma_ctrl(&self) -> u8 {
        if self.hdma_active {
            self.hdma_blocks & 0x7F
        } else {
            0xFF
        }
    }

    /// Called by the integration loop each time the PPU enters H-Blank (mode 0)
    /// on a visible line. Transfers one 0x10-byte block if an HDMA is active.
    /// Returns true if a block was transferred (caller charges the cycles).
    pub fn hdma_hblank_step(&mut self, bus: &mut dyn Bus) -> bool {
        if !self.hdma_active {
            return false;
        }
        self.transfer_block(bus);
        if self.hdma_blocks == 0 {
            self.hdma_active = false;
        } else {
            self.hdma_blocks -= 1;
        }
        true
    }

    /// Copy one 0x10-byte block from the source to VRAM and advance pointers.
    fn transfer_block(&mut self, bus: &mut dyn Bus) {
        for _ in 0..0x10 {
            let b = bus.read8(self.hdma_src);
            bus.write8(0x8000 | (self.hdma_dst & 0x1FFF), b);
            self.hdma_src = self.hdma_src.wrapping_add(1);
            self.hdma_dst = (self.hdma_dst.wrapping_add(1)) & 0x1FFF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::Gbc;

    #[test]
    fn oam_dma_copies_160_bytes() {
        let mut gbc = Gbc::new();
        // Put a recognizable pattern in WRAM at 0xC000.
        for i in 0..0xA0u16 {
            gbc.write8(0xC000 + i, (i as u8).wrapping_add(1));
        }
        gbc.write8(0xFF46, 0xC0); // start OAM DMA from 0xC000
        for i in 0..0xA0u16 {
            assert_eq!(gbc.read8(0xFE00 + i), (i as u8).wrapping_add(1));
        }
    }

    #[test]
    fn gdma_copies_to_vram() {
        let mut gbc = Gbc::new();
        for i in 0..0x10u16 {
            gbc.write8(0xC000 + i, 0xA0 + i as u8);
        }
        gbc.write8(0xFF51, 0xC0); // src hi
        gbc.write8(0xFF52, 0x00); // src lo
        gbc.write8(0xFF53, 0x00); // dst hi (VRAM offset 0)
        gbc.write8(0xFF54, 0x00); // dst lo
        gbc.write8(0xFF55, 0x00); // GDMA, 1 block (16 bytes)
        for i in 0..0x10u16 {
            assert_eq!(gbc.read8(0x8000 + i), 0xA0 + i as u8);
        }
        // Status register reads "complete".
        assert_eq!(gbc.read8(0xFF55), 0xFF);
    }
}
