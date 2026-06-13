//! Pure-Rust GBA core, ported 1:1 from the TypeScript core in ../../src.
//!
//! Ownership model (see CONTRACT.md): one `Gba` god-struct owns every
//! subsystem and implements [`bus::Bus`]; each former TS class becomes a
//! struct owning only its own state, and cross-subsystem calls pass `&mut`
//! references as parameters (mirroring the TS constructor wiring).

// --- Foundation (hand-ported; the contract every other module codes against).
pub mod bus;
pub mod irq;
pub mod regions;
pub mod state;

// --- Subsystems (ported one-file-per-agent against the foundation).
pub mod arm;
pub mod bios;
pub mod cheats;
pub mod cpu;
pub mod dma;
pub mod eeprom;
pub mod flash;
pub mod keypad;
pub mod ppu;
pub mod rtc;
pub mod save_detect;
pub mod shifter;
pub mod sio;
pub mod sound;
pub mod sram;
pub mod thumb;
pub mod timers;
pub mod emulator;
pub mod savestate;
pub mod debug;

pub use emulator::Gba;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests
// don't pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// Battery-backed save backend (Flash / SRAM / EEPROM). Ported from the
/// `SaveBridge` interface in src/memory/bus.ts plus the shared shape of the
/// concrete backends (`data`, `load_save`). `read` takes `&mut self` because
/// EEPROM advances a bit-serial cursor on read.
pub trait Save {
    fn read(&mut self, addr: u32) -> u32;
    fn write(&mut self, addr: u32, v: u32);
    fn data(&self) -> &[u8];
    fn load_save(&mut self, bytes: &[u8]);
}
