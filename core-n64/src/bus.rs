//! The CPU's view of memory: the [`Bus`] trait. The interpreter codes against
//! `&mut dyn Bus`, so it never needs to know which device backs an address.
//! The production implementor is the [`crate::n64::N64`] god-struct, which owns
//! RDRAM + every RCP register block + the cartridge and routes the physical
//! address.
//!
//! The N64 is BIG-ENDIAN: `read16`/`read32`/`read64` assemble bytes MSB-first.
//! Addresses passed in are 32-bit *physical* addresses (the interpreter has
//! already folded the virtual KSEG0/KSEG1 address with
//! [`crate::regions::virt_to_phys`]).

/// The memory interface the interpreter sees. All data and code accesses route
/// through this. [`crate::n64::N64`] is the production implementor; CPU unit
/// tests use a flat [`TestBus`].
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn read64(&mut self, addr: u32) -> u64;
    fn write8(&mut self, addr: u32, v: u8);
    fn write16(&mut self, addr: u32, v: u16);
    fn write32(&mut self, addr: u32, v: u32);
    fn write64(&mut self, addr: u32, v: u64);

    /// Instruction fetch (always a 32-bit aligned word).
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }
}

/// A flat-RAM bus for CPU unit tests: a small big-endian byte array covering a
/// configurable window, with everything else reading 0. Lets the opcode tests
/// run the interpreter without the full god-struct.
#[cfg(test)]
pub struct TestBus {
    pub ram: Vec<u8>,
}

#[cfg(test)]
impl TestBus {
    pub fn new(size: usize) -> Self {
        TestBus {
            ram: vec![0; size],
        }
    }

    #[inline]
    fn in_range(&self, addr: u32, n: usize) -> bool {
        (addr as usize).checked_add(n).map(|e| e <= self.ram.len()) == Some(true)
    }
}

#[cfg(test)]
impl Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u8 {
        if self.in_range(addr, 1) {
            self.ram[addr as usize]
        } else {
            0
        }
    }
    fn read16(&mut self, addr: u32) -> u16 {
        if self.in_range(addr, 2) {
            u16::from_be_bytes([self.ram[addr as usize], self.ram[addr as usize + 1]])
        } else {
            0
        }
    }
    fn read32(&mut self, addr: u32) -> u32 {
        if self.in_range(addr, 4) {
            let a = addr as usize;
            u32::from_be_bytes([self.ram[a], self.ram[a + 1], self.ram[a + 2], self.ram[a + 3]])
        } else {
            0
        }
    }
    fn read64(&mut self, addr: u32) -> u64 {
        ((self.read32(addr) as u64) << 32) | self.read32(addr.wrapping_add(4)) as u64
    }
    fn write8(&mut self, addr: u32, v: u8) {
        if self.in_range(addr, 1) {
            self.ram[addr as usize] = v;
        }
    }
    fn write16(&mut self, addr: u32, v: u16) {
        if self.in_range(addr, 2) {
            let b = v.to_be_bytes();
            self.ram[addr as usize] = b[0];
            self.ram[addr as usize + 1] = b[1];
        }
    }
    fn write32(&mut self, addr: u32, v: u32) {
        if self.in_range(addr, 4) {
            let a = addr as usize;
            let b = v.to_be_bytes();
            self.ram[a..a + 4].copy_from_slice(&b);
        }
    }
    fn write64(&mut self, addr: u32, v: u64) {
        self.write32(addr, (v >> 32) as u32);
        self.write32(addr.wrapping_add(4), v as u32);
    }
}
