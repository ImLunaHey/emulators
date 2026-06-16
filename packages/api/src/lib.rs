//! `core-api` — the contract every emulator core and the launcher implement.
//!
//! # Design (decided in the multi-emulator monorepo discussion)
//!
//! * **The host is a thin set of I/O adapters; Rust owns all logic.** The host
//!   provides input, a video sink, an audio sink, storage, and networking.
//!   Everything stateful — emulation, the library model, save lifecycle, and
//!   the launcher's UI logic — lives in Rust.
//!
//! * **The launcher is just another [`FrameSource`].** The home menu produces
//!   frames through the same pipe as a console core, so the host's present
//!   loop is generic: launcher ↔ game ↔ game is one operation (swap the active
//!   source). The host never special-cases the menu.
//!
//! * **Screen geometry is part of the contract, not a global constant.** A
//!   source reports a [`VideoLayout`] (how many screens, sizes, which is
//!   touch). The host renders `layout().screens.len()` surfaces and reconciles
//!   on every swap — so NDS (2 screens) → PS1 (1 screen) just drops a surface,
//!   with no imperative teardown.
//!
//! * **Closed enums over traits wherever the set is fixed.** [`System`],
//!   [`ScreenSize`], and [`Input`] are closed: adding a console / screen mode /
//!   button is a compile-time checklist the `match` arms enforce. Traits
//!   ([`FrameSource`], [`EmulatorCore`]) are reserved for the genuine open set
//!   — independently-developed cores, dispatched once per frame.

#![forbid(unsafe_code)]

// ===========================================================================
// Input
// ===========================================================================

/// Canonical controller state handed to every [`FrameSource`] each frame.
///
/// One closed struct, a superset of all systems' inputs: a GBA core ignores
/// the sticks and touch; a PS1 DualShock core reads the sticks; the DS and the
/// launcher read `touch`. Closed by design — adding a field prompts every
/// consumer at compile time, which a per-system input trait would not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// Digital buttons, OR-ed together from [`button`].
    pub buttons: u32,
    /// Left analog stick; each axis `-127..=127`, `0` = centered.
    pub left_stick: (i8, i8),
    /// Right analog stick, same convention.
    pub right_stick: (i8, i8),
    /// Touchscreen / launcher pointer sample, if the surface is being touched.
    pub touch: Option<TouchPoint>,
}

/// A touch/pointer sample, in the target screen's own pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    /// Index into [`VideoLayout::screens`] that was touched (DS bottom screen,
    /// the launcher's single screen, …).
    pub screen: u8,
}

/// Canonical button bit positions. Superset across systems; each core maps
/// only the bits it actually has.
pub mod button {
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const X: u32 = 1 << 2;
    pub const Y: u32 = 1 << 3;
    pub const SELECT: u32 = 1 << 4;
    pub const START: u32 = 1 << 5;
    pub const UP: u32 = 1 << 6;
    pub const DOWN: u32 = 1 << 7;
    pub const LEFT: u32 = 1 << 8;
    pub const RIGHT: u32 = 1 << 9;
    pub const L1: u32 = 1 << 10;
    pub const R1: u32 = 1 << 11;
    pub const L2: u32 = 1 << 12;
    pub const R2: u32 = 1 << 13;
    pub const L3: u32 = 1 << 14;
    pub const R3: u32 = 1 << 15;
}

/// Which inputs a given console actually exposes — the *subset* of the
/// canonical [`Input`] superset it reads. The launcher uses this to (a) render
/// only the buttons that console has on the on-screen pad / remap UI, and (b)
/// know whether to show sticks / a touch surface.
///
/// Key consequence: the host maps the physical gamepad to the canonical
/// [`button`] set **once**. Every console then reads only the bits in its
/// `buttons` mask, so adding a console with fewer buttons needs no remapping;
/// adding one that needs a *new* button means adding a bit to the closed
/// [`button`] set (a compile-time checklist) and including it here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerProfile {
    /// OR-ed [`button`] bits this console has (GBA omits X/Y/L2/R2/L3/R3; a
    /// DualShock core sets them all).
    pub buttons: u32,
    /// Number of analog sticks: 0 (GBA/SNES), 1, or 2 (DualShock).
    pub analog_sticks: u8,
    /// Whether this console has a touch surface (DS bottom screen).
    pub touch: bool,
}

impl ControllerProfile {
    /// True if `bit` (a single [`button`] constant) is present on this console.
    pub fn has(&self, bit: u32) -> bool {
        self.buttons & bit != 0
    }
}

// ===========================================================================
// Video
// ===========================================================================

/// Pixel format every [`FrameSource`] writes. Cores convert their native
/// format to this once per frame; the host blits it verbatim.
pub const PIXEL_FORMAT: &str = "RGBA8888";

/// How a screen's resolution is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenSize {
    /// Hardware-native, fixed resolution — the host integer-scales it and
    /// preserves aspect. GBA `240x160`, PS1 `320x240`, DS `256x192` ×2.
    Fixed { w: u32, h: u32 },
    /// Render-to-fit — the source draws at whatever size the host requests via
    /// [`FrameSource::resize`]. Used by the launcher and any non-hardware UI so
    /// text and cover art stay crisp instead of being scaled from a retro buffer.
    Resizable,
}

/// One logical screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub size: ScreenSize,
    /// Whether this screen accepts touch/pointer input (DS bottom, launcher).
    pub touch: bool,
}

/// How multiple screens relate *logically*. The host turns this into a
/// *physical* arrangement using device orientation + user prefs (portrait
/// stacked vs landscape side-by-side, integer scale, which screen is larger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutHint {
    Single,
    StackedVertical,
    SideBySide,
}

/// The set of screens a source is currently presenting.
///
/// May change between frames (PS1 mid-game resolution switch, DS rotation), so
/// the host re-reads it and reconciles its surfaces. The on-screen canvas count
/// is exactly `screens.len()`; nothing is held fixed across a source swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoLayout {
    pub screens: Vec<Screen>,
    pub hint: LayoutHint,
}

// ===========================================================================
// The core contract
// ===========================================================================

/// Anything that produces frames the host can present: every console core AND
/// the launcher. The host drives a single `dyn FrameSource` and stays generic
/// over geometry — switching launcher ↔ game ↔ game is just swapping the source.
pub trait FrameSource {
    /// Current screen layout. Cheap to call; re-read every frame.
    fn layout(&self) -> VideoLayout;

    /// Advance exactly one frame given the latest input.
    fn run_frame(&mut self, input: &Input);

    /// RGBA8888 pixels for screen `i` in `0..layout().screens.len()`.
    fn pixels(&self, screen: usize) -> &[u8];

    /// Tell a [`ScreenSize::Resizable`] source the pixel size to render at next
    /// frame. No-op for fixed-resolution hardware cores.
    fn resize(&mut self, _screen: usize, _w: u32, _h: u32) {}

    /// Audio produced this frame, interleaved stereo `f32` in `-1.0..=1.0`.
    /// The host copies it into the platform audio sink before the next frame.
    fn drain_audio(&mut self) -> &[f32];

    /// Output sample rate of [`FrameSource::drain_audio`].
    fn sample_rate(&self) -> u32;
}

/// A playable console core: a [`FrameSource`] plus ROM / save / state lifecycle.
///
/// Per-system extras (DS slot-2 cartridge, link cable) are intentionally *not*
/// here — they live on the concrete core type, since they aren't universal.
pub trait EmulatorCore: FrameSource {
    /// Which system this core emulates.
    fn system(&self) -> System;

    /// The console's input surface — the subset of [`Input`] it reads. Drives
    /// the on-screen pad and the remap UI (see [`ControllerProfile`]).
    fn controller(&self) -> ControllerProfile;

    /// Load a ROM image and reset to power-on state.
    fn load_rom(&mut self, bytes: &[u8]);
    fn reset(&mut self);

    // ---- battery save (Flash / SRAM / EEPROM / memory card / …) ----
    fn save_ram(&self) -> Vec<u8>;
    fn load_save_ram(&mut self, bytes: &[u8]);
    /// True if the save changed since the last [`EmulatorCore::clear_save_dirty`].
    fn save_dirty(&self) -> bool;
    fn clear_save_dirty(&mut self);

    // ---- savestates ----
    fn save_state(&self) -> Vec<u8>;
    /// Returns `false` if the blob is incompatible (wrong system or version).
    fn load_state(&mut self, blob: &[u8]) -> bool;

    // ---- cheats ----
    fn set_cheats(&mut self, codes_newline_joined: &str);
}

// ===========================================================================
// System identity
// ===========================================================================

/// The set of systems the monorepo can run. Closed enum on purpose: the
/// launcher's content-type dispatch and [`detect_system`] both `match` on it,
/// so adding a console is a compile-time checklist, not a silent gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Gba,
    Nds,
    Ps1,
    // GameCube, N64, GbGbc, Nes, …
}

/// Best-effort system identification from an image's bytes (header magic, with
/// the call site falling back to file extension). Returns `None` if
/// unrecognized. The real signature tables live in the `library` crate; this
/// is just the seam so the contract is self-contained.
pub fn detect_system(_bytes: &[u8]) -> Option<System> {
    None
}
