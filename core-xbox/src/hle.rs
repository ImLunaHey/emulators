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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

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
// Virtual filesystem (XISO-backed). Globals, mirroring the allocator design:
// one mounted disc image + an open-file table. Lock order is always DISC then
// FILES to avoid deadlock.
// ---------------------------------------------------------------------------

/// An open virtual-file handle into the mounted disc image.
struct FileHandle {
    offset: usize, // byte offset of the file within the disc image
    size: usize,
    pos: usize,
}

/// The mounted disc image (the XISO bytes). Empty when none is loaded.
static DISC: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Open-file table; the guest handle is `FILE_HANDLE_BASE + index`.
static FILES: Mutex<Vec<Option<FileHandle>>> = Mutex::new(Vec::new());
const FILE_HANDLE_BASE: u32 = 0x0001_0000;

// NT status codes used by the filesystem.
const STATUS_END_OF_FILE: u32 = 0xC000_0011;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const FILE_OPENED: u32 = 1; // IoStatusBlock.Information for a successful open

/// Install the mounted disc image (moves the bytes in) and reset open files.
/// Called by the loader once the game disc is mounted.
pub fn set_disc(image: Vec<u8>) {
    let mut disc = DISC.lock().unwrap();
    let mut files = FILES.lock().unwrap();
    *disc = image;
    files.clear();
}

/// Map an Xbox object path to a disc-relative path, or `None` if it doesn't
/// refer to the game disc (e.g. an HDD partition we don't emulate).
fn disc_relative(p: &str) -> Option<String> {
    let s = p.replace('\\', "/");
    let low = s.trim_start_matches('/').to_ascii_lowercase();
    for pre in [
        "device/cdrom0/",
        "device/cdrom0",
        "cdrom0/",
        "??/d:/",
        "??/d:",
        "d:/",
        "d:",
    ] {
        if let Some(rest) = low.strip_prefix(pre) {
            return Some(rest.trim_start_matches('/').to_string());
        }
    }
    None
}

/// Open a file by Xbox path. Returns `(status, handle, information)`.
fn open_file(path: &str) -> (u32, u32, u32) {
    let rel = match disc_relative(path) {
        Some(r) if !r.is_empty() => r,
        _ => return (STATUS_OBJECT_NAME_NOT_FOUND, 0, 0),
    };
    let found = {
        let disc = DISC.lock().unwrap();
        crate::xiso::resolve_path(&disc, &rel)
    };
    match found {
        Some((offset, size)) => {
            let mut files = FILES.lock().unwrap();
            files.push(Some(FileHandle { offset, size, pos: 0 }));
            (0, FILE_HANDLE_BASE + (files.len() as u32 - 1), FILE_OPENED)
        }
        None => (STATUS_OBJECT_NAME_NOT_FOUND, 0, 0),
    }
}

/// Read from an open handle into guest memory. Returns `(status, bytes_read)`.
fn read_file(mem: &mut Mem, h: u32, buf: u32, len: u32, byte_offset: Option<u32>) -> (u32, u32) {
    if h < FILE_HANDLE_BASE {
        return (STATUS_INVALID_HANDLE, 0);
    }
    let idx = (h - FILE_HANDLE_BASE) as usize;
    let disc = DISC.lock().unwrap();
    let mut files = FILES.lock().unwrap();
    let fh = match files.get_mut(idx).and_then(|s| s.as_mut()) {
        Some(f) => f,
        None => return (STATUS_INVALID_HANDLE, 0),
    };
    let pos = byte_offset.map(|o| o as usize).unwrap_or(fh.pos);
    let avail = fh.size.saturating_sub(pos);
    let n = (len as usize).min(avail);
    let start = fh.offset + pos;
    for i in 0..n {
        let b = disc.get(start + i).copied().unwrap_or(0);
        mem.ram_write8(buf.wrapping_add(i as u32), b as u32);
    }
    fh.pos = pos + n;
    if n == 0 {
        (STATUS_END_OF_FILE, 0)
    } else {
        (0, n as u32)
    }
}

/// Close an open handle. Returns true if it was one of ours.
fn close_file(h: u32) -> bool {
    if h < FILE_HANDLE_BASE {
        return false;
    }
    let idx = (h - FILE_HANDLE_BASE) as usize;
    let mut files = FILES.lock().unwrap();
    if let Some(slot) = files.get_mut(idx) {
        if slot.is_some() {
            *slot = None;
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Kernel DATA exports (variables the game reads as memory) + the system clock.
//
// DATA exports (KeTickCount, object-type pointers, Xbox*Info, ...) are NOT
// callable: the game reads them as memory through their import thunk. So their
// thunk must point at a real backing variable in RAM, not a call stub. We back
// each with a small block and tick KeTickCount every frame so timed waits
// (e.g. "spin until KeTickCount advances N") complete.
// ---------------------------------------------------------------------------

static KDATA: Mutex<Option<std::collections::HashMap<u32, u32>>> = Mutex::new(None);
const ORD_KE_TICK_COUNT: u32 = 156;
const ORD_XBOX_KRNL_VERSION: u32 = 324;
/// KeTickCount units (~ms) advanced per emulated frame (~60 Hz → ~16 ms).
const KTICK_PER_FRAME: u32 = 16;

/// Return (lazily allocating + initializing) the guest address of a DATA
/// export's backing variable. The loader patches the import thunk to this.
pub fn data_export_addr(ordinal: u32, mem: &mut Mem) -> u32 {
    let mut g = KDATA.lock().unwrap();
    let map = g.get_or_insert_with(std::collections::HashMap::new);
    if let Some(&a) = map.get(&ordinal) {
        return a;
    }
    let addr = heap_alloc(64, 16); // room for scalars or a small struct
    map.insert(ordinal, addr);
    // Sensible initial contents.
    match ordinal {
        ORD_XBOX_KRNL_VERSION => {
            // XBOX_KRNL_VERSION { Major, Minor, Build, Qfe } — kernel 1.0.5838.1.
            mem.ram_write16(addr, 1);
            mem.ram_write16(addr.wrapping_add(2), 0);
            mem.ram_write16(addr.wrapping_add(4), 5838);
            mem.ram_write16(addr.wrapping_add(6), 1);
        }
        _ => {
            for i in 0..16 {
                mem.ram_write32(addr.wrapping_add(i * 4), 0);
            }
        }
    }
    addr
}

/// Advance the system tick count (call once per emulated frame).
pub fn tick_clock(mem: &mut Mem) {
    let g = KDATA.lock().unwrap();
    if let Some(map) = g.as_ref() {
        if let Some(&addr) = map.get(&ORD_KE_TICK_COUNT) {
            let v = mem.ram_read32(addr).wrapping_add(KTICK_PER_FRAME);
            mem.ram_write32(addr, v);
        }
    }
}

/// Reset all HLE global state (allocator, scheduler, ISR, filesystem, kernel
/// data). Called when a new game boots so stale state from a prior run (these
/// are process-globals) doesn't leak across loads.
pub fn reset() {
    NEXT_HEAP.store(HEAP_BASE, Ordering::SeqCst);
    *SCHED.lock().unwrap() = None;
    *CONNECTED_ISR.lock().unwrap() = None;
    *KDATA.lock().unwrap() = None;
    FILES.lock().unwrap().clear();
    DISC.lock().unwrap().clear();
    REBOOT.store(false, Ordering::SeqCst);
    *PERSISTED.lock().unwrap() = None;
    *LAUNCH_DATA.lock().unwrap() = None;
}

// ---------------------------------------------------------------------------
// Quick-reboot / relaunch (XLaunchNewImage).
//
// A game can persist a launch-data page (MmPersistContiguousMemory) and call
// HalReturnToFirmware(QuickReboot). The real kernel reboots, preserves that
// page, re-exposes it through the LaunchDataPage DATA export, and re-launches
// default.xbe — on the second boot the game sees the page and proceeds instead
// of rebooting again. We emulate exactly that handoff.
// ---------------------------------------------------------------------------

static REBOOT: AtomicBool = AtomicBool::new(false);
/// Regions marked to survive a quick-reboot: (base, size).
static PERSISTED: Mutex<Option<Vec<(u32, u32)>>> = Mutex::new(None);
/// Base of the most-recently persisted page — the launch-data page.
static LAUNCH_DATA: Mutex<Option<u32>> = Mutex::new(None);
const ORD_LAUNCH_DATA_PAGE: u32 = 164;

/// Consume a pending reboot request (set by HalReturnToFirmware).
pub fn take_reboot() -> bool {
    REBOOT.swap(false, Ordering::SeqCst)
}

/// Move the mounted disc image out of the kernel (so it survives reset()).
pub fn take_disc() -> Vec<u8> {
    std::mem::take(&mut *DISC.lock().unwrap())
}

/// Read the launch-data page base recorded before a reboot.
pub fn take_launch_data() -> Option<u32> {
    *LAUNCH_DATA.lock().unwrap()
}

/// Capture the bytes of all persisted regions (call before reset wipes RAM).
pub fn snapshot_persisted(mem: &Mem) -> Vec<(u32, Vec<u8>)> {
    let g = PERSISTED.lock().unwrap();
    let mut out = Vec::new();
    if let Some(list) = g.as_ref() {
        for &(base, size) in list {
            let mut bytes = Vec::with_capacity(size as usize);
            for i in 0..size {
                bytes.push(mem.ram_read8(base.wrapping_add(i)) as u8);
            }
            out.push((base, bytes));
        }
    }
    out
}

/// Write the captured persisted bytes back into RAM (after a re-boot).
pub fn restore_persisted(mem: &mut Mem, snap: Vec<(u32, Vec<u8>)>) {
    for (base, bytes) in snap {
        for (i, b) in bytes.iter().enumerate() {
            mem.ram_write8(base.wrapping_add(i as u32), *b as u32);
        }
    }
}

/// Point the LaunchDataPage DATA export at the preserved launch page so the
/// relaunched image detects it was launched (rather than cold-booted).
pub fn set_launch_data_slot(mem: &mut Mem, page: u32) {
    let slot = data_export_addr(ORD_LAUNCH_DATA_PAGE, mem);
    mem.ram_write32(slot, page);
    *LAUNCH_DATA.lock().unwrap() = Some(page);
}

// ---------------------------------------------------------------------------
// Cooperative thread scheduler.
//
// The Xbox is multi-threaded (a main thread plus asset-loader / audio threads).
// We run all guest threads on the one interpreter via round-robin preemption
// plus real blocking on kernel events: a thread that waits on an unsignalled
// object yields to another, and is woken when the object is signalled. Without
// this, a game's main thread waits forever on loader threads that never run.
//
// LIMITATION: the x87/SSE state lives in process thread-locals, so it is NOT
// switched per guest thread yet — integer/segment/control state is. Good enough
// to get past the asset-load barrier; FP-heavy cross-thread state may be racy.
// ---------------------------------------------------------------------------

/// A saved integer/segment/control CPU context for a guest thread.
#[derive(Clone)]
struct ThreadCtx {
    regs: [u32; 8],
    eip: u32,
    eflags: u32,
    seg_sel: [u16; 6],
    seg_base: [u32; 6],
    cr: [u32; 5],
    halted: bool,
}

#[derive(Clone, PartialEq)]
enum TState {
    Ready,
    Blocked(u32), // waiting on this object key (event ptr)
    Terminated,
}

struct Thread {
    ctx: ThreadCtx,
    state: TState,
}

struct Sched {
    threads: Vec<Thread>,
    current: usize,
    started: bool,
    signaled: std::collections::HashSet<u32>,
    /// Saved context(s) of code interrupted by a delivered ISR (the ISR runs on
    /// the interrupted thread's stack; we restore on its return).
    isr_saved: Vec<ThreadCtx>,
}

static SCHED: Mutex<Option<Sched>> = Mutex::new(None);

/// The connected device ISR: (ServiceRoutine, ServiceContext), captured from
/// KeInitializeInterrupt. We deliver it on each vblank.
static CONNECTED_ISR: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// Return address pushed under a delivered ISR; when EIP reaches it the
/// orchestrator restores the interrupted context.
pub const ISR_RETURN_SENTINEL: u32 = 0xDEAD_0004;

/// Capture the device interrupt's service routine + context (KeInitializeInterrupt).
pub fn set_isr(routine: u32, context: u32) {
    *CONNECTED_ISR.lock().unwrap() = Some((routine, context));
}

/// Deliver the connected ISR (called once per vblank): save the current context
/// and set the CPU up to call `ServiceRoutine(Interrupt, ServiceContext)`. No-op
/// if no ISR is connected or one is already running. Returns true if delivered.
pub fn deliver_isr(cpu: &mut Cpu, mem: &mut Mem) -> bool {
    let (routine, context) = match *CONNECTED_ISR.lock().unwrap() {
        Some(x) => x,
        None => return false,
    };
    // Don't deliver a fresh ISR if a delivered call is still running.
    {
        let g = SCHED.lock().unwrap();
        if g.as_ref().map_or(true, |s| !s.isr_saved.is_empty()) {
            return false;
        }
    }
    // ServiceRoutine(Interrupt, ServiceContext).
    deliver_call(cpu, mem, routine, &[0, context])
}

/// Set the CPU up to call a guest routine `routine(args...)` (stdcall) on top of
/// the current context, saving the interrupted context so [`isr_return`] can
/// restore it when the routine returns to the sentinel. Used for ISR + DPC
/// delivery. Returns false if `routine` is null or nesting is too deep.
pub fn deliver_call(cpu: &mut Cpu, mem: &mut Mem, routine: u32, args: &[u32]) -> bool {
    if routine == 0 {
        return false;
    }
    let mut g = SCHED.lock().unwrap();
    let s = match g.as_mut() {
        Some(s) => s,
        None => return false,
    };
    if s.isr_saved.len() > 8 {
        return false; // bound nesting
    }
    if std::env::var_os("XBOX_TRACE_ISR").is_some() {
        eprintln!("[isr] deliver routine={routine:#010X}");
    }
    s.isr_saved.push(capture(cpu));
    let mut sp = cpu.reg32(ESP);
    for &a in args.iter().rev() {
        sp = sp.wrapping_sub(4);
        mem.ram_write32(sp, a);
    }
    sp = sp.wrapping_sub(4);
    mem.ram_write32(sp, ISR_RETURN_SENTINEL);
    cpu.set_reg32(ESP, sp);
    cpu.eip = routine;
    true
}

/// Restore the context interrupted by a delivered ISR (EIP hit the sentinel).
pub fn isr_return(cpu: &mut Cpu) {
    if std::env::var_os("XBOX_TRACE_ISR").is_some() {
        eprintln!("[isr] return");
    }
    let mut g = SCHED.lock().unwrap();
    if let Some(s) = g.as_mut() {
        if let Some(ctx) = s.isr_saved.pop() {
            apply(cpu, &ctx);
        }
    }
}

fn capture(cpu: &Cpu) -> ThreadCtx {
    ThreadCtx {
        regs: cpu.regs,
        eip: cpu.eip,
        eflags: cpu.eflags,
        seg_sel: cpu.seg_sel,
        seg_base: cpu.seg_base,
        cr: cpu.cr,
        halted: cpu.halted,
    }
}

fn apply(cpu: &mut Cpu, c: &ThreadCtx) {
    if c.eip < 0x0010_0000 && std::env::var_os("XBOX_TRACE_THREAD").is_some() {
        eprintln!("[thread] apply suspicious eip={:#010X} esp={:#X}", c.eip, c.regs[ESP]);
    }
    cpu.regs = c.regs;
    cpu.eip = c.eip;
    cpu.eflags = c.eflags;
    cpu.seg_sel = c.seg_sel;
    cpu.seg_base = c.seg_base;
    cpu.cr = c.cr;
    cpu.halted = c.halted;
    cpu.fault = None;
}

/// Pick the next Ready thread after `current` (round-robin). Returns its index.
fn pick_next(s: &Sched) -> Option<usize> {
    let n = s.threads.len();
    for off in 1..=n {
        let i = (s.current + off) % n;
        if s.threads[i].state == TState::Ready {
            return Some(i);
        }
    }
    if s.threads[s.current].state == TState::Ready {
        return Some(s.current);
    }
    None
}

/// Create a guest thread (PsCreateSystemThreadEx). Registers the creator as a
/// thread on first use, then enqueues the new thread Ready — execution stays
/// with the creator (no switch).
pub fn create_thread(cpu: &mut Cpu, mem: &mut Mem, entry: u32, ctx1: u32, ctx2: u32) {
    let mut g = SCHED.lock().unwrap();
    let s = g.get_or_insert_with(|| Sched {
        threads: Vec::new(),
        current: 0,
        started: false,
        signaled: std::collections::HashSet::new(),
        isr_saved: Vec::new(),
    });
    if !s.started {
        s.threads.push(Thread {
            ctx: capture(cpu),
            state: TState::Ready,
        });
        s.current = 0;
        s.started = true;
    }
    let stack_size = 0x0004_0000u32;
    let base = heap_alloc(stack_size, PAGE);
    let mut sp = base.wrapping_add(stack_size);
    sp = sp.wrapping_sub(4);
    mem.ram_write32(sp, ctx2);
    sp = sp.wrapping_sub(4);
    mem.ram_write32(sp, ctx1);
    sp = sp.wrapping_sub(4);
    mem.ram_write32(sp, THREAD_EXIT_SENTINEL);
    let mut ctx = capture(cpu); // inherit flat protected-mode segments
    ctx.eip = entry;
    ctx.regs[ESP] = sp;
    ctx.halted = false;
    if std::env::var_os("XBOX_TRACE_THREAD").is_some() {
        eprintln!(
            "[thread] create #{} entry={entry:#010X} ctx1={ctx1:#X} sp={sp:#X}",
            s.threads.len()
        );
    }
    s.threads.push(Thread {
        ctx,
        state: TState::Ready,
    });
}

/// Block the current thread on `obj` and switch to the next ready thread. If
/// `obj` is already signalled, consume it and return without blocking.
pub fn wait_object(cpu: &mut Cpu, obj: u32) {
    let mut g = SCHED.lock().unwrap();
    let s = match g.as_mut() {
        Some(s) => s,
        None => return,
    };
    if s.signaled.remove(&obj) {
        return; // already signalled
    }
    s.threads[s.current].ctx = capture(cpu);
    s.threads[s.current].state = TState::Blocked(obj);
    if let Some(next) = pick_next(s) {
        s.current = next;
        let ctx = s.threads[next].ctx.clone();
        apply(cpu, &ctx);
    } else {
        // Nothing else to run: unblock self to avoid a hard hang.
        s.threads[s.current].state = TState::Ready;
    }
}

/// Signal an object: mark it signalled and wake any threads blocked on it.
pub fn signal_object(obj: u32) {
    let mut g = SCHED.lock().unwrap();
    if let Some(s) = g.as_mut() {
        s.signaled.insert(obj);
        for t in &mut s.threads {
            if t.state == TState::Blocked(obj) {
                t.state = TState::Ready;
            }
        }
    }
}

/// Clear an object's signalled state.
pub fn reset_object(obj: u32) {
    let mut g = SCHED.lock().unwrap();
    if let Some(s) = g.as_mut() {
        s.signaled.remove(&obj);
    }
}

/// Voluntarily yield / round-robin preempt: save the current thread (Ready) and
/// run the next ready one.
pub fn preempt(cpu: &mut Cpu) {
    let mut g = SCHED.lock().unwrap();
    let s = match g.as_mut() {
        Some(s) if s.threads.len() > 1 => s,
        _ => return,
    };
    s.threads[s.current].ctx = capture(cpu);
    if s.threads[s.current].state == TState::Blocked(0) {
        // never; placeholder
    }
    let cur = s.current;
    if s.threads[cur].state != TState::Terminated {
        s.threads[cur].state = TState::Ready;
    }
    if let Some(next) = pick_next(s) {
        if next != cur {
            s.current = next;
            let ctx = s.threads[next].ctx.clone();
            apply(cpu, &ctx);
        }
    }
}

/// Terminate the current thread and switch away.
pub fn terminate_current(cpu: &mut Cpu) {
    let mut g = SCHED.lock().unwrap();
    let s = match g.as_mut() {
        Some(s) => s,
        None => {
            cpu.halted = true;
            return;
        }
    };
    s.threads[s.current].state = TState::Terminated;
    if let Some(next) = pick_next(s) {
        s.current = next;
        let ctx = s.threads[next].ctx.clone();
        apply(cpu, &ctx);
    } else {
        cpu.halted = true; // last thread done
    }
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

/// Read the path out of an Xbox OBJECT_ATTRIBUTES at guest `oa`:
/// `{ HANDLE RootDirectory; POBJECT_STRING ObjectName; ULONG Attributes; }`,
/// where OBJECT_STRING is `{ USHORT Length; USHORT MaximumLength; PCHAR Buffer; }`
/// (ANSI on the Xbox).
fn read_obj_path(mem: &Mem, oa: u32) -> String {
    if oa == 0 {
        return String::new();
    }
    let name = mem.ram_read32(oa.wrapping_add(4));
    if name == 0 {
        return String::new();
    }
    let len = mem.ram_read16(name) & 0xFFFF;
    let buf = mem.ram_read32(name.wrapping_add(4));
    let mut s = String::with_capacity(len as usize);
    for i in 0..len.min(512) {
        s.push(mem.ram_read8(buf.wrapping_add(i)) as u8 as char);
    }
    s
}

// ---------------------------------------------------------------------------
// Ordinal constants (cross-checked against xboxkrnl.exe.def / OpenXDK).
// ---------------------------------------------------------------------------

const ORD_DBG_PRINT: u32 = 8;
const ORD_KE_INITIALIZE_DPC: u32 = 107;
const ORD_KE_INSERT_QUEUE_DPC: u32 = 119;
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
const ORD_PS_TERMINATE_SYSTEM_THREAD: u32 = 258;
const ORD_KE_INITIALIZE_EVENT: u32 = 108;
const ORD_KE_SET_EVENT: u32 = 145;
const ORD_KE_RESET_EVENT: u32 = 138;
const ORD_KE_PULSE_EVENT: u32 = 123;
const ORD_KE_WAIT_FOR_SINGLE_OBJECT: u32 = 159;
const ORD_KE_WAIT_FOR_MULTIPLE_OBJECTS: u32 = 158;
const ORD_KE_DELAY_EXECUTION_THREAD: u32 = 99;
const ORD_NT_YIELD_EXECUTION: u32 = 238;
const ORD_NT_WAIT_FOR_SINGLE_OBJECT: u32 = 233;
const ORD_NT_WAIT_FOR_SINGLE_OBJECT_EX: u32 = 234;
const ORD_NT_OPEN_FILE: u32 = 202;
const ORD_NT_CREATE_FILE: u32 = 190;
const ORD_NT_READ_FILE: u32 = 219;
const ORD_NT_CLOSE: u32 = 187;
const ORD_HAL_GET_INTERRUPT_VECTOR: u32 = 44;
const ORD_KE_INITIALIZE_INTERRUPT: u32 = 109;
const ORD_KE_CONNECT_INTERRUPT: u32 = 98;
const ORD_MM_CLAIM_GPU_INSTANCE_MEMORY: u32 = 168;
const ORD_HAL_READ_WRITE_PCI_SPACE: u32 = 46;
const ORD_HAL_RETURN_TO_FIRMWARE: u32 = 49;
const ORD_AV_SEND_TV_ENCODER_OPTION: u32 = 2;
const ORD_AV_SET_DISPLAY_MODE: u32 = 3;

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
    100, // KeDisconnectInterrupt
    1,   // AvGetSavedDataAddress (return 0 = none)
    4,   // AvSetSavedDataAddress
    // (interrupt connect/init, events, waits, threads, Av display handled below)
];
/// Return address pushed under a new thread's entry: if the thread ever returns,
/// EIP lands here (recognizable, and out of mapped code) so it stops cleanly.
pub const THREAD_EXIT_SENTINEL: u32 = 0xDEAD_0000;

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
            // VOID MmPersistContiguousMemory(PVOID Base, ULONG Size, BOOLEAN
            // Persist) — mark a region to survive a quick-reboot (the launch-data
            // page). Record it so the reboot path preserves + exposes it.
            let base = arg(cpu, mem, 0);
            let size = arg(cpu, mem, 1);
            let persist = arg(cpu, mem, 2);
            if persist != 0 {
                let mut g = PERSISTED.lock().unwrap();
                g.get_or_insert_with(Vec::new).push((base, size));
                *LAUNCH_DATA.lock().unwrap() = Some(base);
            }
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
            // VOID KeInitializeDpc(PKDPC Dpc, PKDEFERRED_ROUTINE, PVOID Context).
            // Store routine/context in the KDPC (DeferredRoutine @ +0x0C,
            // DeferredContext @ +0x10) so KeInsertQueueDpc can run it.
            let dpc = arg(cpu, mem, 0);
            let routine = arg(cpu, mem, 1);
            let context = arg(cpu, mem, 2);
            if dpc != 0 {
                mem.ram_write32(dpc.wrapping_add(0x0C), routine);
                mem.ram_write32(dpc.wrapping_add(0x10), context);
            }
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("KeInitializeDpc")
        }
        ORD_KE_INSERT_QUEUE_DPC => {
            // BOOLEAN KeInsertQueueDpc(Dpc, SystemArgument1, SystemArgument2).
            // Run the DPC routine now (DeferredRoutine(Dpc, Context, Arg1, Arg2)):
            // return TRUE to the caller, then deliver the deferred call.
            let dpc = arg(cpu, mem, 0);
            let a1 = arg(cpu, mem, 1);
            let a2 = arg(cpu, mem, 2);
            let routine = mem.ram_read32(dpc.wrapping_add(0x0C));
            let context = mem.ram_read32(dpc.wrapping_add(0x10));
            stdcall_return(cpu, mem, 1, 12);
            deliver_call(cpu, mem, routine, &[dpc, context, a1, a2]);
            Dispatch::Handled("KeInsertQueueDpc")
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

        // ---- Files (XISO-backed virtual filesystem) ----
        ORD_NT_OPEN_FILE | ORD_NT_CREATE_FILE => {
            // NtOpenFile(PHANDLE FileHandle, ACCESS_MASK, POBJECT_ATTRIBUTES,
            //   PIO_STATUS_BLOCK, ULONG ShareAccess). NtCreateFile has more args
            // but OBJECT_ATTRIBUTES (arg2) + IoStatusBlock (arg3) line up.
            let fh_out = arg(cpu, mem, 0);
            let oa = arg(cpu, mem, 2);
            let iosb = arg(cpu, mem, 3);
            let path = read_obj_path(mem, oa);
            let (status, handle, info) = open_file(&path);
            if fh_out != 0 {
                mem.ram_write32(fh_out, handle);
            }
            if iosb != 0 {
                mem.ram_write32(iosb, status); // IoStatusBlock.Status
                mem.ram_write32(iosb.wrapping_add(4), info); // .Information
            }
            let argbytes = hle_table::lookup(ordinal).map(|(_, b)| b as u32).unwrap_or(20);
            stdcall_return(cpu, mem, status, argbytes);
            Dispatch::Handled(if ordinal == ORD_NT_OPEN_FILE { "NtOpenFile" } else { "NtCreateFile" })
        }
        ORD_NT_READ_FILE => {
            // NtReadFile(HANDLE, HANDLE Event, PIO_APC_ROUTINE, PVOID ApcCtx,
            //   PIO_STATUS_BLOCK, PVOID Buffer, ULONG Length, PLARGE_INTEGER
            //   ByteOffset) — 32 bytes.
            let h = arg(cpu, mem, 0);
            let iosb = arg(cpu, mem, 4);
            let buf = arg(cpu, mem, 5);
            let len = arg(cpu, mem, 6);
            let byteoff_ptr = arg(cpu, mem, 7);
            let off = if byteoff_ptr != 0 {
                Some(mem.ram_read32(byteoff_ptr)) // low 32 bits of the LARGE_INTEGER
            } else {
                None
            };
            let (status, info) = read_file(mem, h, buf, len, off);
            if iosb != 0 {
                mem.ram_write32(iosb, status);
                mem.ram_write32(iosb.wrapping_add(4), info);
            }
            stdcall_return(cpu, mem, status, 32);
            Dispatch::Handled("NtReadFile")
        }
        ORD_NT_CLOSE => {
            let h = arg(cpu, mem, 0);
            close_file(h);
            stdcall_return(cpu, mem, STATUS_SUCCESS, 4);
            Dispatch::Handled("NtClose")
        }

        // ---- Reboot / firmware ----
        ORD_HAL_RETURN_TO_FIRMWARE => {
            if std::env::var("XBOX_TRACE_KERNEL").is_ok() {
                if let Some(page) = *LAUNCH_DATA.lock().unwrap() {
                    let ty = mem.ram_read32(page);
                    let tid = mem.ram_read32(page.wrapping_add(4));
                    let mut path = String::new();
                    for i in 0..520u32 {
                        let b = mem.ram_read8(page.wrapping_add(8 + i)) as u8;
                        if b == 0 { break; }
                        path.push(b as char);
                    }
                    let flags = mem.ram_read32(page.wrapping_add(528));
                    eprintln!("[hle] reboot: launchType={ty} titleId={tid:08X} flags={flags:08X} path={path:?}");
                } else {
                    eprintln!("[hle] reboot: no launch-data page persisted");
                }
            }
            // The game persisted launch data and asked to reboot+relaunch
            // (XLaunchNewImage). Signal the orchestrator to re-boot; halt this
            // thread so nothing runs until it does.
            REBOOT.store(true, Ordering::SeqCst);
            cpu.halted = true;
            Dispatch::Handled("HalReturnToFirmware")
        }

        // ---- Display / TV encoder (Av*) ----
        ORD_AV_SEND_TV_ENCODER_OPTION => {
            // AvSendTVEncoderOption(RegisterBase, Option, Param, ULONG *Result).
            // No encoder modeled; report success and a zero result.
            let result = arg(cpu, mem, 3);
            if result != 0 {
                mem.ram_write32(result, 0);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 16);
            Dispatch::Handled("AvSendTVEncoderOption")
        }
        ORD_AV_SET_DISPLAY_MODE => {
            // AvSetDisplayMode(RegisterBase, Step, Mode, Format, Pitch, FrameBuffer).
            // The display mode is a no-op for us — the game renders into a PGRAPH
            // surface we already scan out. Report success.
            stdcall_return(cpu, mem, STATUS_SUCCESS, 24);
            Dispatch::Handled("AvSetDisplayMode")
        }

        // ---- PCI config space ----
        ORD_HAL_READ_WRITE_PCI_SPACE => {
            // HalReadWritePCISpace(Bus, Slot, RegisterNumber, Buffer, Length,
            //   WritePCISpace). On read, return the NV2A's config (so the GPU is
            //   mapped at its fixed addresses); writes are accepted (ignored).
            let reg = arg(cpu, mem, 2);
            let buffer = arg(cpu, mem, 3);
            let length = arg(cpu, mem, 4);
            let write = arg(cpu, mem, 5);
            if write == 0 && buffer != 0 {
                let val: u32 = match reg & !3 {
                    0x00 => 0x02A0_10DE, // vendor 10DE (NVIDIA) / device 02A0 (NV2A)
                    0x10 => 0xFD00_0000, // BAR0: NV2A register block
                    0x14 => 0xF000_0000, // BAR1: framebuffer / AGP aperture
                    _ => 0,
                };
                for i in 0..length.min(4) {
                    mem.ram_write8(buffer.wrapping_add(i), (val >> (i * 8)) & 0xFF);
                }
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 24);
            Dispatch::Handled("HalReadWritePCISpace")
        }

        // ---- GPU instance memory ----
        ORD_MM_CLAIM_GPU_INSTANCE_MEMORY => {
            // PVOID MmClaimGpuInstanceMemory(SIZE_T NumberOfBytes,
            //   SIZE_T *NumberOfPaddingBytes). Return the top of contiguous RAM
            //   (the GPU instance memory sits at the end); no padding.
            let pad_out = arg(cpu, mem, 1);
            if pad_out != 0 {
                mem.ram_write32(pad_out, 0);
            }
            let top = crate::regions::RAM_SIZE as u32; // 0x0400_0000 (64 MB)
            stdcall_return(cpu, mem, top, 8);
            Dispatch::Handled("MmClaimGpuInstanceMemory")
        }

        // ---- Interrupts ----
        ORD_KE_INITIALIZE_INTERRUPT => {
            // KeInitializeInterrupt(Interrupt, ServiceRoutine, ServiceContext,
            //   Vector, Irql, Mode, ShareVector) — capture the service routine so
            //   we can deliver it on vblank; also fill the KINTERRUPT struct.
            let kint = arg(cpu, mem, 0);
            let routine = arg(cpu, mem, 1);
            let context = arg(cpu, mem, 2);
            if kint != 0 {
                mem.ram_write32(kint, routine);
                mem.ram_write32(kint.wrapping_add(4), context);
            }
            set_isr(routine, context);
            stdcall_return(cpu, mem, STATUS_SUCCESS, 28);
            Dispatch::Handled("KeInitializeInterrupt")
        }
        ORD_KE_CONNECT_INTERRUPT => {
            // KeConnectInterrupt(Interrupt) — the ISR was captured at init; just
            // report success (TRUE). Delivery happens each vblank.
            stdcall_return(cpu, mem, 1, 4);
            Dispatch::Handled("KeConnectInterrupt")
        }

        // ---- Interrupts (stub: echo the bus level as the vector) ----
        ORD_HAL_GET_INTERRUPT_VECTOR => {
            // ULONG HalGetInterruptVector(ULONG BusInterruptLevel, PKIRQL Irql).
            let level = arg(cpu, mem, 0);
            let irql_out = arg(cpu, mem, 1);
            if irql_out != 0 {
                mem.ram_write32(irql_out, level); // plausible IRQL
            }
            stdcall_return(cpu, mem, level, 8);
            Dispatch::Handled("HalGetInterruptVector")
        }

        // ---- Threads + synchronization (cooperative scheduler) ----
        ORD_PS_CREATE_SYSTEM_THREAD_EX => {
            // PsCreateSystemThreadEx(PHANDLE, ExtraSize, KernelStackSize,
            //   TlsDataSize, PULONG ThreadId, Ctx1, Ctx2, CreateSuspended,
            //   DebuggerThread, StartRoutine) — 10 args. Enqueue the thread Ready;
            //   the scheduler runs it. Execution stays with the creator.
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
            // Return to the creator FIRST (so the creator thread is captured at
            // its real resume point, not the HLE trap stub), then enqueue the new
            // thread.
            stdcall_return(cpu, mem, STATUS_SUCCESS, 40);
            create_thread(cpu, mem, start, ctx1, ctx2);
            Dispatch::Handled("PsCreateSystemThreadEx")
        }
        ORD_PS_TERMINATE_SYSTEM_THREAD => {
            terminate_current(cpu);
            Dispatch::Handled("PsTerminateSystemThread")
        }
        ORD_KE_INITIALIZE_EVENT => {
            // KeInitializeEvent(Event, Type, SignalState) — start signalled if so.
            let ev = arg(cpu, mem, 0);
            let initial = arg(cpu, mem, 2);
            if initial != 0 {
                signal_object(ev);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 12);
            Dispatch::Handled("KeInitializeEvent")
        }
        ORD_KE_SET_EVENT => {
            // LONG KeSetEvent(Event, Increment, Wait) — wake waiters; return prev.
            signal_object(arg(cpu, mem, 0));
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("KeSetEvent")
        }
        ORD_KE_PULSE_EVENT => {
            let ev = arg(cpu, mem, 0);
            signal_object(ev);
            reset_object(ev);
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("KePulseEvent")
        }
        ORD_KE_RESET_EVENT => {
            reset_object(arg(cpu, mem, 0));
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("KeResetEvent")
        }
        ORD_KE_WAIT_FOR_SINGLE_OBJECT => {
            // KeWaitForSingleObject(Object, WaitReason, WaitMode, Alertable,
            //   Timeout) — return WAIT_OBJECT_0, then block on the object.
            let obj = arg(cpu, mem, 0);
            stdcall_return(cpu, mem, STATUS_SUCCESS, 20);
            wait_object(cpu, obj);
            Dispatch::Handled("KeWaitForSingleObject")
        }
        ORD_KE_WAIT_FOR_MULTIPLE_OBJECTS => {
            // Approximate: don't block (return WAIT_OBJECT_0).
            stdcall_return(cpu, mem, STATUS_SUCCESS, 32);
            Dispatch::Handled("KeWaitForMultipleObjects")
        }
        ORD_KE_DELAY_EXECUTION_THREAD | ORD_NT_YIELD_EXECUTION => {
            let argbytes = hle_table::lookup(ordinal).map(|(_, b)| b as u32).unwrap_or(0);
            stdcall_return(cpu, mem, STATUS_SUCCESS, argbytes);
            preempt(cpu); // yield to another thread
            Dispatch::Handled("Yield")
        }
        ORD_NT_WAIT_FOR_SINGLE_OBJECT | ORD_NT_WAIT_FOR_SINGLE_OBJECT_EX => {
            // Handle-based waits: don't block (return signalled).
            let argbytes = hle_table::lookup(ordinal).map(|(_, b)| b as u32).unwrap_or(12);
            stdcall_return(cpu, mem, STATUS_SUCCESS, argbytes);
            Dispatch::Handled("NtWaitForSingleObject")
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
