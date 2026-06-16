//! Inter-Processor Communication — IPCSYNC + the two IPC FIFO queues. A SINGLE
//! `Ipc` lives on `Nds` (shared by both cores, unlike the per-core devices).
//! Ported from ../../ds-recomp/src/io/ipc.ts.
//!
//! ## Ownership (see CONTRACT.md)
//!
//! The TS `Ipc` held `irq9` + `irq7` references and raised the REMOTE core's
//! IRQ on a sync strobe / FIFO transition. We don't store those — every method
//! that can raise an interrupt takes `irq9: &mut Irq, irq7: &mut Irq` as
//! parameters. `Nds` owns both `Irq`s and the `Ipc`; since they're disjoint
//! fields the split borrow at the call site is fine.
//!
//! The IO dispatch routes `0x04000180` (IPCSYNC), `0x04000184` (IPCFIFOCNT),
//! `0x04000188` (SEND), and `0x04100000` (RECV) here, passing the `is_arm9`
//! perspective of the accessing core.

use super::irq::{Irq, IRQ_IPC_FIFO_EMPTY, IRQ_IPC_FIFO_NOT_EMPTY, IRQ_IPC_SYNC};

const FIFO_CAPACITY: usize = 16;

// IPCFIFOCNT bit layout (each core sees its own send/recv perspective).
const CNT_SEND_EMPTY: u32 = 0x0001;
const CNT_SEND_FULL: u32 = 0x0002;
const CNT_SEND_EMPTY_IRQ_EN: u32 = 0x0004;
const CNT_SEND_CLEAR: u32 = 0x0008; // write-only
const CNT_RECV_EMPTY: u32 = 0x0100;
const CNT_RECV_FULL: u32 = 0x0200;
const CNT_RECV_NOT_EMPTY_IRQ_EN: u32 = 0x0400;
const CNT_ERROR: u32 = 0x4000;
const CNT_ENABLE: u32 = 0x8000;

/// A 16-entry word FIFO with sticky last-read (real HW returns the last popped
/// value on an empty read).
#[derive(Default)]
pub struct Queue {
    pub buf: [u32; FIFO_CAPACITY],
    pub head: usize,
    pub tail: usize,
    pub size: usize,
    /// Sticky value returned on empty-read (matches hardware).
    pub last_read: u32,
}

impl Queue {
    pub fn push(&mut self, v: u32) -> bool {
        if self.size >= FIFO_CAPACITY {
            return false;
        }
        self.buf[self.tail] = v;
        self.tail = (self.tail + 1) % FIFO_CAPACITY;
        self.size += 1;
        true
    }
    pub fn pop(&mut self) -> Option<u32> {
        if self.size == 0 {
            return None;
        }
        let v = self.buf[self.head];
        self.head = (self.head + 1) % FIFO_CAPACITY;
        self.size -= 1;
        self.last_read = v;
        Some(v)
    }
    /// Read the head value WITHOUT consuming it (used by the PXI-drain assist).
    pub fn peek(&self) -> Option<u32> {
        if self.size == 0 {
            None
        } else {
            Some(self.buf[self.head])
        }
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.size = 0;
    }
}

#[derive(Default)]
pub struct Ipc {
    // ── IPCSYNC: per-core 4-bit OUT nibble + the receive-IRQ-enable bit 14.
    pub sync9_out: u32,
    pub sync9_rx_irq_en: bool,
    pub sync7_out: u32,
    pub sync7_rx_irq_en: bool,

    // ── FIFO: q9to7 fed by ARM9's SEND / consumed by ARM7's RECV; q7to9 the
    // reverse. Each core sees its own perspective in CNT.
    pub q9to7: Queue,
    pub q7to9: Queue,

    // ── Per-core CNT control bits.
    pub enable9: bool,
    pub enable7: bool,
    pub send_empty_irq_en9: bool,
    pub send_empty_irq_en7: bool,
    pub recv_not_empty_irq_en9: bool,
    pub recv_not_empty_irq_en7: bool,
    pub error9: bool,
    pub error7: bool,

    /// True once either core has done a real (non-synthetic) FIFO SEND. The
    /// PPU's VBlank heartbeat consults this before synthesizing IPC beacons.
    pub real_fifo_traffic_seen: bool,
    /// Frames since the last real send — feeds the deadlock heartbeat.
    pub frames_since_last_send: u32,
    /// Opt-in: synthesize NitroSDK "command complete" PXI replies on q7to9
    /// after an ARM9→ARM7 SEND. Off for the FIFO round-trip unit tests.
    pub pxi_stub_server_enabled: bool,
}

impl Ipc {
    pub fn new() -> Self {
        Self::default()
    }

    // ─── IPCSYNC (0x04000180) ────────────────────────────────────────────
    pub fn read_sync(&self, is_arm9: bool) -> u32 {
        let (remote_out, our_out, our_rx_en) = if is_arm9 {
            (self.sync7_out, self.sync9_out, self.sync9_rx_irq_en)
        } else {
            (self.sync9_out, self.sync7_out, self.sync7_rx_irq_en)
        };
        (remote_out & 0x0F) | ((our_out & 0x0F) << 8) | if our_rx_en { 0x4000 } else { 0 }
    }
    /// Strobing bit 13 / an OUT-nibble change raises `IRQ_IPC_SYNC` on the
    /// REMOTE core (gated by its rx-irq-enable) — hence both `Irq`s.
    pub fn write_sync(&mut self, is_arm9: bool, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        let out = (value >> 8) & 0x0F;
        let send_irq = (value & 0x2000) != 0;
        let rx_irq_en = (value & 0x4000) != 0;
        let old_out = if is_arm9 { self.sync9_out } else { self.sync7_out };
        if is_arm9 {
            self.sync9_out = out;
            self.sync9_rx_irq_en = rx_irq_en;
        } else {
            self.sync7_out = out;
            self.sync7_rx_irq_en = rx_irq_en;
        }
        // Bit 13 explicitly requests a remote IRQ. We also fire on any OUT-nibble
        // change (gated by remote rx-en): Pokemon polls IPCSYNC without strobing
        // bit 13, expecting a wake on value change (no$gba/melonDS behavior) —
        // without it the early handshake deadlocks.
        let value_changed = out != old_out;
        if send_irq || value_changed {
            if is_arm9 && self.sync7_rx_irq_en {
                irq7.raise(IRQ_IPC_SYNC);
            }
            if !is_arm9 && self.sync9_rx_irq_en {
                irq9.raise(IRQ_IPC_SYNC);
            }
        }
    }

    // ─── IPCFIFOCNT (0x04000184) ─────────────────────────────────────────
    pub fn read_cnt(&self, is_arm9: bool) -> u32 {
        let (send_q, recv_q) = if is_arm9 {
            (&self.q9to7, &self.q7to9)
        } else {
            (&self.q7to9, &self.q9to7)
        };
        let mut v = 0;
        if send_q.size == 0 {
            v |= CNT_SEND_EMPTY;
        }
        if send_q.size >= FIFO_CAPACITY {
            v |= CNT_SEND_FULL;
        }
        if recv_q.size == 0 {
            v |= CNT_RECV_EMPTY;
        }
        if recv_q.size >= FIFO_CAPACITY {
            v |= CNT_RECV_FULL;
        }
        let (send_empty_en, recv_ne_en, error, enable) = if is_arm9 {
            (
                self.send_empty_irq_en9,
                self.recv_not_empty_irq_en9,
                self.error9,
                self.enable9,
            )
        } else {
            (
                self.send_empty_irq_en7,
                self.recv_not_empty_irq_en7,
                self.error7,
                self.enable7,
            )
        };
        if send_empty_en {
            v |= CNT_SEND_EMPTY_IRQ_EN;
        }
        if recv_ne_en {
            v |= CNT_RECV_NOT_EMPTY_IRQ_EN;
        }
        if error {
            v |= CNT_ERROR;
        }
        if enable {
            v |= CNT_ENABLE;
        }
        v
    }
    /// Enabling an IRQ bit whose condition already holds fires it immediately
    /// (edge on the combined condition) — needs the local core's `Irq`.
    pub fn write_cnt(&mut self, is_arm9: bool, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        let want_send_empty = (value & CNT_SEND_EMPTY_IRQ_EN) != 0;
        let want_recv_ne = (value & CNT_RECV_NOT_EMPTY_IRQ_EN) != 0;
        let want_enable = (value & CNT_ENABLE) != 0;
        let clear_send = (value & CNT_SEND_CLEAR) != 0;
        let ack_error = (value & CNT_ERROR) != 0;
        // FIFO IRQs are edge-triggered on the COMBINED (irq-enable AND
        // fifo-state) condition, not on fifo-state alone. Newly enabling an IRQ
        // bit while its condition already holds fires immediately — NitroSDK
        // relies on this when ARM7 registers its recv callback after ARM9 has
        // already queued words (the empty→non-empty edge is long gone).
        if is_arm9 {
            let send_empty_rising = want_send_empty && !self.send_empty_irq_en9;
            let recv_ne_rising = want_recv_ne && !self.recv_not_empty_irq_en9;
            self.send_empty_irq_en9 = want_send_empty;
            self.recv_not_empty_irq_en9 = want_recv_ne;
            self.enable9 = want_enable;
            if clear_send {
                self.q9to7.clear();
            }
            if ack_error {
                self.error9 = false;
            }
            if send_empty_rising && self.q9to7.size == 0 {
                irq9.raise(IRQ_IPC_FIFO_EMPTY);
            }
            if recv_ne_rising && self.q7to9.size > 0 {
                irq9.raise(IRQ_IPC_FIFO_NOT_EMPTY);
            }
        } else {
            let send_empty_rising = want_send_empty && !self.send_empty_irq_en7;
            let recv_ne_rising = want_recv_ne && !self.recv_not_empty_irq_en7;
            self.send_empty_irq_en7 = want_send_empty;
            self.recv_not_empty_irq_en7 = want_recv_ne;
            self.enable7 = want_enable;
            if clear_send {
                self.q7to9.clear();
            }
            if ack_error {
                self.error7 = false;
            }
            if send_empty_rising && self.q7to9.size == 0 {
                irq7.raise(IRQ_IPC_FIFO_EMPTY);
            }
            if recv_ne_rising && self.q9to7.size > 0 {
                irq7.raise(IRQ_IPC_FIFO_NOT_EMPTY);
            }
        }
    }

    // ─── FIFO SEND (0x04000188) / RECV (0x04100000) ──────────────────────
    /// A successful empty→non-empty transition raises the REMOTE core's
    /// recv-not-empty IRQ; full-FIFO sets the local error flag.
    pub fn write_send(&mut self, is_arm9: bool, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        self.send_inner(is_arm9, value, false, irq9, irq7);
    }

    /// Shared SEND path. `synthetic=true` marks a PPU/PXI-injected word that
    /// must NOT flip the real-traffic bookkeeping the deadlock heartbeat reads.
    fn send_inner(
        &mut self,
        is_arm9: bool,
        value: u32,
        synthetic: bool,
        irq9: &mut Irq,
        irq7: &mut Irq,
    ) {
        let enable = if is_arm9 { self.enable9 } else { self.enable7 };
        if !enable {
            return;
        }
        if !synthetic {
            self.real_fifo_traffic_seen = true;
            self.frames_since_last_send = 0;
        }
        // ARM7→ARM9 reply normalization seam (currently identity).
        let value = if is_arm9 {
            value
        } else {
            normalize_pxi_reply(value)
        };
        let remote_recv_ne_en = if is_arm9 {
            self.recv_not_empty_irq_en7
        } else {
            self.recv_not_empty_irq_en9
        };
        let q = if is_arm9 {
            &mut self.q9to7
        } else {
            &mut self.q7to9
        };
        let was_empty = q.size == 0;
        let pushed = q.push(value);
        if !pushed {
            // Send FIFO full — set the sender's error flag.
            if is_arm9 {
                self.error9 = true;
            } else {
                self.error7 = true;
            }
            return;
        }
        if was_empty && remote_recv_ne_en {
            if is_arm9 {
                irq7.raise(IRQ_IPC_FIFO_NOT_EMPTY);
            } else {
                irq9.raise(IRQ_IPC_FIFO_NOT_EMPTY);
            }
        }
        // Stub PXI server: after an ARM9→ARM7 word is queued, synthesize any
        // "command complete" reply the NitroSDK PXI server would produce. Opt-in
        // so the FIFO round-trip tests stay free of unsolicited q7to9 traffic.
        if is_arm9 && !synthetic && self.pxi_stub_server_enabled {
            self.process_arm9_command(value, irq9, irq7);
        }
    }

    /// NitroSDK PXI subsystems each run an ARM7 server loop that consumes
    /// commands from q9to7 and pushes "command complete" replies on q7to9. We
    /// don't model those servers — we queue the single reply word the SDK's
    /// dispatcher needs to mark its outstanding request finished and wake the
    /// blocked ARM9 caller. Only specific value-shapes seen in retail traces are
    /// matched (matching whole tag-families injected noise into homebrew IPC).
    fn process_arm9_command(&mut self, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        match value {
            // Meteos WM init: 0x0501504D (tag 0x05, init service 0x01).
            0x0501_504D => self.queue_arm7_reply(0x0501_504E, irq9, irq7),
            // Nintendogs SYSTEM init: 0x00040005 → reply 0x00040025.
            0x0004_0005 => self.queue_arm7_reply(0x0004_0025, irq9, irq7),
            _ => {}
        }
    }

    /// Push a synthetic ARM7→ARM9 reply. Goes through `send_inner` with
    /// `synthetic=true` so it doesn't disturb the deadlock bookkeeping; that
    /// path also handles the ARM9 empty→non-empty IRQ + reply normalization.
    fn queue_arm7_reply(&mut self, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        if !self.enable7 {
            return;
        }
        self.send_inner(false, value, true, irq9, irq7);
    }

    /// Popping the last word raises the SENDER's send-empty IRQ.
    pub fn read_recv(&mut self, is_arm9: bool, irq9: &mut Irq, irq7: &mut Irq) -> u32 {
        let enable = if is_arm9 { self.enable9 } else { self.enable7 };
        let q = if is_arm9 {
            &mut self.q7to9
        } else {
            &mut self.q9to7
        };
        if !enable {
            return q.last_read;
        }
        let before = q.size;
        let popped = q.pop();
        let now_empty = q.size == 0;
        let last_read = q.last_read;
        match popped {
            None => {
                // Empty-read error on the consumer; sticky last value.
                if is_arm9 {
                    self.error9 = true;
                } else {
                    self.error7 = true;
                }
                last_read
            }
            Some(v) => {
                // Sender's send-empty IRQ fires when their send-FIFO drains.
                if before > 0 && now_empty {
                    if is_arm9 {
                        if self.send_empty_irq_en7 {
                            irq7.raise(IRQ_IPC_FIFO_EMPTY);
                        }
                    } else if self.send_empty_irq_en9 {
                        irq9.raise(IRQ_IPC_FIFO_EMPTY);
                    }
                }
                v
            }
        }
    }

    /// Public hook for the NitroOS PXI-drain assist (bios/nitro_os.rs) to push
    /// a synthetic ARM7→ARM9 completion ack.
    pub fn queue_arm7_ack(&mut self, value: u32, irq9: &mut Irq, irq7: &mut Irq) {
        self.queue_arm7_reply(value, irq9, irq7);
    }
}

/// PXI reply normalization — currently identity (see ipc.ts for why the old
/// per-tag masks were removed). Kept as a seam for future per-tag fixups.
#[inline]
pub fn normalize_pxi_reply(value: u32) -> u32 {
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn irqs() -> (Irq, Irq) {
        // IME on + all IPC sources enabled so raise() shows in pending().
        let mut a = Irq::new();
        let mut b = Irq::new();
        for irq in [&mut a, &mut b] {
            irq.set_ime(1);
            irq.set_ie(IRQ_IPC_SYNC | IRQ_IPC_FIFO_EMPTY | IRQ_IPC_FIFO_NOT_EMPTY);
        }
        (a, b)
    }

    #[test]
    fn queue_ring_wraps_and_reports_full() {
        let mut q = Queue::default();
        for i in 0..FIFO_CAPACITY as u32 {
            assert!(q.push(i));
        }
        assert!(!q.push(99)); // full
        assert_eq!(q.size, FIFO_CAPACITY);
        assert_eq!(q.peek(), Some(0));
        assert_eq!(q.pop(), Some(0));
        assert!(q.push(99)); // room again
        // Drain everything; last popped becomes sticky.
        let mut last = 0;
        while let Some(v) = q.pop() {
            last = v;
        }
        assert_eq!(q.pop(), None);
        assert_eq!(q.last_read, last);
        assert_eq!(q.size, 0);
    }

    #[test]
    fn sync_read_perspective_and_remote_irq() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        // ARM7 arms its rx-irq-enable so ARM9 sync writes wake it.
        ipc.write_sync(false, 0x4000, &mut irq9, &mut irq7);
        // ARM9 writes OUT nibble 0xA, no bit-13 strobe.
        ipc.write_sync(true, 0x0A << 8, &mut irq9, &mut irq7);
        // ARM7 sees ARM9's OUT in the low nibble; ARM9 sees its own in 8..12.
        assert_eq!(ipc.read_sync(false) & 0x0F, 0x0A);
        assert_eq!((ipc.read_sync(true) >> 8) & 0x0F, 0x0A);
        // Value-change fired IPC_SYNC on the remote (ARM7) gated by its rx-en.
        assert!(irq7.pending());
        assert!(!irq9.pending());
    }

    #[test]
    fn sync_no_irq_without_remote_rx_en() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        // ARM7 has NOT enabled rx-irq; strobing bit 13 from ARM9 does nothing.
        ipc.write_sync(true, 0x2000 | (0x05 << 8), &mut irq9, &mut irq7);
        assert!(!irq7.pending());
    }

    #[test]
    fn fifo_round_trip_arm9_to_arm7() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE | CNT_RECV_NOT_EMPTY_IRQ_EN, &mut irq9, &mut irq7);
        ipc.write_send(true, 0xDEAD_BEEF, &mut irq9, &mut irq7);
        // Empty→non-empty raised ARM7's recv-not-empty IRQ.
        assert!(irq7.pending());
        // ARM9's send-FIFO non-empty -> not RECV_EMPTY from ARM7's view.
        assert_eq!(ipc.read_cnt(false) & CNT_RECV_EMPTY, 0);
        assert_eq!(ipc.read_recv(false, &mut irq9, &mut irq7), 0xDEAD_BEEF);
        assert!(ipc.real_fifo_traffic_seen);
    }

    #[test]
    fn recv_drain_raises_sender_send_empty() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE | CNT_SEND_EMPTY_IRQ_EN, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE, &mut irq9, &mut irq7);
        // Enabling send-empty-irq while the send FIFO is already empty fires it
        // immediately (combined-condition edge); ack it so we can observe the
        // drain-driven edge in isolation.
        assert!(irq9.pending());
        irq9.ack_if(IRQ_IPC_FIFO_EMPTY);
        ipc.write_send(true, 1, &mut irq9, &mut irq7);
        ipc.write_send(true, 2, &mut irq9, &mut irq7);
        assert_eq!(ipc.read_recv(false, &mut irq9, &mut irq7), 1);
        // Sender's send-empty IRQ must NOT fire until the FIFO is fully drained.
        assert!(!irq9.pending());
        assert_eq!(ipc.read_recv(false, &mut irq9, &mut irq7), 2);
        assert!(irq9.pending());
    }

    #[test]
    fn send_full_sets_error_and_empty_read_sets_error() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE, &mut irq9, &mut irq7);
        for i in 0..FIFO_CAPACITY as u32 {
            ipc.write_send(true, i, &mut irq9, &mut irq7);
        }
        assert_eq!(ipc.read_cnt(true) & CNT_ERROR, 0);
        ipc.write_send(true, 0xFF, &mut irq9, &mut irq7); // overflow
        assert_ne!(ipc.read_cnt(true) & CNT_ERROR, 0);
        // Ack the error via CNT.
        ipc.write_cnt(true, CNT_ENABLE | CNT_ERROR, &mut irq9, &mut irq7);
        assert_eq!(ipc.read_cnt(true) & CNT_ERROR, 0);
        // Empty-read on ARM9 (q7to9 empty) sets ARM9 error + sticky last_read.
        let v = ipc.read_recv(true, &mut irq9, &mut irq7);
        assert_eq!(v, 0); // never popped -> default sticky
        assert_ne!(ipc.read_cnt(true) & CNT_ERROR, 0);
    }

    #[test]
    fn disabled_fifo_send_is_dropped() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        // No CNT_ENABLE written.
        ipc.write_send(true, 0x1234, &mut irq9, &mut irq7);
        assert_eq!(ipc.q9to7.size, 0);
        assert!(!ipc.real_fifo_traffic_seen);
    }

    #[test]
    fn enable_edge_fires_recv_not_empty_when_already_queued() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE, &mut irq9, &mut irq7); // recv-IRQ off
        ipc.write_send(true, 7, &mut irq9, &mut irq7);
        assert!(!irq7.pending()); // edge missed: recv-IRQ wasn't enabled
        // ARM7 now enables recv-not-empty while q9to7 already non-empty.
        ipc.write_cnt(false, CNT_ENABLE | CNT_RECV_NOT_EMPTY_IRQ_EN, &mut irq9, &mut irq7);
        assert!(irq7.pending());
    }

    #[test]
    fn pxi_stub_server_synthesizes_reply() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.pxi_stub_server_enabled = true;
        ipc.write_cnt(true, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_send(true, 0x0501_504D, &mut irq9, &mut irq7);
        // Synthetic reply landed on q7to9 for ARM9 to read.
        assert_eq!(ipc.q7to9.size, 1);
        assert_eq!(ipc.read_recv(true, &mut irq9, &mut irq7), 0x0501_504E);
        // Synthetic traffic didn't reset frames-since-send to lie to the
        // heartbeat beyond the real send that triggered it.
    }

    #[test]
    fn cnt_send_clear_flushes_send_fifo() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.write_send(true, 1, &mut irq9, &mut irq7);
        ipc.write_send(true, 2, &mut irq9, &mut irq7);
        assert_eq!(ipc.q9to7.size, 2);
        ipc.write_cnt(true, CNT_ENABLE | CNT_SEND_CLEAR, &mut irq9, &mut irq7);
        assert_eq!(ipc.q9to7.size, 0);
        assert_ne!(ipc.read_cnt(true) & CNT_SEND_EMPTY, 0);
    }

    #[test]
    fn queue_arm7_ack_pushes_to_arm9() {
        let (mut irq9, mut irq7) = irqs();
        let mut ipc = Ipc::new();
        ipc.write_cnt(true, CNT_ENABLE | CNT_RECV_NOT_EMPTY_IRQ_EN, &mut irq9, &mut irq7);
        ipc.write_cnt(false, CNT_ENABLE, &mut irq9, &mut irq7);
        ipc.queue_arm7_ack(0xCAFE, &mut irq9, &mut irq7);
        assert!(irq9.pending()); // ARM9 recv-not-empty edge
        assert_eq!(ipc.read_recv(true, &mut irq9, &mut irq7), 0xCAFE);
    }
}
