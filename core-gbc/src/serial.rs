//! Serial transfer (SB 0xFF01 / SC 0xFF02), minimal.
//!
//! Spec: Pan Docs — Serial Data Transfer (gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html).
//!
//! We don't model a real link cable. We model just enough for single-player
//! games not to hang: a transfer started as the *internal*-clock master
//! (SC bit 7 = transfer, bit 0 = internal clock) shifts in 0xFF (no peer) and
//! completes after the appropriate number of cycles, then raises the Serial
//! interrupt. External-clock transfers (bit 0 = 0, waiting for a peer) never
//! complete on their own, which is also the correct single-player behavior.

use crate::interrupts::{Interrupt, Irq};

/// T-cycles per serial bit at the normal-speed 8192 Hz internal clock
/// (4194304 / 8192 = 512 cycles/bit), times 8 bits = 4096 cycles per byte.
const NORMAL_TRANSFER_CYCLES: u32 = 512 * 8;

#[derive(Default, Clone)]
pub struct Serial {
    /// SB (0xFF01) — the transfer data byte.
    pub sb: u8,
    /// SC (0xFF02) — control: bit 7 start, bit 1 clock speed (CGB), bit 0 source.
    pub sc: u8,
    /// Remaining T-cycles until the in-progress transfer completes (0 = idle).
    countdown: u32,
}

impl Serial {
    /// Advance an in-progress internal-clock transfer.
    pub fn step(&mut self, cycles: u32, irq: &mut Irq) {
        if self.countdown == 0 {
            return;
        }
        if self.countdown <= cycles {
            self.countdown = 0;
            // No peer: shifting in all-ones.
            self.sb = 0xFF;
            self.sc &= 0x7F; // clear the transfer-start bit
            irq.request(Interrupt::Serial);
        } else {
            self.countdown -= cycles;
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF01 => self.sb,
            // Bits 6-1 read as 1 (bit 1 is meaningful only on CGB); keep the
            // written bits and set the unused ones.
            0xFF02 => self.sc | 0x7C,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0xFF01 => self.sb = v,
            0xFF02 => {
                self.sc = v;
                // Start an internal-clock transfer (bit 7 set, bit 0 = internal).
                if (v & 0x81) == 0x81 {
                    // CGB fast-clock (bit 1) divides the transfer time by 4.
                    let div = if v & 0x02 != 0 { 4 } else { 1 };
                    self.countdown = NORMAL_TRANSFER_CYCLES / div;
                } else if v & 0x80 == 0 {
                    self.countdown = 0;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_transfer_completes_and_irqs() {
        let mut s = Serial::default();
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        s.write(0xFF01, 0x42);
        s.write(0xFF02, 0x81); // start, internal clock
        s.step(NORMAL_TRANSFER_CYCLES, &mut irq);
        assert_eq!(s.read(0xFF01), 0xFF); // shifted in 0xFF
        assert_eq!(s.read(0xFF02) & 0x80, 0); // transfer flag cleared
        assert_eq!(irq.pending() & Interrupt::Serial.mask(), Interrupt::Serial.mask());
    }

    #[test]
    fn external_clock_transfer_stalls() {
        let mut s = Serial::default();
        let mut irq = Irq::new();
        irq.write_ie(0xFF);
        s.write(0xFF02, 0x80); // start, external clock (no peer)
        s.step(100_000, &mut irq);
        assert_eq!(irq.pending() & Interrupt::Serial.mask(), 0); // never completes
    }
}
