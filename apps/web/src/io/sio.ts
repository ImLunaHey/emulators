// SIO leftovers kept after the hard-swap to the Rust/wasm core.
//
// The full SIO state machine now lives in the Rust core (core/src/sio.rs).
// Two things from the old TS module are still referenced by the surviving UI /
// transport layer:
//   - `MultiplayResult` — the 4-slot exchange shape, used by sio-signal.ts.
//   - `LocalLoopback` — the inert "no cable" transport the LinkPanel installs
//     on disconnect (the adapter only reads isConnected()/isMaster() and an
//     optional pump()).

export interface MultiplayResult {
  d0: number;
  d1: number;
  d2: number;
  d3: number;
  error: boolean;
}

// Inert transport: no partner ever connects. Assigned to `sio.transport` when
// the link is dropped so `getActive()` reads it as "not a SignalTransport".
export class LocalLoopback {
  isConnected(): boolean {
    return false;
  }
  isMaster(): boolean {
    return true;
  }
}
