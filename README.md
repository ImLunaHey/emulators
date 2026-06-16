# emulators

A Game Boy Advance emulator that runs entirely in the browser. Cycle-batched ARM7TDMI interpreter (ARM + THUMB), full PPU (every BG mode, sprites with rotation/scaling, affine, windows, mosaic, blending), DMA, timers, IRQs, PSG + DirectSound audio in stereo, SIO link cable over WebRTC, S-3511A RTC, and autodetected SRAM / Flash / EEPROM saves on an HLE BIOS. The **core is written in Rust and compiled to WebAssembly**; a React + Tailwind UI wraps it with a ROM library, save states, cheats, gamepad/keyboard support, and an installable PWA shell.

No prebuilt BIOS, no off-the-shelf cores — the whole stack is written from scratch.

## Status

| Game | Boots | Plays | Sound | Notes |
|---|---|---|---|---|
| Pokemon FireRed | ✓ | ✓ | ✓ | Oak intro + name entry verified |
| Pokemon Emerald | ✓ | ✓ | ✓ | |
| Pokemon Ruby | ✓ | ✓ | ✓ | "Battery has run dry" fixed |
| Garfield: Search for Pooky | ✓ | ✓ | ✓ | Language select renders |
| Crash Bandicoot | ✓ | ✓ | ✓ | Title intro + Earth flyby |

The Rust core ships a **235-vector `cargo test` suite** covering the CPU (ARM + THUMB instruction vectors, IRQ entry/return, banking), the PPU (text/bitmap/affine modes, sprites, the compositor's priority/blend/window logic — plus self-contained **golden-frame** tests), DMA, timers, IRQ, sound (PSG + DirectSound), the save back-ends, the RTC, and save-state round-trips. During the port the core was additionally validated **frame-by-frame** (registers + all RAM) against the original TypeScript interpreter — bit-identical for 120 frames across the test ROMs.

## Core (Rust → WebAssembly)

The emulator core lives in [`core/`](core/) as a standalone Rust crate (`gba-core`) with no DOM or browser dependencies. It's a faithful, instruction-approximate interpreter — every former TS class is a Rust struct, wired together by one `Gba` god-struct that owns all state and runs a frame at a time. `wasm-pack` compiles it to a ~133 KB WebAssembly module (`core/pkg/`), and a thin `wasm-bindgen` surface (`WasmGba`) exposes `load_rom` / `run_frame` / `framebuffer` / `set_keys` / audio / battery saves / save states / cheats / the link-cable bridge / a debug-introspection snapshot.

The browser app talks to it through `src/ui/wasmEmulator.ts`, an adapter that re-exposes the surface the React UI expects and forwards everything to the wasm instance.

## Quick start

```bash
git clone git@github.com:ImLunaHey/emulators.git
cd emulators
npm install
npm run dev
```

The compiled wasm core (`core/pkg/`) is committed, so a fresh clone runs and builds without a Rust toolchain. **If you change the Rust core**, rebuild it with `npm run build:wasm` (needs the [Rust toolchain](https://rustup.rs) with the `wasm32-unknown-unknown` target and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)) and commit the updated `core/pkg/`. `npm run build` / `deploy` consume the committed artifact; `npm run deploy` rebuilds it first.

Open the URL Vite prints. For a stable local URL, `npm run dev:portless` serves `https://emulators.localhost` via [portless](https://www.npmjs.com/package/portless).

Open the app, then add a ROM through the library — drag any `.gba` file in, or use the picker. ROMs are stored in your browser's IndexedDB and **never leave the browser**; in-cart saves and save states persist locally too.

## ROM library

The home page is a library: a searchable, sortable grid of your ROMs with cover art fetched from Hasheous / IGDB / LibRetro, a **"Continue playing"** hero for your most recent game, and a per-ROM details page. Click a card to jump to `/play/:romId`. Everything is local-first — the only network calls are for cover art lookups.

## Controls

| GBA | Keyboard | PS5 / DualShock | Xbox-style |
|---|---|---|---|
| A | Z | ✕ Cross | A |
| B | X | ○ Circle | B |
| L / R | A / S | L1 / R1 | LB / RB |
| Start | Enter | Options | Menu |
| Select | Shift | Share | View |
| D-pad | Arrow keys | D-pad | D-pad |

Player shortcuts:

| Key | Action |
|---|---|
| Tab | Fast-forward (hold) |
| `.` | Frame-step |
| F2 / F4 | Quick save / quick load |
| Backspace | Rewind (when enabled in Settings) |

Input works from on-screen buttons, the keyboard, and hardware gamepads — **multiple gamepads at once**, with active-controller switching. The on-screen buttons are clickable / touchable and light up for any input source. Keyboard bindings are remappable, and there's turbo/autofire per button. A controller hotkey (**Start+Select**, or the **PS5 touchpad**) opens a controller-navigable menu so you can drive the whole UI from the pad. The HID hat-axis encoding (PS5 on macOS Safari) is auto-decoded.

## Save states & in-cart saves

In-cart saves are automatic and persist locally, keyed by the game's code. The save back-end is **autodetected** from the ROM's AGB signature — 32 KB SRAM, 64 KB / 128 KB Flash, or 512 B / 8 KB serial EEPROM — defaulting to Flash 128 KB. Export / import the raw `.sav` blob from the player.

Save states are separate: snapshot the full emulator state into numbered **slots**, each with a thumbnail. Quick save / load is on F2 / F4, plus auto-save and auto-resume so you pick up where you left off. The snapshot format is a versioned, tagged-section binary blob produced by the Rust core.

## Settings & extras

- **Audio** — volume, mute.
- **Speed** — emulation-speed multipliers, plus fast-forward (Tab) and frame-step (`.`).
- **Video** — pixel-perfect vs. bilinear scaling, GBA LCD color correction, and an LCD-grid overlay.
- **Rewind** — hold Backspace to scrub backwards when enabled.
- **Haptics** — gamepad/touch rumble feedback.
- **Cheats** — GameShark / Action Replay style codes via the Cheats panel.
- **Link cable** — SIO over WebRTC for local multiplayer / trading via the Link panel.
- **Screenshots** of the current frame.
- **PWA** — installable, with an offline app shell.

The UI is a themed dark shell with shared modals, toasts, and mobile bottom-sheets.

## Architecture

```
core/                  Rust GBA core — a standalone crate, no browser deps
  src/
    state.rs             ARM7TDMI register file, banking, CPSR/SPSR
    arm.rs / thumb.rs    ARM (32-bit) + THUMB (16-bit) interpreters
    shifter.rs           Barrel shifter
    cpu.rs               step(), IRQ handling, exception entry, BIOS stub
    bus.rs / regions.rs  Memory map + region routing (Mem)
    flash.rs / sram.rs / eeprom.rs / save_detect.rs   Save back-ends + autodetect
    rtc.rs               Seiko S-3511A RTC bit-bang protocol
    ppu.rs               All BG modes, sprites (affine), windows, mosaic, blending
    dma.rs / timers.rs / irq.rs   DMA, timers (+ count-up cascade), IE/IF/IME
    sound.rs             PSG 1-4 + DirectSound A/B FIFOs, stereo mix
    sio.rs               Serial IO (normal / multiplayer) + async link bridge
    keypad.rs / cheats.rs
    bios.rs              BIOS SWI high-level emulation
    savestate.rs         Full-state snapshot / restore
    emulator.rs          The `Gba` god-struct: owns everything, runs frames,
                         implements the memory bus (io.ts routing lives here)
    debug.rs             Introspection surface for the UI debug panel
    wasm.rs              wasm-bindgen surface (WasmGba)
  pkg/                   wasm-pack output (committed; rebuilt by `npm run build:wasm`)

src/                   React + TypeScript browser app
  ui/                    React UI (LibraryPage, PlayerPage, Screen, Gamepad,
                         ControllerPanel, SaveStatesPanel, SettingsPanel,
                         CheatsPanel, LinkPanel, DebugPanel, audio sink, …)
  ui/wasmEmulator.ts     Adapter exposing the wasm core to the UI
  io/sio-signal.ts       WebRTC link-cable transport (drives the core's SIO)
  io/keypad.ts           Key enum (button bit layout)
  io/cheats.ts           Cheat parsing (the engine runs in the core)
  io/sio.ts              MultiplayResult type + LocalLoopback sentinel
  emulator.ts            `type Emulator = WasmEmulator` (UI prop type)
  worker.ts              Cloudflare Worker entry (serves the built app)
```

## Build + test

```bash
npm run build:wasm  # wasm-pack build core → core/pkg (rebuild after core changes)
npm run build       # tsc + vite build → dist/ (uses the committed core/pkg)
npm test            # cargo test (Rust core; 235 vectors)
npm run lint        # oxlint (UI/TS)
npm run dev         # Vite dev server
npm run preview     # build, then serve via wrangler dev
npm run deploy      # build + wrangler deploy (Cloudflare)
```

ROMs aren't shipped — `public/*.gba` is gitignored.

## Bugs / requests

[github.com/ImLunaHey/emulators/issues](https://github.com/ImLunaHey/emulators/issues) — please include the game, what you were doing, and (if a visual bug) a screenshot. If you can reproduce it from a fresh boot with a specific sequence of inputs, even better.

## Tech

- **Rust** core compiled to **WebAssembly** via `wasm-bindgen` / `wasm-pack`
- React 19 + Tailwind 4 for UI, React Router for navigation
- TanStack Query (with persistence) for cover-art fetching/caching
- Vite for the dev server + bundler
- `cargo test` for the core, Oxlint for the UI
- Cloudflare Workers + Wrangler for deploy
- IndexedDB for ROM storage, local persistence for saves + save states
- Web Audio API for sound, Web Gamepad API for controllers, Pointer events for touch + mouse
- WebRTC for the link cable
- PWA (web manifest + service worker) for install + offline shell

The core (`core/`) is a dependency-free Rust crate that compiles to WebAssembly and can be reused under a different host — the React app is just one shell over it.
