//! Unified native FFI for every emulator core.
//!
//! One opaque [`Emu`] handle wraps any core, behind a single C ABI (`emu_*`) so a
//! native front-end (the macOS SwiftPM app in `apps/EmuApp`) can drive them all
//! uniformly. This is the desktop counterpart to the iOS-only `core-ffi` (GBA
//! only) — it covers GBA, PS1, NDS, NES, SMS/GG, GBC and Xbox, and exists so big
//! media (a 4.7 GB Xbox disc) can be loaded outside the browser's 4 GB wasm32
//! address space.
//!
//! # Contract
//!
//! * Call [`emu_new`] once per session; release with [`emu_free`].
//! * Per frame: [`emu_set_keys`], [`emu_run_frame`], then read the framebuffer
//!   via [`emu_framebuffer_ptr`] / [`emu_framebuffer_len`] (RGBA8888,
//!   `emu_width` × `emu_height` × 4). The framebuffer pointer is refreshed at the
//!   end of each `emu_run_frame`; copy or upload it before the next call.
//! * Audio: [`emu_drain_audio`] copies interleaved samples; pair with
//!   [`emu_sample_rate`] and [`emu_channels`].
//!
//! All pointers passed in must be valid for their stated length; the handle must
//! come from [`emu_new`] and must not be used after [`emu_free`].

use gbc_core::Gbc;
use nds_core::Nds;
use nes_core::Nes;
use ps1_core::Psx;
use sms_core::Sms;
use xbox_core::Xbox;

use gba_core::Gba;

use atari2600_core::Atari;
use genesis_core::Genesis;
use n64_core::N64;
use ngpc_core::Ngpc;
use pce_core::Pce;
use snes_core::Snes;
use virtualboy_core::Vb;
use wonderswan_core::WonderSwan;

/// System selector passed to [`emu_new`]. Keep in sync with the C header and the
/// Swift `System` enum.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum System {
    Gba = 0,
    Ps1 = 1,
    Nds = 2,
    Nes = 3,
    Sms = 4,
    GameGear = 5,
    Gbc = 6,
    Xbox = 7,
    Snes = 8,
    Genesis = 9,
    Pce = 10,
    Atari2600 = 11,
    Ngpc = 12,
    WonderSwan = 13,
    VirtualBoy = 14,
    N64 = 15,
}

impl System {
    fn from_u32(v: u32) -> Option<System> {
        Some(match v {
            0 => System::Gba,
            1 => System::Ps1,
            2 => System::Nds,
            3 => System::Nes,
            4 => System::Sms,
            5 => System::GameGear,
            6 => System::Gbc,
            7 => System::Xbox,
            8 => System::Snes,
            9 => System::Genesis,
            10 => System::Pce,
            11 => System::Atari2600,
            12 => System::Ngpc,
            13 => System::WonderSwan,
            14 => System::VirtualBoy,
            15 => System::N64,
            _ => return None,
        })
    }
}

/// The wrapped core. One boxed god-struct per system.
enum Inner {
    Gba(Box<Gba>),
    Ps1(Box<Psx>),
    Nds(Box<Nds>),
    Nes(Box<Nes>),
    Sms(Box<Sms>),
    Gbc(Box<Gbc>),
    Xbox(Box<Xbox>),
    Snes(Box<Snes>),
    Genesis(Box<Genesis>),
    Pce(Box<Pce>),
    Atari2600(Box<Atari>),
    Ngpc(Box<Ngpc>),
    WonderSwan(Box<WonderSwan>),
    VirtualBoy(Box<Vb>),
    N64(Box<N64>),
}

/// The opaque session handle.
pub struct Emu {
    inner: Inner,
    /// Latest presented framebuffer (RGBA8888), refreshed each `run_frame`.
    fb: Vec<u8>,
    width: u32,
    height: u32,
    sample_rate: u32,
    channels: u32,
    /// Frames run via this handle (uniform counter; not every core exposes one).
    frames: u32,
}

impl Emu {
    fn new(system: System) -> Emu {
        // (sample rate, channels) mirror the web players' audio settings.
        let (inner, w, h, rate, ch): (Inner, u32, u32, u32, u32) = match system {
            System::Gba => (Inner::Gba(Box::new(Gba::new())), 240, 160, 32768, 2),
            System::Ps1 => (Inner::Ps1(Box::new(Psx::new())), 640, 480, 44100, 2),
            System::Nds => (Inner::Nds(Box::new(Nds::new())), 256, 384, 44100, 2),
            System::Nes => (Inner::Nes(Box::new(Nes::new())), 256, 240, 44100, 1),
            System::Sms => (Inner::Sms(Box::new(Sms::new_system(false))), 256, 192, 44100, 1),
            System::GameGear => (Inner::Sms(Box::new(Sms::new_system(true))), 160, 144, 44100, 1),
            System::Gbc => (Inner::Gbc(Box::new(Gbc::new())), 160, 144, 48000, 2),
            System::Xbox => (Inner::Xbox(Box::new(Xbox::new())), 640, 480, 48000, 2),
            System::Snes => (Inner::Snes(Box::new(Snes::new())), 256, 224, 32000, 2),
            System::Genesis => (Inner::Genesis(Box::new(Genesis::new())), 320, 224, 44100, 2),
            System::Pce => (Inner::Pce(Box::new(Pce::new())), 256, 224, 44100, 2),
            System::Atari2600 => (Inner::Atari2600(Box::new(Atari::new())), 160, 192, 44100, 1),
            System::Ngpc => (Inner::Ngpc(Box::new(Ngpc::new())), 160, 152, 44100, 2),
            System::WonderSwan => {
                (Inner::WonderSwan(Box::new(WonderSwan::new_model(true))), 224, 144, 44100, 2)
            }
            System::VirtualBoy => (Inner::VirtualBoy(Box::new(Vb::new())), 384, 224, 44100, 2),
            System::N64 => (Inner::N64(Box::new(N64::new())), 320, 240, 44100, 2),
        };
        let mut e = Emu {
            inner,
            fb: Vec::new(),
            width: w,
            height: h,
            sample_rate: rate,
            channels: ch,
            frames: 0,
        };
        e.refresh();
        e
    }

    /// Load a ROM / disc image. Returns true on success.
    fn load_rom(&mut self, bytes: &[u8]) -> bool {
        match &mut self.inner {
            Inner::Gba(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Ps1(c) => {
                c.load_rom(bytes.to_vec());
                true
            }
            Inner::Nds(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Nes(c) => c.load_rom(bytes).is_ok(),
            Inner::Sms(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Gbc(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Xbox(c) => {
                c.load_rom(bytes.to_vec());
                true
            }
            Inner::Snes(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Genesis(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Pce(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Atari2600(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::Ngpc(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::WonderSwan(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::VirtualBoy(c) => {
                c.load_rom(bytes);
                true
            }
            Inner::N64(c) => {
                c.load_rom(bytes);
                true
            }
        }
    }

    /// Load a BIOS/flash image (PS1, Xbox). No-op for cores that don't need one.
    fn load_bios(&mut self, bytes: &[u8]) -> bool {
        match &mut self.inner {
            Inner::Ps1(c) => {
                c.load_bios(bytes);
                true
            }
            Inner::Xbox(c) => {
                c.load_bios(bytes);
                true
            }
            _ => false,
        }
    }

    fn run_frame(&mut self) {
        match &mut self.inner {
            Inner::Gba(c) => c.run_frame(),
            Inner::Ps1(c) => c.run_frame(),
            Inner::Nds(c) => c.run_frame(),
            Inner::Nes(c) => c.run_frame(),
            Inner::Sms(c) => c.run_frame(),
            Inner::Gbc(c) => c.run_frame(),
            Inner::Xbox(c) => c.run_frame(),
            Inner::Snes(c) => c.run_frame(),
            Inner::Genesis(c) => c.run_frame(),
            Inner::Pce(c) => c.run_frame(),
            Inner::Atari2600(c) => c.run_frame(),
            Inner::Ngpc(c) => c.run_frame(),
            Inner::WonderSwan(c) => c.run_frame(),
            Inner::VirtualBoy(c) => c.run_frame(),
            Inner::N64(c) => c.run_frame(),
        }
        self.frames = self.frames.wrapping_add(1);
        self.refresh();
    }

    /// Set the controller state from a uniform **active-high pressed** bitmask in
    /// each core's web-player bit layout. Cores that want a different convention
    /// are adapted here (e.g. the NDS keypad registers are active-low), so every
    /// front-end — Swift, libretro — uses the same "1 = pressed" contract.
    fn set_keys(&mut self, bits: u32) {
        match &mut self.inner {
            Inner::Gba(c) => c.set_keys(bits),
            Inner::Ps1(c) => c.set_keys(bits as u16),
            Inner::Nds(c) => {
                // KEYINPUT/EXTKEYIN are active-low: 0 = pressed. Low 10 bits are
                // the keypad (GBA order), bits 10/11 are X/Y in the ext register.
                let keyinput = 0x3FF & !bits;
                let ext = 0x3 & !(bits >> 10);
                c.set_keys(keyinput, ext);
            }
            Inner::Nes(c) => c.set_keys((bits & 0xFF) as u8),
            Inner::Sms(c) => c.set_keys(bits),
            Inner::Gbc(c) => c.set_keys((bits & 0xFF) as u8),
            Inner::Xbox(c) => c.set_keys(bits),
            Inner::Snes(c) => c.set_keys(bits),
            Inner::Genesis(c) => c.set_keys(bits),
            Inner::Pce(c) => c.set_keys(bits),
            Inner::Atari2600(c) => c.set_keys(bits),
            Inner::Ngpc(c) => c.set_keys(bits),
            Inner::WonderSwan(c) => c.set_keys(bits),
            Inner::VirtualBoy(c) => c.set_keys(bits),
            Inner::N64(c) => c.set_keys(bits),
        }
    }

    /// Drain audio into `out` (capacity `max` floats). Returns count written.
    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        let samples: Vec<f32> = match &mut self.inner {
            Inner::Gba(c) => c.drain_audio(),
            Inner::Ps1(c) => c.drain_audio(),
            Inner::Nds(c) => c.drain_audio(),
            Inner::Nes(c) => c.drain_audio(),
            Inner::Sms(c) => c.drain_audio(),
            Inner::Gbc(c) => c.drain_audio(),
            Inner::Xbox(c) => c.drain_audio(),
            Inner::Snes(c) => c.drain_audio(),
            Inner::Genesis(c) => c.drain_audio(),
            Inner::Pce(c) => c.drain_audio(),
            Inner::Atari2600(c) => c.drain_audio(),
            Inner::Ngpc(c) => c.drain_audio(),
            Inner::WonderSwan(c) => c.drain_audio(),
            Inner::VirtualBoy(c) => c.drain_audio(),
            Inner::N64(c) => c.drain_audio(),
        };
        let n = samples.len().min(out.len());
        out[..n].copy_from_slice(&samples[..n]);
        n
    }

    fn frame_count(&self) -> u32 {
        self.frames
    }

    /// Refresh the cached framebuffer + dimensions from the core.
    fn refresh(&mut self) {
        self.fb.clear();
        match &mut self.inner {
            Inner::Gba(c) => {
                self.width = 240;
                self.height = 160;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Ps1(c) => {
                self.width = c.width();
                self.height = c.height();
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Nds(c) => {
                // Stack the two 256x192 screens into one 256x384 image.
                self.width = 256;
                self.height = 384;
                self.fb.extend_from_slice(c.top_framebuffer());
                self.fb.extend_from_slice(c.bottom_framebuffer());
            }
            Inner::Nes(c) => {
                // NES picture is a fixed 256x240.
                self.width = 256;
                self.height = 240;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Sms(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Gbc(c) => {
                // Game Boy (Color) is a fixed 160x144 panel.
                self.width = 160;
                self.height = 144;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Xbox(c) => {
                self.width = c.width();
                self.height = c.height();
                self.fb.extend_from_slice(c.framebuffer());
            }
            // The new cores all expose width()/height()->usize + framebuffer()->&[u8].
            Inner::Snes(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Genesis(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Pce(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Atari2600(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::Ngpc(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::WonderSwan(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::VirtualBoy(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
            Inner::N64(c) => {
                self.width = c.width() as u32;
                self.height = c.height() as u32;
                self.fb.extend_from_slice(c.framebuffer());
            }
        }
    }

    /// Current persistent save data (battery SRAM/Flash/EEPROM), or empty for
    /// cores without battery-backed storage. The bytes are the core's native
    /// `.sav` image — directly interchangeable with other emulators. (`.to_vec`
    /// normalizes the cores that hand back a borrowed slice vs an owned Vec.)
    fn save_data(&self) -> Vec<u8> {
        match &self.inner {
            Inner::Gba(c) => c.save_ram().to_vec(),
            Inner::Gbc(c) => c.save_ram().to_vec(),
            Inner::Snes(c) => c.save_ram().to_vec(),
            Inner::Genesis(c) => c.save_ram().to_vec(),
            Inner::Sms(c) => c.save_ram().to_vec(),
            Inner::WonderSwan(c) => c.save_ram().to_vec(),
            _ => Vec::new(),
        }
    }

    /// Load a previously-saved `.sav` image into the core's battery store.
    /// No-op for cores without one.
    fn load_save(&mut self, bytes: &[u8]) {
        match &mut self.inner {
            Inner::Gba(c) => c.load_save_ram(bytes),
            Inner::Gbc(c) => c.load_save_ram(bytes),
            Inner::Snes(c) => c.load_save_ram(bytes),
            Inner::Genesis(c) => c.load_save_ram(bytes),
            Inner::Sms(c) => c.load_save_ram(bytes),
            Inner::WonderSwan(c) => c.load_save_ram(bytes),
            _ => {}
        }
    }

    /// Whether the save store changed since the last `clear_save_dirty` — the
    /// host polls this to decide when to flush a `.sav` to disk.
    fn save_dirty(&self) -> bool {
        match &self.inner {
            Inner::Gba(c) => c.save_dirty(),
            Inner::Gbc(c) => c.save_dirty(),
            Inner::Snes(c) => c.save_dirty(),
            Inner::Genesis(c) => c.save_dirty(),
            Inner::Sms(c) => c.save_dirty(),
            Inner::WonderSwan(c) => c.save_dirty(),
            _ => false,
        }
    }

    fn clear_save_dirty(&mut self) {
        match &mut self.inner {
            Inner::Gba(c) => c.clear_save_dirty(),
            Inner::Gbc(c) => c.clear_save_dirty(),
            Inner::Snes(c) => c.clear_save_dirty(),
            Inner::Genesis(c) => c.clear_save_dirty(),
            Inner::Sms(c) => c.clear_save_dirty(),
            Inner::WonderSwan(c) => c.clear_save_dirty(),
            _ => {}
        }
    }

    /// Attach (or detach) a link peripheral. Returns true if the core applied it.
    /// Today only the GBA models attachments: the Wireless Adapter swaps the SIO
    /// transport; Link Cable / None use the default serial loopback.
    fn set_attachment(&mut self, attachment: Attachment) {
        if let Inner::Gba(c) = &mut self.inner {
            c.sio_set_wireless_adapter(attachment == Attachment::WirelessAdapter);
        }
    }
}

/// Link peripherals a core can present on its serial port. The Swift front-end
/// shows the ones in `System::supported_attachments` and applies the choice via
/// `emu_set_attachment`.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    None = 0,
    LinkCable = 1,
    WirelessAdapter = 2,
}

impl Attachment {
    fn from_u32(v: u32) -> Attachment {
        match v {
            1 => Attachment::LinkCable,
            2 => Attachment::WirelessAdapter,
            _ => Attachment::None,
        }
    }
}

// Bitmask flags returned by `emu_supported_attachments` (1 << kind).
const ATTACH_LINK_CABLE: u32 = 1 << 1;
const ATTACH_WIRELESS_ADAPTER: u32 = 1 << 2;

/// On-disk save category for a system, so the front-end picks the right file
/// extension and management UI. Mirrors `EMU_SAVE_*` in emu_native.h.
const SAVE_KIND_NONE: u32 = 0;
const SAVE_KIND_BATTERY: u32 = 1; // .sav (SRAM/Flash/EEPROM)
const SAVE_KIND_MEMORY_CARD: u32 = 2; // .mcr (PS1)
const SAVE_KIND_HDD: u32 = 3; // raw HDD image (Xbox)

impl System {
    /// Bitmask of supported link attachments (`ATTACH_*`).
    fn supported_attachments(self) -> u32 {
        match self {
            // The GBA link port: a plain cable or the Wireless Adapter.
            System::Gba => ATTACH_LINK_CABLE | ATTACH_WIRELESS_ADAPTER,
            _ => 0,
        }
    }

    /// On-disk save category (`SAVE_KIND_*`).
    fn save_kind(self) -> u32 {
        match self {
            System::Gba
            | System::Gbc
            | System::Snes
            | System::Genesis
            | System::Sms
            | System::GameGear
            | System::WonderSwan => SAVE_KIND_BATTERY,
            System::Ps1 => SAVE_KIND_MEMORY_CARD,
            System::Xbox => SAVE_KIND_HDD,
            _ => SAVE_KIND_NONE,
        }
    }
}

// ============================ C ABI ============================

/// Allocate a core for `system` (see [`System`]). Returns null on an unknown id.
#[no_mangle]
pub extern "C" fn emu_new(system: u32) -> *mut Emu {
    match System::from_u32(system) {
        Some(s) => Box::into_raw(Box::new(Emu::new(s))),
        None => std::ptr::null_mut(),
    }
}

/// Free a handle from [`emu_new`].
///
/// # Safety
/// `emu` must be a pointer from [`emu_new`] (or null), used at most once.
#[no_mangle]
pub unsafe extern "C" fn emu_free(emu: *mut Emu) {
    if !emu.is_null() {
        drop(Box::from_raw(emu));
    }
}

/// Load a ROM / disc image. Returns true on success.
///
/// # Safety
/// `emu` valid; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn emu_load_rom(emu: *mut Emu, data: *const u8, len: usize) -> bool {
    if emu.is_null() || (data.is_null() && len != 0) {
        return false;
    }
    let bytes = std::slice::from_raw_parts(data, len);
    (*emu).load_rom(bytes)
}

/// Load a BIOS / flash image (PS1, Xbox). Returns true if the core used it.
///
/// # Safety
/// `emu` valid; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn emu_load_bios(emu: *mut Emu, data: *const u8, len: usize) -> bool {
    if emu.is_null() || (data.is_null() && len != 0) {
        return false;
    }
    let bytes = std::slice::from_raw_parts(data, len);
    (*emu).load_bios(bytes)
}

/// Run one frame and refresh the framebuffer.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_run_frame(emu: *mut Emu) {
    if !emu.is_null() {
        (*emu).run_frame();
    }
}

/// Set the controller button bitmask.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_set_keys(emu: *mut Emu, bits: u32) {
    if !emu.is_null() {
        (*emu).set_keys(bits);
    }
}

/// Pointer to the current RGBA8888 framebuffer (refreshed each `emu_run_frame`).
///
/// # Safety
/// `emu` valid; the pointer is valid until the next `emu_run_frame` / `emu_free`.
#[no_mangle]
pub unsafe extern "C" fn emu_framebuffer_ptr(emu: *const Emu) -> *const u8 {
    if emu.is_null() {
        return std::ptr::null();
    }
    (*emu).fb.as_ptr()
}

/// Length in bytes of the framebuffer (`width * height * 4`).
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_framebuffer_len(emu: *const Emu) -> usize {
    if emu.is_null() {
        return 0;
    }
    (*emu).fb.len()
}

/// Current display width in pixels.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_width(emu: *const Emu) -> u32 {
    if emu.is_null() {
        return 0;
    }
    (*emu).width
}

/// Current display height in pixels.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_height(emu: *const Emu) -> u32 {
    if emu.is_null() {
        return 0;
    }
    (*emu).height
}

/// Drain interleaved audio samples into `out` (capacity `max` floats). Returns
/// the number written.
///
/// # Safety
/// `emu` valid; `out` valid for `max` floats.
#[no_mangle]
pub unsafe extern "C" fn emu_drain_audio(emu: *mut Emu, out: *mut f32, max: usize) -> usize {
    if emu.is_null() || out.is_null() || max == 0 {
        return 0;
    }
    let slice = std::slice::from_raw_parts_mut(out, max);
    (*emu).drain_audio(slice)
}

/// Audio sample rate (Hz).
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_sample_rate(emu: *const Emu) -> u32 {
    if emu.is_null() {
        return 0;
    }
    (*emu).sample_rate
}

/// Audio channel count (1 = mono, 2 = interleaved stereo).
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_channels(emu: *const Emu) -> u32 {
    if emu.is_null() {
        return 0;
    }
    (*emu).channels
}

/// Frames completed since reset.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_frame_count(emu: *const Emu) -> u32 {
    if emu.is_null() {
        return 0;
    }
    (*emu).frame_count()
}

// ---- saves / memory cards / HDD ----

/// Byte length of the current save image (0 if the core has no battery store).
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_save_data_len(emu: *const Emu) -> usize {
    if emu.is_null() {
        return 0;
    }
    (*emu).save_data().len()
}

/// Copy the save image into `out` (capacity `max` bytes). Returns the number of
/// bytes written (`min(save_len, max)`). Pair with `emu_save_data_len`.
///
/// # Safety
/// `emu` valid; `out` valid for `max` bytes.
#[no_mangle]
pub unsafe extern "C" fn emu_save_data(emu: *const Emu, out: *mut u8, max: usize) -> usize {
    if emu.is_null() || out.is_null() || max == 0 {
        return 0;
    }
    let data = (*emu).save_data();
    let n = data.len().min(max);
    std::ptr::copy_nonoverlapping(data.as_ptr(), out, n);
    n
}

/// Load a `.sav` image into the core's battery store (call right after
/// `emu_load_rom`).
///
/// # Safety
/// `emu` valid; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn emu_load_save(emu: *mut Emu, data: *const u8, len: usize) {
    if emu.is_null() || (data.is_null() && len != 0) {
        return;
    }
    let bytes = std::slice::from_raw_parts(data, len);
    (*emu).load_save(bytes);
}

/// Whether the save store changed since the last `emu_clear_save_dirty`.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_save_dirty(emu: *const Emu) -> bool {
    !emu.is_null() && (*emu).save_dirty()
}

/// Clear the save-dirty flag (call after persisting the `.sav`).
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_clear_save_dirty(emu: *mut Emu) {
    if !emu.is_null() {
        (*emu).clear_save_dirty();
    }
}

/// On-disk save category for `system` (`EMU_SAVE_*`): 0 none, 1 battery (.sav),
/// 2 memory card (.mcr), 3 HDD image. Lets the front-end pick the extension and
/// management UI without a live handle.
#[no_mangle]
pub extern "C" fn emu_save_kind(system: u32) -> u32 {
    System::from_u32(system).map_or(SAVE_KIND_NONE, System::save_kind)
}

// ---- link attachments ----

/// Bitmask of link attachments `system` supports (`EMU_ATTACH_*`: bit 1 = link
/// cable, bit 2 = wireless adapter). 0 if the system has no link port modeled.
#[no_mangle]
pub extern "C" fn emu_supported_attachments(system: u32) -> u32 {
    System::from_u32(system).map_or(0, System::supported_attachments)
}

/// Select the active link attachment (0 none, 1 link cable, 2 wireless adapter).
/// No-op for cores that don't model the chosen attachment.
///
/// # Safety
/// `emu` valid.
#[no_mangle]
pub unsafe extern "C" fn emu_set_attachment(emu: *mut Emu, attachment: u32) {
    if !emu.is_null() {
        (*emu).set_attachment(Attachment::from_u32(attachment));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_each_system_has_framebuffer() {
        for id in 0..=7u32 {
            let e = unsafe { &mut *emu_new(id) };
            assert_eq!(
                e.fb.len(),
                (e.width * e.height * 4) as usize,
                "system {id} framebuffer sized to dims"
            );
            assert!(e.sample_rate >= 32768);
        }
    }

    #[test]
    fn unknown_system_is_null() {
        assert!(emu_new(99).is_null());
    }

    #[test]
    fn xbox_mounts_disc_via_ffi() {
        // Reuse the xbox core's synthetic-disc shape through the FFI path.
        let e = emu_new(System::Xbox as u32);
        // A non-disc rom is fine (no mount); just ensure run_frame is safe.
        unsafe {
            emu_run_frame(e);
            assert!(emu_frame_count(e) >= 1);
            assert!(emu_framebuffer_len(e) > 0);
            emu_free(e);
        }
    }

    #[test]
    fn save_kinds_and_attachments_by_system() {
        // Battery cores report .sav; PS1 a memory card; Xbox an HDD; the rest none.
        assert_eq!(emu_save_kind(System::Gba as u32), SAVE_KIND_BATTERY);
        assert_eq!(emu_save_kind(System::Snes as u32), SAVE_KIND_BATTERY);
        assert_eq!(emu_save_kind(System::Ps1 as u32), SAVE_KIND_MEMORY_CARD);
        assert_eq!(emu_save_kind(System::Xbox as u32), SAVE_KIND_HDD);
        assert_eq!(emu_save_kind(System::Nes as u32), SAVE_KIND_NONE);
        assert_eq!(emu_save_kind(999), SAVE_KIND_NONE);
        // Only the GBA models link attachments today.
        assert_eq!(
            emu_supported_attachments(System::Gba as u32),
            ATTACH_LINK_CABLE | ATTACH_WIRELESS_ADAPTER
        );
        assert_eq!(emu_supported_attachments(System::Ps1 as u32), 0);
    }

    #[test]
    fn gba_save_roundtrips_through_ffi() {
        let e = emu_new(System::Gba as u32);
        unsafe {
            emu_load_rom(e, [0u8; 0x100].as_ptr(), 0x100);
            // GBA defaults to a 128 KB save image.
            let len = emu_save_data_len(e);
            assert!(len > 0, "battery-backed core exposes a save image");
            // Load a save pattern, read it back through the copy-out path.
            let pattern = vec![0xABu8; len];
            emu_load_save(e, pattern.as_ptr(), pattern.len());
            let mut out = vec![0u8; len];
            let n = emu_save_data(e, out.as_mut_ptr(), out.len());
            assert_eq!(n, len);
            assert_eq!(out, pattern);
            emu_free(e);
        }
    }

    #[test]
    fn set_attachment_is_safe_for_all_systems() {
        // Applying any attachment to any system must never crash, even where the
        // core ignores it.
        for id in 0..=15u32 {
            let e = emu_new(id);
            unsafe {
                emu_set_attachment(e, 2); // wireless adapter
                emu_set_attachment(e, 1); // link cable
                emu_set_attachment(e, 0); // none
                emu_run_frame(e);
                emu_free(e);
            }
        }
    }
}
