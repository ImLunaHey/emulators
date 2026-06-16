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

use crate::cpu::state::{EAX, ECX, EDX, ESP};
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

/// Dedicated arena for kernel DATA-export backing variables (KeTickCount,
/// XboxKrnlVersion, LaunchDataPage, …). These MUST NOT come from the
/// game-visible contiguous heap (`NEXT_HEAP`): that bump allocator is
/// deterministic, so the addresses it hands the *game* are byte-identical on a
/// warm reboot — and a kernel scratch cell placed there collides with a game
/// allocation. Concretely, Halo's launcher keeps its launch-data pointer in a
/// heap cell that lands at 0x0200_3000; if the kernel writes the persisted page
/// pointer into that same cell (which happened when `data_export_addr` shared
/// the heap), the warm-booted launcher sees its launch data "already present"
/// and reboots forever. This arena sits in the 1 MB gap just below the heap,
/// above any loaded XBE image (which ends well under 2 MB), so it can never
/// alias either the game's image or its allocations.
const KDATA_BASE: u32 = 0x01F0_0000;
const KDATA_END: u32 = HEAP_BASE; // 0x0200_0000
static NEXT_KDATA: AtomicU32 = AtomicU32::new(KDATA_BASE);

/// Bump-allocate a kernel DATA-export backing block from the reserved kernel
/// arena (see [`KDATA_BASE`]). Falls back to the game heap only if exhausted.
fn kdata_alloc(size: u32, align: u32) -> u32 {
    let align = align.max(16);
    let size = align_up(size.max(1), 16);
    loop {
        let cur = NEXT_KDATA.load(Ordering::Relaxed);
        let base = align_up(cur, align);
        match base.checked_add(size) {
            Some(end) if end <= KDATA_END => {
                if NEXT_KDATA
                    .compare_exchange(cur, end, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    return base;
                }
            }
            _ => return heap_alloc(size, align), // arena full: degrade gracefully
        }
    }
}

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
/// Distinct handles for kernel sync objects (mutant/semaphore). Each create
/// hands back a unique handle in a range clear of file handles; the cooperative
/// scheduler never contends them, so they're acquirable immediately (waits on a
/// handle return signalled — see NtWaitForSingleObject).
const SYNC_HANDLE_BASE: u32 = 0x2000_0000;
static SYNC_NEXT: AtomicU32 = AtomicU32::new(SYNC_HANDLE_BASE);
/// Backing KTHREAD block returned by KeGetCurrentThread (lazily allocated).
static CURRENT_KTHREAD: AtomicU32 = AtomicU32::new(0);
/// Open symbolic-link handles → the link's source path (so a later
/// NtQuerySymbolicLinkObject can report the device it resolves to).
static SYMLINKS: Mutex<Option<std::collections::HashMap<u32, String>>> = Mutex::new(None);

/// The device path an Xbox drive-letter symlink resolves to.
fn symlink_target(src: &str) -> &'static str {
    let s = src.to_ascii_uppercase();
    if s.contains("C:") {
        "\\Device\\Harddisk0\\Partition2"
    } else if s.contains("T:") || s.contains("U:") {
        "\\Device\\Harddisk0\\Partition1"
    } else {
        "\\Device\\CdRom0" // D: and anything else: the game disc
    }
}
fn alloc_sync_handle() -> u32 {
    SYNC_NEXT.fetch_add(4, Ordering::SeqCst)
}

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

/// True if the path refers to the (stubbed) hard disk rather than the game disc.
/// Halo 2's launcher mounts/opens `\Device\Harddisk0\partition1\` (and the T:/U:
/// title/save partitions) during boot; a real console always has an HDD, so
/// failing the open makes the title relaunch (reboot). We present an empty HDD.
fn is_hdd_path(p: &str) -> bool {
    let low = p.replace('\\', "/").to_ascii_lowercase();
    let low = low.trim_start_matches('/');
    low.starts_with("device/harddisk")
        || low.starts_with("??/t:")
        || low.starts_with("??/u:")
        || low.starts_with("??/y:")
        || low.starts_with("??/z:")
        || low.starts_with("t:")
        || low.starts_with("u:")
}

/// True if an HDD path's final component looks like a concrete file (has a `.`
/// extension), as opposed to a directory/partition. On a fresh HDD such files
/// don't exist, so opening them must fail.
fn hdd_path_is_file(p: &str) -> bool {
    let s = p.replace('\\', "/");
    let last = s.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    // A drive/partition root like "partition1" or a hex title dir has no '.'.
    last.contains('.')
}

/// Sentinel `FileHandle.offset` marking a hard-disk pseudo-handle (no backing
/// disc bytes; reads return zero-filled success).
const HDD_PSEUDO_OFFSET: usize = usize::MAX;

/// True if an HDD path is dashboard-managed title METADATA in the title's
/// `UDATA\<titleid>\` directory (`TitleMeta.xbx` — title display name/settings,
/// `TitleImage.xbx`/`SaveImage.xbx` — the title's thumbnail images,
/// `SaveMeta.xbx`) rather than per-save gameplay payload. The dashboard creates
/// this UDATA metadata when a title is first registered, so on a real console it
/// exists before the game ever runs. We boot the XBE directly (no dashboard), so
/// it was never created; reporting it absent makes a launcher that treats
/// "title metadata missing" as "first-time setup, reboot to regenerate it" loop
/// forever, since our HDD doesn't persist the write — this is exactly the Halo
/// CE / Halo 2 reboot loop. Present this metadata as existing-but-empty so the
/// launcher proceeds to the game.
///
/// Per-save gameplay files live in save-slot subdirectories (not `UDATA\<id>\`
/// directly) and are intentionally NOT matched here, so a fresh console still
/// has no actual saves.
fn is_title_metadata_file(p: &str) -> bool {
    let s = p.replace('\\', "/").to_ascii_lowercase();
    // Must sit directly in the user-data area for a title.
    if !s.contains("/udata/") {
        return false;
    }
    let last = s.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    matches!(
        last,
        "titlemeta.xbx" | "titleimage.xbx" | "saveimage.xbx" | "savemeta.xbx"
    )
}

/// Open a file by Xbox path. Returns `(status, handle, information)`.
fn open_file(path: &str) -> (u32, u32, u32) {
    let r = open_file_inner(path);
    if std::env::var_os("XBOX_TRACE_FS").is_some() {
        eprintln!("[fs] open {path:?} -> status={:08X}", r.0);
    }
    r
}

fn open_file_inner(path: &str) -> (u32, u32, u32) {
    // Hard-disk paths: present an empty-but-present HDD so the launcher's
    // partition/directory opens succeed and it doesn't relaunch (reboot to
    // dashboard). But a fresh console has NO save data, so opening a concrete
    // *file* (e.g. a save's TitleMeta.xbx / SaveImage.xbx) must report
    // NOT_FOUND — returning a zero-filled success makes the game parse junk as a
    // valid save and jump through a garbage pointer. We treat any HDD path whose
    // final component has a file extension as a non-existent file.
    if is_hdd_path(path) {
        if hdd_path_is_file(path) {
            // A concrete file on the (otherwise empty) HDD. Two cases:
            //
            //  * Title METADATA under UDATA\<titleid>\ (TitleMeta.xbx, TitleImage
            //    .xbx, SaveMeta.xbx) is written by the DASHBOARD when a title is
            //    first registered — on a real console it already exists before the
            //    game ever runs. We boot the XBE directly (no dashboard), so these
            //    never got created; reporting them absent makes a launcher that
            //    treats "metadata missing" as "first-time setup" reboot to
            //    regenerate it — forever, since our HDD doesn't persist the write.
            //    (This is exactly Halo CE/2's reboot loop.) Present them as
            //    existing-but-empty so the launcher proceeds to the game.
            //
            //  * Actual SAVE data (SaveImage.xbx and per-slot save files) must stay
            //    absent: a fresh console has no saves, and returning a zero-filled
            //    "save" would make the game parse junk as a valid savegame.
            if is_title_metadata_file(path) {
                let mut files = FILES.lock().unwrap();
                files.push(Some(FileHandle { offset: HDD_PSEUDO_OFFSET, size: 0x4000, pos: 0 }));
                return (0, FILE_HANDLE_BASE + (files.len() as u32 - 1), FILE_OPENED);
            }
            return (STATUS_OBJECT_NAME_NOT_FOUND, 0, 0);
        }
        let mut files = FILES.lock().unwrap();
        files.push(Some(FileHandle { offset: HDD_PSEUDO_OFFSET, size: 0x4000, pos: 0 }));
        return (0, FILE_HANDLE_BASE + (files.len() as u32 - 1), FILE_OPENED);
    }
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
    // Hard-disk pseudo-handle: no backing disc bytes — return the requested
    // length as zero-filled success (an empty/blank partition).
    if fh.offset == HDD_PSEUDO_OFFSET {
        let n = len as usize;
        for i in 0..n {
            mem.ram_write8(buf.wrapping_add(i as u32), 0);
        }
        return (0, n as u32);
    }
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

/// Query an open handle's (size, position). Returns None for an invalid handle.
fn file_size_pos(h: u32) -> Option<(usize, usize)> {
    if h < FILE_HANDLE_BASE {
        return None;
    }
    let idx = (h - FILE_HANDLE_BASE) as usize;
    let files = FILES.lock().unwrap();
    files.get(idx).and_then(|s| s.as_ref()).map(|f| (f.size, f.pos))
}

/// Set an open handle's position (NtSetInformationFile / FilePositionInformation).
fn file_set_pos(h: u32, pos: usize) -> bool {
    if h < FILE_HANDLE_BASE {
        return false;
    }
    let idx = (h - FILE_HANDLE_BASE) as usize;
    let mut files = FILES.lock().unwrap();
    if let Some(f) = files.get_mut(idx).and_then(|s| s.as_mut()) {
        f.pos = pos;
        return true;
    }
    false
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
const ORD_KE_INTERRUPT_TIME: u32 = 120;
const ORD_KE_SYSTEM_TIME: u32 = 154;
const ORD_XBOX_KRNL_VERSION: u32 = 324;
/// `HalDiskCachePartitionCount` (DATA export, ordinal 40) — the number of HDD
/// cache partitions. On a real console this is non-zero (the X/Y/Z game-cache
/// partitions); the dashboard/kernel sets it at boot. During init Halo reads
/// this export as a count, `dec`s it, and uses `count*12` as a memcpy length, so
/// a zero here underflows to 0xFFFFFFFF and triggers a ~4-billion-dword
/// `rep movsd` (EIP 0x19505) that wipes the game's own loaded image, after which
/// EIP free-runs through zeroed RAM. Initialize it to the standard partition
/// count so the count stays positive and the memcpy is bounded.
const ORD_HAL_DISK_CACHE_PARTITION_COUNT: u32 = 40;
const HAL_DISK_CACHE_PARTITION_COUNT: u32 = 3;
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
    let addr = kdata_alloc(64, 16); // room for scalars or a small struct
    if std::env::var_os("XBOX_TRACE_KDATA").is_some() {
        eprintln!("[kdata] ordinal {ordinal} -> {addr:#010X}");
    }
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
        ORD_HAL_DISK_CACHE_PARTITION_COUNT => {
            mem.ram_write32(addr, HAL_DISK_CACHE_PARTITION_COUNT);
        }
        ORD_LAUNCH_DATA_PAGE => {
            // On a real console `LaunchDataPage` is a pointer the kernel sets to a
            // 4 KB launch-data page when a title is launched WITH data (via the
            // dashboard or XLaunchNewImage), and leaves NULL on a plain cold
            // boot. We leave the pointer NULL here; the reboot path
            // (set_launch_data_slot) fills it on a warm relaunch.
            mem.ram_write32(addr, 0);
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
    let map = match g.as_ref() {
        Some(m) => m,
        None => return,
    };
    // KeTickCount: milliseconds since boot (32-bit).
    if let Some(&addr) = map.get(&ORD_KE_TICK_COUNT) {
        let v = mem.ram_read32(addr).wrapping_add(KTICK_PER_FRAME);
        mem.ram_write32(addr, v);
    }
    // KeInterruptTime / KeSystemTime: 100 ns ticks (64-bit). Advancing these too
    // lets time-based delay loops (e.g. pbkit's encoder settle waits) make
    // progress across frames instead of spinning forever on a frozen clock.
    const HUNDRED_NS_PER_FRAME: u64 = KTICK_PER_FRAME as u64 * 10_000;
    for ord in [ORD_KE_INTERRUPT_TIME, ORD_KE_SYSTEM_TIME] {
        if let Some(&addr) = map.get(&ord) {
            let lo = mem.ram_read32(addr) as u64;
            let hi = mem.ram_read32(addr.wrapping_add(4)) as u64;
            let v = ((hi << 32) | lo).wrapping_add(HUNDRED_NS_PER_FRAME);
            mem.ram_write32(addr, v as u32);
            mem.ram_write32(addr.wrapping_add(4), (v >> 32) as u32);
        }
    }
}

/// Convert a Windows FILETIME (100 ns ticks since 1601-01-01) to NT TIME_FIELDS
/// components: (Year, Month, Day, Hour, Minute, Second, Milliseconds, Weekday).
/// Weekday is 0=Sunday. Uses Howard Hinnant's days→civil algorithm.
fn filetime_to_fields(t: u64) -> (i16, i16, i16, i16, i16, i16, i16, i16) {
    let ms_total = (t / 10_000) as i64; // ms since 1601-01-01
    let ms = (ms_total % 1000) as i16;
    let secs = ms_total / 1000;
    let second = (secs % 60) as i16;
    let mins = secs / 60;
    let minute = (mins % 60) as i16;
    let hours = mins / 60;
    let hour = (hours % 24) as i16;
    let days = hours / 24; // days since 1601-01-01 (a Monday)
    let weekday = ((days + 1).rem_euclid(7)) as i16; // 0=Sunday
    // Shift days-since-1601 to the algorithm's internal epoch (0000-03-01).
    // 134774 = days from 1601-01-01 to 1970-01-01; 719468 = 1970 → 0000-03-01.
    let z = days - 134774 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as i16; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as i16; // [1, 12]
    let year = (if m <= 2 { y + 1 } else { y }) as i16;
    (year, m, d, hour, minute, second, ms, weekday)
}

// ---------------------------------------------------------------------------
// SHA-1 (XcSHAInit/Update/Final). Real implementation — nxdk's rand() seed and
// any title that hashes data depend on a correct digest. The context is opaque
// to the caller (a >=96-byte buffer), so we lay it out as:
//   +0  total byte count (u64)   +8  state H0..H4 (5×u32)
//   +28 buffer fill (u32)        +32 64-byte block buffer
// ---------------------------------------------------------------------------

const SHA1_H0: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];

fn sha1_block(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (state[0], state[1], state[2], state[3], state[4]);
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let tmp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = tmp;
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

struct Sha1Ctx {
    total: u64,
    state: [u32; 5],
    buf: [u8; 64],
    buflen: usize,
}

fn sha1_load(mem: &Mem, ctx: u32) -> Sha1Ctx {
    let total = (mem.ram_read32(ctx) as u64) | ((mem.ram_read32(ctx.wrapping_add(4)) as u64) << 32);
    let mut state = [0u32; 5];
    for (i, s) in state.iter_mut().enumerate() {
        *s = mem.ram_read32(ctx.wrapping_add(8 + i as u32 * 4));
    }
    let buflen = (mem.ram_read32(ctx.wrapping_add(28)) as usize).min(63);
    let mut buf = [0u8; 64];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = mem.ram_read8(ctx.wrapping_add(32 + i as u32)) as u8;
    }
    Sha1Ctx { total, state, buf, buflen }
}

fn sha1_store(mem: &mut Mem, ctx: u32, c: &Sha1Ctx) {
    mem.ram_write32(ctx, c.total as u32);
    mem.ram_write32(ctx.wrapping_add(4), (c.total >> 32) as u32);
    for (i, &s) in c.state.iter().enumerate() {
        mem.ram_write32(ctx.wrapping_add(8 + i as u32 * 4), s);
    }
    mem.ram_write32(ctx.wrapping_add(28), c.buflen as u32);
    for (i, &b) in c.buf.iter().enumerate() {
        mem.ram_write8(ctx.wrapping_add(32 + i as u32), b as u32);
    }
}

/// Current system tick in milliseconds (the KeTickCount export's value, or 0
/// before it's been allocated).
fn current_tick_ms(mem: &Mem) -> u32 {
    let g = KDATA.lock().unwrap();
    g.as_ref()
        .and_then(|m| m.get(&ORD_KE_TICK_COUNT).copied())
        .map(|addr| mem.ram_read32(addr))
        .unwrap_or(0)
}

/// Reset all HLE global state (allocator, scheduler, ISR, filesystem, kernel
/// data). Called when a new game boots so stale state from a prior run (these
/// are process-globals) doesn't leak across loads.
pub fn reset() {
    NEXT_HEAP.store(HEAP_BASE, Ordering::SeqCst);
    NEXT_KDATA.store(KDATA_BASE, Ordering::SeqCst);
    *SCHED.lock().unwrap() = None;
    *CONNECTED_ISR.lock().unwrap() = None;
    *KDATA.lock().unwrap() = None;
    FILES.lock().unwrap().clear();
    DISC.lock().unwrap().clear();
    REBOOT.store(false, Ordering::SeqCst);
    SAVED_DATA_ADDR.store(0, Ordering::SeqCst);
    SYNC_NEXT.store(SYNC_HANDLE_BASE, Ordering::SeqCst);
    CURRENT_KTHREAD.store(0, Ordering::SeqCst);
    *SYMLINKS.lock().unwrap() = None;
    *PERSISTED.lock().unwrap() = None;
    *LAUNCH_DATA.lock().unwrap() = None;
    *DISPLAY_MODE.lock().unwrap() = None;
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
/// Saved display-data address (AvSetSavedDataAddress). Survives a quick-reboot;
/// non-zero on the warm boot is the game's "I already ran the launcher" signal.
static SAVED_DATA_ADDR: AtomicU32 = AtomicU32::new(0);
/// Regions marked to survive a quick-reboot: (base, size).
static PERSISTED: Mutex<Option<Vec<(u32, u32)>>> = Mutex::new(None);
/// Base of the most-recently persisted page — the launch-data page.
static LAUNCH_DATA: Mutex<Option<u32>> = Mutex::new(None);
const ORD_LAUNCH_DATA_PAGE: u32 = 164;

/// Display mode set by the game via `AvSetDisplayMode` (frame-buffer address,
/// pitch, width, height), pending application to the NV2A by the orchestrator.
/// `Some` only when the game programmed a display. See [`take_display_mode`].
static DISPLAY_MODE: Mutex<Option<(u32, u32, u16, u16)>> = Mutex::new(None);

/// Consume a pending `AvSetDisplayMode` configuration. The orchestrator
/// (`Xbox::run_frame`) calls this and forwards it to the NV2A so scanout can
/// present the game's framebuffer.
pub fn take_display_mode() -> Option<(u32, u32, u16, u16)> {
    DISPLAY_MODE.lock().unwrap().take()
}

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

/// Read / restore the saved display-data address across a quick-reboot.
pub fn take_saved_data() -> u32 {
    SAVED_DATA_ADDR.load(Ordering::SeqCst)
}
pub fn set_saved_data(addr: u32) {
    SAVED_DATA_ADDR.store(addr, Ordering::SeqCst);
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
    if std::env::var_os("XBOX_TRACE_NV").is_some() {
        eprintln!("[set_launch_data_slot] slot(ord164 backing)={slot:08X} page={page:08X}");
    }
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
const ORD_KE_REMOVE_QUEUE_DPC: u32 = 137;
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
const ORD_NT_WRITE_FILE: u32 = 236;
const ORD_NT_CLOSE: u32 = 187;
const ORD_NT_QUERY_VOLUME_INFORMATION_FILE: u32 = 218;
const ORD_NT_QUERY_INFORMATION_FILE: u32 = 211;
const ORD_NT_SET_INFORMATION_FILE: u32 = 226;
const ORD_NT_QUERY_FULL_ATTRIBUTES_FILE: u32 = 210;
const ORD_NT_DEVICE_IO_CONTROL_FILE: u32 = 196;
const ORD_NT_OPEN_SYMBOLIC_LINK_OBJECT: u32 = 203;
const ORD_NT_QUERY_SYMBOLIC_LINK_OBJECT: u32 = 215;
const ORD_NT_CREATE_MUTANT: u32 = 192;
const ORD_NT_RELEASE_MUTANT: u32 = 221;
const ORD_NT_CREATE_EVENT: u32 = 189;
const ORD_KE_GET_CURRENT_THREAD: u32 = 104;
const ORD_KE_GET_CURRENT_IRQL: u32 = 103;
const ORD_KE_RAISE_IRQL_TO_DPC: u32 = 129;
const ORD_KE_RAISE_IRQL_TO_SYNCH: u32 = 130;
const ORD_KF_RAISE_IRQL: u32 = 160;
const ORD_KF_LOWER_IRQL: u32 = 161;
const ORD_INTERLOCKED_COMPARE_EXCHANGE: u32 = 51;
const ORD_INTERLOCKED_DECREMENT: u32 = 52;
const ORD_INTERLOCKED_INCREMENT: u32 = 53;
const ORD_INTERLOCKED_EXCHANGE: u32 = 54;
const ORD_INTERLOCKED_EXCHANGE_ADD: u32 = 55;
const ORD_XC_SHA_INIT: u32 = 335;
const ORD_XC_SHA_UPDATE: u32 = 336;
const ORD_XC_SHA_FINAL: u32 = 337;
const IOCTL_DISK_GET_DRIVE_GEOMETRY: u32 = 0x0007_0000;
const IOCTL_DISK_GET_PARTITION_INFO: u32 = 0x0007_4004;
const ORD_NT_FS_CONTROL_FILE: u32 = 200;
const ORD_RTL_TIME_TO_TIME_FIELDS: u32 = 305;
const ORD_KE_QUERY_SYSTEM_TIME: u32 = 128;
/// Windows FILETIME (100 ns ticks since 1601) for ~2023-01-01, used as the base
/// for KeQuerySystemTime; the system tick (ms) is added so time advances.
const SYSTEMTIME_BASE: u64 = 133_170_048_000_000_000;
const ORD_EX_QUERY_NON_VOLATILE_SETTING: u32 = 24;
const ORD_RTL_EQUAL_STRING: u32 = 279;
const ORD_HAL_GET_INTERRUPT_VECTOR: u32 = 44;
const ORD_KE_INITIALIZE_INTERRUPT: u32 = 109;
const ORD_KE_CONNECT_INTERRUPT: u32 = 98;
const ORD_MM_CLAIM_GPU_INSTANCE_MEMORY: u32 = 168;
const ORD_HAL_READ_WRITE_PCI_SPACE: u32 = 46;
const ORD_HAL_RETURN_TO_FIRMWARE: u32 = 49;
const ORD_AV_SEND_TV_ENCODER_OPTION: u32 = 2;
/// AvSendTVEncoderOption "get settings" option — reports AV pack + video standard.
const AV_ENC_GET_SETTINGS: u32 = 6;
const ORD_AV_SET_DISPLAY_MODE: u32 = 3;
const ORD_AV_GET_SAVED_DATA_ADDRESS: u32 = 1;
const ORD_AV_SET_SAVED_DATA_ADDRESS: u32 = 4;

/// Fake handle / id handed back for created threads (we don't model handles yet).
const FAKE_THREAD_HANDLE: u32 = 0x0000_BEEF;

/// Plausible EEPROM / non-volatile config values keyed by the XC_* ValueIndex
/// passed to ExQueryNonVolatileSetting. A retail console returns concrete
/// settings here; returning 0 ("unconfigured") makes setup code bail/reboot.
fn nonvolatile_value(index: u32) -> u32 {
    match index {
        0x0007 => 0x00000000, // XC_VIDEO flags (no widescreen/letterbox/60Hz)
        0x0008 => 0x00010001, // XC_AUDIO (stereo, no AC3/DTS)
        0x000C => 0x00000001, // XC_LANGUAGE = English
        0x0102 => 0x00400100, // XC_FACTORY_AV_REGION (NTSC-M / video standard)
        0x0103 => 0x00000001, // XC_FACTORY_GAME_REGION = North America
        0x0010 => 0x0a000a00, // XC_TIMEZONE_BIAS
        _ => 0x00000001,      // generic non-zero "configured" default
    }
}

/// Ordinals safe to stub as "return STATUS_SUCCESS, clean the stack" — init /
/// notification / registration functions whose side effects don't matter for
/// reaching the title screen. Grown empirically as the boot progresses.
const SAFE_NOOP: &[u32] = &[
    47,  // HalRegisterShutdownNotification
    113, // KeInitializeTimerEx
    67,  // IoCreateSymbolicLink (drive-letter mount — accept)
    69,  // IoDeleteSymbolicLink
    301, // RtlNtStatusToDosError (returns 0 = ERROR_SUCCESS)
    149, // KeSetTimer
    100, // KeDisconnectInterrupt
    // (AvGet/SetSavedDataAddress handled below — they carry the warm-boot signal;
    //  interrupt connect/init, events, waits, threads, Av display also below)
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
            if std::env::var_os("XBOX_TRACE_NV").is_some() {
                eprintln!("[persist] base={base:08X} size={size:08X} persist={persist}");
            }
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
        ORD_KE_REMOVE_QUEUE_DPC => {
            // BOOLEAN KeRemoveQueueDpc(Dpc) — we run DPCs synchronously on insert,
            // so none are ever pending: report "not queued" (FALSE).
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("KeRemoveQueueDpc")
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
        ORD_NT_WRITE_FILE => {
            // NtWriteFile(HANDLE, HANDLE Event, PIO_APC_ROUTINE, PVOID ApcCtx,
            //   PIO_STATUS_BLOCK, PVOID Buffer, ULONG Length, PLARGE_INTEGER
            //   ByteOffset) — 32 bytes. We have no writable backing store (the
            //   disc is read-only and the HDD is a stub), so accept the write and
            //   report all bytes written without persisting them. This keeps a
            //   game that journals to the HDD progressing; it just won't see its
            //   data survive a (re)boot.
            let iosb = arg(cpu, mem, 4);
            let len = arg(cpu, mem, 6);
            if iosb != 0 {
                mem.ram_write32(iosb, STATUS_SUCCESS);
                mem.ram_write32(iosb.wrapping_add(4), len); // Information = bytes written
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 32);
            Dispatch::Handled("NtWriteFile")
        }
        ORD_NT_CLOSE => {
            let h = arg(cpu, mem, 0);
            close_file(h);
            stdcall_return(cpu, mem, STATUS_SUCCESS, 4);
            Dispatch::Handled("NtClose")
        }
        ORD_NT_OPEN_SYMBOLIC_LINK_OBJECT => {
            // NtOpenSymbolicLinkObject(PHANDLE, POBJECT_ATTRIBUTES) — 8 bytes.
            let h_out = arg(cpu, mem, 0);
            let path = read_obj_path(mem, arg(cpu, mem, 1));
            let h = alloc_sync_handle();
            SYMLINKS
                .lock()
                .unwrap()
                .get_or_insert_with(std::collections::HashMap::new)
                .insert(h, path);
            if h_out != 0 {
                mem.ram_write32(h_out, h);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 8);
            Dispatch::Handled("NtOpenSymbolicLinkObject")
        }
        ORD_NT_QUERY_SYMBOLIC_LINK_OBJECT => {
            // NtQuerySymbolicLinkObject(HANDLE, POBJECT_STRING LinkTarget,
            //   PULONG ReturnedLength) — 12 bytes. OBJECT_STRING is
            //   { USHORT Length, USHORT MaximumLength, PCHAR Buffer }.
            let h = arg(cpu, mem, 0);
            let out_str = arg(cpu, mem, 1);
            let ret_len = arg(cpu, mem, 2);
            let src = SYMLINKS
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|m| m.get(&h).cloned())
                .unwrap_or_default();
            let target = symlink_target(&src);
            if out_str != 0 {
                let maxlen = mem.ram_read16(out_str.wrapping_add(2)) & 0xFFFF;
                let buf = mem.ram_read32(out_str.wrapping_add(4));
                let n = (target.len() as u32).min(maxlen);
                for (i, b) in target.bytes().take(n as usize).enumerate() {
                    mem.ram_write8(buf.wrapping_add(i as u32), b as u32);
                }
                mem.ram_write16(out_str, n); // Length
            }
            if ret_len != 0 {
                mem.ram_write32(ret_len, target.len() as u32);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 12);
            Dispatch::Handled("NtQuerySymbolicLinkObject")
        }
        ORD_NT_CREATE_MUTANT => {
            // NtCreateMutant(PHANDLE, POBJECT_ATTRIBUTES, BOOLEAN InitialOwner) —
            // 12 bytes. Hand back an opaque handle; waits on it don't block.
            let h_out = arg(cpu, mem, 0);
            if h_out != 0 {
                mem.ram_write32(h_out, alloc_sync_handle());
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 12);
            Dispatch::Handled("NtCreateMutant")
        }
        ORD_KE_GET_CURRENT_THREAD => {
            // PKTHREAD KeGetCurrentThread(VOID) — a stable non-null pointer to a
            // KTHREAD block for the running thread (used as a thread identity
            // token / for thread-local fields).
            let mut h = CURRENT_KTHREAD.load(Ordering::SeqCst);
            if h == 0 {
                h = kdata_alloc(0x100, 16);
                // KTHREAD.TlsData (offset 0x28) — the per-thread TLS slot array.
                // nxdk asserts it's non-null AND that (TlsData + 4) is 16-byte
                // aligned (it stores a self-pointer at [TlsData] and aligns the
                // slots to +4). Allocate so TlsData ≡ 12 (mod 16).
                let tls = kdata_alloc(0x1010, 16) + 12;
                mem.ram_write32(h.wrapping_add(0x28), tls);
                CURRENT_KTHREAD.store(h, Ordering::SeqCst);
            }
            stdcall_return(cpu, mem, h, 0);
            Dispatch::Handled("KeGetCurrentThread")
        }

        // ---- IRQL ----
        // The cooperative scheduler has no interrupt levels, so every thread runs
        // at PASSIVE_LEVEL (0). Get/raise/lower all report/return 0; the fastcall
        // Kf* variants take their new level in ECX (ignored) and clean no stack.
        ORD_KE_GET_CURRENT_IRQL
        | ORD_KE_RAISE_IRQL_TO_DPC
        | ORD_KE_RAISE_IRQL_TO_SYNCH
        | ORD_KF_RAISE_IRQL
        | ORD_KF_LOWER_IRQL => {
            stdcall_return(cpu, mem, 0, 0);
            Dispatch::Handled("Irql")
        }

        // ---- Interlocked atomics (fastcall: args in ECX/EDX) ----
        // Single-threaded-at-a-time scheduler, so a plain read-modify-write IS
        // the atomic operation. Return value in EAX; only the (zero or one)
        // stack args beyond ECX/EDX are cleaned.
        ORD_INTERLOCKED_INCREMENT => {
            let p = cpu.reg32(ECX);
            let v = mem.ram_read32(p).wrapping_add(1);
            mem.ram_write32(p, v);
            stdcall_return(cpu, mem, v, 0);
            Dispatch::Handled("InterlockedIncrement")
        }
        ORD_INTERLOCKED_DECREMENT => {
            let p = cpu.reg32(ECX);
            let v = mem.ram_read32(p).wrapping_sub(1);
            mem.ram_write32(p, v);
            stdcall_return(cpu, mem, v, 0);
            Dispatch::Handled("InterlockedDecrement")
        }
        ORD_INTERLOCKED_EXCHANGE => {
            // LONG InterlockedExchange(Target=ECX, Value=EDX) — set, return old.
            let p = cpu.reg32(ECX);
            let old = mem.ram_read32(p);
            mem.ram_write32(p, cpu.reg32(EDX));
            stdcall_return(cpu, mem, old, 0);
            Dispatch::Handled("InterlockedExchange")
        }
        ORD_INTERLOCKED_EXCHANGE_ADD => {
            // LONG InterlockedExchangeAdd(Addend=ECX, Value=EDX) — add, return old.
            let p = cpu.reg32(ECX);
            let old = mem.ram_read32(p);
            mem.ram_write32(p, old.wrapping_add(cpu.reg32(EDX)));
            stdcall_return(cpu, mem, old, 0);
            Dispatch::Handled("InterlockedExchangeAdd")
        }
        ORD_INTERLOCKED_COMPARE_EXCHANGE => {
            // LONG InterlockedCompareExchange(Dest=ECX, Exchange=EDX,
            //   Comparand=[esp+4]) — CAS; one stack arg (4 bytes) to clean.
            let p = cpu.reg32(ECX);
            let exchange = cpu.reg32(EDX);
            let comparand = arg(cpu, mem, 0);
            let old = mem.ram_read32(p);
            if old == comparand {
                mem.ram_write32(p, exchange);
            }
            stdcall_return(cpu, mem, old, 4);
            Dispatch::Handled("InterlockedCompareExchange")
        }
        ORD_XC_SHA_INIT => {
            // VOID XcSHAInit(PUCHAR Context) — reset to the SHA-1 initial state.
            let ctx = arg(cpu, mem, 0);
            sha1_store(mem, ctx, &Sha1Ctx { total: 0, state: SHA1_H0, buf: [0; 64], buflen: 0 });
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("XcSHAInit")
        }
        ORD_XC_SHA_UPDATE => {
            // VOID XcSHAUpdate(PUCHAR Context, PUCHAR Input, ULONG Length).
            let ctx = arg(cpu, mem, 0);
            let input = arg(cpu, mem, 1);
            let len = arg(cpu, mem, 2);
            let mut c = sha1_load(mem, ctx);
            for i in 0..len {
                c.buf[c.buflen] = mem.ram_read8(input.wrapping_add(i)) as u8;
                c.buflen += 1;
                if c.buflen == 64 {
                    let block = c.buf;
                    sha1_block(&mut c.state, &block);
                    c.buflen = 0;
                }
            }
            c.total = c.total.wrapping_add(len as u64);
            sha1_store(mem, ctx, &c);
            stdcall_return(cpu, mem, 0, 12);
            Dispatch::Handled("XcSHAUpdate")
        }
        ORD_XC_SHA_FINAL => {
            // VOID XcSHAFinal(PUCHAR Context, PUCHAR Digest) — pad + emit 20 bytes.
            let ctx = arg(cpu, mem, 0);
            let digest = arg(cpu, mem, 1);
            let mut c = sha1_load(mem, ctx);
            let bit_len = c.total.wrapping_mul(8);
            // Append 0x80, pad with zeros to a 56-byte boundary, then the 64-bit
            // big-endian bit length.
            c.buf[c.buflen] = 0x80;
            c.buflen += 1;
            if c.buflen == 64 {
                let b = c.buf;
                sha1_block(&mut c.state, &b);
                c.buflen = 0;
            }
            while c.buflen != 56 {
                if c.buflen == 64 {
                    let b = c.buf;
                    sha1_block(&mut c.state, &b);
                    c.buflen = 0;
                }
                c.buf[c.buflen] = 0;
                c.buflen += 1;
            }
            for i in 0..8 {
                c.buf[56 + i] = (bit_len >> (56 - i * 8)) as u8;
            }
            let b = c.buf;
            sha1_block(&mut c.state, &b);
            for (i, &s) in c.state.iter().enumerate() {
                let bytes = s.to_be_bytes();
                for (j, &by) in bytes.iter().enumerate() {
                    mem.ram_write8(digest.wrapping_add((i * 4 + j) as u32), by as u32);
                }
            }
            stdcall_return(cpu, mem, 0, 8);
            Dispatch::Handled("XcSHAFinal")
        }
        ORD_NT_CREATE_EVENT => {
            // NtCreateEvent(PHANDLE, POBJECT_ATTRIBUTES, EVENT_TYPE,
            //   BOOLEAN InitialState) — 16 bytes. Hand back a sync handle;
            //   handle-based waits don't block in this model.
            let h_out = arg(cpu, mem, 0);
            if h_out != 0 {
                mem.ram_write32(h_out, alloc_sync_handle());
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 16);
            Dispatch::Handled("NtCreateEvent")
        }
        ORD_NT_RELEASE_MUTANT => {
            // NtReleaseMutant(HANDLE, PLONG PreviousCount) — 8 bytes.
            let prev = arg(cpu, mem, 1);
            if prev != 0 {
                mem.ram_write32(prev, 0);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 8);
            Dispatch::Handled("NtReleaseMutant")
        }
        ORD_RTL_TIME_TO_TIME_FIELDS => {
            // VOID RtlTimeToTimeFields(PLARGE_INTEGER Time, PTIME_FIELDS Fields).
            // Fields = 8 SHORTs: Year, Month, Day, Hour, Minute, Second,
            // Milliseconds, Weekday.
            let time_ptr = arg(cpu, mem, 0);
            let fields = arg(cpu, mem, 1);
            if time_ptr != 0 && fields != 0 {
                let lo = mem.ram_read32(time_ptr) as u64;
                let hi = mem.ram_read32(time_ptr.wrapping_add(4)) as u64;
                let (y, mo, d, h, mi, s, ms, wd) = filetime_to_fields((hi << 32) | lo);
                for (off, v) in [(0, y), (2, mo), (4, d), (6, h), (8, mi), (10, s), (12, ms), (14, wd)] {
                    mem.ram_write16(fields.wrapping_add(off), (v as u16) as u32);
                }
            }
            stdcall_return(cpu, mem, 0, 8);
            Dispatch::Handled("RtlTimeToTimeFields")
        }
        ORD_KE_QUERY_SYSTEM_TIME => {
            // VOID KeQuerySystemTime(PLARGE_INTEGER CurrentTime) — 100 ns ticks
            // since 1601. Base time + the system tick (ms) so it advances.
            let out = arg(cpu, mem, 0);
            if out != 0 {
                let t = SYSTEMTIME_BASE + (current_tick_ms(mem) as u64) * 10_000;
                mem.ram_write32(out, t as u32);
                mem.ram_write32(out.wrapping_add(4), (t >> 32) as u32);
            }
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("KeQuerySystemTime")
        }
        ORD_NT_DEVICE_IO_CONTROL_FILE => {
            // NtDeviceIoControlFile(HANDLE, HANDLE Event, PIO_APC_ROUTINE,
            //   PVOID ApcCtx, PIO_STATUS_BLOCK, ULONG IoControlCode, PVOID InBuf,
            //   ULONG InLen, PVOID OutBuf, ULONG OutLen) — 40 bytes.
            let iosb = arg(cpu, mem, 4);
            let code = arg(cpu, mem, 5);
            let out_buf = arg(cpu, mem, 8);
            let out_len = arg(cpu, mem, 9);
            if std::env::var_os("XBOX_TRACE_IOCTL").is_some() {
                let in_buf = arg(cpu, mem, 6);
                let in_len = arg(cpu, mem, 7);
                eprintln!(
                    "[ioctl] code={code:#010x} in={in_buf:#x}/{in_len} out={out_buf:#x}/{out_len}"
                );
            }
            // Start zeroed, then fill the structures the game's HDD/cache setup
            // queries. Returning zeros makes it see a 0-byte disk and retry
            // forever, so present a plausible ~8 GB fixed disk with FATX-recognized
            // partitions.
            if out_buf != 0 {
                for i in (0..out_len.min(4096)).step_by(4) {
                    mem.ram_write32(out_buf.wrapping_add(i), 0);
                }
            }
            let info = match code {
                IOCTL_DISK_GET_DRIVE_GEOMETRY if out_buf != 0 && out_len >= 24 => {
                    // DISK_GEOMETRY { Cylinders(i64), MediaType(u32),
                    //   TracksPerCylinder(u32), SectorsPerTrack(u32), BytesPerSector(u32) }
                    mem.ram_write32(out_buf, 16644); // Cylinders low (≈8 GB @ CHS)
                    mem.ram_write32(out_buf.wrapping_add(4), 0); // Cylinders high
                    mem.ram_write32(out_buf.wrapping_add(8), 12); // MediaType = FixedMedia
                    mem.ram_write32(out_buf.wrapping_add(12), 16); // TracksPerCylinder
                    mem.ram_write32(out_buf.wrapping_add(16), 63); // SectorsPerTrack
                    mem.ram_write32(out_buf.wrapping_add(20), 512); // BytesPerSector
                    24
                }
                IOCTL_DISK_GET_PARTITION_INFO if out_buf != 0 && out_len >= 28 => {
                    // PARTITION_INFORMATION { StartingOffset(i64), PartitionLength(i64),
                    //   HiddenSectors(u32), PartitionNumber(u32), PartitionType(u8),
                    //   BootIndicator(u8), RecognizedPartition(u8), RewritePartition(u8) }
                    mem.ram_write32(out_buf, 0); // StartingOffset low
                    mem.ram_write32(out_buf.wrapping_add(4), 0); // StartingOffset high
                    mem.ram_write32(out_buf.wrapping_add(8), 0x2EE0_0000); // PartitionLength low (~750 MB)
                    mem.ram_write32(out_buf.wrapping_add(12), 0); // PartitionLength high
                    mem.ram_write32(out_buf.wrapping_add(16), 0); // HiddenSectors
                    mem.ram_write32(out_buf.wrapping_add(20), 0); // PartitionNumber
                    mem.ram_write8(out_buf.wrapping_add(24), 0x42); // PartitionType (non-zero)
                    mem.ram_write8(out_buf.wrapping_add(25), 0); // BootIndicator
                    mem.ram_write8(out_buf.wrapping_add(26), 1); // RecognizedPartition = TRUE
                    mem.ram_write8(out_buf.wrapping_add(27), 0); // RewritePartition
                    28
                }
                _ => 0,
            };
            if iosb != 0 {
                mem.ram_write32(iosb, STATUS_SUCCESS);
                mem.ram_write32(iosb.wrapping_add(4), info); // Information = bytes returned
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 40);
            Dispatch::Handled("NtDeviceIoControlFile")
        }
        ORD_NT_FS_CONTROL_FILE => {
            // NtFsControlFile(HANDLE, HANDLE Event, PIO_APC_ROUTINE, PVOID ApcCtx,
            //   PIO_STATUS_BLOCK, ULONG FsControlCode, PVOID InBuf, ULONG InLen,
            //   PVOID OutBuf, ULONG OutLen) — 40 bytes. No real FS backing; accept
            //   the control op with a zeroed output buffer.
            let iosb = arg(cpu, mem, 4);
            let out_buf = arg(cpu, mem, 8);
            let out_len = arg(cpu, mem, 9);
            if std::env::var_os("XBOX_TRACE_IOCTL").is_some() {
                eprintln!("[fsctl] code={:#010x} out={out_buf:#x}/{out_len}", arg(cpu, mem, 5));
            }
            if out_buf != 0 {
                for i in (0..out_len.min(4096)).step_by(4) {
                    mem.ram_write32(out_buf.wrapping_add(i), 0);
                }
            }
            if iosb != 0 {
                mem.ram_write32(iosb, STATUS_SUCCESS);
                mem.ram_write32(iosb.wrapping_add(4), 0);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 40);
            Dispatch::Handled("NtFsControlFile")
        }

        // NTSTATUS ExQueryNonVolatileSetting(DWORD ValueIndex, DWORD *Type,
        //   PVOID Value, SIZE_T ValueLength, PSIZE_T ResultLength) — read EEPROM
        // config. Return plausible values so setup checks don't see 0 and bail.
        ORD_EX_QUERY_NON_VOLATILE_SETTING => {
            let value_index = arg(cpu, mem, 0);
            let type_out = arg(cpu, mem, 1);
            let value_out = arg(cpu, mem, 2);
            let value_len = arg(cpu, mem, 3);
            let result_len = arg(cpu, mem, 4);
            if value_index == 0xFFFF {
                // Read the entire 256-byte EEPROM image (nxdk hashes it to seed
                // rand() and asserts ResultLength == 256). Emit a zeroed image
                // with plausible factory fields.
                let n = value_len.min(256);
                if value_out != 0 {
                    for i in 0..n {
                        mem.ram_write8(value_out.wrapping_add(i), 0);
                    }
                    if n >= 0x30 {
                        mem.ram_write32(value_out.wrapping_add(0x2C), 1); // GameRegion = North America
                    }
                    if n >= 0x5C {
                        mem.ram_write32(value_out.wrapping_add(0x58), 0x0040_0100); // VideoStandard = NTSC-M
                    }
                }
                if type_out != 0 {
                    mem.ram_write32(type_out, 3); // REG_BINARY
                }
                if result_len != 0 {
                    mem.ram_write32(result_len, n);
                }
            } else {
                let val: u32 = nonvolatile_value(value_index);
                if type_out != 0 {
                    mem.ram_write32(type_out, 4); // REG_DWORD
                }
                if value_out != 0 && value_len >= 4 {
                    mem.ram_write32(value_out, val);
                }
                if result_len != 0 {
                    mem.ram_write32(result_len, 4);
                }
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 20);
            Dispatch::Handled("ExQueryNonVolatileSetting")
        }

        // NTSTATUS NtQueryVolumeInformationFile(HANDLE, PIO_STATUS_BLOCK,
        //   PVOID FsInformation, ULONG Length, FS_INFORMATION_CLASS Class).
        // The launcher queries the (stubbed) HDD partition size/free space;
        // report a large, mostly-free FATX volume so it's satisfied.
        ORD_NT_QUERY_VOLUME_INFORMATION_FILE => {
            let iosb = arg(cpu, mem, 1);
            let info = arg(cpu, mem, 2);
            let len = arg(cpu, mem, 3);
            let class = arg(cpu, mem, 4);
            if info != 0 && len >= 8 {
                match class {
                    // FileFsSizeInformation (3): TotalAllocationUnits(i64),
                    // AvailableAllocationUnits(i64), SectorsPerUnit(u32),
                    // BytesPerSector(u32).
                    3 if len >= 24 => {
                        mem.ram_write32(info, 0x0010_0000); // total units (low)
                        mem.ram_write32(info.wrapping_add(4), 0);
                        mem.ram_write32(info.wrapping_add(8), 0x000F_0000); // avail (low)
                        mem.ram_write32(info.wrapping_add(12), 0);
                        mem.ram_write32(info.wrapping_add(16), 32); // sectors/unit
                        mem.ram_write32(info.wrapping_add(20), 512); // bytes/sector
                    }
                    _ => {
                        for o in (0..len.min(64)).step_by(4) {
                            mem.ram_write32(info.wrapping_add(o), 0);
                        }
                    }
                }
            }
            if iosb != 0 {
                mem.ram_write32(iosb, STATUS_SUCCESS);
                mem.ram_write32(iosb.wrapping_add(4), len.min(24));
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 20);
            Dispatch::Handled("NtQueryVolumeInformationFile")
        }

        // NTSTATUS NtQueryInformationFile(HANDLE, PIO_STATUS_BLOCK,
        //   PVOID FileInformation, ULONG Length, FILE_INFORMATION_CLASS Class).
        // Report size/position for an open handle so the game's seek/read logic
        // has real numbers. The common classes during boot are FileStandard (5),
        // FilePosition (14), and FileNetworkOpen (34).
        ORD_NT_QUERY_INFORMATION_FILE => {
            let h = arg(cpu, mem, 0);
            let iosb = arg(cpu, mem, 1);
            let info = arg(cpu, mem, 2);
            let len = arg(cpu, mem, 3);
            let class = arg(cpu, mem, 4);
            let (size, pos) = file_size_pos(h).unwrap_or((0, 0));
            let size = size as u64;
            let pos = pos as u64;
            let mut status = STATUS_SUCCESS;
            let mut written = 0u32;
            if info != 0 {
                let w64 = |mem: &mut Mem, a: u32, v: u64| {
                    mem.ram_write32(a, v as u32);
                    mem.ram_write32(a.wrapping_add(4), (v >> 32) as u32);
                };
                match class {
                    // FileStandardInformation (5): AllocationSize(i64),
                    // EndOfFile(i64), NumberOfLinks(u32), DeletePending(u8),
                    // Directory(u8). 24 bytes.
                    5 if len >= 24 => {
                        let alloc = (size + 0xFFF) & !0xFFF;
                        w64(mem, info, alloc);
                        w64(mem, info.wrapping_add(8), size);
                        mem.ram_write32(info.wrapping_add(16), 1); // NumberOfLinks
                        mem.ram_write8(info.wrapping_add(20), 0); // DeletePending
                        mem.ram_write8(info.wrapping_add(21), 0); // Directory
                        written = 24;
                    }
                    // FilePositionInformation (14): CurrentByteOffset(i64). 8 bytes.
                    14 if len >= 8 => {
                        w64(mem, info, pos);
                        written = 8;
                    }
                    // FileNetworkOpenInformation (34): CreationTime, LastAccess,
                    // LastWrite, ChangeTime (each i64), AllocationSize(i64),
                    // EndOfFile(i64), FileAttributes(u32). 56 bytes.
                    34 if len >= 56 => {
                        for i in 0..4 {
                            w64(mem, info.wrapping_add(i * 8), 0);
                        }
                        let alloc = (size + 0xFFF) & !0xFFF;
                        w64(mem, info.wrapping_add(32), alloc);
                        w64(mem, info.wrapping_add(40), size);
                        mem.ram_write32(info.wrapping_add(48), 0x80); // FILE_ATTRIBUTE_NORMAL
                        written = 56;
                    }
                    _ => {
                        // Zero-fill the buffer for unknown classes.
                        for o in (0..len.min(64)).step_by(4) {
                            mem.ram_write32(info.wrapping_add(o), 0);
                        }
                        written = len.min(64);
                        if file_size_pos(h).is_none() {
                            status = STATUS_INVALID_HANDLE;
                        }
                    }
                }
            }
            if iosb != 0 {
                mem.ram_write32(iosb, status);
                mem.ram_write32(iosb.wrapping_add(4), written);
            }
            stdcall_return(cpu, mem, status, 20);
            Dispatch::Handled("NtQueryInformationFile")
        }

        // NTSTATUS NtSetInformationFile(HANDLE, PIO_STATUS_BLOCK,
        //   PVOID FileInformation, ULONG Length, FILE_INFORMATION_CLASS Class).
        // The only class we honour is FilePositionInformation (14) — a seek.
        ORD_NT_SET_INFORMATION_FILE => {
            let h = arg(cpu, mem, 0);
            let iosb = arg(cpu, mem, 1);
            let info = arg(cpu, mem, 2);
            let len = arg(cpu, mem, 3);
            let class = arg(cpu, mem, 4);
            if class == 14 && info != 0 && len >= 8 {
                let pos = mem.ram_read32(info) as usize; // low 32 bits
                file_set_pos(h, pos);
            }
            if iosb != 0 {
                mem.ram_write32(iosb, STATUS_SUCCESS);
                mem.ram_write32(iosb.wrapping_add(4), 0);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 20);
            Dispatch::Handled("NtSetInformationFile")
        }

        // NTSTATUS NtQueryFullAttributesFile(POBJECT_ATTRIBUTES,
        //   PFILE_NETWORK_OPEN_INFORMATION). Resolve the path; if it exists,
        // report its size, else NAME_NOT_FOUND so the game knows it's absent.
        ORD_NT_QUERY_FULL_ATTRIBUTES_FILE => {
            let oa = arg(cpu, mem, 0);
            let out = arg(cpu, mem, 1);
            let path = read_obj_path(mem, oa);
            let (status, handle, _info) = open_file(&path);
            if status == STATUS_SUCCESS {
                let (size, _pos) = file_size_pos(handle).unwrap_or((0, 0));
                close_file(handle);
                if out != 0 {
                    let w64 = |mem: &mut Mem, a: u32, v: u64| {
                        mem.ram_write32(a, v as u32);
                        mem.ram_write32(a.wrapping_add(4), (v >> 32) as u32);
                    };
                    for i in 0..4 {
                        w64(mem, out.wrapping_add(i * 8), 0); // timestamps
                    }
                    let size = size as u64;
                    let alloc = (size + 0xFFF) & !0xFFF;
                    w64(mem, out.wrapping_add(32), alloc);
                    w64(mem, out.wrapping_add(40), size);
                    mem.ram_write32(out.wrapping_add(48), 0x80);
                }
            }
            stdcall_return(cpu, mem, status, 8);
            Dispatch::Handled("NtQueryFullAttributesFile")
        }

        // BOOLEAN RtlEqualString(PSTRING S1, PSTRING S2, BOOLEAN CaseInsensitive).
        // STRING = { USHORT Length; USHORT MaximumLength; PCHAR Buffer }.
        ORD_RTL_EQUAL_STRING => {
            let s1 = arg(cpu, mem, 0);
            let s2 = arg(cpu, mem, 1);
            let ci = arg(cpu, mem, 2) != 0;
            let read = |p: u32| -> (u32, u32) {
                (mem.ram_read16(p) & 0xFFFF, mem.ram_read32(p.wrapping_add(4)))
            };
            let (l1, b1) = read(s1);
            let (l2, b2) = read(s2);
            let equal = if l1 != l2 {
                false
            } else {
                (0..l1).all(|i| {
                    let mut c1 = mem.ram_read8(b1.wrapping_add(i)) as u8;
                    let mut c2 = mem.ram_read8(b2.wrapping_add(i)) as u8;
                    if ci {
                        c1 = c1.to_ascii_uppercase();
                        c2 = c2.to_ascii_uppercase();
                    }
                    c1 == c2
                })
            };
            stdcall_return(cpu, mem, equal as u32, 12);
            Dispatch::Handled("RtlEqualString")
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
            // The only query that matters for bring-up is GET_SETTINGS (option 6):
            // it reports the connected AV pack + video standard, which the video
            // HAL matches against its mode table. Returning 0 means "no display"
            // (AV_PACK_NONE) so no mode is found and the title never calls
            // AvSetDisplayMode. Report a standard AV cable + NTSC-M
            // (adapter=AV_PACK_STANDARD=0x01, standard=NTSC-M=0x0100).
            let option = arg(cpu, mem, 1);
            let result = arg(cpu, mem, 3);
            if result != 0 {
                let val = if option == AV_ENC_GET_SETTINGS { 0x0000_0101 } else { 0 };
                mem.ram_write32(result, val);
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 16);
            Dispatch::Handled("AvSendTVEncoderOption")
        }
        ORD_AV_SET_DISPLAY_MODE => {
            // AvSetDisplayMode(RegisterBase, Step, Mode, Format, Pitch, FrameBuffer)
            // — hands the encoder the framebuffer the display scans out. Capture
            // its address/pitch (and the mode's resolution) so the NV2A can
            // present the game's own framebuffer even when it draws through a path
            // PGRAPH doesn't fully model. The `Mode` word encodes the resolution;
            // retail titles use 640x480, so we default there and only override
            // when the mode clearly selects 720x480 (the other common NTSC mode).
            let mode = arg(cpu, mem, 2);
            let pitch = arg(cpu, mem, 4);
            let framebuffer = arg(cpu, mem, 5);
            let (w, h) = match mode & 0x0FFF {
                // AVMODE width field (low bits) maps to a horizontal pixel count;
                // 0x2D0 = 720, else fall back to 640. Height defaults to 480.
                w if w == 720 || w == 0x2D0 => (720u16, 480u16),
                _ => (640u16, 480u16),
            };
            if framebuffer != 0 {
                *DISPLAY_MODE.lock().unwrap() = Some((framebuffer, pitch, w, h));
            }
            stdcall_return(cpu, mem, STATUS_SUCCESS, 24);
            Dispatch::Handled("AvSetDisplayMode")
        }
        // PVOID AvGetSavedDataAddress(VOID) — the saved display surface from
        // before a quick-reboot. NULL on a cold boot, non-NULL on a warm boot:
        // the launcher reads this to know it already ran (and to skip rebooting).
        ORD_AV_GET_SAVED_DATA_ADDRESS => {
            let addr = SAVED_DATA_ADDR.load(Ordering::SeqCst);
            stdcall_return(cpu, mem, addr, 0);
            Dispatch::Handled("AvGetSavedDataAddress")
        }
        // VOID AvSetSavedDataAddress(PVOID Address) — remember the surface to
        // preserve across the next quick-reboot.
        ORD_AV_SET_SAVED_DATA_ADDRESS => {
            let addr = arg(cpu, mem, 0);
            SAVED_DATA_ADDR.store(addr, Ordering::SeqCst);
            stdcall_return(cpu, mem, 0, 4);
            Dispatch::Handled("AvSetSavedDataAddress")
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

    #[test]
    fn sha1_matches_known_vector() {
        // Drive the same block function the XcSHA* handlers use.
        fn oneshot(data: &[u8]) -> [u8; 20] {
            let mut state = SHA1_H0;
            let mut buf = [0u8; 64];
            let mut n = 0usize;
            for &byte in data {
                buf[n] = byte;
                n += 1;
                if n == 64 {
                    sha1_block(&mut state, &buf);
                    n = 0;
                }
            }
            let bits = (data.len() as u64) * 8;
            buf[n] = 0x80;
            n += 1;
            if n == 64 {
                sha1_block(&mut state, &buf);
                n = 0;
            }
            while n != 56 {
                if n == 64 {
                    sha1_block(&mut state, &buf);
                    n = 0;
                }
                buf[n] = 0;
                n += 1;
            }
            for i in 0..8 {
                buf[56 + i] = (bits >> (56 - i * 8)) as u8;
            }
            sha1_block(&mut state, &buf);
            let mut out = [0u8; 20];
            for i in 0..5 {
                out[i * 4..i * 4 + 4].copy_from_slice(&state[i].to_be_bytes());
            }
            out
        }
        let hex: String = oneshot(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn filetime_to_fields_known_dates() {
        // SYSTEMTIME_BASE is 2023-01-01 00:00:00 UTC (a Sunday).
        let (y, mo, d, h, mi, s, ms, wd) = filetime_to_fields(SYSTEMTIME_BASE);
        assert_eq!((y, mo, d), (2023, 1, 1));
        assert_eq!((h, mi, s, ms), (0, 0, 0, 0));
        assert_eq!(wd, 0); // Sunday
        // Add one day + 2 hours + 3 minutes + 4 seconds + 5 ms.
        let t = SYSTEMTIME_BASE
            + ((((24 + 2) * 60 + 3) * 60 + 4) as u64) * 10_000_000
            + 5 * 10_000;
        let (y, mo, d, h, mi, s, ms, wd) = filetime_to_fields(t);
        assert_eq!((y, mo, d, h, mi, s, ms), (2023, 1, 2, 2, 3, 4, 5));
        assert_eq!(wd, 1); // Monday
    }

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
    fn av_set_display_mode_captures_framebuffer() {
        // AvSetDisplayMode(RegisterBase, Step, Mode, Format, Pitch, FrameBuffer).
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *DISPLAY_MODE.lock().unwrap() = None;
        let (mut cpu, mut mem, esp0) =
            frame(&[0xFD00_0000, 0, 640, 0, 640 * 4, 0x0003_C000]);
        let out = dispatch(&mut cpu, &mut mem, ORD_AV_SET_DISPLAY_MODE);
        assert!(matches!(out, Dispatch::Handled("AvSetDisplayMode")));
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 24);
        let dm = take_display_mode().expect("display mode captured");
        assert_eq!(dm, (0x0003_C000, 640 * 4, 640, 480));
        // Consumed.
        assert_eq!(take_display_mode(), None);
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
    fn hal_disk_cache_partition_count_is_nonzero() {
        // Regression: Halo reads HalDiskCachePartitionCount (ordinal 40) as a
        // count, decrements it, and uses count*12 as a memcpy length. A zero
        // here underflows to 0xFFFFFFFF and wipes the game's own image. The
        // DATA-export backing must be initialized to a positive partition count.
        reset();
        let mut mem = Mem::new();
        let addr = data_export_addr(ORD_HAL_DISK_CACHE_PARTITION_COUNT, &mut mem);
        let count = mem.ram_read32(addr);
        assert_eq!(count, HAL_DISK_CACHE_PARTITION_COUNT);
        assert!(count != 0, "count must be positive to avoid dec-to-0xFFFFFFFF");
        assert!(count.wrapping_sub(1) < 0x1000, "decremented count stays small");
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
    fn nt_query_information_file_standard_reports_size() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Install an open handle with a known size/pos directly in the table.
        {
            let mut files = FILES.lock().unwrap();
            files.clear();
            files.push(Some(FileHandle { offset: 0, size: 0x1234, pos: 0x10 }));
        }
        let h = FILE_HANDLE_BASE;
        let iosb = 0x0007_0000u32;
        let info = 0x0007_1000u32;
        // args: handle, iosb, info, length, class(5 = FileStandardInformation)
        let (mut cpu, mut mem, esp0) = frame(&[h, iosb, info, 24, 5]);
        let out = dispatch(&mut cpu, &mut mem, ORD_NT_QUERY_INFORMATION_FILE);
        assert!(matches!(out, Dispatch::Handled("NtQueryInformationFile")));
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 20);
        // EndOfFile (i64 @ +8) == size.
        assert_eq!(mem.ram_read32(info + 8), 0x1234, "EndOfFile low dword == size");
        assert_eq!(mem.ram_read32(info + 12), 0, "EndOfFile high dword == 0");
        // IoStatusBlock.Information == bytes written (24).
        assert_eq!(mem.ram_read32(iosb + 4), 24);
        FILES.lock().unwrap().clear();
    }

    #[test]
    fn nt_query_information_file_position() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut files = FILES.lock().unwrap();
            files.clear();
            files.push(Some(FileHandle { offset: 0, size: 0x1000, pos: 0x40 }));
        }
        let h = FILE_HANDLE_BASE;
        let info = 0x0008_1000u32;
        // class 14 = FilePositionInformation.
        let (mut cpu, mut mem, esp0) = frame(&[h, 0, info, 8, 14]);
        dispatch(&mut cpu, &mut mem, ORD_NT_QUERY_INFORMATION_FILE);
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 20);
        assert_eq!(mem.ram_read32(info), 0x40, "CurrentByteOffset == pos");
        FILES.lock().unwrap().clear();
    }

    #[test]
    fn nt_set_information_file_seeks() {
        let _g = HEAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut files = FILES.lock().unwrap();
            files.clear();
            files.push(Some(FileHandle { offset: 0, size: 0x1000, pos: 0 }));
        }
        let h = FILE_HANDLE_BASE;
        let info = 0x0009_1000u32;
        let (mut cpu, mut mem, esp0) = frame(&[h, 0, info, 8, 14]);
        mem.ram_write32(info, 0x200); // seek to 0x200
        dispatch(&mut cpu, &mut mem, ORD_NT_SET_INFORMATION_FILE);
        assert_returned(&cpu, esp0, STATUS_SUCCESS, 20);
        assert_eq!(file_size_pos(h).unwrap().1, 0x200, "position updated");
        FILES.lock().unwrap().clear();
    }

    #[test]
    fn hdd_concrete_file_open_fails() {
        // An actual save-game payload file on a fresh HDD must report NOT_FOUND,
        // not zero-filled success (a fresh console has no saves; a zero "save"
        // would be parsed as junk). This is NOT title metadata — it lives in a
        // save-slot subdirectory and has a generic name.
        let (status, _, _) =
            open_file("\\Device\\Harddisk0\\partition1\\UDATA\\4d530064\\00000001\\savegame.dat");
        assert_eq!(status, STATUS_OBJECT_NAME_NOT_FOUND);
        // A directory/partition path still succeeds (present, empty).
        let (status2, _, _) = open_file("\\Device\\Harddisk0\\partition1\\UDATA");
        assert_eq!(status2, STATUS_SUCCESS);
        FILES.lock().unwrap().clear();
    }

    #[test]
    fn hdd_title_metadata_open_succeeds() {
        // Dashboard-managed title metadata under UDATA\<titleid>\ exists before a
        // game ever runs on a real console. Reporting it absent makes Halo CE /
        // Halo 2 reboot-loop (treating "metadata missing" as first-time setup),
        // so we present it as existing-but-empty. Regression guard for that fix.
        for f in ["TitleMeta.xbx", "TitleImage.xbx", "SaveImage.xbx", "SaveMeta.xbx"] {
            let path = format!("\\Device\\Harddisk0\\partition1\\UDATA\\4d530004\\{f}");
            let (status, handle, info) = open_file(&path);
            assert_eq!(status, STATUS_SUCCESS, "{f} must open (present-but-empty)");
            assert_eq!(info, FILE_OPENED);
            // Reads from it return zero-filled success (an empty metadata file).
            assert!(handle >= FILE_HANDLE_BASE);
        }
        FILES.lock().unwrap().clear();
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
