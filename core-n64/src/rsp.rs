//! RSP — Reality Signal Processor. A MIPS-derived scalar+vector coprocessor
//! with 4 KB of data memory (DMEM) and 4 KB of instruction memory (IMEM),
//! driven by the SP register block.
//!
//! FOUNDATION SCOPE (see `lib.rs` matrix): this owns DMEM/IMEM, the SP control
//! registers, the halt/run state, and the DMA engine that copies between RDRAM
//! and DMEM/IMEM (the SP_DMA_* registers). The actual RSP instruction set —
//! both the scalar subset and the vector (VU) unit — is NOT executed; the RSP
//! stays halted, so microcode (graphics/audio tasks) does not run. Games that
//! depend on the RSP for display lists won't render geometry, but the DMA and
//! register plumbing are correct so boot code that pokes the RSP doesn't fault.

use crate::regions;

/// SP register byte offsets (within the SP_REGS block at 0x0404_0000).
pub const SP_MEM_ADDR: u32 = 0x00; // DMEM/IMEM address (+ bank bit 12)
pub const SP_DRAM_ADDR: u32 = 0x04; // RDRAM address
pub const SP_RD_LEN: u32 = 0x08; // RDRAM->SP DMA length
pub const SP_WR_LEN: u32 = 0x0C; // SP->RDRAM DMA length
pub const SP_STATUS: u32 = 0x10; // status (halt/broke/intr/...)
pub const SP_DMA_FULL: u32 = 0x14;
pub const SP_DMA_BUSY: u32 = 0x18;
pub const SP_SEMAPHORE: u32 = 0x1C;
/// PC register lives in a second SP register window at +0x40000.
pub const SP_PC: u32 = 0x40000;

/// SP_STATUS bits.
pub const STATUS_HALT: u32 = 1 << 0;
pub const STATUS_BROKE: u32 = 1 << 1;

pub struct Rsp {
    /// 4 KB data memory.
    pub dmem: Box<[u8; 0x1000]>,
    /// 4 KB instruction memory.
    pub imem: Box<[u8; 0x1000]>,
    /// SP_MEM_ADDR latch.
    pub mem_addr: u32,
    /// SP_DRAM_ADDR latch.
    pub dram_addr: u32,
    /// SP_STATUS register. Halted at reset.
    pub status: u32,
    /// RSP program counter (12-bit, into IMEM).
    pub pc: u32,
    /// Semaphore register (set on read, cleared on write — a simple mutex).
    pub semaphore: bool,
}

impl Default for Rsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Rsp {
    pub fn new() -> Self {
        Rsp {
            dmem: Box::new([0; 0x1000]),
            imem: Box::new([0; 0x1000]),
            mem_addr: 0,
            dram_addr: 0,
            status: STATUS_HALT, // RSP boots halted; the CPU clears HALT to run microcode
            pc: 0,
            semaphore: false,
        }
    }

    /// Read SP DMEM/IMEM (big-endian word) by physical address.
    pub fn mem_read32(&self, paddr: u32) -> u32 {
        if (regions::SP_DMEM_BASE..regions::SP_DMEM_END).contains(&paddr) {
            be32(&self.dmem[..], (paddr - regions::SP_DMEM_BASE) as usize)
        } else if (regions::SP_IMEM_BASE..regions::SP_IMEM_END).contains(&paddr) {
            be32(&self.imem[..], (paddr - regions::SP_IMEM_BASE) as usize)
        } else {
            0
        }
    }

    /// Write SP DMEM/IMEM (big-endian word) by physical address.
    pub fn mem_write32(&mut self, paddr: u32, v: u32) {
        if (regions::SP_DMEM_BASE..regions::SP_DMEM_END).contains(&paddr) {
            set_be32(&mut self.dmem[..], (paddr - regions::SP_DMEM_BASE) as usize, v);
        } else if (regions::SP_IMEM_BASE..regions::SP_IMEM_END).contains(&paddr) {
            set_be32(&mut self.imem[..], (paddr - regions::SP_IMEM_BASE) as usize, v);
        }
    }

    /// Read an SP control register.
    pub fn reg_read(&mut self, offset: u32) -> u32 {
        match offset {
            SP_MEM_ADDR => self.mem_addr,
            SP_DRAM_ADDR => self.dram_addr,
            SP_STATUS => self.status,
            SP_DMA_FULL => 0,
            SP_DMA_BUSY => 0,
            SP_SEMAPHORE => {
                // Reading sets the semaphore (returns its prior value).
                let prev = self.semaphore;
                self.semaphore = true;
                prev as u32
            }
            SP_PC => self.pc,
            _ => 0,
        }
    }

    /// Write an SP control register. The DMA-length writes trigger a transfer,
    /// done in the bus impl (which owns RDRAM); here we just latch values and
    /// apply the SP_STATUS set/clear semantics. Returns a [`DmaRequest`] when a
    /// DMA-length write was issued, so the caller can run the copy.
    pub fn reg_write(&mut self, offset: u32, v: u32) -> Option<DmaRequest> {
        match offset {
            SP_MEM_ADDR => self.mem_addr = v & 0x1FFF,
            SP_DRAM_ADDR => self.dram_addr = v & 0x00FF_FFFF,
            SP_RD_LEN => {
                return Some(DmaRequest {
                    to_rdram: false,
                    length: (v & 0xFFF) + 1,
                })
            }
            SP_WR_LEN => {
                return Some(DmaRequest {
                    to_rdram: true,
                    length: (v & 0xFFF) + 1,
                })
            }
            SP_STATUS => self.apply_status_write(v),
            SP_SEMAPHORE => self.semaphore = false,
            SP_PC => self.pc = v & 0xFFF,
            _ => {}
        }
        None
    }

    /// SP_STATUS write uses set/clear bit pairs for HALT (bits 0/1) etc. We
    /// model the HALT clear/set and BROKE clear used by boot code.
    fn apply_status_write(&mut self, v: u32) {
        if v & (1 << 0) != 0 {
            self.status &= !STATUS_HALT; // clear halt -> "start" (no exec here)
        }
        if v & (1 << 1) != 0 {
            self.status |= STATUS_HALT; // set halt
        }
        if v & (1 << 2) != 0 {
            self.status &= !STATUS_BROKE; // clear broke
        }
    }
}

/// A pending SP DMA: copy `length` bytes between RDRAM (`dram_addr`) and
/// DMEM/IMEM (`mem_addr`). `to_rdram` chooses the direction.
pub struct DmaRequest {
    pub to_rdram: bool,
    pub length: u32,
}

#[inline]
fn be32(buf: &[u8], off: usize) -> u32 {
    let o = off & !3 & (buf.len() - 1);
    u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
}
#[inline]
fn set_be32(buf: &mut [u8], off: usize, v: u32) {
    let o = off & !3 & (buf.len() - 1);
    buf[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boots_halted() {
        let rsp = Rsp::new();
        assert_eq!(rsp.status & STATUS_HALT, STATUS_HALT);
    }

    #[test]
    fn dmem_read_write_big_endian() {
        let mut rsp = Rsp::new();
        rsp.mem_write32(regions::SP_DMEM_BASE + 8, 0xDEAD_BEEF);
        assert_eq!(rsp.mem_read32(regions::SP_DMEM_BASE + 8), 0xDEAD_BEEF);
        assert_eq!(rsp.dmem[8], 0xDE);
        assert_eq!(rsp.dmem[11], 0xEF);
    }

    #[test]
    fn rd_len_write_emits_dma_request() {
        let mut rsp = Rsp::new();
        let req = rsp.reg_write(SP_RD_LEN, 0x0F).unwrap();
        assert!(!req.to_rdram);
        assert_eq!(req.length, 0x10);
    }

    #[test]
    fn status_clear_halt() {
        let mut rsp = Rsp::new();
        rsp.reg_write(SP_STATUS, 1 << 0); // clear halt
        assert_eq!(rsp.status & STATUS_HALT, 0);
    }

    #[test]
    fn semaphore_acquire_release() {
        let mut rsp = Rsp::new();
        assert_eq!(rsp.reg_read(SP_SEMAPHORE), 0); // first read acquires
        assert_eq!(rsp.reg_read(SP_SEMAPHORE), 1); // now held
        rsp.reg_write(SP_SEMAPHORE, 0); // release
        assert_eq!(rsp.reg_read(SP_SEMAPHORE), 0);
    }
}
