// The emulator core was hard-swapped to the Rust/wasm implementation. The
// `Emulator` name is retained as a TYPE alias to the wasm-backed adapter so the
// UI components (typed `emu: Emulator`) keep compiling unchanged. There is no
// longer a TypeScript Emulator class — `App.tsx` constructs `WasmEmulator`.
export type Emulator = import('./ui/wasmEmulator').WasmEmulator;
