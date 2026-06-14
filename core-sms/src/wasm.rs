//! wasm-bindgen surface for the web target. Thin wrapper over `Sms`,
//! mirroring `core/src/wasm.rs`'s `WasmGba`. Gated to wasm32 so host
//! `cargo test` never invokes the macro.
//!
//! ONE binding handles both consoles: the constructor's `game_gear` flag picks
//! Game Gear (160×144, 12-bit palette, GG ports) vs Master System (256×192,
//! 6-bit palette). `.sms` files -> `new(false)`, `.gg` files -> `new(true)`.

use crate::{Sms, System};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmSms {
    inner: Sms,
}

#[wasm_bindgen]
impl WasmSms {
    /// Construct a core. `game_gear == true` selects the Game Gear (160×144,
    /// 12-bit CRAM, GG ports); `false` selects the Master System (256×192,
    /// 6-bit CRAM).
    #[wasm_bindgen(constructor)]
    pub fn new(game_gear: bool) -> WasmSms {
        console_error_panic_hook_set();
        let system = if game_gear {
            System::GameGear
        } else {
            System::Sms
        };
        WasmSms {
            inner: Sms::new(system),
        }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`). SMS is
    /// 256×192; GG is the centred 160×144 crop. Prefer the zero-copy
    /// `framebuffer_ptr`/`framebuffer_len` pair on the hot present path.
    pub fn framebuffer(&mut self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. The host builds a
    /// `Uint8Array(memory.buffer, ptr, len)` view over it — no per-frame copy.
    /// Re-read each present: a wasm allocation that grows memory detaches the
    /// old `memory.buffer`. (Call `framebuffer_len` for the length.)
    pub fn framebuffer_ptr(&mut self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    /// Length in bytes of the framebuffer view (SMS 256×192×4 = 196608, GG
    /// 160×144×4 = 92160).
    pub fn framebuffer_len(&mut self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Pressed-button bitmask for player 1. Bit order: bit0 Up, bit1 Down,
    /// bit2 Left, bit3 Right, bit4 button1, bit5 button2, bit6 Start/Pause.
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Player-2 buttons (SMS only; same bit order).
    pub fn set_keys_p2(&mut self, bits: u32) {
        self.inner.set_keys_p2(bits);
    }

    /// Mono f32 samples produced since the last call (host sample rate
    /// `psg::SAMPLE_RATE` = 44100 Hz).
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

    // ---- battery save (on-cart RAM) ----
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

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("sms-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
