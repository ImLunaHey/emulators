//! Pure-Rust Game Boy Color (CGB) core, written from scratch against the
//! hardware spec (Pan Docs, gbdev.io/pandocs). The fourth core in this repo.
//!
//! Ownership model (mirrors the GBA/NDS/PS1 cores): one [`Gbc`] god-struct owns
//! every subsystem + memory + cart and implements [`bus::Bus`]; each subsystem
//! is a struct owning only its own state, and cross-subsystem calls pass `&mut`
//! references as parameters. No Rc/RefCell. Regions are boxed; selectors are
//! closed enums matched exhaustively; values are fixed-width ints; multi-byte
//! memory is little-endian.
//!
//! This phase lands the foundation: the LR35902 register/interrupt state, the
//! memory map, the cartridge MBC routing, and the bus. Instruction execution
//! and the IO sub-devices (PPU/APU/timer/DMA/joypad/serial) are stubbed.

// --- Foundation (hand-written against the spec; the contract everything else
//     codes against).
pub mod bus;
pub mod interrupts;
pub mod memory;
pub mod regions;

// --- Cartridge + memory-bank controllers.
pub mod cart;

// --- CPU (register state + interrupt dispatch now; exec later).
pub mod cpu;

// --- IO subsystems (empty stubs so parallel agents own one file each).
pub mod apu;
pub mod dma;
pub mod joypad;
pub mod ppu;
pub mod serial;
pub mod timer;

// --- Crash screen (fault readout drawn into the framebuffer).
pub mod crash;

// --- Top-level orchestrator.
pub mod emulator;

// --- wasm-bindgen surface (web target only).
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use emulator::Gbc;
