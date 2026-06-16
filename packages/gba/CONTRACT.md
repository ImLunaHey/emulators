# Rust core port — contract for per-file porting agents

We are porting the TypeScript GBA core in `../src` to Rust, **1:1 and
faithfully**. Web (wasm) ships first, React-Native later. The recompiler
(`src/recomp/*`) is **dropped** — we ship a pure interpreter.

Your job: port **one** TS source file into **one** Rust file under
`core/src/`, preserving exact semantics, comments, and edge cases. Do
**not** edit `lib.rs` (the orchestrator wires module declarations to avoid
races). Do **not** touch other agents' files.

## Ownership model (read this twice)

The TS code is a graph of classes holding references to each other
(`Bus`↔`Io`, `Ppu` holds `bus`+`irq`+`dma`, etc.). Rust can't express those
cycles. We resolve it like every production Rust emulator:

- **One `Gba` god-struct** (written at integration, in `emulator.rs`) owns
  **one instance of every subsystem** plus `Mem`. It implements
  [`bus::Bus`] and routes IO/save/RTC.
- **Each former TS class → one Rust struct that owns ONLY its own state.**
- **Collaborators the TS constructor received become `&mut` PARAMETERS** on
  the methods that need them — never stored fields.

  TS:  `class Ppu { constructor(bus, irq, dma) {...} step(n) {...} }`
  Rust: `impl Ppu { fn step(&mut self, n: u32, mem: &mut Mem, irq: &mut Irq, dma: &mut Dma) }`

- If your TS class does `this.bus.read32(a)` / `this.bus.write16(a,v)`,
  take `bus: &mut dyn crate::bus::Bus` as a parameter and call
  `bus.read32(a)`. (Borrow-checker knots at the call site are the
  orchestrator's problem — give a faithful, readable port.)

## Foundation API (already written — code against it, don't redefine)

- `crate::regions` — `REGION_*: u32`, `*_SIZE: usize`.
- `crate::state::{CpuState, mode, FLAG_N, FLAG_Z, FLAG_C, FLAG_V, FLAG_I,
  FLAG_F, FLAG_T}`. `CpuState` has `r: [u32;16]`, `cpsr: u32`, `halted`,
  and methods `mode()`, `in_thumb()`, `check_cond()`, `set_nz()`, `set_c()`,
  `set_v()`, `switch_mode()`, `get_spsr()/set_spsr()`, `enter_exception()`.
- `crate::irq::{Irq, IRQ_*}`. `Irq` has `raise()`, `set_ie()`, `set_ime()`,
  `ack_write16()`, `pending()`, `cached_pending`.
- `crate::bus::{Bus (trait), Mem}`. `Mem` owns the raw memory regions + ROM
  and handles BIOS/EWRAM/IWRAM/PRAM/VRAM/OAM/ROM only.
- `crate::Save` (trait) — implement for Flash / SRAM / EEPROM:
  `fn read(&mut self,a:u32)->u32; fn write(&mut self,a:u32,v:u32);
   fn data(&self)->&[u8]; fn load_save(&mut self,b:&[u8]);`

## Conventions

- Registers/addresses/values are **`u32`**. Use **wrapping** arithmetic
  (`wrapping_add`, `wrapping_sub`, `<<`/`>>` on `u32`) — JS `| 0` / `>>> 0`
  semantics. Signed ops: cast `as i32` exactly where the TS used `| 0`.
- JS `x >>> n` → `x >> n` on `u32`. JS `x | 0` (force int32) → operate on
  `u32` and cast `as i32` only for signed compares/shifts.
- Bit flags / masks: keep the exact hex constants from the TS.
- snake_case method names; struct named like the TS class (`Ppu`, `Dma`,
  `Sound`, `Sio`, `Timers`, `Keypad`, `Flash128K`→`Flash128`, etc.).
- `#[derive(Default)]` or a `new()` matching the TS field initializers.
- Keep the TS explanatory comments (they encode hard-won bug fixes).
- Where it's cheap, add `#[cfg(test)]` unit tests mirroring the matching
  case in `../src/test/*.test.ts`.

## Build / self-check

The whole crate will **not** compile until every module lands — that's
expected. Don't add your `mod` line. Focus on a faithful, type-correct
single file. You may `cargo check` to catch syntax errors in isolation by
temporarily adding `mod yourfile;` to a scratch — but revert it. Leave
`lib.rs` untouched on exit.

Report: the file you wrote, any TS behavior you couldn't express 1:1, and
any method signature the orchestrator needs to know to wire `Gba`.
