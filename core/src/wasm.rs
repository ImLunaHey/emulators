//! wasm-bindgen surface for the web target. Thin wrapper over `Gba`.
//! Gated to wasm32 so host `cargo test` never invokes the macro.
use crate::Gba;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmGba {
    inner: Gba,
}

#[wasm_bindgen]
impl WasmGba {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmGba {
        console_error_panic_hook_set();
        WasmGba { inner: Gba::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// 240×160 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Raw pressed-button bitmask (bit layout per `keypad::Key`).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Autofire/turbo mask (bit set = that button pulses each frame).
    pub fn set_turbo_mask(&mut self, mask: u32) {
        self.inner.keypad.turbo_mask = mask & 0x3FF;
    }

    /// Interleaved-stereo f32 samples produced since the last call.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count()
    }

    // ---- battery save (cartridge SRAM/Flash/EEPROM) ----
    /// Current save-chip contents (write this to a `.sav`).
    pub fn save_ram(&self) -> Vec<u8> {
        self.inner.save_ram().to_vec()
    }
    /// Load a `.sav` into the save chip (call right after `load_rom`).
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        self.inner.load_save_ram(bytes);
    }
    /// True if the save chip changed since the last `clear_save_dirty` — the
    /// host polls this to know when to persist the `.sav`.
    pub fn save_dirty(&self) -> bool {
        self.inner.save_dirty()
    }
    pub fn clear_save_dirty(&mut self) {
        self.inner.clear_save_dirty();
    }
    /// Erase the save chip (fill 0xFF + mark dirty) — backs the UI's clear-save.
    pub fn reset_save(&mut self) {
        self.inner.reset_save();
    }
    /// Detected save type as a display string (flash128/flash64/sram/...).
    pub fn save_type(&self) -> String {
        self.inner.save_type_str().to_string()
    }

    // ---- save states (full machine snapshot) ----
    pub fn save_state(&self) -> Vec<u8> {
        self.inner.save_state()
    }
    /// Restore a snapshot. Returns false on a bad/incompatible blob.
    pub fn load_state(&mut self, blob: &[u8]) -> bool {
        self.inner.load_state(blob).is_ok()
    }

    // ---- cheats ----
    /// Set the active cheat codes (newline-separated raw codes). Pass the
    /// enabled cheats only; they're applied once per frame.
    pub fn set_cheats(&mut self, codes_newline_joined: &str) {
        self.inner.set_cheats(codes_newline_joined);
    }

    // ---- debug / introspection (DebugPanel + LinkPanel) ----
    /// JSON snapshot of CPU/PPU/DMA/timer/IRQ/sound/SIO scalar state.
    pub fn debug_state(&self) -> String {
        self.inner.debug_state_json()
    }
    /// Debug bus reads (DebugPanel memory viewer / LinkPanel SIOCNT readback).
    pub fn read8(&mut self, addr: u32) -> u32 {
        self.inner.dbg_read8(addr)
    }
    pub fn read16(&mut self, addr: u32) -> u32 {
        self.inner.dbg_read16(addr)
    }
    /// Memory-region copies for the palette/tile/sprite/memory debug views.
    pub fn vram(&self) -> Vec<u8> {
        self.inner.mem.vram.clone()
    }
    pub fn pram(&self) -> Vec<u8> {
        self.inner.mem.pram.clone()
    }
    pub fn oam(&self) -> Vec<u8> {
        self.inner.mem.oam.clone()
    }

    // ---- SIO trace (LinkPanel's SioTracer) ----
    pub fn sio_set_trace(&mut self, on: bool) {
        self.inner.sio_set_trace(on);
    }
    pub fn sio_clear_trace(&mut self) {
        self.inner.sio_clear_trace();
    }
    pub fn sio_trace(&self) -> String {
        self.inner.sio_trace_json()
    }

    // ---- async WebRTC link-cable bridge (sio-signal.ts) ----

    /// Set the live link state. `connected` drives SIOCNT.SD; `master` drives
    /// SIOCNT SI/ID. `connected == false` (default) keeps single-player.
    pub fn sio_set_link(&mut self, connected: bool, master: bool) {
        self.inner.sio_set_link(connected, master);
    }

    /// Poll the master's outgoing multiplay payload. Returns the 16-bit
    /// SIOMLT_SEND value once (take semantics) after a transfer starts over a
    /// connected link, or -1 when there's nothing to send.
    pub fn sio_take_outgoing(&mut self) -> i32 {
        match self.inner.sio_take_outgoing() {
            Some(v) => (v & 0xFFFF) as i32,
            None => -1,
        }
    }

    /// Master-side completion: deliver the synchronized 4-slot result the host
    /// gathered from peers (latch SIOMULTI, bump transfer_seq, clear START,
    /// raise the SIO IRQ if enabled).
    pub fn sio_deliver_multiplay(&mut self, m0: u32, m1: u32, m2: u32, m3: u32, error: bool) {
        self.inner.sio_deliver_multiplay(m0, m1, m2, m3, error);
    }

    /// Slave-side: apply the remote master's broadcast (latch SIOMULTI, clear
    /// START, raise the SIO IRQ if enabled).
    pub fn sio_apply_remote_multiplay(
        &mut self,
        m0: u32,
        m1: u32,
        m2: u32,
        m3: u32,
        error: bool,
    ) {
        self.inner.sio_apply_remote_multiplay(m0, m1, m2, m3, error);
    }

    // ---- IWRAM write-watch (LinkPanel's IwramWatch) ----
    pub fn set_watch(&mut self, lo: u32, hi: u32) {
        self.inner.set_watch(lo, hi);
    }
    pub fn clear_watch(&mut self) {
        self.inner.clear_watch();
    }
    pub fn watch_log(&self) -> String {
        self.inner.take_watch_log()
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("gba-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
