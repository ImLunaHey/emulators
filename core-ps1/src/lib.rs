//! Pure-Rust PlayStation 1 (PSX) core, built from scratch against nocash's
//! psx-spx hardware spec. Third emulator core in this repo, sibling of the GBA
//! (`../core`) and NDS cores.
//!
//! Ownership model (see CONTRACT.md): one [`Psx`] god-struct owns the MIPS
//! R3000A CPU + memory + every subsystem and implements [`bus::Bus`]; each
//! subsystem is a struct owning only its own state, and cross-subsystem calls
//! pass `&mut` references as parameters (mirroring the GBA/NDS cores).
//!
//! This phase lands the CPU register/exception foundation, the memory map, and
//! the bus only. Instruction execution and the sub-device behaviors are
//! `todo!()` seams that later parallel agents fill in — each owns exactly one
//! module file below.

// --- Foundation (the contract every other module codes against).
pub mod bus;
pub mod cpu;
pub mod memory;
pub mod regions;

// --- Top-level orchestrator (the god-struct + Bus impl).
pub mod psx;

// --- Web target: the wasm-bindgen surface (`WasmPsx`). wasm32-only so host
// `cargo test` never invokes the macro, mirroring the GBA core.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

// --- Subsystem seams (one file per future agent; empty TODO stubs for now).
pub mod bios;
pub mod cdrom;
pub mod dma;
pub mod gpu;
pub mod gte;
pub mod irq;
pub mod mdec;
pub mod sio;
pub mod spu;
pub mod timers;

pub use cpu::{Cop0, Cpu, Exception};
pub use memory::Mem;
pub use psx::Psx;
