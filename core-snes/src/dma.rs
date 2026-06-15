//! DMA + HDMA channels ($43x0-$43xB, controlled by $420B / $420C). Built from
//! the fullsnes DMA documentation.
//!
//! Each of the 8 channels has: parameters ($43x0), B-bus address ($43x1, the
//! $21xx register it talks to), A-bus address + bank ($43x2-$43x4), and a byte
//! count / indirect address ($43x5-$43x7). General-purpose DMA ($420B) runs a
//! full transfer immediately; HDMA ($420C) transfers per-scanline (we run it
//! once per visible line via [`Dma::hdma_line`]).
//!
//! The actual byte moves are performed by the god-struct (`Snes`), which owns
//! both the A-bus (CPU memory) and B-bus (PPU/APU registers); this module just
//! holds channel state and drives the transfer loop through a callback-free
//! interface (the orchestrator reads/writes via its own bus methods).

#[derive(Clone, Copy, Default)]
pub struct Channel {
    /// $43x0 — DMAP: direction (bit7), HDMA indirect (bit6), fixed/decrement
    /// (bits 3-4), transfer unit (bits 0-2).
    pub params: u8,
    /// $43x1 — B-bus address ($2100 + this).
    pub b_addr: u8,
    /// $43x2-$43x4 — A-bus address (16-bit) + bank.
    pub a_addr: u16,
    pub a_bank: u8,
    /// $43x5-$43x6 — byte count (GP-DMA) / indirect address (HDMA).
    pub count: u16,
    /// $43x7 — HDMA indirect bank.
    pub indirect_bank: u8,
    /// $43x8-$43x9 — HDMA table current address.
    pub table_addr: u16,
    /// $43xA — HDMA line counter.
    pub line_counter: u8,

    // HDMA runtime state.
    pub hdma_active: bool,
    pub hdma_do_transfer: bool,
}

pub struct Dma {
    pub ch: [Channel; 8],
    /// $420B latch (which channels a GP-DMA should run).
    pub gp_enable: u8,
    /// $420C latch (which channels HDMA is enabled on).
    pub hdma_enable: u8,
}

impl Default for Dma {
    fn default() -> Self {
        Dma::new()
    }
}

impl Dma {
    pub fn new() -> Dma {
        Dma {
            ch: [Channel::default(); 8],
            gp_enable: 0,
            hdma_enable: 0,
        }
    }

    /// Write a $43xx channel register.
    pub fn write_reg(&mut self, addr: u16, v: u8) {
        let chan = ((addr >> 4) & 0x0F) as usize;
        if chan >= 8 {
            return;
        }
        let c = &mut self.ch[chan];
        match addr & 0x0F {
            0x0 => c.params = v,
            0x1 => c.b_addr = v,
            0x2 => c.a_addr = (c.a_addr & 0xFF00) | v as u16,
            0x3 => c.a_addr = (c.a_addr & 0x00FF) | ((v as u16) << 8),
            0x4 => c.a_bank = v,
            0x5 => c.count = (c.count & 0xFF00) | v as u16,
            0x6 => c.count = (c.count & 0x00FF) | ((v as u16) << 8),
            0x7 => c.indirect_bank = v,
            0x8 => c.table_addr = (c.table_addr & 0xFF00) | v as u16,
            0x9 => c.table_addr = (c.table_addr & 0x00FF) | ((v as u16) << 8),
            0xA => c.line_counter = v,
            _ => {}
        }
    }

    pub fn read_reg(&self, addr: u16) -> u8 {
        let chan = ((addr >> 4) & 0x0F) as usize;
        if chan >= 8 {
            return 0;
        }
        let c = &self.ch[chan];
        match addr & 0x0F {
            0x0 => c.params,
            0x1 => c.b_addr,
            0x2 => c.a_addr as u8,
            0x3 => (c.a_addr >> 8) as u8,
            0x4 => c.a_bank,
            0x5 => c.count as u8,
            0x6 => (c.count >> 8) as u8,
            0x7 => c.indirect_bank,
            0x8 => c.table_addr as u8,
            0x9 => (c.table_addr >> 8) as u8,
            0xA => c.line_counter,
            _ => 0,
        }
    }
}

/// The byte-offset pattern for each DMA transfer unit (mode bits 0-2 of DMAP).
/// Returns the sequence of B-bus offsets used per "unit" of the transfer.
pub fn transfer_pattern(mode: u8) -> &'static [u8] {
    match mode & 0x07 {
        0 => &[0],
        1 => &[0, 1],
        2 => &[0, 0],
        3 => &[0, 0, 1, 1],
        4 => &[0, 1, 2, 3],
        5 => &[0, 1, 0, 1],
        6 => &[0, 0],
        _ => &[0, 0, 1, 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_register_writes() {
        let mut dma = Dma::new();
        dma.write_reg(0x4302, 0x34); // ch0 a_addr lo
        dma.write_reg(0x4303, 0x12); // ch0 a_addr hi
        dma.write_reg(0x4304, 0x7E); // ch0 a_bank
        dma.write_reg(0x4305, 0x00); // count lo
        dma.write_reg(0x4306, 0x10); // count hi
        assert_eq!(dma.ch[0].a_addr, 0x1234);
        assert_eq!(dma.ch[0].a_bank, 0x7E);
        assert_eq!(dma.ch[0].count, 0x1000);
    }

    #[test]
    fn transfer_patterns() {
        assert_eq!(transfer_pattern(0), &[0]);
        assert_eq!(transfer_pattern(1), &[0, 1]);
        assert_eq!(transfer_pattern(4), &[0, 1, 2, 3]);
    }
}
