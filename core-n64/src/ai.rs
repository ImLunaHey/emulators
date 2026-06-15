//! AI — the Audio Interface. The N64 streams PCM samples from RDRAM to the DAC
//! via a small DMA FIFO at a programmable rate.
//!
//! FOUNDATION SCOPE (see `lib.rs` matrix): this owns the AI register block so
//! boot code that configures audio doesn't fault, but it does NOT produce
//! samples — the RSP audio microcode (which synthesises the audio buffers) is
//! not executed, so there is nothing to stream. [`crate::n64::N64::drain_audio`]
//! returns an empty buffer. Real audio is a future step.
//!
//! Built from n64brew "Audio Interface".

/// AI register byte offsets.
pub const AI_DRAM_ADDR: u32 = 0x00;
pub const AI_LEN: u32 = 0x04;
pub const AI_CONTROL: u32 = 0x08;
pub const AI_STATUS: u32 = 0x0C;
pub const AI_DACRATE: u32 = 0x10;
pub const AI_BITRATE: u32 = 0x14;

pub struct Ai {
    pub dram_addr: u32,
    pub len: u32,
    pub control: u32,
    pub dacrate: u32,
    pub bitrate: u32,
}

impl Default for Ai {
    fn default() -> Self {
        Self::new()
    }
}

impl Ai {
    pub fn new() -> Self {
        Ai {
            dram_addr: 0,
            len: 0,
            control: 0,
            dacrate: 0,
            bitrate: 0,
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            AI_DRAM_ADDR => self.dram_addr,
            AI_LEN => self.len,
            // AI_STATUS: report "not full / not busy" so games don't spin.
            AI_STATUS => 0,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, v: u32) {
        match offset {
            AI_DRAM_ADDR => self.dram_addr = v & 0x00FF_FFFF,
            AI_LEN => self.len = v & 0x3FFFF,
            AI_CONTROL => self.control = v & 1,
            AI_STATUS => {} // write clears the AI interrupt (ack)
            AI_DACRATE => self.dacrate = v & 0x3FFF,
            AI_BITRATE => self.bitrate = v & 0xF,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_latch() {
        let mut ai = Ai::new();
        ai.write(AI_DACRATE, 0x2E6B);
        assert_eq!(ai.dacrate, 0x2E6B);
        assert_eq!(ai.read(AI_STATUS), 0);
    }
}
