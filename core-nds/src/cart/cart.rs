//! NDS cartridge command interface. The game pokes a 64-bit command into ROMCMD
//! (0x040001A8..AF) and a control word into ROMCTRL (0x040001A4); ROMCTRL bit 31
//! (block-start) kicks off a transfer, the hardware fills a buffer, and the CPU
//! reads ROMDATA (0x04100010) in 32-bit words. The cart also carries a save
//! backup chip on a separate SPI bus (AUXSPICNT 0x040001A0 / AUXSPIDATA
//! 0x040001A2). Ported from ../../ds-recomp/src/cart/cart.ts.
//!
//! Three protocol phases (GBATEK §"DS Cart Protocol"):
//!   - `Raw`  (boot): 0x9F dummy, 0x00 read-header, 0x90 chip-ID.
//!   - `Key1` after cmd 0x3C: chip-ID then secure-area stream, then →Key2.
//!   - `Key2`: encrypted 0xB7/0xB8 — we treat KEY2 as transparent.
//! No real BIOS bytes are used; games needing authentic KEY1/KEY2 (Pokemon
//! anti-piracy) won't pass, but every other SDK cart driver sees the state
//! machine it expects.
//!
//! Ownership (CONTRACT.md): the TS `Cart` stored `onTransferReady` /
//! `onTransferEnd` callbacks back into the IO module — those cycles don't exist
//! here. Instead the IO dispatch in `Nds` calls the cart, inspects the returned
//! `TransferEvent`, and fires the cart-ready DMA / cart-end IRQ itself. The
//! cart owns ONLY its own ROM image, command latch, transfer buffer, and save
//! chip state.

/// ROMCTRL block-size codes (bits 24..26). Index 7 ("4") is the 4-byte form.
const BLOCK_SIZE_TABLE: [u32; 8] = [0, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 4];

/// Cart command-protocol phase. Games walk Raw → Key1 → Key2 during boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Pre-encryption: 0x9F/0x00/0x90 + the 0x3C activate-KEY1 marker.
    Raw,
    /// Post-0x3C: chip-ID, secure-area stream, then the activate-KEY2 marker.
    Key1,
    /// Post-KEY1: encrypted 0xB7 (read) / 0xB8 (chip-ID), treated transparently.
    Key2,
}

/// Side effects a cart access wants the `Nds` IO dispatch to run after the call
/// returns. The TS used stored callbacks; we return this enum so the cart never
/// holds a reference back into the bus/IO graph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransferEvent {
    /// Nothing to do.
    None,
    /// A transfer buffer was just filled — fire the cart-ready DMA timing.
    Ready,
    /// The last ROMDATA word was read AND AUXSPICNT bit 14 is set — the IO
    /// dispatch should raise IRQ_CART (bit 19) on the slot-owning core.
    TransferEnd,
}

pub struct Cart {
    /// The full `.nds` ROM image.
    pub rom: Vec<u8>,

    /// 8-byte command latch. Game writes one byte at a time at 0x1A8..AF.
    cmd: [u8; 8],

    /// Buffer prepared by `start_transfer`. ROMDATA reads pull 32-bit words from
    /// here; the transfer is complete once `pos >= buf.len()`.
    buf: Vec<u8>,
    pos: usize,

    /// Last ROMCTRL value (bit 31 = busy; also holds block-size bits).
    romctrl: u32,
    /// AUXSPICNT (0x040001A0) — low 16 bits.
    auxspicnt: u32,

    /// Protocol phase.
    phase: Phase,
    /// KEY1-phase command counter (chip-ID → secure-area → activate-KEY2).
    key1_cmd_count: u32,

    // ─── Save backup chip (AUXSPI device) ───────────────────────────────
    /// Backing blob — empty FLASH reads as 0xFF. Grown on demand by writes.
    sav: Vec<u8>,
    /// Set on any write to the save blob; the host persists it out of band.
    pub sav_dirty: bool,
    /// Current SPI command byte (0 = idle).
    sav_cmd: u8,
    /// Status-register user bits 2..7 (block-protect + SRWD), writable via WRSR.
    sav_status_user_bits: u8,
    /// Current address accumulator within the active transaction.
    sav_addr: u32,
    /// Address bytes received so far in the current transaction.
    sav_addr_bytes: u32,
    /// Overall byte index within the current transaction.
    sav_byte_pos: u32,
    /// WEL (write-enable latch).
    sav_write_enabled: bool,
    /// Address width for this cart's save chip (1/2/3), from the game-code table.
    sav_addr_size: u32,
    /// AUXSPICNT bit 6 (CS-hold) was set on the last write.
    aux_hold: bool,
    /// Deferred CS release: end the transaction after the NEXT data byte.
    aux_release_after_next: bool,
    /// AUXSPICNT bit 13 — SPI writes route to the save chip.
    aux_to_backup: bool,
    /// Last byte the save chip shifted out (read back via AUXSPIDATA).
    aux_out: u8,
}

impl Default for Cart {
    fn default() -> Self {
        Self::new()
    }
}

impl Cart {
    /// A cart with no ROM mounted. `Nds::new` builds this; `mount` installs a
    /// ROM image later. Reachable from `Nds::new()`, so the body is real.
    pub fn new() -> Self {
        Cart {
            rom: Vec::new(),
            cmd: [0; 8],
            buf: Vec::new(),
            pos: 0,
            romctrl: 0,
            auxspicnt: 0,
            phase: Phase::Raw,
            key1_cmd_count: 0,
            sav: vec![0xFF; 0x10_0000],
            sav_dirty: false,
            sav_cmd: 0,
            sav_status_user_bits: 0,
            sav_addr: 0,
            sav_addr_bytes: 0,
            sav_byte_pos: 0,
            sav_write_enabled: false,
            sav_addr_size: 3,
            aux_hold: false,
            aux_release_after_next: false,
            aux_to_backup: false,
            aux_out: 0xFF,
        }
    }

    /// Install a ROM image and reset the command/transfer/save protocol state.
    /// Looks up the per-game save address-size override off the 4-char game code
    /// at header offset 0x0C. (TS `loadRom`.)
    pub fn mount(&mut self, rom: Vec<u8>) {
        // Look up the per-game save address-size override from the 4-char game
        // code at header offset 0x0C BEFORE moving `rom` into the cart. Unknown
        // codes default to 3-byte FLASH addressing.
        let game_code: String = rom
            .get(0x0C..0x10)
            .map(|s| s.iter().map(|&b| b as char).collect())
            .unwrap_or_default();
        self.sav_addr_size = sav_addr_size_for_game_code(&game_code);

        self.rom = rom;
        self.cmd = [0; 8];
        self.buf = Vec::new();
        self.pos = 0;
        self.romctrl = 0;
        self.phase = Phase::Raw;
        self.key1_cmd_count = 0;
        // Reset save transaction state (but keep the `sav` blob — the host
        // controls reload via `load_sav`).
        self.sav_cmd = 0;
        self.sav_addr = 0;
        self.sav_addr_bytes = 0;
        self.sav_byte_pos = 0;
        self.sav_write_enabled = false;
        self.aux_hold = false;
        self.aux_release_after_next = false;
        self.aux_to_backup = false;
        self.aux_out = 0xFF;
    }

    /// Replace the save blob (host save-file load). (TS `loadSav`.)
    pub fn load_sav(&mut self, data: &[u8]) {
        let len = data.len().max(0x10_0000);
        self.sav = vec![0xFF; len];
        self.sav[..data.len()].copy_from_slice(data);
        self.sav_dirty = false;
    }

    /// Read-only view of the save blob (host persistence).
    pub fn sav(&self) -> &[u8] {
        &self.sav
    }

    // ─── ROMCMD latch (0x040001A8..AF) ──────────────────────────────────

    /// Write one command latch byte (`off` masked to 0..7).
    pub fn write_cmd_byte(&mut self, off: u32, v: u8) {
        self.cmd[(off & 7) as usize] = v;
    }
    /// Read one command latch byte.
    pub fn read_cmd_byte(&self, off: u32) -> u8 {
        self.cmd[(off & 7) as usize]
    }

    // ─── AUXSPICNT (0x040001A0) / AUXSPIDATA (0x040001A2) ───────────────

    /// Write AUXSPICNT — tracks CS-hold edges + backup-select bit. (TS
    /// `writeAuxSpiCnt`.)
    pub fn write_auxspicnt(&mut self, v: u32) {
        let new_hold = ((v >> 6) & 1) != 0;
        let new_select_backup = ((v >> 13) & 1) != 0;
        // CS-hold falling 1 → 0 mid-transaction: defer end-of-transaction
        // until after the NEXT data byte (Pokemon's RDSR depends on this). If
        // we end immediately, the next DAT_W is treated as a new command and
        // the SDK reads 0xFF instead of the status byte.
        if self.aux_hold && !new_hold && self.sav_byte_pos > 0 {
            self.aux_release_after_next = true;
        }
        // CS rising 0 → 1 also starts a new transaction. PAGE PROGRAM (0x02)
        // and WRSR (0x01) auto-clear WEL when the chip "executes" the command
        // on CS rising-edge.
        if !self.aux_hold && new_hold {
            if self.sav_cmd == 0x02 || self.sav_cmd == 0x0A || self.sav_cmd == 0x01 {
                self.sav_write_enabled = false;
            }
            self.sav_cmd = 0;
            self.sav_addr_bytes = 0;
            self.sav_byte_pos = 0;
            self.aux_release_after_next = false;
        }
        self.aux_hold = new_hold;
        self.aux_to_backup = new_select_backup;
        self.auxspicnt = v & 0xFFFF;
    }
    /// Read AUXSPICNT (low 16 bits).
    pub fn read_auxspicnt(&self) -> u32 {
        self.auxspicnt & 0xFFFF
    }
    /// Read the last byte the save chip shifted out.
    pub fn read_auxspidata(&self) -> u32 {
        self.aux_out as u32
    }
    /// Exchange a byte with the save chip (only when backup-select is on).
    /// (TS `writeAuxSpiData`.)
    pub fn write_auxspidata(&mut self, v: u32) {
        if !self.aux_to_backup {
            // ROM-side AUXSPI access — not modeled separately; just clear.
            self.aux_out = 0xFF;
            return;
        }
        self.aux_out = self.sav_tick_byte((v & 0xFF) as u8);
        // If the SDK released CS-hold before this byte, the byte just exchanged
        // was the final one — end the transaction now. This is the framing the
        // NitroSDK actually uses (hold-high on intermediate bytes, hold-low on
        // the last), so the program/erase trigger fires HERE, not on a CS
        // rising edge. PAGE PROGRAM (0x02/0x0A) and WRSR (0x01) auto-clear WEL
        // when the chip executes the command on CS deassert; without this the
        // SDK polls RDSR, sees WEL still latched, and reports the write failed
        // ("the data could not be saved" — e.g. Need for Speed FLASH saves).
        if self.aux_release_after_next {
            if self.sav_cmd == 0x02 || self.sav_cmd == 0x0A || self.sav_cmd == 0x01 {
                self.sav_write_enabled = false;
            }
            self.sav_cmd = 0;
            self.sav_addr_bytes = 0;
            self.sav_byte_pos = 0;
            self.aux_release_after_next = false;
        }
    }

    // ─── ROMCTRL (0x040001A4) ───────────────────────────────────────────

    /// Read ROMCTRL — folds in the live word-ready (bit 23) and busy (bit 31)
    /// state from the current transfer buffer. (TS `readRomCtrl`.)
    pub fn read_romctrl(&self) -> u32 {
        // Bit 23 = "word ready" — set when the buffer has data left to read.
        // Bit 31 = busy — likewise tied to data remaining.
        let mut ctrl = self.romctrl & 0x7F7F_FFFF;
        if self.pos < self.buf.len() {
            ctrl |= 0x0080_0000; // word-ready
            ctrl |= 0x8000_0000; // still busy
        }
        ctrl
    }
    /// Write ROMCTRL — bit 31 (block-start) kicks off a transfer. Returns the
    /// event the IO dispatch should act on (Ready when a buffer was filled).
    /// (TS `writeRomCtrl` + the `onTransferReady` callback, inlined.)
    pub fn write_romctrl(&mut self, v: u32) -> TransferEvent {
        let start_bit = (v >> 31) & 1;
        self.romctrl = v;
        if start_bit != 0 {
            self.start_transfer();
            // The TS fired `onTransferReady` unconditionally inside
            // `startTransfer`; we surface it as the return event so the IO
            // dispatch can run the cart-ready DMA timing.
            return TransferEvent::Ready;
        }
        TransferEvent::None
    }

    // ─── ROMDATA FIFO (0x04100010) ──────────────────────────────────────

    /// Pop the next 32-bit word from the transfer buffer (0xFFFFFFFF past the
    /// end). The second return value is the event for the IO dispatch
    /// (`TransferEnd` on the last word when AUXSPICNT bit 14 is set). (TS
    /// `readRomData` + the `onTransferEnd` callback, inlined.)
    pub fn read_romdata(&mut self) -> (u32, TransferEvent) {
        if self.pos + 4 > self.buf.len() {
            // No more data — return 0xFFFFFFFF per real HW after the last word.
            return (0xFFFF_FFFF, TransferEvent::None);
        }
        let v = (self.buf[self.pos] as u32)
            | ((self.buf[self.pos + 1] as u32) << 8)
            | ((self.buf[self.pos + 2] as u32) << 16)
            | ((self.buf[self.pos + 3] as u32) << 24);
        self.pos += 4;
        // Last word → fire transfer-end IRQ if AUXSPICNT bit 14 is set.
        let event = if self.pos >= self.buf.len() && ((self.auxspicnt >> 14) & 1) != 0 {
            TransferEvent::TransferEnd
        } else {
            TransferEvent::None
        };
        (v, event)
    }

    // ─── Transfer engine + save chip (private) ──────────────────────────

    /// Build the transfer buffer for the latched command + ROMCTRL block size.
    fn start_transfer(&mut self) {
        // Block size — bits 24..26 of ROMCTRL.
        let bs = ((self.romctrl >> 24) & 7) as usize;
        let block_size = BLOCK_SIZE_TABLE[bs];
        let cmd0 = self.cmd[0];

        if block_size == 0 {
            self.buf = Vec::new();
            self.pos = 0;
            return;
        }

        match self.phase {
            Phase::Raw => self.run_raw_command(cmd0, block_size),
            Phase::Key1 => self.run_key1_command(block_size),
            // KEY2 cipher is transparent for our stub. The SDK driver expects
            // cmd 0xB7 (read) and 0xB8 (chip ID); the RAW handlers do the
            // right thing.
            Phase::Key2 => self.run_raw_command(cmd0, block_size),
        }
    }

    /// Service a Raw/Key2 command (0x9F/0x00/0x90/0xB8/0x3C/0xB7).
    fn run_raw_command(&mut self, cmd0: u8, block_size: u32) {
        let block_size = block_size as usize;
        self.pos = 0;
        match cmd0 {
            0x9F => {
                // Dummy — all 0xFF.
                self.buf = vec![0xFF; block_size];
            }
            0x00 => {
                // Read header — repeat the first 0x200 bytes.
                self.buf = vec![0; block_size];
                for i in 0..block_size {
                    self.buf[i] = self.rom.get(i % 0x200).copied().unwrap_or(0);
                }
            }
            0x90 | 0xB8 => {
                // Chip ID — 4-byte value repeated across the block.
                let id = synth_chip_id(self.rom.len());
                self.fill_chip_id(block_size, id);
            }
            0x3C => {
                // Activate KEY1 mode. No data exchanged; cart starts replying
                // to encrypted commands afterwards. SDK driver expects a
                // "success" sentinel of 0x00.
                self.phase = Phase::Key1;
                self.key1_cmd_count = 0;
                self.buf = vec![0; block_size];
            }
            0xB7 => {
                // Read addressed data. Address is big-endian in cmd[1..4].
                let mut addr = ((self.cmd[1] as u32) << 24)
                    | ((self.cmd[2] as u32) << 16)
                    | ((self.cmd[3] as u32) << 8)
                    | (self.cmd[4] as u32);
                // The first 0x8000 bytes are the header+secure area, which
                // cmd 0xB7 maps into the 0x8000-aligned window per GBATEK.
                if addr < 0x8000 {
                    addr = 0x8000 + (addr & 0x1FF);
                }
                self.buf = vec![0; block_size];
                for i in 0..block_size {
                    let src = addr.wrapping_add(i as u32) as usize;
                    self.buf[i] = self.rom.get(src).copied().unwrap_or(0xFF);
                }
            }
            _ => {
                self.buf = vec![0xFF; block_size];
            }
        }
    }

    /// Service a Key1 command by call order (chip-ID → secure area → →Key2).
    ///
    /// The cart's real KEY1 decryption produces a 56-bit command whose top
    /// nibble is the actual opcode; our stub ignores the opcode and walks by
    /// call order. The SDK driver after 0x3C sends: 1× chip-ID, several
    /// secure-area block reads, then an activate-KEY2 command.
    fn run_key1_command(&mut self, block_size: u32) {
        let block_size = block_size as usize;
        self.key1_cmd_count += 1;
        let n = self.key1_cmd_count;
        self.pos = 0;

        // After ~6 KEY1 commands, transition to KEY2.
        if n >= 6 {
            self.phase = Phase::Key2;
            // Cart returns 0x00 for the activate-KEY2 ack.
            self.buf = vec![0; block_size];
            return;
        }
        if n == 1 {
            let id = synth_chip_id(self.rom.len());
            self.fill_chip_id(block_size, id);
            return;
        }
        // Secure area read. Block N (1-indexed; #1 was chip ID) maps to ROM
        // offset 0x4000 + (n-2)*0x200.
        let addr = 0x4000 + (n - 2) * 0x200;
        self.buf = vec![0; block_size];
        for i in 0..block_size {
            let src = (addr as usize).wrapping_add(i);
            self.buf[i] = self.rom.get(src).copied().unwrap_or(0xFF);
        }
    }

    /// Fill `self.buf` with the 4-byte chip ID repeated across `block_size`.
    fn fill_chip_id(&mut self, block_size: usize, id: u32) {
        self.buf = vec![0; block_size];
        let id = id.to_le_bytes();
        let mut i = 0;
        while i + 4 <= block_size {
            self.buf[i..i + 4].copy_from_slice(&id);
            i += 4;
        }
        // Tail (block_size not a multiple of 4) — fill remaining bytes.
        for (j, b) in self.buf[i..].iter_mut().enumerate() {
            *b = id[j];
        }
    }

    /// One SPI byte exchange with the save chip; returns the chip's shift-out.
    /// State is per-transaction (reset by CS release in `write_auxspicnt`).
    fn sav_tick_byte(&mut self, byte: u8) -> u8 {
        let pos = self.sav_byte_pos;
        self.sav_byte_pos += 1;
        if pos == 0 {
            self.sav_cmd = byte;
            self.sav_addr = 0;
            self.sav_addr_bytes = 0;
            // Single-byte commands apply their effect on the command byte.
            // The chip's response to byte 0 of any exchange is the previous
            // shift-out (= 0xFF for stale).
            match byte {
                0x06 => self.sav_write_enabled = true,  // WREN
                0x04 => self.sav_write_enabled = false, // WRDI
                _ => {}
            }
            return 0xFF;
        }
        match self.sav_cmd {
            // RDSR — bit0 WIP (always 0), bit1 WEL, bits2..7 user bits.
            0x05 => {
                (self.sav_status_user_bits & 0xFC) | if self.sav_write_enabled { 0x02 } else { 0x00 }
            }
            0x06 => {
                self.sav_write_enabled = true;
                0xFF
            }
            0x04 => {
                self.sav_write_enabled = false;
                0xFF
            }
            // WRSR — bits 2..7 user-writable; WEL cleared after success.
            0x01 => {
                if self.sav_write_enabled {
                    self.sav_status_user_bits = byte & 0xFC;
                    self.sav_write_enabled = false;
                }
                0xFF
            }
            // READ / READ_HI.
            0x03 | 0x0B => {
                if self.sav_addr_bytes < self.sav_addr_size {
                    self.sav_addr = (self.sav_addr << 8) | byte as u32;
                    self.sav_addr_bytes += 1;
                    if self.sav_addr_bytes < self.sav_addr_size {
                        return 0xFF;
                    }
                    // Final address byte — apply high-variant offset for the
                    // EEPROM 512 B encoding.
                    if self.sav_cmd == 0x0B && self.sav_addr_size == 1 {
                        self.sav_addr += 0x100;
                    }
                    return 0xFF;
                }
                let a = (self.sav_addr & (self.sav.len() as u32 - 1)) as usize;
                self.sav_addr = self.sav_addr.wrapping_add(1);
                self.sav.get(a).copied().unwrap_or(0xFF)
            }
            // WRITE / WRITE_HI.
            0x02 | 0x0A => {
                if self.sav_addr_bytes < self.sav_addr_size {
                    self.sav_addr = (self.sav_addr << 8) | byte as u32;
                    self.sav_addr_bytes += 1;
                    if self.sav_addr_bytes < self.sav_addr_size {
                        return 0xFF;
                    }
                    if self.sav_cmd == 0x0A && self.sav_addr_size == 1 {
                        self.sav_addr += 0x100;
                    }
                    return 0xFF;
                }
                if !self.sav_write_enabled {
                    return 0xFF;
                }
                let a = (self.sav_addr & (self.sav.len() as u32 - 1)) as usize;
                self.sav_addr = self.sav_addr.wrapping_add(1);
                self.sav[a] = byte;
                self.sav_dirty = true;
                0xFF
            }
            // RDID (JEDEC ID) — three-byte ID for a Macronix-like 1 MB FLASH.
            0x9F => match pos {
                1 => 0xC2,
                2 => 0x20,
                3 => 0x14,
                _ => 0xFF,
            },
            _ => 0xFF,
        }
    }
}

/// Per-game save address-size override (game-code → addr bytes). DS games
/// hardcode the chip type in the SDK driver — there's no JEDEC probe — so the
/// default is 3-byte FLASH and only the listed codes use a shorter protocol.
/// (TS `SAV_ADDR_SIZE_BY_GAME_CODE`.)
pub(crate) fn sav_addr_size_for_game_code(code: &str) -> u32 {
    match code {
        "YSZE" => 1, // The Simpsons Game (USA) — 1-byte EEPROM 0.5K.
        "CEPE" => 2, // Age of Empires: Mythologies (USA) — 2-byte EEPROM.
        "B8IE" => 1, // Spider-Man: Edge of Time (USA) — EEPROM 0.5K.
        _ => 3,      // FLASH 256K..8M — the common modern-DS default.
    }
}

/// Synthesize a Macronix-style chip ID encoding the ROM size, matching what the
/// loader stamps into the BIOS-RAM chip-ID fields. (TS `synthChipId`.)
pub(crate) fn synth_chip_id(rom_size: usize) -> u32 {
    let mb = rom_size / (1024 * 1024);
    let size_byte: u32 = if mb >= 128 {
        0xFF
    } else if mb >= 64 {
        0xFD
    } else if mb >= 32 {
        0xFB
    } else if mb >= 16 {
        0xF7
    } else if mb >= 8 {
        0xEF
    } else if mb >= 4 {
        0xDF
    } else {
        0xBF
    };
    let hi: u32 = if mb >= 128 { 0x80 } else { 0x00 };
    (hi << 24) | (size_byte << 8) | 0xC2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rom() -> Vec<u8> {
        // 256 KB fake ROM with header + secure-area pre-filled.
        let mut r = vec![0u8; 256 * 1024];
        for i in 0..0x200 {
            r[i] = ((i + 1) & 0xFF) as u8;
        }
        for i in 0x4000..0x8000 {
            r[i] = ((i & 0xFF) ^ 0xAA) as u8;
        }
        r
    }

    fn load_and_send(cart: &mut Cart, cmd0: u8, block_size: usize) -> Vec<u8> {
        for i in 0..8 {
            cart.write_cmd_byte(i, 0);
        }
        cart.write_cmd_byte(0, cmd0);
        // ROMCTRL: block-size index 1 (= 0x200), start bit 31.
        cart.write_romctrl(0x0100_0000 | (1 << 31));
        let mut out = vec![0u8; block_size];
        let mut i = 0;
        while i < block_size {
            let (v, _) = cart.read_romdata();
            out[i] = (v & 0xFF) as u8;
            out[i + 1] = ((v >> 8) & 0xFF) as u8;
            out[i + 2] = ((v >> 16) & 0xFF) as u8;
            out[i + 3] = ((v >> 24) & 0xFF) as u8;
            i += 4;
        }
        out
    }

    fn fresh() -> Cart {
        let mut c = Cart::new();
        c.mount(make_rom());
        c
    }

    #[test]
    fn raw_dummy_9f_returns_all_ff() {
        let mut c = fresh();
        let out = load_and_send(&mut c, 0x9F, 0x200);
        assert!(out.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn cmd_00_streams_header() {
        let mut c = fresh();
        let out = load_and_send(&mut c, 0x00, 0x200);
        assert_eq!(out[0], 1);
        assert_eq!(out[1], 2);
        assert_eq!(out[0x10], 0x11);
    }

    #[test]
    fn cmd_90_returns_chip_id_repeating() {
        let mut c = fresh();
        let out = load_and_send(&mut c, 0x90, 0x200);
        assert_eq!(out[0], 0xC2);
        assert_eq!(out[4], 0xC2);
    }

    #[test]
    fn cmd_3c_transitions_to_key1() {
        let mut c = fresh();
        load_and_send(&mut c, 0x3C, 0x200);
        assert_eq!(c.phase, Phase::Key1);
    }

    #[test]
    fn first_key1_cmd_returns_chip_id() {
        let mut c = fresh();
        load_and_send(&mut c, 0x3C, 0x200);
        let out = load_and_send(&mut c, 0x00, 0x200);
        assert_eq!(out[0], 0xC2);
    }

    #[test]
    fn key1_secure_area_streams_rom_4000() {
        let mut c = fresh();
        load_and_send(&mut c, 0x3C, 0x200);
        load_and_send(&mut c, 0x00, 0x200); // chip ID
        let out = load_and_send(&mut c, 0x00, 0x200); // secure area block 0
        assert_eq!(out[0], 0xAA); // (0x4000 & 0xFF) ^ 0xAA = 0xAA
        assert_eq!(out[1], 0x01 ^ 0xAA);
    }

    #[test]
    fn six_key1_cmds_reach_key2() {
        let mut c = fresh();
        load_and_send(&mut c, 0x3C, 0x200);
        for _ in 0..6 {
            load_and_send(&mut c, 0x00, 0x200);
        }
        assert_eq!(c.phase, Phase::Key2);
    }

    #[test]
    fn key2_b7_streams_addressed_data() {
        let mut c = fresh();
        load_and_send(&mut c, 0x3C, 0x200);
        for _ in 0..6 {
            load_and_send(&mut c, 0x00, 0x200);
        }
        for i in 0..8 {
            c.write_cmd_byte(i, 0);
        }
        c.write_cmd_byte(0, 0xB7);
        // address 0x10000 (big-endian in cmd[1..4]).
        c.write_cmd_byte(1, 0x00);
        c.write_cmd_byte(2, 0x01);
        c.write_cmd_byte(3, 0x00);
        c.write_cmd_byte(4, 0x00);
        c.write_romctrl(0x0100_0000 | (1 << 31));
        let (v, _) = c.read_romdata();
        assert_eq!(v, 0); // ROM byte 0x10000 unset → 0.
    }

    #[test]
    fn transfer_ready_event_on_start() {
        let mut c = fresh();
        let ev = c.write_romctrl(0x0100_0000 | (1 << 31));
        assert_eq!(ev, TransferEvent::Ready);
        // Without the start bit, nothing happens.
        let ev = c.write_romctrl(0x0100_0000);
        assert_eq!(ev, TransferEvent::None);
    }

    #[test]
    fn transfer_end_event_only_with_bit14() {
        let mut c = fresh();
        // Without bit 14 — drain a 0x9F block, never see TransferEnd.
        c.write_auxspicnt(0);
        c.write_cmd_byte(0, 0x9F);
        c.write_romctrl(0x0100_0000 | (1 << 31));
        let mut saw_end = false;
        loop {
            let (_, ev) = c.read_romdata();
            if c.read_romctrl() & 0x8000_0000 == 0 {
                break;
            }
            if ev == TransferEvent::TransferEnd {
                saw_end = true;
            }
        }
        // Drain one more to hit the end.
        let (_, ev) = c.read_romdata();
        if ev == TransferEvent::TransferEnd {
            saw_end = true;
        }
        assert!(!saw_end);

        // With bit 14 set — the last word reports TransferEnd.
        c.write_auxspicnt(0x4000);
        c.write_cmd_byte(0, 0x9F);
        c.write_romctrl(0x0100_0000 | (1 << 31));
        let mut saw_end = false;
        for _ in 0..(0x200 / 4) {
            let (_, ev) = c.read_romdata();
            if ev == TransferEvent::TransferEnd {
                saw_end = true;
            }
        }
        assert!(saw_end);
    }

    #[test]
    fn romctrl_busy_clears_when_drained() {
        let mut c = fresh();
        c.write_cmd_byte(0, 0x9F);
        c.write_romctrl(0x0100_0000 | (1 << 31));
        assert_ne!(c.read_romctrl() & 0x8000_0000, 0); // busy
        for _ in 0..(0x200 / 4) {
            c.read_romdata();
        }
        assert_eq!(c.read_romctrl() & 0x8000_0000, 0); // idle
        // Past the end → 0xFFFFFFFF.
        assert_eq!(c.read_romdata().0, 0xFFFF_FFFF);
    }

    /// Start a fresh save-chip transaction: a CS rising edge (bit6 0→1) with
    /// the backup-select bit (13) latched resets the per-transaction state.
    fn begin_tx(c: &mut Cart) {
        c.write_auxspicnt(1 << 13); // CS low, backup selected
        c.write_auxspicnt((1 << 13) | (1 << 6)); // CS rising edge → new tx
    }

    /// Drive one full save-chip transaction the way the NitroSDK / real
    /// hardware does it: AUXSPICNT bit 6 (CS-hold) is SET for every byte
    /// except the last, and CLEAR for the final byte (which deasserts CS and
    /// ends the transaction). Backup-select (bit 13) stays latched throughout.
    /// Returns the chip's shift-out for each byte. This is the framing the SDK
    /// actually uses — `begin_tx` artificially forces a rising edge instead.
    fn transact_sav(c: &mut Cart, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let hold = if last { 0 } else { 1 << 6 };
            c.write_auxspicnt((1 << 13) | hold);
            c.write_auxspidata(b as u32);
            out.push(c.read_auxspidata() as u8);
        }
        out
    }

    /// Reproduces the "data could not be saved" bug: the NitroSDK FLASH save
    /// path frames each SPI command as its own transaction (hold-high on the
    /// intermediate bytes, hold-low on the last). WREN is a single hold-low
    /// byte. The PROGRAM that follows must be seen as a fresh command — if the
    /// previous transaction's byte counter is never cleared on CS deassert, the
    /// PROGRAM's command byte is mis-parsed and no data ever lands in the blob.
    #[test]
    fn save_flash_program_sequence_hardware_framing() {
        let mut c = fresh();

        // WREN — single byte, CS-hold low (one-byte transaction).
        transact_sav(&mut c, &[0x06]);

        // RDSR must report WEL set after WREN.
        let resp = transact_sav(&mut c, &[0x05, 0x00]);
        assert_eq!(resp[1] & 0x02, 0x02, "WEL must be set after WREN");

        // PROGRAM (0x02) + 3-byte address 0x000010 + two data bytes.
        transact_sav(&mut c, &[0x02, 0x00, 0x00, 0x10, 0xAB, 0xCD]);
        assert!(c.sav_dirty, "PROGRAM must flip save_dirty");
        assert_eq!(c.sav()[0x10], 0xAB, "PROGRAM must store byte 0");
        assert_eq!(c.sav()[0x11], 0xCD, "PROGRAM must store byte 1");

        // The completed PROGRAM must auto-clear WEL, exactly like real FLASH:
        // the chip latches CS-deassert as the program trigger and resets WEL.
        // The NitroSDK polls RDSR after the program and treats a still-set WEL
        // (with WIP clear) as a failed write → "the data could not be saved".
        let resp = transact_sav(&mut c, &[0x05, 0x00]);
        assert_eq!(resp[1] & 0x02, 0x00, "PROGRAM must auto-clear WEL on CS deassert");

        // READ back to confirm the bytes are visible via the save blob.
        let resp = transact_sav(&mut c, &[0x03, 0x00, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(resp[4], 0xAB);
        assert_eq!(resp[5], 0xCD);
    }

    #[test]
    fn save_chip_wren_rdsr_write_read_roundtrip() {
        let mut c = fresh();
        // WREN then RDSR → WEL set (bit 1).
        begin_tx(&mut c);
        c.write_auxspidata(0x06); // WREN
        begin_tx(&mut c);
        c.write_auxspidata(0x05); // RDSR
        c.write_auxspidata(0x00);
        assert_eq!(c.read_auxspidata() & 0x02, 0x02);

        // WRITE: re-enable, then 0x02 + 3-byte addr 0 + data 0x42.
        begin_tx(&mut c);
        c.write_auxspidata(0x06); // WREN
        begin_tx(&mut c);
        c.write_auxspidata(0x02); // WRITE
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x42);
        assert!(c.sav_dirty);

        // READ back at addr 0.
        begin_tx(&mut c);
        c.write_auxspidata(0x03); // READ
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x00);
        c.write_auxspidata(0x00); // clock out the data byte
        assert_eq!(c.read_auxspidata(), 0x42);
    }

    #[test]
    fn save_addr_size_table() {
        assert_eq!(sav_addr_size_for_game_code("YSZE"), 1);
        assert_eq!(sav_addr_size_for_game_code("CEPE"), 2);
        assert_eq!(sav_addr_size_for_game_code("B8IE"), 1);
        assert_eq!(sav_addr_size_for_game_code("CPUE"), 3);
    }

    #[test]
    fn synth_chip_id_encodes_size() {
        // 256 KB ROM → < 4 MB → 0xBF size byte, Macronix 0xC2 low.
        let id = synth_chip_id(256 * 1024);
        assert_eq!(id & 0xFF, 0xC2);
        assert_eq!((id >> 8) & 0xFF, 0xBF);
        // 8 MB.
        assert_eq!((synth_chip_id(8 * 1024 * 1024) >> 8) & 0xFF, 0xEF);
    }

    #[test]
    fn load_sav_replaces_blob() {
        let mut c = fresh();
        c.load_sav(&[1, 2, 3, 4]);
        assert_eq!(c.sav()[0], 1);
        assert_eq!(c.sav()[3], 4);
        assert_eq!(c.sav()[4], 0xFF);
        assert!(c.sav().len() >= 0x10_0000);
    }
}
