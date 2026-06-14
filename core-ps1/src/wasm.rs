//! wasm-bindgen surface for the web target. Thin wrapper over [`Psx`].
//! Gated to wasm32 so host `cargo test` never invokes the macro.
//!
//! Mirrors the GBA core's `WasmGba` (`../../core/src/wasm.rs`): a constructor
//! that installs the panic hook, BIOS/disc loaders, a per-frame `run_frame`,
//! the zero-copy `framebuffer_ptr`/`framebuffer_len` present path (plus a
//! copying `framebuffer` convenience), the digital-pad `set_keys`, an audio
//! drain, and the frame/size getters.

use crate::Psx;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmPsx {
    inner: Psx,
}

#[wasm_bindgen]
impl WasmPsx {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmPsx {
        console_error_panic_hook_set();
        WasmPsx { inner: Psx::new() }
    }

    /// Load a 512 KB BIOS ROM the user supplies (the PSX cannot boot a real
    /// game without it). Resets the machine to the BIOS entry point.
    pub fn load_bios(&mut self, bytes: &[u8]) {
        self.inner.load_bios(bytes);
    }

    /// Mount a disc image (`.bin`, MODE2/2352) — the disc is the game.
    pub fn load_disc(&mut self, bytes: &[u8]) {
        self.inner.load_disc(bytes);
    }

    /// Load a game image: a PS-X EXE is side-loaded directly (handy for
    /// homebrew / no-BIOS), anything else is mounted as a `.bin` disc.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
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

    /// Current display width in pixels (the PSX video mode varies; the host
    /// re-reads this each present and sizes its canvas accordingly).
    pub fn width(&self) -> u32 {
        self.inner.width()
    }

    /// Current display height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Digital-pad button bitmask, active-high (bit layout per
    /// `crate::sio::Button`).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys((bits & 0xFFFF) as u16);
    }

    /// Interleaved-stereo f32 samples produced since the last call (44.1 kHz).
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
            web_error(&format!("ps1-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
