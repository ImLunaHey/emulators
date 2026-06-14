//! SIO — serial I/O (controllers / memory cards via `SIO0`, link cable `SIO1`).
//!
//! Built from psx-spx "Controllers and Memory Cards" / "Serial Port (SIO)".
//! `SIO0` (controllers + memory cards) lives at 0x1F80_1040..0x1F80_104F and
//! `SIO1` (serial link) at 0x1F80_1050..0x1F80_105F; this module covers both,
//! routed from the SIO window base 0x1F80_1040 (`off` is relative to it). The
//! register map per port:
//!
//! | off | name        | access |
//! |-----|-------------|--------|
//! | +0  | RX/TX data  | r/w    |
//! | +4  | STAT        | r      |
//! | +8  | MODE        | r/w    |
//! | +A  | CTRL        | r/w    |
//! | +E  | BAUD        | r/w    |
//!
//! The shift-register transfer handshake, the pad/memory-card protocol and the
//! SIO IRQ are stubbed; `STAT` reports "TX ready, RX empty" so the BIOS's pad
//! probe times out cleanly instead of hanging.

/// One serial port's register block (`SIO0` or `SIO1`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SioPort {
    pub mode: u16,
    pub ctrl: u16,
    pub baud: u16,
}

/// Minimal digital-pad transfer FSM (SIO0). The BIOS/game polls a controller by
/// asserting `/CS` (CTRL bit 13 selects the port + the JOY transfer) and then
/// clocking a fixed byte sequence through the RX/TX data port (psx-spx
/// "Controllers - Standard Digital/Analog Controllers"):
///
/// ```text
/// host: 01  42  TAP  MOT  MOT  ...
/// pad:  HiZ ID  5A   BTNL BTNH ...     (ID = 0x41 for a digital pad)
/// ```
///
/// We implement exactly the digital-pad path: address `0x01`, command `0x42`
/// (read buttons), reply `0x41 0x5A <btn_lo> <btn_hi>`, where the button bits
/// are active-low. Memory-card access (`0x81`) returns high-Z (0xFF) so the
/// kernel's card probe fails cleanly.
#[derive(Debug, Clone, Copy, Default)]
struct PadFsm {
    /// Index into the 4-byte reply sequence (0 = idle / awaiting `0x01`).
    step: u8,
    /// Latched button state for the in-flight transfer (active-low, 16-bit).
    buttons: u16,
    /// The byte the pad will return on the next data-port read.
    rx: u8,
    /// True while a reply byte is buffered (drives STAT.RXFIFO-not-empty).
    rx_ready: bool,
}

/// Both serial ports.
#[derive(Debug, Clone, Default)]
pub struct Sio {
    /// `SIO0` — controllers / memory cards (0x1F80_1040).
    pub sio0: SioPort,
    /// `SIO1` — serial link cable (0x1F80_1050).
    pub sio1: SioPort,
    /// Live digital-pad button bitmask, active-**high** (1 = pressed); the FSM
    /// inverts it into the active-low wire format. Bit layout = [`Button`].
    pub keys: u16,
    pad: PadFsm,
}

/// Digital-pad button bit positions in the 16-bit pad word (psx-spx). The wire
/// format is active-low; [`Sio::set_keys`] takes an active-high mask and the
/// FSM inverts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Button {
    Select = 0,
    L3 = 1,
    R3 = 2,
    Start = 3,
    Up = 4,
    Right = 5,
    Down = 6,
    Left = 7,
    L2 = 8,
    R2 = 9,
    L1 = 10,
    R1 = 11,
    Triangle = 12,
    Circle = 13,
    Cross = 14,
    Square = 15,
}

impl Button {
    /// This button's bit in the 16-bit pad word.
    #[inline]
    pub fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

impl Sio {
    pub fn new() -> Self {
        Sio::default()
    }

    /// Set the live digital-pad button state (active-high: bit set = pressed),
    /// bit layout per [`Button`]. The host calls this each frame.
    pub fn set_keys(&mut self, keys: u16) {
        self.keys = keys;
    }

    /// Read an SIO register. `off` is relative to the SIO window base
    /// 0x1F80_1040; `off & 0x10` selects `SIO0` (0) vs `SIO1` (1).
    pub fn read(&self, off: u32) -> u32 {
        if off & 0x10 != 0 {
            // SIO1 (link cable) — not modelled; report idle TX-ready.
            return match off & 0xF {
                0x0 => 0xFF,
                0x4 => 0x0000_0005,
                0x8 => self.sio1.mode as u32,
                0xA => self.sio1.ctrl as u32,
                0xE => self.sio1.baud as u32,
                _ => 0,
            };
        }
        match off & 0xF {
            // RX data — the byte the pad clocked back (or 0xFF when none).
            0x0 => {
                if self.pad.rx_ready {
                    self.pad.rx as u32
                } else {
                    0xFF
                }
            }
            // STAT: bit0 TX-ready (always), bit1 RX-FIFO-not-empty (a reply byte
            // is waiting), bit2 TX-empty.
            0x4 => {
                let mut s = 0x0000_0005u32;
                if self.pad.rx_ready {
                    s |= 1 << 1;
                }
                s
            }
            0x8 => self.sio0.mode as u32,
            0xA => self.sio0.ctrl as u32,
            0xE => self.sio0.baud as u32,
            _ => 0,
        }
    }

    /// Write an SIO register. `off` is relative to 0x1F80_1040; `off & 0x10`
    /// selects the port. SIO0 data writes drive the pad transfer FSM.
    pub fn write(&mut self, off: u32, v: u32) {
        if off & 0x10 != 0 {
            match off & 0xF {
                0x8 => self.sio1.mode = v as u16,
                0xA => self.sio1.ctrl = v as u16,
                0xE => self.sio1.baud = v as u16,
                _ => {}
            }
            return;
        }
        match off & 0xF {
            0x0 => self.pad_clock(v as u8),
            0x8 => self.sio0.mode = v as u16,
            0xA => {
                self.sio0.ctrl = v as u16;
                // Deasserting the transfer (clearing CTRL bit 1 = TX enable, or a
                // reset via bit 6) ends the current poll and re-idles the pad.
                if v & 0x40 != 0 || v & 0x2 == 0 {
                    self.pad.step = 0;
                    self.pad.rx_ready = false;
                }
            }
            0xE => self.sio0.baud = v as u16,
            _ => {}
        }
    }

    /// Clock one TX byte through the digital-pad FSM, latching the reply byte
    /// the pad shifts back simultaneously.
    fn pad_clock(&mut self, tx: u8) {
        // Active-low wire format: a pressed button reads as 0.
        let wire = !self.keys;
        match self.pad.step {
            0 => {
                if tx == 0x01 {
                    // Controller addressed: reply high-Z this byte, advance.
                    self.pad.buttons = wire;
                    self.pad.rx = 0xFF;
                    self.pad.rx_ready = true;
                    self.pad.step = 1;
                } else {
                    // Not us (e.g. 0x81 memory card): stay idle, float the line.
                    self.pad.rx = 0xFF;
                    self.pad.rx_ready = true;
                }
            }
            1 => {
                // Command byte; 0x42 = read buttons. Reply the digital-pad ID.
                self.pad.rx = 0x41;
                self.pad.rx_ready = true;
                self.pad.step = 2;
            }
            2 => {
                // Reply the 0x5A "controller ready" byte.
                self.pad.rx = 0x5A;
                self.pad.rx_ready = true;
                self.pad.step = 3;
            }
            3 => {
                // First button byte (low 8 bits, active-low).
                self.pad.rx = (self.pad.buttons & 0xFF) as u8;
                self.pad.rx_ready = true;
                self.pad.step = 4;
            }
            4 => {
                // Second button byte (high 8 bits), then the transfer ends.
                self.pad.rx = (self.pad.buttons >> 8) as u8;
                self.pad.rx_ready = true;
                self.pad.step = 0;
            }
            _ => {
                self.pad.rx = 0xFF;
                self.pad.rx_ready = true;
                self.pad.step = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_reports_tx_ready() {
        let sio = Sio::new();
        assert_ne!(sio.read(0x4) & 1, 0, "TX ready bit set");
    }

    #[test]
    fn ctrl_round_trips_per_port() {
        let mut sio = Sio::new();
        sio.write(0x0A, 0x1001); // SIO0 CTRL
        sio.write(0x1A, 0x2002); // SIO1 CTRL
        assert_eq!(sio.read(0x0A), 0x1001);
        assert_eq!(sio.read(0x1A), 0x2002);
    }
}
