//! Pure-Rust Sega Master System + Game Gear core, built from-scratch against
//! the SMS Power! hardware spec, the Zilog Z80 user manual, and the
//! TMS9918/SMS VDP documentation.
//!
//! ONE core handles BOTH systems. The Game Gear is a portable Master System:
//! identical Z80 CPU, VDP, and SN76489 PSG. It differs only in (a) the visible
//! screen crop (160×144 centred in the SMS 256×192 frame), (b) the palette
//! format (GG CRAM is 12-bit, 2 bytes per entry; SMS is 6-bit, 1 byte), and
//! (c) a handful of GG-only I/O ports (stereo, start button). A [`System`]
//! enum chosen at construction selects all three behaviours.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Sms`]
//! god-struct owns every subsystem (CPU / VDP / PSG / cartridge / input) and
//! implements the Z80 [`bus::Z80Bus`]. Cross-subsystem calls pass `&mut`
//! references as parameters (resolved with `mem::take` at the call site) — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; little-endian; boxed
//! regions; fixed-width integers.

pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod io;
pub mod psg;
pub mod sms;
pub mod vdp;

pub use sms::{Sms, System};

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
