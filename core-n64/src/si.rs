//! SI + PIF — the Serial Interface and the PIF (peripheral interface chip)
//! that bridges to the controllers via the joybus protocol.
//!
//! The CPU writes a joybus command block into PIF RAM (64 bytes), then triggers
//! an SI DMA that "executes" the joybus channels and writes the results back.
//! We implement the controller-status (0xFF) and controller-read (0x01)
//! commands for the single plugged-in controller on channel 0, returning the
//! N64 controller's button/stick state. Other channels report "no device".
//!
//! Built from n64brew "Joybus Protocol" / "PIF-NUS" / "Serial Interface".

/// SI register byte offsets.
pub const SI_DRAM_ADDR: u32 = 0x00;
pub const SI_PIF_AD_RD64B: u32 = 0x04; // PIF RAM -> RDRAM (read controllers)
pub const SI_PIF_AD_WR64B: u32 = 0x10; // RDRAM -> PIF RAM (issue commands)
pub const SI_STATUS: u32 = 0x18;

/// N64 controller button bitmask (the joybus controller-read 4-byte response,
/// first two bytes are buttons). We expose a flat `u32` to the host via
/// `set_keys` and pack it into the response here.
#[derive(Debug, Clone, Copy, Default)]
pub struct Controller {
    /// Packed buttons + stick. See [`Button`] for the bit layout.
    pub state: u32,
    /// Signed analog stick X/Y (-128..127). Defaults to centered (0,0).
    pub stick_x: i8,
    pub stick_y: i8,
    /// True if a controller is plugged into this channel.
    pub plugged: bool,
}

/// Host-facing button bits (match the order documented in `wasm.rs`).
pub mod button {
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const Z: u32 = 1 << 2;
    pub const START: u32 = 1 << 3;
    pub const DUP: u32 = 1 << 4;
    pub const DDOWN: u32 = 1 << 5;
    pub const DLEFT: u32 = 1 << 6;
    pub const DRIGHT: u32 = 1 << 7;
    pub const L: u32 = 1 << 8;
    pub const R: u32 = 1 << 9;
    pub const CUP: u32 = 1 << 10;
    pub const CDOWN: u32 = 1 << 11;
    pub const CLEFT: u32 = 1 << 12;
    pub const CRIGHT: u32 = 1 << 13;
}

pub struct Si {
    /// 64-byte PIF RAM (the joybus command/result area).
    pub pif_ram: Box<[u8; 64]>,
    pub dram_addr: u32,
    pub status: u32,
    /// Four controller ports; only port 0 is plugged by default.
    pub controllers: [Controller; 4],
}

impl Default for Si {
    fn default() -> Self {
        Self::new()
    }
}

impl Si {
    pub fn new() -> Self {
        let mut controllers = [Controller::default(); 4];
        controllers[0].plugged = true; // one controller in port 1
        Si {
            pif_ram: Box::new([0; 64]),
            dram_addr: 0,
            status: 0,
            controllers,
        }
    }

    /// Set port-0 buttons from the host bitmask (see [`button`]).
    pub fn set_keys(&mut self, bits: u32) {
        self.controllers[0].state = bits;
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            SI_DRAM_ADDR => self.dram_addr,
            SI_STATUS => self.status,
            _ => 0,
        }
    }

    /// Write an SI register. The two DMA triggers return a [`SiDma`] the bus
    /// runs (it owns RDRAM); SI_STATUS clears the SI interrupt.
    pub fn write(&mut self, offset: u32, v: u32) -> Option<SiDma> {
        match offset {
            SI_DRAM_ADDR => self.dram_addr = v & 0x00FF_FFFF,
            SI_PIF_AD_WR64B => return Some(SiDma::ToPif),
            SI_PIF_AD_RD64B => return Some(SiDma::FromPif),
            SI_STATUS => self.status = 0,
            _ => {}
        }
        None
    }

    /// Process the joybus command block currently in PIF RAM, writing the
    /// controller responses back into PIF RAM. Called after a RDRAM->PIF DMA
    /// (the CPU has staged the commands) and before the PIF->RDRAM read-back.
    ///
    /// The joybus block is a sequence of per-channel descriptors:
    /// `[tx_len][rx_len][cmd...][rx bytes]`. tx_len 0 = skip channel, 0xFE =
    /// end of list, 0xFF = channel skip (one byte). We handle the common
    /// controller commands on each channel.
    pub fn run_joybus(&mut self) {
        let mut i = 0usize;
        let mut channel = 0usize;
        let ram = &mut *self.pif_ram;
        while i < 63 && channel < 4 {
            let tx = ram[i];
            if tx == 0xFE {
                break; // end of command list
            }
            if tx == 0x00 {
                i += 1;
                continue; // skip with no channel advance per some encodings
            }
            if tx == 0xFF {
                // Channel skip / dummy.
                i += 1;
                channel += 1;
                continue;
            }
            let tx_len = (tx & 0x3F) as usize;
            if i + 1 >= 64 {
                break;
            }
            let rx_byte = ram[i + 1];
            let rx_len = (rx_byte & 0x3F) as usize;
            let cmd_off = i + 2;
            if cmd_off >= 64 {
                break;
            }
            let cmd = ram[cmd_off];
            let rx_off = cmd_off + tx_len;

            let ctrl = self.controllers[channel];
            match cmd {
                0x00 | 0xFF => {
                    // Info / reset: identify as a standard N64 controller
                    // (0x05 0x00 0x01) when plugged, else set the no-device bit.
                    if ctrl.plugged && rx_off + 2 < 64 {
                        ram[rx_off] = 0x05;
                        ram[rx_off + 1] = 0x00;
                        ram[rx_off + 2] = 0x01; // controller pak present
                    } else if rx_off < 64 {
                        ram[i + 1] |= 0x80; // set rx error/no-device bit
                    }
                }
                0x01 => {
                    // Controller state: 4 bytes (buttons hi/lo, stick X, stick Y).
                    if ctrl.plugged && rx_off + 3 < 64 {
                        let (b0, b1) = pack_buttons(ctrl.state);
                        ram[rx_off] = b0;
                        ram[rx_off + 1] = b1;
                        ram[rx_off + 2] = ctrl.stick_x as u8;
                        ram[rx_off + 3] = ctrl.stick_y as u8;
                    } else {
                        ram[i + 1] |= 0x80;
                    }
                }
                _ => {
                    // Unknown command (controller pak read/write etc.): no-op.
                }
            }
            i = rx_off + rx_len.max(if cmd == 0x01 { 4 } else { 3 });
            channel += 1;
        }
    }
}

/// Pack the host button bitmask into the two joybus button bytes.
///
/// Byte 0: A B Z Start Dup Ddown Dleft Dright
/// Byte 1: 0 0 L R Cup Cdown Cleft Cright
fn pack_buttons(s: u32) -> (u8, u8) {
    use button::*;
    let mut b0 = 0u8;
    let mut b1 = 0u8;
    if s & A != 0 {
        b0 |= 1 << 7;
    }
    if s & B != 0 {
        b0 |= 1 << 6;
    }
    if s & Z != 0 {
        b0 |= 1 << 5;
    }
    if s & START != 0 {
        b0 |= 1 << 4;
    }
    if s & DUP != 0 {
        b0 |= 1 << 3;
    }
    if s & DDOWN != 0 {
        b0 |= 1 << 2;
    }
    if s & DLEFT != 0 {
        b0 |= 1 << 1;
    }
    if s & DRIGHT != 0 {
        b0 |= 1 << 0;
    }
    if s & L != 0 {
        b1 |= 1 << 5;
    }
    if s & R != 0 {
        b1 |= 1 << 4;
    }
    if s & CUP != 0 {
        b1 |= 1 << 3;
    }
    if s & CDOWN != 0 {
        b1 |= 1 << 2;
    }
    if s & CLEFT != 0 {
        b1 |= 1 << 1;
    }
    if s & CRIGHT != 0 {
        b1 |= 1 << 0;
    }
    (b0, b1)
}

/// Direction of an SI DMA (between RDRAM and PIF RAM).
pub enum SiDma {
    /// RDRAM -> PIF RAM (issue the joybus command block).
    ToPif,
    /// PIF RAM -> RDRAM (read the joybus responses back).
    FromPif,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_buttons_a_and_start() {
        let (b0, b1) = pack_buttons(button::A | button::START);
        assert_eq!(b0, 0b1001_0000);
        assert_eq!(b1, 0);
    }

    #[test]
    fn controller_status_reports_standard_pad() {
        let mut si = Si::new();
        // Command block: tx=1, rx=3, cmd=0x00 (info) on channel 0.
        si.pif_ram[0] = 0x01;
        si.pif_ram[1] = 0x03;
        si.pif_ram[2] = 0x00;
        si.pif_ram[3] = 0xFE; // would-be end after responses
        si.run_joybus();
        // Response at offset cmd_off + tx_len = 2 + 1 = 3.
        assert_eq!(si.pif_ram[3], 0x05);
        assert_eq!(si.pif_ram[5], 0x01);
    }

    #[test]
    fn controller_read_returns_buttons_and_centered_stick() {
        let mut si = Si::new();
        si.set_keys(button::A | button::CRIGHT);
        // tx=1, rx=4, cmd=0x01 (read state) on channel 0.
        si.pif_ram[0] = 0x01;
        si.pif_ram[1] = 0x04;
        si.pif_ram[2] = 0x01;
        si.run_joybus();
        let rx = 3; // cmd_off(2) + tx_len(1)
        assert_eq!(si.pif_ram[rx] & (1 << 7), 1 << 7); // A pressed
        assert_eq!(si.pif_ram[rx + 1] & 1, 1); // C-right
        assert_eq!(si.pif_ram[rx + 2], 0); // stick X centered
        assert_eq!(si.pif_ram[rx + 3], 0); // stick Y centered
    }

    #[test]
    fn unplugged_channel_sets_error_bit() {
        let mut si = Si::new();
        si.controllers[0].plugged = false;
        si.pif_ram[0] = 0x01;
        si.pif_ram[1] = 0x04;
        si.pif_ram[2] = 0x01;
        si.run_joybus();
        assert_eq!(si.pif_ram[1] & 0x80, 0x80);
    }
}
