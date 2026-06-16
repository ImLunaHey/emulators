//! Touch driver — synthesizes the cooked NitroSDK touch sample into the OS
//! shared-work region in main RAM once per VBlank, skipping the real ARM7 PXI
//! roundtrip we don't model. `Nds` owns one `TouchDriver`. Ported from
//! ../../ds-recomp/src/io/touch_driver.ts.
//!
//! ## Ownership / borrow strategy (the device wave must keep this)
//!
//! The TS driver read `emu.spi.touchX/Y/Z` and wrote the cooked sample through
//! `emu.bus9.write8/16`, then poked `emu.bus7.touchPressed/ScreenX`. All three
//! live on `Nds`, and writing through the ARM9 bus needs `&mut Nds`. So the
//! per-VBlank tick is a method that takes `&mut Nds` — it reads `nds.spi`,
//! writes via `nds.write8_arm9`/`write16_arm9`, and sets `nds.bus7.touch_*`.
//! `Nds` exposes `Nds::touch_tick_vblank(&mut self)` which delegates here with
//! the driver temporarily taken out (or, since `TouchDriver` holds no memory,
//! the orchestrator can call `self.touch.tick_vblank(spi, …)` with split
//! borrows — see the signature note in the struct return).

/// Cooked touch struct base in main RAM (top of the 0x027FFFxx OS shared-work
/// area). Layout: +0 pressed u8, +1 reserved (Brain Training reads X here),
/// +2 x u16, +4 y u16, +6 updateFrame u8.
pub const TOUCH_STRUCT_BASE: u32 = 0x027F_FFA8;
pub const TOUCH_PRESSED_OFFSET: u32 = 0x00;
pub const TOUCH_X_OFFSET: u32 = 0x02;
pub const TOUCH_Y_OFFSET: u32 = 0x04;
pub const TOUCH_FRAME_OFFSET: u32 = 0x06;

/// Pressure threshold for "pen down" (UI writes 0 released / ~0x800 pressed).
pub const PRESSURE_THRESHOLD: u32 = 0x100;

#[derive(Default)]
pub struct TouchDriver {
    /// When false the driver is a no-op (escape hatch if it regresses a game).
    pub enabled: bool,
    /// Incrementing nonzero "new data" byte written at +0x06.
    pub update_frame: u32,
}

impl TouchDriver {
    pub fn new() -> Self {
        TouchDriver {
            enabled: true,
            update_frame: 0,
        }
    }

    /// Cook the current pointer state into `(pressed, screen_x, screen_y,
    /// update_frame)`, advancing the internal frame counter. Pure — no memory
    /// access. `Nds::touch_tick_vblank` calls this, then writes the result into
    /// main RAM via the ARM9 bus and updates `Bus7`'s touch HLE flags. Splits
    /// the memory-touching half off so the borrow checker stays happy.
    ///
    /// Returns `None` when disabled (the orchestrator then skips the write).
    pub fn cook(&mut self, touch_x: Option<u32>, touch_y: Option<u32>, touch_z: u32)
        -> Option<CookedTouch>
    {
        if !self.enabled {
            return None;
        }

        // touch_x/touch_y are nullable (None = "released by UI"); touch_z is the
        // pressure latch (0 = released, ~0x800 = pressed).
        let x = touch_x.unwrap_or(0);
        let y = touch_y.unwrap_or(0);
        let pressed = touch_z > PRESSURE_THRESHOLD;

        // Clamp to bottom-screen ranges (256x192) so a stray UI value can't
        // write garbage into the u16 fields. When released, X/Y are zeroed —
        // games sometimes use (0, 0) as a "no contact" sentinel alongside
        // pressed = 0.
        let screen_x = if pressed { x.min(255) } else { 0 };
        let screen_y = if pressed { y.min(191) } else { 0 };

        // Wrap to nonzero u8. Games may treat 0 as "no sample ever arrived",
        // so we bump BEFORE returning — the first tick stamps 1 — and skip 0
        // on wrap-around for the same reason.
        self.update_frame = (self.update_frame + 1) & 0xFF;
        if self.update_frame == 0 {
            self.update_frame = 1;
        }

        Some(CookedTouch {
            pressed,
            screen_x,
            screen_y,
            update_frame: self.update_frame,
        })
    }
}

/// The cooked sample `Nds::touch_tick_vblank` writes into main RAM + `Bus7`.
///
/// The memory-touching half (which `Nds` owns) writes, via the ARM9 bus:
///   - u8  `pressed`     at `TOUCH_STRUCT_BASE + TOUCH_PRESSED_OFFSET`
///   - u16 `screen_x`    at `TOUCH_STRUCT_BASE + TOUCH_X_OFFSET`
///   - u16 `screen_y`    at `TOUCH_STRUCT_BASE + TOUCH_Y_OFFSET`
///   - u8  `update_frame`at `TOUCH_STRUCT_BASE + TOUCH_FRAME_OFFSET`
///   - u8  `screen_x & 0xFF` at `TOUCH_STRUCT_BASE + 0x01` (SDK-1.x byte-+1 X
///     layout — Brain Training / DS Training reads X here, not as the u16 at
///     +2). `screen_x` is already clamped to 0..=255 so the `& 0xFF` is a no-op
///     but kept for documentation parity with the layout note.
///
/// It also hands the cooked state to `Bus7` so ARM7's mid-frame writes of the
/// "no valid touch" marker get rewritten with the live X:
///   - `bus7.touch_pressed = pressed`
///   - `bus7.touch_screen_x = screen_x & 0xFF`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CookedTouch {
    pub pressed: bool,
    pub screen_x: u32,
    pub screen_y: u32,
    pub update_frame: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressed_with_coords_when_z_exceeds_threshold() {
        let mut t = TouchDriver::new();
        let c = t.cook(Some(128), Some(96), 0x800).unwrap();
        assert!(c.pressed);
        assert_eq!(c.screen_x, 128);
        assert_eq!(c.screen_y, 96);
        // updateFrame should be nonzero on the very first tick.
        assert_ne!(c.update_frame, 0);
    }

    #[test]
    fn released_when_z_zero() {
        let mut t = TouchDriver::new();
        let c = t.cook(Some(128), Some(96), 0).unwrap();
        assert!(!c.pressed);
        assert_eq!(c.screen_x, 0);
        assert_eq!(c.screen_y, 0);
    }

    #[test]
    fn update_frame_increments_each_tick() {
        let mut t = TouchDriver::new();
        let f1 = t.cook(Some(50), Some(50), 0x800).unwrap().update_frame;
        let f2 = t.cook(Some(50), Some(50), 0x800).unwrap().update_frame;
        let f3 = t.cook(Some(50), Some(50), 0x800).unwrap().update_frame;
        assert_ne!(f2, f1);
        assert_ne!(f3, f2);
        assert!(f1 <= 0xFF && f3 <= 0xFF);
    }

    #[test]
    fn update_frame_wraps_to_nonzero() {
        let mut t = TouchDriver::new();
        // Advance well past a full u8 cycle; at update_frame==0xFF the next
        // tick computes 0x100 & 0xFF == 0, which must be lifted back to 1.
        let mut last = 0;
        for _ in 0..600 {
            last = t.cook(Some(1), Some(1), 0x800).unwrap().update_frame;
            assert_ne!(last, 0);
        }
        let _ = last;
    }

    #[test]
    fn disabled_returns_none() {
        let mut t = TouchDriver::new();
        t.enabled = false;
        assert!(t.cook(Some(200), Some(150), 0x800).is_none());
    }

    #[test]
    fn clamps_out_of_range_coords() {
        let mut t = TouchDriver::new();
        let c = t.cook(Some(500), Some(300), 0x800).unwrap();
        assert_eq!(c.screen_x, 255);
        assert_eq!(c.screen_y, 191);
    }

    #[test]
    fn none_coords_with_pressure_yield_zero_coords() {
        let mut t = TouchDriver::new();
        let c = t.cook(None, None, 0x800).unwrap();
        assert!(c.pressed);
        assert_eq!(c.screen_x, 0);
        assert_eq!(c.screen_y, 0);
    }

    #[test]
    fn low_pressure_not_treated_as_pressed() {
        let mut t = TouchDriver::new();
        // 0x50 is below the 0x100 PRESSURE_THRESHOLD.
        let c = t.cook(Some(128), Some(96), 0x50).unwrap();
        assert!(!c.pressed);
        assert_eq!(c.screen_x, 0);
        assert_eq!(c.screen_y, 0);
    }

    #[test]
    fn threshold_boundary_exclusive() {
        let mut t = TouchDriver::new();
        // Exactly at the threshold is NOT pressed (TS uses `z > THRESHOLD`).
        assert!(!t.cook(Some(10), Some(10), PRESSURE_THRESHOLD).unwrap().pressed);
        assert!(t.cook(Some(10), Some(10), PRESSURE_THRESHOLD + 1).unwrap().pressed);
    }

    #[test]
    fn drag_tracks_each_sample() {
        let mut t = TouchDriver::new();
        for (x, y) in [(10u32, 20u32), (50, 60), (100, 100), (200, 150)] {
            let c = t.cook(Some(x), Some(y), 0x800).unwrap();
            assert_eq!(c.screen_x, x);
            assert_eq!(c.screen_y, y);
            assert_eq!(c.screen_x & 0xFF, x & 0xFF);
        }
    }
}
