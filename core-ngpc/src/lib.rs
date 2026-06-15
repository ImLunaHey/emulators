//! Pure-Rust Neo Geo Pocket / Neo Geo Pocket Color core, built from-scratch
//! against public hardware documentation: the NeoGeo Pocket dev wiki / ngpcspec,
//! the Toshiba TLCS-900/H1 User's Manual (main CPU), the Zilog Z80 manual (sound
//! CPU), and the K1GE/K2GE video + T6W28 PSG notes.
//!
//! ONE core handles both the mono NGP and the colour NGPC: the colour video
//! chip (K2GE) is a superset of the mono one (K1GE); the cart header byte at
//! offset 0x23 (0x10 = colour) selects the mode at load time.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Ngpc`]
//! god-struct owns every subsystem (TLCS-900/H CPU, Z80 sound CPU, video, PSG,
//! cartridge, input) and implements the CPU [`cpu::Bus`]. Cross-subsystem calls
//! pass `&mut` references (resolved with `mem::take` at the call site) — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; little-endian; boxed
//! regions; fixed-width integers.
//!
//! IMPLEMENTATION STATUS (see module docs for detail):
//!   * TLCS-900/H CPU — register banks, flags, the variable-length encoding,
//!     the core arithmetic/logic/shift/branch instruction set with unit tests.
//!     Block-transfer and a handful of exotic forms are stubbed (set `illegal`).
//!   * K1GE/K2GE video — 160×152, two scroll planes + 64 sprites, 12-bit
//!     palette → RGBA, H/V-blank interrupt latches. WORKING + tested.
//!   * T6W28 PSG + DAC — SN76489-style dual PSG with L/R attenuation. WORKING.
//!   * Z80 sound CPU — full interpreter present (ported from the SMS core), not
//!     yet clocked in the frame loop (audio comes from the PSG ports directly).
//!   * Input — D-pad + A/B + Option at system register 0x6F82. WORKING.
//!   * BIOS — HLE'd (no boot ROM): `load_rom` sets SP, clears the interrupt
//!     mask, and jumps to the cart header entry. NEXT STEP: a real/HLE BIOS for
//!     the SWI services and power-on handshake commercial games expect.

pub mod cart;
pub mod cpu;
pub mod crash;
pub mod input;
pub mod ngpc;
pub mod psg;
pub mod video;
pub mod z80;
pub mod z80bus;

pub use ngpc::Ngpc;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
