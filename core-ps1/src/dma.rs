//! DMA controller — 7 channels + `DPCR` / `DICR`.
//!
//! Built from psx-spx "DMA Channels". Seven channels move blocks between main
//! RAM and a device without CPU involvement:
//!
//! | ch | base         | device  | direction          |
//! |----|--------------|---------|--------------------|
//! | 0  | 0x1F80_1080  | MDECin  | RAM → MDEC         |
//! | 1  | 0x1F80_1090  | MDECout | MDEC → RAM         |
//! | 2  | 0x1F80_10A0  | GPU     | lists + image data |
//! | 3  | 0x1F80_10B0  | CDROM   | CDROM → RAM        |
//! | 4  | 0x1F80_10C0  | SPU     | sound data         |
//! | 5  | 0x1F80_10D0  | PIO     | expansion port     |
//! | 6  | 0x1F80_10E0  | OTC     | reverse-clear OT   |
//!
//! Each channel is three registers: `MADR` (+0, base address), `BCR` (+4, block
//! control), `CHCR` (+8, channel control). Two shared registers follow at
//! 0x1F80_10F0 (`DPCR`, per-channel priority/enable) and 0x1F80_10F4 (`DICR`,
//! interrupt enable/flags). `off` everywhere is relative to the DMA window base
//! 0x1F80_1080.
//!
//! ## Borrow pattern (the "DMA touches memory" decision)
//!
//! A transfer reads/writes **both** main RAM (owned by [`crate::memory::Mem`])
//! **and** a device (GPU/SPU/CDROM/MDEC, each its own field on [`crate::psx::Psx`]).
//! Per the contract, `Dma` owns **only its channel registers** — it never holds
//! a reference to `Mem` or to a device. Instead, when software sets a channel's
//! CHCR start bit, [`Dma::take_pending`] returns a plain-data [`Transfer`]
//! descriptor (channel, direction, sync mode, address, word count/step). The
//! **orchestrator** ([`crate::psx::Psx`]) owns the run loop: it split-borrows
//! `mem` and the one target device and shuttles words between them with
//! `bus`-level reads/writes, then calls [`Dma::complete`] to clear the busy bit
//! and (if armed) latch the channel's `DICR` interrupt. This keeps the cyclic
//! Mem↔device aliasing out of `Dma` entirely — no `Rc`/`RefCell`, just an owned
//! descriptor handed across the borrow boundary.

use crate::irq::{Interrupt, Irq};

/// The seven DMA channels, identified by their hardware number / device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Channel {
    MdecIn = 0,
    MdecOut = 1,
    Gpu = 2,
    Cdrom = 3,
    Spu = 4,
    Pio = 5,
    Otc = 6,
}

impl Channel {
    /// Channel from its index 0..6.
    #[inline]
    pub fn from_index(i: u32) -> Option<Channel> {
        Some(match i {
            0 => Channel::MdecIn,
            1 => Channel::MdecOut,
            2 => Channel::Gpu,
            3 => Channel::Cdrom,
            4 => Channel::Spu,
            5 => Channel::Pio,
            6 => Channel::Otc,
            _ => return None,
        })
    }
}

/// CHCR sync mode (bits 9..10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Burst — transfer all words once DREQ is asserted (OTC, CDROM).
    Burst,
    /// Slice — one block per DREQ (MDEC, SPU, GPU VRAM image).
    Slice,
    /// Linked-list — walk a chain of headers in RAM (GPU command lists).
    LinkedList,
    /// Reserved (mode 3).
    Reserved,
}

impl SyncMode {
    #[inline]
    fn from_chcr(chcr: u32) -> SyncMode {
        match (chcr >> 9) & 3 {
            0 => SyncMode::Burst,
            1 => SyncMode::Slice,
            2 => SyncMode::LinkedList,
            _ => SyncMode::Reserved,
        }
    }
}

/// Direction of a transfer (CHCR bit 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Device → RAM (CHCR bit 0 = 0).
    ToRam,
    /// RAM → device (CHCR bit 0 = 1).
    FromRam,
}

/// A pending transfer, handed to the orchestrator to actually move the words.
/// Pure data — holds no references, so it crosses the Mem↔device borrow split
/// cleanly (see the module-level borrow-pattern note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    pub channel: Channel,
    pub direction: Direction,
    pub sync: SyncMode,
    /// Start address in RAM (MADR, 24-bit, word-aligned).
    pub base: u32,
    /// `+4` (forward) or `-4` (backward) per CHCR bit 1.
    pub step: i32,
    /// Word count for burst/slice modes (linked-list ignores this and walks the
    /// header chain instead).
    pub words: u32,
}

/// One DMA channel's three registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelRegs {
    /// `MADR` — base address (bits 0..23 used).
    pub madr: u32,
    /// `BCR` — block control (block size / block amount, or word count).
    pub bcr: u32,
    /// `CHCR` — channel control (direction, step, sync, start/busy).
    pub chcr: u32,
}

impl ChannelRegs {
    /// Bit 24 of CHCR — start/busy. Bit 28 — force-start (for sync mode 0,
    /// which has no DREQ). A transfer is "armed" when start is set and either
    /// the channel is not burst or it is force-started.
    #[inline]
    fn armed(&self) -> bool {
        const START: u32 = 1 << 24;
        const TRIGGER: u32 = 1 << 28;
        if self.chcr & START == 0 {
            return false;
        }
        match SyncMode::from_chcr(self.chcr) {
            // Burst needs the manual trigger bit; the others run on DREQ.
            SyncMode::Burst => self.chcr & TRIGGER != 0,
            _ => true,
        }
    }
}

/// The DMA controller register file.
#[derive(Debug, Clone, Default)]
pub struct Dma {
    /// Per-channel registers, indexed by channel number 0..6.
    pub channels: [ChannelRegs; 7],
    /// `DPCR` (0x1F80_10F0) — per-channel priority + master-enable nibbles.
    /// Reset value 0x0765_4321 (psx-spx).
    pub dpcr: u32,
    /// `DICR` (0x1F80_10F4) — interrupt enables (bits 16..22), master enable
    /// (bit 23), per-channel flags (bits 24..30), master flag (bit 31).
    pub dicr: u32,
}

impl Dma {
    pub fn new() -> Self {
        Dma {
            channels: [ChannelRegs::default(); 7],
            dpcr: 0x0765_4321,
            dicr: 0,
        }
    }

    /// Read a DMA register. `off` is relative to the DMA window base
    /// 0x1F80_1080. Channels occupy 0x00..0x6F (3 words each), then `DPCR` at
    /// 0x70 and `DICR` at 0x74.
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x70 => self.dpcr,
            0x74 => self.dicr,
            _ => {
                let ch = (off >> 4) as usize;
                if ch >= 7 {
                    return 0;
                }
                let regs = &self.channels[ch];
                match off & 0xF {
                    0x0 => regs.madr,
                    0x4 => regs.bcr,
                    0x8 => regs.chcr,
                    _ => 0,
                }
            }
        }
    }

    /// Write a DMA register. Writing a channel's CHCR may start a transfer (see
    /// [`Dma::take_pending`]). `off` is relative to 0x1F80_1080.
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x70 => self.dpcr = v,
            0x74 => self.write_dicr(v),
            _ => {
                let ch = (off >> 4) as usize;
                if ch >= 7 {
                    return;
                }
                let regs = &mut self.channels[ch];
                match off & 0xF {
                    0x0 => regs.madr = v & 0x00FF_FFFF,
                    0x4 => regs.bcr = v,
                    0x8 => regs.chcr = v,
                    _ => {}
                }
            }
        }
    }

    /// Apply a `DICR` write. Bits 0..15 and 16..23 are writable; the per-channel
    /// flag bits (24..30) are write-1-to-clear (acknowledge); the master flag
    /// (31) is recomputed (psx-spx).
    fn write_dicr(&mut self, v: u32) {
        const FLAGS: u32 = 0x7F << 24;
        let keep = self.dicr & FLAGS & !v; // acknowledge written-1 flags
        self.dicr = (v & 0x00FF_FFFF) | keep;
        self.update_master_flag();
    }

    /// Recompute the `DICR` master flag (bit 31) per psx-spx:
    ///
    /// ```text
    /// b31 = b15 OR (b23 AND (b24..30 != 0))
    /// ```
    ///
    /// i.e. the bus-error flag (bit 15) forces it, otherwise it is set when the
    /// master interrupt enable (bit 23) is on and *any* per-channel completion
    /// flag (bits 24..30) is latched. The per-channel enable masks (bits 16..22)
    /// only gate whether [`Dma::complete`] *sets* a flag in the first place;
    /// they do **not** participate in this OR.
    fn update_master_flag(&mut self) {
        const MASTER_ENABLE: u32 = 1 << 23;
        let force = self.dicr & (1 << 15) != 0; // bus-error always flags
        let flags = (self.dicr >> 24) & 0x7F;
        let any = flags != 0 && (self.dicr & MASTER_ENABLE != 0);
        if force || any {
            self.dicr |= 1 << 31;
        } else {
            self.dicr &= !(1 << 31);
        }
    }

    /// Return the first channel whose CHCR is armed, as a [`Transfer`]
    /// descriptor for the orchestrator to execute — or `None` if no channel is
    /// ready. Decodes direction / sync / step / word-count from the registers.
    /// Does not clear the busy bit; that happens in [`Dma::complete`].
    pub fn take_pending(&self) -> Option<(usize, Transfer)> {
        for ch in 0..7 {
            let regs = &self.channels[ch];
            if !regs.armed() {
                continue;
            }
            let channel = Channel::from_index(ch as u32)?;
            let chcr = regs.chcr;
            let direction = if chcr & 1 != 0 {
                Direction::FromRam
            } else {
                Direction::ToRam
            };
            let step = if chcr & 2 != 0 { -4 } else { 4 };
            let sync = SyncMode::from_chcr(chcr);
            let words = match sync {
                // OTC / CDROM: BCR low 16 bits = word count (0 ⇒ 0x10000).
                SyncMode::Burst => {
                    let bc = regs.bcr & 0xFFFF;
                    if bc == 0 {
                        0x1_0000
                    } else {
                        bc
                    }
                }
                // Slice: blocksize * blockamount.
                SyncMode::Slice => {
                    let bs = regs.bcr & 0xFFFF;
                    let ba = (regs.bcr >> 16) & 0xFFFF;
                    bs * ba
                }
                // Linked-list walks headers; word count comes from the chain.
                SyncMode::LinkedList | SyncMode::Reserved => 0,
            };
            return Some((
                ch,
                Transfer {
                    channel,
                    direction,
                    sync,
                    base: regs.madr & 0x00FF_FFFF,
                    step,
                    words,
                },
            ));
        }
        None
    }

    /// Mark channel `ch`'s transfer finished: clear CHCR start/busy (bits 24/28)
    /// and, if the channel's `DICR` interrupt is enabled, latch its completion
    /// flag and (when that newly raises the master flag) signal the IRQ
    /// controller. Called by the orchestrator after it moves the words.
    pub fn complete(&mut self, ch: usize, irq: &mut Irq) {
        if ch >= 7 {
            return;
        }
        const START: u32 = 1 << 24;
        const TRIGGER: u32 = 1 << 28;
        self.channels[ch].chcr &= !(START | TRIGGER);

        let enable_bit = 1u32 << (16 + ch);
        if self.dicr & enable_bit != 0 {
            self.dicr |= 1u32 << (24 + ch); // latch completion flag
        }
        let was_master = self.dicr & (1 << 31) != 0;
        self.update_master_flag();
        let now_master = self.dicr & (1 << 31) != 0;
        // Rising edge of the master flag drives the single DMA IRQ line.
        if !was_master && now_master {
            irq.raise(Interrupt::Dma);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpcr_reset_value() {
        assert_eq!(Dma::new().dpcr, 0x0765_4321);
    }

    #[test]
    fn channel_register_addressing() {
        let mut dma = Dma::new();
        // GPU channel (2) MADR at off 0x20.
        dma.write(0x20, 0x0010_0000);
        assert_eq!(dma.read(0x20), 0x0010_0000);
        // OTC channel (6) CHCR at off 0x68.
        dma.write(0x68, 0x1100_0002);
        assert_eq!(dma.read(0x68), 0x1100_0002);
    }

    #[test]
    fn otc_burst_transfer_decodes() {
        let mut dma = Dma::new();
        // OTC: MADR, BCR word count, CHCR = reverse-clear (dir to RAM, step -4,
        // burst, start+trigger).
        dma.write(0x60, 0x0010_0000); // MADR
        dma.write(0x64, 0x0000_0004); // BCR = 4 words
        dma.write(0x68, 0x1100_0002); // CHCR start+trigger, step -4
        let (ch, t) = dma.take_pending().expect("armed");
        assert_eq!(ch, 6);
        assert_eq!(t.channel, Channel::Otc);
        assert_eq!(t.direction, Direction::ToRam);
        assert_eq!(t.step, -4);
        assert_eq!(t.words, 4);
    }

    #[test]
    fn complete_clears_busy_and_flags_irq() {
        let mut dma = Dma::new();
        let mut irq = Irq::new();
        // Arm GPU channel and enable its DICR interrupt + master enable.
        dma.write(0x28, (1 << 24) | (1 << 28)); // CHCR start+trigger (burst)
        dma.write(0x74, (1 << 23) | (1 << (16 + 2))); // master enable + ch2 enable
        dma.complete(2, &mut irq);
        assert_eq!(dma.channels[2].chcr & (1 << 24), 0, "busy cleared");
        assert_ne!(dma.dicr & (1 << (24 + 2)), 0, "completion flag latched");
        assert_ne!(irq.stat & Interrupt::Dma.bit(), 0, "DMA IRQ raised");
    }

    #[test]
    fn slice_mode_decodes_blocksize_times_blockamount() {
        let mut dma = Dma::new();
        // SPU channel (4): BCR = blocksize 0x10, blockamount 0x20 -> 0x200 words.
        dma.write(0x40, 0x0008_0000); // MADR
        dma.write(0x44, (0x0020 << 16) | 0x0010); // BCR: BA=0x20, BS=0x10
        // CHCR: dir FromRam (bit0), step +4, sync=Slice (bits9..10 = 1), start.
        dma.write(0x48, (1 << 24) | (1 << 9) | 1);
        let (ch, t) = dma.take_pending().expect("slice armed without trigger");
        assert_eq!(ch, 4);
        assert_eq!(t.channel, Channel::Spu);
        assert_eq!(t.sync, SyncMode::Slice);
        assert_eq!(t.direction, Direction::FromRam);
        assert_eq!(t.step, 4);
        assert_eq!(t.words, 0x10 * 0x20);
    }

    #[test]
    fn linked_list_decodes_with_zero_word_count() {
        let mut dma = Dma::new();
        // GPU channel (2) linked-list: dir FromRam, sync=LinkedList (bits9..10=2).
        dma.write(0x20, 0x0012_3450); // MADR (chain head)
        dma.write(0x28, (1 << 24) | (2 << 9) | 1);
        let (ch, t) = dma.take_pending().expect("linked-list armed without trigger");
        assert_eq!(ch, 2);
        assert_eq!(t.sync, SyncMode::LinkedList);
        assert_eq!(t.base, 0x0012_3450);
        assert_eq!(t.words, 0, "word count comes from the chain, not BCR");
    }

    #[test]
    fn burst_not_armed_without_trigger_bit() {
        let mut dma = Dma::new();
        // OTC burst with start set but no force-trigger (bit 28) -> idle.
        dma.write(0x64, 0x0000_0004);
        dma.write(0x68, (1 << 24) | 0x0000_0002); // start, no trigger
        assert!(
            dma.take_pending().is_none(),
            "burst waits for the force-trigger / DREQ"
        );
        // Setting the trigger arms it.
        dma.write(0x68, (1 << 24) | (1 << 28) | 0x0000_0002);
        assert!(dma.take_pending().is_some());
    }

    #[test]
    fn madr_masks_to_24_bits() {
        let mut dma = Dma::new();
        dma.write(0x00, 0xFF12_3456);
        assert_eq!(dma.read(0x00), 0x0012_3456, "high byte of MADR is dropped");
    }

    #[test]
    fn dicr_acknowledge_clears_flag_and_lowers_master() {
        let mut dma = Dma::new();
        let mut irq = Irq::new();
        // Enable ch2 + master, complete it -> flag + master flag set.
        dma.write(0x28, (1 << 24) | (1 << 28));
        dma.write(0x74, (1 << 23) | (1 << (16 + 2)));
        dma.complete(2, &mut irq);
        assert_ne!(dma.dicr & (1 << 31), 0, "master flag set after completion");

        // Acknowledge: keep master enable + ch2 enable, write 1 to flag bit 26.
        dma.write(0x74, (1 << 23) | (1 << (16 + 2)) | (1 << (24 + 2)));
        assert_eq!(dma.dicr & (1 << (24 + 2)), 0, "completion flag acknowledged");
        assert_eq!(dma.dicr & (1 << 31), 0, "master flag recomputed low");
    }

    #[test]
    fn master_flag_ignores_per_channel_enable_mask() {
        // Spec: b31 = b15 OR (b23 AND any flag 24..30). The enable mask (16..22)
        // does NOT gate bit 31 — it only gates whether complete() sets the flag.
        let mut dma = Dma::new();
        // Manually latch a flag with master enable but NO matching enable mask.
        dma.dicr = (1 << 23) | (1 << (24 + 0)); // master enable + ch0 flag
        dma.update_master_flag();
        assert_ne!(dma.dicr & (1 << 31), 0, "any flag + master enable -> b31");

        // Bus-error flag (bit 15) forces the master flag regardless of enable.
        let mut dma = Dma::new();
        dma.dicr = 1 << 15;
        dma.update_master_flag();
        assert_ne!(dma.dicr & (1 << 31), 0, "bus-error forces b31");
    }

    #[test]
    fn second_completion_does_not_redundantly_raise_irq() {
        // The DMA IRQ fires only on the *rising* edge of the master flag.
        let mut dma = Dma::new();
        let mut irq = Irq::new();
        dma.write(0x74, (1 << 23) | (1 << (16 + 2)) | (1 << (16 + 4)));
        dma.write(0x28, (1 << 24) | (1 << 28));
        dma.complete(2, &mut irq);
        assert_ne!(irq.stat & Interrupt::Dma.bit(), 0);
        // Ack the I_STAT bit; a *second* completion while master flag already
        // high must not re-raise (no rising edge).
        irq.write(0x0, !Interrupt::Dma.bit());
        dma.write(0x48, (1 << 24) | (1 << 28));
        dma.complete(4, &mut irq);
        assert_eq!(
            irq.stat & Interrupt::Dma.bit(),
            0,
            "no rising edge -> no new IRQ latch"
        );
    }
}
