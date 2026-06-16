// wasmEmulator.ts — drop-in replacement for the old TS `Emulator` class,
// backed by the Rust/wasm core (`@emulators/gba`).
//
// The React app reads the emulator through a fixed surface (Screen,
// PlayerPage, DebugPanel, LinkPanel, SaveStatesPanel, CheatsPanel). This
// adapter re-exposes that surface and forwards every operation to the wasm
// `WasmGba`, so `App.tsx` hard-swaps the core just by constructing a
// `WasmEmulator` where it used to `new Emulator()`.
//
// Structure: `WasmEmulator` owns the wasm handle + lifecycle, and the nested
// surfaces the UI pokes at (`keypad`, `ppu`, `sound`, `cpu`, `dma`, `timers`,
// `irq`, `save`, `bus`, `io`) are small facet classes constructed with a
// back-reference to the core via the internal `WasmCore` interface.
//
// Async-init timing: wasm `init()` is async, so the `WasmGba` doesn't exist
// synchronously. The constructor kicks off `init()` and exposes
// `ready: Promise<void>`. Until it resolves: `runFrame()` is a no-op returning
// zeroed stats, `ppu.frame` returns a zeroed 240×160×4 buffer, `loadRom()`
// stashes its bytes to replay on ready, cheats/turbo set early are replayed,
// and every debug getter degrades to zero (empty snapshot).
//
// Known gaps vs. the pure-TS Emulator:
//   - The link-cable `transport` defaults to an inert disconnected stub; the
//     real WebRTC `SignalTransport` is assigned by LinkPanel.connectTo and
//     driven each frame via `pumpLink()`.
//   - `bus.write*` are no-ops (the UI's memory views are read-only; IwramWatch
//     uses the native wasm watch via `setWatch/clearWatch/watchLog`).

// @ts-ignore — the wasm pkg ships its own .d.ts but the relative import may
// resolve before pkg is built in some tooling; tolerate either way.
import init, { WasmGba } from '@emulators/gba';
import type { Cheat } from '../io/cheats';

const FB_LEN = 240 * 160 * 4;

// Shape of the JSON returned by `g.debug_state()`. Every field is optional so
// a partial / missing snapshot (pre-ready) degrades to zeros.
interface DebugSnap {
  r?: number[];
  cpsr?: number;
  halted?: boolean;
  dispcnt?: number;
  dispstat?: number;
  vcount?: number;
  mosaic?: number;
  bgcnt?: number[];
  bg_hofs?: number[];
  bg_vofs?: number[];
  win0_h?: number;
  win0_v?: number;
  win1_h?: number;
  win1_v?: number;
  win_in?: number;
  win_out?: number;
  bldcnt?: number;
  bldalpha?: number;
  dma?: DmaChannel[];
  timers?: TimerChannel[];
  ie?: number;
  iflag?: number;
  ime?: number;
  soundcnt_x?: number;
  soundcnt_h?: number;
  count_a?: number;
  count_b?: number;
  mlt_send?: number;
  multi0?: number;
  multi1?: number;
  transfer_seq?: number;
}

interface DmaChannel { enabled: boolean; src: number; dst: number; count: number }
interface TimerChannel { enabled: boolean; reload: number; counter: number; prescale: number }

// Minimal link transport the LinkPanel debug strip pokes at. `pump` (optional)
// is called by the adapter's runFrame each frame so an async link transport
// (SignalTransport) can poll the core for an outgoing multiplay payload and
// forward peer results back in.
export interface LinkTransportLike {
  isMaster(): boolean;
  isConnected(): boolean;
  pump?(): void;
}

// The slice of `WasmEmulator` the facet adapters depend on. Keeps the facets
// decoupled from the full class and avoids exposing the wasm handle widely.
interface WasmCore {
  /** The live wasm instance, or undefined until `ready` resolves. */
  readonly gba: WasmGba | undefined;
  /** A ~once-per-frame cached parse of `debug_state()` (zeros when not ready). */
  snap(): DebugSnap;
  /** Forward an autofire mask now, or stash it to replay on ready. */
  applyTurbo(mask: number): void;
  /** Shared zeroed framebuffer handed out before the core is ready. */
  readonly zeroFb: Uint8Array;
  /** Zero-copy view over the wasm framebuffer (zeroFb until ready). */
  frameView(): Uint8Array;
}

// ---------------------------------------------------------------- facets

// JS-side input state. press/release maintain a held bitmask which runFrame()
// pushes to wasm; turboMask is forwarded so the core can autofire. Bit layout
// matches the `Key` enum (A=0,B=1,SELECT=2,START=3,RIGHT=4,LEFT=5,UP=6,DOWN=7,
// R=8,L=9). turboPhase/tickTurbo/read16 are vestigial — they exist only to
// satisfy the `Keypad` structural type the UI passes to Gamepad /
// useKeypadHighlight; the real autofire + register read happen in the core.
class WasmKeypad {
  pressed = 0;
  turboPhase = 0;
  private _turbo = 0;
  constructor(private core: WasmCore) {}
  get turboMask(): number { return this._turbo; }
  set turboMask(mask: number) { this._turbo = mask; this.core.applyTurbo(mask); }
  press(k: number): void { this.pressed |= 1 << k; }
  release(k: number): void { this.pressed &= ~(1 << k); }
  tickTurbo(): void {}
  read16(): number { return ~this.pressed & 0x3ff; }
}

// Framebuffer + the PPU registers the DebugPanel's IO view reads. camelCase
// names map from the snapshot's snake_case keys (bgHOFS ← bg_hofs, etc.).
class WasmPpu {
  // frameDone: plain mailbox flag for the host frame-step paths.
  frameDone = false;
  constructor(private core: WasmCore) {}
  get frame(): Uint8Array { return this.core.frameView(); }
  get dispcnt(): number { return this.core.snap().dispcnt ?? 0; }
  get dispstat(): number { return this.core.snap().dispstat ?? 0; }
  get vcount(): number { return this.core.snap().vcount ?? 0; }
  get mosaic(): number { return this.core.snap().mosaic ?? 0; }
  get bgcnt(): number[] { return this.core.snap().bgcnt ?? [0, 0, 0, 0]; }
  get bgHOFS(): number[] { return this.core.snap().bg_hofs ?? [0, 0, 0, 0]; }
  get bgVOFS(): number[] { return this.core.snap().bg_vofs ?? [0, 0, 0, 0]; }
  get win0H(): number { return this.core.snap().win0_h ?? 0; }
  get win0V(): number { return this.core.snap().win0_v ?? 0; }
  get win1H(): number { return this.core.snap().win1_h ?? 0; }
  get win1V(): number { return this.core.snap().win1_v ?? 0; }
  get winIn(): number { return this.core.snap().win_in ?? 0; }
  get winOut(): number { return this.core.snap().win_out ?? 0; }
  get bldcnt(): number { return this.core.snap().bldcnt ?? 0; }
  get bldalpha(): number { return this.core.snap().bldalpha ?? 0; }
}

class WasmSound {
  readonly sampleRate = 32768;
  constructor(private core: WasmCore) {}
  drainOutput(): Float32Array { return this.core.gba ? this.core.gba.drain_audio() : new Float32Array(0); }
  get countA(): number { return this.core.snap().count_a ?? 0; }
  get countB(): number { return this.core.snap().count_b ?? 0; }
  get soundcntX(): number { return this.core.snap().soundcnt_x ?? 0; }
  get soundcntH(): number { return this.core.snap().soundcnt_h ?? 0; }
}

class WasmCpuDebug {
  constructor(private core: WasmCore) {}
  get state(): { r: number[]; cpsr: number; halted: boolean } {
    const s = this.core.snap();
    const r = s.r ?? [];
    const out = Array.from({ length: 16 }, (_, i) => r[i] ?? 0);
    return { r: out, cpsr: s.cpsr ?? 0, halted: s.halted ?? false };
  }
}

class WasmDmaDebug {
  constructor(private core: WasmCore) {}
  get ch(): DmaChannel[] {
    const d = this.core.snap().dma ?? [];
    return Array.from({ length: 4 }, (_, i) => d[i] ?? { enabled: false, src: 0, dst: 0, count: 0 });
  }
}

class WasmTimersDebug {
  constructor(private core: WasmCore) {}
  get ch(): TimerChannel[] {
    const t = this.core.snap().timers ?? [];
    return Array.from({ length: 4 }, (_, i) => t[i] ?? { enabled: false, reload: 0, counter: 0, prescale: 0 });
  }
}

class WasmIrqDebug {
  constructor(private core: WasmCore) {}
  get ie(): number { return this.core.snap().ie ?? 0; }
  get iflag(): number { return this.core.snap().iflag ?? 0; }
  get ime(): number { return this.core.snap().ime ?? 0; }
}

// Cartridge save chip. `data` is a read-only copy from wasm; `reset()` writes
// through to erase it (mutating `data` would only touch the copy).
class WasmSave {
  onChange: (() => void) | null = null;
  constructor(private core: WasmCore) {}
  loadSave(bytes: Uint8Array): void { this.core.gba?.load_save_ram(bytes); }
  get data(): Uint8Array { return this.core.gba ? this.core.gba.save_ram() : new Uint8Array(0); }
  reset(): void { this.core.gba?.reset_save(); }
}

// Debug memory reads (DebugPanel). Writes are no-ops: the UI's memory views are
// read-only, and IwramWatch uses the native wasm watch (setWatch/watchLog).
// read* are arrow fields so they stay bound if the UI destructures them.
class WasmBus {
  constructor(private core: WasmCore) {}
  read8 = (a: number): number => this.core.gba?.read8(a) ?? 0;
  read16 = (a: number): number => this.core.gba?.read16(a) ?? 0;
  write8 = (_a: number, _v: number): void => {};
  write16 = (_a: number, _v: number): void => {};
  write32 = (_a: number, _v: number): void => {};
  get vram(): Uint8Array { return this.core.gba ? this.core.gba.vram() : new Uint8Array(0); }
  get oam(): Uint8Array { return this.core.gba ? this.core.gba.oam() : new Uint8Array(0); }
  get pram16(): Uint16Array {
    if (!this.core.gba) return new Uint16Array(0);
    const p = this.core.gba.pram();
    return new Uint16Array(p.buffer, p.byteOffset, p.byteLength >> 1);
  }
}

// SIO register view + the link-cable transport slot (LinkPanel reads these and
// assigns `transport`). The async bridge methods live on `WasmEmulator`.
class WasmSio {
  // Inert default transport; LinkPanel swaps in the real SignalTransport.
  transport: LinkTransportLike = { isMaster: () => true, isConnected: () => false };
  private _traceOn = false;
  constructor(private core: WasmCore) {}
  get mltSend(): number { return this.core.snap().mlt_send ?? 0; }
  get multi(): number[] {
    const s = this.core.snap();
    return [s.multi0 ?? 0xffff, s.multi1 ?? 0xffff, 0xffff, 0xffff];
  }
  get transferSeq(): number { return this.core.snap().transfer_seq ?? 0; }
  get traceOn(): boolean { return this._traceOn; }
  set traceOn(on: boolean) { this._traceOn = on; this.core.gba?.sio_set_trace(on); }
  get trace(): Array<{ seq: number; pc: number; op: string; off: number; val: number; n: number }> {
    if (!this.core.gba) return [];
    try { return JSON.parse(this.core.gba.sio_trace()); } catch { return []; }
  }
  clearTrace(): void { this.core.gba?.sio_clear_trace(); }
}

class WasmIo {
  readonly sio: WasmSio;
  constructor(private core: WasmCore) { this.sio = new WasmSio(core); }
  read16 = (a: number): number => this.core.gba?.read16(a) ?? 0;
}

// ---------------------------------------------------------------- adapter

export class WasmEmulator implements WasmCore {
  /** Resolves once the wasm module is initialized and `gba` is live. */
  readonly ready: Promise<void>;

  /** The live wasm instance — undefined until `ready` resolves (WasmCore). */
  gba: WasmGba | undefined;

  readonly zeroFb = new Uint8Array(FB_LEN);

  // wasm linear memory, captured from `init()`'s output — used to build the
  // zero-copy framebuffer view. The cached view is rebuilt only when the
  // backing buffer detaches (memory growth) or the framebuffer pointer moves.
  private wasmMemory: WebAssembly.Memory | undefined;
  private _fbView: Uint8Array | undefined;
  private _fbBuffer: ArrayBufferLike | undefined;
  private _fbPtr = 0;

  // Stashed until ready.
  private _pendingRom: Uint8Array | null = null;
  private _pendingTurbo = 0;
  private _cheats: Cheat[] = [];

  // Cached debug snapshot + the perf-clock time it was taken.
  private _snapCache: DebugSnap = {};
  private _snapTs = 0;

  // Facet surfaces the UI reads through.
  readonly keypad = new WasmKeypad(this);
  readonly ppu = new WasmPpu(this);
  readonly sound = new WasmSound(this);
  readonly cpu = new WasmCpuDebug(this);
  readonly dma = new WasmDmaDebug(this);
  readonly timers = new WasmTimersDebug(this);
  readonly irq = new WasmIrqDebug(this);
  readonly save = new WasmSave(this);
  readonly bus = new WasmBus(this);
  readonly io = new WasmIo(this);

  constructor() {
    // `init()` resolves to the wasm InitOutput, which exposes `memory` — we
    // keep it to build the zero-copy framebuffer view in `frameView()`.
    this.ready = init().then((out: { memory?: WebAssembly.Memory } | undefined) => {
      this.wasmMemory = out?.memory;
      this.gba = new WasmGba();
      if (this._pendingRom) {
        this.gba.load_rom(this._pendingRom);
        this._pendingRom = null;
      }
      if (this._pendingTurbo) this.setTurboMask(this._pendingTurbo);
      this.applyCheats();
    });
  }

  // ---- WasmCore (facet back-channel) -------------------------------------
  // Zero-copy framebuffer: a Uint8Array view straight onto the wasm memory
  // holding the PPU's framebuffer, replacing the per-frame 153 KB copy that
  // `framebuffer()` did. Rebuild the view only when the backing buffer or the
  // framebuffer pointer changes (wasm growth detaches the buffer; a savestate
  // load can re-seat the framebuffer). Falls back to the zeroed buffer until
  // the core is ready.
  frameView(): Uint8Array {
    const g = this.gba;
    const mem = this.wasmMemory;
    if (!g || !mem) return this.zeroFb;
    const api = g as unknown as { framebuffer_ptr(): number };
    const ptr = api.framebuffer_ptr();
    const buf = mem.buffer;
    if (this._fbView && this._fbBuffer === buf && this._fbPtr === ptr) return this._fbView;
    this._fbView = new Uint8Array(buf, ptr, FB_LEN);
    this._fbBuffer = buf;
    this._fbPtr = ptr;
    return this._fbView;
  }

  snap(): DebugSnap {
    if (!this.gba) return this._snapCache;
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    if (now - this._snapTs >= 8) {
      try {
        this._snapCache = JSON.parse(this.gba.debug_state()) as DebugSnap;
      } catch {
        // Keep the previous snapshot on a parse hiccup.
      }
      this._snapTs = now;
    }
    return this._snapCache;
  }

  applyTurbo(mask: number): void {
    if (this.gba) this.setTurboMask(mask);
    else this._pendingTurbo = mask;
  }

  // `set_turbo_mask` and the `sio_*` link bridge are part of the wasm API but
  // missing from the shipped .d.ts; reach them through narrow casts.
  private setTurboMask(mask: number): void {
    (this.gba as unknown as { set_turbo_mask(m: number): void } | undefined)?.set_turbo_mask(mask);
  }
  private get linkApi(): {
    sio_set_link(connected: boolean, master: boolean): void;
    sio_take_outgoing(): number;
    sio_peek_mlt_send(): number;
    sio_deliver_multiplay(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
    sio_apply_remote_multiplay(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
    sio_set_wireless_adapter(enabled: boolean): void;
    sio_wl_update(frames: number): void;
    sio_wl_host_add_client(): number;
    sio_wl_client_set_connected(devid: number, clnum: number): void;
    sio_wl_disconnect_peer(): void;
    sio_wl_deliver_packet(bytes: Uint8Array): void;
    sio_wl_take_outgoing(): Uint8Array | undefined;
    sio_wl_add_scanned_host(devid: number, data: Uint32Array): void;
    sio_wl_clear_scanned_hosts(): void;
    sio_wl_broadcast(): Uint32Array | undefined;
  } | undefined {
    return this.gba as unknown as never;
  }

  // ---- async link-cable bridge (SignalTransport drives these) ------------
  linkSetState(connected: boolean, master: boolean): void { this.linkApi?.sio_set_link(connected, master); }
  linkTakeOutgoing(): number { return this.linkApi?.sio_take_outgoing() ?? -1; }
  linkPeekOutgoing(): number { return this.linkApi?.sio_peek_mlt_send() ?? 0xFFFF; }
  linkDeliver(m0: number, m1: number, m2: number, m3: number, error: boolean): void {
    this.linkApi?.sio_deliver_multiplay(m0, m1, m2, m3, error);
  }
  linkApplyRemote(m0: number, m1: number, m2: number, m3: number, error: boolean): void {
    this.linkApi?.sio_apply_remote_multiplay(m0, m1, m2, m3, error);
  }
  /**
   * Attach (or detach) the GBA Wireless Adapter as the SIO Normal-32 peripheral.
   * When attached, wireless-capable games detect the adapter and reach their
   * multiplayer / Download-Play menus (single-player HLE — no radio peers yet).
   */
  setWirelessAdapter(enabled: boolean): void { this.linkApi?.sio_set_wireless_adapter(enabled); }

  // ---- Wireless Adapter peer seam ----------------------------------------
  // A wireless transport (RFU packets over the same WebSocket room as the link
  // cable) drives these; no-ops unless the adapter is the active transport.
  /** Advance the adapter's wait timeout by `frames` (call once per frame). */
  wlUpdate(frames: number): void { this.linkApi?.sio_wl_update(frames); }
  /** Register a connected client (host side); returns its device ID, or 0. */
  wlHostAddClient(): number { return this.linkApi?.sio_wl_host_add_client() ?? 0; }
  /** Finalize this adapter as a client the host accepted. */
  wlClientSetConnected(devid: number, clnum: number): void { this.linkApi?.sio_wl_client_set_connected(devid, clnum); }
  /** Drop the peer link (wakes a parked wait with a disconnect event). */
  wlDisconnectPeer(): void { this.linkApi?.sio_wl_disconnect_peer(); }
  /** Inject a packet received from the peer. */
  wlDeliverPacket(bytes: Uint8Array): void { this.linkApi?.sio_wl_deliver_packet(bytes); }
  /** Take the packet the game queued to send, if any. */
  wlTakeOutgoing(): Uint8Array | undefined { return this.linkApi?.sio_wl_take_outgoing(); }
  /** Surface a discovered host: device ID + 6 broadcast words. */
  wlAddScannedHost(devid: number, data: Uint32Array): void { this.linkApi?.sio_wl_add_scanned_host(devid, data); }
  /** Clear the discovered-hosts list. */
  wlClearScannedHosts(): void { this.linkApi?.sio_wl_clear_scanned_hosts(); }
  /** This host's broadcast to announce: `[devid, w0..w5]`, or undefined. */
  wlBroadcast(): Uint32Array | undefined { return this.linkApi?.sio_wl_broadcast(); }

  /** Drive one frame's worth of the async link transport, if one is set. */
  pumpLink(): void { this.io.sio.transport.pump?.(); }

  // ---- core lifecycle ----------------------------------------------------
  loadRom(bytes: Uint8Array): void {
    if (this.gba) this.gba.load_rom(bytes);
    else this._pendingRom = bytes;
  }

  /**
   * Boot a multiboot (`.mb`) image as a child unit (Single-Pak link receive):
   * it runs from EWRAM at 0x020000C0. Returns false if the image won't fit.
   * `load_multiboot` isn't in the shipped .d.ts yet, so reach it via a cast.
   */
  loadMultiboot(bytes: Uint8Array): boolean {
    const g = this.gba as unknown as { load_multiboot(b: Uint8Array): boolean } | undefined;
    return g?.load_multiboot(bytes) ?? false;
  }

  runFrame(): { frames: number } {
    if (!this.gba) return { frames: 0 };
    // Push the JS-side held bitmask; wasm handles turbo/autofire internally.
    this.gba.set_keys(this.keypad.pressed & 0x3ff);
    this.gba.run_frame();
    // Drive the async link transport (WebRTC) once per frame.
    this.pumpLink();
    if (this.gba.save_dirty()) {
      this.gba.clear_save_dirty();
      this.save.onChange?.();
    }
    return { frames: this.gba.frame_count() };
  }

  /**
   * Resumable slice runner for the synchronous local-link (duo) path. Runs up to
   * `maxCycles` of the current frame and returns: 0 = slice exhausted (call
   * again), 1 = frame completed, 2 = paused on a pending master link transfer
   * (resolve it via the link bridge, then call again to resume). Keys must be
   * pushed before the frame's first slice. No-op (returns 1) until ready.
   */
  runSlice(maxCycles: number): number {
    if (!this.gba) return 1;
    // Keep the keypad register current for this slice (cheap, idempotent).
    this.gba.set_keys(this.keypad.pressed & 0x3ff);
    const st = (this.gba as unknown as { run_slice(n: number): number }).run_slice(maxCycles);
    // On frame completion, mirror the save-dirty -> onChange contract.
    if (st === 1 && this.gba.save_dirty()) {
      this.gba.clear_save_dirty();
      this.save.onChange?.();
    }
    return st;
  }

  // ---- save states -------------------------------------------------------
  saveState(): Uint8Array {
    if (!this.gba) throw new Error('wasm core not ready');
    return this.gba.save_state();
  }
  loadState(blob: Uint8Array): void {
    if (!this.gba) throw new Error('wasm core not ready');
    this.gba.load_state(blob);
  }

  /** Detected save type (display-only); 'flash128' until a ROM is loaded. */
  get saveType(): string { return this.gba ? this.gba.save_type() : 'flash128'; }

  // ---- IwramWatch bridge (native wasm watch) -----------------------------
  setWatch(lo: number, hi: number): void { this.gba?.set_watch(lo, hi); }
  clearWatch(): void { this.gba?.clear_watch(); }
  watchLog(): Array<{ pc: number; addr: number; size: number; val: number }> {
    if (!this.gba) return [];
    try { return JSON.parse(this.gba.watch_log()); } catch { return []; }
  }

  // ---- cheats ------------------------------------------------------------
  get cheats(): Cheat[] { return this._cheats; }
  set cheats(next: Cheat[]) { this._cheats = next; this.applyCheats(); }
  private applyCheats(): void {
    if (!this.gba) return;
    const codes = this._cheats.filter((c) => c.enabled).map((c) => c.code).join('\n');
    this.gba.set_cheats(codes);
  }
  /** Validate a raw cheat code through the Rust parser — the same one the
   *  engine applies — returning line counts for the editor. Replaces the old
   *  TS reimplementation that could silently drift from what actually runs. */
  parseCheatSummary(code: string): { supported: number; unsupported: number; total: number } {
    if (!this.gba) return { supported: 0, unsupported: 0, total: 0 };
    const [supported, unsupported, total] = this.gba.parse_cheat_summary(code);
    return { supported, unsupported, total };
  }
}
