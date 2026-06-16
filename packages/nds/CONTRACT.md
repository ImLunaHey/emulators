# Nintendo DS core port — contract for per-file porting agents

We are porting the TypeScript DS core in `../../ds-recomp/src` to Rust as the
**second** emulator core in this workspace, a sibling of the GBA core in
`../core`. This crate (`nds-core`) currently holds the **memory + CPU-state
foundation**. CPU instruction execution and every IO/PPU/cart/BIOS subsystem
land later, one file per agent.

**The TypeScript is a REFERENCE, not a source of truth.** Capture the intended
hardware behavior; fix obvious bugs; write idiomatic Rust (closed enums +
exhaustive `match`, real ownership, correct fixed-width integers). Do not
transliterate TS-isms.

Your job: port **one** TS source file into **one** Rust file under the module
tree below. Do **not** edit `lib.rs` or any `mod.rs` (the module declarations
are pre-wired to avoid races). Do **not** touch other agents' files.

## Ownership model (read this twice — same as the GBA core)

The TS code is a graph of classes holding references to each other (`Bus9`↔
`Io`, `Cp15`→`Bus9`+`mem`+`cpu`, …) — cycles Rust can't express. We break them
exactly like the GBA core:

- **One `Nds` god-struct** (`nds.rs`) owns one instance of every subsystem plus
  `SharedMemory`. It exposes the per-CPU bus accessors and (later) routes IO.
- **Each former TS class → one Rust struct owning ONLY its own state.**
- **Collaborators the TS constructor received become `&mut`/`&` PARAMETERS** on
  the methods that need them — never stored fields.

  TS:  `class Cp15 { constructor(bus9, mem) {…} write(...) {…} }`
  Rust: `impl Cp15 { fn write(&mut self, …, bus9: &mut Bus9, mem: &mut SharedMemory, cpu: &mut CpuState) }`

- If your TS class does `this.bus.read32(a)`, take the relevant accessor (e.g.
  `nds: &mut Nds` or the specific `&mut` pieces) as a parameter. Borrow-checker
  knots at the call site are the orchestrator's problem.

## Foundation API (already written — code against it, don't redefine)

### `crate::memory`
- `regions` — `*_BASE: u32`, `*_SIZE: usize`, `*_MASK: u32`, `WIFI_BASE/END`,
  `ITCM_SIZE`/`DTCM_SIZE`/`BIOS_SIZE`.
- `SharedMemory` — the single backing copy of every block both CPUs touch:
  `main_ram` (4 MB), `shared_wram` (32 KB), `arm7_iwram` (64 KB), `pram`,
  `oam`, `vram` (656 KB), `bios_arm7`, `bios_arm9` (all boxed fixed arrays),
  plus `wramcnt: WramCnt`. `load_main_ram(bytes, offset)`.
- `WramCnt` — closed enum `{AllToArm9, UpperToArm9, LowerToArm9, AllToArm7}`
  with `from_bits(u32)` / `bits()`. Replaces the TS magic `0..3`.
- `Bus9` — ARM9-private state: `itcm`/`dtcm` SRAM + their CP15 config
  (`*_base`, `*_virtual_size`, `*_enabled`, `*_load_mode`). Method
  `resolve(addr, for_write, &mut SharedMemory, &VramRouter, &[u8;9]) ->
  bus9::Resolved<'_>` returns `Mem(&mut [u8], idx)` / `Io` / `None`.
- `Bus7` — ARM7-private state: touch-struct HLE flags (`touch_pressed`,
  `touch_screen_x`). `resolve(addr, &mut SharedMemory, &VramRouter, &[u8;9])
  -> bus7::Resolved<'_>` returns `Mem` / `Io` / `Wifi` / `None`. Write
  interceptors `munge_write8/16/32(addr, v) -> v`.
- `VramRouter` — stateless w.r.t. VRAMCNT; every method takes `vramcnt:
  &[u8;9]`. `resolve_arm9/arm7(addr, …) -> Option<usize>` (flat `vram[]`
  index), plus the ext-palette + 3D-texture resolvers and `read_vram_stat`.

### `crate::cpu` / `crate::state`
- `state::{CpuState, mode::{USR,FIQ,IRQ,SVC,ABT,UND,SYS}, FLAG_N/Z/C/V/I/F/T}`.
  `CpuState` is shared by BOTH CPUs (same banked register model). Has `r:
  [u32;16]`, `cpsr`, `halted`, and `mode()`, `in_thumb()`, `irq_disabled()`,
  `check_cond()`, `set_nz()`, `set_nz64_hi()`, `set_c()`, `set_v()`, `c()`,
  `switch_mode()`, `get_spsr()/set_spsr()`, `enter_exception()`.
- `cpu::Cp15` — ARM9 system-control coprocessor. `read(opc1,crn,crm,opc2)`,
  `write(opc1,crn,crm,opc2,value, &mut Bus9, &mut SharedMemory, &mut
  CpuState)` (applies TCM relocation, control-register enable/load-mode bits,
  and Wait-For-Interrupt), `update_irq_handler_ptr_literal(&Bus9, &mut
  SharedMemory)`.

### `crate::nds`
- `Nds` — the god-struct. Fields: `mem`, `bus9`, `bus7`, `vram`, `vramcnt:
  [u8;9]` (placeholder owner until the PPU lands), `state9`, `state7`, `cp15`.
  Per-CPU accessors `read{8,16,32}_arm9/arm7` and `write{8,16,32}_arm9/arm7`
  (little-endian), plus `cp15_read`/`cp15_write`. IO/WiFi routing,
  `run_frame`, and `load_rom` are `todo!()` seams the subsystems fill.
- `Core` — `{Arm9, Arm7}`.

## Module tree (pre-declared; pick YOUR file)

```
cpu::{arm, thumb, shifter, exec}          // CPU execution (exec ← cpu.ts)
io::{dma, ds_math, ipc, irq, rtc, sound, spi, timers, touch}
ppu::{ppu, engine_a, text_bg, affine_bg, bitmap_bg, sprites}
cart::{loader, header, cart, overlays}
bios::{hle, nitro_os}
```
The 3D GPU (`gx`, `gx_fog`, `gx_lighting`) is **deferred** — no module yet.

## Conventions

- Registers/addresses/values are **`u32`**. Use **wrapping** arithmetic for
  JS `| 0` / `>>> 0` semantics. JS `x >>> n` → `x >> n` on `u32`; cast `as
  i32` only where the TS used `| 0` for a signed compare/shift.
- The DS is **little-endian**; assemble u16/u32 from bytes LE (matches the TS
  `DataView`).
- Keep exact hex constants/masks from the TS. Prefer closed enums over magic
  numbers where the GBA core would (see `WramCnt`).
- snake_case methods; struct named like the TS class.
- Keep the TS explanatory comments — they encode hard-won bug fixes (the
  WRAMCNT=3 reset default, the 0x027FFF8C SDK-flag OR, the WFI unmask, the
  0x01000000 main-RAM alias, …).
- Where cheap, add `#[cfg(test)]` tests mirroring `../../ds-recomp/src/test/*`.

## Build / self-check

`cargo check --manifest-path core-nds/Cargo.toml` is green at the foundation.
It will stay green as you add a faithful, type-correct file (the subsystem
mod lines are already wired; your file just needs to fill its stub). Run
`cargo test --manifest-path core-nds/Cargo.toml` to keep the foundation tests
passing. Leave `lib.rs` and every `mod.rs` untouched on exit.

Report: the file you wrote, any TS behavior you couldn't express 1:1, and any
method signature the orchestrator needs to know to wire `Nds`.
