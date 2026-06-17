// GBA Wireless Adapter (RFU) packet relay over the SignalRoom WebSocket.
//
// Unlike the link cable (a tight, bit-synchronous, data-dependent protocol that
// can't survive WAN latency), the wireless adapter is packet-based and buffered
// — games send/poll data packets and tolerate RF-scale latency and jitter. That
// makes it the right transport for ONLINE play: each machine runs a SINGLE GBA
// and we just forward the adapter's packets to the peer. One screen, one
// simulation, works LAN → WAN.
//
// The Rust side (wireless.rs) is the adapter HLE; this class is the network
// half. It drives the wl_* bridge each frame (via `pump`, called by
// WasmEmulator.runFrame) and relays over the room:
//   host:   wlBroadcast → 'wl-beacon';  'wl-connect' → wlHostAddClient → 'wl-accept'
//   client: 'wl-beacon' → wlAddScannedHost;  wlPendingConnect → 'wl-connect';
//           'wl-accept' → wlClientSetConnected
//   both:   wlTakeOutgoing → 'wl-data' → wlDeliverPacket
// Host/client role is decided by the GAME (Union Room "create" vs "join"), not
// by who made the network room — the transport reacts to whatever each side does.

// The wl_* surface this needs; WasmEmulator satisfies it structurally.
export interface WirelessBridge {
  wlUpdate(frames: number): void;
  wlBroadcast(): Uint32Array | undefined; // [devid, w0..w5]; devid 0 = not hosting
  wlPendingConnect(): number;             // host devid to connect to, or -1
  wlTakeOutgoing(): Uint8Array | undefined;
  wlAddScannedHost(devid: number, data: Uint32Array): void;
  wlHostAddClient(): number;              // assigns + returns the client's devid
  wlClientSetConnected(devid: number, clnum: number): void;
  wlDeliverPacket(bytes: Uint8Array): void;
  wlDisconnectPeer(): void;
}

interface WireMsg {
  type: 'self' | 'peer-join' | 'peer-leave' | 'wl-beacon' | 'wl-connect' | 'wl-accept' | 'wl-data';
  to?: string;
  from?: string;
  peerId?: string;
  peers?: string[];
  payload?: unknown;
}

export interface WirelessOptions {
  roomId: string;
  // Room creator → host (broadcasts + accepts connections, never connects out);
  // joiner → client (scans + connects, never broadcasts). The Union Room is a
  // mesh where every GBA both hosts and connects, which over a point-to-point
  // relay makes both sides connect out and then deadlock in WAIT (each expecting
  // the other to send first). Forcing one host + one client gives a clear data
  // driver and breaks that symmetry.
  isHost: boolean;
  signalingBase?: string;
}

// How often (in frames) the host re-announces its beacon while not yet
// connected. The client only needs it to land once during its scan window.
const BEACON_EVERY = 10;

export class WirelessTransport {
  private ws: WebSocket | null = null;
  private peerId: string | null = null;
  private beaconTick = 0;
  private isHost = true;     // role in the relay (creator = host)
  private connected = false; // a peer has joined the network room
  // Diagnostics surfaced to the UI.
  packetsOut = 0;
  packetsIn = 0;
  lastEvent = '';

  onPeerJoin: ((id: string) => void) | null = null;
  onPeerLeave: (() => void) | null = null;
  onError: ((e: Error) => void) | null = null;

  // eslint-disable-next-line no-unused-vars
  constructor(private bridge: WirelessBridge) {}

  // ---- LinkTransportLike (so it can sit in emu.io.sio.transport) ----------
  isMaster(): boolean { return true; }
  isConnected(): boolean {
    return !!this.ws && this.ws.readyState === WebSocket.OPEN && this.peerId !== null;
  }

  async connect(opts: WirelessOptions): Promise<void> {
    this.isHost = opts.isHost;
    const base = opts.signalingBase ?? defaultSignalingBase();
    const url = `${base}/api/signal/${encodeURIComponent(opts.roomId)}`;
    await this.openWs(url);
  }

  async disconnect(): Promise<void> {
    if (this.ws) { try { this.ws.close(); } catch { /* */ } this.ws = null; }
    this.peerId = null;
    this.connected = false;
  }

  // Called once per emulated frame by WasmEmulator.runFrame (via pumpLink).
  pump(): void {
    this.bridge.wlUpdate(1);
    if (!this.peerId) return;

    // Host only: re-announce the beacon periodically (while hosting, devid != 0).
    if (this.isHost && this.beaconTick++ % BEACON_EVERY === 0) {
      const b = this.bridge.wlBroadcast();
      if (b && b.length >= 7 && b[0] !== 0) {
        this.send({ type: 'wl-beacon', to: this.peerId, payload: { devid: b[0], data: Array.from(b.subarray(1, 7)) } });
      }
    }

    // Client only: the game asked to connect to a discovered host → relay it.
    // (Drain the pending flag regardless so it doesn't accumulate on the host.)
    const reqid = this.bridge.wlPendingConnect();
    if (!this.isHost && reqid >= 0) {
      this.send({ type: 'wl-connect', to: this.peerId, payload: { reqid } });
      this.lastEvent = `connect→${reqid.toString(16)}`;
    }

    // Both: drain any queued outgoing data packets and relay them.
    let pkt = this.bridge.wlTakeOutgoing();
    while (pkt && pkt.length) {
      this.send({ type: 'wl-data', to: this.peerId, payload: { bytes: Array.from(pkt) } });
      this.packetsOut++;
      pkt = this.bridge.wlTakeOutgoing();
    }
  }

  // ------------------------------------------------------------------------

  private openWs(url: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      this.ws = ws;
      ws.onopen = () => resolve();
      ws.onmessage = (ev) => this.onWire(ev.data as string);
      ws.onerror = () => { const e = new Error('signaling WebSocket error'); this.onError?.(e); reject(e); };
      ws.onclose = () => {
        if (this.peerId) { this.bridge.wlDisconnectPeer(); this.onPeerLeave?.(); }
        this.peerId = null;
        this.connected = false;
      };
    });
  }

  private onPeerConnected(id: string): void {
    if (this.peerId) return;
    this.peerId = id;
    this.connected = true;
    this.lastEvent = 'peer joined';
    this.onPeerJoin?.(id);
  }

  private onWire(raw: string): void {
    let msg: WireMsg;
    try { msg = JSON.parse(raw); } catch { return; }
    switch (msg.type) {
      case 'self':
        if (msg.peers && msg.peers.length > 0) this.onPeerConnected(msg.peers[0]);
        break;
      case 'peer-join':
        if (msg.peerId) this.onPeerConnected(msg.peerId);
        break;
      case 'peer-leave':
        if (msg.peerId && msg.peerId === this.peerId) {
          this.bridge.wlDisconnectPeer();
          this.peerId = null;
          this.connected = false;
          this.lastEvent = 'peer left';
          this.onPeerLeave?.();
        }
        break;
      case 'wl-beacon': {
        // Only the client scans for hosts; the host ignores beacons so its game
        // never tries to connect out (it stays a pure host).
        if (this.isHost) break;
        const p = msg.payload as { devid: number; data: number[] };
        this.bridge.wlAddScannedHost(p.devid, Uint32Array.from(p.data));
        this.lastEvent = `saw host ${p.devid.toString(16)}`;
        break;
      }
      case 'wl-connect': {
        // Only the host accepts connections: register the client + tell it the
        // assigned devid.
        if (!this.isHost) break;
        const clientDevid = this.bridge.wlHostAddClient();
        this.send({ type: 'wl-accept', to: this.peerId!, payload: { devid: clientDevid, clnum: 0 } });
        this.lastEvent = `accepted client ${clientDevid.toString(16)}`;
        break;
      }
      case 'wl-accept': {
        // Only the client finalizes its connection.
        if (this.isHost) break;
        const p = msg.payload as { devid: number; clnum: number };
        this.bridge.wlClientSetConnected(p.devid, p.clnum);
        this.lastEvent = `connected as ${p.devid.toString(16)}`;
        break;
      }
      case 'wl-data': {
        const p = msg.payload as { bytes: number[] };
        this.bridge.wlDeliverPacket(Uint8Array.from(p.bytes));
        this.packetsIn++;
        break;
      }
    }
  }

  private send(msg: WireMsg): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    try { this.ws.send(JSON.stringify(msg)); } catch { /* recovers next frame */ }
  }
}

function defaultSignalingBase(): string {
  const { protocol, host } = window.location;
  return `${protocol === 'https:' ? 'wss:' : 'ws:'}//${host}`;
}
