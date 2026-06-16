//! libretro core wrapping the unified `emu-native` `Emu`, so every core can run
//! in RetroArch / any libretro frontend.
//!
//! This is a single "polyglot" core: it detects which system to spin up from the
//! loaded content's file extension (and the Xbox disc magic), then drives that
//! `Emu` each frame — translating RetroPad input, swizzling the RGBA framebuffer
//! to libretro's XRGB8888, and converting f32 audio to interleaved i16 stereo.
//!
//! libretro is single-instance and single-threaded, so the state lives in a
//! `static mut` accessed through `state()`, matching the C reference cores.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_void};
use emu_native::{
    Emu, System, BTN_DOWN, BTN_EAST, BTN_L1, BTN_L2, BTN_LEFT, BTN_NORTH, BTN_R1, BTN_R2, BTN_RIGHT,
    BTN_SELECT, BTN_SOUTH, BTN_START, BTN_UP, BTN_WEST,
};

// ---- libretro ABI types ----

#[repr(C)]
pub struct retro_system_info {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
pub struct retro_game_geometry {
    pub base_width: u32,
    pub base_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub aspect_ratio: f32,
}

#[repr(C)]
pub struct retro_system_timing {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
pub struct retro_system_av_info {
    pub geometry: retro_game_geometry,
    pub timing: retro_system_timing,
}

#[repr(C)]
pub struct retro_game_info {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

type EnvironmentCb = extern "C" fn(cmd: u32, data: *mut c_void) -> bool;
type VideoRefreshCb = extern "C" fn(data: *const c_void, width: u32, height: u32, pitch: usize);
type AudioSampleCb = extern "C" fn(left: i16, right: i16);
type AudioBatchCb = extern "C" fn(data: *const i16, frames: usize) -> usize;
type InputPollCb = extern "C" fn();
type InputStateCb = extern "C" fn(port: u32, device: u32, index: u32, id: u32) -> i16;

// Environment commands we use.
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;
const RETRO_PIXEL_FORMAT_XRGB8888: u32 = 1;

// Device + RetroPad button ids.
const RETRO_DEVICE_JOYPAD: u32 = 1;
const ID_B: u32 = 0;
const ID_Y: u32 = 1;
const ID_SELECT: u32 = 2;
const ID_START: u32 = 3;
const ID_UP: u32 = 4;
const ID_DOWN: u32 = 5;
const ID_LEFT: u32 = 6;
const ID_RIGHT: u32 = 7;
const ID_A: u32 = 8;
const ID_X: u32 = 9;
const ID_L: u32 = 10;
const ID_R: u32 = 11;
const ID_L2: u32 = 12;
const ID_R2: u32 = 13;

// ---- global state ----

struct State {
    emu: Option<Emu>,
    environ: Option<EnvironmentCb>,
    video: Option<VideoRefreshCb>,
    audio_batch: Option<AudioBatchCb>,
    input_poll: Option<InputPollCb>,
    input_state: Option<InputStateCb>,
    video_buf: Vec<u32>,
    audio_f32: Vec<f32>,
    audio_i16: Vec<i16>,
    // Battery save (SRAM/Flash/EEPROM). RetroArch reads/writes this buffer
    // directly via `retro_get_memory_data`: it loads the `.srm` into it after
    // load, and persists it back on save. We mirror it to/from the core.
    sram: Vec<u8>,
    sram_loaded: bool,
}

static mut STATE: State = State {
    emu: None,
    environ: None,
    video: None,
    audio_batch: None,
    input_poll: None,
    input_state: None,
    video_buf: Vec::new(),
    audio_f32: Vec::new(),
    audio_i16: Vec::new(),
    sram: Vec::new(),
    sram_loaded: false,
};

// libretro memory type ids.
const RETRO_MEMORY_SAVE_RAM: u32 = 0;

#[allow(static_mut_refs)]
fn state() -> &'static mut State {
    // SAFETY: libretro calls every entry point from a single thread.
    unsafe { &mut *core::ptr::addr_of_mut!(STATE) }
}

// ---- system detection ----

/// Pick a system from the content path's extension; falls back to sniffing the
/// Xbox disc magic in `data`.
fn detect_system(path: Option<&str>, data: &[u8]) -> Option<System> {
    let ext = path
        .and_then(|p| p.rsplit('.').next())
        .map(|e| e.to_ascii_lowercase());
    let by_ext = match ext.as_deref() {
        Some("gba") => Some(System::Gba),
        Some("nds") => Some(System::Nds),
        Some("nes") => Some(System::Nes),
        Some("sms") => Some(System::Sms),
        Some("gg") => Some(System::GameGear),
        Some("gb" | "gbc") => Some(System::Gbc),
        Some("smc" | "sfc") => Some(System::Snes),
        Some("md" | "gen" | "smd") => Some(System::Genesis),
        Some("pce") => Some(System::Pce),
        Some("a26") => Some(System::Atari2600),
        Some("ngc" | "ngp") => Some(System::Ngpc),
        Some("ws" | "wsc") => Some(System::WonderSwan),
        Some("vb" | "vboy") => Some(System::VirtualBoy),
        Some("n64" | "z64" | "v64") => Some(System::N64),
        Some("xbe" | "xiso") => Some(System::Xbox),
        Some("cue" | "bin" | "img" | "iso" | "pbp") => Some(System::Ps1),
        _ => None,
    };
    // Xbox disc magic at sector 32 (offset 0x10000) overrides an ambiguous .iso.
    const MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";
    if data.len() >= 0x10000 + MAGIC.len() && &data[0x10000..0x10000 + MAGIC.len()] == MAGIC {
        return Some(System::Xbox);
    }
    by_ext
}

/// Read the RetroPad into our logical `BTN_*` mask.
fn read_input(cb: InputStateCb) -> u32 {
    let mut m = 0u32;
    let mut on = |id: u32, flag: u32| {
        if cb(0, RETRO_DEVICE_JOYPAD, 0, id) != 0 {
            m |= flag;
        }
    };
    on(ID_UP, BTN_UP);
    on(ID_DOWN, BTN_DOWN);
    on(ID_LEFT, BTN_LEFT);
    on(ID_RIGHT, BTN_RIGHT);
    on(ID_A, BTN_EAST);
    on(ID_B, BTN_SOUTH);
    on(ID_X, BTN_NORTH);
    on(ID_Y, BTN_WEST);
    on(ID_L, BTN_L1);
    on(ID_R, BTN_R1);
    on(ID_L2, BTN_L2);
    on(ID_R2, BTN_R2);
    on(ID_START, BTN_START);
    on(ID_SELECT, BTN_SELECT);
    m
}

// ---- libretro entry points ----

#[no_mangle]
pub extern "C" fn retro_api_version() -> u32 {
    1
}

#[no_mangle]
pub extern "C" fn retro_init() {}

#[no_mangle]
pub extern "C" fn retro_deinit() {
    state().emu = None;
}

#[no_mangle]
pub unsafe extern "C" fn retro_get_system_info(info: *mut retro_system_info) {
    if info.is_null() {
        return;
    }
    (*info).library_name = c"imlunahey emulator".as_ptr();
    (*info).library_version = c"0.1".as_ptr();
    (*info).valid_extensions = c"gba|nds|nes|sms|gg|gb|gbc|smc|sfc|md|gen|smd|pce|a26|ngc|ngp|ws|wsc|vb|vboy|n64|z64|v64|cue|bin|img|iso|pbp|xbe|xiso".as_ptr();
    (*info).need_fullpath = false;
    (*info).block_extract = false;
}

#[no_mangle]
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut retro_system_av_info) {
    if info.is_null() {
        return;
    }
    let s = state();
    let (w, h, rate) = match s.emu.as_ref() {
        Some(e) => (e.width(), e.height(), e.sample_rate() as f64),
        None => (320, 240, 44100.0),
    };
    (*info).geometry = retro_game_geometry {
        base_width: w,
        base_height: h,
        // Generous max so cores that change resolution (PS1) don't get clipped.
        max_width: 1024,
        max_height: 1024,
        aspect_ratio: w as f32 / h as f32,
    };
    (*info).timing = retro_system_timing {
        fps: 60.0,
        sample_rate: rate,
    };
}

#[no_mangle]
pub extern "C" fn retro_set_environment(cb: EnvironmentCb) {
    let s = state();
    s.environ = Some(cb);
    // Request XRGB8888 video (our framebuffer is 32-bit).
    let mut fmt: u32 = RETRO_PIXEL_FORMAT_XRGB8888;
    cb(
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
        &mut fmt as *mut u32 as *mut c_void,
    );
}

#[no_mangle]
pub extern "C" fn retro_set_video_refresh(cb: VideoRefreshCb) {
    state().video = Some(cb);
}

#[no_mangle]
pub extern "C" fn retro_set_audio_sample(_cb: AudioSampleCb) {}

#[no_mangle]
pub extern "C" fn retro_set_audio_sample_batch(cb: AudioBatchCb) {
    state().audio_batch = Some(cb);
}

#[no_mangle]
pub extern "C" fn retro_set_input_poll(cb: InputPollCb) {
    state().input_poll = Some(cb);
}

#[no_mangle]
pub extern "C" fn retro_set_input_state(cb: InputStateCb) {
    state().input_state = Some(cb);
}

#[no_mangle]
pub extern "C" fn retro_set_controller_port_device(_port: u32, _device: u32) {}

#[no_mangle]
pub extern "C" fn retro_reset() {
    // Recreate the core for the same system (a clean reset).
    let s = state();
    if let Some(sys) = s.emu.as_ref().map(|e| e.system()) {
        s.emu = Some(Emu::new(sys));
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro_load_game(game: *const retro_game_info) -> bool {
    if game.is_null() {
        return false;
    }
    let g = &*game;
    if g.data.is_null() || g.size == 0 {
        return false;
    }
    let data = core::slice::from_raw_parts(g.data as *const u8, g.size);
    let path = if g.path.is_null() {
        None
    } else {
        core::ffi::CStr::from_ptr(g.path).to_str().ok()
    };
    let Some(system) = detect_system(path, data) else {
        return false;
    };
    let mut emu = Emu::new(system);
    if !emu.load_rom(data) {
        return false;
    }
    // Size the SRAM mirror to the core's save chip. RetroArch grabs a pointer to
    // this buffer (via retro_get_memory_data) and writes the .srm into it before
    // the first frame; `retro_run` then pushes it into the core.
    let save = emu.save_data();
    let s = state();
    s.sram = save;
    s.sram_loaded = false;
    s.emu = Some(emu);
    true
}

#[no_mangle]
pub extern "C" fn retro_load_game_special(_t: u32, _info: *const retro_game_info, _n: usize) -> bool {
    false
}

#[no_mangle]
pub extern "C" fn retro_unload_game() {
    let s = state();
    s.emu = None;
    s.sram = Vec::new();
    s.sram_loaded = false;
}

#[no_mangle]
pub extern "C" fn retro_get_region() -> u32 {
    0 // RETRO_REGION_NTSC
}

#[no_mangle]
pub extern "C" fn retro_run() {
    let s = state();
    if let Some(poll) = s.input_poll {
        poll();
    }
    let Some(emu) = s.emu.as_mut() else {
        return;
    };

    // Push the .srm RetroArch loaded into our buffer into the core, once.
    if !s.sram_loaded {
        s.sram_loaded = true;
        if !s.sram.is_empty() {
            emu.load_save(&s.sram);
        }
    }

    if let Some(istate) = s.input_state {
        emu.set_buttons(read_input(istate));
    }
    emu.run_frame();

    // Mirror the core's save back out (only when it changed) so RetroArch
    // persists current progress to the .srm. The buffer length is fixed, so the
    // pointer RetroArch holds stays valid.
    if emu.save_dirty() {
        let d = emu.save_data();
        if d.len() == s.sram.len() {
            s.sram.copy_from_slice(&d);
        }
        emu.clear_save_dirty();
    }

    // Video: RGBA8888 -> XRGB8888 (0xFFRRGGBB).
    let w = emu.width();
    let h = emu.height();
    let fb = emu.framebuffer();
    if fb.len() == (w * h * 4) as usize {
        s.video_buf.clear();
        s.video_buf.reserve((w * h) as usize);
        for px in fb.chunks_exact(4) {
            let (r, gg, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
            s.video_buf.push(0xFF00_0000 | (r << 16) | (gg << 8) | b);
        }
        if let Some(video) = s.video {
            video(
                s.video_buf.as_ptr() as *const c_void,
                w,
                h,
                (w * 4) as usize,
            );
        }
    }

    // Audio: f32 interleaved -> i16 interleaved stereo.
    if s.audio_f32.len() != 16384 {
        s.audio_f32.resize(16384, 0.0);
    }
    let channels = emu.channels().max(1) as usize;
    let n = emu.drain_audio(&mut s.audio_f32);
    if n > 0 {
        s.audio_i16.clear();
        let to_i16 = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16;
        if channels >= 2 {
            for pair in s.audio_f32[..n].chunks_exact(2) {
                s.audio_i16.push(to_i16(pair[0]));
                s.audio_i16.push(to_i16(pair[1]));
            }
        } else {
            for &v in &s.audio_f32[..n] {
                let x = to_i16(v);
                s.audio_i16.push(x);
                s.audio_i16.push(x);
            }
        }
        let frames = s.audio_i16.len() / 2;
        if let Some(batch) = s.audio_batch {
            batch(s.audio_i16.as_ptr(), frames);
        }
    }
}

// Save state / memory / cheats: not yet wired through the FFI.
#[no_mangle]
pub extern "C" fn retro_serialize_size() -> usize {
    0
}
#[no_mangle]
pub extern "C" fn retro_serialize(_data: *mut c_void, _size: usize) -> bool {
    false
}
#[no_mangle]
pub extern "C" fn retro_unserialize(_data: *const c_void, _size: usize) -> bool {
    false
}
#[no_mangle]
pub extern "C" fn retro_cheat_reset() {}
#[no_mangle]
pub extern "C" fn retro_cheat_set(_index: u32, _enabled: bool, _code: *const c_char) {}
#[no_mangle]
pub extern "C" fn retro_get_memory_data(id: u32) -> *mut c_void {
    let s = state();
    if id == RETRO_MEMORY_SAVE_RAM && !s.sram.is_empty() {
        s.sram.as_mut_ptr() as *mut c_void
    } else {
        core::ptr::null_mut()
    }
}
#[no_mangle]
pub extern "C" fn retro_get_memory_size(id: u32) -> usize {
    if id == RETRO_MEMORY_SAVE_RAM {
        state().sram.len()
    } else {
        0
    }
}
