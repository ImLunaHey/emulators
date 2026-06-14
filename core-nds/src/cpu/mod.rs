//! DS CPU foundation: the shared ARM register-file state and the ARM9 CP15
//! coprocessor. Instruction execution (arm/thumb/shifter/exec) lands later in
//! the empty modules pre-declared here.

pub mod cp15;
pub mod state;

// --- Instruction execution (ported per-file against the foundation).
pub mod arm;
pub mod exec;
pub mod shifter;
pub mod thumb;

pub use cp15::Cp15;
pub use state::CpuState;
