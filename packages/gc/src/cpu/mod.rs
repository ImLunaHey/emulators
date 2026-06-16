//! The IBM PowerPC 750CXe "Gekko" CPU.
//!
//! Built from scratch against the PowerPC architecture (the public PowerPC
//! Operating Environment / User ISA) and YAGCD §2.2 ("The Gekko CPU"). The
//! Gekko is a 32-bit big-endian PowerPC 750 (G3) variant with Nintendo custom
//! extensions (paired-single SIMD over the 64-bit FPRs, locked-cache scratchpad,
//! a write-gather pipe). This module lands the architectural register file
//! ([`state`]), the special-purpose registers / exception model ([`spr`]), and a
//! starter integer-instruction interpreter ([`exec`]).
//!
//! PowerPC is **BIG-ENDIAN**; instruction fetch and every data access go through
//! the big-endian [`crate::bus::Bus`]. Instructions are fixed 32-bit words.

pub mod exec;
pub mod spr;
pub mod state;

pub use spr::Exception;
pub use state::Cpu;
