//! GBA Wireless Adapter (AGB-015 / "RFU") — high-level emulation.
//!
//! The Wireless Adapter is the only first-party peripheral that talks to the
//! GBA over SIO **Normal-32** mode (a 32-bit SPI shift register). It is not a
//! cable: the adapter is a little radio that runs its own command protocol on
//! top of the serial link. Games (Pokémon FR/LG/Emerald, the Download Play
//! menus, etc.) drive it by shifting 32-bit words and reading the reply the
//! adapter shifts back.
//!
//! We model the adapter as a `LinkTransport`: each completed Normal-32 transfer
//! routes through `normal32_exchange(word) -> word`, which is exactly the
//! adapter's "receive a word, return the next reply" SPI step. That lets a game
//! detect the adapter (the NINTENDO handshake) and walk the command protocol.
//!
//! PEER TRAFFIC — beyond the single-player handshake, this module carries real
//! data between two emulators (scoped to one host + one client for now) through
//! a host-driven seam: the JS/host transport relays broadcasts and packets over
//! the same WebSocket room used for the link cable, and feeds them in via
//! `deliver_packet` / `host_add_client` / `add_scanned_host` etc., pulling
//! outgoing packets back out with `take_outgoing` / `broadcast_payload`.
//!
//! CLOCK-REVERSAL NOTE — on real hardware the adapter answers a "wait" command
//! (WAIT / SEND+WAIT / retransmit+WAIT) by becoming the SPI *master* and
//! clocking an event word back into the GBA. Our `Sio` always completes a
//! Normal-32 transfer synchronously through `normal32_exchange` and raises the
//! SIO IRQ on completion, so we get the same observable result without modeling
//! the bus-master role swap: while parked in `WaitEvent` the adapter returns the
//! queued event word (data-available / disconnect / timeout) on the GBA's next
//! poll, and the transfer-completion IRQ wakes the game's wait handler. The host
//! ticks `update()` each frame so the wait can time out with no peer.
//!
//! The protocol is reverse-engineered hardware behavior, not a spec. The
//! command IDs, magic words, and FSM transitions below follow the public
//! documentation that the homebrew community produced:
//!   - gba-link-connection — docs/wireless_adapter.md (Rodrigo Alfonso et al.)
//!   - blog.kuiper.dev/gba-wireless-adapter (Corwin)
//!   - davidgf.net/2024/01/13/gba-wireless-adapter (David Guillen Fandos)
//!
//! Take the "meaning" of the less-understood status words with a grain of salt;
//! the values are what real adapters emit.

use crate::sio::{LinkTransport, MultiplayResult};

// ---- Command IDs (GBA -> adapter), `0x9966LLCC` low byte. ----
const CMD_HELLO: u8 = 0x10; // First command after the handshake.
const CMD_LINKPWR: u8 = 0x11; // Signal/RF strength per client.
const CMD_SYSVER: u8 = 0x12; // Firmware/hardware version word.
const CMD_SYSSTAT: u8 = 0x13; // Connection status + assigned device ID.
const CMD_SLOTSTAT: u8 = 0x14; // List of connected device slots.
const CMD_CONFIGSTAT: u8 = 0x15; // Read back the adapter configuration.
const CMD_BCST_DATA: u8 = 0x16; // Set the 6-word broadcast payload (host).
const CMD_SYSCFG: u8 = 0x17; // Configure timeout / retransmit count.
const CMD_HOST_START: u8 = 0x19; // Begin broadcasting + accepting clients.
const CMD_HOST_ACCEPT: u8 = 0x1a; // Poll connected clients.
const CMD_HOST_STOP: u8 = 0x1b; // Stop accepting new clients.
const CMD_BCRD_START: u8 = 0x1c; // Begin a broadcast-read (scan) session.
const CMD_BCRD_FETCH: u8 = 0x1d; // Fetch scanned hosts (7 words each).
const CMD_BCRD_STOP: u8 = 0x1e; // End the scan session.
const CMD_CONNECT: u8 = 0x1f; // Connect to a host by device ID.
const CMD_ISCONNECTED: u8 = 0x20; // Poll connection progress.
const CMD_CONCOMPL: u8 = 0x21; // Finalize the connection.
const CMD_SEND_DATA: u8 = 0x24; // Send a data packet.
const CMD_SEND_DATAW: u8 = 0x25; // Send a data packet and wait for a reply.
const CMD_RECV_DATA: u8 = 0x26; // Poll for received data.
const CMD_WAIT: u8 = 0x27; // Wait for an event / timeout.
const CMD_WAIT2: u8 = 0x35; // Undocumented; also puts the GBA in the wait state.
const CMD_DISCONNECT: u8 = 0x30; // Drop client(s) / self-disconnect.
const CMD_RTX_WAIT: u8 = 0x37; // Retransmit + wait.
const CMD_BYE: u8 = 0x3d; // Power down (needs a reset to wake).

// Status words the adapter reports inside command responses.
const CONN_INPROGRESS: u32 = 0x0100_0000; // ISCONNECTED: still connecting.
const CONN_FAILED: u32 = 0x0200_0000; // ISCONNECTED: connect failed.
const CONN_COMP_FAIL: u32 = 0x0100_0000; // CONCOMPL: never connected.

// The version word a real adapter returns for SYSVER (firmware/hw revision).
const SYSVER_WORD: u32 = 0x0083_0117;

// Whenever a side has nothing to send during an SPI exchange it shifts this
// "idle" pattern. The GBA reads it back while the adapter is digesting a
// command and hasn't queued a reply yet — and while a wait has no event.
const SPI_IDLE: u32 = 0x8000_0000;

// Acknowledge / error framing. An ACK is `0x99660080 | (len << 8) | cmd`
// (i.e. command + 0x80, with the response length). A rejected command yields
// the fixed `0x996601ee` followed by a one-word error code.
const ACK_BASE: u32 = 0x9966_0080;
const ERR_WORD: u32 = 0x9966_01ee;
const ERR_BAD_STATE: u8 = 1; // Valid command issued in the wrong state.

// Event "commands" the adapter pushes back to the GBA while it is parked in a
// wait (`0x9966LLCC` with the adapter's own response command id). The trailing
// `SPI_IDLE` mirrors the word real hardware leaves on the bus.
const EVT_DATA_AVAIL: u32 = 0x9966_0028; // 0x28: new data is ready to poll.
const EVT_TIMEOUT: u32 = 0x9966_0027; // 0x27: the wait timed out, no event.
const EVT_DISCONNECT: u32 = 0x9966_0129; // 0x29 + 1 data word: peer dropped.

// ConfigStatus (0x15) trailer word both roles end with. Observed on real
// hardware; meaning unknown, but the value matters for byte-exact parity.
const CONFIG_TRAILER: u32 = 257;

// Steps of the per-word SPI command exchange.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Com {
    Reset,     // Fresh out of reset; waiting for the handshake to start.
    Handshake, // Mid NINTENDO exchange.
    WaitCmd,   // Idle; expecting a `0x9966....` command word.
    WaitData,  // Reading the command's payload words.
    RespCmd,   // Shifting back the ACK word.
    RespData,  // Shifting back the response payload words.
    RespErr,   // Shifting back the fixed error word.
    RespErr2,  // Shifting back the error code.
    WaitEvent, // Parked after a wait-class command; polling for an event.
}

// The adapter's radio/session state. The discriminants match the state byte
// (bits 24-31) that SystemStatus (0x13) reports, per wireless_adapter.md.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wifi {
    Idle = 0,          // not hosting / connected
    ServingClosed = 1, // host, room closed to new clients (EndHost)
    ServingOpen = 2,   // host, open room (StartHost) — accepts connections
    Searching = 3,     // scanning for hosts (BroadcastReadStart)
    Connecting = 4,    // mid-connect to a host (Connect)
    Client = 5,        // connected as a client (FinishConnection)
}

pub struct WirelessAdapter {
    com: Com,
    wifi: Wifi,

    // The previous word the GBA shifted in. The handshake reply folds in the
    // ones-complement of this (see `handshake_reply`).
    prev_data: u32,

    // Current command in flight: id, declared payload length, and a cursor used
    // both while reading the payload and while shifting the response back.
    cmd: u8,
    plen: u8,
    cnt: usize,
    // Doubles as the incoming payload buffer and the outgoing response buffer
    // (input is fully consumed by `process_command` before the response is
    // produced, exactly as on hardware).
    buf: Vec<u32>,
    // Error code to emit in the `RespErr2` step.
    err_code: u8,

    // Setup (CMD_SYSCFG) knobs. `timeout` is in frames; a wait with no event
    // gives up after that many `update()` ticks. `rtx_max` is the retransmit
    // cap; `max_players` (0 = 5 players … 3 = 2 players) limits room slots.
    // `setup_config` keeps the raw word for ConfigStatus (0x15) to echo back.
    timeout: u8,
    rtx_max: u8,
    max_players: u8,
    setup_config: u32,

    // Session identity. Device IDs are normally random per session; we generate
    // them deterministically so the differential harness stays reproducible.
    rng: u32,
    host_devid: u16,
    client_devid: u16,
    client_clnum: u16,

    // ---- async peer seam (scoped to one host + one client) ----
    //
    // The devid of the single connected client when we are a host (0 = none).
    peer_devid: u16,
    // Our broadcast payload (set by CMD_BCST_DATA) for the transport to relay.
    broadcast: [u32; 6],
    // Hosts the transport has discovered for us to list during a scan and to
    // resolve a CONNECT against: (devid, 6-word broadcast payload).
    scanned: Vec<(u16, [u32; 6])>,
    // A packet received from the peer, awaiting CMD_RECV_DATA.
    rx: Option<Vec<u8>>,
    // A packet the game queued via CMD_SEND_DATA(W), awaiting `take_outgoing`.
    tx: Option<Vec<u8>>,
    // The last data bytes sent via CMD_SEND_DATA, for "ghost sends" (a header
    // with no data words resends the last N bytes — how a host lets a client
    // take the initiative). Per the spec, capped at 4 bytes.
    last_sent: Vec<u8>,
    // An event queued to deliver while parked in `WaitEvent`. Delivered by the
    // clock-reversal path (`reverse_clock`): the adapter becomes the SPI master
    // and clocks these words into the GBA.
    event: Option<Vec<u32>>,
    // Words currently being reverse-clocked into the GBA, one per slave transfer
    // (drained front-to-back). Empty when not mid-push.
    reverse: Vec<u32>,
    // Frames elapsed since entering `WaitEvent`, for the no-event timeout.
    wait_ticks: u32,
    // True while the GBA is the clock SLAVE for a wait/reverse exchange — from
    // the wait-class ACK (MS_CHANGE 0x27, DATA_TX_AND_CHANGE 0x25, 0x35, 0x37)
    // until it resumes master mode and issues the next command. librfu's slave
    // SIO handler does handshake_wait(0) → drive SO high → handshake_wait(1),
    // the REVERSE of the master handler's wait(1)→SO→wait(0). So `gpio_si` must
    // mirror SO (return `so_high`) during this window, but invert it (`!so_high`)
    // during the master command phase — otherwise the slave receive's second
    // handshake_wait never satisfies and the receive loops in timeout recovery
    // ("Communicating…" forever). Stays set through the final reverse word's
    // handler (which runs after `com` has already flipped back to WaitCmd).
    reversing: bool,
    // Set (to the requested host devid) when the game issues CMD_CONNECT against
    // a discovered host. The transport takes it once to relay a connect request
    // to that host; the host answers via `host_add_client` + the transport calls
    // our `client_set_connected`. None when there's no pending connect.
    connect_requested: Option<u16>,
    // Diagnostics for the wait/reverse-clock path (surfaced to the UI strip):
    // how many packets were delivered, how many times reverse_clock was driven,
    // and how many times it actually fed a word back.
    diag_deliver: u32,
    diag_rc_called: u32,
    diag_rc_fired: u32,
    // Diagnostic ring of (sent, reply) SPI word exchanges. Lets the host dump
    // exactly what the game shifts to the adapter and what the HLE replies, to
    // find where detection diverges. Capped; oldest drop first.
    trace: Vec<(u32, u32)>,
}

impl WirelessAdapter {
    pub fn new() -> Self {
        Self {
            com: Com::Reset,
            wifi: Wifi::Idle,
            prev_data: 0,
            cmd: 0,
            plen: 0,
            cnt: 0,
            buf: Vec::new(),
            err_code: 0,
            timeout: 0,
            rtx_max: 0,
            max_players: 0,
            setup_config: 0,
            // Fixed seed → deterministic device IDs (good for tests + the
            // differential oracle). A real adapter seeds from noise.
            rng: 0x1234_5678,
            host_devid: 0,
            client_devid: 0,
            client_clnum: 0,
            peer_devid: 0,
            broadcast: [0; 6],
            scanned: Vec::new(),
            rx: None,
            tx: None,
            last_sent: Vec::new(),
            event: None,
            reverse: Vec::new(),
            wait_ticks: 0,
            reversing: false,
            connect_requested: None,
            diag_deliver: 0,
            diag_rc_called: 0,
            diag_rc_fired: 0,
            trace: Vec::new(),
        }
    }

    // Soft reset triggered when the game re-runs the NINTENDO handshake mid-
    // session (it pulsed the unmodeled reset GPIO). Clears protocol + session
    // state but keeps the RNG (so device IDs stay deterministic) and the
    // diagnostic trace (so a capture spans the re-handshake).
    fn soft_reset(&mut self) {
        let rng = self.rng;
        let trace = std::mem::take(&mut self.trace);
        *self = Self::new();
        self.rng = rng;
        self.trace = trace;
    }

    // Return the adapter to its just-powered state. The real device resets when
    // the game pulses the adapter's reset line (GBATEK GPIO bit). We don't model
    // that line, so the host calls this when (re)attaching the adapter.
    pub fn reset(&mut self) {
        let rng = self.rng;
        *self = Self::new();
        self.rng = rng; // keep the PRNG moving across resets
    }

    // Deterministic, nonzero 16-bit device ID. A zero ID means "empty slot" in
    // several responses, so we never hand one out.
    fn new_devid(&mut self) -> u16 {
        loop {
            // Numerical Recipes LCG.
            self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let id = (self.rng >> 16) as u16;
            if id != 0 {
                return id;
            }
        }
    }

    // The handshake reply: the GBA's low half echoed into our high half, plus
    // the ones-complement of the *previous* word's low half. Reproduces the
    // documented NINTENDO-exchange table verbatim.
    fn handshake_reply(&self, sent: u32) -> u32 {
        (sent << 16) | (!self.prev_data & 0xFFFF)
    }

    // WAIT / SEND_DATAW / RTX_WAIT reverse the SPI clock on hardware; we model
    // them by parking in `WaitEvent` after the ACK.
    fn is_wait_class(cmd: u8) -> bool {
        cmd == CMD_WAIT || cmd == CMD_WAIT2 || cmd == CMD_SEND_DATAW || cmd == CMD_RTX_WAIT
    }

    // One SPI word exchange (with diagnostic logging). See `transfer_inner`.
    pub fn transfer(&mut self, sent: u32) -> u32 {
        let reply = self.transfer_inner(sent);
        if self.trace.len() >= 4096 {
            self.trace.remove(0);
        }
        self.trace.push((sent, reply));
        reply
    }

    /// Drain the captured (sent, reply) SPI word exchanges for diagnosis.
    pub fn take_trace(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.trace)
    }

    /// One-line snapshot of the wait/reverse-clock path for the debug strip:
    /// deliveries, reverse_clock calls/fires, wait ticks, the FSM step, and
    /// whether a wait event / received packet is currently pending.
    pub fn diag(&self) -> String {
        format!(
            "dlv={} rcCall={} rcFire={} wait={} com={:?} ev={} rx={}",
            self.diag_deliver,
            self.diag_rc_called,
            self.diag_rc_fired,
            self.wait_ticks,
            self.com,
            self.event.is_some(),
            self.rx.is_some(),
        )
    }

    // One SPI word exchange: take the word the GBA shifted out, advance the FSM,
    // return the word the adapter shifts back this transfer.
    fn transfer_inner(&mut self, sent: u32) -> u32 {
        let retval = match self.com {
            Com::Reset => {
                // The GBA opens with `0x7FFF494E` ("..NI"); the low half being
                // the first NINTENDO pair starts the handshake. Until then the
                // adapter shifts zeros.
                if sent & 0xFFFF == 0x494E {
                    self.com = Com::Handshake;
                }
                0
            }
            Com::Handshake => {
                // The exchange ends when the GBA sends `0xB0BB8001`.
                if sent == 0xB0BB_8001 {
                    self.com = Com::WaitCmd;
                }
                self.handshake_reply(sent)
            }
            Com::WaitCmd => {
                // A command word is `0x9966LLCC`: LL payload words, CC command.
                if sent == 0x9966_00A8 || sent == 0x9966_00A7 {
                    // The GBA's acknowledgement of a reverse-clocked notification
                    // (0x28 data-available → 0xA8, or 0x27 timeout → 0xA7). It is
                    // the GBA's reply word inside the reversed exchange, not a
                    // command to run — consume it and stay idle. (Reached only if
                    // the game ACKs in master mode rather than via the reverse.)
                    SPI_IDLE
                } else if sent >> 16 == 0x9966 {
                    // The GBA is driving a command word again → it has returned
                    // to clock master, so the slave/reverse handshake window is
                    // over. Restore `gpio_si` to its master-phase polarity.
                    self.reversing = false;
                    self.plen = (sent >> 8) as u8;
                    self.cmd = sent as u8;
                    self.cnt = 0;
                    self.buf.clear();
                    if self.plen == 0 {
                        self.dispatch();
                    } else {
                        self.com = Com::WaitData;
                    }
                    // The adapter is busy receiving; it shifts the idle pattern.
                    SPI_IDLE
                } else if sent == 0x7FFF_494E || sent == 0xFFFF_494E {
                    // Re-initialization: games put the adapter to sleep with Bye
                    // (0x3d) and re-use it by pulsing the reset line (a GPIO we
                    // don't model) and re-running the NINTENDO handshake. We only
                    // see the SPI words, so treat the canonical handshake opener
                    // arriving here as a reset + restart — replying 0 like the
                    // Reset state does, so the game's recovery proceeds. Without
                    // this FR/LG's detection sends 0x7FFF494E hundreds of times to
                    // no effect and reports "adapter not connected properly".
                    self.soft_reset();
                    self.com = Com::Handshake;
                    0
                } else {
                    SPI_IDLE
                }
            }
            Com::WaitData => {
                self.buf.push(sent);
                self.cnt += 1;
                if self.cnt == self.plen as usize {
                    self.dispatch();
                }
                SPI_IDLE
            }
            Com::RespCmd => {
                // ACK: command + 0x80, carrying the response length.
                let ack = ACK_BASE | self.cmd as u32 | ((self.plen as u32) << 8);
                self.cnt = 0;
                self.com = if Self::is_wait_class(self.cmd) {
                    // Park and wait for an event (or a timeout); see the
                    // clock-reversal note in the module header. NOTE: `reversing`
                    // (the gpio_si polarity flip) is NOT set here — the *master*
                    // handler still has to finish this command's closing
                    // handshake_wait and perform the master→slave switch with
                    // master polarity. It flips only once the adapter actually
                    // reverse-clocks the first word (see `reverse_clock`).
                    self.wait_ticks = 0;
                    Com::WaitEvent
                } else if self.plen > 0 {
                    Com::RespData
                } else {
                    Com::WaitCmd
                };
                ack
            }
            Com::RespData => {
                let word = self.buf[self.cnt];
                self.cnt += 1;
                if self.cnt == self.plen as usize {
                    self.com = Com::WaitCmd;
                }
                word
            }
            Com::RespErr => {
                self.com = Com::RespErr2;
                ERR_WORD
            }
            Com::RespErr2 => {
                self.com = Com::WaitCmd;
                self.err_code as u32
            }
            Com::WaitEvent => {
                // Parked after a wait command. The data-available / timeout
                // notification is delivered by the clock-reversal path
                // (`reverse_clock`), NOT as a reply to a GBA-driven poll — on real
                // hardware the adapter seizes the clock to push it. A stray poll
                // that lands here (game shifted before parking) just idles; the
                // host Sio parks the game's transfer via `wants_reverse()` and
                // drives the push instead.
                SPI_IDLE
            }
        };
        self.prev_data = sent;
        retval
    }

    // A command has been fully received (header + payload). Run it and arm the
    // response steps.
    fn dispatch(&mut self) {
        let input = std::mem::take(&mut self.buf);
        match self.process_command(&input) {
            Ok(resp) => {
                self.plen = resp.len() as u8;
                self.buf = resp;
                self.com = Com::RespCmd;
            }
            Err(code) => {
                self.err_code = code;
                self.plen = 1; // the error frame is one extra word
                self.com = Com::RespErr;
            }
        }
    }

    // Is this adapter currently a host (open or closed room)?
    fn is_host(&self) -> bool {
        matches!(self.wifi, Wifi::ServingOpen | Wifi::ServingClosed)
    }

    // Execute a command. `input` is the received payload; the return is either
    // the response words (possibly empty) or an error code for a rejected
    // command. Command semantics follow gba-link-connection's wireless_adapter.md.
    fn process_command(&mut self, input: &[u32]) -> Result<Vec<u32>, u8> {
        match self.cmd {
            // Pure acknowledgements. (0x35 is undocumented but, like Wait, parks
            // the GBA — the wait-class routing after the ACK handles that.)
            CMD_HELLO | CMD_BYE | CMD_WAIT | CMD_WAIT2 => Ok(vec![]),

            CMD_SYSCFG => {
                // Bits 0-7: wait timeout (frames). Bits 8-15: retransmit cap.
                // Bits 16-17: maxPlayers (0=5p … 3=2p). Keep the raw word for
                // ConfigStatus to echo.
                let cfg = input.first().copied().unwrap_or(0);
                self.setup_config = cfg;
                self.timeout = cfg as u8;
                self.rtx_max = (cfg >> 8) as u8;
                self.max_players = ((cfg >> 16) & 3) as u8;
                Ok(vec![])
            }

            CMD_SYSVER => Ok(vec![SYSVER_WORD]),

            CMD_SYSSTAT => {
                // Bits 24-31: state (Wifi discriminant). Bits 16-23: slot bit for
                // a client. Bits 0-15: device ID (0 when not hosting/connected).
                let state = self.wifi as u32;
                let w = if self.is_host() {
                    (state << 24) | self.host_devid as u32
                } else {
                    match self.wifi {
                        Wifi::Client => {
                            (state << 24)
                                | ((1u32 << self.client_clnum) << 16)
                                | self.client_devid as u32
                        }
                        Wifi::Connecting => (state << 24) | self.client_devid as u32,
                        // Idle (0) / Searching (3): state byte only, no device ID.
                        _ => state << 24,
                    }
                };
                Ok(vec![w])
            }

            // Connected-client list with an extra leading word: the clientNumber
            // the next connection will get (0xFF if the room can't accept one).
            // Each client word is `clientNumber << 24 | devid`. (We model one.)
            CMD_SLOTSTAT => Ok(if self.is_host() {
                let next = if self.wifi == Wifi::ServingOpen && self.peer_devid == 0 {
                    0 // next joiner becomes clientNumber 0
                } else {
                    0xFF // slot taken or room closed
                };
                if self.peer_devid != 0 {
                    vec![next, self.peer_devid as u32]
                } else {
                    vec![next]
                }
            } else {
                vec![]
            }),

            CMD_CONFIGSTAT => {
                // Read back config: host → 6 broadcast words + setup + trailer (8);
                // client → 6 zeros + trailer (7). Matches observed hardware.
                if self.is_host() {
                    let mut out = self.broadcast.to_vec();
                    out.push(self.setup_config);
                    out.push(CONFIG_TRAILER);
                    Ok(out)
                } else {
                    Ok(vec![0, 0, 0, 0, 0, 0, CONFIG_TRAILER])
                }
            }

            CMD_LINKPWR => {
                let w = if self.is_host() {
                    if self.peer_devid != 0 {
                        0x0000_00FF // client 0 at full strength (byte 0)
                    } else {
                        0
                    }
                } else if self.wifi == Wifi::Client {
                    // A client reports only its own level, in its slot's byte.
                    0xFFu32 << (self.client_clnum * 8)
                } else {
                    0
                };
                Ok(vec![w])
            }

            // Set the broadcast payload the transport relays to scanning peers.
            CMD_BCST_DATA => {
                if input.len() >= 6 {
                    self.broadcast.copy_from_slice(&input[..6]);
                }
                Ok(vec![])
            }

            // Enter scan mode (no response data).
            CMD_BCRD_START => {
                if !self.is_host() && self.wifi != Wifi::Client {
                    self.wifi = Wifi::Searching;
                }
                Ok(vec![])
            }

            // Return the scanned hosts: per host, a metadata word (server id in
            // the low 16 bits, next-slot byte in bits 16-23) then the 6 broadcast
            // words. `0x1e` additionally exits scan mode.
            CMD_BCRD_FETCH | CMD_BCRD_STOP => {
                let mut out = Vec::with_capacity(self.scanned.len() * 7);
                for &(devid, data) in &self.scanned {
                    // Next-slot byte 0 → joinable as clientNumber 0 (we model a
                    // single client per host).
                    out.push(devid as u32);
                    out.extend_from_slice(&data);
                }
                if self.cmd == CMD_BCRD_STOP && self.wifi == Wifi::Searching {
                    self.wifi = Wifi::Idle;
                }
                Ok(out)
            }

            CMD_HOST_START => {
                if self.wifi == Wifi::Client || self.wifi == Wifi::Connecting {
                    return Err(ERR_BAD_STATE);
                }
                // Keep a STABLE device ID across the game's HOST_STOP/START
                // matchmaking churn (re-rolling it stranded the scanning peer on
                // a dead beacon). Mint once; reopen the room otherwise.
                if self.host_devid == 0 {
                    self.host_devid = self.new_devid();
                }
                if !self.is_host() {
                    self.peer_devid = 0;
                }
                self.wifi = Wifi::ServingOpen;
                Ok(vec![])
            }

            CMD_HOST_STOP => {
                if !self.is_host() {
                    return Err(ERR_BAD_STATE);
                }
                // Close the room: existing connection stays alive, new ones are
                // refused. With no client, drop back to idle.
                self.wifi = if self.peer_devid != 0 {
                    Wifi::ServingClosed
                } else {
                    Wifi::Idle
                };
                Ok(vec![])
            }

            // Poll new connections: `clientNumber << 24 | devid` per client, or
            // empty. Fails on a closed/non-host (the game falls back to SlotStatus).
            CMD_HOST_ACCEPT => {
                if self.wifi != Wifi::ServingOpen {
                    return Err(ERR_BAD_STATE);
                }
                Ok(if self.peer_devid != 0 {
                    vec![self.peer_devid as u32]
                } else {
                    vec![]
                })
            }

            // Connect to a host by device ID. If the transport surfaced that host,
            // move to Connecting (it then drives the handshake + calls
            // `client_set_connected`); otherwise leave the state so ISCONNECTED
            // reports the failure.
            CMD_CONNECT => {
                if self.is_host() {
                    return Err(ERR_BAD_STATE);
                }
                let reqid = (input.first().copied().unwrap_or(0) & 0xFFFF) as u16;
                if self.scanned.iter().any(|&(id, _)| id == reqid) {
                    self.wifi = Wifi::Connecting;
                    self.connect_requested = Some(reqid);
                }
                Ok(vec![])
            }

            CMD_ISCONNECTED => {
                if self.is_host() {
                    return Err(ERR_BAD_STATE);
                }
                let w = match self.wifi {
                    Wifi::Connecting => CONN_INPROGRESS,
                    Wifi::Client => {
                        self.client_devid as u32 | ((self.client_clnum as u32) << 16)
                    }
                    _ => CONN_FAILED,
                };
                Ok(vec![w])
            }

            CMD_CONCOMPL => {
                if self.is_host() {
                    return Err(ERR_BAD_STATE);
                }
                if self.wifi == Wifi::Client {
                    Ok(vec![
                        self.client_devid as u32 | ((self.client_clnum as u32) << 16),
                    ])
                } else {
                    self.wifi = Wifi::Idle;
                    Ok(vec![CONN_COMP_FAIL])
                }
            }

            // Queue an outgoing packet for the transport to relay. Only valid in
            // a session. The send-and-wait variant additionally parks in the wait
            // state (handled by `is_wait_class` after the ACK).
            CMD_SEND_DATA | CMD_SEND_DATAW => {
                if self.is_host() || self.wifi == Wifi::Client {
                    self.capture_outgoing(input);
                    Ok(vec![])
                } else {
                    Err(ERR_BAD_STATE)
                }
            }

            // Retransmit the last host packet + wait. Our relay re-sends the last
            // captured bytes as a ghost send so the peer still gets a tick.
            CMD_RTX_WAIT => {
                if self.is_host() || self.wifi == Wifi::Client {
                    if !self.last_sent.is_empty() {
                        self.tx = Some(self.last_sent.clone());
                    }
                    Ok(vec![])
                } else {
                    Err(ERR_BAD_STATE)
                }
            }

            // Poll received data. First word is the per-slot byte-count header;
            // the rest are the packet's little-endian words. No packet → header 0.
            CMD_RECV_DATA => {
                let pkt = self.rx.take();
                Ok(if self.is_host() {
                    recv_response(pkt.as_deref(), true)
                } else if self.wifi == Wifi::Client {
                    recv_response(pkt.as_deref(), false)
                } else {
                    vec![]
                })
            }

            CMD_DISCONNECT => {
                if self.wifi == Wifi::Client {
                    self.wifi = Wifi::Idle;
                    self.client_devid = 0;
                    self.client_clnum = 0;
                } else if self.is_host() {
                    self.peer_devid = 0;
                }
                Ok(vec![])
            }

            // Unknown command. The documented-but-unknown IDs (0x18, 0x32-0x39)
            // are *valid* (they don't raise the invalid-command error), so we
            // leniently ACK rather than wedge the FSM.
            _ => Ok(vec![]),
        }
    }

    // Pull the outgoing packet bytes out of a SEND_DATA payload and stash them
    // for the host to relay. The header word's byte count is encoded differently
    // for host vs client (per the reference librfu layout).
    fn capture_outgoing(&mut self, input: &[u32]) {
        let Some(&header) = input.first() else {
            return;
        };
        // Size-header layout (per the wireless_adapter.md spec):
        //   host send:   low 7 bits = byte count.
        //   client send: count << (3 + (1+clnum)*5) → client 0 = count << 8.
        // (So a 4-byte client-0 packet has header 0x400, not 0x004.)
        let blen = if self.is_host() {
            (header & 0x7F) as usize
        } else {
            ((header >> (8 + self.client_clnum * 5)) & 0x1F) as usize
        };
        let mut bytes = Vec::with_capacity(blen);
        for w in &input[1..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() >= blen {
            // A real send: data words present. Remember the tail for ghost
            // resends (capped at 4 bytes per the spec) and relay it.
            bytes.truncate(blen);
            self.last_sent = bytes.clone();
            if self.last_sent.len() > 4 {
                let start = self.last_sent.len() - 4;
                self.last_sent.drain(..start);
            }
            self.tx = Some(bytes);
        } else {
            // GHOST SEND: a header declaring N bytes with no data words. Per the
            // spec this "resends the last N bytes (up to 4) — garbage that stays
            // in the hardware buffer" — how a host lets a client take the
            // initiative (Pokémon's per-frame `[header]` poll). We deliver N real
            // bytes (the last-sent tail, zero-padded when we have no history) so
            // the packet is ALWAYS non-empty: an empty relay packet can be
            // dropped before reaching the peer, which would leave the client's
            // WAIT parked forever (it never receives the 0x28 to wake it).
            let want = blen.min(4).max(1);
            let mut g = vec![0u8; want];
            let have = self.last_sent.len().min(want);
            if have > 0 {
                g[want - have..].copy_from_slice(&self.last_sent[self.last_sent.len() - have..]);
            }
            self.tx = Some(g);
        }
    }

    // -------- host-facing seam (driven by the JS/host transport) --------

    /// Advance the wait timeout by `frames`. Called once per emulated frame; a
    /// no-event wait gives up after the configured timeout (CMD_SYSCFG).
    pub fn update(&mut self, frames: u32) {
        if self.com == Com::WaitEvent {
            self.wait_ticks = self.wait_ticks.saturating_add(frames);
        }
    }

    /// Register the (single) connected client on the host side and return the
    /// device ID assigned to it. The transport calls this when a peer's connect
    /// request arrives.
    pub fn host_add_client(&mut self) -> u16 {
        let id = self.new_devid();
        self.peer_devid = id;
        if !self.is_host() {
            if self.host_devid == 0 {
                self.host_devid = self.new_devid();
            }
            self.wifi = Wifi::ServingOpen;
        }
        id
    }

    /// Take the host devid the game asked to CONNECT to (once), so the transport
    /// can relay a connect request. `None` when no connect is pending.
    pub fn take_pending_connect(&mut self) -> Option<u16> {
        self.connect_requested.take()
    }

    /// Finalize this adapter as a client: the host accepted us with the given
    /// device ID and client slot number. Flips a pending Connecting to Client.
    pub fn client_set_connected(&mut self, devid: u16, clnum: u16) {
        self.wifi = Wifi::Client;
        self.client_devid = devid;
        self.client_clnum = clnum;
    }

    /// Drop the peer link. Queues a disconnect event so a parked wait wakes the
    /// game; a host returns to "no client", a client returns to idle.
    pub fn disconnect_peer(&mut self) {
        if self.is_host() {
            self.peer_devid = 0;
        } else if matches!(self.wifi, Wifi::Client | Wifi::Connecting) {
            self.wifi = Wifi::Idle;
            self.client_devid = 0;
            self.client_clnum = 0;
        }
        // 0x99660129: disconnect notification with bit 8 = "connection lost",
        // plus a one-word client bitmask. Wakes a parked wait via reverse-clock.
        self.event = Some(vec![EVT_DISCONNECT, 0x0000_000F, SPI_IDLE]);
    }

    /// Inject a packet received from the peer. It is handed to the game on the
    /// next CMD_RECV_DATA, and wakes a parked wait via a data-available event.
    pub fn deliver_packet(&mut self, bytes: &[u8]) {
        self.diag_deliver = self.diag_deliver.wrapping_add(1);
        self.rx = Some(bytes.to_vec());
        // Every delivery (including an empty heartbeat) raises a data-available
        // event so the peer's parked WAIT wakes this frame and the session keeps
        // advancing instead of timing out and giving up.
        self.event = Some(vec![EVT_DATA_AVAIL, SPI_IDLE]);
    }

    /// Take the packet the game queued via CMD_SEND_DATA(W), if any, for the
    /// transport to relay to the peer. Take-once semantics.
    pub fn take_outgoing(&mut self) -> Option<Vec<u8>> {
        self.tx.take()
    }

    /// Add a host the transport discovered, so it appears in scan results and a
    /// CONNECT to its device ID succeeds.
    pub fn add_scanned_host(&mut self, devid: u16, data: [u32; 6]) {
        if !self.scanned.iter().any(|&(id, _)| id == devid) {
            self.scanned.push((devid, data));
        }
    }

    /// Clear the discovered-hosts list (e.g. when a scan session restarts).
    pub fn clear_scanned_hosts(&mut self) {
        self.scanned.clear();
    }

    /// This host's broadcast payload (set by the game via CMD_BCST_DATA) for the
    /// transport to announce to scanning peers.
    pub fn broadcast_payload(&self) -> [u32; 6] {
        self.broadcast
    }

    /// This host's device ID (0 until CMD_HOST_START), used as the broadcast's
    /// server id.
    pub fn host_device_id(&self) -> u16 {
        self.host_devid
    }
}

// CMD_RECV_DATA response body for a received packet. The host encodes each
// client's byte count into the header at `8 + slot*5` (slot 0 here); a client
// just puts the length in the header. The packet bytes follow as little-endian
// words.
fn recv_response(pkt: Option<&[u8]>, host: bool) -> Vec<u32> {
    let Some(bytes) = pkt.filter(|b| !b.is_empty()) else {
        return vec![0];
    };
    let header = if host {
        (bytes.len() as u32 & 0x1F) << 8 // slot 0
    } else {
        bytes.len() as u32
    };
    let mut out = vec![header];
    for chunk in bytes.chunks(4) {
        let mut w = [0u8; 4];
        w[..chunk.len()].copy_from_slice(chunk);
        out.push(u32::from_le_bytes(w));
    }
    out
}

impl Default for WirelessAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkTransport for WirelessAdapter {
    // The adapter speaks only through the Normal-32 data words; it is not a
    // multiplay partner, so SIOCNT SD/SI/ID read back as the no-cable default
    // (matching `LocalLoopback`).
    fn is_connected(&self) -> bool {
        false
    }
    fn is_master(&self) -> bool {
        true
    }

    fn normal32_exchange(&mut self, local_data: u32) -> u32 {
        self.transfer(local_data)
    }

    // Wireless games never use these modes, but the trait requires them; answer
    // as "no partner", same as loopback.
    fn normal8_exchange(&mut self, _local_data: u32) -> u32 {
        0xFF
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
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    // The adapter is always ready for the next 32-bit transfer, so it mirrors
    // the inverse of the GBA's SO line on SI: GBA drives SO low → SI reads high;
    // GBA drives SO high → SI reads low. librfu polls this between transfers to
    // pace the SPI; without it the wireless session init times out and the game
    // loops re-initializing.
    fn gpio_si(&self, so_high: bool) -> Option<bool> {
        // Master command phase: SI = !SO (the adapter answers each of the GBA's
        // SO toggles with the opposite SI, satisfying the master handler's
        // wait(1)→SO-high→wait(0) sequence). Slave/reverse phase: the GBA's
        // handler runs wait(0)→SO-high→wait(1), so SI must MIRROR SO instead —
        // otherwise the second handshake_wait spins and the receive never
        // completes. See `reversing`.
        if self.reversing {
            Some(so_high)
        } else {
            Some(!so_high)
        }
    }

    // The adapter wants to seize the clock while parked after a wait-class
    // command and either an event is queued, a push is in progress, or the wait
    // has timed out. The host Sio uses this to park the game's Normal-32 transfer
    // and drive it via `reverse_clock` instead of completing it GBA-master style.
    fn wants_reverse(&self) -> bool {
        self.com == Com::WaitEvent
            && (self.event.is_some()
                || !self.reverse.is_empty()
                || (self.timeout != 0 && self.wait_ticks >= self.timeout as u32))
    }

    // Parked in a wait: the Sio parks slave-mode Normal-32 transfers while this
    // holds so they complete only when the adapter reverse-clocks a word.
    fn is_waiting(&self) -> bool {
        self.com == Com::WaitEvent
    }

    // Clock the next notification word into the GBA. The first call loads the
    // queued event (data-available `[0x99660028, idle]`, or `[0x99660027]` on
    // timeout); subsequent calls drain it one word per slave transfer. When the
    // last word is pushed, clock control returns to the GBA (back to WaitCmd) —
    // the game then issues RECV_DATA (0x26) in the normal GBA-master direction to
    // read the actual packet. `gba_out` (the GBA's 0x996600A8 ack) is consumed.
    fn reverse_clock(&mut self, gba_out: u32) -> Option<u32> {
        self.diag_rc_called = self.diag_rc_called.wrapping_add(1);
        if self.com != Com::WaitEvent {
            return None;
        }
        if self.reverse.is_empty() {
            if let Some(ev) = self.event.take() {
                self.reverse = ev;
            } else if self.timeout != 0 && self.wait_ticks >= self.timeout as u32 {
                // Two words like the data-available event: the notification, then
                // the idle word the GBA reads back while clocking its 0x996600A7
                // ack. A 1-word timeout left the ack to be misparsed as a fresh
                // WAIT command in WaitCmd, re-parking the FSM forever.
                self.reverse = vec![EVT_TIMEOUT, SPI_IDLE];
            } else {
                return None;
            }
        }
        let word = self.reverse.remove(0);
        // The adapter is now driving the bus as clock master → the GBA is the
        // clock slave. Flip `gpio_si` to slave polarity (mirror SO) so librfu's
        // slave handler's handshake_wait(0)/handshake_wait(1) both satisfy. Stays
        // set until the GBA resumes master mode and issues its next command.
        self.reversing = true;
        self.diag_rc_fired = self.diag_rc_fired.wrapping_add(1);
        // Log the reverse-clock direction too (gba_out = what the GBA shifted
        // back, e.g. the 0x996600A8 ack), so the trace shows the wake sequence
        // and whatever the game does right after — otherwise this whole phase is
        // invisible (it bypasses `transfer`).
        if self.trace.len() >= 4096 {
            self.trace.remove(0);
        }
        self.trace.push((gba_out, word));
        if self.reverse.is_empty() {
            self.com = Com::WaitCmd;
            self.wait_ticks = 0;
        }
        Some(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The documented NINTENDO-exchange table: (GBA word -> adapter reply).
    const HANDSHAKE: &[(u32, u32)] = &[
        (0x7FFF_494E, 0x0000_0000),
        (0xFFFF_494E, 0x494E_B6B1),
        (0xB6B1_494E, 0x494E_B6B1),
        (0xB6B1_544E, 0x544E_B6B1),
        (0xABB1_544E, 0x544E_ABB1),
        (0xABB1_4E45, 0x4E45_ABB1),
        (0xB1BA_4E45, 0x4E45_B1BA),
        (0xB1BA_4F44, 0x4F44_B1BA),
        (0xB0BB_4F44, 0x4F44_B0BB),
        (0xB0BB_8001, 0x8001_B0BB),
    ];

    fn run_handshake(a: &mut WirelessAdapter) {
        for &(sent, expect) in HANDSHAKE {
            assert_eq!(a.transfer(sent), expect, "handshake word {sent:#010x}");
        }
        assert_eq!(a.com, Com::WaitCmd);
    }

    // Drive a command through the FSM and collect the response words. Returns
    // (ack_word, payload_words).
    fn send_cmd(a: &mut WirelessAdapter, cmd: u8, payload: &[u32]) -> (u32, Vec<u32>) {
        let header = 0x9966_0000 | ((payload.len() as u32) << 8) | cmd as u32;
        assert_eq!(a.transfer(header), SPI_IDLE);
        for &w in payload {
            assert_eq!(a.transfer(w), SPI_IDLE);
        }
        // First reply word is the ACK (or the error word).
        let ack = a.transfer(SPI_IDLE);
        // Response length lives in byte 1 of the ACK.
        let len = ((ack >> 8) & 0xFF) as usize;
        let mut resp = Vec::new();
        for _ in 0..len {
            resp.push(a.transfer(SPI_IDLE));
        }
        (ack, resp)
    }

    // Drain the clock-reversal push while the adapter is parked in a wait: the
    // adapter seizes the clock and feeds the event/data words in one per slave
    // transfer. Mirrors what the host Sio does via `wants_reverse`/`reverse_clock`.
    fn drain_reverse(a: &mut WirelessAdapter) -> Vec<u32> {
        let mut out = Vec::new();
        while a.wants_reverse() {
            match a.reverse_clock(0) {
                Some(w) => out.push(w),
                None => break,
            }
        }
        out
    }

    #[test]
    fn handshake_matches_documented_table() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
    }

    #[test]
    fn reset_word_starts_handshake_low_half() {
        let mut a = WirelessAdapter::new();
        // A word whose low half isn't the NINTENDO pair keeps us in Reset.
        assert_eq!(a.transfer(0x0000_0000), 0);
        assert_eq!(a.com, Com::Reset);
        // The real opener flips us into the handshake.
        assert_eq!(a.transfer(0x7FFF_494E), 0);
        assert_eq!(a.com, Com::Handshake);
    }

    #[test]
    fn hello_and_version() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Hello: ACK with no payload.
        let (ack, resp) = send_cmd(&mut a, CMD_HELLO, &[]);
        assert_eq!(ack, 0x9966_0090); // 0x10 + 0x80, len 0
        assert!(resp.is_empty());
        // Version: one data word.
        let (ack, resp) = send_cmd(&mut a, CMD_SYSVER, &[]);
        assert_eq!(ack, 0x9966_0192); // 0x12 + 0x80, len 1
        assert_eq!(resp, vec![SYSVER_WORD]);
    }

    #[test]
    fn rehandshake_after_bye_recovers() {
        // FR/LG's adapter detection does: handshake → Bye (0x3d) → reset +
        // re-handshake. The adapter must restart the handshake when the opener
        // arrives while waiting for a command, replying 0 like a cold reset —
        // otherwise the game spins on 0x7FFF494E forever ("not connected
        // properly"). Regression guard for that recovery.
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Bye: bare ACK, back to waiting for a command.
        let (ack, _) = send_cmd(&mut a, CMD_BYE, &[]);
        assert_eq!(ack, ACK_BASE | CMD_BYE as u32); // 0x3d | 0x80
        assert_eq!(a.com, Com::WaitCmd);
        // The game re-opens the handshake: the opener is consumed like a cold
        // reset (reply 0, → Handshake), then the rest of the documented table
        // flows, ending back at WaitCmd.
        assert_eq!(a.transfer(0x7FFF_494E), 0);
        assert_eq!(a.com, Com::Handshake);
        for &(sent, expect) in &HANDSHAKE[1..] {
            assert_eq!(a.transfer(sent), expect, "re-handshake word {sent:#010x}");
        }
        assert_eq!(a.com, Com::WaitCmd);
        // And a normal command works after recovery.
        let (ack, _) = send_cmd(&mut a, CMD_HELLO, &[]);
        assert_eq!(ack, 0x9966_0090);
    }

    #[test]
    fn setup_stores_timeout_and_rtx() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // The value Pokémon / the download-play ROM use: 0x003C0420.
        let (ack, resp) = send_cmd(&mut a, CMD_SYSCFG, &[0x003C_0420]);
        assert_eq!(ack, 0x9966_0097); // 0x17 + 0x80, len 0
        assert!(resp.is_empty());
        assert_eq!(a.timeout, 0x20);
        assert_eq!(a.rtx_max, 0x04);
    }

    #[test]
    fn host_session_lifecycle() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Broadcast data, then start hosting → we get a device ID and go Host.
        send_cmd(&mut a, CMD_BCST_DATA, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(a.broadcast_payload(), [1, 2, 3, 4, 5, 6]);
        let (ack, _) = send_cmd(&mut a, CMD_HOST_START, &[]);
        assert_eq!(ack, 0x9966_0099); // 0x19 + 0x80
        assert_eq!(a.wifi, Wifi::ServingOpen);
        assert_ne!(a.host_devid, 0);
        // SYSSTAT reports the state (2 = serving/open in bits 24-31) + device ID.
        let (_, resp) = send_cmd(&mut a, CMD_SYSSTAT, &[]);
        assert_eq!(resp, vec![(2 << 24) | a.host_devid as u32]);
        // Poll for clients — none connected, empty list.
        let (ack, resp) = send_cmd(&mut a, CMD_HOST_ACCEPT, &[]);
        assert_eq!(ack & 0xFF, (CMD_HOST_ACCEPT | 0x80) as u32); // 0x1a + 0x80
        assert!(resp.is_empty());
        // Close the room → back to idle (no clients to keep alive).
        send_cmd(&mut a, CMD_HOST_STOP, &[]);
        assert_eq!(a.wifi, Wifi::Idle);
    }

    #[test]
    fn scan_finds_no_hosts() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_BCRD_START, &[]);
        let (ack, resp) = send_cmd(&mut a, CMD_BCRD_FETCH, &[]);
        assert_eq!(ack, 0x9966_009d); // 0x1d + 0x80, len 0
        assert!(resp.is_empty());
        send_cmd(&mut a, CMD_BCRD_STOP, &[]);
    }

    #[test]
    fn connect_then_fails_with_no_host() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Connect to some host ID — ACKed, but no such host exists.
        let (ack, _) = send_cmd(&mut a, CMD_CONNECT, &[0x0000_ABCD]);
        assert_eq!(ack, 0x9966_009f); // 0x1f + 0x80, len 0
        assert_eq!(a.wifi, Wifi::Idle);
        // ISCONNECTED reports failure.
        let (_, resp) = send_cmd(&mut a, CMD_ISCONNECTED, &[]);
        assert_eq!(resp, vec![CONN_FAILED]);
    }

    #[test]
    fn invalid_state_returns_error_frame() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // HOST_STOP while idle is a state error → error frame, not an ACK.
        let header = 0x9966_0000 | CMD_HOST_STOP as u32;
        assert_eq!(a.transfer(header), SPI_IDLE);
        assert_eq!(a.transfer(SPI_IDLE), ERR_WORD);
        assert_eq!(a.transfer(SPI_IDLE), ERR_BAD_STATE as u32);
        // FSM recovers and accepts the next command.
        let (ack, _) = send_cmd(&mut a, CMD_HELLO, &[]);
        assert_eq!(ack, 0x9966_0090);
    }

    #[test]
    fn transport_routes_normal32() {
        // Exercise the adapter through the LinkTransport seam the Sio uses.
        let mut a = WirelessAdapter::new();
        let t: &mut dyn LinkTransport = &mut a;
        assert!(t.is_master());
        assert!(!t.is_connected());
        assert_eq!(t.normal32_exchange(0x7FFF_494E), 0);
        assert_eq!(t.normal32_exchange(0xFFFF_494E), 0x494E_B6B1);
    }

    // ---- async peer seam ----

    #[test]
    fn scan_then_connect_to_discovered_host() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // The transport surfaces a host on the air.
        a.add_scanned_host(0xABCD, [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        // The scan lists it: metadata word (server id) + 6 broadcast words.
        send_cmd(&mut a, CMD_BCRD_START, &[]);
        let (_, resp) = send_cmd(&mut a, CMD_BCRD_FETCH, &[]);
        assert_eq!(resp, vec![0xABCD, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        send_cmd(&mut a, CMD_BCRD_STOP, &[]);
        // Connecting to its device ID moves us to Connecting...
        send_cmd(&mut a, CMD_CONNECT, &[0x0000_ABCD]);
        assert_eq!(a.wifi, Wifi::Connecting);
        let (_, resp) = send_cmd(&mut a, CMD_ISCONNECTED, &[]);
        assert_eq!(resp, vec![CONN_INPROGRESS]);
        // ...and the transport finalizes the connection (slot 1).
        a.client_set_connected(0x0042, 1);
        let (_, resp) = send_cmd(&mut a, CMD_CONCOMPL, &[]);
        assert_eq!(resp, vec![0x0042 | (1 << 16)]);
    }

    #[test]
    fn host_sees_injected_client() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        // A peer connects; the transport registers it.
        let cid = a.host_add_client();
        assert_ne!(cid, 0);
        // HOST_ACCEPT and SLOTSTAT now report the client. SLOTSTAT's leading
        // word is the next clientNumber, 0xFF here since the single slot is taken.
        let (_, resp) = send_cmd(&mut a, CMD_HOST_ACCEPT, &[]);
        assert_eq!(resp, vec![cid as u32]);
        let (_, resp) = send_cmd(&mut a, CMD_SLOTSTAT, &[]);
        assert_eq!(resp, vec![0xFF, cid as u32]);
        // Signal strength shows the slot-0 client present.
        let (_, resp) = send_cmd(&mut a, CMD_LINKPWR, &[]);
        assert_eq!(resp, vec![0x0000_00FF]);
    }

    #[test]
    fn system_status_reports_each_state() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Idle → state 0.
        let (_, r) = send_cmd(&mut a, CMD_SYSSTAT, &[]);
        assert_eq!(r[0] >> 24, 0);
        // Searching (3) after BroadcastReadStart.
        send_cmd(&mut a, CMD_BCRD_START, &[]);
        let (_, r) = send_cmd(&mut a, CMD_SYSSTAT, &[]);
        assert_eq!(r[0] >> 24, 3);
        // Serving/open (2) after StartHost; the device ID is in the low 16 bits.
        send_cmd(&mut a, CMD_HOST_START, &[]);
        let (_, r) = send_cmd(&mut a, CMD_SYSSTAT, &[]);
        assert_eq!(r[0] >> 24, 2);
        assert_eq!(r[0] & 0xFFFF, a.host_devid as u32);
        // Serving/closed (1) after EndHost with a client attached.
        a.host_add_client();
        send_cmd(&mut a, CMD_HOST_STOP, &[]);
        let (_, r) = send_cmd(&mut a, CMD_SYSSTAT, &[]);
        assert_eq!(r[0] >> 24, 1);
    }

    #[test]
    fn config_status_host_and_client() {
        // Host: 6 broadcast words + the raw Setup word + the trailer.
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_BCST_DATA, &[1, 2, 3, 4, 5, 6]);
        send_cmd(&mut a, CMD_SYSCFG, &[0x003C_0420]);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        let (ack, resp) = send_cmd(&mut a, CMD_CONFIGSTAT, &[]);
        assert_eq!((ack >> 8) & 0xFF, 8); // 8-word response as host
        assert_eq!(resp, vec![1, 2, 3, 4, 5, 6, 0x003C_0420, CONFIG_TRAILER]);
        // Client: six zeros + the trailer.
        let mut c = WirelessAdapter::new();
        run_handshake(&mut c);
        c.client_set_connected(0x1234, 0);
        let (ack, resp) = send_cmd(&mut c, CMD_CONFIGSTAT, &[]);
        assert_eq!((ack >> 8) & 0xFF, 7); // 7-word response as client
        assert_eq!(resp, vec![0, 0, 0, 0, 0, 0, CONFIG_TRAILER]);
    }

    #[test]
    fn ghost_send_resends_last_bytes() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        a.host_add_client();
        // A real 4-byte send is captured and remembered.
        send_cmd(&mut a, CMD_SEND_DATA, &[0x0000_0004, 0xDDCC_BBAA]);
        assert_eq!(a.take_outgoing(), Some(vec![0xAA, 0xBB, 0xCC, 0xDD]));
        // A header-only "ghost send" (1 byte, no data) resends the last byte —
        // non-empty, so the peer still gets a tick to talk back.
        send_cmd(&mut a, CMD_SEND_DATA, &[0x0000_0001]);
        assert_eq!(a.take_outgoing(), Some(vec![0xDD]));
    }

    #[test]
    fn client_signal_level_in_own_byte() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        // Connected as clientNumber 2 → signal reported in byte 2 only.
        a.client_set_connected(0xBEEF, 2);
        let (_, resp) = send_cmd(&mut a, CMD_LINKPWR, &[]);
        assert_eq!(resp, vec![0x00FF_0000]);
    }

    #[test]
    fn send_capture_and_receive_roundtrip_host() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        a.host_add_client();
        // Game sends 4 bytes (header byte-count = 4, one data word).
        send_cmd(&mut a, CMD_SEND_DATA, &[0x0000_0004, 0xDDCC_BBAA]);
        // The transport pulls the outgoing bytes (little-endian).
        assert_eq!(a.take_outgoing(), Some(vec![0xAA, 0xBB, 0xCC, 0xDD]));
        assert_eq!(a.take_outgoing(), None);
        // The transport delivers a 3-byte reply from the peer.
        a.deliver_packet(&[0x01, 0x02, 0x03]);
        let (_, resp) = send_cmd(&mut a, CMD_RECV_DATA, &[]);
        // Header: 3 bytes in slot 0 (<<8); then the packet word (LE, padded).
        assert_eq!(resp, vec![0x0000_0300, 0x0003_0201]);
    }

    #[test]
    fn wait_wakes_on_delivered_packet() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        a.host_add_client();
        // Configure a timeout so the wait isn't infinite, then issue WAIT.
        send_cmd(&mut a, CMD_SYSCFG, &[0x0000_0004]); // 4-frame timeout
        let (ack, _) = send_cmd(&mut a, CMD_WAIT, &[]);
        assert_eq!(ack, 0x9966_00a7); // 0x27 + 0x80
        assert_eq!(a.com, Com::WaitEvent);
        // No event yet: the adapter doesn't want the clock, and a stray poll idles.
        assert!(!a.wants_reverse());
        assert_eq!(a.transfer(SPI_IDLE), SPI_IDLE);
        // A packet arrives → the adapter reverse-clocks a data-available event.
        a.deliver_packet(&[0xAA]);
        let ev = drain_reverse(&mut a);
        assert_eq!(ev, vec![EVT_DATA_AVAIL, SPI_IDLE]);
        assert_eq!(a.com, Com::WaitCmd); // clock control returned to the GBA
    }

    #[test]
    fn wait_times_out_with_no_event() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        send_cmd(&mut a, CMD_SYSCFG, &[0x0000_0002]); // 2-frame timeout
        send_cmd(&mut a, CMD_WAIT, &[]);
        // No event; idle until the timeout elapses.
        assert!(!a.wants_reverse());
        a.update(1);
        assert!(!a.wants_reverse());
        a.update(1); // now wait_ticks >= timeout
        let ev = drain_reverse(&mut a);
        assert_eq!(ev, vec![EVT_TIMEOUT, SPI_IDLE]);
        assert_eq!(a.com, Com::WaitCmd);
    }

    #[test]
    fn disconnect_event_wakes_wait() {
        let mut a = WirelessAdapter::new();
        run_handshake(&mut a);
        send_cmd(&mut a, CMD_HOST_START, &[]);
        a.host_add_client();
        send_cmd(&mut a, CMD_WAIT, &[]);
        a.disconnect_peer();
        assert_eq!(a.peer_devid, 0);
        let ev = drain_reverse(&mut a);
        assert_eq!(ev, vec![EVT_DISCONNECT, 0x0000_000F, SPI_IDLE]);
    }
}
