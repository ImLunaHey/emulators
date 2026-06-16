//! Memory blocks that BOTH the ARM9 and ARM7 cores can touch (with
//! optional routing). Ported from ../../ds-recomp/src/memory/shared.ts.
//!
//! In the TS these are constructed once at the emulator level and the same
//! `Uint8Array` references are handed to both buses, so a write from ARM9 is
//! observable from ARM7 next cycle (and vice versa) — that's how the real
//! hardware behaves for Main RAM and the shared WRAM block.
//!
//! In Rust we resolve the shared-ownership the GBA-core way: the top-level
//! `Nds` god-struct owns a single `SharedMemory`, and both `Bus9`/`Bus7`
//! borrow it as a `&mut` parameter on their access methods rather than
//! storing a reference. So there is exactly one backing copy and both CPUs'
//! accessors route through it.
//!
//! Large regions are heap-allocated as boxed fixed-size arrays (never placed
//! on the stack), matching the GBA core's `boxed_region`.

use super::regions as R;

/// Heap-allocate a zeroed fixed-size region without ever placing `N` bytes on
/// the stack. `Box::new([0; N])` would build the array on the stack first;
/// `vec![0; N].into_boxed_slice()` allocates straight on the heap.
#[inline]
pub(crate) fn boxed_region<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().unwrap()
}

/// WRAMCNT split mode for the 32 KB shared WRAM block. ARM9-only writable.
/// Per GBATEK §"WRAMCNT" — the value the ARM9 picks decides how much of the
/// block each CPU sees and at what offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WramCnt {
    /// 0 → all 32 KB to ARM9; ARM7 sees its IWRAM mirror at 0x03000000.
    AllToArm9 = 0,
    /// 1 → ARM9 sees the upper 16 KB; ARM7 sees the lower 16 KB.
    UpperToArm9 = 1,
    /// 2 → ARM9 sees the lower 16 KB; ARM7 sees the upper 16 KB.
    LowerToArm9 = 2,
    /// 3 → all 32 KB to ARM7; ARM9 sees zeros/open-bus at 0x03000000.
    AllToArm7 = 3,
}

impl WramCnt {
    #[inline]
    pub fn from_bits(v: u32) -> Self {
        match v & 0x3 {
            0 => WramCnt::AllToArm9,
            1 => WramCnt::UpperToArm9,
            2 => WramCnt::LowerToArm9,
            _ => WramCnt::AllToArm7,
        }
    }
    #[inline]
    pub fn bits(self) -> u32 {
        self as u32
    }
}

/// The memory blocks shared between the two CPUs (with WRAMCNT/VRAM routing).
/// Heap-allocated boxed arrays — there is exactly ONE of these in `Nds`.
pub struct SharedMemory {
    /// 4 MB Main RAM — both CPUs see the same bytes.
    pub main_ram: Box<[u8; R::MAIN_RAM_SIZE]>,
    /// 32 KB shared WRAM block — split between CPUs by WRAMCNT.
    pub shared_wram: Box<[u8; R::SHARED_WRAM_SIZE]>,
    /// 64 KB ARM7-only IWRAM.
    pub arm7_iwram: Box<[u8; R::ARM7_IWRAM_SIZE]>,
    /// 2 KB palette RAM (engine A 1 KB + engine B 1 KB).
    pub pram: Box<[u8; R::PRAM_SIZE]>,
    /// 2 KB OAM (engine A 1 KB + engine B 1 KB).
    pub oam: Box<[u8; R::OAM_SIZE]>,
    /// 656 KB VRAM (partitioned by bank-routing via the `VramRouter`).
    pub vram: Box<[u8; R::VRAM_TOTAL_SIZE]>,

    /// WRAMCNT — how the 32 KB shared block is split between the CPUs.
    ///
    /// After reset, real hardware has WRAMCNT=0. The ARM9 BIOS then sets it
    /// to 3 (all-to-ARM7) before signaling ARM7 to take over. We run both
    /// CPUs concurrently with no BIOS handoff, so initializing to 3 here
    /// matches what the ARM9 would have done — and Pokemon Platinum's ARM7
    /// autoload writes 0x037F8000+ expecting shared WRAM mapped there.
    pub wramcnt: WramCnt,

    /// Tiny BIOS regions, one per CPU. Reads from 0x00000000..0x00003FFF
    /// (and 0xFFFF0000..0xFFFF3FFF on ARM9) hit these. The CPU module
    /// pre-loads a canonical IRQ-dispatch stub at offset 0x18 so any IRQ
    /// taken on the exception vector finds something to execute.
    pub bios_arm7: Box<[u8; R::BIOS_SIZE]>,
    pub bios_arm9: Box<[u8; R::BIOS_SIZE]>,
}

impl Default for SharedMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedMemory {
    pub fn new() -> Self {
        SharedMemory {
            main_ram: boxed_region(),
            shared_wram: boxed_region(),
            arm7_iwram: boxed_region(),
            pram: boxed_region(),
            oam: boxed_region(),
            vram: boxed_region(),
            wramcnt: WramCnt::AllToArm7,
            bios_arm7: boxed_region(),
            bios_arm9: boxed_region(),
        }
    }

    /// Load the 4 MB Main RAM image (or any prefix of it). Used by tests and
    /// the cart loader to seed RAM.
    pub fn load_main_ram(&mut self, bytes: &[u8], offset: usize) {
        let n = bytes.len().min(R::MAIN_RAM_SIZE - offset.min(R::MAIN_RAM_SIZE));
        self.main_ram[offset..offset + n].copy_from_slice(&bytes[..n]);
    }
}
