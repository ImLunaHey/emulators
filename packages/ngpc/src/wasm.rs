//! wasm-bindgen surface for the web target. Thin wrapper over `Ngpc`, mirroring
//! the sibling cores' bindings. Gated to wasm32 so host `cargo test` never
//! invokes the macro.

use crate::Ngpc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmNgpc {
    inner: Ngpc,
}

#[wasm_bindgen]
impl WasmNgpc {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmNgpc {
        console_error_panic_hook_set();
        WasmNgpc { inner: Ngpc::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (160×152×4) copied into a fresh JS `Uint8Array`.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer in wasm linear memory (zero-copy present).
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask. bit0 Up, bit1 Down, bit2 Left, bit3 Right,
    /// bit4 A, bit5 B, bit6 Option.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
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
}

impl Default for WasmNgpc {
    fn default() -> Self {
        WasmNgpc::new()
    }
}

fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("ngpc-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
