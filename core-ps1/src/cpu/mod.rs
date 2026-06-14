//! MIPS R3000A CPU: architectural register state ([`state`]), the COP0
//! system-control coprocessor + exception model ([`cop0`]), and the
//! instruction-execution seam ([`exec`], filled in later).

pub mod cop0;
pub mod exec;
pub mod state;

pub use cop0::{Cop0, Exception};
pub use state::{Cpu, LoadSlot, RESET_VECTOR};
