//! DMA controller — 4 channels per CPU, so each `Nds` owns one `Dma` for the
//! ARM9 and one for the ARM7. Ported from ../../ds-recomp/src/io/dma.ts.
//!
//! ## Ownership / borrow strategy (READ — the device wave must not change this)
//!
//! The TS `Dma` held a `bus` reference and ran transfers itself
//! (`this.bus.read32(...)` inside `runChannel`). We can't store a bus ref in
//! Rust, and the transfer touches the SAME `Nds` that owns the `Dma` — a
//! self-borrow knot. We break it exactly like the rest of the core: **the
//! `Dma` struct owns ONLY channel + control state and never touches memory**.
//! The memory-walking loop lives on `Nds` (`Nds::run_dma_channel9` /
//! `run_dma_channel7`) which can freely call its own `read*/write*` accessors.
//!
//! Split-borrow recipe the orchestrator uses (so the device agent knows the
//! contract): a register write that arms an immediate-timing channel returns
//! the channel index to run via the `write*` return value; `Nds` then reads the
//! channel's latched src/dst/count, runs the transfer through its bus
//! accessors, and writes the post-transfer state back via [`Dma::finish_channel`].
//! Timing-triggered runs (`Nds::dma_trigger_vblank9`, …) ask the `Dma` for the
//! list of armed channels via [`Dma::channels_for_timing`] and loop.
//!
//! The IRQ-on-done line is raised through the core's `Irq` by `Nds` after the
//! transfer (the `Dma` exposes [`Dma::channel_irq_bit`] so `Nds` knows which
//! `IRQ_DMA0..3` bit to raise).

/// DMA timing modes. ARM9 decodes 3 bits (0..7); ARM7 decodes 2 bits and only
/// sees Immediate/VBlank/HBlank/Special. The closed enum replaces the TS magic
/// numbers; `Special` covers the ARM7's slot-3 (= sound/Wi-Fi) and is left for
/// the device wave to interpret per-core.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DmaTiming {
    #[default]
    Immediate,
    VBlank,
    HBlank,
    /// ARM9: line-render start (timing 3). ARM7: wireless (timing 2 in its
    /// 2-bit field maps here when slot is used).
    HDraw,
    /// ARM9 timing 4 — main-memory display FIFO.
    MainMemDisplay,
    /// Cart "card ready" timing (ARM9 = 5).
    CardReady,
    /// ARM9 timing 6 — Wi-Fi / GBA-cart.
    Special6,
    /// ARM9 timing 7 — geometry-command FIFO (GXFIFO). Our GX drains
    /// synchronously, so this fires on arm + every VBlank (see dma.ts notes).
    GxFifo,
}

impl DmaTiming {
    /// Decode the timing field. ARM9 uses control bits 11..13 (3 bits); ARM7
    /// uses bits 12..13 (2 bits: Immediate/VBlank/HBlank/Special).
    ///
    /// `ctrl` is the **16-bit** DMACNT control half (the high half of the
    /// 32-bit DMACNT). On ARM7 the 2-bit field maps to the four classic
    /// timings: 0=Immediate, 1=VBlank, 2=HBlank, 3=Special (= sound/Wi-Fi
    /// slot, represented as `HDraw` here since the device wave interprets it
    /// per-core for the ARM7).
    pub fn decode(ctrl: u32, is_arm9: bool) -> DmaTiming {
        if is_arm9 {
            match (ctrl >> 11) & 0x7 {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::HBlank,
                3 => DmaTiming::HDraw,
                4 => DmaTiming::MainMemDisplay,
                5 => DmaTiming::CardReady,
                6 => DmaTiming::Special6,
                7 => DmaTiming::GxFifo,
                _ => unreachable!(),
            }
        } else {
            match (ctrl >> 12) & 0x3 {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::HBlank,
                3 => DmaTiming::HDraw, // ARM7 "Special": DS cart / GBA cart / Wi-Fi
                _ => unreachable!(),
            }
        }
    }
}

/// One DMA channel's register + latched state. Public fields so the `Nds`
/// orchestrator can read latched src/dst/count to walk the transfer.
#[derive(Clone, Copy, Default)]
pub struct DmaChannel {
    pub src: u32,
    pub dst: u32,
    /// 32-bit DMACNT: word count in the low 16 bits, control in the high 16.
    pub count_ctrl: u32,
    /// Latched copies captured on rising-edge enable, reused for repeat.
    pub count_latched: u32,
    pub src_latched: u32,
    pub dst_latched: u32,
    pub enabled: bool,
    pub timing: DmaTiming,
    /// Address modes: 0=incr, 1=decr, 2=fixed, 3=incr+reload.
    pub src_mode: u8,
    pub dst_mode: u8,
    pub repeat: bool,
    /// false = 16-bit transfer unit, true = 32-bit.
    pub word32: bool,
    pub irq_on_done: bool,
}

impl DmaChannel {
    /// Effective word count for a run: the latched low-16-bits count, where 0
    /// means the maximum (ARM9: 0x200000, ARM7: 0x10000). The orchestrator
    /// calls this to size the transfer loop.
    pub fn word_count(&self, is_arm9: bool) -> u32 {
        if self.count_latched == 0 {
            if is_arm9 {
                0x20_0000
            } else {
                0x1_0000
            }
        } else {
            self.count_latched
        }
    }

    /// Transfer-unit stride in bytes (4 for word, 2 for halfword).
    #[inline]
    pub fn step(&self) -> u32 {
        if self.word32 {
            4
        } else {
            2
        }
    }
}

pub struct Dma {
    pub channels: [DmaChannel; 4],
    /// ARM9 decodes a 3-bit timing field + 0x200000 default word count; ARM7
    /// decodes 2 bits + 0x10000. The orchestrator also needs this to pick the
    /// per-core memory-map nuances.
    pub is_arm9: bool,
}

impl Dma {
    pub fn new(is_arm9: bool) -> Self {
        Dma {
            channels: [DmaChannel::default(); 4],
            is_arm9,
        }
    }

    // ─── Register IO (byte-granularity; the IO dispatch composes wider) ──
    //
    // Address `lo` is `addr & 0xFF` (range 0xB0..0xE0). `channel_for_addr`
    // decodes (channel, offset-within-12-byte-block):
    //   off 0 = SRC, off 4 = DST, off 8 = DMACNT (count low / control high).

    /// Decode `(channel, byte-offset 0..11)` for a DMA register address, or
    /// `None` if outside the 0xB0..0xE0 window.
    pub fn channel_for_addr(addr: u32) -> Option<(usize, usize)> {
        let lo = addr & 0xFF;
        if lo < 0xB0 || lo >= 0xE0 {
            return None;
        }
        let rel = (lo - 0xB0) as usize;
        let ch = rel / 12;
        if ch > 3 {
            return None;
        }
        Some((ch, rel - ch * 12))
    }

    pub fn read8(&self, addr: u32) -> u32 {
        let Some((ch, off)) = Self::channel_for_addr(addr) else {
            return 0;
        };
        let c = &self.channels[ch];
        let shift = (off & 3) * 8;
        if off < 4 {
            (c.src >> shift) & 0xFF
        } else if off < 8 {
            (c.dst >> shift) & 0xFF
        } else {
            (c.count_ctrl >> shift) & 0xFF
        }
    }

    pub fn read16(&self, addr: u32) -> u32 {
        let Some((ch, off)) = Self::channel_for_addr(addr) else {
            return 0;
        };
        let c = &self.channels[ch];
        match off {
            0 => c.src & 0xFFFF,
            2 => (c.src >> 16) & 0xFFFF,
            4 => c.dst & 0xFFFF,
            6 => (c.dst >> 16) & 0xFFFF,
            8 => c.count_ctrl & 0xFFFF,
            10 => (c.count_ctrl >> 16) & 0xFFFF,
            _ => 0,
        }
    }

    pub fn read32(&self, addr: u32) -> u32 {
        let Some((ch, off)) = Self::channel_for_addr(addr) else {
            return 0;
        };
        let c = &self.channels[ch];
        match off {
            0 => c.src,
            4 => c.dst,
            _ => c.count_ctrl,
        }
    }

    /// Write a DMA register byte. Returns `Some(channel)` if the write armed an
    /// immediate/GXFIFO-timed channel that `Nds` must run NOW (the orchestrator
    /// calls `Nds::run_dma_channel{9,7}` with the returned index). Wider writes
    /// (`write16`/`write32`) do the same.
    pub fn write8(&mut self, addr: u32, value: u32) -> Option<usize> {
        let (ch, off) = Self::channel_for_addr(addr)?;
        let shift = (off & 3) * 8;
        let byte = value & 0xFF;
        if off < 4 {
            let c = &mut self.channels[ch];
            c.src = (c.src & !(0xFF << shift)) | (byte << shift);
            None
        } else if off < 8 {
            let c = &mut self.channels[ch];
            c.dst = (c.dst & !(0xFF << shift)) | (byte << shift);
            None
        } else {
            let cur = self.channels[ch].count_ctrl;
            let next = (cur & !(0xFF << shift)) | (byte << shift);
            self.apply_count(ch, next)
        }
    }

    pub fn write16(&mut self, addr: u32, value: u32) -> Option<usize> {
        let (ch, off) = Self::channel_for_addr(addr)?;
        let v = value & 0xFFFF;
        match off {
            0 => {
                let c = &mut self.channels[ch];
                c.src = (c.src & 0xFFFF_0000) | v;
                None
            }
            2 => {
                let c = &mut self.channels[ch];
                c.src = (c.src & 0x0000_FFFF) | (v << 16);
                None
            }
            4 => {
                let c = &mut self.channels[ch];
                c.dst = (c.dst & 0xFFFF_0000) | v;
                None
            }
            6 => {
                let c = &mut self.channels[ch];
                c.dst = (c.dst & 0x0000_FFFF) | (v << 16);
                None
            }
            8 => {
                let cur = self.channels[ch].count_ctrl;
                self.apply_count(ch, (cur & 0xFFFF_0000) | v)
            }
            10 => {
                let cur = self.channels[ch].count_ctrl;
                self.apply_count(ch, (cur & 0x0000_FFFF) | (v << 16))
            }
            _ => None,
        }
    }

    pub fn write32(&mut self, addr: u32, value: u32) -> Option<usize> {
        let (ch, off) = Self::channel_for_addr(addr)?;
        match off {
            0 => {
                self.channels[ch].src = value;
                None
            }
            4 => {
                self.channels[ch].dst = value;
                None
            }
            _ => self.apply_count(ch, value),
        }
    }

    /// Decode a full 32-bit DMACNT write into channel control + enable state.
    /// On rising-edge enable, latch src/dst/count and report the channel back
    /// to `Nds` to run immediately when the timing is Immediate (or GXFIFO on
    /// ARM9 — our GX FIFO drains synchronously, so the "below half-full"
    /// condition is always met on arm; see the ds-recomp dma.ts note).
    fn apply_count(&mut self, ch: usize, value: u32) -> Option<usize> {
        let c = &mut self.channels[ch];
        let was_enabled = c.enabled;
        c.count_ctrl = value;
        let ctrl = (value >> 16) & 0xFFFF;
        c.dst_mode = ((ctrl >> 5) & 0x3) as u8;
        c.src_mode = ((ctrl >> 7) & 0x3) as u8;
        c.repeat = (ctrl & 0x0200) != 0;
        c.word32 = (ctrl & 0x0400) != 0;
        c.timing = DmaTiming::decode(ctrl, self.is_arm9);
        c.irq_on_done = (ctrl & 0x4000) != 0;
        c.enabled = (ctrl & 0x8000) != 0;

        if !was_enabled && c.enabled {
            // Latch source/dest/count for repeat reloads.
            c.src_latched = c.src;
            c.dst_latched = c.dst;
            c.count_latched = value & 0xFFFF; // word count in low 16 bits
            let timing = c.timing;
            if timing == DmaTiming::Immediate
                || (self.is_arm9 && timing == DmaTiming::GxFifo)
            {
                return Some(ch);
            }
        }
        None
    }

    /// The list of channel indices currently enabled with the given timing —
    /// the `Nds` timing triggers iterate this and run each one through the bus.
    pub fn channels_for_timing(&self, timing: DmaTiming) -> impl Iterator<Item = usize> + '_ {
        (0..4).filter(move |&i| self.channels[i].enabled && self.channels[i].timing == timing)
    }

    /// `IRQ_DMA0 << channel` — the completion-IRQ bit `Nds` raises after a
    /// transfer when `channel.irq_on_done` is set.
    #[inline]
    pub fn channel_irq_bit(channel: usize) -> u32 {
        super::irq::IRQ_DMA0 << channel
    }

    /// Post-transfer bookkeeping for channel `i`: writes back the moved
    /// src/dst (unless mode-3 reload, which snaps back to the latched start),
    /// then either rearms the channel for repeat (reloading the latched count)
    /// or clears the enable bit. `Nds` calls this after walking the transfer
    /// with the final src/dst it computed, so the register state the game polls
    /// matches. Returns `true` if a completion IRQ should fire.
    pub fn finish_channel(&mut self, i: usize, final_src: u32, final_dst: u32) -> bool {
        let c = &mut self.channels[i];

        // Writeback the moved pointers. Mode 3 ("increment + reload") snaps the
        // destination back to its latched start after the transfer; the source
        // has no mode-3 reload on hardware (mode 3 in the src field is treated
        // as fixed), so it keeps the value the orchestrator computed.
        if c.src_mode != 3 {
            c.src = final_src;
        }
        if c.dst_mode == 3 {
            c.dst = c.dst_latched;
        } else {
            c.dst = final_dst;
        }

        let raise_irq = c.irq_on_done;

        if c.repeat {
            // Repeat keeps the channel armed: reload the latched count so the
            // next timing trigger transfers the same length, and for mode-3
            // destination reload restore the latched start for the next burst.
            c.count_latched = c.count_ctrl & 0xFFFF;
            if c.dst_mode == 3 {
                c.dst = c.dst_latched;
            }
        } else {
            c.enabled = false;
            c.count_ctrl &= 0x7FFF_FFFF; // clear the enable bit (bit 31)
        }

        raise_irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ARM9 DMA register base addresses (low byte). channel n: SRC=0xB0+12n,
    // DST=0xB4+12n, DMACNT=0xB8+12n. We only use the low byte here because
    // `channel_for_addr` masks to `addr & 0xFF`.
    const fn src_addr(ch: u32) -> u32 {
        0x0400_00B0 + ch * 12
    }
    const fn dst_addr(ch: u32) -> u32 {
        0x0400_00B4 + ch * 12
    }
    const fn cnt_addr(ch: u32) -> u32 {
        0x0400_00B8 + ch * 12
    }

    fn arm9() -> Dma {
        Dma::new(true)
    }

    // ─── address decode ──────────────────────────────────────────────────

    #[test]
    fn channel_for_addr_decodes_all_four() {
        for ch in 0..4u32 {
            assert_eq!(Dma::channel_for_addr(src_addr(ch)), Some((ch as usize, 0)));
            assert_eq!(Dma::channel_for_addr(dst_addr(ch)), Some((ch as usize, 4)));
            assert_eq!(Dma::channel_for_addr(cnt_addr(ch)), Some((ch as usize, 8)));
        }
        assert_eq!(Dma::channel_for_addr(0x0400_00AF), None);
        assert_eq!(Dma::channel_for_addr(0x0400_00E0), None);
    }

    // ─── timing decode ───────────────────────────────────────────────────

    #[test]
    fn timing_decode_arm9_3bit() {
        let t = |field: u32| DmaTiming::decode(field << 11, true);
        assert_eq!(t(0), DmaTiming::Immediate);
        assert_eq!(t(1), DmaTiming::VBlank);
        assert_eq!(t(2), DmaTiming::HBlank);
        assert_eq!(t(3), DmaTiming::HDraw);
        assert_eq!(t(4), DmaTiming::MainMemDisplay);
        assert_eq!(t(5), DmaTiming::CardReady);
        assert_eq!(t(6), DmaTiming::Special6);
        assert_eq!(t(7), DmaTiming::GxFifo);
    }

    #[test]
    fn timing_decode_arm7_2bit_ignores_bit11() {
        // ARM7 only sees bits 12..13. Bit 11 set must NOT shift the meaning.
        let t = |field: u32| DmaTiming::decode((field << 12) | (1 << 11), false);
        assert_eq!(t(0), DmaTiming::Immediate);
        assert_eq!(t(1), DmaTiming::VBlank);
        assert_eq!(t(2), DmaTiming::HBlank);
        assert_eq!(t(3), DmaTiming::HDraw);
    }

    // ─── register read/write round-trips ─────────────────────────────────

    #[test]
    fn write32_src_dst_readback() {
        let mut d = arm9();
        assert_eq!(d.write32(src_addr(2), 0x0123_4567), None);
        assert_eq!(d.write32(dst_addr(2), 0x89AB_CDEF), None);
        assert_eq!(d.read32(src_addr(2)), 0x0123_4567);
        assert_eq!(d.read32(dst_addr(2)), 0x89AB_CDEF);
        assert_eq!(d.read16(src_addr(2)), 0x4567);
        assert_eq!(d.read16(src_addr(2) + 2), 0x0123);
        assert_eq!(d.read8(src_addr(2)), 0x67);
        assert_eq!(d.read8(src_addr(2) + 3), 0x01);
    }

    #[test]
    fn byte_writes_compose_src() {
        let mut d = arm9();
        d.write8(src_addr(0), 0xEF);
        d.write8(src_addr(0) + 1, 0xBE);
        d.write8(src_addr(0) + 2, 0xAD);
        d.write8(src_addr(0) + 3, 0xDE);
        assert_eq!(d.read32(src_addr(0)), 0xDEAD_BEEF);
    }

    #[test]
    fn half_writes_compose_count_ctrl() {
        let mut d = arm9();
        // Low half (count) then high half (control, no enable).
        assert_eq!(d.write16(cnt_addr(0), 0x0040), None); // count low
        assert_eq!(d.write16(cnt_addr(0) + 2, 0x0400), None); // word32, no enable
        assert_eq!(d.read32(cnt_addr(0)), 0x0400_0040);
        assert!(d.channels[0].word32);
        assert!(!d.channels[0].enabled);
    }

    // ─── control decode + arm/return-channel ─────────────────────────────

    #[test]
    fn immediate_enable_returns_channel_index() {
        let mut d = arm9();
        d.write32(src_addr(0), 0x0200_0000);
        d.write32(dst_addr(0), 0x0200_1000);
        // count=8, enable(0x8000) immediate.
        let armed = d.write32(cnt_addr(0), (0x8000 << 16) | 8);
        assert_eq!(armed, Some(0));
        let c = &d.channels[0];
        assert!(c.enabled);
        assert_eq!(c.timing, DmaTiming::Immediate);
        assert_eq!(c.count_latched, 8);
        assert_eq!(c.src_latched, 0x0200_0000);
        assert_eq!(c.dst_latched, 0x0200_1000);
    }

    #[test]
    fn vblank_enable_does_not_run_immediately() {
        let mut d = arm9();
        // enable | vblank(timing 1 << 11) — does not return a channel to run.
        let ctrl = 0x8000 | (1 << 11);
        let armed = d.write32(cnt_addr(3), (ctrl << 16) | 4);
        assert_eq!(armed, None);
        assert!(d.channels[3].enabled);
        assert_eq!(d.channels[3].timing, DmaTiming::VBlank);
    }

    #[test]
    fn gxfifo_enable_runs_on_arm9_but_not_arm7() {
        let ctrl = 0x8000 | (7 << 11); // GXFIFO timing
        let mut d9 = Dma::new(true);
        assert_eq!(d9.write32(cnt_addr(0), (ctrl << 16) | 1), Some(0));
        // On ARM7 the 7<<11 field decodes via bits 12..13 = 0b11 = HDraw,
        // which is not immediate, so nothing runs.
        let mut d7 = Dma::new(false);
        assert_eq!(d7.write32(cnt_addr(0), (ctrl << 16) | 1), None);
        assert_eq!(d7.channels[0].timing, DmaTiming::HDraw);
    }

    #[test]
    fn control_field_decode() {
        let mut d = arm9();
        // dst mode 1 (dec) <<5, src mode 2 (fixed) <<7, repeat<<9, word<<10,
        // irq<<14, enable<<15.
        let ctrl = (1 << 5) | (2 << 7) | (1 << 9) | (1 << 10) | (1 << 14) | (1 << 15);
        d.write32(cnt_addr(1), (ctrl << 16) | 16);
        let c = &d.channels[1];
        assert_eq!(c.dst_mode, 1);
        assert_eq!(c.src_mode, 2);
        assert!(c.repeat);
        assert!(c.word32);
        assert!(c.irq_on_done);
        assert!(c.enabled);
    }

    // ─── word_count default (count==0) ───────────────────────────────────

    #[test]
    fn zero_count_defaults_per_core() {
        let mut c = DmaChannel::default();
        c.count_latched = 0;
        assert_eq!(c.word_count(true), 0x20_0000);
        assert_eq!(c.word_count(false), 0x1_0000);
        c.count_latched = 0x40;
        assert_eq!(c.word_count(true), 0x40);
    }

    #[test]
    fn step_is_word_or_half() {
        let mut c = DmaChannel::default();
        c.word32 = false;
        assert_eq!(c.step(), 2);
        c.word32 = true;
        assert_eq!(c.step(), 4);
    }

    // ─── finish_channel: non-repeat clears enable + bit 31 ───────────────

    #[test]
    fn finish_non_repeat_clears_enable_and_writes_back() {
        let mut d = arm9();
        d.write32(src_addr(0), 0x0200_0000);
        d.write32(dst_addr(0), 0x0200_1000);
        // immediate, irq enable, count 8.
        let ctrl = (1 << 14) | (1 << 15);
        d.write32(cnt_addr(0), (ctrl << 16) | 8);
        // Simulate Nds running an 8-halfword incr/incr transfer.
        let final_src = 0x0200_0000 + 8 * 2;
        let final_dst = 0x0200_1000 + 8 * 2;
        let raise = d.finish_channel(0, final_src, final_dst);
        assert!(raise); // irq_on_done was set
        let c = &d.channels[0];
        assert!(!c.enabled);
        assert_eq!(c.count_ctrl & 0x8000_0000, 0); // bit 31 cleared
        assert_eq!(c.src, final_src);
        assert_eq!(c.dst, final_dst);
        // Readback CNT high half: enable bit (15) cleared.
        assert_eq!(d.read16(cnt_addr(0) + 2) & 0x8000, 0);
    }

    #[test]
    fn finish_no_irq_when_disabled() {
        let mut d = arm9();
        d.write32(cnt_addr(0), (0x8000u32 << 16) | 1); // enable, no irq
        assert!(!d.finish_channel(0, 0, 0));
    }

    // ─── finish_channel: repeat keeps enable, reloads count ──────────────

    #[test]
    fn finish_repeat_keeps_enable_and_reloads_count() {
        let mut d = arm9();
        d.write32(src_addr(3), 0x0200_0000);
        d.write32(dst_addr(3), 0x0700_0000);
        // enable | repeat(1<<9) | vblank(1<<11), count 8.
        let ctrl = 0x8000 | (1 << 9) | (1 << 11);
        d.write32(cnt_addr(3), (ctrl << 16) | 8);
        // Game might mutate count_latched mid-run; finish reloads it.
        d.channels[3].count_latched = 0;
        d.finish_channel(3, 0x0200_0010, 0x0700_0010);
        let c = &d.channels[3];
        assert!(c.enabled);
        assert_eq!(c.count_latched, 8); // reloaded from count_ctrl low half
        assert_ne!(c.count_ctrl & 0x8000_0000, 0); // enable bit untouched
    }

    // ─── finish_channel: dst mode 3 (incr+reload) snaps back ─────────────

    #[test]
    fn finish_dst_mode3_reloads_to_latched_start() {
        let mut d = arm9();
        d.write32(src_addr(0), 0x0200_0000);
        d.write32(dst_addr(0), 0x0700_0000);
        // dst mode 3 (0b11 << 5 = 0x60), enable, repeat, vblank.
        let ctrl = 0x8000 | 0x60 | (1 << 9) | (1 << 11);
        d.write32(cnt_addr(0), (ctrl << 16) | 4);
        assert_eq!(d.channels[0].dst_mode, 3);
        // Nds walked dst forward during the transfer; finish snaps it back.
        d.finish_channel(0, 0x0200_0008, 0x0700_0008);
        assert_eq!(d.channels[0].dst, 0x0700_0000); // latched start restored
        assert_eq!(d.channels[0].src, 0x0200_0008); // src kept (mode 0)
    }

    // ─── src mode 3 keeps its value (treated as fixed) ───────────────────

    #[test]
    fn finish_src_mode3_keeps_register() {
        let mut d = arm9();
        d.write32(src_addr(0), 0x0200_0000);
        // src mode 3 (0b11 << 7 = 0x180), enable, immediate.
        let ctrl = 0x8000 | 0x180;
        d.write32(cnt_addr(0), (ctrl << 16) | 4);
        assert_eq!(d.channels[0].src_mode, 3);
        d.finish_channel(0, 0xDEAD_BEEF, 0x0700_0008);
        // src not overwritten with the computed final (mode-3 src == fixed).
        assert_eq!(d.channels[0].src, 0x0200_0000);
    }

    // ─── channels_for_timing iterator ────────────────────────────────────

    #[test]
    fn channels_for_timing_filters_enabled_and_match() {
        let mut d = arm9();
        // ch0: vblank enabled. ch1: hblank enabled. ch2: vblank disabled.
        d.write32(cnt_addr(0), ((0x8000 | (1 << 11)) << 16) | 1);
        d.write32(cnt_addr(1), ((0x8000 | (2 << 11)) << 16) | 1);
        d.write32(cnt_addr(2), (((1 << 11)) << 16) | 1); // vblank, NOT enabled
        let vblank: Vec<usize> = d.channels_for_timing(DmaTiming::VBlank).collect();
        assert_eq!(vblank, vec![0]);
        let hblank: Vec<usize> = d.channels_for_timing(DmaTiming::HBlank).collect();
        assert_eq!(hblank, vec![1]);
    }

    // ─── channel_irq_bit ─────────────────────────────────────────────────

    #[test]
    fn channel_irq_bits() {
        assert_eq!(Dma::channel_irq_bit(0), super::super::irq::IRQ_DMA0);
        assert_eq!(Dma::channel_irq_bit(1), super::super::irq::IRQ_DMA1);
        assert_eq!(Dma::channel_irq_bit(2), super::super::irq::IRQ_DMA2);
        assert_eq!(Dma::channel_irq_bit(3), super::super::irq::IRQ_DMA3);
    }
}
