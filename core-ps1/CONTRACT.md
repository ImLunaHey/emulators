# `core-ps1` — PlayStation 1 core contract for per-file agents

This is the **third** emulator core in the repo (sibling of the GBA `../core`
and the NDS core). Unlike those, it is built **from scratch against nocash's
psx-spx hardware spec** — there is no TypeScript source to port. The
**foundation** (CPU register/exception state, the memory map, the bus) is
landed; your job is to fill in **one** subsystem module under `src/` against
these interfaces.

Your job: implement **one** subsystem file (`gte`, `gpu`, `spu`, `dma`,
`timers`, `cdrom`, `mdec`, `irq`, `sio`, `bios`, or `cpu::exec`). Each is an
empty `// TODO` stub today. Do **not** edit `lib.rs` or `*/mod.rs` (module
declarations are already wired to avoid races) and do **not** touch other
agents' files.

## Ownership model (read this twice)

PSX hardware is a graph of devices that reference each other. Rust can't express
those cycles, so — exactly like the GBA/NDS cores:

- **One [`Psx`] god-struct** (`src/psx.rs`) owns **one instance of every
  subsystem** plus [`Mem`], and implements [`bus::Bus`].
- **Each subsystem → one struct that owns ONLY its own state.**
- **Collaborators a method needs become `&mut` PARAMETERS**, never stored
  fields. If your device does a bus access, take `bus: &mut dyn crate::bus::Bus`
  and call `bus.read32(a)` / `bus.write16(a, v)`. Borrow-checker knots at the
  call site (`mem::take` the device, pass `self` as the bus) are the
  orchestrator's problem — give a faithful, readable implementation.
- **No `Rc`/`RefCell`.** Closed enums + exhaustive `match`. Little-endian.
  Correct fixed-width integer types (`u8`/`u16`/`u32`); wrapping arithmetic.

When you add a real subsystem struct, replace its `()` placeholder field in
`Psx` (`pub gpu: ()` → `pub gpu: Gpu`) and its `new()` init — coordinate that
one-line change with the orchestrator.

## Foundation API (already written — code against it, don't redefine)

### `crate::regions`
Memory-map sizes/masks/bases (`RAM_SIZE` = 2 MB, `SCRATCHPAD_SIZE` = 1 KB,
`BIOS_SIZE` = 512 KB, `IO_BASE` = 0x1F80_1000, …) and:
- `fn mask_region(addr: u32) -> u32` — virtual→physical segment fold
  (KUSEG/KSEG0/KSEG1 → `& 0x1FFF_FFFF`; KSEG2 passes through for the
  cache-control register at `CACHE_CONTROL_ADDR` = 0xFFFE_0130).

### `crate::cpu` — the MIPS R3000A
- `cpu::state::Cpu` — architectural state: `regs: [u32;32]` (**r0 hardwired to
  0**, enforced by `reg(i)`/`set_reg(i,v)`), `hi`, `lo`, and the two quirks:
  - **Branch delay slot**: `pc` (executing), `next_pc` (fetch next),
    `current_pc` (the instr before — drives `CAUSE.BD`), `in_delay_slot`,
    `branch_taken`. A taken branch rewrites `next_pc`; the delay-slot instr
    still runs from the previously-latched value.
  - **Load delay slot**: `load: LoadSlot { reg, value }` (`reg == 0` ⇒ empty).
    `queue_load(reg,val)` issues a load; `commit_load()` (call at the top of the
    *next* instruction) writes it back; `shadow_load(reg)` cancels a pending
    load whose destination an instruction overwrites first.
  - `cache_isolated()` — SR.IsC; `raise_exception(Exception)` — does the COP0
    bookkeeping and redirects the PC pair to the handler vector.
  - `RESET_VECTOR` = 0xBFC0_0000 (execution starts in the BIOS).
- `cpu::cop0::Cop0` — system control: `sr`, `cause`, `epc`, `bad_vaddr`,
  `prid` (+ debug regs). `read(reg)`/`write(reg,v)` are MFC0/MTC0.
  `enter_exception(cause, pc, in_delay) -> u32` sets CAUSE (Excode+BD), EPC,
  pushes the SR (KU,IE) stack, and **returns** the vector (0x8000_0080 if
  BEV=0, 0xBFC0_0180 if BEV=1). `return_from_exception()` is RFE (pop stack).
  Bit constants: `SR_IEC/KUC/IEP/KUP/IEO/KUO/IM/ISC/BEV/CU0/CU2`,
  `CAUSE_EXCCODE_MASK/_SHIFT`, `CAUSE_IP_*`, `CAUSE_BD`.
- `cpu::cop0::Exception` — closed enum of Excode values (`Interrupt`,
  `AddressErrorLoad/Store`, `BusError*`, `Syscall`, `Breakpoint`,
  `ReservedInstruction`, `CoprocessorUnusable`, `Overflow`).

### `crate::memory::Mem`
Owns the **dumb** backing storage only (no I/O, no translation): boxed
`ram` (2 MB), `scratchpad` (1 KB), `bios` (512 KB, `load_bios(&[u8])`,
`bios_loaded`). Power-of-2-masked accessors `ram_read32`/`scratch_write16`/…
take a **region-local offset** (already folded). `region_read(region, size)` /
`region_write(region, size, v)` handle RAM/scratchpad/BIOS for a classified
`Region` and return `None`/`false` for I/O so the bus routes it.

### `crate::bus`
- `trait Bus { read8/16/32, write8/16/32, fetch32 }` — what the CPU sees;
  `Psx` is the implementor.
- `enum Region { Ram(off), Scratchpad(off), Io(off), Bios(off),
  Expansion1/2/3(off), CacheControl, Unmapped }` — closed classification of a
  **physical** address.
- `fn classify(paddr) -> Region`, `fn translate(vaddr) -> Region` (mask +
  classify). RAM offsets are pre-folded with `RAM_MASK` (mirror in first 8 MB);
  scratchpad/BIOS offsets pre-masked.

### `crate::psx::Psx`
The god-struct. Owns `mem`, `cpu`, a `()` slot per subsystem, and the
memory-control / cache-control registers. Implements `Bus`; `read`/`write`
translate → classify → route to `Mem` or to `io_read`/`io_write`
(**`todo!()` seams** — implement your device's register window there, or expose
a method the orchestrator calls). **Cache-isolation (SR.IsC) already drops RAM
writes** in `write()`, matching the BIOS's i-cache-invalidation boot trick.

## Conventions

- Addresses/registers/values are `u32`; use wrapping arithmetic. Little-endian.
- `snake_case` methods; struct named like the device (`Gpu`, `Dma`, `Spu`, …).
- `#[derive(Default)]` or a `new()`; keep spec page/section refs in comments.
- Add `#[cfg(test)]` unit tests for non-trivial logic (the foundation modules
  have examples).

## Build / self-check

`cargo check --manifest-path core-ps1/Cargo.toml` passes today and must keep
passing. `cargo test` runs the foundation tests. Leave `lib.rs` / `mod.rs`
untouched on exit; report the file you wrote, any spec ambiguity you hit, and
any `Psx`-field change the orchestrator needs to wire your subsystem.
