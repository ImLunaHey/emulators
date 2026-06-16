//! The TLCS-900/H memory bus. The CPU has a single 24-bit byte-addressed memory
//! space (no separate I/O port space — peripherals are memory-mapped). `Ngpc`
//! (see `ngpc.rs`) is the production implementor; CPU unit tests use a flat-RAM
//! stub.
//!
//! Multi-byte accesses are little-endian. The default 16/32-bit helpers are
//! derived from `read8`/`write8` so implementors only supply the byte methods.

pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, v: u8);

    #[inline]
    fn read16(&mut self, addr: u32) -> u16 {
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn read32(&mut self, addr: u32) -> u32 {
        let lo = self.read16(addr) as u32;
        let hi = self.read16(addr.wrapping_add(2)) as u32;
        (hi << 16) | lo
    }

    #[inline]
    fn write16(&mut self, addr: u32, v: u16) {
        self.write8(addr, (v & 0xFF) as u8);
        self.write8(addr.wrapping_add(1), (v >> 8) as u8);
    }

    #[inline]
    fn write32(&mut self, addr: u32, v: u32) {
        self.write16(addr, (v & 0xFFFF) as u16);
        self.write16(addr.wrapping_add(2), (v >> 16) as u16);
    }
}

/// A flat 24-bit RAM bus used only by CPU unit tests.
#[cfg(test)]
pub struct TestBus {
    pub mem: Vec<u8>,
}

#[cfg(test)]
impl TestBus {
    pub fn new() -> TestBus {
        TestBus {
            mem: vec![0u8; 0x100_0000],
        }
    }
}

#[cfg(test)]
impl Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.mem[(addr & 0xFF_FFFF) as usize]
    }
    fn write8(&mut self, addr: u32, v: u8) {
        self.mem[(addr & 0xFF_FFFF) as usize] = v;
    }
}
