//! BIOS support — HLE hooks for the kernel call vectors.
//!
//! Built from psx-spx "BIOS Function Summary". The 512 KB BIOS ROM itself is
//! plain storage in [`crate::memory::Mem`]; this module is the seam for
//! *optionally* intercepting the kernel's call vectors — A(0xA0), B(0xB0),
//! C(0xC0) — to high-level-emulate the slow ROM routines (TTY putchar,
//! controller/card services, the `std` library) once the CPU is running real
//! code. It owns no large state, so it is **not** a [`crate::psx::Psx`] field;
//! the orchestrator calls [`bios_call`] from its exec loop when `pc` hits a
//! vector. For now every call returns `Unhandled`, i.e. "let the real ROM run".

/// The three BIOS call vectors (psx-spx). A `JR $t2` through one of these with
/// the function number in `$t1` enters the kernel jump table.
pub const VECTOR_A: u32 = 0x0000_00A0;
pub const VECTOR_B: u32 = 0x0000_00B0;
pub const VECTOR_C: u32 = 0x0000_00C0;

/// Result of an HLE attempt at a BIOS call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiosCall {
    /// Not intercepted — fall through and execute the real ROM routine.
    Unhandled,
    /// Intercepted; the orchestrator should return to the caller with this
    /// value in `$v0`.
    Handled(u32),
}

/// Attempt to high-level-emulate the BIOS call at `vector` (one of
/// [`VECTOR_A`]/[`VECTOR_B`]/[`VECTOR_C`]) with function number `func`. Stub:
/// nothing is intercepted yet, so the real ROM always runs.
pub fn bios_call(vector: u32, func: u32) -> BiosCall {
    let _ = (vector, func);
    // TODO: TTY (A(3Dh) std_out_putchar / B(3Dh)), controller + memory-card
    // services, malloc/std heap, etc.
    BiosCall::Unhandled
}

/// True once a real BIOS image has been supplied; gates whether the orchestrator
/// installs the [`install_min_hle`] fallback environment.
#[inline]
pub fn have_bios(bios_loaded: bool) -> bool {
    bios_loaded
}

/// Install the **minimum** high-level-emulated boot environment into a freshly
/// reset machine when **no** real BIOS ROM is present (psx-spx "Kernel Memory").
///
/// A genuine PSX cannot run without its 512 KB BIOS — it owns the exception
/// dispatcher, the kernel jump tables (A/B/C), and the boot shell that mounts
/// the disc. We obviously can't reproduce that from nothing; what we *can* do,
/// so a BIOS-less core still steps frames without trapping into garbage, is lay
/// down a tiny self-consistent stub:
///
/// * a `RFE; nop` exception handler at the general vector (0x8000_0080) so any
///   exception returns cleanly instead of running uninitialised RAM,
/// * the same trampoline at the three kernel call vectors (A0/B0/C0) so an
///   unhandled `jr` through them returns,
/// * a `jr $ra; nop` at the very bottom of RAM so a `jal 0` returns.
///
/// `ram` is the 2 MB main-RAM backing store (already zeroed at reset). This is
/// deliberately conservative: real games need the real BIOS, but the smoke test
/// (and any pure-CPU harness) boots and runs.
pub fn install_min_hle(ram: &mut [u8]) {
    // MIPS encodings used by the trampolines.
    const NOP: u32 = 0x0000_0000;
    const RFE: u32 = (0x10 << 26) | (0x10 << 21) | 0x10; // COP0 RFE
    const JR_RA: u32 = (31u32 << 21) | 0x08; // jr $ra (SPECIAL funct 0x08)

    let write32 = |ram: &mut [u8], addr: u32, v: u32| {
        let a = (addr & 0x1F_FFFF) as usize; // fold to the 2 MB RAM window
        if a + 4 <= ram.len() {
            ram[a] = v as u8;
            ram[a + 1] = (v >> 8) as u8;
            ram[a + 2] = (v >> 16) as u8;
            ram[a + 3] = (v >> 24) as u8;
        }
    };

    // General exception handler @ 0x80000080 (KSEG0 → RAM 0x80): RFE then a NOP
    // in the branch-delay slot. (RFE only restores the mode stack; the return
    // address pair is left at the handler, so this is a minimal "ignore and
    // continue" — enough for the smoke test, not a real kernel.)
    write32(ram, 0x80, RFE);
    write32(ram, 0x84, NOP);

    // Kernel call vectors A0/B0/C0: jr $ra; nop. An unhandled call returns.
    for v in [VECTOR_A, VECTOR_B, VECTOR_C] {
        write32(ram, v, JR_RA);
        write32(ram, v + 4, NOP);
    }

    // Bottom of RAM: jr $ra; nop, so a `jal 0` (null call) returns.
    write32(ram, 0x0, JR_RA);
    write32(ram, 0x4, NOP);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_fall_through_for_now() {
        assert_eq!(bios_call(VECTOR_A, 0x3D), BiosCall::Unhandled);
    }

    #[test]
    fn min_hle_installs_trampolines() {
        let mut ram = vec![0u8; 0x20_0000];
        install_min_hle(&mut ram);
        // RFE word at the exception vector offset 0x80.
        let rfe = u32::from_le_bytes([ram[0x80], ram[0x81], ram[0x82], ram[0x83]]);
        assert_eq!(rfe, (0x10 << 26) | (0x10 << 21) | 0x10);
        // jr $ra at the A0 vector.
        let a0 = u32::from_le_bytes([ram[0xA0], ram[0xA1], ram[0xA2], ram[0xA3]]);
        assert_eq!(a0 & 0x3F, 0x08, "SPECIAL funct = jr");
    }
}
