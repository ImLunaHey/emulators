//! wasm-bindgen surface for the web target. Thin wrapper over `Snes`, mirroring
//! `core-sms/src/wasm.rs`'s `WasmSms`. Gated to wasm32 so host `cargo test`
//! never invokes the macro.

use crate::Snes;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmSnes {
    inner: Snes,
}

#[wasm_bindgen]
impl WasmSnes {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmSnes {
        console_error_panic_hook_set();
        WasmSnes { inner: Snes::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). 256x224.
    /// Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot
    /// present path.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask for player 1. Bit order (see `input::Key`):
    /// B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.inner.set_keys_port(1, bits);
    }

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

    // ---- battery save ----
    pub fn save_ram(&self) -> Vec<u8> {
        self.inner.save_ram()
    }
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        self.inner.load_save_ram(bytes);
    }
    pub fn save_dirty(&self) -> bool {
        self.inner.save_dirty()
    }
    pub fn clear_save_dirty(&mut self) {
        self.inner.clear_save_dirty();
    }
}

impl Default for WasmSnes {
    fn default() -> Self {
        WasmSnes::new()
    }
}

fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("snes-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
