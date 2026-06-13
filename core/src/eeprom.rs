// EEPROM bit-serial state machine.
//
// EEPROM is wired into the cart bus at 0x0D000000-0x0DFFFFFF. The host
// accesses it via DMA3 to/from 0x0DFFFF00; each 16-bit DMA transfer
// carries exactly ONE bit, in bit 0 of the value.
//
// Commands (MSB first):
//   READ   1 1 [N addr bits] [0 terminator]            (host then reads 4 zero bits + 64 data bits)
//   WRITE  1 0 [N addr bits] [64 data bits] [0 term]
//
// N = 6 for the 512-byte chip, 14 for the 8K chip (only the low 10 of
// those 14 are meaningful — high 4 ignored). EEPROM_V signature alone
// doesn't say which size; we default to 8K which is what every modern
// title uses, including Minish Cap.

pub struct Eeprom {
    pub data: Vec<u8>,
    // The TS exposed `onChange` (dirty callback); modelled as a flag.
    pub dirty: bool,

    addr_bits: u32,
    // Total bits received so far in the current command. Reset to 0 when
    // we either finish a command or abort one.
    cmd_len: u32,
    // For the first two bits we just remember them in `cmdBits`; the
    // address is collected separately into `addr`.
    cmd_bits: u32,
    is_read: bool,
    addr: u32,
    write_buf: [u8; 8],

    // Read-response state. After a successful READ command we feed bits
    // back: 4 zero bits followed by 64 data bits (MSB first).
    in_response: bool,
    resp_idx: u32,
}

impl Eeprom {
    pub fn new(size: usize) -> Self {
        let mut data = vec![0xFFu8; size];
        data.fill(0xFF);
        Eeprom {
            data,
            dirty: false,
            addr_bits: if size == 512 { 6 } else { 14 },
            cmd_len: 0,
            cmd_bits: 0,
            is_read: false,
            addr: 0,
            write_buf: [0u8; 8],
            in_response: false,
            resp_idx: 0,
        }
    }
}

impl Default for Eeprom {
    fn default() -> Self {
        // size: 512 | 8192 = 8192
        Eeprom::new(8192)
    }
}

impl crate::Save for Eeprom {
    fn load_save(&mut self, bytes: &[u8]) {
        self.data.fill(0xFF);
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
    }

    // Returns the next response bit in bit 0. When not in response phase
    // we return 1 (open-bus pull-up on real hardware).
    fn read(&mut self, _addr: u32) -> u32 {
        if !self.in_response {
            return 1;
        }
        let mut bit: u32 = 0;
        if self.resp_idx >= 4 {
            let data_bit = self.resp_idx - 4; // 0..63
            let block = self.addr & ((self.data.len() as u32 / 8) - 1);
            let byte_off = (block * 8 + (data_bit >> 3)) as usize;
            let bit_off = 7 - (data_bit & 7);
            bit = ((self.data[byte_off] as u32) >> bit_off) & 1;
        }
        self.resp_idx += 1;
        if self.resp_idx >= 68 {
            self.in_response = false;
            self.resp_idx = 0;
        }
        bit
    }

    // Consume the next command bit (bit 0 of v).
    fn write(&mut self, _addr: u32, v: u32) {
        let bit = v & 1;
        self.cmd_len += 1;

        // Bit 1: must be 1 to start a command.
        if self.cmd_len == 1 {
            if bit == 1 {
                self.cmd_bits = 1;
            } else {
                self.cmd_len = 0;
            }
            return;
        }
        // Bit 2: 1 = read, 0 = write.
        if self.cmd_len == 2 {
            self.is_read = bit == 1;
            self.cmd_bits = (self.cmd_bits << 1) | bit;
            self.addr = 0;
            return;
        }
        // Bits 3..2+addrBits: address (MSB first).
        if self.cmd_len <= 2 + self.addr_bits {
            self.addr = (self.addr << 1) | bit;
            return;
        }
        // For READ: one more bit (terminator, ignored) and we transition to
        // response. The host typically sends a 0 here. Either way, we move
        // on after one extra bit.
        if self.is_read {
            self.in_response = true;
            self.resp_idx = 0;
            self.cmd_len = 0;
            self.cmd_bits = 0;
            return;
        }
        // For WRITE: next 64 bits are data, bit-by-bit, then 1 terminator.
        let data_bit = self.cmd_len - (2 + self.addr_bits) - 1; // 0..64
        if data_bit < 64 {
            let byte_off = (data_bit >> 3) as usize;
            let bit_off = 7 - (data_bit & 7);
            if bit != 0 {
                self.write_buf[byte_off] |= 1 << bit_off;
            } else {
                self.write_buf[byte_off] &= !(1 << bit_off);
            }
            return;
        }
        // 65th post-address bit = terminator. Commit the 8-byte block.
        if data_bit == 64 {
            let block = self.addr & ((self.data.len() as u32 / 8) - 1);
            let off = (block * 8) as usize;
            self.data[off..off + 8].copy_from_slice(&self.write_buf);
            self.dirty = true;
            self.cmd_len = 0;
            self.cmd_bits = 0;
            self.write_buf.fill(0);
        }
    }

    fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Save;

    // Helper: feed a single bit (bit 0) as one DMA transfer.
    fn wbit(e: &mut Eeprom, b: u32) {
        e.write(0, b);
    }

    #[test]
    fn write_then_read_8k_roundtrip() {
        let mut e = Eeprom::new(8192); // 14 address bits
        let block: u32 = 5;

        // WRITE command: 1 0 [14 addr bits] [64 data bits] [0 term]
        wbit(&mut e, 1);
        wbit(&mut e, 0);
        for i in (0..14).rev() {
            wbit(&mut e, (block >> i) & 1);
        }
        // 64 data bits: a recognizable pattern (MSB first per byte).
        let payload: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        for byte in payload.iter() {
            for k in (0..8).rev() {
                wbit(&mut e, ((*byte as u32) >> k) & 1);
            }
        }
        // terminator
        wbit(&mut e, 0);
        assert!(e.dirty);

        // READ command: 1 1 [14 addr bits] [0 term]
        wbit(&mut e, 1);
        wbit(&mut e, 1);
        for i in (0..14).rev() {
            wbit(&mut e, (block >> i) & 1);
        }
        wbit(&mut e, 0); // terminator -> enters response

        // 4 zero bits.
        for _ in 0..4 {
            assert_eq!(e.read(0), 0);
        }
        // 64 data bits, reassemble and compare.
        let mut out = [0u8; 8];
        for byte in 0..8 {
            let mut acc = 0u8;
            for _ in 0..8 {
                acc = (acc << 1) | (e.read(0) as u8 & 1);
            }
            out[byte] = acc;
        }
        assert_eq!(out, payload);

        // After 68 bits consumed, response is over; reads return 1.
        assert_eq!(e.read(0), 1);
    }

    #[test]
    fn read_when_idle_returns_one() {
        let mut e = Eeprom::new(512);
        assert_eq!(e.read(0), 1);
    }
}
