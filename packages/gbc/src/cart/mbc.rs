//! Memory Bank Controllers (cartridge mappers).
//!
//! Spec: Pan Docs — MBCs (gbdev.io/pandocs/MBCs.html) and the per-controller
//! pages. The MBC sits between the CPU bus and the ROM/RAM chips: writes to
//! otherwise-ROM addresses (0x0000-0x7FFF) are intercepted as control-register
//! writes that select the active ROM/RAM bank and enable/disable RAM. This
//! file models the controller as a closed enum and implements the bank-select
//! routing; the register decode is intentionally minimal-but-correct for the
//! common controllers (full edge-case behavior is filled in alongside exec).

/// The cartridge controller, decoded from the header type byte (0x0147).
///
/// Closed enum — every bus access matches it exhaustively, no catch-all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MbcKind {
    /// No mapper: 32 KiB ROM, optional 8 KiB RAM (0x00/0x08/0x09).
    NoMbc,
    /// MBC1 (0x01-0x03): up to 2 MiB ROM / 32 KiB RAM, mode select.
    Mbc1,
    /// MBC2 (0x05-0x06): up to 256 KiB ROM, built-in 512x4-bit RAM.
    Mbc2,
    /// MBC3 (0x0F-0x13): up to 2 MiB ROM / 32 KiB RAM; RTC present when true.
    Mbc3 { rtc: bool },
    /// MBC5 (0x19-0x1E): up to 8 MiB ROM / 128 KiB RAM; 9-bit ROM bank.
    Mbc5 { rumble: bool },
}

impl MbcKind {
    /// Decode the cartridge-type byte at header 0x0147.
    pub fn from_cart_type(byte: u8) -> MbcKind {
        match byte {
            0x00 | 0x08 | 0x09 => MbcKind::NoMbc,
            0x01 | 0x02 | 0x03 => MbcKind::Mbc1,
            0x05 | 0x06 => MbcKind::Mbc2,
            0x0F | 0x10 => MbcKind::Mbc3 { rtc: true },
            0x11 | 0x12 | 0x13 => MbcKind::Mbc3 { rtc: false },
            0x19 | 0x1A | 0x1B => MbcKind::Mbc5 { rumble: false },
            0x1C | 0x1D | 0x1E => MbcKind::Mbc5 { rumble: true },
            // MMM01, MBC6/7, cameras, TAMA5, HuC etc. are not modeled yet; the
            // safest fallback is "no mapper" so the foundation still mounts.
            _ => MbcKind::NoMbc,
        }
    }

    /// Whether the header type byte indicates battery-backed RAM (save chip).
    pub fn has_battery(byte: u8) -> bool {
        matches!(
            byte,
            0x03 | 0x06 | 0x09 | 0x0D | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E | 0x22 | 0xFF
        )
    }
}

/// MBC3 real-time-clock latched registers. A latch write (0x6000-0x7FFF,
/// 0x00 then 0x01) copies the live clock into these for the CPU to read. We
/// reserve the state here; the tick logic lands with the timer/exec phase.
#[derive(Clone, Copy, Default)]
pub struct Rtc {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub days_lo: u8,
    /// Bit0: day counter MSB; bit6: halt; bit7: day-counter carry.
    pub days_hi: u8,
    /// 0x00 seen on the latch port; next 0x01 performs the latch.
    pub latch_armed: bool,
    /// Currently selected RTC register (0x08-0x0C) when RAM bank >= 0x08.
    pub mapped_reg: Option<u8>,
}

/// The live mapper state: the controller kind plus its bank-select registers.
///
/// The cart (which owns the ROM/RAM byte buffers) calls into these to translate
/// a CPU address into a flat ROM/RAM offset, and routes control-register writes
/// here. This struct holds no ROM/RAM bytes itself — only selection state.
#[derive(Clone)]
pub struct Mbc {
    pub kind: MbcKind,

    /// RAM (and, on MBC1, banking-mode-dependent ROM upper bits) enable latch.
    pub ram_enabled: bool,

    /// Selected ROM bank for the 0x4000-0x7FFF window. Width varies by
    /// controller (5 bits MBC1, 7 bits MBC3, 9 bits MBC5); the cart masks it to
    /// the actual bank count.
    pub rom_bank: u16,
    /// Selected external-RAM bank for 0xA000-0xBFFF (or RTC register on MBC3).
    pub ram_bank: u8,

    /// MBC1 banking mode: false = ROM banking (default), true = RAM banking /
    /// upper-ROM mode. Also affects whether 0x0000-0x3FFF can be re-banked.
    pub mode: bool,
    /// MBC1's secondary 2-bit register (RAM bank or ROM bits 5-6).
    pub bank_hi: u8,

    /// Total ROM banks (for masking the selected bank to the cart size).
    pub rom_bank_count: u16,
    /// Total RAM banks (0 when the cart has no external RAM).
    pub ram_bank_count: u8,

    /// MBC3 RTC, present only when `kind == Mbc3 { rtc: true }`.
    pub rtc: Rtc,
}

impl Mbc {
    /// Build the mapper state for a parsed header.
    pub fn new(kind: MbcKind, rom_bank_count: u16, ram_bank_count: u8) -> Mbc {
        Mbc {
            kind,
            ram_enabled: false,
            // The 0x4000-0x7FFF window powers up mapped to bank 1 (it can never
            // hold bank 0 on MBC1/2/3; MBC5 *can* select bank 0 there).
            rom_bank: 1,
            ram_bank: 0,
            mode: false,
            bank_hi: 0,
            rom_bank_count: rom_bank_count.max(2),
            ram_bank_count,
            rtc: Rtc::default(),
        }
    }

    // -------------------------------------------------------------------------
    // Address translation. The cart calls these with the raw CPU address; they
    // return a flat offset into the ROM/RAM byte buffer (already wrapped to the
    // cart's bank count). `None` from `ram_offset` means the access is blocked
    // (RAM disabled, no RAM present, or an RTC register is mapped instead).
    // -------------------------------------------------------------------------

    /// Flat ROM offset for a 0x0000-0x7FFF read.
    pub fn rom_offset(&self, addr: u16) -> usize {
        let bank = if addr < crate::regions::ROMN_START {
            // Low window: bank 0, except MBC1 RAM-banking mode can map the
            // upper-bit register here for >= 1 MiB carts.
            match self.kind {
                MbcKind::Mbc1 if self.mode => {
                    ((self.bank_hi as u16) << 5) & self.rom_mask()
                }
                _ => 0,
            }
        } else {
            self.effective_rom_bank() & self.rom_mask()
        };
        let within = (addr as usize) & (crate::regions::ROM_BANK_SIZE - 1);
        (bank as usize) * crate::regions::ROM_BANK_SIZE + within
    }

    /// Flat external-RAM offset for a 0xA000-0xBFFF access, or `None` if the
    /// access should be ignored (RAM disabled / absent / RTC selected).
    pub fn ram_offset(&self, addr: u16) -> Option<usize> {
        if !self.ram_enabled || self.ram_bank_count == 0 {
            return None;
        }
        // MBC3 with RAM bank 0x08-0x0C addresses the RTC, not RAM.
        if let MbcKind::Mbc3 { rtc: true } = self.kind {
            if self.ram_bank >= 0x08 {
                return None;
            }
        }
        let bank = match self.kind {
            // MBC1 only uses the RAM-bank register in RAM-banking mode.
            MbcKind::Mbc1 if self.mode => self.bank_hi as usize,
            MbcKind::Mbc1 => 0,
            _ => self.ram_bank as usize,
        };
        let bank = bank % (self.ram_bank_count as usize).max(1);
        let within = (addr as usize) & (crate::regions::ERAM_BANK_SIZE - 1);
        Some(bank * crate::regions::ERAM_BANK_SIZE + within)
    }

    /// The full ROM bank selected for the 0x4000-0x7FFF window, before masking.
    fn effective_rom_bank(&self) -> u16 {
        match self.kind {
            MbcKind::NoMbc => 1,
            MbcKind::Mbc1 => {
                // Low 5 bits from rom_bank (a 0 selection reads as 1); bits 5-6
                // come from bank_hi (only when not in RAM-banking mode).
                let low5 = if (self.rom_bank & 0x1F) == 0 {
                    1
                } else {
                    self.rom_bank & 0x1F
                };
                let hi = if self.mode { 0 } else { (self.bank_hi as u16) << 5 };
                low5 | hi
            }
            MbcKind::Mbc2 => {
                let b = self.rom_bank & 0x0F;
                if b == 0 {
                    1
                } else {
                    b
                }
            }
            MbcKind::Mbc3 { .. } => {
                let b = self.rom_bank & 0x7F;
                if b == 0 {
                    1
                } else {
                    b
                }
            }
            // MBC5 can legitimately select bank 0 in the high window.
            MbcKind::Mbc5 { .. } => self.rom_bank & 0x1FF,
        }
    }

    /// Power-of-two-rounded mask for the ROM bank index.
    fn rom_mask(&self) -> u16 {
        // rom_bank_count is a power of two from the header; mask = count - 1.
        self.rom_bank_count.wrapping_sub(1)
    }

    // -------------------------------------------------------------------------
    // Control-register writes (intercepted writes to 0x0000-0x7FFF). These only
    // mutate selection state; they never touch ROM bytes.
    // -------------------------------------------------------------------------

    /// Route a write to a 0x0000-0x7FFF address into the mapper's registers.
    pub fn write_control(&mut self, addr: u16, value: u8) {
        match self.kind {
            MbcKind::NoMbc => {}
            MbcKind::Mbc1 => self.write_mbc1(addr, value),
            MbcKind::Mbc2 => self.write_mbc2(addr, value),
            MbcKind::Mbc3 { rtc } => self.write_mbc3(addr, value, rtc),
            MbcKind::Mbc5 { .. } => self.write_mbc5(addr, value),
        }
    }

    fn write_mbc1(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            0x2000..=0x3FFF => {
                // 5-bit ROM bank; 0 is bumped to 1 at use time.
                self.rom_bank = (self.rom_bank & 0x60) | (value as u16 & 0x1F);
            }
            0x4000..=0x5FFF => self.bank_hi = value & 0x03,
            0x6000..=0x7FFF => self.mode = (value & 0x01) != 0,
            _ => {}
        }
    }

    fn write_mbc2(&mut self, addr: u16, value: u8) {
        // MBC2 multiplexes RAM-enable and ROM-bank on the low half; bit 8 of
        // the address selects which (Pan Docs MBC2).
        if addr < 0x4000 {
            if addr & 0x0100 == 0 {
                self.ram_enabled = (value & 0x0F) == 0x0A;
            } else {
                let b = value as u16 & 0x0F;
                self.rom_bank = if b == 0 { 1 } else { b };
            }
        }
    }

    fn write_mbc3(&mut self, addr: u16, value: u8, has_rtc: bool) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            0x2000..=0x3FFF => {
                let b = value & 0x7F;
                self.rom_bank = if b == 0 { 1 } else { b as u16 };
            }
            0x4000..=0x5FFF => {
                self.ram_bank = value;
                if has_rtc && (0x08..=0x0C).contains(&value) {
                    self.rtc.mapped_reg = Some(value);
                } else {
                    self.rtc.mapped_reg = None;
                }
            }
            0x6000..=0x7FFF => {
                // RTC latch: 0x00 then 0x01 latches the live clock.
                if has_rtc {
                    if value == 0x00 {
                        self.rtc.latch_armed = true;
                    } else if value == 0x01 && self.rtc.latch_armed {
                        self.rtc.latch_armed = false;
                        // (live->latched copy happens in the RTC tick module)
                    } else {
                        self.rtc.latch_armed = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn write_mbc5(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            // Low 8 bits of the 9-bit ROM bank.
            0x2000..=0x2FFF => self.rom_bank = (self.rom_bank & 0x100) | value as u16,
            // Bit 8 of the ROM bank.
            0x3000..=0x3FFF => self.rom_bank = (self.rom_bank & 0x0FF) | ((value as u16 & 1) << 8),
            // RAM bank (low nibble; bit 3 may drive rumble on rumble carts).
            0x4000..=0x5FFF => self.ram_bank = value & 0x0F,
            _ => {}
        }
    }
}
