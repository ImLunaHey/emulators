---
name: gamecube-core
description: A foundation-only GameCube core (core-gc) was scaffolded; not a working emulator
metadata:
  type: project
---

`core-gc/` is a **foundation-only** GameCube core scaffolded 2026-06-15 (agent), matching the repo's core conventions (crate-type cdylib+rlib, wasm-only wasm-bindgen, `WasmGc` mirroring `WasmPsx`). It is NOT a working emulator.

Built: Gekko (PowerPC 750CXe) CPU — register file + ~25 integer instructions (addi/addis/add/subf/or/ori/oris/and/andi/cmp/cmpi/b/bl/bc(bdnz)/bclr/lwz/stw/mfspr/mtspr/rlwinm/sc/rfi) with unimplemented opcodes raising a Program exception (no silent no-ops); **big-endian** memory map (24MB 1T-SRAM at 0x8000_0000 cached / 0xC000_0000 uncached, 64KB MMIO at 0xCC00_0000, 2MB IPL ROM); Flipper GPU stub owning a 640x480 RGBA8888 framebuffer (render_frame = no-op clear); `Gc` god-struct + Bus trait (8/16/32/**64**-bit). 48 unit tests pass, 0 clippy warnings.

Entirely absent (future work): Flipper GX pipeline (CP/XF/TEV/PE/EFB/VI), DSP, DI/DVD, EXI (memcards), SI (controllers), AI audio, PI/MI interrupts + decrementer, full MMU/BAT, FP/paired-single, IPL/BS1+BS2 boot. No `build:wasm:gc` script in package.json yet (would follow the existing `build:wasm:*` pattern). A full GC emulator is a months-long effort.
