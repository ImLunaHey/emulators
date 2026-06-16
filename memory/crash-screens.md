---
name: crash-screens
description: Every emulator core renders its own fault/crash screen in Rust (not React)
metadata:
  type: project
---

All six cores detect a fatal fault and draw a crash screen **in Rust**, into their own RGBA framebuffer — the host (React `*Player.tsx`) just blits the framebuffer, so a crash appears automatically. Do NOT add crash UI in React (the user was firm on this). Each core has a `src/crash.rs` (5x7 bitmap font + `render(fb, w, h, lines)`; PS1's is the `[u32]` original, the rest are RGBA8888-byte ports) and a `fault: Option<...>` on its top-level struct; once faulted, `run_frame` freezes the CPU and re-presents the panel each frame.

Per-architecture trigger (what counts as a "crash"):
- **PS1** (R3000A): exception storm — >10k exceptions in one frame (`FAULT_THRESHOLD`). See [[ps1-biosless-strategy]].
- **GBA / NDS** (ARM): undefined-instruction exception storm (agents ADDED the UND exception entry on the architecturally-reserved undefined slots — GBA `0x06000010` mask, NDS `UDF` `0x07F000F0` — then count >10k/frame). NDS draws on the top screen.
- **NES** (6502): JAM/KIL opcodes (0x02,0x12,…,0xF2) — were NOPs, now latch a fault.
- **GBC** (SM83): invalid opcodes (0xD3,0xDB,0xDD,0xE3,0xE4,0xEB,0xEC,0xED,0xF4,0xFC,0xFD).
- **SMS** (Z80): best-effort — HALT with interrupts disabled (IFF1=0) persisting a whole frame (a real deadlock). Z80 has no illegal-opcode trap, so this is the only clean condition.

Panel text: `"<CORE> CORE FAULT"` + a CAUSE line + `PC` (uppercase hex; font is A-Z/0-9/space/`:=-.` only). Each core has a unit test forcing its trigger and asserting `fault.is_some()` + non-background framebuffer pixels.
