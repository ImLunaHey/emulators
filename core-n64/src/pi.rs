//! PI — the Peripheral Interface. Drives the cartridge bus: the CPU programs a
//! source (cart ROM) address, a destination (RDRAM) address, and a length, and
//! the PI DMAs the data across, then raises MI INTR_PI.
//!
//! This is how games load code/data from the cart into RDRAM (the IPL3 boot
//! itself does a PI DMA of the first MB). We implement the register block and
//! the DMA; timing is instantaneous (the interrupt fires immediately).
//!
//! Built from n64brew "Peripheral Interface".

/// PI register byte offsets.
pub const PI_DRAM_ADDR: u32 = 0x00;
pub const PI_CART_ADDR: u32 = 0x04;
pub const PI_RD_LEN: u32 = 0x08; // RDRAM -> cart
pub const PI_WR_LEN: u32 = 0x0C; // cart -> RDRAM (the common direction)
pub const PI_STATUS: u32 = 0x10;

pub struct Pi {
    pub dram_addr: u32,
    pub cart_addr: u32,
    pub status: u32,
    /// PI domain timing registers (latch/pulse/page/release) — stored, inert.
    pub dom: [u32; 8],
}

impl Default for Pi {
    fn default() -> Self {
        Self::new()
    }
}

impl Pi {
    pub fn new() -> Self {
        Pi {
            dram_addr: 0,
            cart_addr: 0,
            status: 0,
            dom: [0; 8],
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            PI_DRAM_ADDR => self.dram_addr,
            PI_CART_ADDR => self.cart_addr,
            PI_STATUS => self.status,
            0x14..=0x33 => self.dom[((offset - 0x14) / 4) as usize & 7],
            _ => 0,
        }
    }

    /// Write a PI register. A write to PI_WR_LEN / PI_RD_LEN starts a DMA and
    /// returns the [`DmaRequest`] describing it (the bus owns RDRAM + cart and
    /// performs the copy, then raises INTR_PI).
    pub fn write(&mut self, offset: u32, v: u32) -> Option<DmaRequest> {
        match offset {
            PI_DRAM_ADDR => self.dram_addr = v & 0x00FF_FFFF,
            PI_CART_ADDR => self.cart_addr = v,
            PI_WR_LEN => {
                return Some(DmaRequest {
                    to_rdram: true,
                    length: (v & 0x00FF_FFFF) + 1,
                })
            }
            PI_RD_LEN => {
                return Some(DmaRequest {
                    to_rdram: false,
                    length: (v & 0x00FF_FFFF) + 1,
                })
            }
            PI_STATUS => {
                // bit0 = reset DMA controller, bit1 = clear INTR (ack).
                self.status = 0;
            }
            0x14..=0x33 => self.dom[((offset - 0x14) / 4) as usize & 7] = v,
            _ => {}
        }
        None
    }
}

/// A pending PI DMA: copy `length` bytes between the cart (`cart_addr`) and
/// RDRAM (`dram_addr`). `to_rdram` is the cart->RDRAM direction (the usual one).
pub struct DmaRequest {
    pub to_rdram: bool,
    pub length: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wr_len_starts_cart_to_rdram_dma() {
        let mut pi = Pi::new();
        pi.write(PI_DRAM_ADDR, 0x1000);
        pi.write(PI_CART_ADDR, 0x1000_0000);
        let req = pi.write(PI_WR_LEN, 0xFF).unwrap();
        assert!(req.to_rdram);
        assert_eq!(req.length, 0x100);
        assert_eq!(pi.dram_addr, 0x1000);
    }
}
