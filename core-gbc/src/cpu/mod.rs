//! The LR35902 CPU: register state + interrupt dispatch (`state`) and the
//! instruction decode/execute interpreter (`exec`, stubbed this phase).

pub mod exec;
pub mod state;

pub use state::Cpu;
