//! wasm-bindgen surface for the web target. Thin wrapper over `Gbc`.
//! Gated to wasm32 so host `cargo test` never invokes the macro.
//!
//! Mirrors `core/src/wasm.rs`: a single emulated screen (160×144 RGBA8888), a
//! copy + zero-copy framebuffer pair, packed-bit input, an audio drain, the
//! frame counter, and the battery-RAM save passthrough.

use crate::Gbc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmGbc {
    inner: Gbc,
}

#[wasm_bindgen]
impl WasmGbc {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmGbc {
        console_error_panic_hook_set();
        WasmGbc { inner: Gbc::new() }
    }

    /// Mount a `.gbc` or `.gb` ROM. A `.gb` (DMG) ROM runs in the CGB's
    /// DMG-compat mode automatically (the header CGB flag drives it).
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// 160×144 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
    /// Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot
    /// present path; this copying variant is kept for callers that want an
    /// owned buffer.
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

    /// Length in bytes of the framebuffer view (160×144×4 = 92160).
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask. Bit order: A=0, B=1, Select=2, Start=3,
    /// Right=4, Left=5, Up=6, Down=7 (active high).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys((bits & 0xFF) as u8);
    }

    /// Interleaved-stereo f32 samples produced since the last call.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count()
    }

    pub fn width(&self) -> u32 {
        crate::ppu::SCREEN_W as u32
    }

    pub fn height(&self) -> u32 {
        crate::ppu::SCREEN_H as u32
    }

    // ---- battery save (cartridge SRAM) ----
    /// Current save-chip contents (write this to a `.sav`).
    pub fn save_ram(&self) -> Vec<u8> {
        self.inner.save_ram().to_vec()
    }
    /// Load a `.sav` into the save chip (call right after `load_rom`).
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        self.inner.load_save_ram(bytes);
    }
    /// True if the save chip changed since the last `clear_save_dirty`.
    pub fn save_dirty(&self) -> bool {
        self.inner.save_dirty()
    }
    pub fn clear_save_dirty(&mut self) {
        self.inner.clear_save_dirty();
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("gbc-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
