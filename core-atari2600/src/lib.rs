//! Pure-Rust Atari 2600 (VCS) core, built from-scratch against the Stella
//! Programmer's Guide, the AtariAge TIA hardware docs, and the 6502.org
//! instruction reference.
//!
//! The 2600 is the most timing-critical machine in this monorepo. There is no
//! frame buffer in hardware: the TIA (Television Interface Adaptor) generates
//! the video signal one colour clock at a time, and the CPU runs in lockstep
//! with it (1 CPU cycle = 3 TIA colour clocks). Games "race the beam" —
//! rewriting TIA registers between (and within) scanlines so that the same few
//! object registers paint a whole screen. Correct beam timing *is* the
//! renderer.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Atari`]
//! god-struct owns every subsystem (CPU / TIA / RIOT / cartridge / input) and
//! implements the CPU [`bus::Bus`]. Cross-subsystem calls pass `&mut`
//! references as parameters (resolved with `mem::take` at the call site) — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; little-endian; boxed
//! regions; fixed-width integers.
//!
//! ## Implemented
//! - **6507 CPU** ([`cpu`]): full documented 6502 instruction set + the common
//!   unofficial opcodes, correct flags and cycle counts (page-cross / branch
//!   penalties), NMI/IRQ/RESET, JAM detection. 13-bit address bus (the 2600's
//!   address space is mirrored through the 6507's reduced pin count).
//! - **TIA** ([`tia`]): the 2 players, 2 missiles, 1 ball, the 20-bit
//!   playfield (PF0/PF1/PF2, reflect + score modes), object positioning
//!   (RESPx + HMOVE fine motion), the NTSC 128-colour palette → RGBA, the
//!   priority / collision (CXxxxx) latches, VSYNC / VBLANK, WSYNC (CPU stalls
//!   to end of scanline), and the 2-channel AUDxx audio.
//! - **RIOT (6532)** ([`riot`]): 128 bytes of RAM, the interval timer
//!   (TIM1T/TIM8T/TIM64T/T1024T + INTIM), and the SWCHA/SWCHB I/O ports.
//! - **Cartridge** ([`cart`]): 2K / 4K plain ROM and the standard F8 (8K),
//!   F6 (16K), F4 (32K) bank-switching schemes, detected by ROM size.
//! - **Input** ([`Atari::set_keys`]): joystick + fire via SWCHA/INPT4, and the
//!   console reset/select switches via SWCHB.
//!
//! ## Stubbed / next steps
//! - HMOVE "comb effect" (the 8 blanked pixels at the left edge) is modelled,
//!   but the late-HMOVE quirks of writing HMOVE far into a line are simplified.
//! - Audio is a faithful polynomial-counter implementation but is not bit-exact
//!   against real silicon for every AUDC mode.
//! - Paddle / driving controllers, and the less common bank schemes
//!   (E0/FE/3F/SuperChip RAM) are not implemented.

pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod riot;
pub mod tia;

mod atari;
pub use atari::Atari;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
