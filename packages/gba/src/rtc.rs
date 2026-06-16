// Seiko S-3511A RTC sitting on GPIO at ROM 0x080000C4 / 0xC6 / 0xC8.
// FireRed reads the date/time for berry growth and Pokemon time-based
// events. We bit-bang a minimal subset of the SIO protocol that the
// official RTC library uses: reset, status read, datetime read.

// Pin meanings (from GPIO_DATA register at 0xC4):
//   bit 0 = SCK (clock, in)
//   bit 1 = SIO (data, bidirectional)
//   bit 2 = CS  (chip select, in)

// Time-source seam:
//   The TS port read the real wall clock via `new Date()` inside
//   `dateTimeBcd()`. To keep the differential harness deterministic, the
//   Rust port factors the clock out behind an injectable source. `Rtc`
//   holds a `now_fn: fn() -> DateTime` which defaults to a FIXED epoch
//   (see `default_now`). The host (web/wasm or native) replaces it via
//   `set_now_fn(...)` to wire a real JS `Date` / `SystemTime`-derived
//   clock. The struct stays `Default`-able and the protocol bit-banging is
//   byte-for-byte identical to the TS — only the source of the seven
//   date/time components differs.

/// Broken-down local date/time, mirroring the JS `Date` accessors the TS
/// used (`getFullYear`, `getMonth` (0-based), `getDate`, `getDay`
/// (0=Sunday), `getHours`, `getMinutes`, `getSeconds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// Full year, e.g. 2000. (TS used `d.getFullYear()`.)
    pub year: u32,
    /// Month 1..=12. (TS used `d.getMonth() + 1`.)
    pub month: u32,
    /// Day of month 1..=31. (TS used `d.getDate()`.)
    pub day: u32,
    /// Day of week 0..=6, Sunday = 0. (TS used `d.getDay()`.)
    pub dow: u32,
    /// Hour 0..=23. (TS used `d.getHours()`.)
    pub hour: u32,
    /// Minute 0..=59. (TS used `d.getMinutes()`.)
    pub minute: u32,
    /// Second 0..=59. (TS used `d.getSeconds()`.)
    pub second: u32,
}

/// Fixed default clock — deterministic for the differential harness.
/// 2000-01-01 00:00:00, which was a Saturday (dow = 6).
fn default_now() -> DateTime {
    DateTime {
        year: 2000,
        month: 1,
        day: 1,
        dow: 6,
        hour: 0,
        minute: 0,
        second: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtcState {
    Idle,
    Cmd,
    Reply,
    Recv,
}

pub struct Rtc {
    pub enabled: bool,
    selected: bool, // CS high
    clk: u32,
    data: u32,
    dir: u32, // GPIO_DIR — bits set = pin is OUT (CPU writes)
    state: RtcState,
    buffer: u32,
    bits: u32,
    cmd: u32,
    payload: Vec<u32>,
    cursor: usize,
    // Status byte: initial value when chip first powers up. Real S-3511A
    // hardware returns 0x82 here when the battery has just been installed
    // (POW = 1, BUSY = 0, 24H not yet set, ALARM disabled). Pokemon
    // Ruby/Sapphire/Emerald check this on boot: if POW = 1 they prompt
    // to set the date and clear POW; if POW = 0 they assume the chip is
    // already running. Without POW=1 on the very first read after boot,
    // the game treats the response as "RTC says it's running but I never
    // told it to" and prints "the internal battery has run dry".
    status: u32,
    #[allow(dead_code)]
    has_been_initialized: bool,
    payload_len: usize,
    // Injectable time source (see "Time-source seam" above). Defaults to a
    // fixed epoch so the differential harness is deterministic.
    now_fn: fn() -> DateTime,
}

impl Default for Rtc {
    fn default() -> Self {
        Self {
            enabled: false,
            selected: false,
            clk: 0,
            data: 0,
            dir: 0,
            state: RtcState::Idle,
            buffer: 0,
            bits: 0,
            cmd: 0,
            payload: Vec::new(),
            cursor: 0,
            status: 0x80,
            has_been_initialized: false,
            payload_len: 0,
            now_fn: default_now,
        }
    }
}

impl Rtc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a custom clock source (host wires this to a real
    /// JS `Date` / `SystemTime`-derived clock). Defaults to a fixed epoch.
    pub fn set_now_fn(&mut self, now_fn: fn() -> DateTime) {
        self.now_fn = now_fn;
    }

    pub fn read(&self, off: u32) -> u32 {
        // 0xC4 = data, 0xC6 = dir, 0xC8 = enable.
        match off {
            0xC4 => {
                if !self.enabled {
                    return 0;
                }
                let sck = self.clk & 1;
                let sio = self.data & 1;
                let cs = if self.selected { 1 } else { 0 };
                // Only return bits where the host pin is configured as INPUT.
                let in_mask = (!self.dir) & 0x7;
                ((sck | (sio << 1) | (cs << 2)) & in_mask) & 0xFFFF_FFFF
            }
            0xC6 => self.dir,
            0xC8 => {
                if self.enabled {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0xC4 => {
                if !self.enabled {
                    return;
                }
                let new_cs = (v >> 2) & 1;
                let new_sck = v & 1;
                let new_sio = (v >> 1) & 1;

                if !self.selected && new_cs != 0 {
                    self.state = RtcState::Cmd;
                    self.bits = 0;
                    self.buffer = 0;
                } else if self.selected && new_cs == 0 {
                    self.state = RtcState::Idle;
                }
                self.selected = new_cs == 1;

                // Rising edge of SCK while CS high → clock in/out a bit.
                let sck_rising = self.clk == 0 && new_sck != 0;
                let _sck_falling = self.clk != 0 && new_sck == 0;
                self.clk = new_sck;

                if self.selected {
                    // S-3511A clocks both directions on the RISING edge of SCK:
                    // the host sets SIO while SCK is low, then raises SCK; the chip
                    // samples the host's SIO on that rising edge, and emits its
                    // outgoing bit so the host can read it after the same rising
                    // edge. Previously we were sampling on the falling edge, which
                    // gave us the bit from the *previous* SIO setup — the AGB SDK's
                    // status-register probe never read its own write back, and
                    // Pokemon Ruby/Sapphire/Emerald flag that as "battery dry".
                    if self.state == RtcState::Cmd && sck_rising {
                        // Host writes command MSB-first.
                        self.buffer = ((self.buffer << 1) | new_sio) & 0xFF;
                        self.bits += 1;
                        if self.bits == 8 {
                            self.begin_command(self.buffer);
                        }
                    } else if self.state == RtcState::Reply && sck_rising {
                        // S-3511A device replies LSB-first, so the host's bit 0 (the
                        // very first one clocked out) is the data byte's bit 0. That
                        // matches our `(byte >> this.bits) & 1` indexing.
                        let byte = self.payload.get(self.cursor).copied().unwrap_or(0);
                        self.data = (byte >> self.bits) & 1;
                        self.bits += 1;
                        if self.bits == 8 {
                            self.bits = 0;
                            self.cursor += 1;
                            if self.cursor >= self.payload.len() {
                                self.state = RtcState::Idle;
                            }
                        }
                    } else if self.state == RtcState::Recv && sck_rising {
                        // Host writes one byte LSB-first.
                        self.buffer = (self.buffer | (new_sio << self.bits)) & 0xFF;
                        self.bits += 1;
                        if self.bits == 8 {
                            self.payload.push(self.buffer);
                            self.buffer = 0;
                            self.bits = 0;
                            self.cursor += 1;
                            if self.cursor >= self.payload_len {
                                self.finish_write();
                            }
                        }
                    }
                }
                // SIO mirror: ONLY during cmd/recv (host is driving) do we follow
                // newSio. In reply state the chip is driving and we must preserve
                // the bit we put on the line on the rising edge above; otherwise
                // the host always reads back its own (typically zero) SIO write
                // instead of the chip's reply data.
                if self.state == RtcState::Cmd || self.state == RtcState::Recv {
                    self.data = new_sio;
                }
            }
            0xC6 => self.dir = v & 0x7,
            0xC8 => self.enabled = (v & 1) == 1,
            _ => {}
        }
    }

    fn begin_command(&mut self, cmd: u32) {
        self.cmd = cmd;
        self.cursor = 0;
        self.bits = 0;
        // Critical: must clear `buffer` here too. The recv path ORs new bits
        // into `buffer`, so any leftover bits from the command byte would
        // leak straight into the data byte. (Pokemon Ruby writes 0x42 to
        // status register; without this reset, buffer stayed at 0x62 from
        // the preceding write-status command and the writeback "succeeded"
        // with a corrupted value, so the subsequent status read mismatched
        // and the game flagged "battery has run dry".)
        self.buffer = 0;
        self.payload = Vec::new();

        // S-3511A command byte: bits 7..4 = 0110, bit 3..1 = reg, bit 0 = R/W (1 = read).
        let reg = (cmd >> 1) & 0x7;
        let reading = (cmd & 1) == 1;

        match reg {
            0 => {
                // Reset / force.
                self.status = 0x40;
                self.state = RtcState::Idle;
            }
            1 => {
                // Status.
                if reading {
                    self.payload = vec![self.status];
                    self.state = RtcState::Reply;
                } else {
                    self.payload_len = 1;
                    self.state = RtcState::Recv;
                }
            }
            2 => {
                // Date/time (7 bytes BCD).
                if reading {
                    self.payload = self.date_time_bcd();
                    self.state = RtcState::Reply;
                } else {
                    self.payload_len = 7;
                    self.state = RtcState::Recv;
                }
            }
            3 => {
                // Time only (3 bytes).
                if reading {
                    self.payload = self.date_time_bcd()[4..].to_vec();
                    self.state = RtcState::Reply;
                } else {
                    self.payload_len = 3;
                    self.state = RtcState::Recv;
                }
            }
            _ => {
                self.state = RtcState::Idle;
            }
        }
    }

    fn finish_write(&mut self) {
        // Status writes: store the byte so subsequent reads match. Pokemon
        // Ruby/Sapphire/Emerald write status then read it back; mismatch is
        // reported as "battery has run dry".
        let reg = (self.cmd >> 1) & 0x7;
        if reg == 1 && !self.payload.is_empty() {
            self.status = self.payload[0];
        }
        // Date/time writes — host wallclock stays authoritative.
        self.state = RtcState::Idle;
    }

    fn date_time_bcd(&self) -> Vec<u32> {
        let d = (self.now_fn)();
        let bcd = |n: u32| -> u32 { (((n / 10) << 4) | (n % 10)) & 0xFF };
        let year = d.year % 100;
        let month = d.month;
        let day = d.day;
        let dow = d.dow;
        let hour = d.hour;
        let minute = d.minute;
        let second = d.second;
        vec![
            bcd(year),
            bcd(month),
            bcd(day),
            bcd(dow),
            bcd(hour),
            bcd(minute),
            bcd(second),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drive the bit-banged protocol the way the AGB RTC library does: pull
    // CS high, clock a command byte MSB-first, then either clock out the
    // reply LSB-first or clock in a write byte LSB-first.

    fn enable(rtc: &mut Rtc) {
        rtc.write(0xC8, 1);
        // dir: SCK + SIO + CS are outputs while the host drives.
        rtc.write(0xC6, 0x7);
    }

    // Clock in one host-driven bit (sets SIO low, raises then lowers SCK).
    fn clock_in(rtc: &mut Rtc, cs: u32, sio: u32) {
        // SCK low, set SIO.
        rtc.write(0xC4, (cs << 2) | (sio << 1) | 0);
        // SCK high (rising edge samples / emits).
        rtc.write(0xC4, (cs << 2) | (sio << 1) | 1);
    }

    // Clock out one chip-driven bit; read SIO afterwards. Configure SIO as
    // input so the read reflects the chip's driven bit.
    fn clock_out_bit(rtc: &mut Rtc, cs: u32) -> u32 {
        // SCK low.
        rtc.write(0xC4, cs << 2);
        // SCK high — chip emits its outgoing bit on the rising edge.
        rtc.write(0xC4, (cs << 2) | 1);
        rtc.write(0xC6, 0x5); // SIO -> input (bit1 clear)
        let v = rtc.read(0xC4);
        rtc.write(0xC6, 0x7); // restore SIO -> output
        (v >> 1) & 1
    }

    fn send_command(rtc: &mut Rtc, cmd: u32) {
        // MSB-first.
        for i in (0..8).rev() {
            clock_in(rtc, 1, (cmd >> i) & 1);
        }
    }

    fn read_byte(rtc: &mut Rtc) -> u32 {
        // LSB-first.
        let mut byte = 0u32;
        for i in 0..8 {
            byte |= clock_out_bit(rtc, 1) << i;
        }
        byte
    }

    #[test]
    fn status_read_returns_power_on_default() {
        let mut rtc = Rtc::default();
        enable(&mut rtc);
        // CS rising to start command.
        send_command(&mut rtc, 0x63); // reg 1 (status), read
        let b = read_byte(&mut rtc);
        assert_eq!(b, 0x80);
    }

    #[test]
    fn status_write_then_read_matches() {
        let mut rtc = Rtc::default();
        enable(&mut rtc);
        // Write status 0x42.
        send_command(&mut rtc, 0x62); // reg 1, write
        for i in 0..8 {
            clock_in(&mut rtc, 1, (0x42 >> i) & 1);
        }
        // Deselect.
        rtc.write(0xC4, 0);
        // Read it back.
        send_command(&mut rtc, 0x63); // reg 1, read
        assert_eq!(read_byte(&mut rtc), 0x42);
    }

    #[test]
    fn datetime_read_yields_default_epoch_bcd() {
        let mut rtc = Rtc::default();
        enable(&mut rtc);
        send_command(&mut rtc, 0x65); // reg 2 (date/time), read
        // Default epoch: 2000-01-01 Sat 00:00:00.
        // year=00, month=01, day=01, dow=06, h=00, m=00, s=00 (BCD).
        let expected = [0x00, 0x01, 0x01, 0x06, 0x00, 0x00, 0x00];
        for &e in expected.iter() {
            assert_eq!(read_byte(&mut rtc), e);
        }
    }

    #[test]
    fn injectable_clock_is_honored() {
        let mut rtc = Rtc::default();
        rtc.set_now_fn(|| DateTime {
            year: 2023,
            month: 12,
            day: 25,
            dow: 1,
            hour: 13,
            minute: 45,
            second: 9,
        });
        enable(&mut rtc);
        send_command(&mut rtc, 0x65); // reg 2, read
        // BCD: year 23 -> 0x23, month 12 -> 0x12, day 25 -> 0x25,
        // dow 1 -> 0x01, hour 13 -> 0x13, min 45 -> 0x45, sec 9 -> 0x09.
        let expected = [0x23, 0x12, 0x25, 0x01, 0x13, 0x45, 0x09];
        for &e in expected.iter() {
            assert_eq!(read_byte(&mut rtc), e);
        }
    }

    #[test]
    fn read_when_disabled_returns_zero() {
        let rtc = Rtc::default();
        assert_eq!(rtc.read(0xC4), 0);
    }
}
