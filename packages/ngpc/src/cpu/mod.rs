//! Toshiba TLCS-900/H CPU — the Neo Geo Pocket Color's main processor.
//!
//! Split across:
//!   * `state`   — register file (banked), flags, the `Cpu` struct + reg access
//!   * `bus`     — the 24-bit memory `Bus` trait (+ a flat test bus)
//!   * `exec`    — instruction fetch + first-byte dispatch + addressing modes
//!   * `alu`     — ALU primitives (flags), the second-opcode-byte handlers,
//!     condition codes, shifts/rotates
//!   * `control` — interrupt acceptance, RETI, SWI

pub mod alu;
pub mod bus;
pub mod control;
pub mod exec;
pub mod state;

pub use bus::Bus;
pub use state::{Cpu, Size};
