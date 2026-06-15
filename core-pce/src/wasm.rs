//! wasm-bindgen surface for the web target. Thin wrapper over `Pce`, mirroring
//! the sibling cores' `wasm.rs`. Gated to wasm32 so host `cargo test` never
//! invokes the macro.

use crate::Pce;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmPce {
    inner: Pce,
}

#[wasm_bindgen]
impl WasmPce {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmPce {
        console_error_panic_hook_set();
        WasmPce { inner: Pce::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). PC Engine is
    /// 256×224. Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on
    /// the hot present path.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory.
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask. Bit order: bit0 Up, bit1 Down, bit2 Left,
    /// bit3 Right, bit4 I, bit5 II, bit6 Select, bit7 Run.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Mono f32 samples produced since the last call (host sample rate 44100 Hz).
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

impl Default for WasmPce {
    fn default() -> Self {
        WasmPce::new()
    }
}

fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("pce-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
