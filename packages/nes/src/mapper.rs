//! Cartridge mappers as a closed enum (no trait objects, exhaustive `match`).
//!
//! Implemented: 0 NROM, 1 MMC1, 2 UxROM, 3 CNROM, 4 MMC3.
//! Each variant maps a CPU/PPU address to a flat offset into PRG-ROM / CHR and
//! optionally overrides nametable mirroring. MMC3 additionally drives a
//! scanline IRQ counter clocked by PPU A12 rising edges.
//!
//! Bank-offset convention: `prg_offset(addr)` returns a byte offset into the
//! PRG-ROM `Vec`; the cart masks it modulo the ROM length, so we work in raw
//! (possibly out-of-range) offsets and let the cart fold them.

use crate::cart::Mirroring;

const PRG_BANK_16K: usize = 16 * 1024;
const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_8K: usize = 8 * 1024;
const CHR_BANK_4K: usize = 4 * 1024;
const CHR_BANK_1K: usize = 1024;

pub enum Mapper {
    Nrom(Nrom),
    Mmc1(Mmc1),
    Uxrom(Uxrom),
    Cnrom(Cnrom),
    Mmc3(Mmc3),
}

impl Mapper {
    /// `prg_banks_16k` / `chr_banks_8k` are the header-declared counts (used to
    /// place the fixed last PRG bank and bound bank registers).
    pub fn new(id: u16, prg_banks_16k: usize, chr_banks_8k: usize) -> Option<Mapper> {
        Some(match id {
            0 => Mapper::Nrom(Nrom { prg_banks_16k }),
            1 => Mapper::Mmc1(Mmc1::new(prg_banks_16k)),
            2 => Mapper::Uxrom(Uxrom::new(prg_banks_16k)),
            3 => Mapper::Cnrom(Cnrom::default()),
            4 => Mapper::Mmc3(Mmc3::new(prg_banks_16k * 2, chr_banks_8k)),
            _ => return None,
        })
    }

    #[inline]
    pub fn prg_offset(&self, addr: u16) -> usize {
        match self {
            Mapper::Nrom(m) => m.prg_offset(addr),
            Mapper::Mmc1(m) => m.prg_offset(addr),
            Mapper::Uxrom(m) => m.prg_offset(addr),
            Mapper::Cnrom(m) => m.prg_offset(addr),
            Mapper::Mmc3(m) => m.prg_offset(addr),
        }
    }

    #[inline]
    pub fn chr_offset(&self, addr: u16) -> usize {
        match self {
            Mapper::Nrom(_) => addr as usize,
            Mapper::Mmc1(m) => m.chr_offset(addr),
            Mapper::Uxrom(_) => addr as usize,
            Mapper::Cnrom(m) => m.chr_offset(addr),
            Mapper::Mmc3(m) => m.chr_offset(addr),
        }
    }

    #[inline]
    pub fn cpu_write(&mut self, addr: u16, v: u8) {
        match self {
            Mapper::Nrom(_) => {}
            Mapper::Mmc1(m) => m.write(addr, v),
            Mapper::Uxrom(m) => m.write(addr, v),
            Mapper::Cnrom(m) => m.write(addr, v),
            Mapper::Mmc3(m) => m.write(addr, v),
        }
    }

    #[inline]
    pub fn mirroring_override(&self) -> Option<Mirroring> {
        match self {
            Mapper::Mmc1(m) => Some(m.mirroring()),
            Mapper::Mmc3(m) => Some(m.mirroring()),
            _ => None,
        }
    }

    #[inline]
    pub fn ppu_a12_clock(&mut self, addr: u16) {
        if let Mapper::Mmc3(m) = self {
            m.ppu_a12_clock(addr);
        }
    }

    #[inline]
    pub fn take_irq(&mut self) -> bool {
        match self {
            Mapper::Mmc3(m) => {
                let p = m.irq_pending;
                m.irq_pending = false;
                p
            }
            _ => false,
        }
    }
}

// --------------------------------------------------------------------------
// Mapper 0: NROM. 16 or 32 KiB PRG, fixed CHR. No bank switching.
// --------------------------------------------------------------------------
pub struct Nrom {
    prg_banks_16k: usize,
}
impl Nrom {
    #[inline]
    fn prg_offset(&self, addr: u16) -> usize {
        let a = addr as usize - 0x8000;
        if self.prg_banks_16k <= 1 {
            a & 0x3FFF // 16 KiB mirrored into both halves
        } else {
            a // 32 KiB linear
        }
    }
}

// --------------------------------------------------------------------------
// Mapper 1: MMC1. Serial 5-bit shift register; control/CHR0/CHR1/PRG regs.
// --------------------------------------------------------------------------
pub struct Mmc1 {
    shift: u8,
    shift_count: u8,
    control: u8, // bit0-1 mirroring, 2-3 prg mode, 4 chr mode
    chr_bank0: u8,
    chr_bank1: u8,
    prg_bank: u8,
    prg_banks_16k: usize,
}
impl Mmc1 {
    fn new(prg_banks_16k: usize) -> Mmc1 {
        Mmc1 {
            shift: 0,
            shift_count: 0,
            // Power-on: PRG mode 3 (fix last bank at $C000) is the common reset
            // state, so games that never write control still boot.
            control: 0x0C,
            chr_bank0: 0,
            chr_bank1: 0,
            prg_bank: 0,
            prg_banks_16k,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        if v & 0x80 != 0 {
            // Reset: clear shift register, set PRG mode to fix last bank.
            self.shift = 0;
            self.shift_count = 0;
            self.control |= 0x0C;
            return;
        }
        self.shift = (self.shift >> 1) | ((v & 1) << 4);
        self.shift_count += 1;
        if self.shift_count == 5 {
            let val = self.shift & 0x1F;
            match (addr >> 13) & 0x03 {
                0 => self.control = val,
                1 => self.chr_bank0 = val,
                2 => self.chr_bank1 = val,
                _ => self.prg_bank = val,
            }
            self.shift = 0;
            self.shift_count = 0;
        }
    }

    fn mirroring(&self) -> Mirroring {
        match self.control & 0x03 {
            0 => Mirroring::SingleLower,
            1 => Mirroring::SingleUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        }
    }

    #[inline]
    fn prg_offset(&self, addr: u16) -> usize {
        let prg_mode = (self.control >> 2) & 0x03;
        let bank = (self.prg_bank & 0x0F) as usize;
        let last = self.prg_banks_16k.saturating_sub(1);
        let a = addr as usize - 0x8000;
        match prg_mode {
            0 | 1 => {
                // 32 KiB mode: ignore low bit of bank.
                let base = (bank & !1) * PRG_BANK_16K;
                base + a
            }
            2 => {
                // Fix first bank at $8000, switch $C000.
                if addr < 0xC000 {
                    a & 0x3FFF
                } else {
                    bank * PRG_BANK_16K + (a & 0x3FFF)
                }
            }
            _ => {
                // mode 3: switch $8000, fix last bank at $C000.
                if addr < 0xC000 {
                    bank * PRG_BANK_16K + (a & 0x3FFF)
                } else {
                    last * PRG_BANK_16K + (a & 0x3FFF)
                }
            }
        }
    }

    #[inline]
    fn chr_offset(&self, addr: u16) -> usize {
        let chr_mode = (self.control >> 4) & 0x01;
        let a = addr as usize;
        if chr_mode == 0 {
            // 8 KiB mode: ignore low bit.
            let bank = (self.chr_bank0 & !1) as usize;
            bank * CHR_BANK_4K + a
        } else {
            // 4 KiB mode: two independent banks.
            if addr < 0x1000 {
                (self.chr_bank0 as usize) * CHR_BANK_4K + a
            } else {
                (self.chr_bank1 as usize) * CHR_BANK_4K + (a - 0x1000)
            }
        }
    }
}

// --------------------------------------------------------------------------
// Mapper 2: UxROM. $8000 switchable 16 KiB PRG, $C000 fixed last bank.
// CHR is RAM. Bus conflicts ignored.
// --------------------------------------------------------------------------
pub struct Uxrom {
    bank: u8,
    prg_banks_16k: usize,
}
impl Uxrom {
    fn new(prg_banks_16k: usize) -> Uxrom {
        Uxrom { bank: 0, prg_banks_16k }
    }
    fn write(&mut self, _addr: u16, v: u8) {
        self.bank = v;
    }
    #[inline]
    fn prg_offset(&self, addr: u16) -> usize {
        let a = addr as usize - 0x8000;
        if addr < 0xC000 {
            (self.bank as usize) * PRG_BANK_16K + a
        } else {
            self.prg_banks_16k.saturating_sub(1) * PRG_BANK_16K + (a - 0x4000)
        }
    }
}

// --------------------------------------------------------------------------
// Mapper 3: CNROM. Fixed PRG, switchable 8 KiB CHR bank.
// --------------------------------------------------------------------------
#[derive(Default)]
pub struct Cnrom {
    chr_bank: u8,
    prg_banks_16k: usize,
}
impl Cnrom {
    fn write(&mut self, _addr: u16, v: u8) {
        self.chr_bank = v & 0x03;
    }
    #[inline]
    fn prg_offset(&self, addr: u16) -> usize {
        let a = addr as usize - 0x8000;
        if self.prg_banks_16k <= 1 {
            a & 0x3FFF
        } else {
            a
        }
    }
    #[inline]
    fn chr_offset(&self, addr: u16) -> usize {
        (self.chr_bank as usize) * CHR_BANK_8K + addr as usize
    }
}

// --------------------------------------------------------------------------
// Mapper 4: MMC3. 8 bank registers (R0-R7), PRG/CHR mode bits, mirroring,
// and a scanline IRQ counter clocked by PPU A12 rising edges.
// --------------------------------------------------------------------------
pub struct Mmc3 {
    bank_select: u8, // bit0-2 reg index, bit6 prg mode, bit7 chr mode
    regs: [u8; 8],
    mirror_horizontal: bool,
    prg_banks_8k: usize,

    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enable: bool,
    pub irq_pending: bool,
    last_a12: bool,
}
impl Mmc3 {
    fn new(prg_banks_8k: usize, _chr_banks_8k: usize) -> Mmc3 {
        Mmc3 {
            bank_select: 0,
            regs: [0; 8],
            mirror_horizontal: false,
            prg_banks_8k: prg_banks_8k.max(1),
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enable: false,
            irq_pending: false,
            last_a12: false,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        let even = addr & 1 == 0;
        match (addr & 0xE001, even) {
            (0x8000, _) => self.bank_select = v,
            (0x8001, _) => {
                let idx = (self.bank_select & 0x07) as usize;
                self.regs[idx] = v;
            }
            (0xA000, _) => self.mirror_horizontal = v & 1 == 1,
            (0xA001, _) => { /* PRG-RAM protect — unimplemented */ }
            (0xC000, _) => self.irq_latch = v,
            (0xC001, _) => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            (0xE000, _) => {
                self.irq_enable = false;
                self.irq_pending = false;
            }
            (0xE001, _) => self.irq_enable = true,
            _ => {}
        }
    }

    fn mirroring(&self) -> Mirroring {
        if self.mirror_horizontal {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        }
    }

    #[inline]
    fn prg_offset(&self, addr: u16) -> usize {
        let prg_mode = (self.bank_select >> 6) & 1;
        let last = self.prg_banks_8k - 1;
        let second_last = self.prg_banks_8k.saturating_sub(2);
        // Which 8 KiB window of $8000-$FFFF.
        let window = (addr as usize - 0x8000) / PRG_BANK_8K;
        let bank = match (window, prg_mode) {
            (0, 0) => self.regs[6] as usize,
            (0, 1) => second_last,
            (1, _) => self.regs[7] as usize,
            (2, 0) => second_last,
            (2, 1) => self.regs[6] as usize,
            (3, _) => last,
            _ => last,
        };
        (bank % self.prg_banks_8k) * PRG_BANK_8K + (addr as usize - 0x8000) % PRG_BANK_8K
    }

    #[inline]
    fn chr_offset(&self, addr: u16) -> usize {
        let chr_mode = (self.bank_select >> 7) & 1;
        let a = addr as usize;
        // Two 2 KiB banks (R0,R1) and four 1 KiB banks (R2-R5); chr_mode swaps
        // which half of the pattern table each set covers.
        let region = a / CHR_BANK_1K; // 0..8
        let (bank, within) = if chr_mode == 0 {
            match region {
                0 => ((self.regs[0] & 0xFE) as usize, a),
                1 => ((self.regs[0] & 0xFE) as usize + 1, a - CHR_BANK_1K),
                2 => ((self.regs[1] & 0xFE) as usize, a - 2 * CHR_BANK_1K),
                3 => ((self.regs[1] & 0xFE) as usize + 1, a - 3 * CHR_BANK_1K),
                4 => (self.regs[2] as usize, a - 4 * CHR_BANK_1K),
                5 => (self.regs[3] as usize, a - 5 * CHR_BANK_1K),
                6 => (self.regs[4] as usize, a - 6 * CHR_BANK_1K),
                _ => (self.regs[5] as usize, a - 7 * CHR_BANK_1K),
            }
        } else {
            match region {
                0 => (self.regs[2] as usize, a),
                1 => (self.regs[3] as usize, a - CHR_BANK_1K),
                2 => (self.regs[4] as usize, a - 2 * CHR_BANK_1K),
                3 => (self.regs[5] as usize, a - 3 * CHR_BANK_1K),
                4 => ((self.regs[0] & 0xFE) as usize, a - 4 * CHR_BANK_1K),
                5 => ((self.regs[0] & 0xFE) as usize + 1, a - 5 * CHR_BANK_1K),
                6 => ((self.regs[1] & 0xFE) as usize, a - 6 * CHR_BANK_1K),
                _ => ((self.regs[1] & 0xFE) as usize + 1, a - 7 * CHR_BANK_1K),
            }
        };
        bank * CHR_BANK_1K + within
    }

    /// Clock the IRQ counter on a PPU A12 (bit 12) low→high transition.
    fn ppu_a12_clock(&mut self, addr: u16) {
        let a12 = (addr & 0x1000) != 0;
        if a12 && !self.last_a12 {
            if self.irq_counter == 0 || self.irq_reload {
                self.irq_counter = self.irq_latch;
                self.irq_reload = false;
            } else {
                self.irq_counter -= 1;
            }
            if self.irq_counter == 0 && self.irq_enable {
                self.irq_pending = true;
            }
        }
        self.last_a12 = a12;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nrom_16k_mirrors() {
        let m = Nrom { prg_banks_16k: 1 };
        assert_eq!(m.prg_offset(0x8000), 0);
        assert_eq!(m.prg_offset(0xC000), 0); // mirror
        assert_eq!(m.prg_offset(0xFFFF), 0x3FFF);
    }

    #[test]
    fn uxrom_fixes_last_bank() {
        let mut m = Uxrom::new(8);
        m.write(0x8000, 2);
        assert_eq!(m.prg_offset(0x8000), 2 * PRG_BANK_16K);
        // $C000 always reads the last 16 KiB bank.
        assert_eq!(m.prg_offset(0xC000), 7 * PRG_BANK_16K);
    }

    #[test]
    fn cnrom_switches_chr() {
        let mut m = Cnrom { chr_bank: 0, prg_banks_16k: 2 };
        m.write(0x8000, 3);
        assert_eq!(m.chr_offset(0x0000), 3 * CHR_BANK_8K);
    }

    #[test]
    fn mmc1_serial_write_sets_prg() {
        let mut m = Mmc1::new(8);
        // Write 5 bits LSB-first to load PRG bank reg ($E000) with value 5.
        for bit in [1u8, 0, 1, 0, 0] {
            m.write(0xE000, bit);
        }
        assert_eq!(m.prg_bank & 0x0F, 0b00101);
    }

    #[test]
    fn mmc3_irq_counts_down() {
        let mut m = Mmc3::new(16, 8);
        m.write(0xC000, 2); // latch
        m.write(0xC001, 0); // reload
        m.write(0xE001, 0); // enable
        // First A12 edge reloads to 2, subsequent edges count down.
        m.ppu_a12_clock(0x0000);
        m.ppu_a12_clock(0x1000); // reload -> 2
        m.ppu_a12_clock(0x0000);
        m.ppu_a12_clock(0x1000); // 2 -> 1
        assert!(!m.irq_pending);
        m.ppu_a12_clock(0x0000);
        m.ppu_a12_clock(0x1000); // 1 -> 0, fire
        assert!(m.irq_pending);
    }
}
