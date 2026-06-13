//! C-ABI surface over `gba_core::Gba` for the React Native / mobile host.
//!
//! The companion header is `include/gba_core_ffi.h`. The intended consumer is a
//! JSI / TurboModule native module (Swift on iOS, Kotlin/JNI on Android) that:
//!   1. `gba_new()` once, holding the opaque pointer for the session;
//!   2. each frame: `gba_set_keys`, `gba_run_frame`, then wraps the framebuffer
//!      via `gba_framebuffer_ptr` / `gba_framebuffer_len` as a *zero-copy* JSI
//!      ArrayBuffer handed to JS (mirrors the wasm `framebuffer_ptr` path);
//!   3. `gba_free()` on teardown.
//!
//! All pointer params are borrowed for the duration of the call only. The
//! framebuffer/save pointers returned by the accessors alias the core's own
//! memory and stay valid until the next mutating call — copy out before the
//! next `gba_run_frame` / state load.
//!
//! Safety: every `gba_*` function dereferences the opaque handle, so the caller
//! must pass a pointer obtained from `gba_new()` and never use it after
//! `gba_free()`. Data pointers must be valid for the given length.

use gba_core::Gba;

/// Allocate a core instance. Returns an opaque handle owned by the caller;
/// release it with [`gba_free`]. Never null.
#[no_mangle]
pub extern "C" fn gba_new() -> *mut Gba {
    Box::into_raw(Box::new(Gba::new()))
}

/// Free a core instance previously returned by [`gba_new`]. Null is a no-op.
///
/// # Safety
/// `gba` must come from `gba_new` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn gba_free(gba: *mut Gba) {
    if !gba.is_null() {
        drop(Box::from_raw(gba));
    }
}

/// Load a cartridge image (resets the CPU and picks the save backend).
///
/// # Safety
/// `data` must point to `len` readable bytes; `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_load_rom(gba: *mut Gba, data: *const u8, len: usize) {
    let g = &mut *gba;
    g.load_rom(std::slice::from_raw_parts(data, len));
}

/// Run one full video frame.
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_run_frame(gba: *mut Gba) {
    (*gba).run_frame();
}

/// Set the raw pressed-button bitmask (bit layout per `keypad::Key`).
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_set_keys(gba: *mut Gba, bits: u32) {
    (*gba).set_keys(bits);
}

/// Pointer to the 240×160 RGBA8888 framebuffer inside the core. Pair with
/// [`gba_framebuffer_len`]. Valid until the next `gba_run_frame` / state load —
/// wrap it as a zero-copy view, don't retain it across frames.
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_framebuffer_ptr(gba: *const Gba) -> *const u8 {
    (*gba).framebuffer().as_ptr()
}

/// Length in bytes of the framebuffer (240×160×4 = 153600).
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_framebuffer_len(gba: *const Gba) -> usize {
    (*gba).framebuffer().len()
}

/// Frame counter (monotonic; for the host's pacing/telemetry).
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_frame_count(gba: *const Gba) -> u32 {
    (*gba).frame_count()
}

/// Drain interleaved-stereo f32 samples produced since the last call into
/// `out` (capacity `max` samples). Returns the number written; any samples
/// beyond `max` for this call are dropped.
///
/// # Safety
/// `out` must point to `max` writable f32 slots; `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_drain_audio(gba: *mut Gba, out: *mut f32, max: usize) -> usize {
    let samples = (*gba).drain_audio();
    let n = samples.len().min(max);
    std::ptr::copy_nonoverlapping(samples.as_ptr(), out, n);
    n
}

// ---- battery save (cartridge SRAM/Flash/EEPROM) ----

/// Copy the current save-chip contents into `out` (capacity `max`). Returns the
/// chip's real size; if it exceeds `max` nothing is copied (call again with a
/// big-enough buffer — query the size by passing `max = 0`).
///
/// # Safety
/// `out` must point to `max` writable bytes; `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_save_ram(gba: *const Gba, out: *mut u8, max: usize) -> usize {
    let data = (*gba).save_ram();
    if data.len() <= max && !out.is_null() {
        std::ptr::copy_nonoverlapping(data.as_ptr(), out, data.len());
    }
    data.len()
}

/// Load a `.sav` image into the active save chip (call right after the ROM).
///
/// # Safety
/// `data` must point to `len` readable bytes; `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_load_save_ram(gba: *mut Gba, data: *const u8, len: usize) {
    (*gba).load_save_ram(std::slice::from_raw_parts(data, len));
}

/// True if the save chip changed since the last [`gba_clear_save_dirty`].
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_save_dirty(gba: *const Gba) -> bool {
    (*gba).save_dirty()
}

/// Clear the save-dirty flag (call after persisting the `.sav`).
///
/// # Safety
/// `gba` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn gba_clear_save_dirty(gba: *mut Gba) {
    (*gba).clear_save_dirty();
}
