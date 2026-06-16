//! Pure-Rust NEC PC Engine / TurboGrafx-16 core, built from-scratch against the
//! public hardware documentation (Charles MacDonald's PC Engine notes, the
//! Archaic Pixels / pcedev wiki, the HuC6280 datasheet).
//!
//! Hardware:
//!   - CPU: Hudson HuC6280 @ 7.16 MHz — a 65C02 core PLUS a banking MMU (8
//!     mapping registers, TAM/TMA), block-transfer ops (TII/TDD/TIA/TAI/TIN),
//!     ST0/ST1/ST2 VDC-write instructions, CSL/CSH speed switch, the SET flag,
//!     and a built-in timer + I/O port + 6-channel wavetable PSG.
//!   - Video: HuC6270 VDC (background tilemap + sprites + VRAM) feeding the
//!     HuC6260 VCE (9-bit GRB palette, 512 colors -> RGBA8888).
//!   - Audio: the 6-channel wavetable PSG built into the HuC6280.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Pce`]
//! god-struct owns every subsystem (CPU / VDC / VCE / PSG / cartridge / input)
//! and implements the CPU [`bus::Bus`]. Cross-subsystem calls pass `&mut`
//! references as parameters (resolved with `mem::take` at the call site) — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; little-endian; boxed
//! regions; fixed-width integers.
//!
//! IMPLEMENTED:
//!   - Full HuC6280 instruction set: every 65C02 opcode + addressing mode, the
//!     banking MMU (TAM/TMA + 8 MPR registers), block transfers (TII/TDD/TIA/
//!     TAI/TIN), ST0/ST1/ST2, CSL/CSH, SET, TST, TSB/TRB, BBR/BBS/RMB/SMB,
//!     zero-page-indirect, BRA, decimal mode, the I/O port, and the timer.
//!   - HuC6270 VDC: VRAM, the address/data register pair (AR + the MAWR/MARR/
//!     VWR/VRR auto-increment ports), background scrolling tilemap render,
//!     sprites (SATB), VBlank + raster (RCR) interrupts.
//!   - HuC6260 VCE: 9-bit GRB palette RAM (512 entries) -> RGBA8888.
//!   - HuCard ROM loading with size detection + the 384 KB bank-swap quirk.
//!   - Standard pad input via the 2-bit SEL/CLR joypad protocol.
//!   - 6-channel wavetable PSG: register interface + best-effort synthesis.
//!
//! STUBBED / BEST-EFFORT (next steps):
//!   - PSG LFO and exact noise spectrum are approximate.
//!   - VDC sprite-overflow / collision flags are set best-effort.
//!   - Cycle timing is per-instruction approximate (no sub-instruction
//!     dot-accurate VDC), good enough to boot and render a title screen.
//!   - Variable display width (256/336/512) is exposed but rendering targets the
//!     standard 256-wide path.

pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod input;
pub mod pce;
pub mod psg;
pub mod vce;
pub mod vdc;

pub use pce::Pce;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
