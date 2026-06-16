//! SPI bus (ARM7-only) — Power Management (device 0), Firmware flash (1),
//! Touchscreen TSC2046 (2). `Nds` owns one `Spi` on the ARM7 side; the ARM9 IO
//! map has no SPI. Ported from ../../ds-recomp/src/io/spi.ts.
//!
//! SPICNT (0x040001C0) selects the device + controls chip-select hold;
//! SPIDATA (0x040001C2) is the byte-exchange port. Each `write_data` shifts
//! ARM7's byte to the selected device and latches the device's reply for the
//! next `read_data`. We complete instantly and never report busy.
//!
//! Ownership: fully self-contained — no external deps. The live pointer state
//! (`touch_x` / `touch_y` / `touch_z` / `mic_sample`) is poked directly by the
//! UI/FFI and read by the touch driver and the ARM7 RTC/`Bus7` HLE.

/// 512 KB firmware blob to match retail DS firmware.
pub const FIRMWARE_SIZE: usize = 0x8_0000;

pub struct Spi {
    /// SPICNT (16-bit). Bits: 0-1 baud, 7 busy (we clear), 8-9 device select,
    /// 10 transfer size, 11 CS-hold, 13 IRQ-on-complete, 15 enable.
    pub cnt: u32,
    /// Response byte SPIDATA returns on the next read.
    pub data: u32,

    // ── Transaction state (reset on CS release / device change). ──
    pub device: u8,
    pub byte_pos: u32,
    pub fw_cmd: u8,
    pub fw_addr: u32,
    /// Write-Enable Latch (set by WREN 0x06, cleared by WRDI 0x04 / program).
    pub fw_wel: bool,
    pub tsc_channel: u8,
    /// Deferred CS release: when CS-hold falls 1→0 mid-transaction the end is
    /// deferred until after the NEXT byte completes (NitroSDK SPI driver
    /// relies on this — see spi.ts field doc).
    pub release_after_next: bool,

    /// 512 KB firmware image (boxed — large). Seeded with a plausible retail
    /// header + user-settings block in [`Spi::init_firmware`].
    pub firmware: Box<[u8; FIRMWARE_SIZE]>,

    /// 12-bit ADC mic-sample latch (TSC2046 AUX channel 6). Default = silence.
    pub mic_sample: u32,

    /// Power-management chip register file (device 0).
    pub pm_regs: [u8; 8],
    pub pm_cmd: u8,

    // ── Live pointer state (UI/FFI writes; touch driver + ADC read). ──
    /// Bottom-screen pixel coords when pressed; `None` when released.
    pub touch_x: Option<u32>,
    pub touch_y: Option<u32>,
    /// 12-bit pressure latch (0 = released, ~0x800 = pressed).
    pub touch_z: u32,
}

impl Default for Spi {
    fn default() -> Self {
        Self::new()
    }
}

impl Spi {
    pub fn new() -> Self {
        let mut spi = Spi {
            cnt: 0,
            data: 0,
            device: 0,
            byte_pos: 0,
            fw_cmd: 0,
            fw_addr: 0,
            fw_wel: false,
            tsc_channel: 0,
            release_after_next: false,
            firmware: Box::new([0u8; FIRMWARE_SIZE]),
            mic_sample: 0x800,
            pm_regs: [0u8; 8],
            pm_cmd: 0,
            touch_x: None,
            touch_y: None,
            touch_z: 0,
        };
        spi.init_firmware();
        spi
    }

    /// Seed the firmware header + touchscreen-calibration / user-settings block
    /// so games' firmware verifiers pass and the touch ADC mapping is sane.
    /// Mirrors GBATEK §"DS Firmware" layout; values follow melonDS-style HLE.
    fn init_firmware(&mut self) {
        let f = &mut *self.firmware;
        let w16 = |f: &mut [u8; FIRMWARE_SIZE], a: usize, v: u16| {
            f[a] = (v & 0xFF) as u8;
            f[a + 1] = (v >> 8) as u8;
        };

        // ── Firmware header (0x00..0x20). 16-bit offsets are in 8-byte units.
        w16(f, 0x00, 0x0020); // ARM9 GUI bootcode offset / 8
        w16(f, 0x02, 0x0040); // ARM7 GUI bootcode offset / 8
        w16(f, 0x04, 0x0080); // panic / decompression-src offset / 8
        w16(f, 0x06, 0x7F00); // WiFi data offset / 8 (~0x3F800)
        w16(f, 0x08, 0x3FF8); // ARM9 boot RAM dest >> 8
        w16(f, 0x0A, 0x3FF8); // ARM7 boot RAM dest >> 8
        f[0x0C] = 0xFF; // type = retail
        f[0x0D] = 0x00; // bootcode CRC8 placeholder
        w16(f, 0x0E, 0x4D17); // timestamp (arbitrary)
        f[0x1D] = 0x05; // firmware version

        // CRCs over the (zero-filled) GUI code regions — verifiers only check
        // self-consistency, so CRC(zeros) over zeros passes.
        let arm9_gui = crc16ccitt(f, 0x100, 0x800);
        let arm7_gui = crc16ccitt(f, 0x200, 0x800);
        w16(f, 0x14, arm9_gui);
        w16(f, 0x16, arm7_gui);

        // ── User-settings block at 0x3FE00 (latest slot).
        const OFF: usize = 0x3FE00;
        f[OFF] = 5; // version
        f[OFF + 0x01] = 0;

        // Touchscreen calibration: two reference points (ADC ↔ pixel).
        //   px(32, 32)   → adc(0x200, 0x200)
        //   px(224, 160) → adc(0xE00, 0xE00)
        w16(f, OFF + 0x58, 0x200); // adc X1
        w16(f, OFF + 0x5A, 0x200); // adc Y1
        f[OFF + 0x5C] = 32; // pixel X1
        f[OFF + 0x5D] = 32; // pixel Y1
        w16(f, OFF + 0x5E, 0xE00); // adc X2
        w16(f, OFF + 0x60, 0xE00); // adc Y2
        f[OFF + 0x62] = 224; // pixel X2
        f[OFF + 0x63] = 160; // pixel Y2

        // Language / flags (bits 0-2 = language; 1 = English).
        f[OFF + 0x64] = 0x01;

        // Update counter (must be non-zero so this slot wins).
        w16(f, OFF + 0x70, 0x0001);

        // CRC-16 over bytes 0x00..0x6F at 0x72.
        let crc = crc16ccitt(f, OFF, 0x70);
        w16(f, OFF + 0x72, crc);

        // Alternate slot at 0x3FF00 with a lower counter, so 0x3FE00 wins.
        f.copy_within(OFF..OFF + 0x100, 0x3FF00);
        w16(f, 0x3FF70, 0x0000);
        let alt_crc = crc16ccitt(f, 0x3FF00, 0x70);
        w16(f, 0x3FF72, alt_crc);
    }

    // ─── Register interface (routed by the ARM7 IO dispatch) ─────────────
    pub fn read_cnt(&self) -> u32 {
        self.cnt & 0xFFFF
    }

    /// SPICNT write: handles device change + chip-select-hold edge handling.
    pub fn write_cnt(&mut self, value: u32) {
        let old_hold = (self.cnt >> 11) & 1 != 0;
        let new_hold = (value >> 11) & 1 != 0;
        let new_dev = ((value >> 8) & 0x3) as u8;

        // Device change mid-transaction ends it immediately.
        if self.byte_pos > 0 && new_dev != self.device {
            self.end_transaction();
            self.release_after_next = false;
        }
        // CS-hold falling 1→0 mid-transaction: defer the end of the
        // transaction until after the NEXT byte completes.
        if old_hold && !new_hold && self.byte_pos > 0 {
            self.release_after_next = true;
        }

        self.device = new_dev;
        // Busy bit (7) always reads back clear — we complete instantly.
        self.cnt = value & 0xFFFF & !0x80;
    }

    pub fn read_data(&self) -> u32 {
        self.data & 0xFF
    }

    /// Exchange one byte with the selected device, latching the reply into
    /// `data` and advancing/ending the transaction per CS-hold.
    pub fn write_data(&mut self, value: u32) {
        let byte = (value & 0xFF) as u8;

        let response = match SpiDevice::from_select(self.device) {
            SpiDevice::PowerManagement => self.tick_power_management(byte),
            SpiDevice::Firmware => self.tick_firmware(byte),
            SpiDevice::Touchscreen => self.tick_touchscreen(byte),
            // Open bus pulls high.
            SpiDevice::Reserved => 0xFF,
        };

        self.data = response as u32;
        self.byte_pos += 1;

        // End the transaction when CS-hold is clear (single-byte transfer) or a
        // prior writeCnt deferred a release-after-next.
        let hold = (self.cnt >> 11) & 1 != 0;
        if !hold || self.release_after_next {
            self.end_transaction();
            self.release_after_next = false;
        }
    }

    // ─── Per-device state machines ───────────────────────────────────────

    /// Firmware flash (ST-style M25P40 / 45PE family) command interpreter.
    fn tick_firmware(&mut self, byte: u8) -> u8 {
        const MASK: u32 = (FIRMWARE_SIZE as u32) - 1;

        if self.byte_pos == 0 {
            self.fw_cmd = byte;
            self.fw_addr = 0;
            // Single-byte commands take effect at command latch.
            match byte {
                0x06 => self.fw_wel = true,  // WREN
                0x04 => self.fw_wel = false, // WRDI
                _ => {}
            }
            return 0xFF;
        }

        match self.fw_cmd {
            // READ
            0x03 => {
                if self.byte_pos <= 3 {
                    self.fw_addr = ((self.fw_addr << 8) | byte as u32) & 0xFF_FFFF;
                    return 0xFF;
                }
                let r = self.firmware[(self.fw_addr & MASK) as usize];
                self.fw_addr = (self.fw_addr + 1) & MASK;
                r
            }
            // RDSR — status register: bit 0 = WIP (busy, always 0), bit 1 = WEL.
            0x05 => {
                if self.fw_wel {
                    0x02
                } else {
                    0x00
                }
            }
            // PageProgram (0x02) / PageWrite (0x0A, erase+program).
            0x02 | 0x0A => {
                if self.byte_pos <= 3 {
                    self.fw_addr = ((self.fw_addr << 8) | byte as u32) & 0xFF_FFFF;
                    return 0xFF;
                }
                if self.fw_wel {
                    let base = (self.fw_addr & MASK) & !0xFF;
                    // Wrap within the 256-byte page.
                    let off = (self.fw_addr + (self.byte_pos - 4)) & 0xFF;
                    self.firmware[(base | off) as usize] = byte;
                }
                0xFF
            }
            // RDID — manufacturer / device ID.
            0x9F => match self.byte_pos {
                1 => 0x20, // mfr
                2 => 0x40, // dev hi
                3 => 0x12, // dev lo
                _ => 0xFF,
            },
            _ => 0xFF,
        }
    }

    /// TSC2046 touchscreen / mic ADC.
    fn tick_touchscreen(&mut self, byte: u8) -> u8 {
        if self.byte_pos == 0 {
            self.tsc_channel = (byte >> 4) & 0x7;
            return 0x00;
        }
        let value12 = self.adc_value_for_channel(self.tsc_channel);
        match self.byte_pos {
            1 => ((value12 >> 5) & 0x7F) as u8,
            2 => ((value12 & 0x1F) << 3) as u8,
            _ => 0x00,
        }
    }

    /// TSC2046 channel → 12-bit ADC value, derived from the live pointer state
    /// through the same calibration `init_firmware` stamps.
    fn adc_value_for_channel(&self, ch: u8) -> u32 {
        let pressed = self.touch_x.is_some() && self.touch_y.is_some();

        // Channels: 0=TEMP0, 1=Y, 2=BAT, 3=Z1, 4=Z2, 5=X, 6=AUX(mic), 7=TEMP1.
        if !pressed {
            return match ch {
                1 => 0xFFF,                 // Y reads max when not touched
                3 => 0xFFF,                 // Z1 = no pressure (high impedance)
                4 => 0x000,                 // Z2 = no pressure
                6 => self.mic_sample & 0xFFF, // AUX wired to mic preamp
                _ => 0x000,
            };
        }

        let touch_x = self.touch_x.unwrap();
        let touch_y = self.touch_y.unwrap();

        // Linear pixel→ADC map matching the stamped calibration points.
        let lerp = |px: u32, px_low: u32, px_high: u32| -> u32 {
            const ADC_LOW: i64 = 0x200;
            const ADC_HIGH: i64 = 0xE00;
            let num = (px as i64 - px_low as i64) * (ADC_HIGH - ADC_LOW);
            let den = px_high as i64 - px_low as i64;
            // Round-to-nearest.
            let v = ADC_LOW + (num + den / 2) / den;
            v.clamp(0, 0xFFF) as u32
        };

        match ch {
            1 => lerp(touch_y, 32, 160), // Y channel
            5 => lerp(touch_x, 32, 224), // X channel
            // Z1/Z2 pressure varies with contact position — bias by distance
            // from the panel midpoint (px 128) so reads aren't a stuck probe.
            3 => 0x100 + touch_x.abs_diff(128),
            4 => 0xE00 + touch_x.abs_diff(128),
            6 => self.mic_sample & 0xFFF, // AUX (mic)
            _ => 0x000,
        }
    }

    /// Power-management chip register file (device 0).
    fn tick_power_management(&mut self, byte: u8) -> u8 {
        // Byte 0 = command: bit 7 = R/W flag (1 = read), bits 0-6 = reg index.
        if self.byte_pos == 0 {
            self.pm_cmd = byte;
            return 0x00;
        }
        let reg = (self.pm_cmd & 0x7F) as usize;
        let is_read = self.pm_cmd & 0x80 != 0;
        if reg >= self.pm_regs.len() {
            return 0x00;
        }
        if is_read {
            self.pm_regs[reg]
        } else {
            self.pm_regs[reg] = byte;
            0x00
        }
    }

    /// Reset transaction scratch state at chip-select release.
    fn end_transaction(&mut self) {
        // A completed program/erase clears the Write-Enable Latch, like the
        // real flash — the SDK re-WRENs before every page.
        if matches!(self.fw_cmd, 0x02 | 0x0A) {
            self.fw_wel = false;
        }
        self.byte_pos = 0;
        self.fw_cmd = 0;
        self.fw_addr = 0;
        self.tsc_channel = 0;
        self.pm_cmd = 0;
    }
}

/// SPI bus device select (SPICNT bits 8-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpiDevice {
    PowerManagement,
    Firmware,
    Touchscreen,
    Reserved,
}

impl SpiDevice {
    fn from_select(sel: u8) -> Self {
        match sel & 0x3 {
            0 => SpiDevice::PowerManagement,
            1 => SpiDevice::Firmware,
            2 => SpiDevice::Touchscreen,
            _ => SpiDevice::Reserved,
        }
    }
}

/// CRC-16/CCITT (reflected, poly 0xA001, init 0xFFFF) over `buf[off..off+len]`.
fn crc16ccitt(buf: &[u8], off: usize, len: usize) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in &buf[off..off + len] {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a full transaction: assert CS-hold, push each byte, release on the
    /// final byte. Returns the response latched for each pushed byte.
    fn transact(spi: &mut Spi, device: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            // CS-hold (bit 11) set for all but the final byte; enable (15).
            let hold = if last { 0 } else { 1 << 11 };
            spi.write_cnt(0x8000 | hold | ((device as u32) << 8));
            spi.write_data(b as u32);
            out.push(spi.read_data() as u8);
        }
        out
    }

    #[test]
    fn firmware_header_and_user_settings_seeded() {
        let spi = Spi::new();
        let f = &*spi.firmware;
        assert_eq!(f[0x0C], 0xFF, "retail type byte");
        // User-settings update counter at 0x3FE70 must beat the alt slot.
        let main = u16::from_le_bytes([f[0x3FE70], f[0x3FE71]]);
        let alt = u16::from_le_bytes([f[0x3FF70], f[0x3FF71]]);
        assert!(main > alt, "primary slot must win the counter race");
        // Stamped CRC must validate over 0x00..0x6F.
        let crc = crc16ccitt(f, 0x3FE00, 0x70);
        let stored = u16::from_le_bytes([f[0x3FE72], f[0x3FE73]]);
        assert_eq!(crc, stored, "user-settings CRC must self-validate");
    }

    #[test]
    fn firmware_read_returns_calibration_block() {
        let mut spi = Spi::new();
        let cal = u16::from_le_bytes([spi.firmware[0x3FE58], spi.firmware[0x3FE59]]);
        // READ (0x03) addr 0x3FE58, then two dummy bytes to clock data out.
        let resp = transact(&mut spi, 1, &[0x03, 0x03, 0xFE, 0x58, 0x00, 0x00]);
        let lo = resp[4];
        let hi = resp[5];
        assert_eq!(u16::from_le_bytes([lo, hi]), cal);
    }

    #[test]
    fn firmware_wren_sets_wel_and_rdsr_reports_it() {
        let mut spi = Spi::new();
        // WREN.
        transact(&mut spi, 1, &[0x06]);
        assert!(spi.fw_wel);
        // RDSR returns 0x02 (WEL set) on the data byte.
        let resp = transact(&mut spi, 1, &[0x05, 0x00]);
        assert_eq!(resp[1], 0x02);
        // WRDI clears it.
        transact(&mut spi, 1, &[0x04]);
        assert!(!spi.fw_wel);
        let resp = transact(&mut spi, 1, &[0x05, 0x00]);
        assert_eq!(resp[1], 0x00);
    }

    #[test]
    fn firmware_page_program_writes_then_clears_wel() {
        let mut spi = Spi::new();
        transact(&mut spi, 1, &[0x06]); // WREN
        // PageProgram (0x02) at 0x000010, two data bytes.
        transact(&mut spi, 1, &[0x02, 0x00, 0x00, 0x10, 0xAB, 0xCD]);
        assert_eq!(spi.firmware[0x10], 0xAB);
        assert_eq!(spi.firmware[0x11], 0xCD);
        // Program auto-clears WEL.
        assert!(!spi.fw_wel);
    }

    #[test]
    fn firmware_program_ignored_without_wel() {
        let mut spi = Spi::new();
        let before = spi.firmware[0x20];
        // No WREN → write is dropped.
        transact(&mut spi, 1, &[0x02, 0x00, 0x00, 0x20, 0x55]);
        assert_eq!(spi.firmware[0x20], before);
    }

    #[test]
    fn firmware_rdid_returns_chip_id() {
        let mut spi = Spi::new();
        let resp = transact(&mut spi, 1, &[0x9F, 0x00, 0x00, 0x00]);
        assert_eq!(&resp[1..4], &[0x20, 0x40, 0x12]);
    }

    #[test]
    fn touchscreen_released_y_max_x_min() {
        let mut spi = Spi::new();
        spi.touch_x = None;
        spi.touch_y = None;
        // Y channel (1 << 4 = 0x10), two read bytes.
        let resp = transact(&mut spi, 2, &[0x10, 0x00, 0x00]);
        let y = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(y, 0xFFF, "released Y reads max");
        // X channel (5 << 4 = 0x50).
        let resp = transact(&mut spi, 2, &[0x50, 0x00, 0x00]);
        let x = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(x, 0x000, "released X reads min");
    }

    #[test]
    fn touchscreen_pressed_maps_through_calibration() {
        let mut spi = Spi::new();
        // Reference points should reproduce exactly.
        spi.touch_x = Some(32);
        spi.touch_y = Some(32);
        let resp = transact(&mut spi, 2, &[0x50, 0x00, 0x00]); // X
        let x = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(x, 0x200);
        let resp = transact(&mut spi, 2, &[0x10, 0x00, 0x00]); // Y
        let y = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(y, 0x200);

        spi.touch_x = Some(224);
        spi.touch_y = Some(160);
        let resp = transact(&mut spi, 2, &[0x50, 0x00, 0x00]);
        let x = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(x, 0xE00);
    }

    #[test]
    fn touchscreen_mic_channel_reads_latch_when_released() {
        let mut spi = Spi::new();
        spi.touch_x = None;
        spi.touch_y = None;
        spi.mic_sample = 0x123;
        // AUX channel (6 << 4 = 0x60).
        let resp = transact(&mut spi, 2, &[0x60, 0x00, 0x00]);
        let v = ((resp[1] as u32) << 5) | ((resp[2] as u32) >> 3);
        assert_eq!(v, 0x123);
    }

    #[test]
    fn power_management_write_then_read_roundtrips() {
        let mut spi = Spi::new();
        // Write reg 0 = 0x0D.
        transact(&mut spi, 0, &[0x00, 0x0D]);
        assert_eq!(spi.pm_regs[0], 0x0D);
        // Read reg 0 (cmd bit 7 set): data byte returns stored value.
        let resp = transact(&mut spi, 0, &[0x80, 0x00]);
        assert_eq!(resp[1], 0x0D);
    }

    #[test]
    fn cs_hold_release_is_deferred_one_byte() {
        let mut spi = Spi::new();
        // Start a READ with CS-hold asserted.
        spi.write_cnt(0x8000 | (1 << 11) | (1 << 8)); // device 1, hold
        spi.write_data(0x03); // cmd READ
        assert_eq!(spi.byte_pos, 1);
        // Address bytes still held.
        spi.write_data(0x00);
        spi.write_data(0x00);
        spi.write_data(0x10);
        assert_eq!(spi.byte_pos, 4);
        // Drop CS-hold mid-transaction: end deferred until after next byte.
        spi.write_cnt(0x8000 | (1 << 8)); // device 1, hold cleared
        assert!(spi.release_after_next);
        spi.write_data(0x00); // clocks one data byte, THEN ends
        assert_eq!(spi.byte_pos, 0, "transaction ended after the deferred byte");
        assert!(!spi.release_after_next);
    }

    #[test]
    fn device_change_mid_transaction_ends_it() {
        let mut spi = Spi::new();
        spi.write_cnt(0x8000 | (1 << 11) | (1 << 8)); // device 1, hold
        spi.write_data(0x03);
        assert_eq!(spi.byte_pos, 1);
        // Switch to device 2 mid-transaction.
        spi.write_cnt(0x8000 | (1 << 11) | (2 << 8));
        assert_eq!(spi.byte_pos, 0, "device change resets transaction");
        assert_eq!(spi.device, 2);
    }

    #[test]
    fn write_cnt_clears_busy_bit() {
        let mut spi = Spi::new();
        spi.write_cnt(0xFFFF);
        assert_eq!(spi.read_cnt() & 0x80, 0, "busy bit always reads clear");
    }
}
