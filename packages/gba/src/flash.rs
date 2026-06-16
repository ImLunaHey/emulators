// Flash 128 KB save chip emulation — minimal state machine that's enough
// for FireRed's save layer (Macronix MX29L1000, manufacturer 0xC2, device 0x09).
//
// FireRed probes the chip by issuing the standard Atmel/SST/Macronix command
// sequence at 0xE005555/0xE002AAA. We implement: read mode, ID mode, sector
// erase, byte program, and bank switch (BankSelect cmd 0xB0 at 0xE000000).
//
// The chip is split into two 64 KB banks; only the active bank is visible
// through the 0x0E000000 window.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlashCmd {
    Normal,
    AwaitFirst,
    AwaitSecond,
    Identify,
    EraseAwaitFirst,
    EraseAwaitSecond,
    EraseSector,
    Program,
    BankSelect,
}

pub struct Flash128 {
    pub data: Vec<u8>, // 128 KB, 2 banks of 64 KB
    pub state: FlashCmd,
    pub id_mode: bool,
    pub bank: u32,
    // Called whenever the chip data changes; the host wires this to persist
    // to localStorage / IndexedDB. In TS this was an `onChange` callback; here
    // we expose it as a dirty flag the host polls and clears.
    pub dirty: bool,
}

impl Flash128 {
    // Manufacturer/device pair for Macronix MX29L010 — the only 128 KB
    // Flash variant whose command set our chip implementation matches.
    // (Lying about being Sanyo would route the game's save driver through
    // Sanyo-specific commands we don't handle.)
    pub const ID_MAKER: u8 = 0xC2;
    pub const ID_DEVICE: u8 = 0x09;

    pub fn new() -> Self {
        Flash128 {
            data: vec![0; 0x20000],
            state: FlashCmd::Normal,
            id_mode: false,
            bank: 0,
            dirty: false,
        }
    }
}

impl Default for Flash128 {
    fn default() -> Self {
        Flash128::new()
    }
}

impl crate::Save for Flash128 {
    // Load save data from a serialized 128 KB buffer (or shorter — padded
    // with 0xFF). Used to restore the game's save on page load.
    fn load_save(&mut self, bytes: &[u8]) {
        self.data.iter_mut().for_each(|b| *b = 0xFF);
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
    }

    fn read(&mut self, addr: u32) -> u32 {
        let addr = addr & 0xFFFF;
        if self.id_mode {
            if addr == 0 {
                return Flash128::ID_MAKER as u32;
            }
            if addr == 1 {
                return Flash128::ID_DEVICE as u32;
            }
        }
        self.data[((self.bank << 16) | addr) as usize] as u32
    }

    fn write(&mut self, addr: u32, v: u32) {
        let addr = addr & 0xFFFF;
        let v = v & 0xFF;

        match self.state {
            FlashCmd::Program => {
                self.data[((self.bank << 16) | addr) as usize] = v as u8;
                self.state = FlashCmd::Normal;
                self.dirty = true;
                return;
            }

            FlashCmd::BankSelect => {
                self.bank = v & 1;
                self.state = FlashCmd::Normal;
                return;
            }

            FlashCmd::EraseSector => {
                // 0x30 → erase that 4 KB sector. The address's low 12 bits must
                // be zero (sector-aligned); only the bank + sector-index portion
                // matters.
                if (addr & 0xFFF) == 0 && v == 0x30 {
                    let base = ((self.bank << 16) | (addr & 0xF000)) as usize;
                    self.data[base..base + 0x1000]
                        .iter_mut()
                        .for_each(|b| *b = 0xFF);
                    self.state = FlashCmd::Normal;
                    self.dirty = true;
                    return;
                }
                // 0x10 → 0x5555 → erase entire chip.
                if addr == 0x5555 && v == 0x10 {
                    self.data.iter_mut().for_each(|b| *b = 0xFF);
                    self.state = FlashCmd::Normal;
                    self.dirty = true;
                    return;
                }
                self.state = FlashCmd::Normal;
                return;
            }

            _ => {}
        }

        // The erase command sequence requires a SECOND unlock pair after
        // 0x80 (= AA→5555, 55→2AAA, 80→5555, then AA→5555, 55→2AAA, then
        // either 0x30→sectoraddr or 0x10→5555). We MUST match the
        // EraseAwait* states BEFORE the generic AwaitFirst/AwaitSecond
        // patterns, otherwise the second AA→5555 short-circuits back to
        // the start of an unlock sequence and the erase never completes —
        // which is the symptom Pokemon Ruby reported ("saving... don't
        // turn off" stuck forever).
        if self.state == FlashCmd::EraseAwaitFirst && addr == 0x5555 && v == 0xAA {
            self.state = FlashCmd::EraseAwaitSecond;
            return;
        }
        if self.state == FlashCmd::EraseAwaitSecond && addr == 0x2AAA && v == 0x55 {
            self.state = FlashCmd::EraseSector;
            return;
        }
        // (The EraseSector dispatch — both 0x30 sector erase and 0x10 chip
        //  erase — is handled at the top of write() inside the switch
        //  statement, since the FlashCmd.EraseSector branch is checked
        //  there before we reach the generic unlock paths.)
        // Generic unlock cycle 1.
        if addr == 0x5555 && v == 0xAA {
            self.state = FlashCmd::AwaitFirst;
            return;
        }
        if self.state == FlashCmd::AwaitFirst && addr == 0x2AAA && v == 0x55 {
            self.state = FlashCmd::AwaitSecond;
            return;
        }
        if self.state == FlashCmd::AwaitSecond && addr == 0x5555 {
            match v {
                0x90 => {
                    self.id_mode = true;
                    self.state = FlashCmd::Normal;
                    return;
                }
                0xF0 => {
                    self.id_mode = false;
                    self.state = FlashCmd::Normal;
                    return;
                }
                0x80 => {
                    self.state = FlashCmd::EraseAwaitFirst;
                    return;
                }
                0xA0 => {
                    self.state = FlashCmd::Program;
                    return;
                }
                0xB0 => {
                    self.state = FlashCmd::BankSelect;
                    return;
                }
                _ => {}
            }
        }
        self.state = FlashCmd::Normal;
    }

    fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Save;

    fn unlock(f: &mut Flash128) {
        f.write(0x5555, 0xAA);
        f.write(0x2AAA, 0x55);
    }

    #[test]
    fn id_mode_reports_macronix() {
        let mut f = Flash128::new();
        unlock(&mut f);
        f.write(0x5555, 0x90);
        assert!(f.id_mode);
        assert_eq!(f.read(0), 0xC2);
        assert_eq!(f.read(1), 0x09);
        unlock(&mut f);
        f.write(0x5555, 0xF0);
        assert!(!f.id_mode);
    }

    #[test]
    fn program_byte() {
        let mut f = Flash128::new();
        f.load_save(&[]);
        unlock(&mut f);
        f.write(0x5555, 0xA0);
        f.write(0x1234, 0x42);
        assert_eq!(f.read(0x1234), 0x42);
        assert!(f.dirty);
    }

    #[test]
    fn bank_switch_visibility() {
        let mut f = Flash128::new();
        f.load_save(&[]);
        // program a byte in bank 0
        unlock(&mut f);
        f.write(0x5555, 0xA0);
        f.write(0x0000, 0x11);
        // switch to bank 1
        unlock(&mut f);
        f.write(0x5555, 0xB0);
        f.write(0x0000, 1);
        assert_eq!(f.bank, 1);
        f.write(0x5555, 0xAA); // re-unlock for program in bank 1
        f.write(0x2AAA, 0x55);
        f.write(0x5555, 0xA0);
        f.write(0x0000, 0x22);
        assert_eq!(f.read(0x0000), 0x22);
        assert_eq!(f.data[0x0000], 0x11);
        assert_eq!(f.data[0x10000], 0x22);
    }

    #[test]
    fn sector_erase() {
        let mut f = Flash128::new();
        f.load_save(&[0x00; 0x20000]);
        // erase 4 KB sector at 0x2000
        unlock(&mut f);
        f.write(0x5555, 0x80);
        unlock(&mut f);
        f.write(0x2000, 0x30);
        for i in 0x2000..0x3000 {
            assert_eq!(f.data[i], 0xFF);
        }
        assert_eq!(f.data[0x1FFF], 0x00);
        assert_eq!(f.data[0x3000], 0x00);
    }

    #[test]
    fn chip_erase() {
        let mut f = Flash128::new();
        f.load_save(&[0x00; 0x20000]);
        unlock(&mut f);
        f.write(0x5555, 0x80);
        unlock(&mut f);
        f.write(0x5555, 0x10);
        assert!(f.data.iter().all(|&b| b == 0xFF));
    }
}
