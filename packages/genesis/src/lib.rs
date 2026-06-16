//! Pure-Rust Sega Genesis / Mega Drive core, built from-scratch against public
//! hardware documentation: the Motorola M68000 Programmer's Reference Manual,
//! the Sega 315-5313 VDP notes, plutiedev.com, and the Zilog Z80 / SN76489
//! specs reused from the sibling SMS core. There is no source to port.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Genesis`]
//! god-struct owns every subsystem (68000, Z80, VDP, YM2612, SN76489 PSG,
//! cartridge, input) and implements BOTH CPUs' bus traits. Cross-subsystem
//! calls pass `&mut` references resolved with `mem::take` at the call site — no
//! `Rc`/`RefCell`. Closed enums + exhaustive `match`; the 68000 is BIG-ENDIAN;
//! boxed regions; fixed-width integers.
//!
//! # Implementation status
//!
//! Implemented:
//!   - Motorola 68000: the full common instruction set — MOVE/MOVEA/MOVEQ,
//!     all immediate ALU ops, ADD/SUB/AND/OR/EOR/CMP (+ A/X/M/I/Q forms),
//!     MULU/MULS, DIVU/DIVS, shifts/rotates (AS/LS/ROX/RO), bit ops, BTST/etc,
//!     Bcc/BRA/BSR/DBcc/Scc, JMP/JSR/RTS/RTE/RTR, LEA/PEA, MOVEM, EXT/SWAP,
//!     LINK/UNLK, TRAP/TRAPV, STOP/NOP/RESET, MOVE to/from SR/CCR/USP, all 12
//!     addressing modes, CCR flags, supervisor/user stacks, and autovectored
//!     interrupts (level 4 HINT / 6 VINT) + exception vectoring.
//!   - VDP: 24 registers, the control-port command protocol, VRAM/CRAM/VSRAM,
//!     auto-increment, DMA (68k->VDP, VRAM fill, VRAM copy), H/V counters,
//!     V/H interrupts, and a per-frame renderer (planes A/B with scroll +
//!     per-tile priority, sprites). 320x224 (H40) and 256x224 (H32).
//!   - Z80 sound CPU (reused verbatim from the SMS core) with 8 KiB RAM, the
//!     68000 BUSREQ/RESET arbitration, and the bank-switch window into 68000
//!     space.
//!   - YM2612: complete register latching + best-effort tone synthesis.
//!   - SN76489 PSG (reused from the SMS core).
//!   - Cartridge header parsing + plain ROM + battery SRAM.
//!   - 3/6-button controller protocol.
//!
//! Stubbed / next steps (video is the priority; these are accuracy gaps):
//!   - Cycle accuracy (instruction timings are approximate, rendering is
//!     per-frame not per-scanline → raster effects / mid-frame palette swaps
//!     won't show).
//!   - 68000 address-error / bus-error exceptions (odd-address access), BCD
//!     (ABCD/SBCD/NBCD), CHK.
//!   - YM2612 4-operator envelope/algorithm/LFO chain (only register state +
//!     a placeholder oscillator).
//!   - Bank-switch mappers (SSF2 etc.) and the window plane (plane A/B only).
//!   - VDP shadow/highlight + interlace.

pub mod bus;
pub mod cart;
pub mod crash;
pub mod genesis;
pub mod io;
pub mod m68k;
pub mod psg;
pub mod vdp;
pub mod ym2612;
pub mod z80;

pub use genesis::Genesis;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
