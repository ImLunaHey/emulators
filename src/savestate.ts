// Whole-emulator save-state entry points.
//
// The core was hard-swapped to the Rust/wasm implementation (WasmEmulator),
// which owns the actual snapshot format (a byte-compatible port of the
// original TS format — magic 'GBAS', versioned tagged sections — lives in
// core/src/savestate.rs). These free functions simply delegate to the
// adapter's methods so the existing call sites (PlayerPage, SaveStatesPanel)
// keep working unchanged.

// Typed `any` because call sites hold the shared `emu: Emulator`-typed
// reference (the runtime object is a WasmEmulator, which has these methods).
export function saveState(emu: any): Uint8Array {
  return emu.saveState();
}

export function loadState(emu: any, blob: Uint8Array): void {
  emu.loadState(blob);
}
