//! Pure-Rust Bandai WonderSwan / WonderSwan Color core, built from-scratch
//! against the WonderSwan dev wiki ("WSMan" / Cheri's WS docs) and the NEC
//! V30MZ (80186-compatible x86) instruction reference. There is no source to
//! port.
//!
//! ONE core handles BOTH systems. The WonderSwan Color is a superset of the
//! mono WonderSwan: identical V30MZ CPU, tilemap + sprite video, and 4-channel
//! audio. The Color model adds a 12-bit RGB palette (4096 colours), more VRAM
//! (64 KiB vs 16 KiB), and a "packed" 4bpp tile format. A [`Model`] enum chosen
//! at construction selects the colour behaviour.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`WonderSwan`]
//! god-struct owns every subsystem (CPU / video / audio / cartridge / input)
//! and implements the V30MZ [`bus::V30Bus`]. Cross-subsystem calls pass `&mut`
//! references as parameters (resolved with `mem::take` at the call site) — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; little-endian; boxed
//! regions; fixed-width integers.
//!
//! Implemented:
//!   * V30MZ CPU: full 8086 base opcode map + 80186 additions, ModR/M decoding,
//!     segmentation, prefixes (segment override / REP / LOCK), string ops,
//!     interrupts/IRET, the flags. (`cpu.rs`, unit-tested.)
//!   * Memory map: internal RAM (16 KiB mono / 64 KiB Color), ROM bank windows
//!     selected by the bank registers, I/O port space $00-$FF. (`ws.rs`)
//!   * Cartridge footer parsing (header at the END of the ROM). (`cart.rs`)
//!   * Video: tilemap (2 scroll layers) + sprite renderer, display control
//!     registers, line/vblank timing + interrupts. (`video.rs`)
//!   * Audio: 4-channel sound (tone / voice / sweep / noise) producing f32
//!     samples. (`audio.rs`)
//!   * Input: directional X/Y pads + A/B + Start mapped onto the key matrix.
//!
//! Stubbed / partial (see module docs + the FINAL message): a few rare 80186
//! corner-case flag edges; the EEPROM/RTC; sound DMA and the hyper-voice; some
//! exotic mapper features. The CPU + video path is complete enough to boot a
//! commercial ROM and reach a title screen.

pub mod audio;
pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod video;
pub mod ws;

pub use ws::{Model, WonderSwan};

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
