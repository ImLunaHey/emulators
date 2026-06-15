//! HLE (high-level emulation) Xbox kernel.
//!
//! When the loaded game CALLs a kernel import, the loader has redirected the
//! thunk to a trap address; the orchestrator catches that and calls [`dispatch`]
//! with the ordinal. Instead of running real `xboxkrnl.exe` code, we implement
//! the function's behaviour in Rust, then return to the caller.
//!
//! The Xbox kernel uses the **stdcall** convention: arguments are pushed
//! right-to-left and the callee pops them. So a handled call must, on return:
//!   * read its arguments from the stack (`[ESP+4]`, `[ESP+8]`, … — `[ESP]` is
//!     the return address pushed by the CALL),
//!   * place its result in `EAX`,
//!   * pop the return address into `EIP`,
//!   * add `4 + arg_bytes` to `ESP` (return addr + the args).
//!
//! STUB: [`dispatch`] currently handles nothing. The HLE handlers + stdcall
//! return sequence are implemented separately; until then unknown ordinals are
//! reported so the orchestrator can stop and show what the game needed.

use crate::cpu::Cpu;
use crate::hle_table;
use crate::mem::Mem;

/// Outcome of handling one kernel-import CALL.
#[derive(Debug, Clone)]
pub enum Dispatch {
    /// Handled: control flow was returned to the caller; keep executing.
    Handled(&'static str),
    /// No handler for this ordinal — the orchestrator should stop and report it.
    /// Carries the name if known (from [`hle_table`]).
    Unhandled(Option<&'static str>),
}

/// Handle a kernel-import call trapped at the HLE region. See the module docs for
/// the stdcall return contract. The stub leaves the CPU untouched and reports
/// the call as unhandled.
pub fn dispatch(cpu: &mut Cpu, mem: &mut Mem, ordinal: u32) -> Dispatch {
    let _ = (cpu, mem);
    Dispatch::Unhandled(hle_table::lookup(ordinal).map(|(n, _)| n))
}
