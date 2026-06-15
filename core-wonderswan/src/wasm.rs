//! wasm-bindgen surface for the web target. Thin wrapper over `WonderSwan`,
//! mirroring `core-sms/src/wasm.rs`'s `WasmSms`. Gated to wasm32 so host
//! `cargo test` never invokes the macro.
//!
//! ONE binding handles both models: the constructor's `color` flag picks the
//! WonderSwan Color (12-bit palette, 64 KiB RAM) vs the mono WonderSwan.

use crate::{Model, WonderSwan};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmWonderSwan {
    inner: WonderSwan,
}

#[wasm_bindgen]
impl WasmWonderSwan {
    /// Construct a core. `color == true` selects the WonderSwan Color.
    #[wasm_bindgen(constructor)]
    pub fn new(color: bool) -> WasmWonderSwan {
        console_error_panic_hook_set();
        let model = if color { Model::Color } else { Model::Mono };
        WasmWonderSwan {
            inner: WonderSwan::new(model),
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (224×144), copied into a fresh JS `Uint8Array`.
    /// Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` on the hot path.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask. Bit order: bit0 Up, bit1 Down, bit2 Left,
    /// bit3 Right, bit4 A, bit5 B, bit6 Start.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Mono f32 samples produced since the last call (44100 Hz).
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

    // ---- battery save (cart SRAM) ----
    pub fn save_ram(&self) -> Vec<u8> {
        self.inner.save_ram().to_vec()
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

fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("wonderswan-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
