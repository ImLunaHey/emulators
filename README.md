# gba-recomp

A Game Boy Advance emulator that runs entirely in the browser. ARM7TDMI interpreter, scanline-accurate PPU (text + affine BGs, sprites with rotation/scaling, windows, blending), DirectSound A/B audio, Macronix 128 KB Flash saves, S-3511A RTC. React + Tailwind UI, controller support with on-the-fly remap, drag-and-drop ROM library.

No prebuilt BIOS, no off-the-shelf cores — the whole stack is written from scratch in TypeScript.

## Status

| Game | Boots | Plays | Sound | Notes |
|---|---|---|---|---|
| Pokemon FireRed | ✓ | ✓ | ✓ | Oak intro + name entry verified |
| Pokemon Emerald | ✓ | ✓ | ✓ | |
| Pokemon Ruby | ✓ | ✓ | ✓ | "Battery has run dry" fixed |
| Garfield: Search for Pooky | ✓ | ✓ | ✓ | Language select renders |
| Crash Bandicoot | ✓ | ✓ | ✓ | Title intro + Earth flyby |

210 vitest cases gate the CPU, BIOS HLE, PPU compositor + sprites + BG + window mask, DMA channels, timers, IRQ, RTC bit-bang protocol, Flash save/erase, and LZ77 decompression.

## Quick start

```bash
git clone git@github.com:ImLunaHey/gba-recomp.git
cd gba-recomp
npm install
npm run dev
```

Open the URL Vite prints. The ROM Library panel auto-opens on first launch — drag any `.gba` file into it. ROMs are stored in your browser's IndexedDB (never uploaded anywhere); saves are stored in `localStorage` per-game.

## Controls

| GBA | Keyboard | PS5 / DualShock | Xbox-style |
|---|---|---|---|
| A | Z | ✕ Cross | A |
| B | X | ○ Circle | B |
| L / R | A / S | L1 / R1 | LB / RB |
| Start | Enter | Options | Menu |
| Select | Shift | Share | View |
| D-pad | Arrow keys | D-pad | D-pad |

The on-screen buttons under the canvas are clickable / touchable and light up for any input source. Open **Controller…** to see live button + axis state and rebind any binding (click a row → press the button → it's bound; Esc to unbind; Reset to restore defaults).

The HID hat axis encoding (PS5 on macOS Safari) is auto-decoded.

## Saves

In-game saves are automatic — write to a save slot in Pokemon and it persists to `localStorage` keyed by the game's 4-letter code. **Export .sav** downloads the raw 128 KB Flash blob; **Import .sav** uploads one. **Clear Save** removes the persisted blob.

## Running headless

```bash
# Boot a ROM for N frames and dump CPU/PPU stats
npm run boot -- public/firered.gba 60

# Press A every 60 frames (useful for getting past intros)
PRESS_A_EVERY=60 npm run boot -- public/firered.gba 600

# Dump active OAM after frame N
DUMP_SPRITE_FRAME=300 npm run boot -- public/firered.gba 0
```

ROMs aren't shipped — put `.gba` files in `public/` and they'll be reachable. `public/*.gba` is gitignored.

## Architecture

```
src/
  cpu/            ARM7TDMI interpreter
    state.ts        16 registers, banked SP/LR per mode, CPSR/SPSR
    arm.ts          ARM-mode dispatch + decode (32-bit insns)
    thumb.ts        THUMB-mode dispatch + decode (16-bit insns)
    shifter.ts      Barrel shifter
    cpu.ts          Top-level step(), IRQ handling, exception entry,
                    BIOS stub installation
  ppu/
    ppu.ts          Mode dispatch, scanline timing, VBlank/HBlank/VCount IRQs
    modes_text.ts   Text-mode BG renderer (4bpp and 8bpp)
    modes_affine.ts Affine BG renderer (mode 1 BG2, mode 2 BG2/3)
    modes_bitmap.ts Modes 3/4/5 bitmap renderers
    sprites.ts      OAM scan + sprite render (incl. affine + double-size)
    composite.ts    Per-pixel BG/OBJ priority + windowing + blending
  io/
    io.ts           IO port routing
    irq.ts          IE/IF/IME
    dma.ts          4 channels (immediate / VBlank / HBlank / special)
    timers.ts       4 timers with prescalers + count-up cascade
    sound.ts        DirectSound A/B FIFOs, timer-driven sample drain
    keypad.ts       KEYINPUT bitmask
  memory/
    bus.ts          Region routing (EWRAM/IWRAM/PRAM/VRAM/OAM/ROM/SRAM)
    flash.ts        Macronix 128 KB Flash chip emulation
    rtc.ts          Seiko S-3511A RTC bit-bang protocol
    regions.ts      Memory map constants
  bios/
    hle.ts          BIOS SWI high-level emulation
                    (RegisterRamReset, Div, CpuSet, CpuFastSet,
                     LZ77UnComp{Wram,Vram}, BitUnPack, ObjAffineSet,
                     IntrWait, VBlankIntrWait, SoftReset)
  recomp/           WASM recompiler skeleton (gated off — interpreter
                    handles everything currently)
  ui/               React + Tailwind UI (Screen, Gamepad, ControllerPanel,
                    RomLibrary, LogPane, audio sink, gamepad polling)
  emulator.ts       Composes all the above; runFrame() runs ~280896
                    CPU cycles, advances PPU/Timer/Sound, returns stats
```

## Build + test

```bash
npm run test        # 210 cases via vitest
npm run test:watch  # interactive
npm run lint        # oxlint
npm run build       # tsc -b && vite build → dist/
npm run preview     # serve the production build locally
```

## What's missing

Not implemented yet:

- **PSG channels 1-4** (the GB-compatible square/wave/noise). Most modern games use DirectSound for everything, so this is mostly silent in practice, but a handful of games rely on PSG bings/bleeps.
- **EEPROM and SRAM save types.** Only Flash 128 KB right now. Some games use EEPROM (Pokemon Pinball, FF Tactics Advance) or plain 32 KB SRAM.
- **Mosaic** effect (decoded but not applied to render).
- **Link cable / multiplayer.** No SIO normal / multiplayer / JOY modes — Pokemon trading won't work.
- **Hardware sensors.** Solar (Boktai), tilt (Yoshi Topsy-Turvy), gyro (WarioWare Twisted), rumble.
- **More BIOS HLE.** Missing HuffmanUnComp, RLUnComp, DiffUnFilter, MidiKey2Freq.
- **Savestates** (snapshot/restore of full emulator state — in-cart saves work).
- **Cheats** (GameShark / Action Replay / Code Breaker).

## Tech

- TypeScript everywhere
- React 19 + Tailwind 4 for UI
- Vite for the dev server + bundler
- Vitest for tests
- Oxlint for lint
- IndexedDB for ROM storage, localStorage for save persistence
- Web Audio API for sound
- Web Gamepad API for controllers
- Pointer events for touch + mouse

The core emulator (`cpu/`, `ppu/`, `io/`, `memory/`, `bios/`) is pure TypeScript with TypedArrays — no DOM, no browser APIs. It can be reused under a different UI shell.
