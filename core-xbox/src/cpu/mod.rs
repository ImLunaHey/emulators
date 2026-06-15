//! The Intel Pentium III "Coppermine" CPU (IA-32 / x86).
//!
//! Built from scratch against the Intel IA-32 Software Developer's Manual
//! (Volumes 2 & 3). The Xbox CPU is a 733 MHz Mobile-Celeron-class Pentium III:
//! a 32-bit, **little-endian**, variable-length-instruction CISC core. This
//! module lands the architectural register file ([`state`]) — the eight GPRs,
//! the segment registers, EFLAGS, EIP and the control registers, plus the
//! flag-computation and exception helpers — and a starter integer-instruction
//! interpreter ([`exec`]).
//!
//! x86 powers on in 16-bit **real mode** at the reset vector `0xFFFF_FFF0`
//! (CS:IP = `F000:FFF0`); the boot ROM later enables protected mode (CR0.PE) and
//! paging (CR0.PG). The interpreter honours the real/protected distinction for
//! the default operand/address size and the segment-base computation, but does
//! not yet model the GDT/IDT, privilege levels, or paging — those are documented
//! seams for later phases.

pub mod exec;
pub mod fpu;
pub mod state;

pub use state::{Cpu, Exception, Fault};
