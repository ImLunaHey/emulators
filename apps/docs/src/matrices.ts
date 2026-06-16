// Per-core capability matrices, keyed by core id.
//
// Each matrix benchmarks the core against the "ideal emulator" capability set
// for that system — taken from the emulation-general wiki's per-system
// feature/peripheral comparisons (and, for the Game Boy family, Shonumi's
// "State of Emulation 2024" peripheral catalog) — then scores each row against
// what OUR Rust core in packages/<id> actually implements:
//
//   yes      — implemented and solid
//   partial  — implemented but incomplete / approximate (needs work)
//   testing  — implemented but under-verified vs real games (needs testing)
//   no       — not implemented yet (to add)
//
// Keep these updated alongside core changes. Rendered by <SupportMatrix> on
// each core page.

import type { MatrixGroup } from './cores';

export const MATRICES: Record<string, MatrixGroup[]> = {
  gba: [
    {
      group: 'CPU & timing',
      rows: [
        { feature: 'ARM + THUMB interpreter', support: 'yes', note: 'Validated bit-identical vs a reference interpreter for 120 frames' },
        { feature: 'IRQ / exception banking', support: 'yes' },
        { feature: 'Cycle accuracy', support: 'partial', note: 'Instruction-approximate, not cycle-/dot-exact' },
        { feature: 'BIOS-region open-bus behavior', support: 'partial', note: 'Simplified' },
      ],
    },
    {
      group: 'Video (PPU)',
      rows: [
        { feature: 'All BG modes (text / affine / bitmap)', support: 'yes' },
        { feature: 'Sprites: affine, mosaic, windows, blending', support: 'yes' },
        { feature: 'Golden-frame accuracy tests', support: 'yes' },
        { feature: 'GBA LCD color correction (higan/byuu)', support: 'yes', note: 'Optional toggle in the player' },
        { feature: 'LCD pixel-grid / CRT filter', support: 'yes' },
        { feature: 'High-resolution affine / Mode-7 upscaling', support: 'no', note: 'mGBA-style OpenGL enhancement' },
      ],
    },
    {
      group: 'Audio (APU)',
      rows: [
        { feature: 'PSG (square / wave / noise)', support: 'yes' },
        { feature: 'DirectSound (2 FIFO PCM channels)', support: 'yes' },
        { feature: 'High-quality "XQ" audio (Sappy HLE)', support: 'no', note: 'Cleaner music mixer; enhancement' },
      ],
    },
    {
      group: 'Saves & RTC',
      rows: [
        { feature: 'SRAM / Flash 64–128 KB / EEPROM autodetect', support: 'yes' },
        { feature: 'Cartridge RTC (Pokémon R/S/E time events)', support: 'yes', note: 'Host supplies the clock' },
        { feature: 'Save states + rewind', support: 'yes' },
        { feature: 'Raw .sav exchange with other emulators', support: 'testing', note: 'Persists locally; cross-emulator interchange unverified' },
      ],
    },
    {
      group: 'Sensors & rumble',
      rows: [
        { feature: 'Solar sensor (Boktai series)', support: 'no', note: 'Needs a GPIO ADC; GPIO is currently RTC-only' },
        { feature: 'Tilt / gyro (WarioWare: Twisted!, Yoshi UG, Koro Koro)', support: 'no' },
        { feature: 'Cartridge rumble (Drill Dozer, Pokémon Pinball)', support: 'no' },
        { feature: 'Game Boy Player rumble', support: 'no' },
      ],
    },
    {
      group: 'Connectivity',
      rows: [
        { feature: 'Link cable — multiplayer / trading', support: 'testing', note: 'SIO multiplay over WebRTC; needs broader real-game verification' },
        { feature: 'GameCube ↔ GBA link (JOY-bus)', support: 'no', note: 'Registers accepted but non-functional' },
        { feature: 'Single-Pak link (multiboot)', support: 'no' },
        { feature: 'Wireless Adapter', support: 'no' },
        { feature: 'Mobile Adapter GB', support: 'no' },
      ],
    },
    {
      group: 'Cartridge hardware & compatibility',
      rows: [
        { feature: 'Real (dumped) BIOS support', support: 'no', note: 'HLE BIOS only today' },
        { feature: 'GB/GBC enhanced backward-compat mode', support: 'no' },
        { feature: 'e-Reader (dot-code scanning)', support: 'no' },
        { feature: 'Exotic peripherals (Battle Chip Gate, Campho, Play-Yan, Glucoboy, Turbo File, Soul Doll)', support: 'no' },
        { feature: 'Commercial-game compatibility breadth', support: 'testing', note: 'Only ~5 titles verified so far' },
      ],
    },
  ],

  gbc: [
    {
      group: 'CPU & timing',
      rows: [
        { feature: 'SM83/LR35902 full instruction set', support: 'yes', note: 'Primary + CB ops, all flags' },
        { feature: 'Interrupt dispatch + IME gating', support: 'yes', note: 'VBlank/STAT/Timer/Serial/Joypad' },
        { feature: 'HALT / STOP + HALT bug', support: 'yes' },
        { feature: 'CGB double-speed (KEY1)', support: 'yes', note: 'STOP-triggered speed switch' },
        { feature: 'DIV/TIMA/TMA/TAC timer', support: 'yes', note: 'Falling-edge, reload delay modeled' },
        { feature: 'Illegal-opcode hard-lock detection', support: 'yes', note: 'Latches fault, draws crash screen' },
      ],
    },
    {
      group: 'Video / PPU',
      rows: [
        { feature: 'BG / window / sprite rendering', support: 'yes', note: 'Scanline renderer' },
        { feature: 'Mode FSM + STAT/LYC interrupts', support: 'yes', note: 'OAM/draw/HBlank/VBlank' },
        { feature: 'CGB color palettes + VRAM banking', support: 'yes', note: 'RGB555, BG attrs, OBJ banks' },
        { feature: 'DMG grayscale palettes', support: 'yes' },
        { feature: '10-sprites-per-line limit', support: 'yes' },
        { feature: 'Cycle-accurate pixel FIFO', support: 'partial', note: 'Fixed mode-3 length, scanline approx' },
        { feature: 'Color correction / oversaturation fix', support: 'no' },
      ],
    },
    {
      group: 'Audio / APU',
      rows: [
        { feature: '2 square channels + envelope', support: 'yes' },
        { feature: 'Channel 1 frequency sweep', support: 'yes' },
        { feature: 'Wave + noise (LFSR) channels', support: 'yes' },
        { feature: 'Frame sequencer (256/128/64 Hz)', support: 'yes' },
        { feature: 'NR50/NR51 volume + panning', support: 'yes', note: 'Stereo, master volume' },
      ],
    },
    {
      group: 'Cartridge mappers',
      rows: [
        { feature: 'No-MBC + MBC1', support: 'yes' },
        { feature: 'MBC2 (built-in 512×4 RAM)', support: 'yes' },
        { feature: 'MBC3 (banking)', support: 'yes' },
        { feature: 'MBC5 + rumble flag', support: 'partial', note: 'Rumble bit decoded, no host output' },
        { feature: 'MMM01 / MBC6 / MBC7', support: 'no', note: 'Fall back to No-MBC' },
        { feature: 'TAMA5 / HuC1 / HuC3 / Camera', support: 'no', note: 'Unmapped, No-MBC fallback' },
      ],
    },
    {
      group: 'Saves & RTC',
      rows: [
        { feature: 'Battery-backed cart RAM', support: 'yes', note: 'Dirty tracking, save/load' },
        { feature: 'MBC3 RTC registers + latch', support: 'partial', note: 'Latched but not advanced by real time' },
        { feature: 'RTC persistence to save file', support: 'no' },
        { feature: 'Save states / rewind', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: '8-button joypad matrix ($FF00)', support: 'yes', note: 'Active-low, joypad interrupt' },
        { feature: 'Rumble (MBC5)', support: 'no', note: 'Bit decoded only, no feedback' },
        { feature: "Tilt sensor (Kirby Tilt 'n' Tumble)", support: 'no' },
        { feature: 'Game Boy Camera / Printer', support: 'no' },
        { feature: 'Infrared (GBC IR port)', support: 'no' },
      ],
    },
    {
      group: 'Connectivity',
      rows: [
        { feature: 'Serial transfer (internal clock)', support: 'partial', note: 'Completes vs no peer, shifts in 0xFF' },
        { feature: 'Link cable — multiplayer', support: 'no', note: 'External-clock transfers never complete' },
        { feature: 'Super Game Boy (borders/palettes)', support: 'no' },
        { feature: 'Mobile Adapter GB / online', support: 'no' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'DMG + CGB in one core', support: 'yes', note: 'Selected from header flag' },
        { feature: 'OAM DMA ($FF46)', support: 'yes' },
        { feature: 'HDMA / GDMA VRAM DMA ($FF51–55)', support: 'yes' },
        { feature: 'Unit test coverage', support: 'yes', note: '~55 tests across subsystems' },
        { feature: 'Real-game / test-ROM verification', support: 'testing', note: 'No tested-games list yet' },
      ],
    },
  ],

  nes: [
    {
      group: 'CPU & timing',
      rows: [
        { feature: '151 official 6502 opcodes', support: 'yes' },
        { feature: 'Common unofficial opcodes', support: 'yes', note: 'LAX/SAX/DCP/ISC/SLO/RLA/SRE/RRA/NOP' },
        { feature: 'Page-cross / branch cycle penalties', support: 'yes' },
        { feature: 'NMI / IRQ / RESET sequences', support: 'yes' },
        { feature: 'JAM/KIL hard-halt detection', support: 'yes', note: 'Freezes to crash screen' },
        { feature: 'Cycle-accurate per-cycle bus', support: 'partial', note: 'Instruction-stepped, not sub-instruction' },
      ],
    },
    {
      group: 'Video / PPU',
      rows: [
        { feature: 'Loopy v/t/x/w scrolling', support: 'yes' },
        { feature: 'Background tile + attribute pipeline', support: 'yes' },
        { feature: 'Sprite eval: 8/line, sprite-0, overflow', support: 'yes' },
        { feature: '8×16 sprites', support: 'yes' },
        { feature: 'VBlank NMI + odd-frame dot skip', support: 'yes' },
        { feature: 'Canonical 64-color palette', support: 'yes' },
        { feature: 'Color emphasis / grayscale bits', support: 'no', note: 'PPUMASK emphasis ignored' },
      ],
    },
    {
      group: 'Audio / APU',
      rows: [
        { feature: '2 pulse channels (+ sweep)', support: 'yes' },
        { feature: 'Triangle channel', support: 'yes' },
        { feature: 'Noise channel', support: 'yes' },
        { feature: 'Envelope / length / sweep units', support: 'yes' },
        { feature: 'DMC sample channel', support: 'partial', note: 'Stub: level + enable only, no fetch' },
        { feature: 'Frame sequencer timing', support: 'partial', note: 'Coarse ~7457-cycle approximation' },
      ],
    },
    {
      group: 'Expansion audio',
      rows: [
        { feature: 'VRC6', support: 'no' },
        { feature: 'VRC7', support: 'no' },
        { feature: 'FDS audio', support: 'no' },
        { feature: 'MMC5 audio', support: 'no' },
        { feature: 'Namco 163', support: 'no' },
        { feature: 'Sunsoft 5B', support: 'no' },
      ],
    },
    {
      group: 'Mappers',
      rows: [
        { feature: 'iNES + NES 2.0 header parsing', support: 'yes' },
        { feature: 'Mapper 0 NROM', support: 'yes' },
        { feature: 'Mapper 1 MMC1', support: 'yes' },
        { feature: 'Mapper 2 UxROM', support: 'yes', note: 'Bus conflicts ignored' },
        { feature: 'Mapper 3 CNROM', support: 'yes' },
        { feature: 'Mapper 4 MMC3 + scanline IRQ', support: 'testing', note: 'A12-clocked IRQ, under-verified' },
        { feature: 'CHR-RAM boards', support: 'yes' },
        { feature: 'Other mappers (MMC5/AxROM/Namco/…)', support: 'no', note: 'Unsupported IDs rejected' },
      ],
    },
    {
      group: 'Saves',
      rows: [
        { feature: 'Battery flag detection', support: 'yes' },
        { feature: '8 KiB PRG-RAM at $6000–7FFF', support: 'yes' },
        { feature: 'Persisted battery saves', support: 'no', note: 'RAM present but not exported' },
        { feature: 'Save states', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: 'Standard controller (2 ports)', support: 'yes', note: '$4016/$4017 strobe/shift' },
        { feature: 'Zapper light gun', support: 'no' },
        { feature: 'R.O.B.', support: 'no' },
        { feature: 'Power Pad', support: 'no' },
        { feature: 'Famicom microphone', support: 'no' },
        { feature: 'Four Score (4-player)', support: 'no' },
      ],
    },
    {
      group: 'Region & compatibility',
      rows: [
        { feature: 'NTSC timing', support: 'yes', note: '3 dots/CPU cycle, 262 lines' },
        { feature: 'PAL timing', support: 'no' },
        { feature: 'Dendy timing', support: 'no' },
        { feature: 'FDS (Famicom Disk System)', support: 'no' },
        { feature: 'Four-screen mirroring', support: 'partial', note: 'Approximated as two tables' },
        { feature: 'Test-ROM / accuracy validation', support: 'partial', note: '29 unit tests; no game-compat suite' },
      ],
    },
  ],

  snes: [
    {
      group: 'CPU (65C816 / 5A22)',
      rows: [
        { feature: 'WDC 65C816 instruction set', support: 'yes', note: 'Full documented opcodes + addressing' },
        { feature: 'Emulation / native modes', support: 'yes', note: 'M/X flags, 8/16-bit A and index' },
        { feature: '24-bit banked addressing', support: 'yes' },
        { feature: 'Decimal (BCD) mode', support: 'yes' },
        { feature: 'RESET/NMI/IRQ/BRK/COP interrupts', support: 'yes' },
        { feature: 'Master-clock-exact cycle timing', support: 'partial', note: 'Memory-access-counted approximation' },
        { feature: 'DMA (8 channels)', support: 'yes', note: 'Full transfer runs immediately' },
        { feature: 'HDMA per-scanline', support: 'partial', note: 'Once per visible line; indirect supported' },
      ],
    },
    {
      group: 'Video / PPU (modes)',
      rows: [
        { feature: 'BG modes 0, 1, 3, 5, 7 tile path', support: 'yes', note: '2/4/8 bpp, 8×8 and 16×16 tiles' },
        { feature: 'Mode 7 affine transform', support: 'yes', note: 'Matrix M7A–D applied' },
        { feature: 'OBJ sprites with priority', support: 'yes' },
        { feature: 'Main / sub screen designation', support: 'yes' },
        { feature: 'Color math (add/sub)', support: 'partial', note: 'Simple average; not full sub-screen blend' },
        { feature: 'Windows (mask/logic)', support: 'no', note: 'Registers stubbed' },
        { feature: 'Mosaic', support: 'no' },
        { feature: 'Offset-per-tile (modes 2/4/6)', support: 'no' },
        { feature: 'Hi-res (modes 5/6) / interlace', support: 'no' },
        { feature: 'Per-scanline register changes', support: 'no', note: 'Whole-frame renderer' },
      ],
    },
    {
      group: 'Audio (SPC700 / S-DSP)',
      rows: [
        { feature: 'SPC700 CPU + 64 KiB ARAM', support: 'yes', note: 'Runs the real IPL boot ROM' },
        { feature: 'Mailbox handshake ($2140–43)', support: 'yes', note: 'Prevents boot-on-APU deadlock' },
        { feature: 'S-DSP BRR sample decode', support: 'partial', note: '8 voices, pitch honored' },
        { feature: 'KON / KOFF voice keying', support: 'yes' },
        { feature: 'ADSR / gain envelopes', support: 'partial', note: 'Simplified or omitted' },
        { feature: 'Gaussian interpolation', support: 'partial', note: 'Simplified' },
        { feature: 'Echo / FIR / noise', support: 'no' },
      ],
    },
    {
      group: 'Enhancement chips',
      rows: [
        { feature: 'Super FX / GSU', support: 'no' },
        { feature: 'SA-1', support: 'no' },
        { feature: 'DSP-1 / 2 / 3 / 4', support: 'no' },
        { feature: 'CX4', support: 'no' },
        { feature: 'S-DD1', support: 'no' },
        { feature: 'SPC7110 / OBC1', support: 'no' },
        { feature: 'ST010 / ST011 / ST018', support: 'no' },
        { feature: 'MSU-1', support: 'no' },
      ],
    },
    {
      group: 'Cartridge & saves',
      rows: [
        { feature: 'LoROM mapping', support: 'yes', note: 'Header auto-detect via scoring' },
        { feature: 'HiROM mapping', support: 'yes' },
        { feature: 'ExHiROM', support: 'partial', note: 'Treated as HiROM' },
        { feature: 'Copier (512-byte) header strip', support: 'yes' },
        { feature: 'Battery SRAM persistence', support: 'yes', note: 'Dirty-flag tracking' },
        { feature: 'Satellaview / BS-X', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: 'Standard 12-button controller', support: 'yes', note: 'Two ports' },
        { feature: 'Auto-joypad read ($4218–421F)', support: 'yes' },
        { feature: 'SNES Mouse', support: 'no' },
        { feature: 'Super Scope', support: 'no' },
        { feature: 'Multitap', support: 'no' },
        { feature: 'Super Game Boy', support: 'no' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Boots / plays many titles', support: 'yes', note: 'No deadlock on boot-on-APU games' },
        { feature: 'Unit test coverage', support: 'partial', note: '46 tests across subsystems' },
        { feature: 'Verified game library', support: 'testing', note: 'No catalogued tested-games list' },
        { feature: 'Audio fidelity vs hardware', support: 'partial', note: 'Plausible sound, not accurate' },
      ],
    },
  ],

  sms: [
    {
      group: 'CPU (Z80)',
      rows: [
        { feature: 'Main + shadow register sets', support: 'yes', note: "AF/BC/DE/HL + ', IX/IY, I/R" },
        { feature: 'Main + CB + ED opcode pages', support: 'yes' },
        { feature: 'DD/FD + DDCB/FDCB (IX/IY)', support: 'yes', note: 'Displaced bit ops' },
        { feature: 'Undocumented opcodes + X/Y flags', support: 'yes' },
        { feature: 'Interrupts NMI + IM 0/1/2, EI delay', support: 'yes' },
        { feature: 'M-cycle / T-state timing', support: 'yes', note: 'Per-instruction accurate' },
        { feature: 'Deadlock crash screen', support: 'yes', note: 'HALT-with-DI latches a fault' },
      ],
    },
    {
      group: 'Video / VDP',
      rows: [
        { feature: 'Mode 4 (SMS/GG mode)', support: 'yes', note: 'The mode all SMS/GG games use' },
        { feature: 'Legacy TMS9918 modes 0–3', support: 'no', note: 'SG-1000 modes not implemented' },
        { feature: 'Backgrounds, scroll, flip, priority', support: 'yes' },
        { feature: 'Sprites: 8/line, 8×8/8×16, zoom', support: 'yes' },
        { feature: 'Sprite overflow + collision flags', support: 'yes' },
        { feature: 'Line + frame interrupts', support: 'yes' },
        { feature: 'H counter read', support: 'partial', note: 'Stable mid-line value, not cycle-accurate' },
      ],
    },
    {
      group: 'Audio',
      rows: [
        { feature: 'SN76489 PSG: 3 tone + 1 noise', support: 'yes', note: '15-bit LFSR noise' },
        { feature: '4-bit per-channel volume', support: 'yes' },
        { feature: 'Game Gear stereo (port $06)', support: 'partial', note: 'Stereo byte stored; output mixed mono' },
        { feature: 'YM2413 FM (Japanese SMS)', support: 'no', note: 'FM Sound Unit not emulated' },
      ],
    },
    {
      group: 'Mappers & saves',
      rows: [
        { feature: 'Sega mapper + bank switching', support: 'yes', note: 'Fixed first 1 KiB, frame paging' },
        { feature: 'Codemasters mapper', support: 'yes', note: 'Auto-detected by checksum' },
        { feature: 'Korean mappers', support: 'no' },
        { feature: 'Copier header strip', support: 'yes' },
        { feature: 'On-cart battery RAM', support: 'yes', note: 'Up to 32 KiB, dirty-flag tracked' },
        { feature: 'Game Gear EEPROM saves', support: 'no' },
        { feature: 'Save states', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: 'Two digital control pads', support: 'yes', note: 'Ports $DC/$DD, active-low' },
        { feature: 'SMS Pause → NMI', support: 'yes', note: 'Edge-detected' },
        { feature: 'Game Gear Start (port $00)', support: 'yes' },
        { feature: 'Light Phaser', support: 'no' },
        { feature: 'Paddle Control', support: 'no' },
        { feature: 'SegaScope 3-D Glasses', support: 'no' },
        { feature: 'Gear-to-Gear link cable', support: 'no' },
      ],
    },
    {
      group: 'Region & timing',
      rows: [
        { feature: 'NTSC 262-line timing', support: 'yes', note: '60 Hz, 228 cycles/line' },
        { feature: 'PAL 50 Hz timing', support: 'no' },
        { feature: 'Region detect / lock', support: 'no', note: 'GG region bit not enforced' },
      ],
    },
    {
      group: 'Systems & compatibility',
      rows: [
        { feature: 'Master System (256×192)', support: 'yes' },
        { feature: 'Game Gear (160×144 crop)', support: 'yes', note: 'One core serves both' },
        { feature: 'GG 12-bit / SMS 6-bit palette', support: 'yes' },
        { feature: 'Color correction / LCD ghosting', support: 'no', note: 'Raw nibble-replicated RGB' },
        { feature: 'Verified against game library', support: 'testing', note: '53 unit tests; no game corpus yet' },
      ],
    },
  ],

  genesis: [
    {
      group: 'CPU (68000 / Z80)',
      rows: [
        { feature: 'Motorola 68000 instruction set', support: 'yes', note: 'Common opcodes, 12 addressing modes' },
        { feature: '68000 exception vectoring', support: 'yes', note: 'Traps, illegal, privilege' },
        { feature: 'Autovectored level-4/6 interrupts', support: 'yes', note: 'VDP H-int / V-int wired' },
        { feature: 'BCD (ABCD/SBCD) and CHK ops', support: 'no', note: 'Stubbed' },
        { feature: 'Address/odd-access error exceptions', support: 'no', note: 'Not raised' },
        { feature: 'Z80 sub-CPU (all opcode pages)', support: 'yes', note: 'IM 0/1/2, M-cycle timing' },
        { feature: 'Z80 bus request / reset, bank switch', support: 'yes' },
        { feature: 'Cycle-accurate CPU sync', support: 'partial', note: 'Approximate, not slot-exact' },
      ],
    },
    {
      group: 'Video / VDP (315-5313)',
      rows: [
        { feature: 'Planes A/B with scroll + priority', support: 'yes' },
        { feature: 'Window plane', support: 'partial', note: 'Basic region override only' },
        { feature: 'Sprites (80-entry linked list)', support: 'yes' },
        { feature: 'H40 (320) / H32 (256) modes', support: 'yes', note: '224 lines' },
        { feature: 'DMA: mem→VRAM, fill, copy', support: 'yes' },
        { feature: 'Shadow / highlight mode', support: 'no' },
        { feature: 'Interlace modes', support: 'no' },
        { feature: 'Mid-frame raster effects', support: 'no', note: 'Per-frame rendering blocks them' },
      ],
    },
    {
      group: 'Audio (YM2612 / PSG)',
      rows: [
        { feature: 'YM2612 register interface', support: 'yes', note: 'Complete latching, all banks' },
        { feature: 'YM2612 FM synthesis', support: 'partial', note: 'Placeholder sine per channel' },
        { feature: '4-operator envelope / algorithm', support: 'no', note: 'Stubbed' },
        { feature: 'LFO (vibrato / tremolo)', support: 'no' },
        { feature: 'SN76489 PSG', support: 'yes', note: 'Tone + noise channels' },
        { feature: 'Mono / stereo output @ 44.1 kHz', support: 'yes' },
      ],
    },
    {
      group: 'Add-ons & mappers',
      rows: [
        { feature: 'Sega CD / Mega CD', support: 'no' },
        { feature: '32X', support: 'no' },
        { feature: 'SVP (Virtua Racing)', support: 'no' },
        { feature: 'Bank-switch mappers (SSF2)', support: 'no', note: 'Stubbed; plain ROM only' },
        { feature: 'Lock-on / passthrough carts', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: '3-button controller', support: 'yes' },
        { feature: '6-button controller', support: 'yes', note: 'TH-toggle 4-phase sequence' },
        { feature: 'Two players', support: 'yes' },
        { feature: 'Menacer light gun', support: 'no' },
        { feature: 'Multitap (4-player)', support: 'no' },
        { feature: 'Sega Mouse', support: 'no' },
      ],
    },
    {
      group: 'Saves & region',
      rows: [
        { feature: 'Battery-backed SRAM', support: 'yes', note: 'Header window parsed, dirty flag' },
        { feature: 'EEPROM saves', support: 'no' },
        { feature: 'Region / PAL selection', support: 'no', note: 'Version register fixed to overseas' },
        { feature: 'PAL 50 Hz timing', support: 'no', note: 'NTSC timing only' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'ROM formats (.md/.gen/.smd)', support: 'yes' },
        { feature: 'Title screens / simple gameplay', support: 'testing', note: 'Runs; varies per game' },
        { feature: 'Automated test suite', support: 'yes', note: '85 tests across subsystems' },
        { feature: 'Raster-heavy / commercial accuracy', support: 'testing', note: 'Under-verified vs library' },
      ],
    },
  ],

  pce: [
    {
      group: 'CPU (HuC6280)',
      rows: [
        { feature: '65C02 instruction set + CMOS ext.', support: 'yes', note: 'BRA, STZ, TSB/TRB, RMB/SMB, BBR/BBS' },
        { feature: 'Banking MMU (8 MPRs, TAM/TMA)', support: 'yes', note: '16-bit logical → 21-bit physical' },
        { feature: 'Block transfers (TII/TDD/TIA/TAI/TIN)', support: 'yes' },
        { feature: 'ST0/ST1/ST2 fast VDC writes', support: 'yes' },
        { feature: 'CSL/CSH speed switching', support: 'partial', note: 'Flag tracked; cycle scaling approximate' },
        { feature: 'Decimal mode (ADC/SBC)', support: 'yes' },
        { feature: 'IRQ1/TIQ/IRQ2 + timer', support: 'yes', note: 'IRQ2 line present but unused' },
      ],
    },
    {
      group: 'Video (HuC6270 VDC / VCE)',
      rows: [
        { feature: '64 KiB VRAM + register file', support: 'yes' },
        { feature: 'Background tilemap + scrolling', support: 'yes', note: 'Virtual map up to 128×64' },
        { feature: 'Sprite rendering (64 sprites)', support: 'yes', note: 'Variable size, flip, priority' },
        { feature: 'Sprite-0 collision + overflow', support: 'partial', note: '>16/line cap approximate' },
        { feature: 'VBlank + raster (RCR) interrupts', support: 'yes' },
        { feature: 'SATB DMA', support: 'no', note: 'Address stored; table read in place' },
        { feature: 'VRAM→VRAM DMA', support: 'no', note: 'Registers accepted, no transfer' },
        { feature: '512-entry 9-bit GRB palette', support: 'yes' },
        { feature: 'Dot-clock width select', support: 'partial', note: 'Only 256-wide path rendered' },
      ],
    },
    {
      group: 'Audio (PSG)',
      rows: [
        { feature: '6 wavetable channels', support: 'yes', note: '32-byte 5-bit waveforms' },
        { feature: 'Per-channel freq/volume/balance', support: 'yes' },
        { feature: 'DDA direct mode', support: 'yes' },
        { feature: 'Noise channels (4–5)', support: 'partial', note: 'LFSR approximate spectrum' },
        { feature: 'LFO modulation', support: 'no', note: 'Registers stored, not synthesised' },
      ],
    },
    {
      group: 'CD & expansion',
      rows: [
        { feature: 'CD-ROM² / System Card', support: 'no' },
        { feature: 'Super CD-ROM²', support: 'no' },
        { feature: 'Arcade Card', support: 'no' },
        { feature: 'SuperGrafx (dual VDC + VPC)', support: 'no' },
        { feature: 'ADPCM / CD audio', support: 'no' },
      ],
    },
    {
      group: 'Input & peripherals',
      rows: [
        { feature: 'Standard pad (SEL/CLR 2-bit)', support: 'yes', note: 'D-pad, I/II, Select, Run' },
        { feature: 'Avenue Pad 6 / 6-button', support: 'no' },
        { feature: 'Multitap / TurboTap (5-player)', support: 'no', note: 'Single port only' },
      ],
    },
    {
      group: 'Cartridges & saves',
      rows: [
        { feature: 'HuCard loading + header strip', support: 'yes' },
        { feature: '384 KiB split-mirror layout', support: 'yes', note: 'Populous-style bank map' },
        { feature: 'SF2 2.5 MB mapper', support: 'no', note: 'No bank-switch mapper' },
        { feature: 'Backup RAM / Tennokoe saves', support: 'no', note: 'Recognized but not stored' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Region (JP PCE / US TG-16)', support: 'partial', note: 'Country bit fixed to Japanese' },
        { feature: 'Boots + renders title screens', support: 'yes' },
        { feature: 'Cycle/timing accuracy', support: 'partial', note: 'Scanline-stepped, not dot-accurate' },
        { feature: 'Game compatibility coverage', support: 'testing', note: 'No curated tested-games list' },
        { feature: 'Unit test suite', support: 'yes', note: '61 cargo tests across subsystems' },
      ],
    },
  ],

  nds: [
    {
      group: 'CPUs',
      rows: [
        { feature: 'ARM9 (ARMv5TE)', support: 'yes', note: 'Full ARM/Thumb decode + v5 extensions' },
        { feature: 'ARM7 (ARMv4T)', support: 'yes' },
        { feature: 'CP15 coprocessor (TCM/cache/MPU)', support: 'yes' },
        { feature: 'Banked regs + 7-mode exceptions', support: 'yes' },
      ],
    },
    {
      group: '2D engines',
      rows: [
        { feature: 'Dual engines (A main + B sub)', support: 'yes', note: 'Two 256×192 framebuffers' },
        { feature: 'Text/tile backgrounds', support: 'yes', note: '4/8 bpp, extended palettes, mosaic' },
        { feature: 'Affine / rot-scale backgrounds', support: 'yes' },
        { feature: 'Extended & large bitmap modes', support: 'yes' },
        { feature: 'Sprites (affine, bitmap, ext palette)', support: 'yes' },
        { feature: 'Windows + blending + brightness', support: 'yes' },
        { feature: 'Display capture (DISPCAPCNT)', support: 'no', note: 'Register present, logic stubbed' },
        { feature: 'Main-memory display mode', support: 'no' },
      ],
    },
    {
      group: '3D GPU',
      rows: [
        { feature: 'Geometry / T&L (matrix stacks)', support: 'yes' },
        { feature: 'Per-vertex Gouraud lighting', support: 'yes', note: '4 lights; no specular' },
        { feature: 'Polygon rasterization (filled)', support: 'testing', note: 'Perspective-correct triangles/quads' },
        { feature: 'Wireframe polygons', support: 'no', note: 'Falls back to solid fill' },
        { feature: 'Textures (all 7 formats + CLUT)', support: 'partial', note: 'Point-sampled, no bilinear' },
        { feature: 'Texture blend modes', support: 'partial', note: 'Modulation only; no decal/toon' },
        { feature: 'Fog / edge marking', support: 'partial', note: 'Approximate; no polygon IDs' },
        { feature: 'Anti-aliasing', support: 'no' },
        { feature: 'Hi-res / upscale rendering', support: 'no', note: 'Native 256×192 only' },
      ],
    },
    {
      group: 'Audio',
      rows: [
        { feature: '16-channel mixer', support: 'yes', note: 'Per-channel volume/pan' },
        { feature: 'PCM8 / PCM16', support: 'yes' },
        { feature: 'IMA-ADPCM', support: 'yes' },
        { feature: 'PSG square / noise', support: 'no', note: 'Recognized but emits silence' },
        { feature: 'Sound capture units', support: 'no' },
      ],
    },
    {
      group: 'Input (keypad/touch/mic)',
      rows: [
        { feature: 'Keypad buttons (+ X/Y/lid)', support: 'yes' },
        { feature: 'Touchscreen', support: 'yes', note: 'TSC2046 ADC, calibration, pressure' },
        { feature: 'Microphone', support: 'partial', note: 'Host-injectable; no real capture' },
        { feature: 'Rumble / motion sensors', support: 'no' },
      ],
    },
    {
      group: 'Saves & cart',
      rows: [
        { feature: 'ROM header + binary loading', support: 'yes', note: 'ARM9/ARM7 relocation, overlays' },
        { feature: 'Cart command protocol', support: 'partial', note: 'Key1/Key2 state machine; no real Blowfish' },
        { feature: 'FLASH save chip (AUXSPI)', support: 'yes', note: 'Read/write/status, dirty-flag persist' },
        { feature: 'EEPROM saves', support: 'partial', note: 'Per-game address-size table' },
        { feature: 'NAND / SRAM saves', support: 'no' },
        { feature: 'RTC', support: 'yes', note: 'S3511 BCD date/time, host-injectable' },
        { feature: 'BIOS HLE boot', support: 'yes', note: 'Direct boot, SWI HLE, no real BIOS' },
      ],
    },
    {
      group: 'Connectivity & DSi',
      rows: [
        { feature: 'Wi-Fi / wireless', support: 'no', note: 'MMIO stub returns 0' },
        { feature: 'Local multiplayer / Download Play', support: 'no' },
        { feature: 'DSi enhancements', support: 'no', note: 'Unit code detected, not handled' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'DMA / timers / IRQ / IPC', support: 'yes', note: '4 DMA/core, 8 timing modes' },
        { feature: 'Anti-piracy (Key1) titles', support: 'no', note: 'Pokémon-class checks fail to boot' },
        { feature: '2D-centric games', support: 'yes' },
        { feature: '3D games', support: 'testing', note: 'Rasterizer works; features approximate' },
        { feature: 'Test coverage', support: 'yes', note: '365 unit tests across subsystems' },
      ],
    },
  ],

  ps1: [
    {
      group: 'CPU & GTE',
      rows: [
        { feature: 'MIPS R3000A interpreter', support: 'yes', note: 'All SPECIAL/REGIMM opcode classes' },
        { feature: 'Branch + load delay slots', support: 'yes' },
        { feature: 'Exceptions & COP0 system control', support: 'yes', note: 'Vectors, SR/CAUSE/EPC, RFE' },
        { feature: 'Cache isolation (i-cache boot trick)', support: 'yes' },
        { feature: 'GTE (COP2) geometry engine', support: 'yes', note: 'RTPS/RTPT, MVMVA, NCDS, AVSZ' },
        { feature: 'Cycle-accurate timing', support: 'no', note: 'One cycle per instruction' },
      ],
    },
    {
      group: 'GPU',
      rows: [
        { feature: 'Software rasterizer over 1 MB VRAM', support: 'yes', note: '1024×512 16bpp' },
        { feature: 'Gouraud triangles / quads', support: 'yes' },
        { feature: 'Texture mapping + CLUT (4/8/15bpp)', support: 'yes' },
        { feature: 'Semi-transparency / blend modes', support: 'yes' },
        { feature: 'Drawing-area clip + draw offset', support: 'yes' },
        { feature: '24bpp display + interlace fields', support: 'partial', note: 'Basic interlace' },
        { feature: 'Dithering', support: 'no', note: 'Not applied to 555 output' },
        { feature: 'Perspective-correct textures (PGXP)', support: 'no' },
        { feature: 'Internal-resolution upscaling', support: 'no', note: 'Native resolution only' },
        { feature: 'Texture filtering', support: 'no', note: 'Nearest-neighbour only' },
      ],
    },
    {
      group: 'Audio / SPU',
      rows: [
        { feature: '24 ADPCM voices + mixing', support: 'yes' },
        { feature: 'ADSR envelopes', support: 'yes' },
        { feature: '4-point Gaussian pitch interpolation', support: 'yes' },
        { feature: 'f32 stereo @ 44.1 kHz output', support: 'yes' },
        { feature: 'Reverb', support: 'partial', note: 'Single feedback echo, not APF/comb network' },
        { feature: 'CD-DA audio playback', support: 'no', note: 'Data reads only' },
        { feature: 'XA-ADPCM streaming audio', support: 'no' },
      ],
    },
    {
      group: 'CD-ROM',
      rows: [
        { feature: 'Command/response IRQ engine', support: 'yes', note: 'INT1..INT5 sequencing' },
        { feature: 'Data sector reads (ReadN/ReadS)', support: 'yes', note: 'MODE2/2352, 1× + 2×' },
        { feature: 'Raw .bin (MODE2/2352) image', support: 'yes' },
        { feature: 'cue / iso / img mounting', support: 'partial', note: 'Core mounts raw bin; parsing host-side' },
        { feature: 'PBP / CHD containers', support: 'no' },
        { feature: 'Multi-disc / disc swap', support: 'no', note: 'Single mounted image' },
      ],
    },
    {
      group: 'Other devices',
      rows: [
        { feature: 'DMA — all 7 channels', support: 'yes', note: 'Burst/slice/linked-list, OTC clear' },
        { feature: 'MDEC FMV decoder', support: 'yes', note: 'Run-level, IDCT, YUV→RGB' },
        { feature: 'Root counters / timers', support: 'partial', note: 'Dot/HBLANK/VBLANK approximate' },
        { feature: 'Interrupt controller (I_STAT/I_MASK)', support: 'yes' },
      ],
    },
    {
      group: 'Controllers & memory cards',
      rows: [
        { feature: 'Digital pad over SIO0', support: 'yes', note: 'Active-low + /ACK IRQ' },
        { feature: 'Analog / DualShock', support: 'no' },
        { feature: 'Multitap', support: 'no' },
        { feature: 'Mouse / lightgun / Guncon', support: 'no' },
        { feature: 'Memory-card detect', support: 'partial', note: 'Probe returns high-Z (clean fail)' },
        { feature: 'Memory-card save/load', support: 'no' },
        { feature: 'SIO1 link cable', support: 'no' },
      ],
    },
    {
      group: 'BIOS, boot & testing',
      rows: [
        { feature: 'Real BIOS execution', support: 'yes', note: 'BIOS-gated; ships OpenBIOS' },
        { feature: 'HLE BIOS kernel calls', support: 'no', note: 'A/B/C vectors fall through to ROM' },
        { feature: 'ISO9660 disc-boot parsing', support: 'yes', note: 'SYSTEM.CNF → EXE fallback' },
        { feature: 'PS-X EXE sideload', support: 'yes' },
        { feature: 'Unit test coverage', support: 'yes', note: '~144 tests across subsystems' },
        { feature: 'Game compatibility', support: 'testing', note: 'No curated tested-games list yet' },
        { feature: 'Save states', support: 'no' },
      ],
    },
  ],

  n64: [
    {
      group: 'CPU (VR4300 / COP0 / COP1)',
      rows: [
        { feature: 'MIPS III integer ISA', support: 'yes', note: '200+ instructions, 64-bit GPRs' },
        { feature: 'Branch delay slots', support: 'yes' },
        { feature: 'COP0 exception model', support: 'yes', note: '16 cause codes, Cause/Status/EPC, ERET' },
        { feature: 'Count/Compare timer interrupt', support: 'yes' },
        { feature: '32-entry TLB', support: 'partial', note: 'Only KSEG0/KSEG1 translated' },
        { feature: 'COP1 single/double FPU', support: 'partial', note: 'Arithmetic, CVT, compare' },
        { feature: 'IEEE rounding modes & exception flags', support: 'no', note: 'Host round-to-nearest only' },
        { feature: 'Cycle-accurate timing', support: 'no', note: 'Instruction-deterministic, CPI=1' },
      ],
    },
    {
      group: 'RSP (signal processor)',
      rows: [
        { feature: 'SP register block & status', support: 'yes' },
        { feature: 'DMEM/IMEM + SP DMA', support: 'yes', note: 'RDRAM ↔ DMEM/IMEM' },
        { feature: 'Scalar (SU) instruction execution', support: 'no', note: 'RSP stays halted' },
        { feature: 'Vector (VU) unit', support: 'no' },
        { feature: 'Graphics/audio microcode (HLE or LLE)', support: 'no' },
      ],
    },
    {
      group: 'RDP / graphics',
      rows: [
        { feature: 'DPC command register block', support: 'partial', note: 'Accepts pointers, consumes as no-op' },
        { feature: 'Triangle / rectangle rasterization', support: 'no' },
        { feature: 'Color combiner / blender / Z-buffer', support: 'no' },
        { feature: 'Texturing & bilinear filtering', support: 'no' },
        { feature: 'Internal-resolution upscaling', support: 'no' },
        { feature: 'VI framebuffer scanout', support: 'partial', note: 'CPU-filled framebuffers visible; 320×240' },
        { feature: 'Vertical interrupt per frame', support: 'yes' },
      ],
    },
    {
      group: 'Audio',
      rows: [
        { feature: 'AI register block & DMA config', support: 'yes' },
        { feature: 'PCM sample streaming to host', support: 'no', note: 'Drains empty buffer' },
        { feature: 'Audio microcode (HLE/LLE)', support: 'no' },
      ],
    },
    {
      group: 'Boot & cart',
      rows: [
        { feature: 'ROM byte-order detection (.z64/.n64/.v64)', support: 'yes' },
        { feature: 'HLE IPL3 boot', support: 'partial', note: 'CIC 6102 register state only' },
        { feature: 'Other CIC variants', support: 'no' },
        { feature: 'PI cartridge DMA', support: 'yes', note: 'Instantaneous timing' },
        { feature: '8 MB RDRAM / Expansion Pak', support: 'partial', note: 'Always allocated, reported via RI' },
        { feature: 'Region / TV-type detection', support: 'no', note: 'Hardcoded NTSC' },
        { feature: '64DD disk drive', support: 'no' },
      ],
    },
    {
      group: 'Saves & input',
      rows: [
        { feature: 'SRAM / EEPROM / FlashRAM saves', support: 'no' },
        { feature: 'Controller Pak (memory card)', support: 'no', note: 'Joybus reports pak, no storage' },
        { feature: 'PIF joybus protocol', support: 'yes' },
        { feature: 'One controller (channel 0)', support: 'yes', note: 'Buttons + analog stick' },
        { feature: 'Controllers on channels 1–3', support: 'no' },
        { feature: 'Rumble Pak / Transfer Pak', support: 'no' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Commercial games rendering', support: 'no', note: 'Needs RSP/RDP' },
        { feature: 'Unit-test coverage', support: 'yes', note: '69 tests: CPU, COP0/1, DMA, boot, joybus' },
        { feature: 'CPU-framebuffer homebrew display', support: 'testing', note: 'Visible but unverified' },
        { feature: 'Plugin / HLE-LLE selection', support: 'no', note: 'Single integrated core' },
      ],
    },
  ],

  xbox: [
    {
      group: 'CPU (IA-32)',
      rows: [
        { feature: 'Real-mode boot at reset vector', support: 'yes', note: 'Powers on 16-bit, CS:IP F000:FFF0' },
        { feature: 'Protected-mode (32-bit) execution', support: 'yes', note: 'Flat model; CR0.PE honored' },
        { feature: 'Integer ALU / MOV / shifts / LEA', support: 'yes' },
        { feature: 'Control flow (JMP/Jcc/CALL/RET/LOOP)', support: 'yes', note: 'All 16 condition codes' },
        { feature: 'String ops + REP', support: 'yes', note: 'MOVS/STOS/SCAS/CMPS' },
        { feature: 'MUL / DIV / IDIV', support: 'yes' },
        { feature: 'x87 FPU floating point', support: 'partial', note: 'f64-approx stack; common ESC opcodes' },
        { feature: 'SSE / SSE2 (XMM)', support: 'partial', note: 'Packed/scalar float' },
        { feature: 'MMX', support: 'no', note: 'Raise #UD' },
        { feature: 'Paging / MMU / TLB', support: 'no', note: 'All addresses physical' },
        { feature: 'Interrupts / IDT (INT/IRET)', support: 'no', note: 'Faults recorded; no vectoring' },
      ],
    },
    {
      group: 'NV2A GPU',
      rows: [
        { feature: 'PFIFO pushbuffer DMA pusher', support: 'yes', note: 'Header parse + DMA_PUT kick/drain' },
        { feature: 'CLEAR_SURFACE (color/depth)', support: 'yes', note: 'Honors clip + clear rect' },
        { feature: 'Surface setup / color formats', support: 'partial', note: 'ARGB8888 only' },
        { feature: 'Transform T&L (MVP / viewport)', support: 'yes', note: 'model·view·proj + perspective divide' },
        { feature: 'Vertex submission (arrays / element16 / immediate)', support: 'partial', note: 'No INLINE_ARRAY / ELEMENT32' },
        { feature: 'Primitives (tri / strip / fan / quads)', support: 'partial', note: 'No lines or points' },
        { feature: 'Triangle rasterizer (Gouraud)', support: 'yes', note: 'Barycentric, top-left fill rule' },
        { feature: 'Depth buffer / Z-test', support: 'partial', note: 'Less-equal path; under-verified' },
        { feature: 'Textures (ARGB linear + swizzle)', support: 'partial', note: 'No DXT / palettized' },
        { feature: 'Alpha blending / back-face culling', support: 'no' },
        { feature: 'Register combiners (pixel shaders)', support: 'no' },
        { feature: 'Vertex shaders (program microcode)', support: 'no', note: 'Constants stored, not executed' },
        { feature: 'Fixed-function lighting', support: 'no' },
      ],
    },
    {
      group: 'Display / scanout',
      rows: [
        { feature: 'PCRTC framebuffer present', support: 'partial', note: 'Reads PCRTC_START; ARGB blit' },
        { feature: 'VBlank interrupt', support: 'yes', note: '60 Hz raise' },
        { feature: 'Flip methods / FLIP_STALL sync', support: 'partial', note: 'Flip select; no stall wait' },
      ],
    },
    {
      group: 'Kernel / BIOS',
      rows: [
        { feature: 'HLE kernel exports (~73)', support: 'partial', note: 'Mm/Nt/Ke/Rtl/Av/Hal families' },
        { feature: 'Memory allocation', support: 'yes', note: 'Bump-heap allocator' },
        { feature: 'SHA-1 / interlocked / IRQL / time', support: 'yes' },
        { feature: 'Threads / events / wait objects', support: 'partial', note: 'Single-thread cooperative; mostly no-ops' },
        { feature: 'Encrypted MCPX/2BL boot chain', support: 'no', note: 'HLE-only; XBE loaded directly' },
      ],
    },
    {
      group: 'MCPX / I-O',
      rows: [
        { feature: 'USB controller / gamepad input', support: 'no', note: 'set_keys latches bits, unused' },
        { feature: 'IDE HDD / DVD drive', support: 'no', note: 'HDD stubbed empty' },
        { feature: 'SMBus', support: 'no' },
        { feature: 'APU / AC97 audio', support: 'no', note: 'drain_audio always empty' },
        { feature: 'Networking (Xbox Live / system link)', support: 'no' },
        { feature: 'Save / FATX HDD filesystem', support: 'no' },
      ],
    },
    {
      group: 'Media (XBE / XISO)',
      rows: [
        { feature: 'XBE parse + section load', support: 'yes', note: 'Header, sections, RAM image' },
        { feature: 'XBE entry/thunk de-obfuscation', support: 'yes', note: 'XOR keys; import-thunk patch' },
        { feature: 'XISO / XDVDFS mount + file read', support: 'yes', note: 'Volume + directory walk' },
        { feature: '64 MB RAM + MMIO/flash bus routing', support: 'yes' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'nxdk hello.xbe (CRT + kernel imports)', support: 'testing', note: 'Boots; stalls on unhandled export' },
        { feature: 'nxdk triangle.xbe (NV2A pushbuffer)', support: 'testing', note: '~237 methods; triangle drawn' },
        { feature: 'Commercial retail games', support: 'no', note: 'Needs kernel/MCPX/APU/shaders' },
        { feature: 'Cargo unit tests (CPU/NV2A/XBE/XISO/HLE)', support: 'yes' },
      ],
    },
  ],

  atari2600: [
    {
      group: 'CPU (6507)',
      rows: [
        { feature: 'Official 6502 opcode set', support: 'yes', note: 'Correct flags + cycle counts' },
        { feature: 'Common unofficial opcodes', support: 'yes', note: 'LAX/SAX/DCP/ISC/SLO/RLA/SRE/RRA/NOP' },
        { feature: 'Cycle accuracy (page-cross/branch)', support: 'yes' },
        { feature: 'BCD decimal mode (ADC/SBC)', support: 'yes' },
        { feature: '13-bit address bus mirroring', support: 'yes' },
        { feature: 'JAM/KIL illegal-opcode detection', support: 'yes', note: 'Latches fault + crash screen' },
      ],
    },
    {
      group: 'Video (TIA)',
      rows: [
        { feature: 'Beam-racing pixel renderer', support: 'yes', note: '1 CPU cycle = 3 color clocks' },
        { feature: 'Players P0/P1 + NUSIZ copies/size', support: 'yes' },
        { feature: 'Missiles M0/M1 + ball + sizing', support: 'yes' },
        { feature: '20-bit playfield (reflect + score)', support: 'yes' },
        { feature: 'Object priority (PF/BL promote)', support: 'yes' },
        { feature: 'RESPx positioning', support: 'partial', note: 'Fixed pipeline delay, not exact' },
        { feature: 'HMOVE fine motion + left-edge comb', support: 'partial', note: 'Late-HMOVE quirks simplified' },
        { feature: 'VDEL, REFPx, RESMP', support: 'yes' },
        { feature: 'All 15 collision latches', support: 'yes' },
        { feature: 'VSYNC/VBLANK/WSYNC/RSYNC', support: 'yes' },
      ],
    },
    {
      group: 'Audio (TIA)',
      rows: [
        { feature: '2 independent channels', support: 'yes' },
        { feature: 'AUDC waveforms (tones / noise)', support: 'partial', note: 'All modes, not bit-exact' },
        { feature: 'AUDF frequency + AUDV volume', support: 'yes' },
        { feature: 'Host resampling to 44.1 kHz', support: 'partial', note: 'Approximate clocking' },
      ],
    },
    {
      group: 'RIOT (6532)',
      rows: [
        { feature: '128 bytes RAM + stack mirror', support: 'yes' },
        { feature: 'Interval timer (1/8/64/1024)', support: 'yes' },
        { feature: 'Timer underflow + INSTAT flag', support: 'yes' },
        { feature: 'SWCHA/SWCHB I/O ports', support: 'yes' },
        { feature: 'Port data-direction (SWACNT/SWBCNT)', support: 'no', note: 'Ports modeled as pure inputs' },
      ],
    },
    {
      group: 'Bankswitching',
      rows: [
        { feature: '2K/4K flat ROM', support: 'yes', note: 'Size-detected, 2K mirrored' },
        { feature: 'F8 (8K) / F6 (16K) / F4 (32K)', support: 'yes' },
        { feature: 'SuperChip extra RAM', support: 'no' },
        { feature: 'E0 / FE / 3F / F0 schemes', support: 'no' },
        { feature: 'CV / UA / E7 / 3E schemes', support: 'no' },
        { feature: 'DPC (Pitfall II)', support: 'no' },
        { feature: 'Supercharger (cassette)', support: 'no' },
      ],
    },
    {
      group: 'Controllers',
      rows: [
        { feature: 'Joystick (4-way + fire), both players', support: 'yes', note: 'SWCHA + INPT4/5' },
        { feature: 'Console Reset/Select switches', support: 'yes', note: 'Via SWCHB' },
        { feature: 'Difficulty / Color-BW switches', support: 'partial', note: 'Fixed at released defaults' },
        { feature: 'Paddles', support: 'no' },
        { feature: 'Driving controller', support: 'no' },
        { feature: 'Keypad / Genesis pad / light gun', support: 'no' },
        { feature: 'AtariVox / SaveKey', support: 'no' },
      ],
    },
    {
      group: 'TV standards & testing',
      rows: [
        { feature: 'NTSC timing (262 lines)', support: 'yes' },
        { feature: 'NTSC color palette', support: 'yes', note: '128-entry palette' },
        { feature: 'PAL timing + palette', support: 'no' },
        { feature: 'SECAM palette', support: 'no' },
        { feature: 'Save states / battery saves', support: 'no' },
        { feature: 'Unit test coverage', support: 'yes', note: '52 cargo tests across subsystems' },
        { feature: 'Verified against real game library', support: 'testing', note: 'No tested-games list yet' },
      ],
    },
  ],

  ngpc: [
    {
      group: 'CPU (TLCS-900/H)',
      rows: [
        { feature: 'Register banks + flags', support: 'yes', note: 'Banked file via RFP, full SR flags' },
        { feature: 'Variable-length instruction decode', support: 'yes' },
        { feature: 'Arithmetic / logic / shifts', support: 'yes', note: 'Byte/word/long with full flags' },
        { feature: 'MUL / DIV / signed variants', support: 'yes' },
        { feature: 'Branches (JP/JR/CALL/RET/DJNZ)', support: 'yes' },
        { feature: 'Interrupts with ILM masking', support: 'yes' },
        { feature: 'Block transfer (LDIR/LDDR)', support: 'no', note: 'Stubbed; flags illegal opcode' },
        { feature: 'Internal timers / prescalers', support: 'no', note: 'Not modeled' },
        { feature: 'Cycle accuracy', support: 'partial', note: 'Approximate fixed per-op counts' },
      ],
    },
    {
      group: 'Sound CPU (Z80)',
      rows: [
        { feature: 'Z80 interpreter present', support: 'yes', note: 'Full core ported from SMS' },
        { feature: 'Clocked in frame loop', support: 'no', note: 'Never stepped — audio disabled' },
        { feature: 'Shared sound RAM', support: 'partial', note: '4 KiB mapped, but Z80 idle' },
        { feature: 'Main-CPU / Z80 comm latch', support: 'partial', note: 'Latched; consumer not running' },
      ],
    },
    {
      group: 'Video (K1GE/K2GE)',
      rows: [
        { feature: '160×152 framebuffer', support: 'yes' },
        { feature: 'Two scroll planes + priority', support: 'yes' },
        { feature: '64 sprites (chain, tile flip)', support: 'yes' },
        { feature: '12-bit RGB palette to RGBA', support: 'yes', note: 'Plus NEG invert bit' },
        { feature: 'K2GE per-sprite palette select', support: 'yes' },
        { feature: 'Mono NGP vs color NGPC', support: 'yes', note: 'Header byte selects at load' },
        { feature: 'V-blank interrupt', support: 'yes' },
        { feature: 'H-blank / line interrupt', support: 'partial', note: 'Latched but not wired to CPU' },
        { feature: 'Raster / line-compare effects', support: 'no' },
      ],
    },
    {
      group: 'Audio (PSG)',
      rows: [
        { feature: 'T6W28 dual PSG', support: 'partial', note: '3 tone + noise, approximate' },
        { feature: 'Stereo L/R attenuation', support: 'partial', note: 'Mixed down to mono' },
        { feature: 'Noise LFSR', support: 'yes', note: '15-bit, white/periodic' },
        { feature: '8-bit DAC ports', support: 'yes' },
        { feature: 'Effective audio in games', support: 'no', note: 'Z80 driver not clocked' },
      ],
    },
    {
      group: 'Saves & input',
      rows: [
        { feature: 'Cartridge flash / SRAM save', support: 'no', note: 'Flash writes accepted then ignored' },
        { feature: 'Save-state / battery export', support: 'no' },
        { feature: 'D-pad + A/B + Option', support: 'yes', note: 'System register active-high' },
      ],
    },
    {
      group: 'Connectivity',
      rows: [
        { feature: 'SNK link cable (multiplayer)', support: 'no' },
        { feature: 'Dreamcast / NGPC link', support: 'no' },
        { feature: 'Real-time clock (RTC)', support: 'no', note: 'Not modeled' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'BIOS / boot ROM', support: 'partial', note: 'HLE only: SP/ILM set, jump to entry' },
        { feature: 'Commercial ROM boot', support: 'testing', note: 'No real SWI services; under-verified' },
        { feature: 'Illegal-opcode crash screen', support: 'yes' },
        { feature: 'Unit tests', support: 'yes', note: '~68 tests across subsystems' },
        { feature: 'Game compatibility verified', support: 'no', note: 'No tested-games list' },
      ],
    },
  ],

  wonderswan: [
    {
      group: 'CPU (V30MZ)',
      rows: [
        { feature: 'NEC V30MZ core', support: 'yes', note: '80186/8086-compatible 16-bit x86' },
        { feature: 'Full 8086 base opcode map', support: 'yes' },
        { feature: '80186 additions', support: 'yes', note: 'PUSHA/POPA, ENTER/LEAVE, INS/OUTS, BOUND' },
        { feature: 'ModR/M + segmentation + prefixes', support: 'yes', note: 'Segment-override, REP/LOCK' },
        { feature: 'Interrupts / IRET / flags', support: 'yes' },
        { feature: 'Cycle timing', support: 'partial', note: 'Approximate per-instruction counts' },
        { feature: 'Rare 80186 flag-edge cases', support: 'partial' },
      ],
    },
    {
      group: 'Video',
      rows: [
        { feature: 'Mono WonderSwan rendering', support: 'yes', note: '2bpp tiles, greyscale pool' },
        { feature: 'Color WSC / SwanCrystal rendering', support: 'yes', note: '4bpp tiles, 12-bit RGB palette' },
        { feature: 'Two scrolling tilemap layers', support: 'yes', note: 'SCR1 background + SCR2 foreground' },
        { feature: 'Sprite layer (128-entry)', support: 'yes', note: 'Flip-x/flip-y' },
        { feature: 'Line-compare + VBlank interrupts', support: 'yes' },
        { feature: 'Per-line / mid-frame register effects', support: 'testing', note: 'Scanline renderer; unverified vs games' },
      ],
    },
    {
      group: 'Audio',
      rows: [
        { feature: '4-channel mono synth', support: 'yes', note: '44.1 kHz phase accumulator' },
        { feature: 'Channel 1 square tone', support: 'yes' },
        { feature: 'Channel 2 PCM voice', support: 'partial', note: 'Single voice level, approximate' },
        { feature: 'Channel 3 frequency sweep', support: 'partial', note: 'Latched but sweep step not applied' },
        { feature: 'Channel 4 LFSR noise', support: 'yes' },
        { feature: 'Sound DMA / hyper-voice', support: 'no', note: 'Stubbed' },
      ],
    },
    {
      group: 'Saves & RTC',
      rows: [
        { feature: 'Battery-backed SRAM', support: 'yes', note: '0–512 KiB per footer, dirty-tracked' },
        { feature: 'SRAM save/load API', support: 'yes' },
        { feature: 'EEPROM saves', support: 'partial', note: 'Footer parsed; non-functional' },
        { feature: 'Real-time clock (RTC)', support: 'partial', note: 'Footer flag parsed; not driven' },
        { feature: 'Cartridge footer parsing', support: 'yes' },
      ],
    },
    {
      group: 'Input & rotation',
      rows: [
        { feature: '7-button key matrix', support: 'yes', note: 'X/Y direction pads + A/B + Start' },
        { feature: 'Multiplexed scan groups', support: 'yes', note: 'Select X-pad / Y-pad / buttons' },
        { feature: 'Horizontal orientation', support: 'yes' },
        { feature: 'Vertical (rotated) orientation', support: 'partial', note: 'Footer flag parsed; host must rotate' },
      ],
    },
    {
      group: 'Connectivity',
      rows: [
        { feature: 'Serial port registers', support: 'no', note: 'Documented but no handler' },
        { feature: 'Link cable', support: 'no' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Boots commercial ROMs to title', support: 'yes' },
        { feature: 'Unified mono + Color core', support: 'yes' },
        { feature: 'Crash screen on undefined opcode', support: 'yes' },
        { feature: 'Unit test coverage', support: 'yes', note: '~53 cargo tests across subsystems' },
        { feature: 'Game compatibility verification', support: 'testing', note: 'No tracked tested-games list' },
      ],
    },
  ],

  virtualboy: [
    {
      group: 'CPU (V810 / FPU)',
      rows: [
        { feature: 'Integer ALU ops (Format I–VI)', support: 'yes', note: 'MOV/ADD/SUB/CMP/logic/shift' },
        { feature: 'MUL/MULU/DIV/DIVU', support: 'yes', note: 'Signed + unsigned, div-by-zero traps' },
        { feature: 'Conditional branches + jumps', support: 'yes' },
        { feature: 'System registers (LDSR/STSR)', support: 'yes', note: 'PSW/EIPC/ECR/TKCW/CHCW' },
        { feature: 'Exceptions + interrupt vectoring', support: 'yes', note: '5 priority levels, TRAP/RETI/HALT' },
        { feature: 'On-chip FPU (ADDF/SUBF/MULF/DIVF)', support: 'yes', note: 'Single-precision via host f32' },
        { feature: 'Nintendo extensions (MPYHW/REV/XB/XH)', support: 'yes' },
        { feature: 'Bit-string ops (move/search)', support: 'partial', note: 'Register bookkeeping only; memory stubbed' },
        { feature: 'Cycle-accurate timing', support: 'partial', note: 'Approximate per-op counts' },
      ],
    },
    {
      group: 'Video / VIP',
      rows: [
        { feature: '384×224 per-eye framebuffer', support: 'yes' },
        { feature: 'Red-on-black, 4 brightness levels', support: 'yes', note: 'BRTA/BRTB/BRTC mapping' },
        { feature: "32-world stack (painter's order)", support: 'yes' },
        { feature: 'Normal BG worlds', support: 'yes' },
        { feature: 'OBJ (sprite) worlds', support: 'partial', note: 'Group/SPT handling simplified' },
        { feature: '8×8 2bpp characters (2048) + flips', support: 'yes' },
        { feature: 'GPLT/JPLT palettes', support: 'yes' },
        { feature: 'Frame/draw interrupts', support: 'yes', note: 'XPEND/FRAMESTART' },
        { feature: 'H-bias / affine BG warping', support: 'partial', note: 'Approximated as normal scroll' },
        { feature: 'Column table (per-column brightness)', support: 'no', note: 'Ignored / treated uniform' },
      ],
    },
    {
      group: 'Stereoscopic output',
      rows: [
        { feature: 'Left-eye rendering', support: 'yes', note: 'RGBA8888 left frame' },
        { feature: 'Right-eye rendering', support: 'no', note: 'Right worlds/parallax not rendered' },
        { feature: 'Red/black (monochrome) display', support: 'yes' },
        { feature: 'Anaglyph output mode', support: 'no' },
        { feature: 'Side-by-side output mode', support: 'no' },
        { feature: 'Real 3D / 3DS stereo output', support: 'no' },
      ],
    },
    {
      group: 'Audio / VSU',
      rows: [
        { feature: 'Channels 1–5 wavetable', support: 'yes', note: '32-sample 6-bit wave RAM' },
        { feature: 'Channel 5 modulation/sweep', support: 'partial', note: 'Approximate periodic sweep' },
        { feature: 'Channel 6 noise (LFSR)', support: 'yes', note: '15-bit shift register' },
        { feature: 'Per-channel envelope volume', support: 'yes' },
        { feature: 'L/R stereo volume', support: 'partial', note: 'Folded to mono output' },
        { feature: 'Auto-shutoff interval', support: 'partial', note: 'Length approximated' },
      ],
    },
    {
      group: 'Saves & input',
      rows: [
        { feature: 'Battery-backed cartridge SRAM', support: 'yes', note: '8 KiB, persisted via host' },
        { feature: 'Cartridge footer parsing', support: 'yes' },
        { feature: 'Save states', support: 'no' },
        { feature: 'Dual D-pads, A/B, L/R, Start/Select', support: 'yes', note: '16-bit SDLR/SDHR word' },
      ],
    },
    {
      group: 'Connectivity & timing',
      rows: [
        { feature: 'Link / communication cable', support: 'no', note: 'CCR/CCSR read as open' },
        { feature: 'Programmable interval timer', support: 'yes', note: '20µs/100µs tick with interrupt' },
        { feature: 'Wait-state control (WCR)', support: 'partial', note: 'Bits stored, no timing effect' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Boots / runs commercial games', support: 'testing', note: 'No per-game compatibility list yet' },
        { feature: 'Illegal-opcode crash screen', support: 'yes' },
        { feature: 'Unit test coverage', support: 'yes', note: '59 cargo tests across subsystems' },
        { feature: 'Hardware accuracy validation', support: 'testing', note: 'Not verified against test ROMs' },
      ],
    },
  ],

  gc: [
    {
      group: 'CPU (Gekko)',
      rows: [
        { feature: 'Architectural register state', support: 'yes', note: '32 GPRs/FPRs, CR/XER/LR/CTR/PC, SPRs' },
        { feature: 'Integer ALU opcodes', support: 'partial', note: '~20 ops: add/or/and/cmp/rlwinm' },
        { feature: 'Record-form (.) CR0 update', support: 'yes' },
        { feature: 'Branches (b/bc/bclr, CTR loops)', support: 'yes' },
        { feature: 'Load/store (lwz/stw)', support: 'partial', note: 'Word only; no byte/half/multiple/update' },
        { feature: 'mfspr/mtspr SPR access', support: 'partial', note: 'LR/CTR/XER/SRR/PVR; many absent' },
        { feature: 'Floating-point unit (FPU)', support: 'no', note: 'FPRs are storage only' },
        { feature: 'Paired-single SIMD + GQRs', support: 'no' },
        { feature: 'Most opcodes (mul/div/shift/cr-ops)', support: 'no', note: 'Decode to Program exception' },
        { feature: 'Cycle-accurate timing', support: 'no', note: 'Arbitrary per-frame instruction budget' },
      ],
    },
    {
      group: 'Memory & bus',
      rows: [
        { feature: '24 MB main RAM (big-endian)', support: 'yes', note: 'Cached + uncached mirrors' },
        { feature: '2 MB IPL boot ROM region', support: 'yes' },
        { feature: 'BAT-window translation', support: 'partial', note: 'Static masking; no page-table MMU' },
        { feature: 'Region classification + routing', support: 'yes', note: 'RAM/HW/IPL/unmapped' },
        { feature: 'Page-table MMU / TLB', support: 'no' },
        { feature: 'Hardware register window', support: 'partial', note: 'Flat backing store, no device behavior' },
        { feature: 'ARAM (auxiliary 16 MB RAM)', support: 'no' },
      ],
    },
    {
      group: 'GPU (Flipper)',
      rows: [
        { feature: 'Host RGBA framebuffer', support: 'partial', note: '640×480, cleared to black per frame' },
        { feature: 'Command Processor (CP) FIFO', support: 'no' },
        { feature: 'Transform unit (XF) / T&L', support: 'no' },
        { feature: 'Texture units + TEV combiners', support: 'no' },
        { feature: 'Pixel Engine + embedded framebuffer', support: 'no' },
        { feature: 'Video Interface (VI) scanout', support: 'no' },
        { feature: 'Internal-resolution upscaling', support: 'no' },
      ],
    },
    {
      group: 'Audio (DSP)',
      rows: [
        { feature: 'DSP coprocessor', support: 'no' },
        { feature: 'Audio Interface (AI) streaming', support: 'no' },
        { feature: 'Audio output / mixing', support: 'no' },
      ],
    },
    {
      group: 'Media / DVD',
      rows: [
        { feature: 'DVD / Disc Interface (DI)', support: 'no' },
        { feature: 'ISO/GCM disc images', support: 'no', note: 'Declared format; no loader yet' },
        { feature: 'DOL executable loading', support: 'no', note: 'Declared format; not parsed' },
        { feature: 'IPL boot image loading', support: 'partial', note: 'Loads into ROM region; no IPL provided' },
      ],
    },
    {
      group: 'Controllers, saves & net',
      rows: [
        { feature: 'Serial Interface (SI) controllers', support: 'no', note: 'set_keys() is a no-op stub' },
        { feature: 'Analog sticks / triggers / rumble', support: 'no' },
        { feature: 'Memory cards via EXI', support: 'no' },
        { feature: 'Game Boy Player', support: 'no' },
        { feature: 'Broadband / modem adapter', support: 'no' },
      ],
    },
    {
      group: 'Compatibility & testing',
      rows: [
        { feature: 'Boots commercial games', support: 'no', note: 'Not a functional emulator yet' },
        { feature: 'Exception model (8 vectors)', support: 'yes', note: 'Correct vector offsets' },
        { feature: 'Unit test coverage', support: 'testing', note: '48 tests: CPU/mem/bus/exceptions' },
        { feature: 'Game compatibility verification', support: 'no', note: 'No games run' },
      ],
    },
  ],
};
