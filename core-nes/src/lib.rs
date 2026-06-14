//! Pure-Rust NES (Nintendo Entertainment System / Famicom) core, built
//! from-scratch against the NESdev wiki hardware spec.
//!
//! Ownership model (see the sibling cores' CONTRACT.md): one [`Nes`]
//! god-struct owns every subsystem (CPU / PPU / APU / cartridge / controllers)
//! and implements both the CPU [`bus::Bus`] and the PPU [`ppu::PpuBus`].
//! Cross-subsystem calls pass `&mut` references as parameters (resolved with
//! `mem::take` at the call site) — no `Rc`/`RefCell`. Closed enums + exhaustive
//! `match`; little-endian; fixed-width integers.

pub mod apu;
pub mod bus;
pub mod cart;
pub mod cpu;
pub mod input;
pub mod mapper;
pub mod nes;
pub mod ppu;

pub use nes::Nes;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
