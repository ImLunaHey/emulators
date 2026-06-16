# emulators

A collection of **from-scratch emulators** — no off-the-shelf cores, no prebuilt
BIOSes. Each core is written in **Rust**, compiled to **WebAssembly** for the
browser and to a native static library for a macOS SwiftUI app. A React +
Tailwind front-end wraps the web build with a ROM library, save states, cheats,
gamepad/keyboard support, audio, and an installable PWA shell.

The Game Boy Advance core is the most mature (see its deep-dive below); the
others range from "boots and plays" to early foundations.

## Systems

| System | Core | Status |
|---|---|---|
| Game Boy Advance | `packages/gba` | Mature — many commercial titles boot, play, and have sound |
| Game Boy / Color | `packages/gbc` | Playable |
| NES | `packages/nes` | Playable |
| SNES | `packages/snes` | Playable |
| Master System / Game Gear | `packages/sms` | Playable |
| Mega Drive / Genesis | `packages/genesis` | Playable |
| PC Engine / TurboGrafx-16 | `packages/pce` | Playable |
| Nintendo DS | `packages/nds` | Playable (2D + 3D geometry engine) |
| PlayStation (PS1) | `packages/ps1` | Playable (BIOS-gated; ships the open-source OpenBIOS) |
| Nintendo 64 | `packages/n64` | In progress |
| Xbox | `packages/xbox` | Foundation — boots nxdk homebrew; the NV2A renders real geometry (the triangle demo draws + animates) |
| Atari 2600 | `packages/atari2600` | Playable |
| Neo Geo Pocket Color | `packages/ngpc` | Playable |
| WonderSwan | `packages/wonderswan` | Playable |
| Virtual Boy | `packages/virtualboy` | Playable |
| GameCube | `packages/gc` | Early WIP |

Every core is a dependency-free Rust crate with no DOM/browser assumptions, so
it can be reused under a different host — the React app and the SwiftUI app are
both just shells over the same cores.

For the full per-core breakdown — exactly what CPU/video/audio/saves each core
implements, which file formats it loads, its test coverage, and the games
verified on it — see the **docs site** (`apps/docs`, run with
`pnpm --filter @emulators/docs dev`).

## Monorepo layout

[pnpm workspaces](https://pnpm.io/workspaces) + [Turborepo](https://turborepo.com).
Each core is its own package (with its own `package.json` + `turbo.json`) under
`packages/*`; the apps live under `apps/*`. Turbo's `build` pipeline compiles
every core's wasm (`wasm-pack`) before the web app's `tsc && vite build`, so a
fresh checkout builds in dependency order.

```
packages/
  gba/ gbc/ nes/ snes/ sms/ genesis/ pce/ nds/ ps1/ n64/   one wasm core each:
  xbox/ atari2600/ ngpc/ wonderswan/ virtualboy/ gc/         @emulators/<name>
    src/                 the Rust core (CPU, PPU/GPU, APU, bus, …)
    Cargo.toml           standalone crate (e.g. gba-core)
    package.json         build = wasm-pack build . --target web --out-dir pkg
    turbo.json           extends the root pipeline
    pkg/                 wasm-pack output (gitignored; built by turbo)
  native/                unified C-ABI FFI over every core (for the native apps)
  ffi/                   GBA-only mobile FFI (iOS xcframework / Android .so)
  api/                   shared core/host contract traits

apps/
  web/                   React + Vite + Cloudflare Worker front-end (@emulators/web)
    src/ui/              players per system + library, save states, cheats, audio
    src/worker.ts        Cloudflare Worker entry (serves the built SPA)
  docs/                  React + Vite static docs site (@emulators/docs)
    src/cores.ts         per-core support catalog (CPU/GPU/APU, formats, tests)
  EmuApp/                native macOS SwiftUI app (links packages/native)

scripts/                 native build helpers (build-macos/ios/android.sh), gen-cheats
turbo.json               root build pipeline
pnpm-workspace.yaml      packages/* + apps/*
```

The web app imports each core by package name (`import init, { WasmNes } from
'@emulators/nes'`); pnpm symlinks the workspace package and Vite bundles its
`pkg/` wasm.

## Quick start

```bash
git clone git@github.com:ImLunaHey/emulators.git
cd emulators
pnpm install
pnpm dev          # builds every core's wasm, then runs the web app (Vite)
```

Open the URL Vite prints. For a stable local URL, run
`pnpm --filter @emulators/web dev:portless` to serve `https://emulators.localhost`
via [portless](https://www.npmjs.com/package/portless).

You'll need the [Rust toolchain](https://rustup.rs) with the
`wasm32-unknown-unknown` target and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)
— turbo builds the wasm cores on first run (`pkg/` is **not** committed).

Open the app, then add a ROM through the library — drag a file in, or use the
picker. ROMs are stored in your browser's IndexedDB (huge media like a disc image
is read from disk on demand and never copied into the browser) and **never leave
your machine**; in-cart saves and save states persist locally too.

## Build + test

All commands run through turbo from the repo root:

```bash
pnpm build                       # build every core's wasm + the web app (turbo)
pnpm build --filter @emulators/web   # just the web app + the cores it depends on
pnpm test                        # cargo test across the cores (turbo)
pnpm lint                        # oxlint (web)
pnpm deploy                      # build + wrangler deploy the web app (Cloudflare)
pnpm --filter @emulators/nes build   # rebuild a single core's wasm
```

Native macOS app:

```bash
./scripts/build-macos.sh         # build the unified native lib + sync the C header
cd apps/EmuApp && swift run EmuApp
```

ROMs aren't shipped — `*.gba`/`*.bin`/`*.iso` etc. are gitignored (the one
exception is the redistributable open-source PS1 BIOS bundled by the web app).

## GBA core (the flagship)

The Game Boy Advance core in [`packages/gba`](packages/gba/) (crate `gba-core`)
is a faithful, instruction-approximate ARM7TDMI interpreter (ARM + THUMB), with a
full PPU (every BG mode, sprites with rotation/scaling, affine, windows, mosaic,
blending), DMA, timers, IRQs, PSG + DirectSound stereo audio, SIO link cable over
WebRTC, S-3511A RTC, and autodetected SRAM / Flash / EEPROM saves on an HLE BIOS.

| Game | Boots | Plays | Sound | Notes |
|---|---|---|---|---|
| Pokemon FireRed | ✓ | ✓ | ✓ | Oak intro + name entry verified |
| Pokemon Emerald | ✓ | ✓ | ✓ | |
| Pokemon Ruby | ✓ | ✓ | ✓ | "Battery has run dry" fixed |
| Garfield: Search for Pooky | ✓ | ✓ | ✓ | Language select renders |
| Crash Bandicoot | ✓ | ✓ | ✓ | Title intro + Earth flyby |

It ships a **235-vector `cargo test` suite** covering the CPU (ARM + THUMB
vectors, IRQ entry/return, banking), the PPU (text/bitmap/affine modes, sprites,
the compositor's priority/blend/window logic — plus golden-frame tests), DMA,
timers, IRQ, sound, the save back-ends, the RTC, and save-state round-trips.
During the original TS→Rust port it was validated frame-by-frame (registers + all
RAM) against the reference interpreter — bit-identical for 120 frames across the
test ROMs.

## Front-end features

- **ROM library** — searchable, sortable grid with cover art (Hasheous / IGDB /
  LibRetro), a "Continue playing" hero, and per-ROM details. Local-first; the
  only network calls are cover-art lookups.
- **Save states** — snapshot / restore per game.
- **Cheats** — GameShark / Action Replay style codes.
- **Link cable** — SIO over WebRTC for local multiplayer / trading (GBA).
- **Gamepad / keyboard / touch**, haptics, screenshots, and a **PWA** install +
  offline shell.

## Controls (GBA)

| GBA | Keyboard | PS5 / DualShock | Xbox-style |
|---|---|---|---|
| A | Z | ✕ Cross | A |
| B | X | ○ Circle | B |
| L / R | A / S | L1 / R1 | LB / RB |
| Start | Enter | Options | Menu |
| Select | Shift | Share | View |
| D-pad | Arrow keys | D-pad | D-pad |

Player shortcuts: **Tab** fast-forward (hold), **`.`** frame-step.

## Tech

- **Rust** cores compiled to **WebAssembly** via `wasm-bindgen` / `wasm-pack`,
  and to a native static lib for the SwiftUI app
- **pnpm workspaces + Turborepo** monorepo
- React 19 + Tailwind 4 + React Router; TanStack Query (persisted) for cover art
- Vite dev server + bundler; Cloudflare Workers + Wrangler for deploy
- IndexedDB + File System Access API for ROM storage; local persistence for saves
- Web Audio for sound, Gamepad API for controllers, Pointer events for touch
- WebRTC for the link cable; PWA (manifest + service worker)

## Bugs / requests

[github.com/ImLunaHey/emulators/issues](https://github.com/ImLunaHey/emulators/issues)
— please include the system, the game, what you were doing, and (for a visual
bug) a screenshot. A reproducible input sequence from a fresh boot is ideal.
