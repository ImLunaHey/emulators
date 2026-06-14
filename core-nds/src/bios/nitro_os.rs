//! NitroSDK OS-thread HLE assist. Pokes the game out of a deadlocked
//! wait-for-thread-wakeup pattern the SDK enters when a thread we never run was
//! supposed to signal completion. Ported from
//! ../../ds-recomp/src/bios/nitro_os.ts.
//!
//! Background: NitroSDK's cooperative thread package blocks via
//! `OS_SleepThread(cond)` and the dispatcher parks the ARM9 in CP15 WFI when
//! nothing is RUNNABLE. The waker thread never runs in our world (we have no
//! NitroSDK binary to call `OS_WakeupThread` from), so the ARM9 lands in WFI
//! forever. Once per frame, after a long WFI deadlock with no plausible wake
//! source, we scan main RAM for OS_Thread records, force the first WAITING
//! thread's condition word + state, and raise IRQ_IPC_SYNC to lift the halt.
//!
//! Ownership (CONTRACT.md): the TS `NitroOsAssist` held an `Emulator` ref and
//! reached into `cpu9`, `irq9`, `ipc`, `dma9`, `mem`. Here `tick` runs as a
//! method ON `Nds` (`Nds::nitro_os_tick`), so it borrows those subsystems out
//! of the god-struct directly; this struct owns only the assist's own counters.

use crate::memory::regions::{MAIN_RAM_BASE, MAIN_RAM_MASK};

/// NitroSDK OS_Thread state codes (stable across SDK 5.x — the kernel scheduler
/// matches against them directly).
pub const OS_THREAD_STATE_RUNNABLE: u16 = 0x0001;
pub const OS_THREAD_STATE_WAITING: u16 = 0x0002;
pub const OS_THREAD_STATE_SLEEPING: u16 = 0x0004;
pub const OS_THREAD_STATE_DEAD: u16 = 0x0008;

/// Frames of uninterrupted WFI halt before the assist will act (60 ≈ 1 s).
const DEADLOCK_FRAMES: u32 = 60;
/// Frames after a synthesized wake we watch for forward progress.
const PROGRESS_WINDOW: u32 = 60;
/// Minimum PC delta we accept as "the kick worked".
const PC_DELTA_GOOD: u32 = 0x1000;

/// Main-RAM byte-address range scanned for OS_Thread records.
const THREAD_SCAN_REGION_LO: u32 = 0x0200_0000;
const THREAD_SCAN_REGION_HI: u32 = 0x0240_0000;

/// OS_Thread struct field offsets (NitroSDK os/thread.h, stable across SDK 5.x).
const THREAD_STATE_OFF: u32 = 0x4C;
const THREAD_WAITING_OFF: u32 = 0x60;
const THREAD_STRUCT_MIN_SIZE: u32 = 0x70;

/// Per-game legacy "tick bump" allowlist — games whose ARM9 busy-spins on an
/// ARM7 ready bit our stub never sets, so the historical +1 bump to the SDK
/// tick word is the only thing that boots them. Gated by game code so it can
/// never corrupt a game (Pokemon D/P/Pt) the bump was breaking.
const LEGACY_TICK_BUMP_GAMES: [&str; 2] = ["BVYE", "BWBE"];

/// 0x027FFF8C SDK tick / PXI callback-ready word (masked into the 4 MB mirror).
const SDK_TICK_COUNTER_ADDR: u32 = 0x02FF_FF8C;

/// PXI-drain hysteresis: drain only at-or-above this queue depth, after this
/// many consecutive stuck frames.
const PXI_DRAIN_THRESHOLD: usize = 12;
const PXI_DRAIN_FRAMES: u32 = 120;

/// Deadlock-assist state machine. Owns only its own counters; the RAM/CPU/IRQ
/// it acts on are borrowed from `Nds` at `tick` time.
#[derive(Default)]
pub struct NitroOsAssist {
    /// Consecutive frames the ARM9 has been halted in WFI with no wake source.
    wfi_frames: u32,
    /// PC at the most recent synthesized wake (watched for forward progress).
    wake_pc: u32,
    /// Frames remaining in the post-wake observation window.
    wake_watch: u32,
    /// Byte offset of the last thread we kicked (so escalation picks another).
    last_kicked_off: u32,

    /// Resolved-once allowlist membership for the legacy tick bump.
    tick_allowed: Option<bool>,
    /// Consecutive frames the ARM9→ARM7 PXI queue has been stuck near full.
    pxi_stuck_frames: u32,

    /// Diagnostics — readable by tests / the debug panel.
    pub synthesized_wakes: u32,
    pub synthetic_ticks: u32,
    pub pxi_drains: u32,
}

impl NitroOsAssist {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Find the first WAITING thread at or after `start_off`. Returns the byte
/// offset inside main RAM, or `None`. (TS `findWaitingThread`.)
fn find_waiting_thread(main_ram: &[u8], start_off: usize) -> Option<usize> {
    let hi = ((THREAD_SCAN_REGION_HI - MAIN_RAM_BASE) as usize)
        .min(main_ram.len().saturating_sub(THREAD_STRUCT_MIN_SIZE as usize));
    let mut off = start_off;
    while off <= hi {
        if read16(main_ram, off + THREAD_STATE_OFF as usize) == OS_THREAD_STATE_WAITING {
            return Some(off);
        }
        off += 4;
    }
    None
}

#[inline]
fn write32(mem: &mut [u8], off: usize, v: u32) {
    mem[off] = (v & 0xFF) as u8;
    mem[off + 1] = ((v >> 8) & 0xFF) as u8;
    mem[off + 2] = ((v >> 16) & 0xFF) as u8;
    mem[off + 3] = ((v >> 24) & 0xFF) as u8;
}

#[inline]
fn read32(mem: &[u8], off: usize) -> u32 {
    (mem.get(off).copied().unwrap_or(0) as u32)
        | ((mem.get(off + 1).copied().unwrap_or(0) as u32) << 8)
        | ((mem.get(off + 2).copied().unwrap_or(0) as u32) << 16)
        | ((mem.get(off + 3).copied().unwrap_or(0) as u32) << 24)
}

impl crate::nds::Nds {
    /// Once-per-frame deadlock assist. Runs the unconditional secondary assists
    /// (legacy tick bump, PXI drain), then — if the ARM9 has been WFI-halted
    /// past `DEADLOCK_FRAMES` with no plausible wake source — synthesizes a
    /// thread wake. Returns `true` iff a wake was synthesized this frame. (TS
    /// `NitroOsAssist.tick`.)
    pub fn nitro_os_tick(&mut self) -> bool {
        // Secondary assists that fire unconditionally per frame (not gated by
        // the CPU's halt state).
        self.nitro_tick_vblank_counter();
        self.nitro_tick_pxi_drain();

        // Not halted? The deadlock counter resets and we observe forward
        // progress against the post-wake window.
        if !self.state9.halted {
            self.nitro_os.wfi_frames = 0;
            if self.nitro_os.wake_watch > 0 {
                let pc = self.state9.r[15];
                let delta = pc.abs_diff(self.nitro_os.wake_pc);
                if delta > PC_DELTA_GOOD {
                    self.nitro_os.wake_watch = 0;
                    self.nitro_os.last_kicked_off = 0;
                } else {
                    self.nitro_os.wake_watch -= 1;
                }
            }
            return false;
        }

        // Halted. Something else will wake the CPU on its own?
        if self.irq9.wake_pending() {
            self.nitro_os.wfi_frames = 0;
            return false;
        }
        if self.nitro_has_pending_wake_source() {
            self.nitro_os.wfi_frames = 0;
            return false;
        }

        self.nitro_os.wfi_frames += 1;
        if self.nitro_os.wfi_frames < DEADLOCK_FRAMES {
            return false;
        }

        self.nitro_synthesize_wake()
    }

    /// Any source that could plausibly wake a halted ARM9 next frame without our
    /// help: IPC FIFO traffic in flight, or an active DMA channel. (TS
    /// `hasPendingWakeSource`.)
    fn nitro_has_pending_wake_source(&self) -> bool {
        if self.ipc.q7to9.size > 0 || self.ipc.q9to7.size > 0 {
            return true;
        }
        self.dma9.channels.iter().any(|c| c.enabled)
    }

    /// Find and kick the next WAITING thread. Returns true iff a wake was
    /// synthesized this frame. (TS `synthesizeWake`.)
    fn nitro_synthesize_wake(&mut self) -> bool {
        let start_off = if self.nitro_os.last_kicked_off > 0 {
            (self.nitro_os.last_kicked_off + THREAD_STRUCT_MIN_SIZE) as usize
        } else {
            (THREAD_SCAN_REGION_LO.saturating_sub(MAIN_RAM_BASE)) as usize
        };

        let Some(thread_off) = find_waiting_thread(&self.mem.main_ram[..], start_off) else {
            // Exhausted candidates — wrap and back off a full DEADLOCK_FRAMES.
            self.nitro_os.last_kicked_off = 0;
            self.nitro_os.wfi_frames = 0;
            return false;
        };

        // NitroSDK's OS_WakeupThread writes the woken thread's own address into
        // its wait-condition word, so the sleeper's `cond == &myThread` passes.
        let thread_addr = MAIN_RAM_BASE.wrapping_add(thread_off as u32);
        let cond_off = thread_off + THREAD_WAITING_OFF as usize;
        let mem = &mut self.mem.main_ram[..];
        if cond_off + 4 <= mem.len() {
            write32(mem, cond_off, thread_addr);
        }
        // Flip the state field to RUNNABLE so the scheduler picks it up.
        let so = thread_off + THREAD_STATE_OFF as usize;
        mem[so] = (OS_THREAD_STATE_RUNNABLE & 0xFF) as u8;
        mem[so + 1] = (OS_THREAD_STATE_RUNNABLE >> 8) as u8;

        // Kick the ARM9 out of WFI with an IPCSYNC IRQ — the SDK re-runs its
        // scheduler on any IRQ return.
        self.irq9
            .raise(crate::io::irq::IRQ_IPC_SYNC);

        self.nitro_os.wake_pc = self.state9.r[15];
        self.nitro_os.wake_watch = PROGRESS_WINDOW;
        self.nitro_os.last_kicked_off = thread_off as u32;
        self.nitro_os.synthesized_wakes += 1;
        self.nitro_os.wfi_frames = 0;
        true
    }

    /// Legacy per-game VBlank tick bump (gated by game code). (TS
    /// `tickVBlankTickCounter`.)
    fn nitro_tick_vblank_counter(&mut self) {
        if self.nitro_os.tick_allowed.is_none() {
            let code = self.nitro_game_code();
            let allowed = LEGACY_TICK_BUMP_GAMES.iter().any(|&g| g == code);
            self.nitro_os.tick_allowed = Some(allowed);
        }
        if self.nitro_os.tick_allowed != Some(true) {
            return;
        }
        let off = (SDK_TICK_COUNTER_ADDR & MAIN_RAM_MASK) as usize;
        let mem = &mut self.mem.main_ram[..];
        if off + 4 > mem.len() {
            return;
        }
        let cur = read32(mem, off);
        // Pointer-shaped values aren't in counter range — back off.
        if cur > 0x00FF_FFFF {
            return;
        }
        write32(mem, off, cur.wrapping_add(1));
        self.nitro_os.synthetic_ticks += 1;
    }

    /// ARM9→ARM7 PXI FIFO drain when the queue stays near-full with no ARM7
    /// reply traffic. (TS `tickPxiDrain`.)
    fn nitro_tick_pxi_drain(&mut self) {
        if self.ipc.q9to7.size < PXI_DRAIN_THRESHOLD {
            self.nitro_os.pxi_stuck_frames = 0;
            return;
        }
        self.nitro_os.pxi_stuck_frames += 1;
        if self.nitro_os.pxi_stuck_frames < PXI_DRAIN_FRAMES {
            return;
        }
        // Drain ONE entry per frame so retries refill at the natural rate.
        let Some(head) = self.ipc.q9to7.peek() else {
            return;
        };
        self.ipc.q9to7.pop();
        self.ipc
            .queue_arm7_ack(head | 0x20, &mut self.irq9, &mut self.irq7);
        self.nitro_os.pxi_drains += 1;
        self.nitro_os.pxi_stuck_frames = 0;
    }

    /// Resolve the mounted ROM's 4-char game code (header offset 0x0C). Empty
    /// when no cart is mounted. (The TS read `emu.header.gameCode`.)
    fn nitro_game_code(&self) -> String {
        let Some(cart) = self.cart.as_ref() else {
            return String::new();
        };
        let rom = &cart.rom;
        if rom.len() < 0x10 {
            return String::new();
        }
        rom[0x0C..0x10]
            .iter()
            .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' })
            .collect()
    }
}

/// Plausibility check for a candidate OS_Thread record at `off` in main RAM:
/// the state word must be a known RUNNABLE/WAITING/SLEEPING code and the struct
/// must fit. False positives are bounded — the assist only ever writes the
/// wait-condition word. (TS `looksLikeThread`.)
pub(crate) fn looks_like_thread(main_ram: &[u8], off: usize) -> bool {
    if off + THREAD_STRUCT_MIN_SIZE as usize > main_ram.len() {
        return false;
    }
    let state = read16(main_ram, off + THREAD_STATE_OFF as usize);
    matches!(
        state,
        OS_THREAD_STATE_RUNNABLE | OS_THREAD_STATE_WAITING | OS_THREAD_STATE_SLEEPING
    )
}

/// Scan main RAM for the first plausible OS_Thread record; returns its main-RAM
/// ADDRESS (0x02xxxxxx) or `None`. Scans on 4-byte boundaries (NitroSDK
/// word-aligns every OS_Thread). (TS `findOsThreadList`.)
pub fn find_os_thread_list(main_ram: &[u8]) -> Option<u32> {
    // Don't mask the byte offset — main RAM is 4 MB and the scan region can
    // reach (or exceed) that. Clamp so a full-RAM scan ends at the buffer end.
    let lo = (THREAD_SCAN_REGION_LO.saturating_sub(MAIN_RAM_BASE)) as usize;
    let hi = ((THREAD_SCAN_REGION_HI - MAIN_RAM_BASE) as usize)
        .min(main_ram.len().saturating_sub(THREAD_STRUCT_MIN_SIZE as usize));
    let mut off = lo;
    while off <= hi {
        if looks_like_thread(main_ram, off) {
            return Some(MAIN_RAM_BASE.wrapping_add(off as u32));
        }
        off += 4;
    }
    None
}

#[inline]
pub(crate) fn read16(mem: &[u8], off: usize) -> u16 {
    (mem.get(off).copied().unwrap_or(0) as u16)
        | ((mem.get(off + 1).copied().unwrap_or(0) as u16) << 8)
}

// Re-export the struct-layout constants so tests can build synthetic thread
// records without copy-pasting offsets (TS does the same).
pub const NITRO_THREAD_STRUCT_SIZE: u32 = THREAD_STRUCT_MIN_SIZE;
pub const NITRO_THREAD_STATE_OFF: u32 = THREAD_STATE_OFF;
pub const NITRO_THREAD_WAITING_OFF: u32 = THREAD_WAITING_OFF;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::irq::{IRQ_IPC_SYNC, IRQ_VBLANK};
    use crate::nds::Nds;

    /// Stamp a synthetic OS_Thread record into main RAM at `addr`.
    fn put_thread(nds: &mut Nds, addr: u32, state: u16) {
        let off = (addr & MAIN_RAM_MASK) as usize;
        let m = &mut nds.mem.main_ram[..];
        m[off + NITRO_THREAD_STATE_OFF as usize] = (state & 0xFF) as u8;
        m[off + NITRO_THREAD_STATE_OFF as usize + 1] = (state >> 8) as u8;
    }

    #[test]
    fn looks_like_thread_matches_known_states() {
        let mut ram = vec![0u8; 0x100];
        // state RUNNABLE at offset 0x4C.
        ram[0x4C] = 0x01;
        assert!(looks_like_thread(&ram, 0));
        ram[0x4C] = 0x08; // DEAD — not a candidate
        assert!(!looks_like_thread(&ram, 0));
    }

    #[test]
    fn find_os_thread_list_returns_first_address() {
        let mut nds = Nds::new();
        // Put a WAITING thread at 0x0220_0000 (inside scan region).
        put_thread(&mut nds, 0x0220_0000, OS_THREAD_STATE_WAITING);
        let addr = find_os_thread_list(&nds.mem.main_ram[..]);
        assert_eq!(addr, Some(0x0220_0000));
    }

    #[test]
    fn tick_no_op_when_running() {
        let mut nds = Nds::new();
        nds.state9.halted = false;
        assert!(!nds.nitro_os_tick());
        assert_eq!(nds.nitro_os.synthesized_wakes, 0);
    }

    #[test]
    fn tick_no_wake_with_pending_irq() {
        let mut nds = Nds::new();
        nds.state9.halted = true;
        nds.irq9.set_ie(IRQ_VBLANK);
        nds.irq9.raise(IRQ_VBLANK); // wake_pending() true
        for _ in 0..(DEADLOCK_FRAMES + 5) {
            assert!(!nds.nitro_os_tick());
        }
        assert_eq!(nds.nitro_os.synthesized_wakes, 0);
    }

    #[test]
    fn tick_synthesizes_wake_after_deadlock() {
        let mut nds = Nds::new();
        nds.state9.halted = true;
        put_thread(&mut nds, 0x0220_0000, OS_THREAD_STATE_WAITING);
        // No wake source, no IPC traffic, no DMA → counter accrues.
        let mut woke = false;
        for _ in 0..(DEADLOCK_FRAMES + 1) {
            if nds.nitro_os_tick() {
                woke = true;
                break;
            }
        }
        assert!(woke);
        assert_eq!(nds.nitro_os.synthesized_wakes, 1);
        // Raised IPCSYNC to lift the halt.
        assert_ne!(nds.irq9.iflag & IRQ_IPC_SYNC, 0);
        // Thread flipped to RUNNABLE; cond word holds its own address.
        let off = (0x0220_0000u32 & MAIN_RAM_MASK) as usize;
        assert_eq!(
            read16(&nds.mem.main_ram[..], off + THREAD_STATE_OFF as usize),
            OS_THREAD_STATE_RUNNABLE
        );
        assert_eq!(
            read32(&nds.mem.main_ram[..], off + THREAD_WAITING_OFF as usize),
            0x0220_0000
        );
    }

    #[test]
    fn legacy_tick_bump_gated_by_game_code() {
        // No cart mounted → no game code → bump disabled.
        let mut nds = Nds::new();
        let off = (SDK_TICK_COUNTER_ADDR & MAIN_RAM_MASK) as usize;
        write32(&mut nds.mem.main_ram[..], off, 0);
        nds.state9.halted = false;
        nds.nitro_os_tick();
        assert_eq!(nds.nitro_os.synthetic_ticks, 0);
        assert_eq!(read32(&nds.mem.main_ram[..], off), 0);
    }
}
