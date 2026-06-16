//! Pure-Rust Nintendo GameCube (GCN) core, built from scratch against public
//! hardware references — primarily YAGCD ("Yet Another GameCube Documentation").
//! Sixth emulator core in this repo, sibling of the GBA (`../core`), NDS, PS1,
//! GBC, NES and SMS cores.
//!
//! # Hardware (YAGCD ch. 5 "Hardware Registers", ch. 2 "Memory Map")
//!
//! * **CPU — IBM PowerPC 750CXe "Gekko" @ 486 MHz.** A superscalar 32-bit
//!   PowerPC with custom extensions (paired-single SIMD over the FPU, a small
//!   locked data-cache "L2C" used as scratchpad, write-gather buffer). PowerPC
//!   is **BIG-ENDIAN** — every memory accessor and the instruction fetch in this
//!   crate is rigorous about that (see [`mem`] and [`cpu`]).
//! * **Main RAM — 24 MB 1T-SRAM ("Splash")** at physical `0x0000_0000`, mapped
//!   into the cached BAT window at `0x8000_0000` and the uncached window at
//!   `0xC000_0000` (the two are mirrors of the same DRAM). YAGCD §2.
//! * **GPU — ATI/ArtX "Flipper" @ 162 MHz**, with 2 MB embedded 1T-SRAM
//!   framebuffer/texture cache. The Command Processor (CP) consumes a FIFO of
//!   display lists; the Transform/Texture/Pixel engines (XF/TEV) do fixed +
//!   register-combiner shading. Modelled here only as a framebuffer stub.
//! * **DSP — Macronix 16-bit "DSP"** for audio mixing (AI streams to the codec).
//! * **DI/DVD, EXI (memory cards / serial), SI (controllers), AI (audio
//!   interface), PI/MI (processor/memory interfaces & interrupts), the IPL boot
//!   ROM** — all hardware-register windows at `0xCC00_0000`. Entirely absent
//!   here; this phase lands only the CPU/mem/bus/framebuffer foundation.
//!
//! # Ownership model (mirrors the PS1 core's CONTRACT.md)
//!
//! One [`Gc`] god-struct owns the Gekko CPU + memory + the Flipper stub and
//! implements [`bus::Bus`]; each subsystem is a struct owning only its own
//! state, and cross-subsystem calls pass `&mut` references as parameters. This
//! phase lands the CPU register/exception foundation, the memory map, the bus,
//! and the framebuffer only — instruction coverage is a meaningful *starter*
//! set, with everything else a documented `Decoded::Unimplemented` seam (never a
//! silent no-op). It is NOT a functional GameCube emulator.

// --- Foundation (the contract every other module codes against).
pub mod bus;
pub mod cpu;
pub mod mem;
pub mod regions;

// --- Flipper GPU framebuffer stub (so the wasm surface compiles).
pub mod gx;

// --- Top-level orchestrator (the god-struct + Bus impl).
pub mod gc;

// --- Web target: the wasm-bindgen surface (`WasmGc`). wasm32-only so host
// `cargo test` never invokes the macro, mirroring the PS1 core.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use cpu::Cpu;
pub use gc::Gc;
pub use mem::Mem;
