//! Serial I/O — the GBA "link cable" controller. Ported 1:1 from
//! src/io/sio.ts.
//!
//! Five modes, picked by RCNT[15:14] + SIOCNT[13:12]:
//!   RCNT[15:14]=00, SIOCNT[13:12]=00 -> Normal-8 / Normal-32  (1:1 link)
//!   RCNT[15:14]=00, SIOCNT[13:12]=10 -> Multi-play             (up to 4 GBAs)
//!   RCNT[15:14]=00, SIOCNT[13:12]=11 -> UART
//!   RCNT[15:14]=01                    -> General-purpose / GPIO (RTC uses this)
//!   RCNT[15:14]=10                    -> JOY-bus (GameCube)
//!
//! We model Normal-8/32 and Multi-play because those are what real games
//! use over a link cable. UART and JOY-bus are accepted but the state
//! machine just keeps START clear (no transfer ever completes).
//!
//! GPIO mode is handled by src/memory/rtc.ts via the cart-GPIO range, not
//! here — we don't intercept RCNT.bit-banging for the RTC. Writes to RCNT
//! in GPIO mode are stored in the raw IO mirror and ignored by Sio.
//!
//! All transfers go through a pluggable Transport. The default is a
//! LocalLoopback that completes transfers immediately with 0xFFFF
//! "no partner connected" data — which is what real hardware floats to
//! when no cable is plugged in, and what most games interpret as "no
//! link partner." Phase B will swap in a Trystero-based WebRTC transport.
//!
//! PORT NOTE — the transport (src/io/sio-signal.ts) is the WebRTC /
//! link-cable transport and stays in JavaScript on both targets. Here the
//! transport is modelled behind the `LinkTransport` trait below; the host
//! supplies the concrete implementation. The `requestMultiplay` lockstep
//! hook is split out into a separate trait method (`request_multiplay`)
//! that returns `false` by default (matching TS's optional method being
//! absent) — see notes near `begin_transfer`.

use crate::irq::{Irq, IRQ_SIO};

// Called when the local GBA, in multi-play master mode, starts a
// transfer. `localData` is what we'd send as the master payload.
// Implementation should return the four-slot result array (master
// slot 0 + up to 3 slave slots). Slots with no partner connected
// must be 0xFFFF.
//
// Called when the local GBA, in Normal-32 mode, starts a transfer
// as the master. Returns the 32-bit word read back into SIODATA32.
// No partner -> 0xFFFFFFFF.
//
// Called when the local GBA, in Normal-8 mode, starts a transfer as
// the master. Returns the 8-bit word read back into SIODATA8.
//
// Whether a remote partner is currently connected. Affects SIOCNT.SD
// (data ready) and the multi-player ID stickiness during boot.
//
// True when this end of the link is the master GBA (cable parent /
// multi-player ID = 0). Implementations that don't model master/
// slave should return true. Affects SIOCNT.SI (bit 2, slave
// indicator) and the multi-player ID in bits 4-5.
//
// Phase B-2 lockstep hook. When the local Sio kicks off a Multi-
// play transfer as master, it calls requestMultiplay to ask the
// peer for a synchronized response. The transport returns true if
// it took the request (the callback will be invoked when the peer
// responds or the transport gives up); false / undefined → Sio
// falls back to the synchronous multiplayExchange path. The
// callback may be invoked synchronously (LocalLoopback) or async
// (WebSocket round-trip).
pub trait LinkTransport {
    fn multiplay_exchange(&mut self, local_data: u32) -> MultiplayResult;
    fn normal32_exchange(&mut self, local_data: u32) -> u32;
    fn normal8_exchange(&mut self, local_data: u32) -> u32;
    fn is_connected(&self) -> bool;
    fn is_master(&self) -> bool;

    // Lockstep hook. The TS `requestMultiplay?` is optional; absence ==
    // "decline" (Sio falls through to the synchronous path). In the TS
    // model the callback could store a `pendingMulti` and zero the cycle
    // budget. In this synchronous Rust port the seam is reduced to a
    // single return value: `Some(result)` means "took the request and
    // here is the synchronized response immediately"; `None` means
    // declined (fall back to the cycle counter + multiplay_exchange at
    // completion). Async transports that need a real round-trip implement
    // the deferral host-side and surface the result here once available.
    // Default impl declines, matching the optional method being absent.
    fn request_multiplay(&mut self, _local_data: u32) -> Option<MultiplayResult> {
        None
    }

    // Downcast hook so the host can reach a concrete transport's own surface
    // (e.g. the Wireless Adapter's packet seam) without the trait carrying every
    // peripheral's methods. Implementations are one-liners (`self`).
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any;
}

#[derive(Clone, Copy, Debug)]
pub struct SioTraceEntry {
    pub seq: u32, // 1-based, monotonic; gives a stable order across the buffer
    pub pc: u32,  // PC of the instruction that issued the IO access
    pub op: char, // 'R' | 'W'
    pub off: u32, // 0x120, 0x128, 0x134, etc.
    pub val: u32,
    pub n: u32, // run length of consecutive identical accesses
}

#[derive(Clone, Copy, Debug)]
pub struct MultiplayResult {
    // Slots 0-3. Master always populates 0; slaves 1-3 are 0xFFFF when no
    // partner is in that slot.
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    // True if any peer reported a transfer-time error (the SIOCNT error
    // flag). Real hardware sets this on framing/timeout faults.
    pub error: bool,
}

// Default transport: no partner ever connects. Multi-play returns "I'm
// the only one here," Normal-32 returns 0xFFFFFFFF, etc. Games that
// just touch link to detect "is anyone there" will see "no partner"
// and continue single-player.
pub struct LocalLoopback;

impl LinkTransport for LocalLoopback {
    fn is_connected(&self) -> bool {
        false
    }
    fn is_master(&self) -> bool {
        true
    }
    fn multiplay_exchange(&mut self, local_data: u32) -> MultiplayResult {
        MultiplayResult {
            d0: local_data & 0xFFFF,
            d1: 0xFFFF,
            d2: 0xFFFF,
            d3: 0xFFFF,
            error: false,
        }
    }
    fn normal32_exchange(&mut self, _local_data: u32) -> u32 {
        0xFFFFFFFF
    }
    fn normal8_exchange(&mut self, _local_data: u32) -> u32 {
        0xFF
    }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

// Multi-play transfer time, in CPU cycles, by baud. Approximating real
// hardware: each transfer is one start bit + 64 bits of data (16 bits
// per slot × 4 slots) + a stop bit. At 16.78 MHz CPU clock:
//   baud 0 (9600 bps):   ~115000 cycles
//   baud 1 (38400 bps):   ~29000 cycles
//   baud 2 (57600 bps):   ~19000 cycles
//   baud 3 (115200 bps):   ~9500 cycles
//
// We were previously clamping all four to 280000 (≈ one emu frame) as
// a B-1 hack because the master's transfer loop would spin too fast
// without per-transfer backpressure. With B-2 lockstep now in place
// (requestMultiplay synchronously awaits the slave's response, so the
// master can't outpace the peer regardless of these numbers), the
// real-hardware values are safe to restore — and *necessary* for
// games like Pokemon Emerald whose link handshake needs many transfers
// per frame to walk its state machine before its missed-packet
// timeout (~8 transfers) fires.
const MULTI_CYCLES_BY_BAUD: [i32; 4] = [115000, 29000, 19000, 9500];
// Normal-32 is a single 32-bit shift register at the chosen SO/SC
// rate. SIOCNT[1] picks 256 kHz (= 64 cycles/bit) or 2 MHz (= 8
// cycles/bit). 32 bits → ~2048 cycles slow / 256 cycles fast. Adds a
// short fudge for setup/teardown.
const NORMAL_CYCLES_SLOW: i32 = 2048;
const NORMAL_CYCLES_FAST: i32 = 256;

// SIOCNT[13:12] mode encoding:
//   00 = Normal-8, 01 = Normal-32, 10 = Multi-play, 11 = UART.
// Normal-8 and Normal-32 share a state path; UART is accepted but no
// transfer is ever scheduled.
const MODE_NORMAL_8: u32 = 0;
const MODE_NORMAL_32: u32 = 1;
const MODE_MULTI: u32 = 2;

pub struct Sio {
    // Register backing — these are what reads return.
    // SIODATA32 (0x120 lo, 0x122 hi), SIOMULTI2/3 (0x124, 0x126),
    // SIOCNT (0x128), SIOMLT_SEND/SIODATA8 (0x12A), RCNT (0x134),
    // JOYCNT (0x140), JOY_RECV (0x150), JOY_TRANS (0x154), JOYSTAT (0x158).
    pub multi: [u16; 4], // 4 multi-play slots / SIODATA32 lo/hi mirror
    pub siocnt: u32,
    pub mlt_send: u32, // SIOMLT_SEND (16-bit) / SIODATA8 (low byte)
    pub rcnt: u32,
    pub joycnt: u32,
    pub joy_recv: u32,
    pub joy_trans: u32,
    pub joystat: u32,

    // Pending transfer state machine. When SIOCNT[7] (START) is set the
    // master schedules a completion `cyclesUntilDone` later; once that
    // hits zero we publish the result and fire the IRQ.
    cycles_until_done: i32,
    active: bool,
    active_mode: u32,
    active_len32: bool, // Normal mode only: 1 = 32-bit, 0 = 8-bit

    // Monotonic counter: how many Multi-play transfers this Sio has
    // completed as the master. The remote slave's Sio watches this in
    // the broadcast state and, on advance, applies the same SIOMULTI
    // snapshot + fires SIO IRQ — that's how real hardware drives the
    // slave's transfer machinery without the slave's software setting
    // START. Wraps modulo 2^32; remote peer compares unsigned.
    pub transfer_seq: u32,

    // Filled in by the transport's requestMultiplay callback when an
    // async lockstep response arrives before the cycle budget runs out.
    // complete() prefers this over the synchronous multiplayExchange.
    pending_multi: Option<MultiplayResult>,

    // ---- JS-driven async link state (WebRTC bridge) ----
    //
    // When a JS link is active (`link_connected == true`), the host (the
    // SignalTransport in sio-signal.ts) — not the Rust `transport` — drives
    // multiplay completion over WebRTC. `read_siocnt` derives SD/SI/ID from
    // these flags instead of `transport.is_connected()/is_master()`, and a
    // master multiplay transfer does NOT auto-complete via the cycle budget;
    // it parks in `awaiting_peer` until the host calls `deliver_multiplay`
    // (or the cycle budget runs out as a dropped-peer timeout fallback).
    //
    // Default `link_connected == false` preserves the existing LocalLoopback
    // single-player behavior bit-for-bit: nothing below this point changes
    // any path when no JS link is active.
    link_active: bool,
    link_connected: bool,
    link_master: bool,

    // Set when a master multiplay transfer started with a JS link connected
    // and is waiting for the host to deliver the synchronized 4-slot result.
    // `step()` keeps counting the cycle budget down as a *timeout*: if the
    // host hasn't delivered by then, `complete()` finishes with 0xFFFF slots
    // + the error flag so a dropped peer can't hang the game.
    awaiting_peer: bool,
    // The 16-bit SIOMLT_SEND payload the master wants the host to broadcast.
    // `take_outgoing()` hands it to JS once (take semantics) so the host
    // knows to send it to peers this frame.
    outgoing: Option<u32>,

    // The transport is swappable at runtime so the UI can switch from
    // "no cable" loopback to a real WebRTC peer without restarting the
    // emulator.
    pub transport: Box<dyn LinkTransport>,

    // Optional access trace. When enabled (via Sio.traceOn = true from
    // the UI), every SIO/RCNT/JOY read and write is logged with the PC
    // that issued it. Consecutive identical accesses collapse into a
    // single entry with an incremented count — otherwise a busy-wait
    // would saturate the buffer in a single transfer. Used to debug
    // games like Mario Kart whose cable detection rejects SD-high alone
    // and we need to see which register/value it's actually waiting on.
    pub trace: Vec<SioTraceEntry>,
    pub trace_on: bool,
    trace_cap: usize,
    trace_seq: u32,
}

impl Sio {
    // TS constructor took `irq`; per the porting contract `irq` becomes a
    // `&mut` parameter on the methods that raise the SIO IRQ, so `new`
    // takes nothing. The transport defaults to LocalLoopback, like TS.
    pub fn new() -> Self {
        Self {
            multi: [0; 4],
            siocnt: 0,
            mlt_send: 0,
            rcnt: 0,
            joycnt: 0,
            joy_recv: 0,
            joy_trans: 0,
            joystat: 0,
            cycles_until_done: 0,
            active: false,
            active_mode: MODE_NORMAL_8,
            active_len32: false,
            transfer_seq: 0,
            pending_multi: None,
            link_active: false,
            link_connected: false,
            link_master: false,
            awaiting_peer: false,
            outgoing: None,
            transport: Box::new(LocalLoopback),
            trace: Vec::new(),
            trace_on: false,
            trace_cap: 4096,
            trace_seq: 0,
        }
    }

    // logTrace is a no-op in the Rust port (see contract). The trace
    // bookkeeping is kept faithful behind the trace_on guard for parity,
    // but callers in the recomp/wasm path don't drive it.
    pub fn log_trace(&mut self, op: char, off: u32, val: u32, pc: u32) {
        if !self.trace_on {
            return;
        }
        if let Some(last) = self.trace.last_mut() {
            if last.pc == pc && last.op == op && last.off == off && last.val == val {
                last.n += 1;
                return;
            }
        }
        self.trace_seq += 1;
        self.trace.push(SioTraceEntry {
            seq: self.trace_seq,
            pc,
            op,
            off,
            val,
            n: 1,
        });
        if self.trace.len() > self.trace_cap {
            self.trace.remove(0);
        }
    }

    pub fn clear_trace(&mut self) {
        self.trace.clear();
        self.trace_seq = 0;
    }

    // Advance the transfer countdown by `cyc` CPU cycles. Called from
    // Emulator.runFrame after each batch, same as PPU/Timers.
    pub fn step(&mut self, cyc: i32, irq: &mut Irq) {
        if !self.active {
            return;
        }
        self.cycles_until_done -= cyc;
        if self.cycles_until_done <= 0 {
            self.complete(irq);
        }
    }

    // Apply a Multi-play transfer that the *remote* master initiated.
    // The transport (slave side) calls this when it sees the master's
    // transferSeq advance. Mirrors what slave hardware does:
    //   - SIOCNT.START transitions to 1 briefly while transfer is
    //     in-flight (per GBATEK, "slaves receive Busy bit")
    //   - SIOMULTI registers reset to FFFFh on transfer start (also
    //     per GBATEK, on ALL GBAs not just master)
    //   - latch the four SIOMULTI slots with received data
    //   - clear SIOCNT.START
    //   - fire SIO IRQ if enabled
    // In this synchronous model the in-flight window is zero, so the
    // set/clear of START is observationally a no-op but keeps the
    // siocnt mirror correct if game code polls it between transfers.
    pub fn apply_remote_multiplay(
        &mut self,
        m0: u32,
        m1: u32,
        m2: u32,
        m3: u32,
        error: bool,
        irq: &mut Irq,
    ) {
        // Per GBATEK: SIOMULTI on all GBAs reset to FFFFh at transfer
        // start. In a sync model the slave sees FFFFh and the final
        // values in the same call, but we still go through the reset
        // step so any future async path (e.g. delayed slave IRQ) gets
        // the right intermediate state.
        self.multi[0] = 0xFFFF;
        self.multi[1] = 0xFFFF;
        self.multi[2] = 0xFFFF;
        self.multi[3] = 0xFFFF;
        self.multi[0] = (m0 & 0xFFFF) as u16;
        self.multi[1] = (m1 & 0xFFFF) as u16;
        self.multi[2] = (m2 & 0xFFFF) as u16;
        self.multi[3] = (m3 & 0xFFFF) as u16;
        if error {
            self.siocnt |= 0x0040;
        } else {
            self.siocnt &= !0x0040;
        }
        // Clear START: on a slave the busy bit is hardware-driven and goes
        // low at transfer end. We don't touch transfer_seq here — that's the
        // master's monotonic counter; the slave only consumes the master's
        // broadcast and must not advance its own (it has no transfer of its
        // own to report back).
        self.siocnt &= !0x80;
        if self.siocnt & 0x4000 != 0 {
            irq.raise(IRQ_SIO);
        }
    }

    // -------- JS-driven async link bridge (WebRTC) --------

    // Host (SignalTransport) sets the live link state. `connected` feeds
    // SIOCNT.SD; `master` feeds SIOCNT.SI/ID. Setting `connected == false`
    // (the default) restores the Rust-`transport`-driven single-player path
    // — `read_siocnt` falls back to `transport.is_connected/is_master`.
    //
    // Marking the link active (connected || master differing from default)
    // is keyed off `connected`: once the host announces a connected link the
    // Rust side stops auto-completing master multiplay transfers and waits
    // for `deliver_multiplay`. When the host announces disconnected, any
    // in-flight awaiting transfer is allowed to time out via the cycle
    // budget (it keeps ticking), so the game never hangs.
    pub fn set_link(&mut self, connected: bool, master: bool) {
        self.link_active = connected;
        self.link_connected = connected;
        self.link_master = master;
    }

    // Whether a JS link is actively driving multiplay (host announced a
    // connected peer). Used to branch the begin/complete paths.
    fn link_drives_multiplay(&self) -> bool {
        self.link_active && self.link_connected && self.link_master
    }

    // Polled by the host each frame. Returns `Some(mlt_send)` exactly once
    // after a master multiplay transfer started over a connected JS link;
    // the host sends it to peers, gathers their slots, and calls
    // `deliver_multiplay`. Returns `None` if there's nothing to send.
    pub fn take_outgoing(&mut self) -> Option<u32> {
        self.outgoing.take()
    }

    // Master-side completion driven by the host once it has the synchronized
    // 4-slot result from the peer(s). Mirrors `complete()`'s multiplay arm:
    // latch SIOMULTI, set/clear the error flag, clear START, bump
    // transfer_seq (so a watching slave applies the same snapshot), and raise
    // the SIO IRQ if enabled. No-op if no transfer is awaiting a peer (a late
    // delivery after the game aborted the transfer must not corrupt state).
    pub fn deliver_multiplay(
        &mut self,
        m0: u32,
        m1: u32,
        m2: u32,
        m3: u32,
        error: bool,
        irq: &mut Irq,
    ) {
        if !self.awaiting_peer {
            return;
        }
        self.awaiting_peer = false;
        self.active = false;
        self.cycles_until_done = 0;
        self.pending_multi = None;
        self.multi[0] = (m0 & 0xFFFF) as u16;
        self.multi[1] = (m1 & 0xFFFF) as u16;
        self.multi[2] = (m2 & 0xFFFF) as u16;
        self.multi[3] = (m3 & 0xFFFF) as u16;
        if error {
            self.siocnt |= 0x0040;
        } else {
            self.siocnt &= !0x0040;
        }
        self.transfer_seq = self.transfer_seq.wrapping_add(1);
        self.siocnt &= !0x80;
        if self.siocnt & 0x4000 != 0 {
            irq.raise(IRQ_SIO);
        }
    }

    // -------- read/write surface called from Io.read16/write16. --------

    pub fn read16(&self, off: u32) -> u32 {
        match off {
            0x120 => self.multi[0] as u32,
            0x122 => self.multi[1] as u32,
            0x124 => self.multi[2] as u32,
            0x126 => self.multi[3] as u32,
            0x128 => self.read_siocnt(),
            0x12A => self.mlt_send,
            0x134 => self.rcnt,
            0x140 => self.joycnt,
            0x150 => self.joy_recv & 0xFFFF,
            0x152 => (self.joy_recv >> 16) & 0xFFFF,
            0x154 => self.joy_trans & 0xFFFF,
            0x156 => (self.joy_trans >> 16) & 0xFFFF,
            0x158 => self.joystat,
            _ => 0,
        }
    }

    pub fn write16(&mut self, off: u32, v: u32, irq: &mut Irq) {
        let v = v & 0xFFFF;
        match off {
            // SIODATA32 / SIOMULTI{0..3} — also writable: in Normal mode the
            // master loads its outgoing word here before raising START.
            0x120 => self.multi[0] = v as u16,
            0x122 => self.multi[1] = v as u16,
            0x124 => self.multi[2] = v as u16,
            0x126 => self.multi[3] = v as u16,
            0x128 => self.write_siocnt(v, irq),
            0x12A => self.mlt_send = v,
            0x134 => self.rcnt = v,
            0x140 => self.joycnt = (self.joycnt & !0x07) | (v & 0x07),
            0x150 => self.joy_recv = (self.joy_recv & 0xFFFF0000) | v,
            0x152 => self.joy_recv = (self.joy_recv & 0x0000FFFF) | (v << 16),
            0x154 => self.joy_trans = (self.joy_trans & 0xFFFF0000) | v,
            0x156 => self.joy_trans = (self.joy_trans & 0x0000FFFF) | (v << 16),
            0x158 => self.joystat = v & 0x3F,
            _ => {}
        }
    }

    // -------- SIOCNT specifics. --------

    // SIOCNT read returns most of what was written, but a few bits are
    // hardware-driven:
    //   bit 2  SI  — slave indicator: 0 = parent / master link, 1 = child.
    //   bit 3  SD  — data ready (multi-play). High when all four GBAs
    //                are reachable. We set this from transport.isConnected.
    //   bits 4-5 multi-player ID. 0 = master, 1 = slave (we model 1:1
    //   only; IDs 2-3 would be additional slaves).
    fn read_siocnt(&self) -> u32 {
        let mut v = self.siocnt & !0x003C; // clear SI, SD, ID
        // When a JS link is active, SD comes from `link_connected` and
        // SI/ID from `link_master` (host-driven). Otherwise fall back to the
        // Rust transport (LocalLoopback → master, disconnected). Note SD and
        // master can be sourced independently: a connected slave reports SD
        // high *and* SI/ID set.
        let (connected, is_master) = if self.link_active {
            (self.link_connected, self.link_master)
        } else {
            (self.transport.is_connected(), self.transport.is_master())
        };
        if !is_master {
            v |= 0x0004; // SI high (slave)
        }
        if connected {
            v |= 0x0008; // SD high
        }
        if !is_master {
            v |= 0x0010; // ID = 1 (slave 1)
        }
        v
    }

    fn write_siocnt(&mut self, v: u32, irq: &mut Irq) {
        let mut v = v;
        let was_start = (self.siocnt & 0x80) != 0;
        // GBATEK SIO Multi-Player Mode, 4000128h - SIOCNT:
        //   Bit 7  Start/Busy Bit  (0=Inactive, 1=Start/Busy) (Read Only for Slaves)
        // So in Multi-play mode a slave's software write to bit 7 has no
        // effect on real hardware — the bit is hardware-controlled (set
        // when master initiates the transfer, cleared at end). Without
        // this guard a slave's software clobbering SIOCNT could trigger
        // beginTransfer() on the slave's Sio, which then races with the
        // master's actual transfer and corrupts SIOMULTI state on the
        // slave side.
        let mode = (v >> 12) & 3;
        let is_master = if self.link_active {
            self.link_master
        } else {
            self.transport.is_master()
        };
        if mode == MODE_MULTI && !is_master {
            // Slave path — preserve the current bit 7 instead of taking
            // whatever the slave wrote. (See SLAVE START note in the module
            // header: a slave never starts its own transfer; its SIOMULTI /
            // IRQ are driven by `apply_remote_multiplay` when the master's
            // broadcast arrives.)
            v = (v & !0x80) | (self.siocnt & 0x80);
        }
        self.siocnt = v;

        let start = (v & 0x80) != 0;
        if start && !was_start {
            // Transfer just kicked off. Figure out mode + queue completion.
            self.begin_transfer();
        } else if !start {
            // Software cleared START mid-transfer — abort. Also drop any
            // pending JS async delivery so a late `deliver_multiplay` after
            // an abort can't latch into the next transfer's state.
            self.active = false;
            self.cycles_until_done = 0;
            self.awaiting_peer = false;
            self.outgoing = None;
        }
        // NOTE: `irq` is threaded through write_siocnt because begin_transfer
        // never raises an IRQ on its own (transfers complete later via step
        // → complete), so this parameter is currently unused here. It is kept
        // on the signature for symmetry with the TS write path and in case a
        // future synchronous-completion edge needs it. Suppress unused-var.
        let _ = irq;
    }

    fn begin_transfer(&mut self) {
        let mode = (self.siocnt >> 12) & 3;
        self.active_mode = mode;
        self.active_len32 = mode == MODE_NORMAL_32;
        if mode == MODE_NORMAL_8 || mode == MODE_NORMAL_32 {
            // Normal mode shift-clock: SIOCNT bit 1 picks 256 kHz (= slow)
            // vs 2 MHz (= fast). Bit 0 is SC direction (external vs
            // internal), which doesn't affect duration of the transfer in
            // our model — we always complete it.
            self.cycles_until_done = if self.siocnt & 2 != 0 {
                NORMAL_CYCLES_FAST
            } else {
                NORMAL_CYCLES_SLOW
            };
            self.active = true;
        } else if mode == MODE_MULTI {
            self.cycles_until_done = MULTI_CYCLES_BY_BAUD[(self.siocnt & 3) as usize];
            self.active = true;
            self.pending_multi = None;
            // Per GBATEK: at the moment master sets SIOCNT.START, all four
            // SIOMULTI registers reset to 0xFFFF on every GBA on the cable.
            // The actual transferred values appear only when the transfer
            // completes. Software that polls SIOMULTI during the in-flight
            // window expects to see the FFFF "no data yet" sentinel; if it
            // sees stale values from the previous transfer instead, the
            // game's protocol logic can mis-interpret the state. Pokemon
            // Emerald's trade handshake specifically checks SIOMULTI values
            // mid-transfer and we'd been delivering stale data.
            self.multi[0] = 0xFFFF;
            self.multi[1] = 0xFFFF;
            self.multi[2] = 0xFFFF;
            self.multi[3] = 0xFFFF;

            // JS-driven async path: when a connected JS link is the master,
            // the host (WebRTC SignalTransport) completes the transfer, not
            // the Rust transport / cycle budget. Stash the outgoing payload
            // for the host to poll via `take_outgoing`, mark `awaiting_peer`,
            // and return. `cycles_until_done` is still armed above and keeps
            // counting in `step()` as a *timeout* — if the host never calls
            // `deliver_multiplay` (dropped peer), `complete()` finishes with
            // 0xFFFF slots + error so the game's transfer loop unsticks.
            if self.link_drives_multiplay() {
                self.awaiting_peer = true;
                self.outgoing = Some(self.mlt_send & 0xFFFF);
                return;
            }

            // Lockstep: ask the transport for a synchronized peer response.
            // If it accepts the request, we'll force-complete the moment
            // the callback fires (typically before the cycle budget ends),
            // so the master's perceived transfer time becomes "real RTT to
            // the slave" instead of the artificial cycle clamp. If the
            // transport declines (LocalLoopback, or no peer yet), we fall
            // through and the cycle counter does the work as before.
            //
            // PORT NOTE: TS uses an optional `requestMultiplay(localData,
            // onComplete)` whose callback sets `pendingMulti` and zeroes
            // `cyclesUntilDone`. The Rust seam collapses that to a
            // synchronous `Option<MultiplayResult>` return (see trait). A
            // `Some` here is equivalent to the TS callback having fired
            // synchronously for the still-active transfer; we replicate the
            // callback body's guard (`active && activeMode == MULTI`) — both
            // hold by construction right here, so we just store the result
            // and zero the budget so the next `step` completes it.
            if let Some(r) = self.transport.request_multiplay(self.mlt_send) {
                if self.active && self.active_mode == MODE_MULTI {
                    self.pending_multi = Some(r);
                    self.cycles_until_done = 0;
                }
            }
        } else {
            // UART — accepted, but we don't model the byte stream. Clear
            // START immediately so the game's wait loop doesn't hang.
            self.siocnt &= !0x80;
        }
    }

    fn complete(&mut self, irq: &mut Irq) {
        self.active = false;
        if self.active_mode == MODE_MULTI {
            // Timeout fallback for the JS async path: the host never
            // delivered (peer dropped / signaling stall). Complete with
            // "no partner" slots + the error flag so the game's transfer
            // loop unsticks instead of hanging. transfer_seq still bumps so
            // a watching slave sees a transfer happened.
            if self.awaiting_peer {
                self.awaiting_peer = false;
                self.outgoing = None;
                self.multi[0] = (self.mlt_send & 0xFFFF) as u16;
                self.multi[1] = 0xFFFF;
                self.multi[2] = 0xFFFF;
                self.multi[3] = 0xFFFF;
                self.siocnt |= 0x0040; // error flag
                self.transfer_seq = self.transfer_seq.wrapping_add(1);
                self.siocnt &= !0x80;
                if self.siocnt & 0x4000 != 0 {
                    irq.raise(IRQ_SIO);
                }
                return;
            }
            // Prefer the lockstep response if it arrived in time; fall back
            // to the synchronous "latest broadcast value" path otherwise.
            let r = self
                .pending_multi
                .take()
                .unwrap_or_else(|| self.transport.multiplay_exchange(self.mlt_send));
            self.multi[0] = (r.d0 & 0xFFFF) as u16;
            self.multi[1] = (r.d1 & 0xFFFF) as u16;
            self.multi[2] = (r.d2 & 0xFFFF) as u16;
            self.multi[3] = (r.d3 & 0xFFFF) as u16;
            if r.error {
                self.siocnt |= 0x0040;
            } else {
                self.siocnt &= !0x0040;
            }
            // Bump seq so a watching slave Sio applies the same SIOMULTI
            // snapshot + IRQ as if its hardware had been pulled along.
            self.transfer_seq = self.transfer_seq.wrapping_add(1);
        } else if self.active_len32 {
            // Normal-32. SIODATA32 = multi[0] (lo) | multi[1] (hi) — same
            // backing as multi-play slot 0/1.
            let out = ((self.multi[1] as u32) << 16) | (self.multi[0] as u32);
            let inp = self.transport.normal32_exchange(out);
            self.multi[0] = (inp & 0xFFFF) as u16;
            self.multi[1] = ((inp >> 16) & 0xFFFF) as u16;
        } else {
            // Normal-8. SIODATA8 lives in the low byte of SIOMLT_SEND
            // (0x12A) — same register, different mode.
            let inp = self.transport.normal8_exchange(self.mlt_send & 0xFF);
            self.mlt_send = inp & 0xFF;
        }
        // Clear START to signal completion.
        self.siocnt &= !0x80;
        if self.siocnt & 0x4000 != 0 {
            irq.raise(IRQ_SIO);
        }
    }
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal8_no_partner_completes_to_ff() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        // Normal-8 mode (00), set outgoing byte, fast clock + START + IRQ en.
        sio.write16(0x12A, 0x42, &mut irq);
        // mode 00 in bits 13:12, bit1 fast, bit14 IRQ, bit7 START.
        sio.write16(0x128, 0x0002 | 0x4000 | 0x0080, &mut irq);
        sio.step(NORMAL_CYCLES_FAST, &mut irq);
        assert_eq!(sio.mlt_send, 0xFF); // loopback normal8 -> 0xFF
        assert_eq!(sio.siocnt & 0x80, 0); // START cleared
        assert_ne!(irq.iflag & IRQ_SIO, 0); // IRQ raised
    }

    #[test]
    fn normal32_no_partner_completes_to_ffff_ffff() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write16(0x120, 0x1234, &mut irq);
        sio.write16(0x122, 0x5678, &mut irq);
        // mode 01 (Normal-32) in bits 13:12, slow clock, START.
        sio.write16(0x128, (MODE_NORMAL_32 << 12) | 0x0080, &mut irq);
        sio.step(NORMAL_CYCLES_SLOW, &mut irq);
        assert_eq!(sio.multi[0], 0xFFFF);
        assert_eq!(sio.multi[1], 0xFFFF);
    }

    #[test]
    fn multiplay_resets_to_ffff_then_completes() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.multi = [0x1111, 0x2222, 0x3333, 0x4444];
        sio.mlt_send = 0xABCD;
        // mode 10 (Multi), baud 0, START.
        sio.write16(0x128, (MODE_MULTI << 12) | 0x0080, &mut irq);
        // Mid-transfer: SIOMULTI reset to FFFF.
        assert_eq!(sio.multi, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]);
        sio.step(MULTI_CYCLES_BY_BAUD[0], &mut irq);
        // Loopback: d0 = local payload, others FFFF.
        assert_eq!(sio.multi[0], 0xABCD);
        assert_eq!(sio.multi[1], 0xFFFF);
        assert_eq!(sio.transfer_seq, 1);
    }

    #[test]
    fn slave_write_to_start_is_ignored_in_multi() {
        struct SlaveLink;
        impl LinkTransport for SlaveLink {
            fn is_connected(&self) -> bool {
                true
            }
            fn is_master(&self) -> bool {
                false
            }
            fn multiplay_exchange(&mut self, _d: u32) -> MultiplayResult {
                MultiplayResult {
                    d0: 0,
                    d1: 0,
                    d2: 0,
                    d3: 0,
                    error: false,
                }
            }
            fn normal32_exchange(&mut self, _d: u32) -> u32 {
                0
            }
            fn normal8_exchange(&mut self, _d: u32) -> u32 {
                0
            }
            fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
                self
            }
        }
        let mut sio = Sio::new();
        sio.transport = Box::new(SlaveLink);
        let mut irq = Irq::new();
        // Slave tries to set START in multi mode; bit 7 must be masked out.
        sio.write16(0x128, (MODE_MULTI << 12) | 0x0080, &mut irq);
        assert_eq!(sio.siocnt & 0x80, 0);
        assert!(!sio.active);
    }

    // ---- JS-driven async WebRTC link bridge ----

    #[test]
    fn js_link_master_async_round_trip() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        // Host announces a connected master link.
        sio.set_link(true, true);
        // SIOCNT readback: SD high (connected), SI/ID low (master).
        let cnt = sio.read16(0x128);
        assert_ne!(cnt & 0x0008, 0); // SD
        assert_eq!(cnt & 0x0004, 0); // SI low
        assert_eq!(cnt & 0x0010, 0); // ID = 0

        sio.mlt_send = 0xBEEF;
        // Master kicks off a multiplay transfer (mode 10, baud 0, IRQ en, START).
        sio.write16(0x128, (MODE_MULTI << 12) | 0x4000 | 0x0080, &mut irq);
        // Mid-transfer: slots reset, transfer parked awaiting the peer.
        assert_eq!(sio.multi, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]);
        assert!(sio.awaiting_peer);
        // Cycle budget does NOT auto-complete while awaiting the host: step a
        // little, transfer still in flight, no seq bump.
        sio.step(100, &mut irq);
        assert_ne!(sio.siocnt & 0x80, 0); // START still set
        assert_eq!(sio.transfer_seq, 0);

        // Host polls the outgoing payload once (take semantics).
        assert_eq!(sio.take_outgoing(), Some(0xBEEF));
        assert_eq!(sio.take_outgoing(), None);

        // Host delivers the synchronized 4-slot result.
        sio.deliver_multiplay(0xBEEF, 0x1234, 0xFFFF, 0xFFFF, false, &mut irq);
        assert_eq!(sio.multi[0], 0xBEEF);
        assert_eq!(sio.multi[1], 0x1234);
        assert_eq!(sio.multi[2], 0xFFFF);
        assert_eq!(sio.multi[3], 0xFFFF);
        assert_eq!(sio.siocnt & 0x80, 0); // START cleared
        assert_eq!(sio.siocnt & 0x0040, 0); // no error
        assert_eq!(sio.transfer_seq, 1); // bumped
        assert_ne!(irq.iflag & IRQ_SIO, 0); // SIO IRQ raised
        assert!(!sio.awaiting_peer);

        // A late delivery after completion is a no-op (transfer no longer awaiting).
        sio.deliver_multiplay(0xAAAA, 0xBBBB, 0xCCCC, 0xDDDD, false, &mut irq);
        assert_eq!(sio.multi[0], 0xBEEF);
        assert_eq!(sio.transfer_seq, 1);
    }

    #[test]
    fn js_link_master_timeout_fallback() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.set_link(true, true);
        sio.mlt_send = 0xCAFE;
        // Master starts a multiplay transfer (baud 0), with IRQ enabled.
        sio.write16(0x128, (MODE_MULTI << 12) | 0x4000 | 0x0080, &mut irq);
        assert!(sio.awaiting_peer);
        // Host hands the payload to JS but the peer never responds. The cycle
        // budget runs out and `complete()` finishes with the error fallback.
        assert_eq!(sio.take_outgoing(), Some(0xCAFE));
        sio.step(MULTI_CYCLES_BY_BAUD[0], &mut irq);
        assert_eq!(sio.multi[0], 0xCAFE); // own payload echoes in slot 0
        assert_eq!(sio.multi[1], 0xFFFF); // no partner
        assert_ne!(sio.siocnt & 0x0040, 0); // error flag set
        assert_eq!(sio.siocnt & 0x80, 0); // START cleared
        assert_eq!(sio.transfer_seq, 1); // bumped
        assert_ne!(irq.iflag & IRQ_SIO, 0);
        assert!(!sio.awaiting_peer);
    }

    #[test]
    fn js_link_slave_siocnt_and_apply_remote() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        // Host announces a connected slave link.
        sio.set_link(true, false);
        let cnt = sio.read16(0x128);
        assert_ne!(cnt & 0x0008, 0); // SD high (connected)
        assert_ne!(cnt & 0x0004, 0); // SI high (slave)
        assert_ne!(cnt & 0x0010, 0); // ID = 1
        // Slave software tries to set START in multi mode — must be ignored
        // (busy bit is hardware-driven for slaves).
        sio.write16(0x128, (MODE_MULTI << 12) | 0x4000 | 0x0080, &mut irq);
        assert_eq!(sio.siocnt & 0x80, 0);
        assert!(!sio.active);
        assert!(!sio.awaiting_peer);
        // Master's broadcast arrives over WebRTC; host applies it. Slots
        // latch, START clears, IRQ fires; slave does NOT bump transfer_seq.
        sio.apply_remote_multiplay(0x0001, 0x0002, 0xFFFF, 0xFFFF, false, &mut irq);
        assert_eq!(sio.multi[0], 0x0001);
        assert_eq!(sio.multi[1], 0x0002);
        assert_eq!(sio.siocnt & 0x80, 0);
        assert_eq!(sio.transfer_seq, 0);
        assert_ne!(irq.iflag & IRQ_SIO, 0);
    }

    #[test]
    fn no_js_link_preserves_loopback() {
        // With no JS link active (default), behavior is unchanged: a master
        // multiplay transfer auto-completes via the cycle budget + loopback.
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.mlt_send = 0xABCD;
        sio.write16(0x128, (MODE_MULTI << 12) | 0x0080, &mut irq);
        assert!(!sio.awaiting_peer); // not parked — no JS link
        sio.step(MULTI_CYCLES_BY_BAUD[0], &mut irq);
        assert_eq!(sio.multi[0], 0xABCD);
        assert_eq!(sio.multi[1], 0xFFFF);
        assert_eq!(sio.transfer_seq, 1);
        // SD low (loopback disconnected), SI/ID low (loopback master).
        let cnt = sio.read16(0x128);
        assert_eq!(cnt & 0x0008, 0);
        assert_eq!(cnt & 0x0004, 0);
    }
}
