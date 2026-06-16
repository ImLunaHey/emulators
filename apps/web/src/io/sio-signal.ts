import type { MultiplayResult } from './sio';

// Link transport that relays SIO state through the same WebSocket
// we use for signaling — no RTCPeerConnection involved. Backed by
// the SignalRoom Durable Object (src/worker.ts). One WS per peer
// per room; the DO forwards `{type:'state', to: peerId}` messages
// to the addressed peer.
//
// This drives the Rust/wasm core (not the old TS Sio) through the
// `LinkBridge` adapter surface (implemented by WasmEmulator): the core
// owns the SIO state machine, and the bridge exposes only the four async
// hooks plus a per-frame `pump`. The WebRTC / WebSocket / room / peer
// machinery is unchanged from the TS-Sio version — only the parts that
// read/write the Sio now go through the bridge.
//
// Why not direct WebRTC?
// - Firefox in various privacy configs silently refuses to gather
//   any ICE candidates, killing the DataChannel before it can open.
// - TURN requires Cloudflare Realtime to be provisioned and routable
//   from the user's network; both of those have failure modes that
//   surface as opaque "ICE failed" errors with no real recovery.
// - For 1:1 link-cable traffic (single small message every 33 ms)
//   the per-byte cost of relaying through CF's edge is negligible,
//   and the connect-success rate goes from "depends on your network
//   and browser" to "if you can load the page, the link works."
//
// Latency budget vs direct peer-to-peer: ~20-50 ms one-way through
// CF's nearest colo, vs ~5-20 ms over LAN with raw DataChannel. Fine
// for Pokemon-style turn-based traffic; noticeable for real-time
// racing, but lockstep / input-delay is the right fix for racing
// anyway — direct-P2P only buys us a few ms there.

const TICK_MS = 33;

// The async link surface the SignalTransport drives. WasmEmulator
// implements this by forwarding to the WasmGba bridge methods; the host
// is the source of truth for SIO state.
export interface LinkBridge {
  // Set the live link state: SD comes from `connected`, SI/ID from `master`.
  linkSetState(connected: boolean, master: boolean): void;
  // Poll the master's outgoing multiplay payload. Returns the 16-bit
  // SIOMLT_SEND value once (take semantics) after a master transfer kicks
  // off over a connected link, or -1 when there's nothing to send.
  linkTakeOutgoing(): number;
  // Non-consuming read of the current SIOMLT_SEND word (0..0xFFFF). The slave
  // uses this to answer the master's mlt-req with its own data — it never
  // masters a transfer, so linkTakeOutgoing() always returns -1 for it.
  linkPeekOutgoing(): number;
  // Master-side completion: deliver the synchronized 4-slot result.
  linkDeliver(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
  // Slave-side: apply the remote master's broadcast.
  linkApplyRemote(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
}

type StateMsg = {
  // Master-only fields — when the master completed a Multi-play transfer it
  // snapshots SIOMULTI[0..3] and a monotonic seq so the slave can apply the
  // same values + fire its IRQ. seq=0 means "no transfer has happened yet";
  // receivers watch for it advancing.
  seq: number;
  m0: number;
  m1: number;
  m2: number;
  m3: number;
};

interface WireMsg {
  // self / peer-join / peer-leave are server-emitted room control.
  // mlt-req — master asks slave for a synchronized response to a
  //   Multi-play transfer that just kicked off. Carries the master's
  //   SIOMLT_SEND and a unique request id so the response can be
  //   correlated even if the master fires several transfers in flight.
  // mlt-resp — slave's reply with the multiplay result both ends will
  //   adopt. The slave applies the same snapshot to its own core at
  //   send time so the two sides converge.
  // state — master's broadcast of its completed transfer snapshot; the
  //   slave applies it when the seq advances.
  type: 'self' | 'peer-join' | 'peer-leave' | 'state' | 'mlt-req' | 'mlt-resp';
  to?: string;
  from?: string;
  peerId?: string;
  peers?: string[];
  payload?: unknown;
}

export interface SignalOptions {
  roomId: string;
  isMaster: boolean;
  // Optional override of the signaling base URL. Defaults to the
  // current page's origin. Useful for tests pointing at a local mock.
  signalingBase?: string;
}

export class SignalTransport {
  private ws: WebSocket | null = null;
  private peerId: string | null = null;
  private tickHandle: ReturnType<typeof setInterval> | null = null;
  private master = true;
  // Last peer transferSeq we applied (slave side). When the master's
  // broadcast seq advances we mirror their SIOMULTI snapshot into our core
  // and let it fire SIO IRQ. Tracked unsigned mod 2^32.
  private lastAppliedSeq = 0;

  // Pending Multi-play requests we've sent to the peer, keyed by reqId. The
  // callback runs when the matching mlt-resp arrives, or when the lockstep
  // timeout expires (whichever first). The map keeps us correct even if the
  // master fires several transfers before the first response arrives.
  private mltReqSeq = 0;
  private pendingReqs = new Map<number, (r: MultiplayResult) => void>();
  // Latency ceiling for a lockstep round-trip. If the response hasn't
  // arrived by then, we deliver the "no peer" result so the core's transfer
  // can complete instead of relying on its cycle-budget timeout.
  private static readonly REQ_TIMEOUT_MS = 250;

  // Master's most recently delivered transfer snapshot + a monotonic seq.
  // Re-broadcast every tick so a slave that missed the lockstep round-trip
  // catches up via the seq-watch path (onWire 'state'). seq=0 = no transfer
  // yet. Updated by `requestMultiplay`'s settle on the master side.
  private lastBroadcast: StateMsg = { seq: 0, m0: 0xFFFF, m1: 0xFFFF, m2: 0xFFFF, m3: 0xFFFF };

  onPeerJoin: ((peerId: string) => void) | null = null;
  onPeerLeave: ((peerId: string) => void) | null = null;
  onError: ((err: Error) => void) | null = null;

  // The async link bridge into the wasm core. Replaces the old TS `Sio`.
  // eslint-disable-next-line no-unused-vars
  constructor(private link: LinkBridge) {}

  async connect(opts: SignalOptions): Promise<void> {
    this.master = opts.isMaster;
    // Announce link state to the core. SD stays low until a peer actually
    // joins (set in onWire); SI/ID reflect our master/slave role now.
    this.link.linkSetState(false, this.master);
    const base = opts.signalingBase ?? defaultSignalingBase();
    const url = `${base}/api/signal/${encodeURIComponent(opts.roomId)}`;
    console.log('[link] connecting', url);
    await this.openWs(url);
    this.tickHandle = setInterval(() => this.broadcast(), TICK_MS);
  }

  async disconnect(): Promise<void> {
    if (this.tickHandle !== null) { clearInterval(this.tickHandle); this.tickHandle = null; }
    if (this.ws) { try { this.ws.close(); } catch { /* */ } this.ws = null; }
    // Resolve any pending lockstep requests with the "no-peer" result so the
    // core's transfer can finish instead of waiting on its timeout.
    for (const cb of this.pendingReqs.values()) {
      cb({ d0: 0xFFFF, d1: 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: true });
    }
    this.pendingReqs.clear();
    this.peerId = null;
    this.lastAppliedSeq = 0;
    // Tell the core the link is down; it falls back to single-player SIOCNT.
    this.link.linkSetState(false, this.master);
  }

  isConnected(): boolean {
    // Connectivity is "the WebSocket is alive and the peer is still in
    // the room", not "we recently received data". When two tabs share
    // a browser only one is foreground at a time; the background tab's
    // 33 ms broadcast tick gets throttled (sometimes to 60+ s between
    // ticks), so a freshness-based check makes SD flicker on/off and
    // games like Mario Kart see no cable. The DO emits peer-leave the
    // moment the peer's WS actually drops — that's our real disconnect
    // signal, and it's plenty.
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return false;
    return this.peerId !== null;
  }
  isMaster(): boolean { return this.master; }

  // Called once per emulated frame by the adapter (WasmEmulator.runFrame).
  // Polls the core for an outgoing master multiplay payload; when one is
  // pending, kicks off the lockstep request to the peer. The slave side's
  // delivery is event-driven (onWire), so pump only matters for the master.
  pump(): void {
    if (!this.master) return;
    const out = this.link.linkTakeOutgoing();
    if (out < 0) return;            // nothing to send this frame
    this.requestMultiplay(out & 0xFFFF);
  }

  // Master side: a multiplay transfer kicked off in the core. Ask the peer
  // for a synchronized response; when it arrives (or times out), deliver the
  // 4-slot result back into the core via the bridge. If we have no peer, the
  // core's own cycle-budget timeout completes the transfer with error.
  private requestMultiplay(localData: number): void {
    if (!this.isConnected()) {
      // No peer: let the core's timeout fallback handle it. Nothing to do.
      return;
    }
    const reqId = ++this.mltReqSeq;
    let done = false;
    const settle = (r: MultiplayResult) => {
      if (done) return;
      done = true;
      this.pendingReqs.delete(reqId);
      this.link.linkDeliver(r.d0, r.d1, r.d2, r.d3, r.error);
      // Stage this snapshot for the periodic 'state' broadcast so a slave
      // that missed the mlt-resp round-trip catches up via seq-watch.
      this.lastBroadcast = {
        seq: (this.lastBroadcast.seq + 1) >>> 0,
        m0: r.d0 & 0xFFFF, m1: r.d1 & 0xFFFF, m2: r.d2 & 0xFFFF, m3: r.d3 & 0xFFFF,
      };
    };
    this.pendingReqs.set(reqId, settle);
    setTimeout(() => {
      // Timeout: treat as "no peer data" and complete with error so the
      // game's transfer loop unsticks.
      settle({ d0: localData & 0xFFFF, d1: 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: true });
    }, SignalTransport.REQ_TIMEOUT_MS);
    try {
      this.ws!.send(JSON.stringify({
        type: 'mlt-req', to: this.peerId,
        payload: { reqId, masterData: localData & 0xFFFF },
      }));
    } catch {
      settle({ d0: localData & 0xFFFF, d1: 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: true });
    }
  }

  // ----------------------------------------------------------------

  private openWs(url: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      this.ws = ws;
      ws.onopen = () => { console.log('[link] WS open'); resolve(); };
      ws.onmessage = (ev) => this.onWire(ev.data as string);
      ws.onerror = () => {
        const err = new Error('signaling WebSocket error');
        this.onError?.(err);
        reject(err);
      };
      ws.onclose = () => {
        console.log('[link] WS closed');
        if (this.peerId) this.onPeerLeave?.(this.peerId);
        this.peerId = null;
        this.link.linkSetState(false, this.master);
      };
    });
  }

  private onPeerConnected(peerId: string): void {
    this.peerId = peerId;
    // Peer is in the room: SD goes high. SI/ID from our role.
    this.link.linkSetState(true, this.master);
    this.onPeerJoin?.(peerId);
  }

  private onWire(raw: string): void {
    let msg: WireMsg;
    try { msg = JSON.parse(raw); } catch { return; }
    switch (msg.type) {
      case 'self':
        // Anyone already in the room becomes our 1:1 peer.
        if (msg.peers && msg.peers.length > 0) {
          this.onPeerConnected(msg.peers[0]);
        }
        break;
      case 'peer-join':
        if (msg.peerId && !this.peerId) {
          this.onPeerConnected(msg.peerId);
        }
        break;
      case 'peer-leave':
        if (msg.peerId && msg.peerId === this.peerId) {
          this.onPeerLeave?.(msg.peerId);
          this.peerId = null;
          this.link.linkSetState(false, this.master);
          // Cancel pending lockstep requests so the master's transfer can
          // unstick. They get the "no peer" result and the core's complete()
          // finishes with error.
          for (const cb of this.pendingReqs.values()) {
            cb({ d0: 0xFFFF, d1: 0xFFFF, d2: 0xFFFF, d3: 0xFFFF, error: true });
          }
          this.pendingReqs.clear();
        }
        break;
      case 'state':
        if (msg.from === this.peerId && msg.payload) {
          const p = msg.payload as StateMsg;
          // Slave-side: when the master's transferSeq advances, mirror their
          // SIOMULTI snapshot into our core and let it fire IRQ. We only act
          // as slave here; the master ignores its peer's state broadcasts.
          if (!this.master && p.seq !== 0 && p.seq !== this.lastAppliedSeq) {
            this.lastAppliedSeq = p.seq;
            this.link.linkApplyRemote(p.m0, p.m1, p.m2, p.m3, false);
          }
        }
        break;
      case 'mlt-req':
        // Slave side: master kicked off a Multi-play transfer and is asking
        // what we'd respond with. Read our *current* SIOMLT_SEND from the
        // core (via take-outgoing semantics is wrong here — the slave never
        // starts a transfer; instead we read the last value the core staged).
        // We surface the slave's outgoing word through linkTakeOutgoing too:
        // the slave's game writes SIOMLT_SEND, but the core only exposes it
        // to JS on a master transfer. For the relay path the slave responds
        // with the master's data echoed into slot 0 + its own payload in
        // slot 1, applies it locally so SIOMULTI/IRQ converge with master's,
        // and replies so master's deliver() resolves.
        if (this.master) break;
        if (msg.from === this.peerId && msg.payload) {
          const p = msg.payload as { reqId: number; masterData: number };
          // The slave's outgoing word is whatever the game last wrote to
          // SIOMLT_SEND. The slave never masters a transfer, so it never stages
          // an `outgoing` payload (linkTakeOutgoing would return -1) — we read
          // the live register non-destructively instead, so the master
          // actually receives the slave's data (its half of a trade) rather
          // than the 0xFFFF "no data" sentinel.
          const slaveData = this.link.linkPeekOutgoing() & 0xFFFF;
          const result: MultiplayResult = {
            d0: p.masterData & 0xFFFF,
            d1: slaveData,
            d2: 0xFFFF,
            d3: 0xFFFF,
            error: false,
          };
          // Apply locally before replying so our SIO IRQ on this transfer
          // fires at the moment of receipt, mirroring how real hardware's
          // slave latches and IRQs together.
          this.link.linkApplyRemote(result.d0, result.d1, result.d2, result.d3, false);
          try {
            this.ws!.send(JSON.stringify({
              type: 'mlt-resp', to: this.peerId,
              payload: { reqId: p.reqId, result },
            }));
          } catch { /* WS may have closed mid-handler */ }
        }
        break;
      case 'mlt-resp':
        if (msg.from === this.peerId && msg.payload) {
          const p = msg.payload as { reqId: number; result: MultiplayResult };
          const cb = this.pendingReqs.get(p.reqId);
          if (cb) cb(p.result);
        }
        break;
    }
  }

  // Master broadcasts its last completed transfer snapshot (staged by
  // `requestMultiplay`'s settle) so a slave that missed the lockstep
  // round-trip can still catch up via the seq-watch path.
  private broadcast(): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    if (!this.peerId) return;
    // Only the master has a meaningful snapshot to broadcast; the slave's
    // tick is a no-op (the master ignores slave state anyway).
    if (!this.master) return;
    try {
      this.ws.send(JSON.stringify({ type: 'state', to: this.peerId, payload: this.lastBroadcast }));
    } catch { /* will recover next tick */ }
  }
}

function defaultSignalingBase(): string {
  const { protocol, host } = window.location;
  const wsProto = protocol === 'https:' ? 'wss:' : 'ws:';
  return `${wsProto}//${host}`;
}
