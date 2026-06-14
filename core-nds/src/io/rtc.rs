//! DS Real-Time Clock at register 0x04000138 (ARM7-only) — a bit-banged 3-wire
//! serial interface to a Seiko S3511-style chip. `Nds` owns one `Rtc` on the
//! ARM7 side. Ported from ../../ds-recomp/src/io/rtc.ts.
//!
//! Register bits (per GBATEK): bit 0 DATA, bit 1 CLK, bit 2 SEL, bits 4-6 the
//! direction controls. Protocol: SEL high to start, 8 command bits MSB-first,
//! then response bytes (read) or written bytes; SEL low ends. Command byte =
//! `{R/W:1, cmd:3, tag:4=0110b}`; commands 0..7 are STATUS1/STATUS2/DATE+TIME/
//! TIME/ALARM1/ALARM2/CLK_ADJ/FREE.
//!
//! Ownership: fully self-contained. Date comes from the `host_date` field
//! (BCD-encoded broken-down time the FFI/host updates; defaults to a fixed
//! deterministic date so tests are reproducible).

/// Broken-down host date the RTC reports, in plain integers. The FFI/host can
/// overwrite this each frame with the wall clock; the device wave BCD-encodes
/// these in the DATE+TIME / TIME responses.
#[derive(Clone, Copy)]
pub struct HostDate {
    pub year: u32, // full year, e.g. 2026
    pub month: u32, // 1..12
    pub day: u32,  // 1..31
    pub weekday: u32, // 0..6 (0 = Sunday)
    pub hour: u32, // 0..23
    pub minute: u32,
    pub second: u32,
}

impl Default for HostDate {
    fn default() -> Self {
        // Fixed deterministic default (mirrors ds-recomp's dateProvider).
        HostDate {
            year: 2026,
            month: 6,
            day: 10,
            weekday: 3,
            hour: 14,
            minute: 30,
            second: 0,
        }
    }
}

#[derive(Default)]
pub struct Rtc {
    /// Current 16-bit value of 0x04000138.
    pub reg: u32,
    /// Last SEL / CLK levels (edge detection).
    pub sel_high: bool,
    pub clk_high: bool,

    /// Command-byte assembly.
    pub cmd_bits: u32,
    pub cmd: u32,
    pub byte_ready: bool,

    /// Read-response buffer + bit cursor.
    pub resp: Vec<u8>,
    pub resp_idx: u32,

    /// Master-write accumulation (MSB-first bytes after the command).
    pub wr_bits: u32,
    pub wr_cur: u32,
    pub wr_bytes: Vec<u8>,

    /// Persistent register state (writable when cmd-byte bit 7 = 0).
    pub status1: u8,
    pub status2: u8,
    pub alarm1: [u8; 3],
    pub alarm2: [u8; 3],
    pub clk_adj: u8,
    pub free_reg: u8,

    /// Date the RTC reports (host-updatable).
    pub host_date: HostDate,
}

impl Rtc {
    pub fn new() -> Self {
        Rtc {
            // Fresh chip: 24-hour mode (status1 bit 1), no alarms pending.
            status1: 0x02,
            ..Default::default()
        }
    }

    /// Read 0x04000138.
    pub fn read(&self) -> u32 {
        self.reg & 0xFFFF
    }

    /// Write 0x04000138 — drives the bit-banged SEL/CLK/DATA state machine.
    ///
    /// Register bits driven by the ARM7:
    ///   bit 0 DATA, bit 1 CLK, bit 2 SEL, bit 4 DATA direction (1 = ARM7→RTC).
    /// SEL rising starts a transaction; while SEL high every CLK rising edge
    /// shifts one bit — first the 8 command bits (MSB-first), then either
    /// response bits clocked out (read) or data bits clocked in (write).
    /// SEL falling commits any captured write bytes to the addressed register.
    pub fn write(&mut self, value: u32) {
        let new_sel = ((value >> 2) & 1) != 0;
        let new_clk = ((value >> 1) & 1) != 0;

        // SEL rising edge — start of a fresh transaction.
        if new_sel && !self.sel_high {
            self.cmd_bits = 0;
            self.cmd = 0;
            self.byte_ready = false;
            self.resp.clear();
            self.resp_idx = 0;
            self.wr_bits = 0;
            self.wr_cur = 0;
            self.wr_bytes.clear();
        }

        // CLK rising edge while selected — sample/shift one bit.
        if new_sel && new_clk && !self.clk_high {
            if !self.byte_ready {
                // Command-byte assembly, MSB-first.
                self.cmd = ((self.cmd << 1) & 0xFF) | (value & 1);
                self.cmd_bits += 1;
                if self.cmd_bits == 8 {
                    self.byte_ready = true;
                    // Bit 7 = R/W: 1 = read → prepare the response now.
                    if (self.cmd & 0x80) != 0 {
                        self.prepare_response();
                    }
                }
            } else if (self.cmd & 0x80) == 0 {
                // Master is writing data, MSB-first into wr_cur.
                self.wr_cur = ((self.wr_cur << 1) & 0xFF) | (value & 1);
                self.wr_bits += 1;
                if self.wr_bits == 8 {
                    self.wr_bytes.push(self.wr_cur as u8);
                    self.wr_cur = 0;
                    self.wr_bits = 0;
                }
            } else {
                // Master is reading data — emit the next response bit on bit 0.
                let byte_idx = (self.resp_idx / 8) as usize;
                let bit_idx = 7 - (self.resp_idx % 8);
                let v = self.resp.get(byte_idx).copied().unwrap_or(0) as u32;
                let bit = (v >> bit_idx) & 1;
                self.reg = (self.reg & !1) | bit;
                self.resp_idx += 1;
            }
        }

        // SEL falling edge — commit any captured write bytes, then idle.
        if !new_sel && self.sel_high {
            if self.byte_ready && (self.cmd & 0x80) == 0 && !self.wr_bytes.is_empty() {
                self.commit_write();
            }
            self.byte_ready = false;
        }

        self.sel_high = new_sel;
        self.clk_high = new_clk;
        // Persist the driven bits (CLK/SEL/direction) so reads of the register
        // see consistent state; the data bit (bit 0) may have been overwritten
        // above with the chip's outgoing reply bit, so leave it as-is.
        self.reg = (self.reg & !0xFE) | (value & 0xFE);
    }

    /// BCD-encode a small decimal (0..99) into one byte.
    fn bcd(n: u32) -> u8 {
        (((n / 10) << 4) | (n % 10)) as u8 & 0xFF
    }

    /// Fill `resp` for the current read command (top-nibble selects 0..7).
    fn prepare_response(&mut self) {
        let cmd_idx = (self.cmd >> 4) & 0x7;
        let d = self.host_date;
        self.resp.clear();
        match cmd_idx {
            0 => self.resp.push(self.status1),
            1 => self.resp.push(self.status2),
            2 => {
                // DATE+TIME: yr, mo, day, dow, hr, min, sec (all BCD).
                self.resp.extend_from_slice(&[
                    Self::bcd(d.year % 100),
                    Self::bcd(d.month),
                    Self::bcd(d.day),
                    Self::bcd(d.weekday),
                    Self::bcd(d.hour),
                    Self::bcd(d.minute),
                    Self::bcd(d.second),
                ]);
            }
            3 => {
                // TIME: hr, min, sec.
                self.resp.extend_from_slice(&[
                    Self::bcd(d.hour),
                    Self::bcd(d.minute),
                    Self::bcd(d.second),
                ]);
            }
            4 => self.resp.extend_from_slice(&self.alarm1),
            5 => self.resp.extend_from_slice(&self.alarm2),
            6 => self.resp.push(self.clk_adj),
            // 7 => FREE; the `& 0x7` mask makes this exhaustive.
            _ => self.resp.push(self.free_reg),
        }
    }

    /// Persist a completed master-write transaction to the addressed register.
    /// The host may shift in fewer bytes than the canonical width — the S3511
    /// accepts whatever it gets, so we update only the bytes provided.
    fn commit_write(&mut self) {
        let cmd_idx = (self.cmd >> 4) & 0x7;
        let b = &self.wr_bytes;
        match cmd_idx {
            0 => {
                if let Some(&v) = b.first() {
                    self.status1 = v;
                }
            }
            1 => {
                if let Some(&v) = b.first() {
                    self.status2 = v;
                }
            }
            // 2/3 DATE+TIME / TIME — host clock is authoritative, ignore writes.
            2 | 3 => {}
            4 => {
                for (dst, &src) in self.alarm1.iter_mut().zip(b.iter()) {
                    *dst = src;
                }
            }
            5 => {
                for (dst, &src) in self.alarm2.iter_mut().zip(b.iter()) {
                    *dst = src;
                }
            }
            6 => {
                if let Some(&v) = b.first() {
                    self.clk_adj = v;
                }
            }
            _ => {
                if let Some(&v) = b.first() {
                    self.free_reg = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bit-bang helpers mirroring how DS firmware drives 0x04000138. SEL = bit 2,
    // CLK = bit 1, DATA = bit 0, and we set the DATA-direction bit (4) when the
    // ARM7 is driving the line.
    const SEL: u32 = 1 << 2;
    const CLK: u32 = 1 << 1;
    const DIR_OUT: u32 = 1 << 4;

    fn start(rtc: &mut Rtc) {
        // SEL rising with CLK low.
        rtc.write(SEL | DIR_OUT);
    }

    fn stop(rtc: &mut Rtc) {
        // SEL falling.
        rtc.write(0);
    }

    // Clock one host-driven bit in: set DATA with CLK low, then raise CLK.
    fn clock_in(rtc: &mut Rtc, bit: u32) {
        let data = bit & 1;
        rtc.write(SEL | DIR_OUT | data); // CLK low
        rtc.write(SEL | DIR_OUT | CLK | data); // CLK rising
    }

    // Send a command byte MSB-first.
    fn send_cmd(rtc: &mut Rtc, cmd: u32) {
        for i in (0..8).rev() {
            clock_in(rtc, (cmd >> i) & 1);
        }
    }

    // Clock one chip-driven bit out (DATA direction = input): raise CLK, read bit 0.
    fn clock_out_bit(rtc: &mut Rtc) -> u32 {
        rtc.write(SEL); // CLK low, ARM7 not driving DATA
        rtc.write(SEL | CLK); // CLK rising — chip emits its bit
        rtc.read() & 1
    }

    // Read a response byte MSB-first (resp is consumed MSB-first per protocol).
    fn read_byte(rtc: &mut Rtc) -> u32 {
        let mut byte = 0u32;
        for _ in 0..8 {
            byte = (byte << 1) | clock_out_bit(rtc);
        }
        byte
    }

    // Command byte = {R/W:1, cmd:3 (bits6-4), tag:4=0110b}. Read sets bit 7.
    fn cmd_byte(cmd_idx: u32, read: bool) -> u32 {
        let rw = if read { 0x80 } else { 0x00 };
        rw | ((cmd_idx & 0x7) << 4) | 0x6
    }

    #[test]
    fn fresh_chip_status1_is_24h_mode() {
        let mut rtc = Rtc::new();
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(0, true));
        assert_eq!(read_byte(&mut rtc), 0x02);
        stop(&mut rtc);
    }

    #[test]
    fn datetime_read_yields_default_host_date_bcd() {
        let mut rtc = Rtc::new();
        // Default host date: 2026-06-10 (Wed=3) 14:30:00.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(2, true));
        let expected = [0x26, 0x06, 0x10, 0x03, 0x14, 0x30, 0x00];
        for &e in expected.iter() {
            assert_eq!(read_byte(&mut rtc), e);
        }
        stop(&mut rtc);
    }

    #[test]
    fn time_read_is_three_bytes() {
        let mut rtc = Rtc::new();
        rtc.host_date = HostDate {
            year: 2030,
            month: 1,
            day: 2,
            weekday: 4,
            hour: 23,
            minute: 59,
            second: 58,
        };
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(3, true));
        let expected = [0x23, 0x59, 0x58];
        for &e in expected.iter() {
            assert_eq!(read_byte(&mut rtc), e);
        }
        stop(&mut rtc);
    }

    #[test]
    fn status_write_then_read_round_trips() {
        let mut rtc = Rtc::new();
        // Write 0x44 to STATUS1.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(0, false));
        for i in (0..8).rev() {
            clock_in(&mut rtc, (0x44u32 >> i) & 1);
        }
        stop(&mut rtc);
        // Read it back.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(0, true));
        assert_eq!(read_byte(&mut rtc), 0x44);
        stop(&mut rtc);
    }

    #[test]
    fn alarm1_three_byte_write_round_trips() {
        let mut rtc = Rtc::new();
        let bytes = [0x11u32, 0x22, 0x33];
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(4, false));
        for &byte in bytes.iter() {
            for i in (0..8).rev() {
                clock_in(&mut rtc, (byte >> i) & 1);
            }
        }
        stop(&mut rtc);
        assert_eq!(rtc.alarm1, [0x11, 0x22, 0x33]);
        // Read back.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(4, true));
        for &b in bytes.iter() {
            assert_eq!(read_byte(&mut rtc), b);
        }
        stop(&mut rtc);
    }

    #[test]
    fn datetime_write_is_ignored_host_clock_authoritative() {
        let mut rtc = Rtc::new();
        let before = (rtc.host_date.year, rtc.host_date.hour);
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(2, false));
        for _ in 0..7 {
            for i in (0..8).rev() {
                clock_in(&mut rtc, (0xFFu32 >> i) & 1);
            }
        }
        stop(&mut rtc);
        assert_eq!((rtc.host_date.year, rtc.host_date.hour), before);
    }

    #[test]
    fn free_register_round_trips() {
        let mut rtc = Rtc::new();
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(7, false));
        for i in (0..8).rev() {
            clock_in(&mut rtc, (0xABu32 >> i) & 1);
        }
        stop(&mut rtc);
        assert_eq!(rtc.free_reg, 0xAB);
    }

    #[test]
    fn transaction_restarts_cleanly_on_sel_rising() {
        let mut rtc = Rtc::new();
        // Start a write but abort mid-byte by re-asserting SEL.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(7, false));
        clock_in(&mut rtc, 1);
        clock_in(&mut rtc, 1);
        stop(&mut rtc);
        // Fresh STATUS1 read must still work and ignore the aborted bits.
        start(&mut rtc);
        send_cmd(&mut rtc, cmd_byte(0, true));
        assert_eq!(read_byte(&mut rtc), 0x02);
        stop(&mut rtc);
    }
}
