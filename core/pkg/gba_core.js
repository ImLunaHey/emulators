/* @ts-self-types="./gba_core.d.ts" */

export class WasmGba {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmGbaFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmgba_free(ptr, 0);
    }
    clear_save_dirty() {
        wasm.wasmgba_clear_save_dirty(this.__wbg_ptr);
    }
    clear_watch() {
        wasm.wasmgba_clear_watch(this.__wbg_ptr);
    }
    /**
     * JSON snapshot of CPU/PPU/DMA/timer/IRQ/sound/SIO scalar state.
     * @returns {string}
     */
    debug_state() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmgba_debug_state(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Interleaved-stereo f32 samples produced since the last call.
     * @returns {Float32Array}
     */
    drain_audio() {
        const ret = wasm.wasmgba_drain_audio(this.__wbg_ptr);
        var v1 = getArrayF32FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 4, 4);
        return v1;
    }
    /**
     * @returns {number}
     */
    frame_count() {
        const ret = wasm.wasmgba_frame_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * 240×160 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
     * @returns {Uint8Array}
     */
    framebuffer() {
        const ret = wasm.wasmgba_framebuffer(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @param {Uint8Array} bytes
     */
    load_rom(bytes) {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmgba_load_rom(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Load a `.sav` into the save chip (call right after `load_rom`).
     * @param {Uint8Array} bytes
     */
    load_save_ram(bytes) {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmgba_load_save_ram(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Restore a snapshot. Returns false on a bad/incompatible blob.
     * @param {Uint8Array} blob
     * @returns {boolean}
     */
    load_state(blob) {
        const ptr0 = passArray8ToWasm0(blob, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmgba_load_state(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    constructor() {
        const ret = wasm.wasmgba_new();
        this.__wbg_ptr = ret;
        WasmGbaFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {Uint8Array}
     */
    oam() {
        const ret = wasm.wasmgba_oam(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @returns {Uint8Array}
     */
    pram() {
        const ret = wasm.wasmgba_pram(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @param {number} addr
     * @returns {number}
     */
    read16(addr) {
        const ret = wasm.wasmgba_read16(this.__wbg_ptr, addr);
        return ret >>> 0;
    }
    /**
     * Debug bus reads (DebugPanel memory viewer / LinkPanel SIOCNT readback).
     * @param {number} addr
     * @returns {number}
     */
    read8(addr) {
        const ret = wasm.wasmgba_read8(this.__wbg_ptr, addr);
        return ret >>> 0;
    }
    /**
     * Erase the save chip (fill 0xFF + mark dirty) — backs the UI's clear-save.
     */
    reset_save() {
        wasm.wasmgba_reset_save(this.__wbg_ptr);
    }
    run_frame() {
        wasm.wasmgba_run_frame(this.__wbg_ptr);
    }
    /**
     * True if the save chip changed since the last `clear_save_dirty` — the
     * host polls this to know when to persist the `.sav`.
     * @returns {boolean}
     */
    save_dirty() {
        const ret = wasm.wasmgba_save_dirty(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * Current save-chip contents (write this to a `.sav`).
     * @returns {Uint8Array}
     */
    save_ram() {
        const ret = wasm.wasmgba_save_ram(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @returns {Uint8Array}
     */
    save_state() {
        const ret = wasm.wasmgba_save_state(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * Detected save type as a display string (flash128/flash64/sram/...).
     * @returns {string}
     */
    save_type() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmgba_save_type(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Set the active cheat codes (newline-separated raw codes). Pass the
     * enabled cheats only; they're applied once per frame.
     * @param {string} codes_newline_joined
     */
    set_cheats(codes_newline_joined) {
        const ptr0 = passStringToWasm0(codes_newline_joined, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmgba_set_cheats(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Raw pressed-button bitmask (bit layout per `keypad::Key`).
     * @param {number} bits
     */
    set_keys(bits) {
        wasm.wasmgba_set_keys(this.__wbg_ptr, bits);
    }
    /**
     * Autofire/turbo mask (bit set = that button pulses each frame).
     * @param {number} mask
     */
    set_turbo_mask(mask) {
        wasm.wasmgba_set_turbo_mask(this.__wbg_ptr, mask);
    }
    /**
     * @param {number} lo
     * @param {number} hi
     */
    set_watch(lo, hi) {
        wasm.wasmgba_set_watch(this.__wbg_ptr, lo, hi);
    }
    /**
     * Slave-side: apply the remote master's broadcast (latch SIOMULTI, clear
     * START, raise the SIO IRQ if enabled).
     * @param {number} m0
     * @param {number} m1
     * @param {number} m2
     * @param {number} m3
     * @param {boolean} error
     */
    sio_apply_remote_multiplay(m0, m1, m2, m3, error) {
        wasm.wasmgba_sio_apply_remote_multiplay(this.__wbg_ptr, m0, m1, m2, m3, error);
    }
    sio_clear_trace() {
        wasm.wasmgba_sio_clear_trace(this.__wbg_ptr);
    }
    /**
     * Master-side completion: deliver the synchronized 4-slot result the host
     * gathered from peers (latch SIOMULTI, bump transfer_seq, clear START,
     * raise the SIO IRQ if enabled).
     * @param {number} m0
     * @param {number} m1
     * @param {number} m2
     * @param {number} m3
     * @param {boolean} error
     */
    sio_deliver_multiplay(m0, m1, m2, m3, error) {
        wasm.wasmgba_sio_deliver_multiplay(this.__wbg_ptr, m0, m1, m2, m3, error);
    }
    /**
     * Set the live link state. `connected` drives SIOCNT.SD; `master` drives
     * SIOCNT SI/ID. `connected == false` (default) keeps single-player.
     * @param {boolean} connected
     * @param {boolean} master
     */
    sio_set_link(connected, master) {
        wasm.wasmgba_sio_set_link(this.__wbg_ptr, connected, master);
    }
    /**
     * @param {boolean} on
     */
    sio_set_trace(on) {
        wasm.wasmgba_sio_set_trace(this.__wbg_ptr, on);
    }
    /**
     * Poll the master's outgoing multiplay payload. Returns the 16-bit
     * SIOMLT_SEND value once (take semantics) after a transfer starts over a
     * connected link, or -1 when there's nothing to send.
     * @returns {number}
     */
    sio_take_outgoing() {
        const ret = wasm.wasmgba_sio_take_outgoing(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {string}
     */
    sio_trace() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmgba_sio_trace(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Memory-region copies for the palette/tile/sprite/memory debug views.
     * @returns {Uint8Array}
     */
    vram() {
        const ret = wasm.wasmgba_vram(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @returns {string}
     */
    watch_log() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmgba_watch_log(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
}
if (Symbol.dispose) WasmGba.prototype[Symbol.dispose] = WasmGba.prototype.free;
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_throw_ea4887a5f8f9a9db: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_ff666cb1272fba04: function(arg0, arg1) {
            console.error(getStringFromWasm0(arg0, arg1));
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./gba_core_bg.js": import0,
    };
}

const WasmGbaFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmgba_free(ptr, 1));

function getArrayF32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedFloat32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('gba_core_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
