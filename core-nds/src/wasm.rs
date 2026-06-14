//! wasm-bindgen surface for the web target. Thin wrapper over `Nds`, the DS
//! sibling of the GBA core's `WasmGba`. Gated to wasm32 so host `cargo test`
//! never invokes the macro.
//!
//! The DS has TWO screens, so the framebuffer API is doubled (top + bottom),
//! each a 256x192 RGBA8888 buffer. Everything else mirrors `WasmGba`'s style:
//! a copying accessor plus a zero-copy `*_ptr`/`*_len` pair for the hot present
//! path, active-low key masks, an HLE touch setter, and the cart save-chip
//! battery-save trio (`save_ram`/`load_save_ram`/`save_dirty`).
use crate::Nds;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmNds {
    inner: Nds,
}

#[wasm_bindgen]
impl WasmNds {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmNds {
        console_error_panic_hook_set();
        WasmNds { inner: Nds::new() }
    }

    /// Load a `.nds` cartridge image and HLE-boot it (parse header, copy
    /// binaries + overlays into RAM, mount the cart, reset both CPUs).
    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    // ---- framebuffers (two 256x192 RGBA8888 screens) ----

    /// 256x192 RGBA8888 framebuffer for the TOP screen (copied into a fresh JS
    /// `Uint8Array`). Prefer the zero-copy `top_framebuffer_ptr`/`_len` pair on
    /// the hot present path; this copying variant is kept for callers that want
    /// an owned buffer.
    pub fn top_framebuffer(&self) -> Vec<u8> {
        self.inner.top_framebuffer().to_vec()
    }

    /// 256x192 RGBA8888 framebuffer for the BOTTOM screen (copied).
    pub fn bottom_framebuffer(&self) -> Vec<u8> {
        self.inner.bottom_framebuffer().to_vec()
    }

    /// Address of the TOP framebuffer inside wasm linear memory. The host builds
    /// a `Uint8Array(memory.buffer, ptr, len)` view over it — no per-frame copy.
    /// Re-read each present: any wasm allocation that grows memory detaches the
    /// old `memory.buffer`.
    pub fn top_framebuffer_ptr(&self) -> usize {
        self.inner.top_framebuffer().as_ptr() as usize
    }
    /// Length in bytes of a framebuffer view (256x192x4 = 196608).
    pub fn top_framebuffer_len(&self) -> usize {
        self.inner.top_framebuffer().len()
    }
    pub fn bottom_framebuffer_ptr(&self) -> usize {
        self.inner.bottom_framebuffer().as_ptr() as usize
    }
    pub fn bottom_framebuffer_len(&self) -> usize {
        self.inner.bottom_framebuffer().len()
    }

    // ---- input ----

    /// Set the button state. Both masks are ACTIVE-LOW (a 0 bit = pressed),
    /// matching the hardware registers the games poll.
    ///
    /// - `keyinput` → KEYINPUT (0x04000130), low 10 bits:
    ///   A, B, Select, Start, Right, Left, Up, Down, R, L.
    /// - `ext_keyinput` → EXTKEYIN (0x04000136): bit 0 = X, bit 1 = Y.
    pub fn set_keys(&mut self, keyinput: u32, ext_keyinput: u32) {
        self.inner.set_keys(keyinput, ext_keyinput);
    }

    /// Set the touchscreen pointer. `pressed` gates the SPI touch latches the
    /// HLE touch tick cooks into the OS shared-work struct each VBlank. When
    /// pressed, `x`/`y` are bottom-screen coordinates (0..255 / 0..191).
    pub fn set_touch(&mut self, pressed: bool, x: u16, y: u16) {
        self.inner.set_touch(pressed, x, y);
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.ppu.frame_count
    }

    // ---- battery save (cartridge AUXSPI backup chip) ----

    /// Current save-chip contents (write this to a `.sav`). Empty if no cart is
    /// mounted.
    pub fn save_ram(&self) -> Vec<u8> {
        match self.inner.cart.as_ref() {
            Some(cart) => cart.sav().to_vec(),
            None => Vec::new(),
        }
    }

    /// Load a `.sav` into the save chip (call right after `load_rom`). No-op if
    /// no cart is mounted.
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        if let Some(cart) = self.inner.cart.as_mut() {
            cart.load_sav(bytes);
        }
    }

    /// True if the save chip changed since the last `clear_save_dirty` — the
    /// host polls this to know when to persist the `.sav`.
    pub fn save_dirty(&self) -> bool {
        self.inner.cart.as_ref().is_some_and(|c| c.sav_dirty)
    }

    pub fn clear_save_dirty(&mut self) {
        if let Some(cart) = self.inner.cart.as_mut() {
            cart.sav_dirty = false;
        }
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("nds-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
