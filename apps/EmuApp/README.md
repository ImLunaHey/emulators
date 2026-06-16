# EmuApp — native macOS front-end

A SwiftUI desktop app that runs the emulator cores **natively**, so it isn't
bound by the browser's 4 GB wasm32 address space — you can open large media like
a 4.7 GB Xbox disc.

It links the unified [`emu-native`](../../packages/native) static library (all cores:
GBA, PS1, NDS, NES, SMS/GG, GBC, Xbox) through a thin C ABI.

## Build & run

```sh
# 1. build the Rust static lib + sync the C header into the Swift C target
./scripts/build-macos.sh           # or: --universal for arm64+x86_64

# 2. build & launch the app
cd apps/EmuApp
swift run EmuApp
```

## Two windows

- **Consoles** — a console-first library. Pick a console, then open a ROM (or
  re-launch a recent one). Recents are remembered per console.
- **Player** — the live game screen. Launching from the library opens it.

## Input

Keyboard and **PS5 / DualSense** controllers (via Apple's GameController
framework; any extended gamepad works).

| Action        | Keyboard            | DualSense            |
|---------------|---------------------|----------------------|
| D-pad         | Arrow keys          | D-pad / left stick   |
| Face (S/E/W/N)| Z / X / A / S       | ✕ / ○ / □ / △        |
| Shoulders     | Q / W               | L1 / R1              |
| Triggers      | D / F               | L2 / R2              |
| Start         | Enter               | Options              |
| Select        | Shift               | Create/Share         |

Per-system bit layouts live in `EmuSystem.keyMask` (they mirror the web players);
the NDS active-low keypad quirk is handled in the Rust FFI so every front-end
uses one "1 = pressed" convention.

## BIOS

PS1 and Xbox need a BIOS/flash image — load one from the console's detail view
(remembered for the session). For Xbox, a BIOS isn't required just to **mount a
disc and read its title** (the foundation core can't boot games yet).

## Notes

- Disc images are memory-mapped (`Data(.mappedIfSafe)`), so opening a 4.7 GB Xbox
  ISO doesn't read it all into RAM.
- The framebuffer is RGBA8888; the screen layer renders nearest-neighbour,
  aspect-fit.
- `Sources/CEmuNative/include/emu_native.h` is a synced copy of the canonical
  header in `packages/native/include/`; `build-macos.sh` refreshes it.
