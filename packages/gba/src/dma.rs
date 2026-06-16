//! DMA controller. Ported 1:1 from src/io/dma.ts.
//!
//! The TS `Dma` constructor received `bus` and `irq`; per the port contract
//! those become `&mut` parameters on the transfer/trigger methods rather than
//! stored fields.

use crate::bus::Bus;
use crate::irq::{Irq, IRQ_DMA0};

// 4 DMA channels. Start timing:
//   00 Immediate    01 VBlank    10 HBlank    11 Special
// Special: ch1/ch2 sound FIFO, ch3 video capture.
//
// DMAxSAD: source        (channel 0: 27-bit, 1-3: 28-bit)
// DMAxDAD: destination   (channel 0-2: 27-bit, 3: 28-bit)
// DMAxCNT_L: count       (channel 0-2: 14-bit, 3: 16-bit)
// DMAxCNT_H: control 16-bit:
//   05 dest control (00 inc, 01 dec, 10 fix, 11 inc/reload)
//   07 src control  (00 inc, 01 dec, 10 fix)
//   09 repeat       10 word/halfword  11 gamepak DRQ
//   12-13 start timing
//   14 irq enable   15 enable

pub const DMA_TIMING_IMMEDIATE: u32 = 0;
pub const DMA_TIMING_VBLANK: u32 = 1;
pub const DMA_TIMING_HBLANK: u32 = 2;
pub const DMA_TIMING_SPECIAL: u32 = 3;

#[derive(Default, Clone)]
pub struct DmaChannel {
    pub src: u32,
    pub dst: u32,
    pub count: u32,
    pub control: u32,

    pub internal_src: u32,
    pub internal_dst: u32,
    pub internal_count: u32,

    pub enabled: bool,
    pub timing: u32,
    pub word: bool,
    pub repeat: bool,
    pub irq_enable: bool,
    pub dst_ctrl: u32,
    pub src_ctrl: u32,
}

impl DmaChannel {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct Dma {
    pub ch: [DmaChannel; 4],
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

impl Dma {
    pub fn new() -> Self {
        Dma {
            ch: [
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
            ],
        }
    }

    // Setter helpers — masked per-channel.
    pub fn write_src(&mut self, i: usize, v: u32) {
        let mask = if i == 0 { 0x07FFFFFF } else { 0x0FFFFFFF };
        self.ch[i].src = v & mask;
    }
    pub fn write_dst(&mut self, i: usize, v: u32) {
        let mask = if i == 3 { 0x0FFFFFFF } else { 0x07FFFFFF };
        self.ch[i].dst = v & mask;
    }
    pub fn write_count(&mut self, i: usize, v: u32) {
        let mask = if i == 3 { 0xFFFF } else { 0x3FFF };
        let mut c = v & mask;
        if c == 0 {
            c = if i == 3 { 0x10000 } else { 0x4000 };
        }
        self.ch[i].count = c;
    }
    pub fn write_control(&mut self, i: usize, v: u32, bus: &mut dyn Bus, irq: &mut Irq) {
        let c = &mut self.ch[i];
        let was_enabled = c.enabled;
        c.control = v & 0xFFFF;
        c.dst_ctrl = (v >> 5) & 3;
        c.src_ctrl = (v >> 7) & 3;
        c.repeat = (v & 0x0200) != 0;
        c.word = (v & 0x0400) != 0;
        c.timing = (v >> 12) & 3;
        c.irq_enable = (v & 0x4000) != 0;
        c.enabled = (v & 0x8000) != 0;

        if !was_enabled && c.enabled {
            c.internal_src = c.src;
            c.internal_dst = c.dst;
            c.internal_count = c.count;
            let timing = c.timing;
            if timing == DMA_TIMING_IMMEDIATE {
                self.run_channel(i, bus, irq);
            }
        }
    }

    // Hook called by PPU at the appropriate transition.
    pub fn trigger_vblank(&mut self, bus: &mut dyn Bus, irq: &mut Irq) {
        for i in 0..4 {
            if self.ch[i].enabled && self.ch[i].timing == DMA_TIMING_VBLANK {
                self.run_channel(i, bus, irq);
            }
        }
    }
    pub fn trigger_hblank(&mut self, bus: &mut dyn Bus, irq: &mut Irq) {
        for i in 0..4 {
            if self.ch[i].enabled && self.ch[i].timing == DMA_TIMING_HBLANK {
                self.run_channel(i, bus, irq);
            }
        }
    }

    // Special-timing helper for sound FIFO (channels 1 and 2 only).
    pub fn trigger_sound_fifo(&mut self, channel: usize, bus: &mut dyn Bus, irq: &mut Irq) {
        let c = &mut self.ch[channel];
        if !c.enabled || c.timing != DMA_TIMING_SPECIAL {
            return;
        }
        // Sound FIFO DMAs always do 4 words to a fixed destination.
        let dst = c.internal_dst;
        let mut src = c.internal_src;
        for _ in 0..4 {
            let val = bus.read32(src);
            bus.write32(dst, val);
            src = src.wrapping_add(4);
        }
        c.internal_src = src;
        // Sound FIFO repeats automatically — don't disable.
        if c.irq_enable {
            irq.raise(IRQ_DMA0 << channel);
        }
    }

    fn run_channel(&mut self, i: usize, bus: &mut dyn Bus, irq: &mut Irq) {
        let c = &mut self.ch[i];
        let word = c.word;
        let step: u32 = if word { 4 } else { 2 };
        let mut src = c.internal_src;
        let mut dst = c.internal_dst;
        let start = dst;
        let count = c.internal_count;
        for _ in 0..count {
            if word {
                let val = bus.read32(src);
                bus.write32(dst, val);
            } else {
                let val = bus.read16(src);
                bus.write16(dst, val);
            }
            match c.src_ctrl {
                0 => src = src.wrapping_add(step),
                1 => src = src.wrapping_sub(step),
                _ => {}
            }
            match c.dst_ctrl {
                0 | 3 => dst = dst.wrapping_add(step),
                1 => dst = dst.wrapping_sub(step),
                _ => {}
            }
        }
        c.internal_src = src;
        if c.dst_ctrl == 3 {
            c.internal_dst = start; // increment-reload restores
        } else {
            c.internal_dst = dst;
        }

        if c.irq_enable {
            irq.raise(IRQ_DMA0 << i);
        }
        if !c.repeat {
            c.enabled = false;
            c.control &= !0x8000;
        } else {
            c.internal_count = c.count;
            if c.dst_ctrl == 3 {
                c.internal_dst = c.dst;
            }
        }
    }
}

// Tests ported from the (deleted) TypeScript suite src/test/dma.test.ts.
// Harness style (A): drive a real `Gba` through IO-register writes via the
// `Bus` trait, exactly as the game's MMIO writes go. Setting the DMA CNT_H
// enable bit on an immediate-timing channel triggers the transfer inline.
#[cfg(test)]
mod tests {
    use crate::bus::Bus;
    use crate::Gba;

    fn make_emu() -> Gba {
        let mut g = Gba::new();
        g.load_rom(&[0u8; 0x100]);
        g
    }

    // Trigger a DMA by writing SAD/DAD/CNT_L/CNT_H via the IO bus — same path
    // as the game's MMIO writes. Channel `ch` base = 0x040000B0 + ch*12.
    fn trigger_dma(g: &mut Gba, ch: u32, src: u32, dst: u32, count: u32, ctrl: u32) {
        let base = 0x0400_00B0 + ch * 12;
        g.write32(base, src);
        g.write32(base + 4, dst);
        g.write16(base + 8, count);
        g.write16(base + 10, ctrl);
    }

    // ---- immediate transfer --------------------------------------------

    #[test]
    fn dma0_16bit_copy_src_dst_inc() {
        let mut g = make_emu();
        for i in 0..16u32 {
            g.write8(0x0300_0100 + i, i + 1);
        }
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 8, 0x8000);
        for i in 0..16u32 {
            assert_eq!(g.read8(0x0300_0200 + i), i + 1);
        }
    }

    #[test]
    fn dma3_32bit_word_copy() {
        let mut g = make_emu();
        g.write32(0x0300_0100, 0xDEAD_BEEF);
        g.write32(0x0300_0104, 0xCAFE_BABE);
        trigger_dma(&mut g, 3, 0x0300_0100, 0x0300_0200, 2, 0x8400);
        assert_eq!(g.read32(0x0300_0200), 0xDEAD_BEEF);
        assert_eq!(g.read32(0x0300_0204), 0xCAFE_BABE);
    }

    #[test]
    fn dma_fixed_src_16bit_fill() {
        let mut g = make_emu();
        g.write16(0x0300_0100, 0x55AA);
        // src fixed (bits 7-8 = 0b10 << 7 = 0x100), dst inc, halfword, enable.
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 4, 0x8100);
        for i in 0..4u32 {
            assert_eq!(g.read16(0x0300_0200 + i * 2), 0x55AA);
        }
    }

    #[test]
    fn dma_dst_fixed_drain_pattern() {
        let mut g = make_emu();
        g.write16(0x0300_0100, 0x1111);
        g.write16(0x0300_0102, 0x2222);
        g.write16(0x0300_0104, 0x3333);
        // dst fixed (bits 5-6 = 0b10 << 5 = 0x40), src inc, halfword.
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 3, 0x8040);
        // Last value written remains at the fixed dst.
        assert_eq!(g.read16(0x0300_0200), 0x3333);
    }

    #[test]
    fn dma_dst_inc_reload_mode() {
        let mut g = make_emu();
        g.write16(0x0300_0100, 0xAAAA);
        g.write16(0x0300_0102, 0xBBBB);
        // dst increment-and-reload: dst++ during transfer (bits 5-6 = 0b11 = 0x60).
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 2, 0x8060);
        assert_eq!(g.read16(0x0300_0200), 0xAAAA);
        assert_eq!(g.read16(0x0300_0202), 0xBBBB);
    }

    #[test]
    fn dma_disables_after_immediate_no_repeat() {
        let mut g = make_emu();
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 1, 0x8000);
        assert!(!g.dma.ch[0].enabled);
    }

    // ---- count + length edge cases -------------------------------------

    #[test]
    fn zero_count_is_max_for_dma0() {
        let mut g = make_emu();
        // count=0 maps to 0x4000 for DMA0-2. Use a fixed src/dst so the
        // 16K-halfword transfer doesn't smear over real memory.
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 0, 0x81C0); // src fixed, dst fixed
        assert_eq!(g.dma.ch[0].count, 0x4000);
    }

    #[test]
    fn dma3_zero_count_is_0x10000() {
        let mut g = make_emu();
        // Set up but don't enable (only check count parsing).
        g.write32(0x0400_00D4, 0x0300_0100); // SAD
        g.write32(0x0400_00D8, 0x0300_0200); // DAD
        g.write16(0x0400_00DC, 0); // count → 0x10000
        assert_eq!(g.dma.ch[3].count, 0x10000);
    }

    #[test]
    fn dma3_max_halfword_count() {
        let mut g = make_emu();
        g.write32(0x0400_00D4, 0x0300_0100);
        g.write32(0x0400_00D8, 0x0300_0200);
        g.write16(0x0400_00DC, 0xFFFF);
        assert_eq!(g.dma.ch[3].count, 0xFFFF);
    }

    // ---- VBlank-triggered repeat (Pokemon shadow-OAM pattern) ----------

    #[test]
    fn dma3_vblank_fires_and_repeats() {
        let mut g = make_emu();
        for i in 0..16u32 {
            g.write8(0x0300_0100 + i, 0x80 + i);
        }
        // enable | repeat (0x0200) | vblank (0x1000), halfword.
        trigger_dma(&mut g, 3, 0x0300_0100, 0x0700_0000, 8, 0x8000 | 0x0200 | 0x1000);
        // VBlank-timed DMA does not fire on enable.
        assert_eq!(g.read8(0x0700_0000), 0);
        // Now trigger VBlank through the same path run_frame uses.
        let mut dma = std::mem::take(&mut g.dma);
        let mut irq = std::mem::take(&mut g.irq);
        dma.trigger_vblank(&mut g, &mut irq);
        g.dma = dma;
        g.irq = irq;
        for i in 0..16u32 {
            assert_eq!(g.read8(0x0700_0000 + i), 0x80 + i);
        }
        // Channel stays enabled because repeat is set.
        assert!(g.dma.ch[3].enabled);
    }

    #[test]
    fn pokemon_oam_update_halfword() {
        let mut g = make_emu();
        // 128 sprites x 8 bytes = 1024 bytes shadow OAM in EWRAM.
        for i in 0..128u32 {
            let off = 0x0200_0000 + i * 8;
            g.write16(off, 0x0040 | i);
            g.write16(off + 2, 0x0080 | i);
            g.write16(off + 4, 0x1000 | i);
            g.write16(off + 6, 0xFFFF);
        }
        // DMA3 immediate, halfword, src+dst inc, 512 halfwords (= 1KB).
        trigger_dma(&mut g, 3, 0x0200_0000, 0x0700_0000, 512, 0x8000);
        for i in 0..1024u32 {
            assert_eq!(g.read8(0x0700_0000 + i), g.read8(0x0200_0000 + i));
        }
    }

    #[test]
    fn pokemon_oam_update_word() {
        let mut g = make_emu();
        for i in 0..256u32 {
            g.write32(0x0200_0000 + i * 4, 0xDEAD_0000 | i);
        }
        // enable + immediate + word + src+dst inc.
        trigger_dma(&mut g, 3, 0x0200_0000, 0x0700_0000, 256, 0x8400);
        for i in 0..256u32 {
            assert_eq!(g.read32(0x0700_0000 + i * 4), 0xDEAD_0000 | i);
        }
    }

    // ---- HBlank repeat (per-scanline) ----------------------------------

    #[test]
    fn hblank_timed_dma_fires_on_hblank() {
        let mut g = make_emu();
        g.write16(0x0300_0100, 0xAA55);
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 1, 0x8000 | 0x0200 | 0x2000);
        assert_eq!(g.read16(0x0300_0200), 0);
        let mut dma = std::mem::take(&mut g.dma);
        let mut irq = std::mem::take(&mut g.irq);
        dma.trigger_hblank(&mut g, &mut irq);
        g.dma = dma;
        g.irq = irq;
        assert_eq!(g.read16(0x0300_0200), 0xAA55);
    }

    // ---- completion observable via CNT_H readback ----------------------

    #[test]
    fn dma3_cnt_h_enable_bit_clears_on_completion() {
        let mut g = make_emu();
        trigger_dma(&mut g, 3, 0x0300_0100, 0x0300_0200, 1, 0x8000);
        assert_eq!(g.read16(0x0400_00DE) & 0x8000, 0);
    }

    #[test]
    fn dma0_2_enable_bit_clears_on_completion() {
        let mut g = make_emu();
        for ch in 0..3u32 {
            trigger_dma(&mut g, ch, 0x0300_0100, 0x0300_0200, 1, 0x8000);
            let cnt_addr = 0x0400_00BA + ch * 12;
            assert_eq!(g.read16(cnt_addr) & 0x8000, 0);
        }
    }

    #[test]
    fn repeat_mode_keeps_enable_bit_set() {
        let mut g = make_emu();
        trigger_dma(&mut g, 3, 0x0300_0100, 0x0700_0000, 1, 0x8000 | 0x0200 | 0x1000);
        let mut dma = std::mem::take(&mut g.dma);
        let mut irq = std::mem::take(&mut g.irq);
        dma.trigger_vblank(&mut g, &mut irq);
        g.dma = dma;
        g.irq = irq;
        assert_ne!(g.read16(0x0400_00DE) & 0x8000, 0);
    }

    // ---- IRQ on completion ---------------------------------------------

    #[test]
    fn dma0_raises_irq_on_completion() {
        let mut g = make_emu();
        g.irq.set_ie(0xFFFF);
        g.irq.set_ime(1);
        // enable + halfword + immediate + IRQ enable (bit 14 = 0x4000).
        trigger_dma(&mut g, 0, 0x0300_0100, 0x0300_0200, 1, 0xC000);
        assert_ne!(g.irq.iflag & (1 << 8), 0); // DMA0 IRQ bit
    }

    #[test]
    fn dma3_irq_uses_bit_11() {
        let mut g = make_emu();
        g.irq.set_ie(0xFFFF);
        g.irq.set_ime(1);
        trigger_dma(&mut g, 3, 0x0300_0100, 0x0300_0200, 1, 0xC000);
        assert_ne!(g.irq.iflag & (1 << 11), 0);
    }
}
