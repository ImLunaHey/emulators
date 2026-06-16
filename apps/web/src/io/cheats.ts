// Cheat code support now lives entirely in Rust (`core/src/cheats.rs`): the
// parser AND the apply-every-frame engine. The TypeScript reimplementation that
// used to live here (parseHex/parseLine/parseCheat + applyCheats/BusLike) was a
// second copy that could silently drift from what the engine actually runs, so
// it was removed. The UI now validates codes through the wasm boundary
// (`WasmEmulator.parseCheatSummary` → `WasmGba.parse_cheat_summary`).
//
// Only the shared data shape stays in TS — it's what the React layer passes
// around and persists. When the cheats menu moves into the Rust-rendered
// console UI, even this goes away.

export interface Cheat {
  name: string;
  code: string;     // raw user input (can be multi-line)
  enabled: boolean;
}
