//! wasm-bindgen surface for the web target. Thin wrapper over [`Xbox`].
//! Gated to wasm32 so host `cargo test` never invokes the macro.
//!
//! Mirrors the PS1/GC cores' wasm surfaces: a constructor that installs the panic
//! hook, a BIOS loader + a (stub) ROM loader, a per-frame `run_frame`, the
//! zero-copy `framebuffer_ptr`/`framebuffer_len` present path (plus a copying
//! `framebuffer` convenience), `set_keys`, `drain_audio`, and the frame/size
//! getters. (`build:wasm:xbox` in the root package.json drives the build.)

use crate::Xbox;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmXbox {
    inner: Xbox,
}

#[wasm_bindgen]
impl WasmXbox {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmXbox {
        console_error_panic_hook_set();
        WasmXbox { inner: Xbox::new() }
    }

    /// Load a flash/BIOS image the user supplies (the Xbox cannot boot without
    /// it). 256 KB retail, or a larger mirrored dump (the tail is used). Resets
    /// the machine to the x86 reset vector.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.inner.load_bios(bytes);
    }

    /// Load a game image (XBE/XISO). A no-op seam today — no loader exists yet.
    pub fn load_rom(&mut self, bytes: Vec<u8>) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). Prefer the
    /// zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot present path.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. The host builds a
    /// `Uint8Array(memory.buffer, ptr, len)` view over it — no per-frame copy.
    /// Re-read each present: any wasm allocation that grows memory detaches the
    /// old `memory.buffer`.
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    /// Length in bytes of the framebuffer view (`width * height * 4`).
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    pub fn width(&self) -> u32 {
        self.inner.width()
    }

    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Controller button bitmask (latched; routed to USB when that lands).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Drain queued audio (interleaved stereo f32). Empty until the APU lands.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count()
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("xbox-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
