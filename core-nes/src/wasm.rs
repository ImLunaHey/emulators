//! wasm-bindgen surface for the web target. Thin wrapper over `Nes`,
//! mirroring `core/src/wasm.rs`'s `WasmGba`. Gated to wasm32 so host
//! `cargo test` never invokes the macro.

use crate::Nes;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmNes {
    inner: Nes,
}

#[wasm_bindgen]
impl WasmNes {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmNes {
        console_error_panic_hook_set();
        WasmNes { inner: Nes::new() }
    }

    /// Load an iNES / NES 2.0 ROM. Returns false on a bad header or an
    /// unsupported mapper.
    pub fn load_rom(&mut self, bytes: &[u8]) -> bool {
        self.inner.load_rom(bytes).is_ok()
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// 256×240 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
    /// Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot
    /// present path; this copying variant is kept for callers that want an
    /// owned buffer.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. The host builds a
    /// `Uint8Array(memory.buffer, ptr, len)` view over it — no per-frame copy.
    /// Re-read each present: a wasm allocation that grows memory detaches the
    /// old `memory.buffer`.
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    /// Length in bytes of the framebuffer view (256×240×4 = 245760).
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask for controller 0. Bit order: bit0 A, bit1 B,
    /// bit2 Select, bit3 Start, bit4 Up, bit5 Down, bit6 Left, bit7 Right.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys((bits & 0xFF) as u8);
    }

    /// Same as `set_keys` but for a specific controller port (0 or 1).
    pub fn set_keys_port(&mut self, port: u32, bits: u32) {
        self.inner.set_keys_port(port as usize, (bits & 0xFF) as u8);
    }

    /// Mono f32 samples produced since the last call (host sample rate
    /// `apu::SAMPLE_RATE` = 44100 Hz).
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count() as u32
    }

    pub fn width(&self) -> u32 {
        crate::ppu::SCREEN_W as u32
    }
    pub fn height(&self) -> u32 {
        crate::ppu::SCREEN_H as u32
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("nes-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
