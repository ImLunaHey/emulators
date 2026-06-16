//! CD-ROM controller.
//!
//! Built from psx-spx "CDROM Drive". The drive is a small command/response
//! engine behind four byte-wide ports at 0x1F80_1800..0x1F80_1803 (`off` here is
//! relative to 0x1F80_1800). The meaning of ports 1..3 depends on the index
//! field (`INDEX`, low 2 bits of port 0):
//!
//! | off | read                | write (by index)                       |
//! |-----|---------------------|----------------------------------------|
//! | 0   | status (INDEX/FIFO) | INDEX select                           |
//! | 1   | response FIFO       | 0:command 1:sound-map data 2/3:vol     |
//! | 2   | data FIFO           | 0:param FIFO 1:IRQ enable 2/3:vol      |
//! | 3   | IRQ flags           | 0:request 1:IRQ flags/ack 2/3:vol/apply|
//!
//! The drive runs a tiny state machine: a host write to the command register
//! latches the command + the parameter FIFO; after a fixed delay [`Cdrom::step`]
//! delivers responses by pushing bytes into the response FIFO, setting the
//! 3-bit IRQ type (INT1..INT5) in `irq_flags`, and (if enabled in `irq_enable`)
//! latching the CD-ROM bit in the shared [`Irq`] controller. The host
//! acknowledges by writing 1-bits to port 3 (index 1); only then does the drive
//! deliver the *next* queued response (e.g. the second INT for ReadN/Init/etc.).
//!
//! Sector data is read from a MODE2/2352 `.bin` disc image (2352 bytes/sector).
//! `Setmode` bit 5 picks the data-FIFO payload: 0x800 bytes (data-only, offset
//! 24 into the raw sector) or the whole 0x924-byte sector (offset 12, skipping
//! the 12-byte sync). See psx-spx "CDROM File Formats / Disk Format".

use crate::irq::{Interrupt, Irq};

/// Raw bytes per sector in a MODE2/2352 image.
const SECTOR_RAW: usize = 2352;
/// Sectors-to-LBA offset: the lead-in occupies the first 2 seconds (150 frames).
const LBA_OFFSET: u32 = 150;
/// FIFO depth for the parameter / response FIFOs (psx-spx: 16 bytes).
const FIFO_CAP: usize = 16;

/// Cycles (at the 33.8688 MHz CPU clock) before a freshly-issued command
/// produces its first acknowledge (INT3). psx-spx gives ~0xC4E1 (~50k) for most
/// commands; the exact value is not observable by software that waits on the
/// IRQ, so we use a round approximation.
const ACK_DELAY: u32 = 50_000;
/// Cycles between the INT3 acknowledge and a follow-up INT (INT2/INT5/INT1).
const SECOND_DELAY: u32 = 50_000;
/// Single-speed read rate: 75 sectors/second at 33.8688 MHz ≈ 451584 cycles.
/// Init (0x0A) completion: drive motor spin-up, ~118 ms (per DuckStation's
/// hardware-derived `INIT_TICKS`). Far longer than a normal second response.
const INIT_TICKS: u32 = 4_000_000;
/// SeekL/SeekP completion: head-move time. A modest fixed approximation (real
/// hardware scales with seek distance; this is enough for boot sequencing).
const SEEK_TICKS: u32 = 200_000;

const READ_DELAY_1X: u32 = 451_584;
/// Double-speed read rate (Setmode bit 7).
const READ_DELAY_2X: u32 = READ_DELAY_1X / 2;

/// IRQ response type encoded in the low 3 bits of `irq_flags` (HINTSTS).
mod int {
    /// New data sector (or report packet) ready.
    pub const DATA: u8 = 1;
    /// Second/“complete” response.
    pub const COMPLETE: u8 = 2;
    /// First/“acknowledge” response.
    pub const ACK: u8 = 3;
    /// End of disc/track.
    pub const END: u8 = 4;
    /// Error / disk error.
    pub const ERROR: u8 = 5;
}

/// `stat` byte bits (the value returned by Getstat and most commands).
mod stat {
    pub const MOTOR: u8 = 1 << 1;
    pub const SEEK: u8 = 1 << 6;
    pub const READ: u8 = 1 << 5;
    // pub const PLAY: u8 = 1 << 7; // CD-DA playback (not modelled)
    // pub const SHELL_OPEN: u8 = 1 << 4; // drive door (not modelled)
}

/// A pending drive action, run by [`Cdrom::step`] once its delay elapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Deliver the first acknowledge (INT3) of `cmd`.
    Ack(u8),
    /// Deliver the second/complete response (INT2) of `cmd`.
    Second(u8),
    /// Read the next data sector and deliver INT1.
    Read,
}

/// CD-ROM controller register/FIFO state.
#[derive(Debug, Clone, Default)]
pub struct Cdrom {
    /// `INDEX` — the bank select (low 2 bits of port 0) that re-maps ports 1..3.
    pub index: u8,
    /// Interrupt-enable register (port 2, index 1) — low 3 bits gate INT1..5.
    pub irq_enable: u8,
    /// Latched interrupt flags (port 3, index 1) — the pending response type in
    /// the low 3 bits.
    pub irq_flags: u8,

    /// Parameter FIFO: host-written command arguments (front = oldest).
    params: Vec<u8>,
    /// Response FIFO: drive-written response bytes (front = oldest).
    response: Vec<u8>,
    /// Data FIFO: the active sector's payload, indexed by `data_pos`.
    data: Vec<u8>,
    /// Read cursor into `data`; padding repeats past the end (psx-spx).
    data_pos: usize,
    /// True once the host has requested the sector into the data FIFO (port 3
    /// bit 7, "Want Data"). DRQ (status bit 6) only asserts after this — the
    /// freshly-read sector sits in the controller's buffer until requested.
    data_ready: bool,

    /// `Setmode` register (bit 5 = sector size, bit 7 = double speed, …).
    mode: u8,
    /// Drive `stat` byte (motor/seek/read/shell bits).
    drive_stat: u8,
    /// Target LBA latched by `Setloc` (sector index from the start of data).
    seek_lba: u32,
    /// LBA of the next sector `ReadN`/`ReadS` will deliver.
    read_lba: u32,

    /// The queued action waiting on `delay`, and the cycle countdown to it.
    pending: Option<Pending>,
    delay: u32,
    /// Set while `ReadN`/`ReadS` is active so each acknowledged INT1 re-arms.
    reading: bool,

    /// The mounted disc image (raw MODE2/2352 bytes). Empty = no disc.
    disc: Vec<u8>,

    /// Set when a command consumes the parameter FIFO; the next parameter push
    /// then clears the (now-stale) FIFO before adding, so each command sees only
    /// its own parameters.
    params_consumed: bool,
}

impl Cdrom {
    pub fn new() -> Self {
        Cdrom {
            // Motor spun up by default so Getstat looks like a ready drive once
            // a disc is present.
            drive_stat: stat::MOTOR,
            ..Cdrom::default()
        }
    }

    /// Mount a `.bin` disc image (MODE2/2352). Takes ownership of the bytes so a
    /// multi-hundred-MB image is moved, not copied. Replaces any previous disc.
    pub fn load_disc(&mut self, bytes: Vec<u8>) {
        self.disc = bytes;
        self.drive_stat = stat::MOTOR;
    }

    /// True if a disc image is mounted.
    pub fn has_disc(&self) -> bool {
        !self.disc.is_empty()
    }

    /// The 2048-byte MODE2/form1 user data of sector `lba` (offset 24 into the
    /// raw 2352-byte sector), or `None` past the end. Used by the HLE disc boot
    /// to read the ISO9660 filesystem directly.
    pub fn sector_user_data(&self, lba: u32) -> Option<&[u8]> {
        let base = lba as usize * SECTOR_RAW + 24;
        self.disc.get(base..base + 2048)
    }

    /// Debug: LBA of the next sector a read would deliver (boot-progress probe).
    pub fn debug_read_lba(&self) -> u32 {
        self.read_lba
    }

    /// Debug: whether a ReadN/ReadS stream is currently active.
    pub fn debug_reading(&self) -> bool {
        self.reading
    }

    /// Number of whole 2352-byte sectors in the mounted image.
    fn sector_count(&self) -> u32 {
        (self.disc.len() / SECTOR_RAW) as u32
    }

    /// The status byte (port 0): low 2 bits = `INDEX`, plus FIFO-state flags:
    /// bit3 PRMEMPT (param FIFO empty), bit4 PRMWRDY (param FIFO not full),
    /// bit5 RSLRRDY (response FIFO not empty), bit6 DRQSTS (data FIFO has data),
    /// bit7 BUSYSTS (busy acknowledging a command).
    fn status(&self) -> u8 {
        let mut s = self.index & 3;
        if self.params.is_empty() {
            s |= 0x08; // PRMEMPT
        }
        if self.params.len() < FIFO_CAP {
            s |= 0x10; // PRMWRDY
        }
        if !self.response.is_empty() {
            s |= 0x20; // RSLRRDY
        }
        if self.data_ready && self.data_pos < self.data.len() {
            s |= 0x40; // DRQSTS — only after the host requests the sector (BFRD)
        }
        // BUSYSTS (bit7): busy while a command's INT3 is still pending.
        if matches!(self.pending, Some(Pending::Ack(_))) {
            s |= 0x80;
        }
        s
    }

    /// Pop one byte from the response FIFO (returns 0 when empty).
    fn pop_response(&mut self) -> u8 {
        if self.response.is_empty() {
            0
        } else {
            self.response.remove(0)
        }
    }

    /// Read one byte from the data FIFO. Once the sector is exhausted the
    /// hardware repeats a padding byte near the end (psx-spx); we repeat the
    /// last valid byte, which is sufficient for DMA word-aligned reads.
    fn read_data_byte(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = if self.data_pos < self.data.len() {
            self.data[self.data_pos]
        } else {
            *self.data.last().unwrap()
        };
        self.data_pos += 1;
        b
    }

    /// Read a CD-ROM port (8-bit). `off` is relative to 0x1F80_1800.
    pub fn read(&mut self, off: u32) -> u32 {
        let v = match off & 3 {
            0 => self.status(),
            // Response FIFO (RESULT) — same in every bank.
            1 => self.pop_response(),
            // Data FIFO (RDDATA).
            2 => self.read_data_byte(),
            // IRQ flags (HINTMSK in bank0/2, HINTSTS in bank1/3). High 3 bits
            // read as 1; low bits hold the enable mask or the pending INT type.
            _ => match self.index & 1 {
                0 => self.irq_enable | 0xE0,
                _ => self.irq_flags | 0xE0,
            },
        };
        v as u32
    }

    /// Write a CD-ROM port (8-bit). `off` is relative to 0x1F80_1800. Behavior
    /// depends on the current `INDEX` (psx-spx bank table).
    pub fn write(&mut self, off: u32, v: u32) {
        let v = v as u8;
        match (off & 3, self.index & 3) {
            // Port 0 (any bank): INDEX/ADDRESS select.
            (0, _) => self.index = v & 3,

            // Port 1, bank 0: COMMAND register.
            (1, 0) => self.queue_command(v),
            // Port 1, bank 1/2/3: sound-map / volume — not modelled.
            (1, _) => {}

            // Port 2, bank 0: PARAMETER FIFO push.
            (2, 0) => {
                // First param after a command starts a fresh set (the prior
                // command consumed the FIFO).
                if self.params_consumed {
                    self.params.clear();
                    self.params_consumed = false;
                }
                if self.params.len() < FIFO_CAP {
                    self.params.push(v);
                }
            }
            // Port 2, bank 1: interrupt-enable (HINTMSK).
            (2, 1) => self.irq_enable = v & 0x1F,
            // Port 2, bank 2/3: volume — not modelled.
            (2, _) => {}

            // Port 3, bank 0: HCHPCTL request register. Bit7 (BFRD) loads the
            // data FIFO from the sector buffer; bit0..6 toggle XA/sound-map
            // (not modelled).
            (3, 0) => {
                if v & 0x80 != 0 {
                    // BFRD set: load the data FIFO from the sector buffer, i.e.
                    // make the current sector readable from the start. DRQ now
                    // asserts (the host polls it / starts the CD DMA).
                    self.data_pos = 0;
                    self.data_ready = true;
                } else {
                    // BFRD cleared: reset/empty the data FIFO; DRQ deasserts.
                    self.data_pos = self.data.len();
                    self.data_ready = false;
                }
            }
            // Port 3, bank 1: HCLRCTL — acknowledge IRQ flags / clear FIFOs.
            (3, 1) => {
                // Bits 0..4 written as 1 acknowledge the corresponding INT flags.
                self.irq_flags &= !(v & 0x1F);
                // Bit6: clear parameter FIFO.
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                // Acknowledging an INT clears the gate in `step`, letting the
                // next queued response (second INT, or next read sector) fire.
            }
            // Port 3, bank 2/3: volume apply — not modelled.
            (3, _) => {}
            // `off & 3` is in 0..=3; the four port arms above are exhaustive.
            _ => unreachable!(),
        }
    }

    /// Latch a command: drain the parameter FIFO into the interpreter and arm
    /// the acknowledge delay.
    fn queue_command(&mut self, cmd: u8) {
        // Arm the first response after the standard acknowledge delay. The
        // actual side effects (Setloc/Setmode/etc.) are applied immediately so
        // a follow-up command sees them; the response timing is what the BIOS
        // waits on.
        self.apply_command_side_effects(cmd);
        // Mark the parameter FIFO consumed: the command's response may still read
        // it (e.g. Test 0x19), so we don't clear now — instead the next param
        // push starts a fresh set. Without this, a later command inherits stale
        // leading bytes (e.g. misaligning Setloc's mm/ss/ff → wrong LBA).
        self.params_consumed = true;
        self.pending = Some(Pending::Ack(cmd));
        self.delay = ACK_DELAY;
    }

    /// Apply the immediate register side effects of a command (the parts that
    /// don't depend on response timing). Reads the parameter FIFO in place.
    fn apply_command_side_effects(&mut self, cmd: u8) {
        match cmd {
            // Setloc: 3 BCD params mm,ss,ff -> target LBA.
            0x02 => {
                let mm = bcd(self.params.first().copied().unwrap_or(0));
                let ss = bcd(self.params.get(1).copied().unwrap_or(0));
                let ff = bcd(self.params.get(2).copied().unwrap_or(0));
                let amm = mm as u32;
                let ass = ss as u32;
                let aff = ff as u32;
                let abs = (amm * 60 + ass) * 75 + aff;
                self.seek_lba = abs.saturating_sub(LBA_OFFSET);
            }
            // Setmode.
            0x0E => self.mode = self.params.first().copied().unwrap_or(0),
            // SeekL / SeekP: latch the read position at the seek target.
            0x15 | 0x16 => self.read_lba = self.seek_lba,
            // ReadN / ReadS: begin reading from the seek target.
            0x06 | 0x1B => {
                self.read_lba = self.seek_lba;
                self.reading = true;
            }
            // Pause / Stop / Init: stop any active read.
            0x08 | 0x09 | 0x0A => self.reading = false,
            _ => {}
        }
    }

    /// Latch the given INT type into `irq_flags` and, if it is unmasked in
    /// `irq_enable` (HINTMSK low 3 bits), assert the shared CD-ROM line. psx-spx
    /// gates the line on `(irq_flags & irq_enable & 7) != 0`.
    fn deliver(&mut self, int_type: u8, irq: &mut Irq) {
        self.irq_flags = (self.irq_flags & !0x07) | (int_type & 0x07);
        if self.irq_flags & self.irq_enable & 0x07 != 0 {
            irq.raise(Interrupt::Cdrom);
        }
    }

    /// Build and queue the acknowledge (INT3) response for `cmd`, returning the
    /// follow-up action (if any) that fires after the host acks.
    fn run_ack(&mut self, cmd: u8) -> Option<Pending> {
        self.response.clear();
        match cmd {
            // Getstat / Nop.
            0x01 => {
                self.push_stat();
                None
            }
            // Setloc.
            0x02 => {
                self.push_stat();
                None
            }
            // ReadN / ReadS: ack now, then stream INT1 sectors.
            0x06 | 0x1B => {
                self.drive_stat = (self.drive_stat & !stat::SEEK) | stat::MOTOR | stat::READ;
                self.push_stat();
                Some(Pending::Read)
            }
            // Stop.
            0x08 => {
                self.drive_stat = stat::MOTOR;
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // Pause.
            0x09 => {
                self.drive_stat &= !stat::READ;
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // Init: reset mode, spin up, INT3 then INT2.
            0x0A => {
                self.mode = 0;
                self.drive_stat = stat::MOTOR;
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // Setmode.
            0x0E => {
                self.push_stat();
                None
            }
            // Mute / Demute / Setfilter: audio-path controls we don't model in
            // detail, but they are routinely issued during a game's CD init
            // sequence. Acknowledge with stat (INT3) — returning the unknown-
            // command error (INT5) made games (e.g. Tony Hawk) treat Init as
            // failed and retry it forever.
            0x0B | 0x0C | 0x0D => {
                self.push_stat();
                None
            }
            // Getparam: stat, mode, 0, file, channel.
            0x0F => {
                let st = self.drive_stat;
                self.response.extend_from_slice(&[st, self.mode, 0x00, 0x00, 0x00]);
                None
            }
            // GetlocL: header (amm,ass,aff,mode) + subheader (file,chan,sm,ci) of
            // the most recently read sector, all BCD for the MSF. libcd reads this
            // to confirm a read landed at the requested position; returning the
            // unknown-command error (INT5) made position-verifying reads fail.
            0x10 => {
                let abs = self.read_lba.wrapping_add(LBA_OFFSET);
                let (mm, ss, ff) = lba_to_msf(abs);
                self.response.extend_from_slice(&[
                    to_bcd(mm), to_bcd(ss), to_bcd(ff), self.mode, 0x00, 0x00, 0x08, 0x00,
                ]);
                None
            }
            // GetlocP: track, index, relative MSF (to track start), absolute MSF
            // (all BCD). Single data track ⇒ track 1; relative LBA == read_lba.
            0x11 => {
                let abs = self.read_lba.wrapping_add(LBA_OFFSET);
                let (amm, ass, aff) = lba_to_msf(abs);
                let (rmm, rss, rff) = lba_to_msf(self.read_lba);
                self.response.extend_from_slice(&[
                    0x01, 0x01, to_bcd(rmm), to_bcd(rss), to_bcd(rff),
                    to_bcd(amm), to_bcd(ass), to_bcd(aff),
                ]);
                None
            }
            // GetTN: stat, first track, last track (BCD). One track on a .bin.
            0x13 => {
                let st = self.drive_stat;
                self.response.extend_from_slice(&[st, 0x01, to_bcd(1)]);
                None
            }
            // GetTD: stat, mm, ss of a track start (BCD). Track 0 = disc end.
            0x14 => {
                let track = self.params.first().copied().unwrap_or(0);
                let lba = if to_bin(track) == 0 {
                    self.sector_count() + LBA_OFFSET
                } else {
                    LBA_OFFSET
                };
                let (mm, ss, _) = lba_to_msf(lba);
                let st = self.drive_stat;
                self.response.extend_from_slice(&[st, to_bcd(mm), to_bcd(ss)]);
                None
            }
            // SeekL / SeekP: ack with the seek bit set, then INT2.
            0x15 | 0x16 => {
                self.drive_stat = (self.drive_stat & !stat::READ) | stat::MOTOR | stat::SEEK;
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // Test: subfunction in param[0]. 0x20 = CD-ROM BIOS date/version.
            0x19 => {
                match self.params.first().copied().unwrap_or(0) {
                    0x20 => self
                        .response
                        .extend_from_slice(&[0x94, 0x09, 0x19, 0xC0]),
                    _ => self.push_stat(),
                }
                None
            }
            // GetID: ack stat, then INT2 with the licence/region packet.
            0x1A => {
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // ReadTOC.
            0x1E => {
                self.push_stat();
                Some(Pending::Second(cmd))
            }
            // Unknown command: error response delivered as INT5 (stat.Error +
            // reason byte 0x40 = "invalid command"). The `step` dispatcher
            // promotes this to INT5 because the stat byte has bit0 set.
            _ => {
                self.response
                    .extend_from_slice(&[self.drive_stat | 0x01, 0x40]);
                None
            }
        }
    }

    /// Build the second/complete (INT2) response for `cmd`.
    fn run_second(&mut self, cmd: u8) {
        self.response.clear();
        match cmd {
            // GetID INT2: flags, 0, disk type (Mode2=0x20), atip, "SCEA".
            0x1A => {
                if self.has_disc() {
                    self.response
                        .extend_from_slice(&[0x02, 0x00, 0x20, 0x00, b'S', b'C', b'E', b'A']);
                } else {
                    // No disc: INT5 error packet (handled by caller's int type).
                    self.response.extend_from_slice(&[0x08, 0x40, 0x00, 0x00, 0, 0, 0, 0]);
                }
            }
            // Seek / Pause / Stop / Init / ReadTOC: plain stat.
            _ => self.push_stat(),
        }
    }

    /// Read the current sector from the disc image into the data FIFO and
    /// advance `read_lba`. Returns false at end-of-disc.
    fn load_sector(&mut self) -> bool {
        let idx = self.read_lba;
        if idx >= self.sector_count() {
            return false;
        }
        let base = idx as usize * SECTOR_RAW;
        // Setmode bit5: 0 => 0x800 data-only bytes (skip 12 sync + 4 header + 8
        // subheader = offset 24); 1 => whole 0x924 sector (skip 12 sync only).
        let (off, len) = if self.mode & 0x20 != 0 {
            (12usize, 0x924usize)
        } else {
            (24usize, 0x800usize)
        };
        let start = base + off;
        let end = (start + len).min(self.disc.len());
        self.data = self.disc[start..end].to_vec();
        // Pad up to the nominal length if the image was short.
        self.data.resize(len, 0);
        self.data_pos = 0;
        self.read_lba = self.read_lba.wrapping_add(1);
        true
    }

    /// Push the current drive stat byte into the response FIFO.
    fn push_stat(&mut self) {
        let s = self.drive_stat;
        self.response.push(s);
    }

    /// One read tick (cycles between INT1 sectors).
    fn read_delay(&self) -> u32 {
        if self.mode & 0x80 != 0 {
            READ_DELAY_2X
        } else {
            READ_DELAY_1X
        }
    }

    /// Advance the drive state machine. `cycles` CPU cycles elapsed since the
    /// last call; when a pending action's countdown reaches zero it is run,
    /// delivering responses + the CD-ROM IRQ.
    pub fn step(&mut self, cycles: u32, irq: &mut Irq) {
        // Don't deliver a new INT until the previous one has been acknowledged.
        if self.irq_flags & 0x07 != 0 {
            return;
        }
        let Some(pending) = self.pending else {
            return;
        };
        self.delay = self.delay.saturating_sub(cycles);
        if self.delay > 0 {
            return;
        }
        self.pending = None;

        match pending {
            Pending::Ack(cmd) => {
                let follow = self.run_ack(cmd);
                // Unknown commands deliver an error (INT5); GetID/no-disc too.
                let int_type = if matches!(self.response.first(), Some(s) if s & 0x01 != 0)
                    && cmd != 0x19
                {
                    int::ERROR
                } else {
                    int::ACK
                };
                self.deliver(int_type, irq);
                if let Some(f) = follow {
                    self.pending = Some(f);
                    self.delay = match f {
                        Pending::Read => self.read_delay(),
                        // Init's completion (INT2) is the drive motor spin-up:
                        // real hardware takes ~118 ms (~4M CPU cycles), not the
                        // ~50k of a normal second response. libcd's CdInit waits
                        // on this; delivering it ~80x too early can race its
                        // handler setup. SeekL/SeekP also take longer (head move).
                        Pending::Second(0x0A) => INIT_TICKS,
                        Pending::Second(0x15) | Pending::Second(0x16) => SEEK_TICKS,
                        _ => SECOND_DELAY,
                    };
                }
            }
            Pending::Second(cmd) => {
                self.run_second(cmd);
                // GetID with no disc reports an error.
                let int_type = if cmd == 0x1A && !self.has_disc() {
                    int::ERROR
                } else {
                    int::COMPLETE
                };
                self.deliver(int_type, irq);
            }
            Pending::Read => {
                if self.load_sector() {
                    self.response.clear();
                    self.push_stat();
                    self.deliver(int::DATA, irq);
                    // Re-arm the next sector; it fires after the host acks this
                    // one (gated by the irq_flags check at the top of `step`).
                    if self.reading {
                        self.pending = Some(Pending::Read);
                        self.delay = self.read_delay();
                    }
                } else {
                    // End of disc.
                    self.reading = false;
                    self.drive_stat &= !stat::READ;
                    self.response.clear();
                    self.push_stat();
                    self.deliver(int::END, irq);
                }
            }
        }
    }
}

/// Decode a BCD byte to binary (e.g. 0x59 -> 59).
#[inline]
fn bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

/// Alias kept for clarity at call sites that take a BCD byte.
#[inline]
fn to_bin(v: u8) -> u8 {
    bcd(v)
}

/// Encode a binary value (0..99) to BCD.
#[inline]
fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Convert an absolute LBA (including the 150-frame lead-in) to (mm, ss, ff).
#[inline]
fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let mm = (lba / 75 / 60) as u8;
    let ss = ((lba / 75) % 60) as u8;
    let ff = (lba % 75) as u8;
    (mm, ss, ff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic MODE2/2352 disc: `n` sectors whose data-only payload
    /// byte equals the sector index (mod 256).
    fn synth_disc(n: usize) -> Vec<u8> {
        let mut d = vec![0u8; n * SECTOR_RAW];
        for s in 0..n {
            let base = s * SECTOR_RAW;
            // 12 sync + 4 header + 8 subheader = 24, then 0x800 user data.
            for i in 0..0x800 {
                d[base + 24 + i] = (s as u8).wrapping_add(i as u8);
            }
        }
        d
    }

    /// Run the drive until it next delivers an IRQ (or `limit` steps elapse).
    fn run_until_irq(cd: &mut Cdrom, irq: &mut Irq) {
        for _ in 0..32 {
            cd.step(1_000_000, irq);
            if cd.irq_flags & 0x07 != 0 {
                return;
            }
        }
    }

    /// Acknowledge the current INT (host write to port3/index1 = 0x1F).
    fn ack(cd: &mut Cdrom) {
        cd.write(0, 1); // INDEX = 1
        cd.write(3, 0x1F);
        cd.write(0, 0); // back to INDEX 0
    }

    #[test]
    fn index_select_round_trips() {
        let mut cd = Cdrom::new();
        cd.write(0, 1);
        assert_eq!(cd.read(0) & 3, 1, "INDEX reflected in status");
    }

    #[test]
    fn irq_flags_acknowledge() {
        let mut cd = Cdrom::new();
        cd.irq_flags = 0x03;
        cd.write(0, 1); // INDEX = 1
        cd.write(3, 0x03); // ack low bits
        assert_eq!(cd.irq_flags & 0x07, 0);
    }

    #[test]
    fn param_fifo_flags_in_status() {
        let mut cd = Cdrom::new();
        // PRMEMPT set, PRMWRDY set initially.
        assert_ne!(cd.status() & 0x08, 0, "param FIFO empty");
        assert_ne!(cd.status() & 0x10, 0, "param FIFO not full");
        cd.write(2, 0x12); // push a param (bank 0)
        assert_eq!(cd.status() & 0x08, 0, "no longer empty");
    }

    #[test]
    fn getstat_acknowledges_with_int3() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.write(1, 0x01); // command Getstat (bank 0)
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ACK, "INT3 acknowledge");
        assert_ne!(irq.stat & Interrupt::Cdrom.bit(), 0, "CD-ROM IRQ raised");
        // Response FIFO holds the stat byte.
        assert_ne!(cd.read(0) & 0x20, 0, "RSLRRDY: response not empty");
        let s = cd.read(1) as u8;
        assert_eq!(s & stat::MOTOR, stat::MOTOR, "motor bit in stat");
    }

    #[test]
    fn test_20_returns_bios_version() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.write(2, 0x20); // param: subfunction 0x20
        cd.write(1, 0x19); // Test command
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ACK);
        assert_eq!(cd.read(1) as u8, 0x94);
        assert_eq!(cd.read(1) as u8, 0x09);
        assert_eq!(cd.read(1) as u8, 0x19);
        assert_eq!(cd.read(1) as u8, 0xC0);
    }

    #[test]
    fn getid_two_responses_for_disc() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.load_disc(synth_disc(4));
        cd.write(1, 0x1A); // GetID
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ACK, "first INT3");
        ack(&mut cd);
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::COMPLETE, "second INT2");
        // Region packet "SCEA" at the tail.
        let bytes: Vec<u8> = (0..8).map(|_| cd.read(1) as u8).collect();
        assert_eq!(&bytes[4..8], b"SCEA");
        assert_eq!(bytes[2], 0x20, "disk type Mode2");
    }

    #[test]
    fn getid_no_disc_is_error() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.write(1, 0x1A);
        run_until_irq(&mut cd, &mut irq);
        ack(&mut cd);
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ERROR, "INT5 with no disc");
    }

    #[test]
    fn setloc_bcd_to_lba() {
        let mut cd = Cdrom::new();
        // 00:02:00 BCD -> abs frame 150 -> data LBA 0.
        cd.write(2, 0x00);
        cd.write(2, 0x02);
        cd.write(2, 0x00);
        cd.apply_command_side_effects(0x02);
        assert_eq!(cd.seek_lba, 0);

        cd.params.clear();
        // 00:03:00 BCD -> abs 225 -> LBA 75.
        cd.write(2, 0x00);
        cd.write(2, 0x03);
        cd.write(2, 0x00);
        cd.apply_command_side_effects(0x02);
        assert_eq!(cd.seek_lba, 75);
    }

    #[test]
    fn read_streams_sectors_and_loads_data_fifo() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.load_disc(synth_disc(8));

        // Setloc to 00:02:00 (LBA 0).
        cd.write(2, 0x00);
        cd.write(2, 0x02);
        cd.write(2, 0x00);
        cd.write(1, 0x02);
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ACK);
        ack(&mut cd);

        // Setmode 0x00 (2048-byte data sectors).
        cd.params.clear();
        cd.write(2, 0x00);
        cd.write(1, 0x0E);
        run_until_irq(&mut cd, &mut irq);
        ack(&mut cd);

        // ReadN.
        cd.params.clear();
        cd.write(1, 0x06);
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::ACK, "ReadN INT3 ack");
        ack(&mut cd);

        // First sector -> INT1 data ready.
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::DATA, "INT1 data ready");

        // Drive into a "read" state.
        assert_ne!(cd.drive_stat & stat::READ, 0);

        // Pull the data FIFO: BFRD then read bytes. Sector 0 payload byte i = i.
        cd.write(0, 0); // INDEX 0
        cd.write(3, 0x80); // BFRD: request sector buffer read
        // Data FIFO should report data available.
        assert_ne!(cd.status() & 0x40, 0, "DRQSTS: data available");
        let b0 = cd.read(2) as u8;
        let b1 = cd.read(2) as u8;
        let b2 = cd.read(2) as u8;
        assert_eq!((b0, b1, b2), (0, 1, 2), "sector 0 data-only payload");

        ack(&mut cd);
        // Second sector streams automatically.
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::DATA, "next INT1");
        cd.write(3, 0x80);
        let s1b0 = cd.read(2) as u8;
        assert_eq!(s1b0, 1, "sector 1 payload byte0 = sector index 1");
    }

    #[test]
    fn whole_sector_mode_reads_924_bytes() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.load_disc(synth_disc(2));
        cd.seek_lba = 0;
        cd.read_lba = 0;
        cd.mode = 0x20; // whole-sector
        cd.reading = true;
        cd.pending = Some(Pending::Read);
        cd.delay = 0;
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.data.len(), 0x924, "whole sector size");
    }

    #[test]
    fn read_past_end_of_disc_gives_int4() {
        let mut cd = Cdrom::new();
        let mut irq = Irq::new();
        cd.irq_enable = 0x07;
        cd.load_disc(synth_disc(1));
        cd.read_lba = 1; // past the only sector
        cd.reading = true;
        cd.pending = Some(Pending::Read);
        cd.delay = 0;
        run_until_irq(&mut cd, &mut irq);
        assert_eq!(cd.irq_flags & 0x07, int::END, "INT4 end of disc");
        assert!(!cd.reading, "reading stopped at end of disc");
    }

    #[test]
    fn bcd_helpers_round_trip() {
        assert_eq!(bcd(0x59), 59);
        assert_eq!(to_bcd(59), 0x59);
        assert_eq!(lba_to_msf(150), (0, 2, 0));
        assert_eq!(lba_to_msf(75), (0, 1, 0));
    }
}
