//! Xbox kernel (`xboxkrnl.exe`) export table: ordinal → (name, stdcall arg
//! bytes). This is pure reference data used by the HLE kernel ([`crate::hle`]) to
//! name imports and to clean the right number of bytes off the stack on return
//! (the Xbox kernel is stdcall: the callee pops its arguments).
//!
//! STUB: the full table is populated separately. `lookup` returns `None` until
//! then; callers must handle the unknown case.

/// Look up a kernel export by ordinal. Returns `(name, arg_byte_count)` — e.g.
/// a 3-DWORD-argument stdcall function returns `("Foo", 12)`.
pub fn lookup(ordinal: u32) -> Option<(&'static str, u16)> {
    let _ = ordinal;
    None
}
