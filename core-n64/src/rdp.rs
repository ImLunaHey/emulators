//! RDP — Reality Display Processor. The N64's rasteriser: it consumes a command
//! list (the "display list" the RSP/CPU builds) and draws triangles/rectangles
//! into the RDRAM framebuffer, with the full combiner/blender/Z/texture
//! pipeline.
//!
//! FOUNDATION SCOPE (see `lib.rs` matrix): this owns the DP command register
//! block (DPC_*) and accepts command-buffer pointers, but it does NOT
//! rasterise — no triangles or fill rectangles are drawn. The VI still scans
//! out whatever is in RDRAM, so a game that fills its framebuffer with the CPU
//! is visible; one that relies on the RDP to draw is not. Implementing the RDP
//! command set + rasteriser is the next major step toward rendering commercial
//! games.
//!
//! Built from n64brew "Display Processor".

/// DPC register byte offsets (the command-buffer control block).
pub const DPC_START: u32 = 0x00;
pub const DPC_END: u32 = 0x04;
pub const DPC_CURRENT: u32 = 0x08;
pub const DPC_STATUS: u32 = 0x0C;

pub struct Rdp {
    pub start: u32,
    pub end: u32,
    pub current: u32,
    pub status: u32,
}

impl Default for Rdp {
    fn default() -> Self {
        Self::new()
    }
}

impl Rdp {
    pub fn new() -> Self {
        Rdp {
            start: 0,
            end: 0,
            current: 0,
            status: 0,
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            DPC_START => self.start,
            DPC_END => self.end,
            DPC_CURRENT => self.current,
            DPC_STATUS => self.status,
            _ => 0,
        }
    }

    /// Write a DPC register. Writing DPC_END would kick off command processing
    /// on real hardware; here we just advance `current` to `end` and mark the
    /// processor idle (commands are accepted but not executed).
    pub fn write(&mut self, offset: u32, v: u32) {
        match offset {
            DPC_START => {
                self.start = v & 0x00FF_FFFF;
                self.current = self.start;
            }
            DPC_END => {
                self.end = v & 0x00FF_FFFF;
                // Stub: pretend we consumed the whole command list immediately.
                self.current = self.end;
            }
            DPC_STATUS => { /* set/clear bits — ignored in the stub */ }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_write_consumes_command_list() {
        let mut rdp = Rdp::new();
        rdp.write(DPC_START, 0x1000);
        rdp.write(DPC_END, 0x2000);
        assert_eq!(rdp.current, 0x2000);
    }
}
