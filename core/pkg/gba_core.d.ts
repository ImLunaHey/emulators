/* tslint:disable */
/* eslint-disable */

export class WasmGba {
    free(): void;
    [Symbol.dispose](): void;
    clear_save_dirty(): void;
    clear_watch(): void;
    /**
     * JSON snapshot of CPU/PPU/DMA/timer/IRQ/sound/SIO scalar state.
     */
    debug_state(): string;
    /**
     * Interleaved-stereo f32 samples produced since the last call.
     */
    drain_audio(): Float32Array;
    frame_count(): number;
    /**
     * 240×160 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
     */
    framebuffer(): Uint8Array;
    load_rom(bytes: Uint8Array): void;
    /**
     * Load a `.sav` into the save chip (call right after `load_rom`).
     */
    load_save_ram(bytes: Uint8Array): void;
    /**
     * Restore a snapshot. Returns false on a bad/incompatible blob.
     */
    load_state(blob: Uint8Array): boolean;
    constructor();
    oam(): Uint8Array;
    pram(): Uint8Array;
    read16(addr: number): number;
    /**
     * Debug bus reads (DebugPanel memory viewer / LinkPanel SIOCNT readback).
     */
    read8(addr: number): number;
    /**
     * Erase the save chip (fill 0xFF + mark dirty) — backs the UI's clear-save.
     */
    reset_save(): void;
    run_frame(): void;
    /**
     * True if the save chip changed since the last `clear_save_dirty` — the
     * host polls this to know when to persist the `.sav`.
     */
    save_dirty(): boolean;
    /**
     * Current save-chip contents (write this to a `.sav`).
     */
    save_ram(): Uint8Array;
    save_state(): Uint8Array;
    /**
     * Detected save type as a display string (flash128/flash64/sram/...).
     */
    save_type(): string;
    /**
     * Set the active cheat codes (newline-separated raw codes). Pass the
     * enabled cheats only; they're applied once per frame.
     */
    set_cheats(codes_newline_joined: string): void;
    /**
     * Raw pressed-button bitmask (bit layout per `keypad::Key`).
     */
    set_keys(bits: number): void;
    /**
     * Autofire/turbo mask (bit set = that button pulses each frame).
     */
    set_turbo_mask(mask: number): void;
    set_watch(lo: number, hi: number): void;
    /**
     * Slave-side: apply the remote master's broadcast (latch SIOMULTI, clear
     * START, raise the SIO IRQ if enabled).
     */
    sio_apply_remote_multiplay(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
    sio_clear_trace(): void;
    /**
     * Master-side completion: deliver the synchronized 4-slot result the host
     * gathered from peers (latch SIOMULTI, bump transfer_seq, clear START,
     * raise the SIO IRQ if enabled).
     */
    sio_deliver_multiplay(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
    /**
     * Set the live link state. `connected` drives SIOCNT.SD; `master` drives
     * SIOCNT SI/ID. `connected == false` (default) keeps single-player.
     */
    sio_set_link(connected: boolean, master: boolean): void;
    sio_set_trace(on: boolean): void;
    /**
     * Poll the master's outgoing multiplay payload. Returns the 16-bit
     * SIOMLT_SEND value once (take semantics) after a transfer starts over a
     * connected link, or -1 when there's nothing to send.
     */
    sio_take_outgoing(): number;
    sio_trace(): string;
    /**
     * Memory-region copies for the palette/tile/sprite/memory debug views.
     */
    vram(): Uint8Array;
    watch_log(): string;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmgba_free: (a: number, b: number) => void;
    readonly wasmgba_clear_save_dirty: (a: number) => void;
    readonly wasmgba_clear_watch: (a: number) => void;
    readonly wasmgba_debug_state: (a: number) => [number, number];
    readonly wasmgba_drain_audio: (a: number) => [number, number];
    readonly wasmgba_frame_count: (a: number) => number;
    readonly wasmgba_framebuffer: (a: number) => [number, number];
    readonly wasmgba_load_rom: (a: number, b: number, c: number) => void;
    readonly wasmgba_load_save_ram: (a: number, b: number, c: number) => void;
    readonly wasmgba_load_state: (a: number, b: number, c: number) => number;
    readonly wasmgba_new: () => number;
    readonly wasmgba_oam: (a: number) => [number, number];
    readonly wasmgba_pram: (a: number) => [number, number];
    readonly wasmgba_read16: (a: number, b: number) => number;
    readonly wasmgba_read8: (a: number, b: number) => number;
    readonly wasmgba_reset_save: (a: number) => void;
    readonly wasmgba_run_frame: (a: number) => void;
    readonly wasmgba_save_dirty: (a: number) => number;
    readonly wasmgba_save_ram: (a: number) => [number, number];
    readonly wasmgba_save_state: (a: number) => [number, number];
    readonly wasmgba_save_type: (a: number) => [number, number];
    readonly wasmgba_set_cheats: (a: number, b: number, c: number) => void;
    readonly wasmgba_set_keys: (a: number, b: number) => void;
    readonly wasmgba_set_turbo_mask: (a: number, b: number) => void;
    readonly wasmgba_set_watch: (a: number, b: number, c: number) => void;
    readonly wasmgba_sio_apply_remote_multiplay: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmgba_sio_clear_trace: (a: number) => void;
    readonly wasmgba_sio_deliver_multiplay: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmgba_sio_set_link: (a: number, b: number, c: number) => void;
    readonly wasmgba_sio_set_trace: (a: number, b: number) => void;
    readonly wasmgba_sio_take_outgoing: (a: number) => number;
    readonly wasmgba_sio_trace: (a: number) => [number, number];
    readonly wasmgba_vram: (a: number) => [number, number];
    readonly wasmgba_watch_log: (a: number) => [number, number];
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
