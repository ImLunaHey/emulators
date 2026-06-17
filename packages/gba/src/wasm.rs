//! wasm-bindgen surface for the web target. Thin wrapper over `Gba`.
//! Gated to wasm32 so host `cargo test` never invokes the macro.
use crate::Gba;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmGba {
    inner: Gba,
}

#[wasm_bindgen]
impl WasmGba {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmGba {
        console_error_panic_hook_set();
        WasmGba { inner: Gba::new() }
    }

    pub fn load_rom(&mut self, bytes: &[u8]) {
        self.inner.load_rom(bytes);
    }

    /// Boot a multiboot (`.mb`) image as a child unit (Single-Pak link receive):
    /// the image runs from EWRAM at `0x020000C0`. Returns `false` if the image
    /// is too small or too large for EWRAM.
    pub fn load_multiboot(&mut self, bytes: &[u8]) -> bool {
        self.inner.load_multiboot(bytes)
    }

    pub fn run_frame(&mut self) {
        self.inner.run_frame();
    }

    /// Resumable frame runner for the synchronous local-link (duo) path. Runs up
    /// to `max_cycles` of the current frame and returns a status:
    ///   0 = slice exhausted (frame not done, no pending transfer) — call again
    ///   1 = visual frame completed — start the next frame
    ///   2 = paused on a pending master link transfer — resolve it, then resume
    /// Lets the host interleave two cores slice-by-slice so each multiplay
    /// transfer is exchanged within the frame the game expects it.
    pub fn run_slice(&mut self, max_cycles: u32) -> u32 {
        let (_, frame_done, paused) = self.inner.run_slice(max_cycles);
        if frame_done {
            1
        } else if paused {
            2
        } else {
            0
        }
    }

    /// 240×160 RGBA8888 framebuffer (copied into a fresh JS `Uint8Array`).
    /// Prefer the zero-copy `framebuffer_ptr`/`framebuffer_len` pair on the hot
    /// present path; this copying variant is kept for callers that want an
    /// owned buffer.
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Address of the framebuffer inside wasm linear memory. The host builds a
    /// `Uint8Array(memory.buffer, ptr, len)` view over it — no per-frame copy.
    /// Re-read each present: any wasm allocation that grows memory detaches the
    /// old `memory.buffer`, and a savestate load can re-seat the buffer.
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }

    /// Length in bytes of the framebuffer view (240×160×4 = 153600).
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }

    /// Raw pressed-button bitmask (bit layout per `keypad::Key`).
    pub fn set_keys(&mut self, bits: u32) {
        self.inner.set_keys(bits);
    }

    /// Autofire/turbo mask (bit set = that button pulses each frame).
    pub fn set_turbo_mask(&mut self, mask: u32) {
        self.inner.keypad.turbo_mask = mask & 0x3FF;
    }

    /// Interleaved-stereo f32 samples produced since the last call.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.inner.drain_audio()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count()
    }

    // ---- battery save (cartridge SRAM/Flash/EEPROM) ----
    /// Current save-chip contents (write this to a `.sav`).
    pub fn save_ram(&self) -> Vec<u8> {
        self.inner.save_ram().to_vec()
    }
    /// Load a `.sav` into the save chip (call right after `load_rom`).
    pub fn load_save_ram(&mut self, bytes: &[u8]) {
        self.inner.load_save_ram(bytes);
    }
    /// True if the save chip changed since the last `clear_save_dirty` — the
    /// host polls this to know when to persist the `.sav`.
    pub fn save_dirty(&self) -> bool {
        self.inner.save_dirty()
    }
    pub fn clear_save_dirty(&mut self) {
        self.inner.clear_save_dirty();
    }
    /// Erase the save chip (fill 0xFF + mark dirty) — backs the UI's clear-save.
    pub fn reset_save(&mut self) {
        self.inner.reset_save();
    }
    /// Detected save type as a display string (flash128/flash64/sram/...).
    pub fn save_type(&self) -> String {
        self.inner.save_type_str().to_string()
    }

    // ---- save states (full machine snapshot) ----
    pub fn save_state(&self) -> Vec<u8> {
        self.inner.save_state()
    }
    /// Restore a snapshot. Returns false on a bad/incompatible blob.
    pub fn load_state(&mut self, blob: &[u8]) -> bool {
        self.inner.load_state(blob).is_ok()
    }

    /// FNV-1a hash of the full machine snapshot — a cheap fingerprint of the
    /// entire deterministic state. Used by lockstep netplay to detect desync
    /// (both peers run the same dual-core sim; hashes must match every frame)
    /// and by the determinism self-check. Computed in Rust so the multi-hundred-
    /// KB snapshot never crosses into JS.
    pub fn state_hash(&self) -> u32 {
        let bytes = self.inner.save_state();
        let mut h: u32 = 0x811c_9dc5;
        for b in bytes {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    // ---- cheats ----
    /// Set the active cheat codes (newline-separated raw codes). Pass the
    /// enabled cheats only; they're applied once per frame.
    pub fn set_cheats(&mut self, codes_newline_joined: &str) {
        self.inner.set_cheats(codes_newline_joined);
    }

    /// Parse a raw cheat code for the editor's live validation, returning
    /// `[supported, unsupported, total]` line counts. Uses the exact parser
    /// the engine applies (`crate::cheats::parse_cheat`), so the UI can never
    /// disagree with what actually runs — the old TS reimplementation could.
    pub fn parse_cheat_summary(&self, code: &str) -> Vec<u32> {
        let lines = crate::cheats::parse_cheat(code);
        let unsupported = lines
            .iter()
            .filter(|l| l.r#type == crate::cheats::LineType::Unsupported)
            .count() as u32;
        let total = lines.len() as u32;
        vec![total - unsupported, unsupported, total]
    }

    // ---- debug / introspection (DebugPanel + LinkPanel) ----
    /// JSON snapshot of CPU/PPU/DMA/timer/IRQ/sound/SIO scalar state.
    pub fn debug_state(&self) -> String {
        self.inner.debug_state_json()
    }
    /// Debug bus reads (DebugPanel memory viewer / LinkPanel SIOCNT readback).
    pub fn read8(&mut self, addr: u32) -> u32 {
        self.inner.dbg_read8(addr)
    }
    pub fn read16(&mut self, addr: u32) -> u32 {
        self.inner.dbg_read16(addr)
    }
    /// Memory-region copies for the palette/tile/sprite/memory debug views.
    pub fn vram(&self) -> Vec<u8> {
        self.inner.mem.vram.to_vec()
    }
    pub fn pram(&self) -> Vec<u8> {
        self.inner.mem.pram.to_vec()
    }
    pub fn oam(&self) -> Vec<u8> {
        self.inner.mem.oam.to_vec()
    }

    // ---- SIO trace (LinkPanel's SioTracer) ----
    pub fn sio_set_trace(&mut self, on: bool) {
        self.inner.sio_set_trace(on);
    }
    pub fn sio_clear_trace(&mut self) {
        self.inner.sio_clear_trace();
    }
    pub fn sio_trace(&self) -> String {
        self.inner.sio_trace_json()
    }

    // ---- async WebRTC link-cable bridge (sio-signal.ts) ----

    /// Set the live link state. `connected` drives SIOCNT.SD; `master` drives
    /// SIOCNT SI/ID. `connected == false` (default) keeps single-player.
    pub fn sio_set_link(&mut self, connected: bool, master: bool) {
        self.inner.sio_set_link(connected, master);
    }

    /// Poll the master's outgoing multiplay payload. Returns the 16-bit
    /// SIOMLT_SEND value once (take semantics) after a transfer starts over a
    /// connected link, or -1 when there's nothing to send.
    pub fn sio_take_outgoing(&mut self) -> i32 {
        match self.inner.sio_take_outgoing() {
            Some(v) => (v & 0xFFFF) as i32,
            None => -1,
        }
    }

    /// Non-consuming read of the current SIOMLT_SEND word (0..0xFFFF). The slave
    /// side reads this to answer the master's `mlt-req` with its own data — the
    /// slave never stages `outgoing` (it doesn't master a transfer), so
    /// `sio_take_outgoing` always returns -1 for it.
    pub fn sio_peek_mlt_send(&self) -> u32 {
        self.inner.sio_peek_mlt_send()
    }

    /// Master-side completion: deliver the synchronized 4-slot result the host
    /// gathered from peers (latch SIOMULTI, bump transfer_seq, clear START,
    /// raise the SIO IRQ if enabled).
    pub fn sio_deliver_multiplay(&mut self, m0: u32, m1: u32, m2: u32, m3: u32, error: bool) {
        self.inner.sio_deliver_multiplay(m0, m1, m2, m3, error);
    }

    /// Slave-side: apply the remote master's broadcast (latch SIOMULTI, clear
    /// START, raise the SIO IRQ if enabled).
    pub fn sio_apply_remote_multiplay(
        &mut self,
        m0: u32,
        m1: u32,
        m2: u32,
        m3: u32,
        error: bool,
    ) {
        self.inner.sio_apply_remote_multiplay(m0, m1, m2, m3, error);
    }

    /// Attach (`true`) or detach (`false`) the GBA Wireless Adapter as the SIO
    /// Normal-32 peripheral. When attached, wireless-capable games detect the
    /// adapter and walk its command protocol (single-player HLE: no peers).
    pub fn sio_set_wireless_adapter(&mut self, enabled: bool) {
        self.inner.sio_set_wireless_adapter(enabled);
    }

    // ---- Wireless Adapter peer seam (the JS transport drives these) ----
    // No-ops unless the wireless adapter is the active transport.

    /// Advance the adapter's wait timeout by `frames` (call once per frame).
    pub fn sio_wl_update(&mut self, frames: u32) {
        self.inner.wl_update(frames);
    }

    /// Register a connected client (host side); returns its device ID, or 0.
    pub fn sio_wl_host_add_client(&mut self) -> u16 {
        self.inner.wl_host_add_client()
    }

    /// Finalize this adapter as a client the host accepted.
    pub fn sio_wl_client_set_connected(&mut self, devid: u16, clnum: u16) {
        self.inner.wl_client_set_connected(devid, clnum);
    }

    /// Drop the peer link (wakes a parked wait with a disconnect event).
    pub fn sio_wl_disconnect_peer(&mut self) {
        self.inner.wl_disconnect_peer();
    }

    /// Inject a packet received from the peer.
    pub fn sio_wl_deliver_packet(&mut self, bytes: &[u8]) {
        self.inner.wl_deliver_packet(bytes);
    }

    /// Take the packet the game queued to send, if any (Uint8Array | undefined).
    pub fn sio_wl_take_outgoing(&mut self) -> Option<Vec<u8>> {
        self.inner.wl_take_outgoing()
    }

    /// Take the host device ID the game asked to CONNECT to (once), or -1 when
    /// no connect is pending. The transport relays a connect request to it.
    pub fn sio_wl_pending_connect(&mut self) -> i32 {
        match self.inner.wl_take_pending_connect() {
            Some(devid) => devid as i32,
            None => -1,
        }
    }

    /// Drain the wireless adapter's SPI word trace as JSON (`[[sent,reply],…]`),
    /// for diagnosing adapter detection.
    pub fn sio_wl_trace(&mut self) -> String {
        self.inner.wl_trace_json()
    }

    /// Surface a discovered host: `devid` + 6 broadcast words (`data.len() == 6`).
    pub fn sio_wl_add_scanned_host(&mut self, devid: u16, data: &[u32]) {
        let mut d = [0u32; 6];
        for (slot, &v) in d.iter_mut().zip(data.iter()) {
            *slot = v;
        }
        self.inner.wl_add_scanned_host(devid, d);
    }

    /// Clear the discovered-hosts list.
    pub fn sio_wl_clear_scanned_hosts(&mut self) {
        self.inner.wl_clear_scanned_hosts();
    }

    /// This host's broadcast to announce: `[devid, w0, w1, w2, w3, w4, w5]`, or
    /// undefined when the adapter isn't active.
    pub fn sio_wl_broadcast(&mut self) -> Option<Vec<u32>> {
        self.inner.wl_broadcast().map(|(devid, data)| {
            let mut out = Vec::with_capacity(7);
            out.push(devid as u32);
            out.extend_from_slice(&data);
            out
        })
    }

    // ---- IWRAM write-watch (LinkPanel's IwramWatch) ----
    pub fn set_watch(&mut self, lo: u32, hi: u32) {
        self.inner.set_watch(lo, hi);
    }
    pub fn clear_watch(&mut self) {
        self.inner.clear_watch();
    }
    pub fn watch_log(&self) -> String {
        self.inner.take_watch_log()
    }
}

/// The Rust-rendered console home screen, shown on boot. The host blits its
/// framebuffer and feeds it input; `run_frame` returns an action token.
#[wasm_bindgen]
pub struct WasmHome {
    inner: crate::home::Home,
}

#[wasm_bindgen]
impl WasmHome {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmHome {
        console_error_panic_hook_set();
        WasmHome {
            inner: crate::home::Home::new(),
        }
    }

    /// Set the installed-game list from parallel newline-joined columns:
    /// ids, titles, system labels, and playable flags ("1"/"0").
    pub fn set_games(
        &mut self,
        ids_newline: &str,
        titles_newline: &str,
        systems_newline: &str,
        playables_newline: &str,
    ) {
        self.inner
            .set_games_from_str(ids_newline, titles_newline, systems_newline, playables_newline);
    }

    /// Advance one frame with an active-high, GBA-layout button mask. Returns
    /// the action token: `""` (nothing), `"add"` (open add dialog), or
    /// `"play:<id>"` (boot that game).
    pub fn run_frame(&mut self, buttons: u32) -> String {
        action_token(self.inner.run_frame(buttons))
    }

    /// Hit-test a pointer tap in launcher pixel space (host maps canvas coords
    /// → 0..width/0..height). Returns the same action token as `run_frame`.
    pub fn pointer(&mut self, x: i32, y: i32) -> String {
        action_token(self.inner.pointer(x, y))
    }

    /// Attach a 32×32 RGBA icon to a game id (e.g. a decoded NDS banner). Push
    /// these before `set_games`.
    pub fn set_icon(&mut self, id: &str, rgba: &[u8]) {
        self.inner.set_icon(id, rgba);
    }

    /// Push the persisted display setting so the settings toggle reflects it.
    pub fn set_crisp(&mut self, crisp: bool) {
        self.inner.set_crisp(crisp);
    }

    /// Scroll the game grid by `delta` pixels (host mouse wheel / drag).
    pub fn scroll_by(&mut self, delta: i32) {
        self.inner.scroll_by(delta);
    }

    /// RGBA8888 framebuffer as a copy — convenient `putImageData` path for the
    /// menu (perf is irrelevant here; the zero-copy pair below exists for
    /// parity with the emulator's hot path).
    pub fn framebuffer(&self) -> Vec<u8> {
        self.inner.framebuffer().to_vec()
    }

    /// Pointer/len into wasm memory for the RGBA8888 framebuffer (zero-copy
    /// blit, same pattern as `WasmGba::framebuffer_ptr`).
    pub fn framebuffer_ptr(&self) -> usize {
        self.inner.framebuffer().as_ptr() as usize
    }
    pub fn framebuffer_len(&self) -> usize {
        self.inner.framebuffer().len()
    }
    pub fn width(&self) -> u32 {
        crate::home::HOME_W as u32
    }
    pub fn height(&self) -> u32 {
        crate::home::HOME_H as u32
    }
}

/// Decode an NDS ROM's banner title (English, falling back to Japanese).
/// Empty string if the ROM has no parseable banner.
#[wasm_bindgen]
pub fn nds_title(rom: &[u8]) -> String {
    crate::nds::parse_banner(rom).map(|b| b.title).unwrap_or_default()
}

/// Decode an NDS ROM's 32×32 banner icon to RGBA8888 (4096 bytes), or an empty
/// buffer if there's no parseable banner.
#[wasm_bindgen]
pub fn nds_icon_rgba(rom: &[u8]) -> Vec<u8> {
    crate::nds::parse_banner(rom).map(|b| b.icon_rgba).unwrap_or_default()
}

/// Encode a launcher action for JS: `""` none, `"add"`, `"play:<id>"`, or
/// `"soon:<systemLabel>"`.
fn action_token(a: crate::home::HomeAction) -> String {
    match a {
        crate::home::HomeAction::None => String::new(),
        crate::home::HomeAction::AddGame => "add".to_string(),
        crate::home::HomeAction::Launch(id) => format!("play:{id}"),
        crate::home::HomeAction::ComingSoon(label) => format!("soon:{label}"),
        crate::home::HomeAction::SetCrisp(v) => {
            if v {
                "crisp:1".to_string()
            } else {
                "crisp:0".to_string()
            }
        }
        crate::home::HomeAction::ClearAll => "clearall".to_string(),
    }
}

// Minimal panic hook: forward Rust panics to console.error so a crash in the
// browser is legible instead of an opaque "unreachable executed".
fn console_error_panic_hook_set() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_error(&format!("gba-core panic: {info}"));
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_error(s: &str);
}
