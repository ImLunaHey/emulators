//! wasm-bindgen surface for the web target. Thin wrapper over [`N64`],
//! mirroring `core-sms/src/wasm.rs`'s `WasmSms`. Gated to wasm32 so host
//! `cargo test` never invokes the macro.

use crate::N64;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmN64 {
    inner: N64,
}

#[wasm_bindgen]
impl WasmN64 {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmN64 {
        console_error_panic_hook_set();
        WasmN64 { inner: N64::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). Prefer the
    /// zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot path.
    pub fn framebuffer(&mut self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. Re-read each
    /// present: a wasm allocation that grows memory detaches `memory.buffer`.
    pub fn framebuffer_ptr(&mut self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    /// Length in bytes of the framebuffer view (width*height*4).
    pub fn framebuffer_len(&mut self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Player-1 controller bitmask. Bit order matches `si::button`:
    /// A,B,Z,Start,Dup,Ddown,Dleft,Dright,L,R,Cup,Cdown,Cleft,Cright.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Mono f32 samples produced since the last call. AI is stubbed, so this is
    /// currently always empty.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count() as u32
    }

    pub fn width(&self) -> u32 {
        self.inner.width() as u32
    }
    pub fn height(&self) -> u32 {
        self.inner.height() as u32
    }
}

impl Default for WasmN64 {
    fn default() -> Self {
        Self::new()
    }
}

// Minimal panic hook: forward Rust panics to console.error.
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("n64-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
