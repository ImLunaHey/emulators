//! NEC VR4300 (MIPS R4300i, MIPS III) CPU: architectural register state
//! ([`state`]), the COP0 system-control coprocessor + exception model
//! ([`cop0`]), the COP1 FPU ([`cop1`]), and the instruction interpreter
//! ([`exec`]).

pub mod cop0;
pub mod cop1;
pub mod exec;
pub mod state;

pub use cop0::{Cop0, Exception, TlbEntry};
pub use cop1::Cop1;
pub use state::{Cpu, RESET_VECTOR};
