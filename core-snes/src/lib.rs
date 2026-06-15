//! Pure-Rust Super Nintendo (SNES / Super Famicom) core, built from-scratch
//! against public hardware documentation: anomie's SNES docs, fullsnes/nocash,
//! superfamicom.org, and the WDC 65C816 datasheet. There is no source to port.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Snes`]
//! god-struct owns every subsystem (the 65816 CPU, the PPU, the APU = SPC700 +
//! S-DSP, the cartridge, and input) and implements the CPU [`bus::Bus`].
//! Cross-subsystem calls pass `&mut` references as parameters (resolved with
//! `mem::take` at the call site) — no `Rc`/`RefCell`. Closed enums + exhaustive
//! `match`; little-endian; boxed (`Box<[T]>`) regions; fixed-width integers.
//!
//! ## Completeness (this build)
//!
//! Implemented:
//! - **CPU (WDC 65C816 / Ricoh 5A22):** full documented instruction set with
//!   8/16-bit accumulator + index via the M/X flags, emulation vs native mode,
//!   24-bit banked addressing, all addressing modes, decimal mode, and
//!   approximate (memory-access-counted) cycle stepping. RESET/NMI/IRQ/BRK/COP.
//! - **Memory map:** LoROM and HiROM mapping with header auto-detection
//!   ($7FC0 / $FFC0), 128 KiB WRAM with the $2180-$2183 WRAM port, battery SRAM,
//!   the B-bus ($2100-$213F PPU, $2140-$2143 APU ports), CPU I/O registers,
//!   general-purpose DMA ($420B / $43xx) and HDMA ($420C) channels.
//! - **PPU:** VRAM/CGRAM/OAM with their register ports, BG modes 0-7 (including
//!   Mode 7 affine), 8x8/16x16 tiles, 4bpp/2bpp/8bpp tile decode, sprite (OBJ)
//!   rendering with priority, BG/OBJ priority resolution, main/sub screen +
//!   basic color math, 15-bit BGR -> RGBA8888 at 256x224 (NTSC).
//! - **APU:** SPC700 CPU + 64 KiB ARAM + the IPL boot ROM, the 4-byte
//!   $2140-$2143 port handshake the main CPU uses to upload code (so boot-on-APU
//!   games don't deadlock), and a partial S-DSP (registers + KON/KOFF + a simple
//!   sample mixer feeding `drain_audio`).
//! - **Input:** standard controller via the auto-joypad read ($4218-$421F) and
//!   the manual $4016/$4017 serial path.
//!
//! Stubbed / partial (see module docs + the README in each file):
//! - S-DSP is a coarse approximation (BRR decode + gaussian interpolation are
//!   simplified; envelopes/echo are minimal). Audio is "won't deadlock + makes
//!   some sound", not accurate.
//! - PPU color math, windows, and mosaic are basic; offset-per-tile (modes 2/4/6)
//!   and high-res modes are not implemented.
//! - Cycle counts are memory-access approximations, not master-clock exact.

pub mod apu;
pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod dma;
pub mod input;
pub mod ppu;
pub mod snes;

pub use snes::Snes;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
