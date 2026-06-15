//! wasm-bindgen surface for the web target. Thin wrapper over [`Atari`],
//! mirroring the sibling cores' `wasm` modules. Gated to wasm32 so host
//! `cargo test` never invokes the macro.

use crate::Atari;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmAtari2600 {
    inner: Atari,
}

#[wasm_bindgen]
impl WasmAtari2600 {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmAtari2600 {
        console_error_panic_hook_set();
        WasmAtari2600 {
            inner: Atari::new(),
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). The 2600 is
    /// 160×192. Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on
    /// the hot present path.
    pub fn framebuffer(&mut self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. Re-read each
    /// present: a wasm allocation that grows memory detaches the old buffer.
    pub fn framebuffer_ptr(&mut self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    pub fn framebuffer_len(&mut self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Player-1 input: bit0 Up, bit1 Down, bit2 Left, bit3 Right, bit4 Fire,
    /// bit5 Reset, bit6 Select.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Player-2 joystick (same Up/Down/Left/Right/Fire ordering).
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.inner.set_keys_p2(bits);
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

impl Default for WasmAtari2600 {
    fn default() -> Self {
        WasmAtari2600::new()
    }
}

fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("atari2600-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
