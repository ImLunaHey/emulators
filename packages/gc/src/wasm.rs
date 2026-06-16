//! wasm-bindgen surface for the web target. Thin wrapper over [`Gc`].
//! Gated to wasm32 so host `cargo test` never invokes the macro.
//!
//! Mirrors the PS1 core's `WasmPsx` (`../../core-ps1/src/wasm.rs`): a
//! constructor that installs the panic hook, an IPL loader, a per-frame
//! `run_frame`, the zero-copy `framebuffer_ptr`/`framebuffer_len` present path
//! (plus a copying `framebuffer` convenience), `set_keys`, and the frame/size
//! getters. (A `build:wasm:gc` script in the root package.json would be added
//! later, alongside the existing `build:wasm:*` per-core scripts.)

use crate::Gc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmGc {
    inner: Gc,
}

#[wasm_bindgen]
impl WasmGc {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmGc {
        console_error_panic_hook_set();
        WasmGc { inner: Gc::new() }
    }

    /// Load a 2 MB IPL boot ROM the user supplies (the GameCube cannot boot a
    /// real game without it). Resets the machine to the Gekko reset vector.
    pub fn load_ipl(&mut self, bytes: &[u8]) {
        self.inner.load_ipl(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). Prefer the
    /// zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot present
    /// path; this copying variant is for callers that want an owned buffer.
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

    /// Current display width in pixels (the host re-reads this each present and
    /// sizes its canvas accordingly).
    pub fn width(&self) -> u32 {
        self.inner.width()
    }

    /// Current display height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Controller button bitmask (routed to the SI subsystem when it lands).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
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
            web_error(&format!("gc-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
