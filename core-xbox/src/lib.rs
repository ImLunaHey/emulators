//! Pure-Rust original Microsoft Xbox (2001) core, built from scratch against
//! public hardware references — the XboxDevWiki / xbox-linux documentation for
//! the system, and the Intel IA-32 Software Developer's Manual for the CPU.
//! Seventh emulator core in this repo, sibling of the GBA (`../core`), NDS, PS1,
//! GBC, NES, SMS and GameCube (`../core-gc`) cores.
//!
//! # Hardware (XboxDevWiki "Hardware", "Memory")
//!
//! * **CPU — Intel Pentium III "Coppermine" @ 733 MHz.** A 32-bit IA-32 (x86)
//!   superscalar core (Mobile Celeron stepping) with MMX/SSE. x86 is
//!   **LITTLE-ENDIAN** — every memory accessor and instruction fetch in this
//!   crate is little-endian (in deliberate contrast to the big-endian GameCube
//!   core). It powers on in 16-bit *real mode* at the reset vector
//!   `0xFFFF_FFF0` (CS:IP = `F000:FFF0`), then the boot ROM enables protected
//!   mode + paging. See [`cpu`].
//! * **RAM — 64 MB unified DDR** at physical `0x0000_0000` (shared by the CPU
//!   and the GPU; there is no separate VRAM). Debug kits had 128 MB; retail is
//!   64 MB, which is what we model. XboxDevWiki "Memory". See [`mem`].
//! * **GPU — Nvidia NV2A @ 233 MHz**, a GeForce3/4-class part with a programmable
//!   vertex + register-combiner pixel pipeline, scanning out of unified RAM. Its
//!   register block lives at `0xFD00_0000`. Modelled here only as a framebuffer
//!   stub ([`gpu`]).
//! * **MCPX southbridge / "MCPX" boot ROM, APU (audio), USB (controllers), the
//!   IDE bus (HDD + DVD), the SMBus** — all the I/O. Entirely absent here; this
//!   phase lands only the CPU/mem/bus/framebuffer foundation.
//! * **Flash BIOS — 256 KB** holding the 2BL + the encrypted kernel, mirrored
//!   across the top 16 MB of the address space (so the reset vector lands in it).
//!   The real boot chain (secret MCPX ROM → decrypt 2BL → kernel) is not
//!   modelled; without a BIOS the CPU fetches open-bus zeros and traps — expected
//!   for this foundation.
//!
//! # Ownership model (mirrors the PS1/GC cores' contract)
//!
//! One [`Xbox`] god-struct owns the Pentium III CPU + memory + the NV2A stub and
//! implements [`bus::Bus`]; each subsystem is a struct owning only its own state,
//! and cross-subsystem calls pass `&mut` references as parameters. This phase
//! lands the CPU register/exception foundation, the memory map, the bus, and the
//! framebuffer only — instruction coverage is a meaningful *starter* set of the
//! IA-32 integer ISA, with everything else a documented `Decoded::Unimplemented`
//! seam that raises an #UD (invalid-opcode) exception (never a silent no-op). It
//! is NOT a functional Xbox emulator.

// --- Foundation (the contract every other module codes against).
pub mod bus;
pub mod cpu;
pub mod mem;
pub mod regions;

// --- NV2A GPU framebuffer stub (so the wasm surface compiles).
pub mod gpu;

// --- Crash screen (rendered when the CPU storms exceptions).
pub mod crash;

// --- XISO / XDVDFS disc parsing (mount + identify a game disc).
pub mod xiso;

// --- Top-level orchestrator (the god-struct + Bus impl).
pub mod xbox;

// --- Web target: the wasm-bindgen surface (`WasmXbox`). wasm32-only so host
// `cargo test` never invokes the macro, mirroring the PS1/GC cores.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use cpu::Cpu;
pub use mem::Mem;
pub use xbox::Xbox;
