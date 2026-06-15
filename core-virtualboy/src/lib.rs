//! Pure-Rust Nintendo Virtual Boy core, built from-scratch against the Planet
//! Virtual Boy "Sacred Tech Scroll" hardware documentation and the NEC V810
//! (uPD70732) architecture manual. There is no source to port.
//!
//! Ownership model (mirrors the sibling cores' CONTRACT.md): one [`Vb`]
//! god-struct owns every subsystem (CPU / VIP / VSU / hardware registers /
//! cartridge / input) and implements the V810 [`bus::Bus`]. Cross-subsystem
//! calls pass `&mut` references as parameters (resolved with `mem::take` at the
//! call site) — no `Rc`/`RefCell`. Closed enums + exhaustive `match`;
//! little-endian; boxed regions; fixed-width integers.
//!
//! ## Completeness map
//!
//! IMPLEMENTED:
//!  - **CPU (V810)** — full Format I-VII integer instruction set: register/
//!    immediate ALU, shifts, mul/div (signed + unsigned), MOVEA/ADDI/MOVHI,
//!    ORI/ANDI/XORI, JR/JAL/JMP, Bcond (all 16 conditions), SETF, LD/ST (byte/
//!    halfword/word, sign- and zero-extended), CLI/SEI, LDSR/STSR over the
//!    system-register bank, TRAP/RETI/HALT, exception/interrupt vectoring with
//!    the EIPC/FEPC banks and PSW EP/NP/ID handling. On-chip **FPU**: CMPF.S,
//!    CVT.WS, CVT.SW, ADDF.S, SUBF.S, MULF.S, DIVF.S, TRNC.SW + the Nintendo
//!    extended ops (MPYHW, REV, XB, XH).
//!  - **Memory map** — VIP DRAM, VSU, hardware control registers, 64 KiB WRAM,
//!    cartridge ROM (mirrored) + battery SRAM, with the region-decode bus.
//!  - **VIP** — normal + H-bias + OBJ worlds (32-world stack, painter's order),
//!    BGMap/CHR/OAM memory, GPLT/JPLT palettes, BRTA/B/C brightness -> red RGBA,
//!    frame/draw interrupts (FRAMESTART/XPEND/LFBEND), DPSTTS/XPSTTS status.
//!    Renders the LEFT eye at 384x224.
//!  - **VSU** — 6 channels (5 wave-table + 1 noise), 5x 32-sample wave RAM,
//!    envelope + frequency stepping, mono mixdown, drain.
//!  - **Timer** — programmable interval timer with the 20us/100us tick + the
//!    timer interrupt. **Input** — full VB pad mapped to the SDLR/SDHR words.
//!  - **Crash screen** — illegal-opcode fault paints a red-on-black readout.
//!
//! PARTIAL / STUBBED (see the per-module docs):
//!  - VIP affine + H-bias warping are approximated as a normal scrolled tilemap;
//!    the column table (per-column brightness repeat) is treated as uniform;
//!    only the LEFT eye is output (no anaglyph), and double-buffer FB selection
//!    is cosmetic.
//!  - Bit-string instructions update the length/Z bookkeeping but do not yet
//!    perform the memory move/search (rare in boot code).
//!  - VSU channel-5 modulation/sweep and the auto-shutoff interval are
//!    approximated; the link/communication port is a stub.
//!  - CPU cycle counts are representative per-class approximations, not exact.

pub mod bus;
pub mod cart;
pub mod cpu;
pub mod crash;
pub mod hw;
pub mod input;
pub mod vb;
pub mod vip;
pub mod vsu;

pub use vb::Vb;

// Web target surface (wasm-bindgen). Gated to wasm32 so host builds/tests don't
// pull in the macro machinery.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
