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
//! The one exception is `DbgPrint`, which is **cdecl/variadic** (the *caller*
//! cleans the stack), so its handler pops only the return address and leaves the
//! arguments in place (`arg_bytes == 0`).
//!
//! Ordinal numbers and stdcall arg-byte counts here are cross-checked against the
//! public `xboxkrnl.exe` export table (XboxDev/nxdk `xboxkrnl.exe.def`) and the
//! OpenXDK headers. The [`crate::hle_table`] module is the authority on ordinal
//! numbers; the constants below mirror it so the matcher works even if the table
//! is still being populated, and unknown ordinals fall through to
//! [`Dispatch::Unhandled`] (named from the table when possible).

use crate::cpu::state::{EAX, ESP};
use crate::cpu::Cpu;
use crate::hle_table;
use crate::mem::Mem;
use std::sync::atomic::{AtomicU32, Ordering};

/// Outcome of handling one kernel-import CALL.
#[derive(Debug, Clone)]
pub enum Dispatch {
    /// Handled: control flow was returned to the caller; keep executing.
    Handled(&'static str),
    /// No handler for this ordinal — the orchestrator should stop and report it.
    /// Carries the name if known (from [`hle_table`]).
    Unhandled(Option<&'static str>),
}

// ---------------------------------------------------------------------------
// Kernel global state.
// ---------------------------------------------------------------------------

/// Common Windows/NT status code: success.
const STATUS_SUCCESS: u32 = 0x0000_0000;
/// `STATUS_NO_MEMORY` — returned when the bump allocator is exhausted.
const STATUS_NO_MEMORY: u32 = 0xC000_0017;

/// Base of the HLE contiguous-memory heap. We reserve a region high in the 64 MB
/// RAM (well above a freshly-loaded XBE at the `0x10000` image base) and hand out
/// 4 KB-aligned blocks from it with a simple bump allocator. There is no free
/// list — `MmFreeContiguousMemory` is a no-op — which is fine for early boot and
/// keeps allocations deterministic and identity-mapped (guest VA == RAM offset,
/// since paging is off). State lives in a module-level `static` because
/// `dispatch` only receives `cpu`/`mem`; it persists for the process lifetime.
const HEAP_BASE: u32 = 0x0200_0000; // 32 MB mark
/// End of the heap (== `RAM_SIZE`, 64 MB). Allocations past this fail.
const HEAP_END: u32 = crate::regions::RAM_SIZE as u32;
/// Allocation granularity / default alignment (a page).
const PAGE: u32 = 0x1000;

/// The bump pointer: next free guest address in the HLE heap. Starts at
/// [`HEAP_BASE`] and only ever increases. `AtomicU32` so no `Mutex` is needed.
static NEXT_HEAP: AtomicU32 = AtomicU32::new(HEAP_BASE);

/// Round `v` up to a multiple of `align` (a power of two).
#[inline]
fn align_up(v: u32, align: u32) -> u32 {
    let a = align.max(1);
    v.wrapping_add(a - 1) & !(a - 1)
}

/// Allocate `size` bytes from the bump heap with at least `align` alignment
/// (clamped to a page minimum). Returns the guest base address, or `0` on
/// exhaustion (mirrors the kernel returning NULL). Thread-safe via a CAS loop so
/// concurrent dispatches can't hand out overlapping blocks.
fn heap_alloc(size: u32, align: u32) -> u32 {
    let align = align.max(PAGE);
    // Always carve at least one page so two zero-size allocations are distinct.
    let size = align_up(size.max(1), PAGE);
    loop {
        let cur = NEXT_HEAP.load(Ordering::Relaxed);
        let base = align_up(cur, align);
        let end = base.checked_add(size);
        match end {
            Some(end) if end <= HEAP_END => {
                if NEXT_HEAP
                    .compare_exchange(cur, end, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    return base;
                }
                // Lost the race; retry.
            }
            _ => return 0, // exhausted or overflow
        }
    }
}

/// Reset the bump heap. Test-only; the orchestrator gets a fresh process per run
/// in practice, but tests need a known starting point.
#[cfg(test)]
fn heap_reset() {
    NEXT_HEAP.store(HEAP_BASE, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// stdcall helpers.
// ---------------------------------------------------------------------------

/// Read the `i`-th DWORD stdcall argument (0-based). With the CALL having pushed
/// the return address, `arg[0]` is at `[ESP + 4]`, `arg[1]` at `[ESP + 8]`, … .
#[inline]
fn arg(cpu: &Cpu, mem: &Mem, i: u32) -> u32 {
    let esp = cpu.reg32(ESP);
    mem.ram_read32(esp.wrapping_add(4 + i * 4))
}

/// Complete a handled call: set `EAX`, pop the return address into `EIP`, and
/// clean `4 + arg_bytes` off the stack (the stdcall callee-cleanup contract).
///
/// For cdecl/variadic functions (`DbgPrint`) pass `arg_bytes == 0` so the caller
/// keeps responsibility for the arguments.
fn stdcall_return(cpu: &mut Cpu, mem: &Mem, eax: u32, arg_bytes: u32) {
    let esp = cpu.reg32(ESP);
    let ret = mem.ram_read32(esp);
    cpu.set_reg32(EAX, eax);
    cpu.eip = ret;
    cpu.set_reg32(ESP, esp.wrapping_add(4 + arg_bytes));
}

// ---------------------------------------------------------------------------
// Guest-string helpers (for the Rtl*String initializers).
// ---------------------------------------------------------------------------

/// Length in bytes of a NUL-terminated ANSI string at guest `ptr`, not counting
/// the terminator. Bounded so a missing terminator can't loop forever.
fn ansi_len(mem: &Mem, ptr: u32) -> u32 {
    if ptr == 0 {
        return 0;
    }
    let mut n = 0u32;
    while n < u16::MAX as u32 {
        if mem.ram_read8(ptr.wrapping_add(n)) == 0 {
            break;
        }
        n += 1;
    }
    n
}

/// Length in **bytes** of a NUL-terminated UTF-16 (wide) string at guest `ptr`,
/// not counting the 2-byte terminator. Bounded like [`ansi_len`].
fn wide_len_bytes(mem: &Mem, ptr: u32) -> u32 {
    if ptr == 0 {
        return 0;
    }
    let mut n = 0u32;
    while n < u16::MAX as u32 {
        if mem.ram_read16(ptr.wrapping_add(n)) == 0 {
            break;
        }
        n += 2;
    }
    n
}

// ---------------------------------------------------------------------------
// Ordinal constants (cross-checked against xboxkrnl.exe.def / OpenXDK).
// ---------------------------------------------------------------------------

const ORD_DBG_PRINT: u32 = 8;
const ORD_KE_INITIALIZE_DPC: u32 = 107;
const ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY: u32 = 165;
const ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY_EX: u32 = 166;
const ORD_MM_ALLOCATE_SYSTEM_MEMORY: u32 = 167;
const ORD_MM_FREE_CONTIGUOUS_MEMORY: u32 = 171;
const ORD_MM_FREE_SYSTEM_MEMORY: u32 = 172;
const ORD_MM_GET_PHYSICAL_ADDRESS: u32 = 173;
const ORD_MM_PERSIST_CONTIGUOUS_MEMORY: u32 = 178;
const ORD_MM_QUERY_ALLOCATION_SIZE: u32 = 180;
const ORD_NT_ALLOCATE_VIRTUAL_MEMORY: u32 = 184;
const ORD_RTL_ENTER_CRITICAL_SECTION: u32 = 277;
const ORD_RTL_ENTER_CRITICAL_SECTION_AND_REGION: u32 = 278;
const ORD_RTL_INIT_ANSI_STRING: u32 = 289;
const ORD_RTL_INIT_UNICODE_STRING: u32 = 290;
const ORD_RTL_INITIALIZE_CRITICAL_SECTION: u32 = 291;
const ORD_RTL_LEAVE_CRITICAL_SECTION: u32 = 294;
const ORD_RTL_LEAVE_CRITICAL_SECTION_AND_REGION: u32 = 295;
const ORD_PS_CREATE_SYSTEM_THREAD_EX: u32 = 255;

/// Fake handle / id handed back for created threads (we don't model handles yet).
const FAKE_THREAD_HANDLE: u32 = 0x0000_BEEF;

/// Ordinals safe to stub as "return STATUS_SUCCESS, clean the stack" — init /
/// notification / registration functions whose side effects don't matter for
/// reaching the title screen. Grown empirically as the boot progresses.
const SAFE_NOOP: &[u32] = &[
    47,  // HalRegisterShutdownNotification
    113, // KeInitializeTimerEx
    24,  // ExQueryNonVolatileSetting (returns success; value left zero)
    301, // RtlNtStatusToDosError (returns 0 = ERROR_SUCCESS)
    149, // KeSetTimer
];
/// Return address pushed under a new thread's entry: if the thread ever returns,
/// EIP lands here (recognizable, and out of mapped code) so it stops cleanly.
const THREAD_EXIT_SENTINEL: u32 = 0xDEAD_0000;

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

/// Handle a kernel-import call trapped at the HLE region. See the module docs for
/// the stdcall return contract. Returns [`Dispatch::Handled`] (with the function
/// name) once control flow has been returned to the caller, or
/// [`Dispatch::Unhandled`] (CPU untouched) so the orchestrator can stop and
/// report what the game needed.
pub fn dispatch(cpu: &mut Cpu, mem: &mut Mem, ordinal: u32) -> Dispatch {
    match ordinal {
        // ---- Debug ----
        ORD_DBG_PRINT => {
            // DbgPrint(format, ...) is cdecl/variadic — the *caller* cleans the
            // stack, so we pop only the return address (arg_bytes = 0). We don't
            // actually format the output; return STATUS_SUCCESS.
            stdcall_return(cpu, mem, STATUS_SUCCESS, 0);
            Dispatch::Handled("DbgPrint")
        }

        // ---- Memory: contiguous heap ----
        ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY => {
            // PVOID MmAllocateContiguousMemory(ULONG NumberOfBytes)
            let size = arg(cpu, mem, 0);
            let base = heap_alloc(size, PAGE);
            stdcall_return(cpu, mem, base, 4);
            Dispatch::Handled("MmAllocateContiguousMemory")
        }
        ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY_EX => {
            // PVOID MmAllocateContiguousMemoryEx(ULONG NumberOfBytes,
            //   PHYSICAL_ADDRESS Lowest, PHYSICAL_ADDRESS Highest,
            //   ULONG Alignment, ULONG ProtectionType)
            let size = arg(cpu, mem, 0);
            let alignment = arg(cpu, mem, 3);
            let base = heap_alloc(size, alignment);
            stdcall_return(cpu, mem, base, 20);
            Dispatch::Handled("MmAllocateContiguousMemoryEx")
        }
        ORD_MM_ALLOCATE_SYSTEM_MEMORY => {
            // PVOID MmAllocateSystemMemory(ULONG NumberOfBytes, ULONG Protect)
            let size = arg(cpu, mem, 0);
            let base = heap_alloc(size, PAGE);
            stdcall_return(cpu, mem, base, 8);
            Dispatch::Handled("MmAllocateSystemMemory")
        }
        ORD_MM_FREE_CONTIGUOUS_MEMORY => {
            // VOID MmFreeContiguousMemory(PVOID BaseAddress) — no free list, so
            // this is a no-op. Returns void; leave EAX as a benign 0.
            let _base = arg(cpu, mem, 0);
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("MmFreeContiguousMemory")
        }
        ORD_MM_FREE_SYSTEM_MEMORY => {
            // NTSTATUS MmFreeSystemMemory(PVOID BaseAddress, ULONG NumberOfBytes)
            stdcall_return(cpu, mem, STATUS_SUCCESS, 8);
            Dispatch::Handled("MmFreeSystemMemory")
        }
        ORD_MM_GET_PHYSICAL_ADDRESS => {
            // PHYSICAL_ADDRESS MmGetPhysicalAddress(PVOID BaseAddress).
            // Paging is off, so the linear address *is* the physical address —
            // identity map.
            let va = arg(cpu, mem, 0);
            stdcall_return(cpu, mem, va, 4);
            Dispatch::Handled("MmGetPhysicalAddress")
        }
        ORD_MM_PERSIST_CONTIGUOUS_MEMORY => {
            // VOID MmPersistContiguousMemory(PVOID, ULONG, BOOLEAN) — affects
            // whether a region survives a quick-reboot; nothing to do here.
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("MmPersistContiguousMemory")
        }
        ORD_MM_QUERY_ALLOCATION_SIZE => {
            // ULONG MmQueryAllocationSize(PVOID BaseAddress). We don't track
            // per-allocation sizes; report a single page as a safe lower bound.
            let _base = arg(cpu, mem, 0);
            stdcall_return(cpu, mem, PAGE, 4);
            Dispatch::Handled("MmQueryAllocationSize")
        }
        ORD_NT_ALLOCATE_VIRTUAL_MEMORY => {
            // NTSTATUS NtAllocateVirtualMemory(PVOID *BaseAddress,
            //   ULONG ZeroBits, PSIZE_T RegionSize, ULONG AllocationType,
            //   ULONG Protect)
            // Out params: *BaseAddress (arg0) and *RegionSize (arg2). We honour a
            // caller-supplied base if non-zero, else bump-allocate.
            let p_base = arg(cpu, mem, 0);
            let p_size = arg(cpu, mem, 2);
            let req_base = if p_base != 0 {
                mem.ram_read32(p_base)
            } else {
                0
            };
            let req_size = if p_size != 0 {
                mem.ram_read32(p_size)
            } else {
                0
            };
            let size = align_up(req_size.max(1), PAGE);
            let base = if req_base != 0 {
                req_base
            } else {
                heap_alloc(size, PAGE)
            };
            let status = if base == 0 {
                STATUS_NO_MEMORY
            } else {
                if p_base != 0 {
                    mem.ram_write32(p_base, base);
                }
                if p_size != 0 {
                    mem.ram_write32(p_size, size);
                }
                STATUS_SUCCESS
            };
            stdcall_return(cpu, mem, status, 20);
            Dispatch::Handled("NtAllocateVirtualMemory")
        }

        // ---- Synchronization (single-threaded HLE: all no-ops) ----
        ORD_KE_INITIALIZE_DPC => {
            // VOID KeInitializeDpc(PKDPC, PKDEFERRED_ROUTINE, PVOID Context)
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("KeInitializeDpc")
        }
        ORD_RTL_INITIALIZE_CRITICAL_SECTION => {
            // VOID RtlInitializeCriticalSection(PRTL_CRITICAL_SECTION)
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("RtlInitializeCriticalSection")
        }
        ORD_RTL_ENTER_CRITICAL_SECTION => {
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("RtlEnterCriticalSection")
        }
        ORD_RTL_LEAVE_CRITICAL_SECTION => {
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("RtlLeaveCriticalSection")
        }
        ORD_RTL_ENTER_CRITICAL_SECTION_AND_REGION => {
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("RtlEnterCriticalSectionAndRegion")
        }
        ORD_RTL_LEAVE_CRITICAL_SECTION_AND_REGION => {
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("RtlLeaveCriticalSectionAndRegion")
        }

        // ---- Strings ----
        ORD_RTL_INIT_ANSI_STRING => {
            // VOID RtlInitAnsiString(PANSI_STRING DestinationString,
            //   PCSZ SourceString)
            // ANSI_STRING { USHORT Length; USHORT MaximumLength; PCHAR Buffer; }
            let dest = arg(cpu, mem, 0);
            let src = arg(cpu, mem, 1);
            if dest != 0 {
                let len = ansi_len(mem, src); // bytes, no terminator
                let max = if src != 0 { len + 1 } else { 0 }; // include NUL
                // Length (USHORT @ +0) | MaximumLength (USHORT @ +2) packed in a
                // DWORD, then Buffer (PCHAR @ +4).
                mem.ram_write32(dest, (len & 0xFFFF) | ((max & 0xFFFF) << 16));
                mem.ram_write32(dest.wrapping_add(4), src);
            }
            stdcall_return(cpu, mem, 0, 8);
            Dispatch::Handled("RtlInitAnsiString")
        }
        ORD_RTL_INIT_UNICODE_STRING => {
            // VOID RtlInitUnicodeString(PUNICODE_STRING DestinationString,
            //   PCWSTR SourceString)
            // UNICODE_STRING { USHORT Length; USHORT MaximumLength; PWSTR Buffer; }
            // Length/MaximumLength are in BYTES.
            let dest = arg(cpu, mem, 0);
            let src = arg(cpu, mem, 1);
            if dest != 0 {
                let len = wide_len_bytes(mem, src); // bytes, no terminator
                let max = if src != 0 { len + 2 } else { 0 }; // include wide NUL
                mem.ram_write32(dest, (len & 0xFFFF) | ((max & 0xFFFF) << 16));
                mem.ram_write32(dest.wrapping_add(4), src);
            }
            stdcall_return(cpu, mem, 0, 8);
            Dispatch::Handled("RtlInitUnicodeString")
        }

        // ---- Safe no-op-success stubs (init/notification functions that just
        // need to return STATUS_SUCCESS with correct stdcall cleanup). The
        // arg-byte count comes from the export table. Grown as boot progresses.
        o if SAFE_NOOP.contains(&o) => {
            let (name, argbytes) = hle_table::lookup(o).unwrap_or(("noop", 0));
            stdcall_return(cpu, mem, STATUS_SUCCESS, argbytes as u32);
            Dispatch::Handled(name)
        }

        // ---- Threads ----
        ORD_PS_CREATE_SYSTEM_THREAD_EX => {
            // NTSTATUS PsCreateSystemThreadEx(PHANDLE ThreadHandle, ULONG
            //   ThreadExtraSize, ULONG KernelStackSize, ULONG TlsDataSize,
            //   PULONG ThreadId, PVOID StartContext1, PVOID StartContext2,
            //   BOOLEAN CreateSuspended, BOOLEAN DebuggerThread,
            //   PKSTART_ROUTINE StartRoutine)  — 10 args, 40 bytes.
            //
            // Single-threaded HLE: rather than spawn a real thread, *switch* the
            // CPU to the new thread so the game's main code runs. The creating
            // context is abandoned (it's typically the init stub that would just
            // idle/exit). A cooperative scheduler for multiple live threads is a
            // larger future phase.
            let h_out = arg(cpu, mem, 0);
            let id_out = arg(cpu, mem, 4);
            let ctx1 = arg(cpu, mem, 5);
            let ctx2 = arg(cpu, mem, 6);
            let start = arg(cpu, mem, 9);
            if h_out != 0 {
                mem.ram_write32(h_out, FAKE_THREAD_HANDLE);
            }
            if id_out != 0 {
                mem.ram_write32(id_out, 1);
            }
            // Build the new thread's stack: StartRoutine(StartContext1,
            // StartContext2) stdcall — push args right-to-left under a return
            // sentinel.
            let stack_size = 0x0004_0000u32; // 256 KB
            let base = heap_alloc(stack_size, PAGE);
            let mut sp = base.wrapping_add(stack_size);
            sp = sp.wrapping_sub(4);
            mem.ram_write32(sp, ctx2);
            sp = sp.wrapping_sub(4);
            mem.ram_write32(sp, ctx1);
            sp = sp.wrapping_sub(4);
            mem.ram_write32(sp, THREAD_EXIT_SENTINEL);
            cpu.set_reg32(ESP, sp);
            cpu.eip = start;
            Dispatch::Handled("PsCreateSystemThreadEx")
        }

        // ---- Unknown: leave the CPU untouched; orchestrator stops & reports ----
        _ => Dispatch::Unhandled(hle_table::lookup(ordinal).map(|(n, _)| n)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::state::ESP;

    /// Serializes tests that touch the shared module-level bump heap so the
    /// parallel test runner can't interleave a `heap_reset` with another test's
    /// allocations.
    static HEAP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stack region high enough to not collide with anything; well within RAM.
    const STACK_TOP: u32 = 0x0010_0000;
    /// A fake return address the handler should jump back to.
    const RET_ADDR: u32 = 0x0001_2345;

    /// Build a CPU/Mem with ESP pointing at a synthesized stdcall frame:
    /// `[ESP] = RET_ADDR`, then `args` at `[ESP+4], [ESP+8], …`. Returns the
    /// original ESP so tests can assert the cleanup.
    fn frame(args: &[u32]) -> (Cpu, Mem, u32) {
        let mut cpu = Cpu::new();
        let mut mem = Mem::new();
        let esp = STACK_TOP;
        cpu.set_reg32(ESP, esp);
        mem.ram_write32(esp, RET_ADDR);
        for (i, &a) in args.iter().enumerate() {
            mem.ram_write32(esp + 4 + (i as u32) * 4, a);
        }
        (cpu, mem, esp)
    }

    /// Assert the standard stdcall return happened: EAX, EIP, and ESP cleanup.
    fn assert_returned(cpu: &Cpu, esp0: u32, expect_eax: u32, arg_bytes: u32) {
        assert_eq!(cpu.reg32(EAX), expect_eax, "EAX");
        assert_eq!(cpu.eip, RET_ADDR, "EIP popped return address");
        assert_eq!(cpu.reg32(ESP), esp0 + 4 + arg_bytes, "ESP cleanup");
    }

    #[test]
    fn align_up_works() {
        assert_eq!(align_up(0, 0x1000), 0);
        assert_eq!(align_up(1, 0x1000), 0x1000);
        assert_eq!(align_up(0x1000, 0x1000), 0x1000);
        assert_eq!(align_up(0x1001, 0x1000), 0x2000);
    }

    #[test]
    fn unknown_ordinal_is_unhandled_and_leaves_cpu_alone() {
        let (mut cpu, mut mem, esp0) = frame(&[]);
        let eip0 = cpu.eip;
        let out = dispatch(&mut cpu, &mut mem, 0xDEAD_BEEF);
        match out {
            Dispatch::Unhandled(_) => {}
            other => panic!("expected Unhandled, got {other:?}"),
        }
        // Control flow untouched.
        assert_eq!(cpu.reg32(ESP), esp0, "ESP unchanged");
        assert_eq!(cpu.eip, eip0, "EIP unchanged");
    }

    #[test]
    fn mm_allocate_contiguous_returns_distinct_increasing_pages() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        heap_reset();
        // First allocation.
        let (mut cpu, mut mem, esp0) = frame(&[0x1000]);
        let out = dispatch(&mut cpu, &mut mem, ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY);
        assert!(matches!(out, Dispatch::Handled("MmAllocateContiguousMemory")));
        let a = cpu.reg32(EAX);
        assert_returned(&cpu, esp0, a, 4);
        assert_eq!(a, HEAP_BASE, "first block at heap base");
        assert_eq!(a % PAGE, 0, "page-aligned");

        // Second allocation: distinct and higher.
        let (mut cpu, mut mem, esp0) = frame(&[0x1]);
        dispatch(&mut cpu, &mut mem, ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY);
        let b = cpu.reg32(EAX);
        assert_returned(&cpu, esp0, b, 4);
        assert!(b > a, "addresses increase: {b:#x} > {a:#x}");
        assert_eq!(b, HEAP_BASE + PAGE, "one page rounding for the 0x1000 alloc");

        // Third: even a zero-size carves a fresh page.
        let (mut cpu, mut mem, _) = frame(&[0]);
        dispatch(&mut cpu, &mut mem, ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY);
        let c = cpu.reg32(EAX);
        assert!(c > b, "distinct from previous");
    }

    #[test]
    fn mm_allocate_contiguous_ex_honours_alignment() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        heap_reset();
        // Burn a page so the next natural bump pointer is not 64K-aligned.
        heap_alloc(1, PAGE);
        let align = 0x10000; // 64 KB
        // args: size, lowest(2 dwords-ish here just one), highest, alignment, prot
        let (mut cpu, mut mem, esp0) = frame(&[0x2000, 0, 0xFFFF_FFFF, align, 0]);
        let out = dispatch(&mut cpu, &mut mem, ORD_MM_ALLOCATE_CONTIGUOUS_MEMORY_EX);
        assert!(matches!(out, Dispatch::Handled("MmAllocateContiguousMemoryEx")));
        let a = cpu.reg32(EAX);
        assert_returned(&cpu, esp0, a, 20);
        assert_eq!(a % align, 0, "respects requested alignment");
    }

    #[test]
    fn mm_get_physical_address_is_identity() {
        let (mut cpu, mut mem, esp0) = frame(&[0x1234_5000]);
        let out = dispatch(&mut cpu, &mut mem, ORD_MM_GET_PHYSICAL_ADDRESS);
        assert!(matches!(out, Dispatch::Handled("MmGetPhysicalAddress")));
        assert_returned(&cpu, esp0, 0x1234_5000, 4);
    }

    #[test]
    fn mm_free_and_persist_are_noops_with_correct_cleanup() {
        let (mut cpu, mut mem, esp0) = frame(&[0xAABB_CCDD]);
        dispatch(&mut cpu, &mut mem, ORD_MM_FREE_CONTIGUOUS_MEMORY);
        assert_returned(&cpu, esp0, 0, 4);

        let (mut cpu, mut mem, esp0) = frame(&[0x1000, 0x40, 1]);
        dispatch(&mut cpu, &mut mem, ORD_MM_PERSIST_CONTIGUOUS_MEMORY);
        assert_returned(&cpu, esp0, 0, 12);
    }

    #[test]
    fn dbgprint_is_cdecl_caller_cleans() {
        // DbgPrint pops only the return address (arg_bytes = 0).
        let (mut cpu, mut mem, esp0) = frame(&[0x4000, 1, 2]);
        let out = dispatch(&mut cpu, &mut mem, ORD_DBG_PRINT);
        assert!(matches!(out, Dispatch::Handled("DbgPrint")));
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 0);
    }

    #[test]
    fn critical_section_calls_are_noops() {
        for ord in [
            ORD_RTL_INITIALIZE_CRITICAL_SECTION,
            ORD_RTL_ENTER_CRITICAL_SECTION,
            ORD_RTL_LEAVE_CRITICAL_SECTION,
            ORD_RTL_ENTER_CRITICAL_SECTION_AND_REGION,
            ORD_RTL_LEAVE_CRITICAL_SECTION_AND_REGION,
        ] {
            let (mut cpu, mut mem, esp0) = frame(&[0x5000]);
            let out = dispatch(&mut cpu, &mut mem, ord);
            assert!(matches!(out, Dispatch::Handled(_)));
            assert_returned(&cpu, esp0, 0, 4);
        }
    }

    #[test]
    fn ke_initialize_dpc_noop() {
        let (mut cpu, mut mem, esp0) = frame(&[0x6000, 0x7000, 0x8000]);
        dispatch(&mut cpu, &mut mem, ORD_KE_INITIALIZE_DPC);
        assert_returned(&cpu, esp0, 0, 12);
    }

    #[test]
    fn rtl_init_ansi_string_fills_struct() {
        // Lay out a source C string "Hi" and a destination ANSI_STRING struct.
        let src = 0x0002_0000u32;
        let dest = 0x0002_1000u32;
        let (mut cpu, mut mem, esp0) = frame(&[dest, src]);
        mem.ram_write8(src, b'H' as u32);
        mem.ram_write8(src + 1, b'i' as u32);
        mem.ram_write8(src + 2, 0);

        let out = dispatch(&mut cpu, &mut mem, ORD_RTL_INIT_ANSI_STRING);
        assert!(matches!(out, Dispatch::Handled("RtlInitAnsiString")));
        assert_returned(&cpu, esp0, 0, 8);

        let packed = mem.ram_read32(dest);
        let len = packed & 0xFFFF;
        let max = (packed >> 16) & 0xFFFF;
        assert_eq!(len, 2, "Length excludes the NUL");
        assert_eq!(max, 3, "MaximumLength includes the NUL");
        assert_eq!(mem.ram_read32(dest + 4), src, "Buffer points at source");
    }

    #[test]
    fn rtl_init_unicode_string_fills_struct() {
        let src = 0x0003_0000u32;
        let dest = 0x0003_1000u32;
        let (mut cpu, mut mem, esp0) = frame(&[dest, src]);
        // Wide "Hi" = 'H',0,'i',0, then wide NUL.
        mem.ram_write16(src, b'H' as u32);
        mem.ram_write16(src + 2, b'i' as u32);
        mem.ram_write16(src + 4, 0);

        let out = dispatch(&mut cpu, &mut mem, ORD_RTL_INIT_UNICODE_STRING);
        assert!(matches!(out, Dispatch::Handled("RtlInitUnicodeString")));
        assert_returned(&cpu, esp0, 0, 8);

        let packed = mem.ram_read32(dest);
        let len = packed & 0xFFFF;
        let max = (packed >> 16) & 0xFFFF;
        assert_eq!(len, 4, "Length is byte count of 2 wide chars");
        assert_eq!(max, 6, "MaximumLength includes the wide NUL");
        assert_eq!(mem.ram_read32(dest + 4), src, "Buffer points at source");
    }

    #[test]
    fn rtl_init_ansi_string_null_source() {
        let dest = 0x0004_0000u32;
        let (mut cpu, mut mem, esp0) = frame(&[dest, 0]);
        dispatch(&mut cpu, &mut mem, ORD_RTL_INIT_ANSI_STRING);
        assert_returned(&cpu, esp0, 0, 8);
        let packed = mem.ram_read32(dest);
        assert_eq!(packed & 0xFFFF, 0, "Length 0 for NULL source");
        assert_eq!((packed >> 16) & 0xFFFF, 0, "MaximumLength 0 for NULL source");
        assert_eq!(mem.ram_read32(dest + 4), 0, "Buffer is NULL");
    }

    #[test]
    fn nt_allocate_virtual_memory_bump_path() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        heap_reset();
        // Out-params: *BaseAddress = 0 (let the kernel choose), *RegionSize set.
        let p_base = 0x0005_0000u32;
        let p_size = 0x0005_0010u32;
        let (mut cpu, mut mem, esp0) = frame(&[p_base, 0, p_size, 0x1000, 0x04]);
        mem.ram_write32(p_base, 0); // request kernel-chosen base
        mem.ram_write32(p_size, 0x3000); // request 12 KB

        let out = dispatch(&mut cpu, &mut mem, ORD_NT_ALLOCATE_VIRTUAL_MEMORY);
        assert!(matches!(out, Dispatch::Handled("NtAllocateVirtualMemory")));
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 20);

        let base = mem.ram_read32(p_base);
        let size = mem.ram_read32(p_size);
        assert_eq!(base, HEAP_BASE, "wrote chosen base into *BaseAddress");
        assert_eq!(size, 0x3000, "page-aligned region size written back");
    }

    #[test]
    fn nt_allocate_virtual_memory_honours_requested_base() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        heap_reset();
        let p_base = 0x0006_0000u32;
        let p_size = 0x0006_0010u32;
        let (mut cpu, mut mem, esp0) = frame(&[p_base, 0, p_size, 0x1000, 0x04]);
        mem.ram_write32(p_base, 0x0030_0000); // caller-supplied base
        mem.ram_write32(p_size, 0x1000);

        dispatch(&mut cpu, &mut mem, ORD_NT_ALLOCATE_VIRTUAL_MEMORY);
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 20);
        assert_eq!(mem.ram_read32(p_base), 0x0030_0000, "kept caller's base");
    }

    #[test]
    fn heap_alloc_exhaustion_returns_zero() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Force the bump pointer to the very top, then request a block.
        NEXT_HEAP.store(HEAP_END, Ordering::SeqCst);
        assert_eq!(heap_alloc(0x1000, PAGE), 0, "exhausted heap returns NULL");
        heap_reset();
    }
}
