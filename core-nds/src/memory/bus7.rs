//! ARM7 view of the DS memory map. Ported from
//! ../../ds-recomp/src/memory/bus7.ts.
//!
//! The ARM7 sees Main RAM, the shared WRAM block (depending on WRAMCNT), its
//! private 64 KB IWRAM at 0x03800000, ARM7-allocated VRAM banks (C/D), and a
//! separate IO + WiFi window. It has no TCMs.
//!
//! Ownership (see CONTRACT.md): like `Bus9`, this struct owns only ARM7-
//! private state — here, the Brain-Training touch-struct HLE flags. The
//! shared blocks + VRAM router are passed in as parameters. IO (region 0x4,
//! including the WiFi sub-window) is flagged via `Resolved::Io` / `Resolved::
//! Wifi` and routed by the god-struct.

use super::regions::{ARM7_IWRAM_MASK, MAIN_RAM_MASK, SHARED_WRAM_MASK, WIFI_BASE, WIFI_END};
use super::shared::{SharedMemory, WramCnt};
use super::vram_router::VramRouter;

/// ARM7 resolve result. Adds a `Wifi` variant on top of the shared
/// `bus9::Resolved` (the WiFi MMIO sub-window the ARM7 alone sees).
pub enum Resolved<'a> {
    Mem(&'a mut [u8], usize),
    Io,
    Wifi,
    None,
}

#[derive(Default)]
pub struct Bus7 {
    /// Brain Training HLE state. The touch driver updates these once per
    /// VBlank so write8/write16 to 0x027FFFA8-A9 can replace ARM7's "no valid
    /// touch" byte with the live cooked screen X.
    pub touch_pressed: bool,
    pub touch_screen_x: u32,
}

impl Bus7 {
    pub fn new() -> Self {
        Bus7::default()
    }

    #[inline]
    fn is_wifi(addr: u32) -> bool {
        (WIFI_BASE..WIFI_END).contains(&addr)
    }
    #[inline]
    fn is_io(addr: u32) -> bool {
        (addr >> 24) == 0x04
    }

    /// Translate an address to a `(slice, index)` backing store, or to one of
    /// the IO/WiFi markers, or unmapped. The WiFi sub-window is detected
    /// before the general IO dispatch so the WiFi stub can shape the response.
    pub fn resolve<'a>(
        &self,
        addr: u32,
        mem: &'a mut SharedMemory,
        vram: &VramRouter,
        vramcnt: &[u8; 9],
    ) -> Resolved<'a> {
        if Self::is_wifi(addr) {
            return Resolved::Wifi;
        }
        if Self::is_io(addr) {
            return Resolved::Io;
        }
        if addr < 0x4000 {
            return Resolved::Mem(&mut mem.bios_arm7[..], addr as usize);
        }
        if (addr >> 24) == 0x02 {
            let idx = (addr & MAIN_RAM_MASK) as usize;
            return Resolved::Mem(&mut mem.main_ram[..], idx);
        }
        // ARM7-allocated VRAM banks (C or D with VRAMCNT_x.MST = 2).
        if (0x0600_0000..0x0604_0000).contains(&addr) {
            return match vram.resolve_arm7(addr, vramcnt) {
                Some(idx) => Resolved::Mem(&mut mem.vram[..], idx),
                None => Resolved::None,
            };
        }
        if (addr >> 24) == 0x03 {
            // 0x03800000+ is always ARM7-private IWRAM (64 KB).
            if addr >= 0x0380_0000 {
                let idx = (addr & ARM7_IWRAM_MASK) as usize;
                return Resolved::Mem(&mut mem.arm7_iwram[..], idx);
            }
            // 0x03000000-0x037FFFFF is shared WRAM, gated by WRAMCNT (the four
            // ARM7-visible mappings are complementary to ARM9's).
            return match mem.wramcnt {
                WramCnt::AllToArm9 => {
                    // ARM7 sees its IWRAM mirror here.
                    let idx = (addr & ARM7_IWRAM_MASK) as usize;
                    Resolved::Mem(&mut mem.arm7_iwram[..], idx)
                }
                WramCnt::UpperToArm9 => {
                    // ARM7 sees the 1st (lower 16 KB) half.
                    let idx = (addr & 0x3FFF) as usize;
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
                WramCnt::LowerToArm9 => {
                    // ARM7 sees the 2nd (upper 16 KB) half.
                    let idx = 0x4000 + (addr & 0x3FFF) as usize;
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
                WramCnt::AllToArm7 => {
                    let idx = (addr & SHARED_WRAM_MASK) as usize;
                    Resolved::Mem(&mut mem.shared_wram[..], idx)
                }
            };
        }
        Resolved::None
    }

    /// Brain Training (SDK 1.x) touch-struct HLE for 8-bit writes. Byte +1 of
    /// the cooked touch struct at 0x027FFFA8 must carry the screen X for the
    /// game's gate to fire; ARM7's touch task writes 0x2C there every frame
    /// ("no valid touch") regardless of what our TSC2046 returns. Intercept
    /// the byte-1 write and replace with the cooked X when touch is pressed.
    /// Returns the possibly-substituted value.
    #[inline]
    pub fn munge_write8(&self, addr: u32, v: u32) -> u32 {
        if addr == 0x027F_FFA9 && self.touch_pressed {
            return self.touch_screen_x & 0xFF;
        }
        v
    }

    /// Same touch HLE for 16-bit writes (the struct's low halfword 0x2C00; we
    /// override the high byte with the screen X).
    #[inline]
    pub fn munge_write16(&self, addr: u32, v: u32) -> u32 {
        if addr == 0x027F_FFA8 && self.touch_pressed {
            return (v & 0x00FF) | ((self.touch_screen_x & 0xFF) << 8);
        }
        v
    }

    /// Nintendo SDK shared-OS-init-flags word at 0x027FFF8C. The real DS BIOS
    /// sets some of these bits during the boot stub before ARM7's binary takes
    /// over (touchscreen ADC + RTC subsystem init). We HLE the BIOS to the
    /// bare minimum and don't run those subsystems, so force-OR the missing
    /// bits into every 32-bit write so the boot-info word matches what the
    /// BIOS would have produced.
    ///   Bit 8 = touchscreen/RTC ready (NSMB, Nintendogs).
    ///   Bit 5 = sound subsystem ready (Tetris DS).
    ///   Bit 0 = "BIOS boot completed" indicator.
    #[inline]
    pub fn munge_write32(&self, addr: u32, v: u32) -> u32 {
        if addr == 0x027F_FF8C {
            return v | 0x121;
        }
        v
    }
}
