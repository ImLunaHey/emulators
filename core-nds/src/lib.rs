//! Pure-Rust Nintendo DS core — the second emulator core in this workspace,
//! a sibling of the GBA core in `../../core`. Ported from the TypeScript core
//! in `../../ds-recomp/src`.
//!
//! This crate currently holds the **memory + CPU-state foundation**: shared
//! RAM, the ARM9 + ARM7 buses, the VRAM bank router, both CPU register files,
//! and the ARM9 CP15. CPU instruction execution and every IO/PPU/cart/BIOS
//! subsystem are pre-declared as empty modules (one file per future porting
//! agent) and land later.
//!
//! Ownership model (see CONTRACT.md): one `Nds` god-struct owns every
//! subsystem + `SharedMemory` and exposes per-CPU bus accessors; each former
//! TS class becomes a struct owning only its own state, and cross-subsystem
//! calls pass `&mut` references as parameters (mirroring the TS constructor
//! wiring — same pattern the GBA core uses).

// --- Foundation (hand-ported; the contract every other module codes against).
pub mod cpu;
pub mod memory;
pub mod nds;

// --- Crash screen (rendered into the top framebuffer on a detected fault loop).
pub mod crash;

// --- Subsystems (ported one-file-per-agent against the foundation). The 3D
// GPU (`gx`) is deferred and intentionally absent.
pub mod bios;
pub mod cart;
pub mod io;
pub mod ppu;

// --- wasm-bindgen surface for the web host (`WasmNds`). Gated to wasm32 so the
// macro never runs during host `cargo test`, mirroring the GBA core's `wasm`.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

// Re-exports so callers can `use nds_core::{Nds, ...}` flatly, like the GBA
// core's `pub use emulator::Gba`.
pub use cpu::state;
pub use nds::{Core, Nds};
