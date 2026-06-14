# `gbc-core` — Game Boy Color core: foundation contract

A from-scratch Rust CGB (Sharp LR35902) emulator core, the fourth in this repo.
Written against the hardware spec — **Pan Docs** (gbdev.io/pandocs) — not ported
from any source. Mirrors the GBA/NDS/PS1 cores' ownership model.

This phase delivers the **foundation**: the CPU register/interrupt state, the
memory map, the cartridge MBC routing, and the bus. Instruction execution and
the IO sub-devices (PPU/APU/timer/DMA/joypad/serial) are **stubs** the per-file
porting agents fill in. Code against the interfaces below; do not redefine them.

## Ownership model (read twice)

One [`Gbc`] god-struct (`emulator.rs`) owns **one instance of every subsystem**
plus `Memory`, `Cart`, and `Irq`, and implements [`bus::Bus`]. There are **no
`Rc`/`RefCell`** cycles. A subsystem struct owns **only its own state**.

A method that needs the bus takes `bus: &mut dyn crate::bus::Bus` as a
**parameter** (never a stored field). When a method owned by `Gbc` itself needs
`&mut dyn Bus` (= `&mut Gbc`), reach it by `std::mem::take`-ing that device out
of `self`, calling with `self` as the bus, then putting it back — exactly as the
GBA core's DMA/PPU triggers do.

Conventions: closed enums matched **exhaustively** (no catch-all on the
hardware-defined sets); fixed-width ints (`u8` registers/data, `u16`
addresses/pairs); little-endian multi-byte memory; boxed regions; `snake_case`.

## Foundation API (already written — code against it)

### `crate::regions`
Memory-map sizes + region boundaries + the IO register addresses the foundation
models directly: `ROM_BANK_SIZE`, `VRAM_BANK_SIZE`/`VRAM_BANKS`,
`WRAM_BANK_SIZE`/`WRAM_BANKS`, `ERAM_BANK_SIZE`, `OAM_SIZE`, `HRAM_SIZE`,
`CRAM_SIZE`; `*_START`/`*_END` bounds; `IE_REGISTER`; and `REG_IF`, `REG_KEY1`,
`REG_VBK`, `REG_SVBK`, `REG_HDMA1..=REG_HDMA5`, `REG_BCPS/BCPD/OCPS/OCPD`.

### `crate::bus::Bus` (trait)
The byte-granular CPU memory interface (LR35902 only does 8-bit accesses):

```rust
fn read8(&mut self, addr: u16) -> u8;
fn write8(&mut self, addr: u16, v: u8);
fn read16(&mut self, addr: u16) -> u16;   // default: two read8, low byte first
fn write16(&mut self, addr: u16, v: u16); // default: two write8, low byte first
```

`Gbc` is the production implementor; its routing (Pan Docs Memory Map):
`0x0000-0x7FFF` cart ROM/MBC · `0x8000-0x9FFF` VRAM (VBK) · `0xA000-0xBFFF`
external RAM (MBC) · `0xC000-0xDFFF` WRAM (SVBK high half) · `0xE000-0xFDFF` echo
of `0xC000-0xDDFF` · `0xFE00-0xFE9F` OAM · `0xFEA0-0xFEFF` unusable (read `0xFF`,
write ignored) · `0xFF00-0xFF7F` IO · `0xFF80-0xFFFE` HRAM · `0xFFFF` IE.

### `crate::interrupts::{Interrupt, Irq}`
`Interrupt` is the closed enum `{ VBlank, Stat, Timer, Serial, Joypad }` with
`bit()`/`mask()`/`vector()` (vectors `0x40,0x48,0x50,0x58,0x60`) and `ALL`
(priority order). `Irq` owns IE (`0xFFFF`) + IF (`0xFF0F`, upper 3 bits read 1):
`request`, `acknowledge`, `read_ie`/`write_ie`, `read_if`/`write_if`,
`pending()` (IE&IF&0x1F, ignores IME — used to wake HALT), `highest_priority()`.

### `crate::cpu::Cpu` (= `cpu::state::Cpu`)
Register file `a,f,b,c,d,e,h,l: u8`, `sp,pc: u16`; pair accessors
`af/bc/de/hl` + `set_*` (`set_af` masks F's low nibble to 0); flag helpers
`flag(mask)`/`set_flag(mask,on)` with `FLAG_Z/N/H/C` (bits 7/6/5/4). Interrupt
state `ime`, `ime_pending` (EI's one-instruction delay), `power: Power`
(`Running/Halted/Stopped`), `halt_bug`. `Cpu::new()` is the post-boot **CGB**
register state (A=0x11, PC=0x0100, SP=0xFFFE).

Interrupt dispatch helper — the standard service sequence:
```rust
fn service_interrupt(&mut self, bus: &mut dyn Bus, irq: &mut Irq) -> Option<Interrupt>;
// IME gate → highest_priority → clear IME, acknowledge, push PC (hi then lo),
// jump to vector, wake from HALT. Returns the serviced interrupt.
```

### `crate::memory::Memory`
Internal RAM + CGB bank/palette state. Boxed `vram` (2 banks, `vram_bank`),
`wram` (8 banks, `wram_bank`), `oam`, `hram`, `bg_palette`/`obj_palette`
(`CRAM_SIZE` each) with `bcps`/`ocps` index ports, and reserved `key1`.
Accessors: `read_vram`/`write_vram`, `read_wram`/`write_wram` (low window =
bank 0; high window = SVBK 1-7, 0→1), `read_oam`/`write_oam`,
`read_hram`/`write_hram`, `read_vbk`/`write_vbk` (bit 0 only, unused→1),
`read_svbk`/`write_svbk` (bits 2-0, 0→1, unused→1), and the auto-incrementing
`read/write_bg_palette_data` + `read/write_obj_palette_data`.

### `crate::cart::Cart` (+ `cart::header::CartHeader`, `cart::mbc::{Mbc, MbcKind}`)
Owns `rom: Vec<u8>`, `ram: Vec<u8>`, the live `mbc`, the parsed `header`,
`has_battery`, `ram_dirty`. `load_rom(bytes)` parses the header (0x0147 type →
`MbcKind`, sizes from 0x0148/0x0149, CGB flag 0x0143), pads ROM, sizes RAM
(MBC2 = built-in 512 nibbles). Bus-facing: `read_rom`/`write_rom` (writes are
MBC control registers), `read_ram`/`write_ram` (MBC-gated, sets `ram_dirty`),
`save_ram`/`load_save_ram`.

`MbcKind` (closed): `NoMbc`, `Mbc1`, `Mbc2`, `Mbc3 { rtc }`, `Mbc5 { rumble }` —
decoded by `from_cart_type(byte)`; `has_battery(byte)`. `Mbc` holds the
bank-select registers and translates addresses: `rom_offset(addr) -> usize`,
`ram_offset(addr) -> Option<usize>` (None = blocked/RTC), and routes control
writes via `write_control(addr, value)` (per-controller `write_mbc1..5`).

### `crate::emulator::Gbc`
Fields: `cpu, mem, cart, irq, ppu, apu, timer, dma, joypad, serial, io_raw`.
`new()`, `load_rom(bytes)`, save passthrough (`save_ram`/`load_save_ram`/
`save_dirty`/`clear_save_dirty`), `request_interrupt(int)`. `step()` and the
device IO arms (`io_read`/`io_write` for joypad/serial/timer/APU/PPU/OAM-DMA/
HDMA) are `todo!()` **seams** — the routing exists; the device behavior is the
porting agents' job.

## What each porting agent owns (one file each)
`cpu/exec.rs` (decode/execute + `Gbc::step`), `ppu.rs`, `apu.rs`, `timer.rs`,
`dma.rs` (OAM DMA `0xFF46` + CGB HDMA/GDMA `0xFF51-0xFF55`), `joypad.rs`
(`0xFF00`), `serial.rs` (`0xFF01/02`). Each currently holds a one-line TODO +
empty `#[derive(Default)]` placeholder struct so the god-struct compiles. Wire
your device into `Gbc::io_read`/`io_write` (replace the matching `todo!()`).

## Build / self-check
`cargo check --manifest-path core-gbc/Cargo.toml` passes; `cargo test` runs 20
foundation unit tests (interrupt priority, F-nibble masking, WRAM/VRAM banking,
echo aliasing, MBC1 bank switching, RAM enable gating).

## Spec sources
Pan Docs: Memory Map, The Cartridge Header, MBCs, CPU Registers and Flags,
Interrupts, CGB Registers (VBK/SVBK/KEY1/HDMA/BCPS-OCPD), Palettes.
