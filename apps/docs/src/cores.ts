// The per-core support catalog that drives the whole docs site.
//
// Every field here was derived from the actual Rust source under packages/*
// (CPU/PPU/GPU/APU modules, Cargo.toml, CONTRACT.md, and each core's tests)
// plus the repo README — not aspirational. When a subsystem is partial or
// stubbed, the text says so. `tests` quotes the cargo suite size; `testedGames`
// only lists titles with documented evidence (most cores have none recorded
// yet, which is honest rather than empty marketing).

export type Maturity = 'mature' | 'playable' | 'wip' | 'foundation';

export const MATURITY_LABEL: Record<Maturity, string> = {
  mature: 'Mature',
  playable: 'Playable',
  wip: 'In progress',
  foundation: 'Foundation',
};

export interface TestedGame {
  title: string;
  boots: boolean;
  plays: boolean;
  sound: boolean;
  notes?: string;
}

// Per-capability support state for a core's feature matrix.
//   yes      — implemented and exercised
//   partial  — implemented but incomplete / approximate (needs work)
//   testing  — implemented but under-verified (needs testing)
//   no       — not implemented yet (to add)
export type Support = 'yes' | 'partial' | 'testing' | 'no';

export const SUPPORT_LABEL: Record<Support, string> = {
  yes: 'Supported',
  partial: 'Partial',
  testing: 'Needs testing',
  no: 'Not yet',
};

export interface MatrixRow {
  feature: string;
  support: Support;
  note?: string;
}

export interface MatrixGroup {
  group: string;
  rows: MatrixRow[];
}

export interface Core {
  id: string;
  /** Full system name. */
  name: string;
  /** Short tagline / hardware line shown under the name. */
  tagline: string;
  /** Cargo crate name. */
  crate: string;
  /** packages/<dir> the core lives in. */
  dir: string;
  /** Signature accent colour (mirrors the launcher's SYSTEM_PRESENTATION). */
  accent: string;
  maturity: Maturity;
  /** One-sentence current status. */
  status: string;
  cpu: string;
  video: string;
  audio: string;
  saves: string;
  input: string;
  /** Accepted ROM/disc file extensions (what the launcher detects). */
  formats: string[];
  features: string[];
  limitations: string[];
  /** One sentence describing the cargo test suite. */
  tests: string;
  testedGames: TestedGame[];
  /**
   * Optional fine-grained capability matrix (supported / partial / needs
   * testing / to-add), grouped by subsystem. Rendered as a table when present.
   * Currently authored for the flagship GBA core.
   */
  matrix?: MatrixGroup[];
}

export const CORES: Core[] = [
  {
    id: 'gba',
    name: 'Game Boy Advance',
    tagline: 'ARM7TDMI · the flagship core',
    crate: 'gba-core',
    dir: 'packages/gba',
    accent: '#7c5cff',
    maturity: 'mature',
    status:
      'A faithful, instruction-approximate ARM7TDMI interpreter with a full PPU, PSG + DirectSound audio, DMA, timers, IRQs, and autodetected saves — validated frame-by-frame against a reference interpreter.',
    cpu: 'ARM7TDMI interpreter covering both the ARM and THUMB instruction sets, with software interrupts, IRQ entry/return, exception banking, and a 2-stage pipeline. Pure interpretation (no recompiler) for deterministic execution.',
    video:
      'Complete PPU: all six display modes (text 0–1, affine 2, bitmap 3–5), four background layers with priority/blend/window logic, sprites with rotation/scaling/affine and double-size, mosaic, and alpha/brightness color effects. Backed by golden-frame tests.',
    audio:
      'PSG (2 square + wave + noise) plus DirectSound stereo (2 FIFO-driven PCM channels) at 32,768 Hz, with timer-driven resampling, master + per-channel L/R volume, and envelope/sweep.',
    saves:
      'Autodetected battery saves from the ROM signature: 32 KB SRAM, 64/128 KB Flash, and 512 B / 8 KB EEPROM (bit-serial via DMA3).',
    input: '10-button keypad (A/B/L/R/Select/Start/D-pad) with turbo, plus a SIO link cable over WebRTC for local multiplayer and trading.',
    formats: ['.gba'],
    features: [
      'Instruction-accurate ARM + THUMB interpreter with CPSR banking',
      'All six PPU display modes with per-layer composition',
      'Sprite affine transforms (rotation/scaling, double-size)',
      'Windows + alpha/brightness color blending',
      '4 DMA channels with immediate/VBlank/HBlank/special timing',
      '4 timers with prescaler and count-up chaining',
      'Cartridge RTC (Seiko S-3511A) — drives the Pokémon Ruby/Sapphire/Emerald time events',
      'BIOS HLE (stub vectors, IRQ handler, cartridge boot bypass)',
      'Link cable over WebRTC: local multiplayer + trading (multi-pak)',
      'GBA LCD color correction (higan/byuu) + LCD/CRT screen filter',
      'Fast-forward, frame-step, and rewind',
      'Save states + GameShark/Action Replay cheats',
    ],
    limitations: [
      'Hardware sensors & rumble not modeled — solar (Boktai), tilt/gyro (WarioWare: Twisted!), and cartridge rumble (Drill Dozer)',
      'Link is local multiplay only — no GameCube/JOY-bus, Single-Pak, Wireless Adapter, or Mobile Adapter GB',
      'HLE BIOS only (no dumped-BIOS option); no GB/GBC enhanced backward-compat mode',
      'No e-Reader or exotic cart peripherals (Battle Chip Gate, Campho Advance, Play-Yan, …)',
      'Instruction-approximate timing (not cycle-exact); BIOS-region open bus simplified',
      'Affine/Mode-7 upscaling and high-quality "XQ" (Sappy) audio enhancements not implemented',
    ],
    tests:
      '235-vector cargo suite covering ARM/THUMB CPU, the PPU (text/bitmap/affine, sprites, compositor priority/blend/window + golden frames), DMA, timers, IRQ, sound, save back-ends, RTC, and save-state round-trips; validated bit-identical against a reference interpreter for 120 frames.',
    testedGames: [
      { title: 'Pokémon FireRed', boots: true, plays: true, sound: true, notes: 'Oak intro + name entry verified' },
      { title: 'Pokémon Emerald', boots: true, plays: true, sound: true },
      { title: 'Pokémon Ruby', boots: true, plays: true, sound: true, notes: '"Battery has run dry" prompt fixed' },
      { title: 'Garfield: Search for Pooky', boots: true, plays: true, sound: true, notes: 'Language select renders' },
      { title: 'Crash Bandicoot', boots: true, plays: true, sound: true, notes: 'Title intro + Earth flyby' },
    ],
    // Benchmarked against the emulation-general wiki + Shonumi's peripheral catalog, cross-checked against the gba-core source.
    matrix: [
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
  },
  {
    id: 'gbc',
    name: 'Game Boy / Color',
    tagline: 'SM83 (LR35902)',
    crate: 'gbc-core',
    dir: 'packages/gbc',
    accent: '#ff5fa2',
    maturity: 'playable',
    status:
      'Full SM83 CPU, scanline PPU, 4-channel APU, DMA, timers, and the MBC1/2/3/5 cartridge mappers — DMG and CGB in one core.',
    cpu: 'Complete SM83/LR35902 instruction set (primary + CB-prefixed ops), all flags, HALT/STOP/IME and interrupt dispatch, plus illegal-opcode detection that renders a crash screen.',
    video:
      '160×144 scanline renderer with BG/window/sprite rendering, the mode FSM (OAM scan / draw / H-Blank / V-Blank), LCDC/STAT/LY/LYC timing, STAT interrupts, and CGB color palettes + VRAM banking.',
    audio:
      '4-channel APU (2 square with envelope/sweep, wave, noise LFSR) driven by the 256/128/64 Hz frame sequencer, with master volume + panning (NR50/NR51), downsampled to f32 stereo.',
    saves: 'Battery-backed cart RAM with dirty tracking; MBC3 RTC registers are present and latched.',
    input: '8-button joypad through the $FF00 active-low matrix, with the joypad interrupt on button-press transitions.',
    formats: ['.gb', '.gbc'],
    features: [
      'NoMBC, MBC1, MBC2 (built-in RAM), MBC3 (+RTC), MBC5 (+rumble flag)',
      'CGB double-speed mode (KEY1) and VRAM/WRAM banking',
      'OAM DMA ($FF46) and HDMA/GDMA ($FF51–55) VRAM transfer',
      'Timer (DIV/TIMA/TMA/TAC) with falling-edge + obscure behaviors',
      'Serial transfer on the internal clock (2× on CGB)',
      'Full interrupt set (VBlank/STAT/Timer/Serial/Joypad) with IME gating',
    ],
    limitations: [
      'Serial never completes on an external clock (no link cable yet)',
      'MBC3 RTC registers are not advanced by real time',
      'MMM01, MBC6/7, TAMA5, and HuC mappers fall back to NoMBC',
      'Scanline renderer approximates the cycle-accurate pixel FIFO',
    ],
    tests:
      '55 unit tests across CPU decode/exec, PPU, APU, timer/joypad/serial, interrupts, memory/banking, the MBCs, DMA, and full-emulator integration (incl. the HALT bug and audio frame sequencing).',
    testedGames: [],
  },
  {
    id: 'nes',
    name: 'NES',
    tagline: 'Ricoh 2A03 (6502)',
    crate: 'nes-core',
    dir: 'packages/nes',
    accent: '#e4000f',
    maturity: 'playable',
    status:
      'A from-scratch 2A03 CPU, 2C02 PPU, and 2A03 APU built to the NESdev specs, with five iNES mappers covering most early commercial titles.',
    cpu: 'Ricoh 2A03 with all 151 official opcodes plus 13 common unofficial ones (LAX/SAX/DCP/ISC/SLO/RLA/SRE/RRA/NOP variants), correct page-cross/branch cycle penalties, and NMI/IRQ/RESET sequences. JAM/KIL hard-halts to a crash screen.',
    video:
      '2C02 PPU with the loopy v/t/x/w scroll model, background tile + attribute pipeline, sprite evaluation (8/line, sprite-0 hit, overflow), VBlank NMI, and the canonical NES palette into a 256×240 framebuffer.',
    audio: '2 pulse + triangle + noise channels with the frame-counter-driven envelope/length/sweep; the DMC channel is a stub (level + enable only). Downsampled to 44.1 kHz mono.',
    saves: '8 KiB PRG-RAM at $6000–7FFF with the iNES battery flag detected; not yet persisted by the core.',
    input: 'Standard controller via the $4016/$4017 strobe/shift protocol across two ports.',
    formats: ['.nes'],
    features: [
      'iNES + NES 2.0 header parsing with mapper decode',
      'Mappers 0 (NROM), 1 (MMC1), 2 (UxROM), 3 (CNROM), 4 (MMC3 + scanline IRQ)',
      'Nametable mirroring: horizontal / vertical / single-screen / four-screen (approx.)',
      'CHR-RAM boards (writable CHR when header CHR size is 0)',
      'OAM DMA ($4014) with correct 513/514-cycle timing',
      'JAM-opcode fault detection with a rendered crash screen',
    ],
    limitations: [
      'DMC channel is a stub (no sample fetch/playback or DMA stalls)',
      'Four-screen mirroring is approximated as two tables',
      'APU frame sequencer uses a coarse ~7457-cycle approximation',
      'PRG-RAM is present but not persisted to storage',
    ],
    tests:
      '29 cargo tests covering CPU instructions, interrupts, the mappers, PPU registers/scroll/VBlank, sprite evaluation, palette, APU channels, controllers, OAM DMA, and JAM fault handling.',
    testedGames: [],
  },
  {
    id: 'snes',
    name: 'SNES',
    tagline: '65C816 (5A22)',
    crate: 'snes-core',
    dir: 'packages/snes',
    accent: '#8b7fd4',
    maturity: 'playable',
    status:
      'A from-scratch 65C816 CPU, all PPU background modes 0–7, and a partial SPC700/S-DSP audio path that boots the real IPL ROM — boots and plays many titles without deadlock.',
    cpu: 'WDC 65C816 (Ricoh 5A22) with the full documented instruction set, emulation/native modes, 8/16-bit accumulator + index via the M/X flags, 24-bit banked addressing, decimal mode, and RESET/NMI/IRQ/BRK/COP interrupts.',
    video:
      'PPU renders BG modes 0–7 (Mode 7 with affine transform), OBJ sprites with priority, and 2/4/8 bpp tiles (8×8 and 16×16) into a 256×224 framebuffer with main/sub screen and basic color math.',
    audio:
      'The SPC700 runs the real IPL boot ROM and typical sound drivers via the $2140–$2143 mailbox; the S-DSP decodes BRR samples with KON/KOFF and a simple pitched mixer (mono f32 @ 32 kHz).',
    saves: 'Battery-backed SRAM detected from the cartridge header, with a dirty flag for host persistence.',
    input: 'Standard 12-button controller via auto-joypad read ($4218–$421F) and the manual serial path.',
    formats: ['.smc', '.sfc'],
    features: [
      'Full 65C816 with native/emulation modes and 24-bit banking',
      'All PPU BG modes 0–7 including Mode 7 affine',
      'OBJ sprite rendering with priority resolution',
      'SPC700 runs the real IPL ROM (prevents game deadlock)',
      '8 DMA channels + HDMA (per-scanline transfers)',
      'LoROM / HiROM auto-detection via header scoring',
    ],
    limitations: [
      'PPU windows, mosaic, and offset-per-tile (modes 2/4/6) are stubbed',
      'No hi-res / interlace modes',
      'S-DSP ADSR envelopes, gaussian interpolation, and echo are simplified',
      'No enhancement chips (SuperFX, SA-1, DSP-1, …)',
      'Whole-frame renderer — no mid-frame per-scanline register changes',
    ],
    tests:
      '46 unit tests across the CPU (16), PPU (4), APU/SPC/DSP (6), DMA (2), cartridge (4), input (2), and core integration (9).',
    testedGames: [],
  },
  {
    id: 'sms',
    name: 'Master System / Game Gear',
    tagline: 'Zilog Z80',
    crate: 'sms-core',
    dir: 'packages/sms',
    accent: '#e07b1f',
    maturity: 'playable',
    status:
      'A full Z80 interpreter with a TMS9918-derived Mode 4 VDP and SN76489 PSG — one core handles both the Master System (256×192) and Game Gear (160×144).',
    cpu: 'Complete Zilog Z80: all register/shadow sets, every opcode page (main, CB, ED, DD/FD, DDCB/FDCB), accurate M-cycle/T-state timing, and interrupts (NMI, IM 0/1/2, EI delay). Deadlock (HALT with IRQs off) trips a crash screen.',
    video:
      'VDP Mode 4 with 16 KiB VRAM, a 32-entry palette (6-bit SMS / 12-bit Game Gear), a 32×28 tilemap over 256 patterns, up to 8 sprites/line with zoom, sprite overflow + collision, and line/frame interrupts.',
    audio: 'SN76489 PSG — 3 square tone channels + 1 noise (15-bit LFSR), 4-bit per-channel volume, Game Gear stereo, sampled at 44.1 kHz.',
    saves: 'Battery-backed cartridge RAM (up to 32 KiB on the Sega mapper) with dirty-flag tracking.',
    input: 'Two digital controllers; SMS Pause triggers an NMI and the Game Gear Start sits on port $00.',
    formats: ['.sms', '.gg'],
    features: [
      'Full Z80 instruction set including undocumented opcodes',
      'Complete VDP Mode 4: backgrounds, sprites, palettes, scroll, flip, priority',
      'SN76489 PSG with tone + noise and Game Gear stereo',
      'Sega and Codemasters ROM mappers with bank switching',
      'On-cart battery RAM with dirty tracking',
      'One core serves both Master System and Game Gear',
    ],
    limitations: [
      'NTSC 262-line timing only (no PAL variant)',
      'H counter returns a stable mid-line value, not cycle-accurate',
      'No save-state serialization',
      'Game Gear region lock is not enforced',
    ],
    tests: '53 unit tests: CPU (21), VDP (8), PSG (5), cartridge mapper (6), I/O (3), and SMS integration (10).',
    testedGames: [],
  },
  {
    id: 'genesis',
    name: 'Mega Drive / Genesis',
    tagline: 'Motorola 68000 + Z80',
    crate: 'genesis-core',
    dir: 'packages/genesis',
    accent: '#1a6dd6',
    maturity: 'playable',
    status:
      'A complete 68000, a functional Z80 sub-CPU, the 315-5313 VDP, and YM2612 + PSG audio — title screens and simple gameplay run; cycle accuracy and full FM synthesis are the remaining gaps.',
    cpu: 'Motorola 68000 with the common instruction set, 12 addressing modes, exception vectoring, and autovectored level-4/6 interrupts. The Z80 sound CPU is complete (all opcode pages, M-cycle timing, IM 0/1/2) and bank-switches into 68000 space.',
    video:
      '315-5313 VDP with 64 KiB VRAM, CRAM, and VSRAM: planes A/B with scroll + per-tile priority, an 80-sprite linked list, the window plane, and DMA (68k→VRAM, fill, copy). Supports H40 (320×224) and H32 (256×224); rendered per-frame.',
    audio:
      'YM2612 6-channel FM with complete register latching but best-effort tone synthesis (a placeholder oscillator per keyed-on channel; the 4-operator envelope/algorithm/LFO chain is stubbed), mixed with a full SN76489 PSG. Mono/stereo f32 @ 44.1 kHz.',
    saves: 'Battery-backed on-cart SRAM, with the header SRAM window parsed and a dirty flag exposed to the host.',
    input: '3/6-button controller protocol with TH-toggle phase sequencing for the extended buttons, two players.',
    formats: ['.md', '.gen', '.smd'],
    features: [
      'Complete Motorola 68000 instruction set with exception vectoring',
      'Full Z80 (all opcode pages) with accurate M-cycle timing',
      'VDP planes A/B, 80-sprite list, window plane, scroll modes',
      'VDP DMA: memory→VRAM, VRAM fill, VRAM copy',
      'YM2612 (register-complete) + SN76489 PSG mixed output',
      '3/6-button controllers with TH-toggle sequencing',
    ],
    limitations: [
      'Cycle-approximate; per-frame rendering blocks mid-frame raster effects',
      'YM2612 4-operator envelope/algorithm/LFO chain not implemented',
      'Bank-switch mappers (SSF2, etc.) stubbed — plain ROM + simple SRAM only',
      'No shadow/highlight or interlace; window plane is minimal',
      'Odd-address bus/address-error exceptions and BCD/CHK ops stubbed',
    ],
    tests:
      '85 tests across m68k (23), z80 (21), genesis integration (13), cart (4), vdp (8), io (5), psg (5), and ym2612 (6) — instructions, memory maps, interrupts, DMA, the controller protocol, and multi-frame stability.',
    testedGames: [],
  },
  {
    id: 'pce',
    name: 'PC Engine / TurboGrafx-16',
    tagline: 'HuC6280',
    crate: 'pce-core',
    dir: 'packages/pce',
    accent: '#f2a900',
    maturity: 'playable',
    status:
      'A from-scratch HuC6280 CPU, HuC6270 VDC with tilemap + sprite rendering, the HuC6260 palette, and a 6-channel wavetable PSG — boots and renders title screens.',
    cpu: 'Hudson HuC6280 @ 7.16 MHz: the full 65C02 set plus CMOS extensions, the banking MMU (8 MPRs, TAM/TMA), block transfers (TII/TDD/TIA/TAI/TIN), ST0/ST1/ST2 fast VDC writes, CSL/CSH speed switching, and a built-in timer + I/O port.',
    video:
      'HuC6270 VDC (64 KiB VRAM) with a scrollable background tilemap (up to 128×64 virtual), 64 sprites (variable size, flip, priority), sprite-0/overflow flags, and VBlank + raster (RCR) interrupts; the HuC6260 VCE provides a 512-entry 9-bit palette into a 256×224 frame.',
    audio: '6-channel wavetable PSG with per-channel frequency/volume, L/R balance, noise (channels 4–5), and DDA direct mode; LFO and the noise spectrum are approximate. Mono f32 @ 44.1 kHz.',
    saves: 'Battery-backed save RAM is recognized but not modeled — saves are not supported.',
    input: 'Standard 2-bit SEL/CLR joypad: D-pad plus I/II/Select/Run.',
    formats: ['.pce'],
    features: [
      'Full HuC6280 with 65C02 CMOS extensions',
      'Banking MMU with 8 Memory Page Registers (TAM/TMA)',
      'Block transfers + ST0/ST1/ST2 fast VDC writes',
      'Background tilemap with scrolling + 64-sprite rendering',
      'Sprite-0 collision and overflow detection',
      '6-channel wavetable PSG with DDA + noise channels',
    ],
    limitations: [
      'No CD-ROM support (IRQ2 for CD/expansion unused)',
      'Save RAM is recognized but not stored',
      'PSG LFO and exact noise spectrum are approximate',
      'Only the 256-wide rendering path is used',
      'SATB and VRAM→VRAM DMA do not perform real transfers',
    ],
    tests:
      '61 cargo tests: CPU (21, incl. TAM/TMA + block transfers), VDC (8), VCE/palette (8), PSG (6), input (4), and 13 integration tests (MMU, ROM load, frame advance, IRQ routing, crash screen).',
    testedGames: [],
  },
  {
    id: 'nds',
    name: 'Nintendo DS',
    tagline: 'ARM9 + ARM7',
    crate: 'nds-core',
    dir: 'packages/nds',
    accent: '#e0e0e6',
    maturity: 'playable',
    status:
      'Dual-ARM CPUs, both 2D engines with all background types + sprites, a functional 3D rasterizer, PCM/ADPCM audio mixing, and the cart loader / BIOS HLE — backed by 365 tests.',
    cpu: 'ARM9 (ARMv5TE, with CP15 cache/TCM/MPU control) and ARM7 (ARMv4T), each with banked register sets, exception entry, and mode switching; full ARM/Thumb decode including the v5 extensions (BLX, CLZ, saturating ops, LDRD/STRD) on the ARM9.',
    video:
      '2D: two 256×192 engines (A/B) with text, affine, and bitmap backgrounds, sprites with affine + extended palettes, windows, and blending over a 9-bank VRAM router. 3D: a geometry pipeline with the matrix stacks, Gouraud shading, perspective-correct texturing, 1/w depth, fog, and edge-marking via a software scanline rasterizer.',
    audio:
      '16-channel mixer with PCM8/PCM16 and IMA-ADPCM decode, per-channel volume/pan, key-on envelope clearing, mixed to stereo @ 44.1 kHz. The PSG square/noise format is recognized but produces silence.',
    saves: 'AUXSPI FLASH save chip with a read/write command state machine, a per-game address-size table, and status/protect bits; data persists via a dirty flag.',
    input: 'Keypad (KEYINPUT) + the X/Y/lid extended keys, plus a touchscreen driver writing NitroSDK-compatible samples to main RAM each VBlank.',
    formats: ['.nds'],
    features: [
      'Dual ARM execution (ARMv5TE + ARMv4T) with full exception handling',
      'CP15 coprocessor (TCM/cache/MPU) and 7-mode banked registers',
      '2D engines: text/affine/bitmap backgrounds, sprites, windows, blending, extended palettes',
      '3D geometry engine: matrix stacks, lighting, perspective-correct texturing, 1/w depth, fog, edge marking',
      '16-channel PCM8/16 + IMA-ADPCM audio mixer',
      'Cart loader with BIOS HLE, binary relocation, overlays, and the encryption protocol',
      'DMA (4/core, 8 timing modes), timers, IRQ, RTC, IPC/FIFO, and the ARM9 math accelerator',
    ],
    limitations: [
      'PSG (square/noise) audio produces silence',
      '3D is point-sampled (no bilinear) and wireframe falls back to solid fill',
      'Display capture (DISPCAPCNT) and main-memory display mode not implemented',
      'Wi-Fi, rumble, and motion sensors are unimplemented',
    ],
    tests:
      '365 unit tests spanning BIOS HLE, cartridge protocol + save FLASH, ARM/Thumb decode + banking, the 2D engines (all background/sprite paths, blending, framebuffer swap), the 3D pipeline (matrices, lighting, texturing, rasterization), audio decode, DMA, IPC, timers, and IRQ.',
    testedGames: [],
  },
  {
    id: 'ps1',
    name: 'PlayStation',
    tagline: 'MIPS R3000A + GTE',
    crate: 'ps1-core',
    dir: 'packages/ps1',
    accent: '#c9c9d4',
    maturity: 'playable',
    status:
      'A full R3000A interpreter, the complete GTE, a software GPU with 3D, an SPU with ADPCM/ADSR, CD-ROM ISO9660 boot, all 7 DMA channels, and the MDEC — 144 tests across every subsystem; BIOS-gated (ships OpenBIOS).',
    cpu: 'MIPS R3000A interpreter with every SPECIAL/REGIMM/COP0/COP2 opcode class, exception handling, branch + load delay slots, interrupts, and COP0 system control. The GTE (COP2) geometry engine is complete (all matrix/vector operations).',
    video:
      'Software rasterizer over a 1024×512 16bpp VRAM: Gouraud triangles/quads, texture mapping with CLUT, 4/8/15bpp modes, drawing-area clipping, interlacing, and display windowing, expanded to an RGBA8888 framebuffer. Handles all GP0 render commands + VRAM transfers.',
    audio: 'SPU with 24 ADPCM voices, full ADSR envelopes, 4-point gaussian pitch interpolation, a 512 KB sound RAM, minimal reverb (echo line), and DMA4 — f32 stereo @ 44.1 kHz.',
    saves: 'The SIO0 digital-controller protocol is implemented; the memory-card probe returns high-Z (clean fail) — no save/load yet.',
    input: 'Digital-pad input over SIO0 with the active-low wire format and /ACK interrupt (psx-spx button layout).',
    formats: ['.cue', '.bin', '.img', '.iso', '.pbp'],
    features: [
      'Full MIPS R3000A with exception vectors + COP0 cache isolation',
      'Complete GTE (COP2) geometry transformation engine',
      'Software GPU: triangles/quads, texture/CLUT, blend modes',
      'CD-ROM ISO9660 boot parsing with an HLE disc-boot fallback',
      'All 7 DMA channels (burst/slice/linked-list, OTC reverse-clear)',
      'SPU voice mixing, ADPCM, ADSR, pitch interpolation',
      'MDEC FMV decoder (JPEG-like IDCT + YUV→RGB)',
    ],
    limitations: [
      'No memory-card save/load yet',
      'No CD-DA audio playback (data reads only)',
      'Minimal reverb (single feedback echo)',
      'One-cycle-per-instruction timing — not cycle-accurate device scheduling',
      'No analog controller or multitap; no SIO1 link cable',
    ],
    tests:
      '144 unit tests covering CPU execution (delay slots, exceptions, 50+ opcodes), GTE geometry, GPU rendering + VRAM transfers, SPU (ADPCM/ADSR/voices), all DMA channels, CD-ROM command sequencing, timers, IRQ, and the frame/exe-load integration paths.',
    testedGames: [],
  },
  {
    id: 'n64',
    name: 'Nintendo 64',
    tagline: 'VR4300 (MIPS III)',
    crate: 'n64-core',
    dir: 'packages/n64',
    accent: '#2e9e4f',
    maturity: 'wip',
    status:
      'A complete VR4300 CPU with COP0/COP1 and exception handling, HLE IPL3 boot, and all RCP register blocks with DMA plumbing — but the RSP, RDP, and audio are register stubs that do not yet execute microcode.',
    cpu: 'Full MIPS III VR4300 integer ISA (200+ instructions) with 64-bit GPRs, HI/LO, branch delay slots, and a 32-entry TLB. COP0 implements exception entry, Cause/Status/EPC, the Count/Compare timer, and ERET; COP1 provides single/double FPU arithmetic, load/store, and convert/move.',
    video:
      'The Video Interface scans a framebuffer out of RDRAM to RGBA8888 (320×240 default) and raises the vertical interrupt per frame, so CPU-filled framebuffers are visible. The RDP accepts command pointers but does not rasterize, so RDP-drawn content is not shown.',
    audio: 'The Audio Interface owns the register block and DMA config but produces no samples — the RSP audio microcode is not executed, so audio drains empty.',
    saves: 'No save/flash support yet; cartridge data is read-only.',
    input: 'The Serial Interface implements the PIF joybus protocol for one controller on channel 0 (buttons + stick via set_keys()).',
    formats: ['.z64', '.n64', '.v64'],
    features: [
      'Complete MIPS III integer ISA with proper branch delay slots',
      'Full COP0 exception model (16 types, TLB, timer interrupt)',
      'Partial COP1 FPU (single/double arithmetic, loads/stores, moves, compares)',
      'HLE IPL3 boot for CIC 6102',
      'ROM byte-order detection (.z64/.n64/.v64)',
      'MI interrupt aggregation + PI/RSP/SI DMA',
      'VI framebuffer scanout to host RGBA8888',
    ],
    limitations: [
      'RSP/RDP/AI are stubs — no display lists, no audio synthesis, no vector unit',
      'TLB does not handle mapped segments beyond KSEG0/KSEG1',
      'FPU rounding simplified to host round-to-nearest (no IEEE flags)',
      'No save/flash cartridge types',
      'Instruction-deterministic (CPI=1), not cycle-accurate',
    ],
    tests:
      '69 unit tests covering CPU instructions, COP0/COP1, the boot sequence, cart loading, interrupt handling, DMA, the controller joybus, and device register read/write semantics.',
    testedGames: [],
  },
  {
    id: 'xbox',
    name: 'Xbox',
    tagline: 'Pentium III · NV2A',
    crate: 'xbox-core',
    dir: 'packages/xbox',
    accent: '#9cd530',
    maturity: 'foundation',
    status:
      'A foundation core: it boots nxdk homebrew (CRT + kernel imports) and the NV2A software rasterizer consumes a real pushbuffer to draw + animate the triangle demo — but no commercial games without a full kernel/MCPX/APU/IDE.',
    cpu: 'Intel Pentium III (Coppermine) IA-32 interpreter @ 733 MHz: integer ALU, MOV (all forms incl. segment/CR), INC/DEC/NEG/NOT/TEST/XCHG/LEA, shifts/rotates, branches (JMP/Jcc/CALL/RET/LOOP), MUL/DIV, and flags. Boots real XBE code from the reset vector (real→protected mode); FPU/SSE/paging raise #UD.',
    video:
      'NV2A GPU @ 233 MHz with a software rasterizer: pushbuffer DMA parsing (PFIFO/PGRAPH), surface setup, color clear, vertex arrays (DRAW_ARRAYS / ARRAY_ELEMENT16) + immediate-mode vertices, an MVP transform chain (viewport scale/offset), and Gouraud flat-shaded triangle rasterization into a 32-bit ARGB surface scanned out via PCRTC with a VBlank interrupt.',
    audio: 'None — the APU is entirely absent.',
    saves: 'None.',
    input: 'None — USB controller input is not modeled.',
    formats: ['.xbe', '.xiso'],
    features: [
      'IA-32 integer ISA interpreter (ALU, MOV, shifts, jumps, CALL/RET, MUL/DIV)',
      '64 MB unified memory map with bus routing (RAM / NV2A MMIO / flash)',
      'XBE parsing: entry-point de-obfuscation, section load, import-thunk patching',
      'XISO/XDVDFS disc mounting (title + entry point + file listing)',
      'HLE Xbox kernel (~50 exports: memory, SHA-1, interlocked, IRQL, time, events, NV2A PLL/PFB)',
      'NV2A PFIFO/PGRAPH pushbuffer parsing + method dispatch',
      'NV2A transform/viewport + vertex arrays + Gouraud triangle rasterization',
      'PCRTC scanout with a 60 Hz VBlank interrupt',
    ],
    limitations: [
      'No MCPX southbridge (USB, IDE/HDD/DVD, SMBus) — no real I/O',
      'No APU (audio entirely unimplemented)',
      'No encrypted kernel boot — relies on patched HLE entry points',
      'No paging / MMU / GDT / IDT (all addresses physical)',
      'No FPU/SSE (raise #UD)',
      'No NV2A 3D pipeline: no shaders, register combiners, textures, depth/stencil, or blending',
      'No commercial-game support — homebrew demos only',
    ],
    tests:
      'Cargo tests across the CPU (instruction decode/execute), NV2A render (rasterization, transforms, interpolation), NV2A pushbuffer (parsing, dispatch, clear), memory/bus, XBE/XISO parsing, and HLE — run with `cargo test --manifest-path packages/xbox/Cargo.toml`.',
    testedGames: [
      { title: 'hello.xbe (nxdk sample)', boots: true, plays: false, sound: false, notes: 'CRT init + kernel imports; stalls on an unhandled kernel export' },
      { title: 'triangle.xbe (nxdk sample)', boots: true, plays: true, sound: false, notes: '~237 real NV2A methods pushed → triangle rendered + scanned out; demonstrates the foundation GPU path' },
    ],
  },
  {
    id: 'atari2600',
    name: 'Atari 2600',
    tagline: 'MOS 6507 · TIA',
    crate: 'atari2600-core',
    dir: 'packages/atari2600',
    accent: '#b8531f',
    maturity: 'playable',
    status:
      'A from-scratch 2600 (VCS) with a cycle-accurate 6507 CPU and beam-racing TIA video + audio — modeling the machine\'s race-the-beam architecture directly.',
    cpu: 'MOS 6507 (6502 variant, 13-bit address bus) with all official opcodes plus common unofficial ones (LAX/SAX/DCP/ISC/SLO/RLA/SRE/RRA/NOP), correct flags and page-cross/branch cycle counts, NMI/IRQ/RESET, and JAM detection.',
    video:
      'The TIA generates one color clock at a time (1 CPU cycle = 3 TIA clocks): 2 players, 2 missiles, 1 ball, a 20-bit playfield (reflect + score modes), RESPx positioning with HMOVE fine motion and left-edge comb blanking, all 15 collision latches, VSYNC/VBLANK, WSYNC CPU stall, and the NTSC palette into a 160×192 window.',
    audio: '2 audio channels with the polynomial-counter waveforms (all AUDC modes: pure tones, 4/5/9-bit noises, divide-by-6/31), AUDV volume, host-sampled at 44.1 kHz.',
    saves: 'None.',
    input: 'Joystick (4 directions + fire) via SWCHA/INPT4-5 and the console Reset/Select switches via SWCHB, both players.',
    formats: ['.a26'],
    features: [
      'Beam-racing video with cycle-accurate TIA timing',
      'Complete 6507 instruction set incl. unofficial opcodes',
      'RIOT (6532) 128-byte RAM, interval timer, and I/O ports',
      'Playfield reflect/score modes, missile/ball sizing, object priority',
      'HMOVE comb effect (left-edge blanking) modeled',
      'All 15 collision latches + WSYNC CPU stall',
    ],
    limitations: [
      'Late-HMOVE quirks are simplified',
      'Audio is not bit-exact for every AUDC/AUDF corner case',
      'Paddle and driving controllers not implemented',
      'Less common bank schemes (E0, FE, 3F, SuperChip RAM) not implemented',
    ],
    tests:
      '52 cargo tests covering CPU instructions, the memory map, cartridge bank-switching (F8/F6/F4), TIA objects, collisions, WSYNC, HMOVE, playfield modes, audio channels, the RIOT timer, and full-frame rendering.',
    testedGames: [],
  },
  {
    id: 'ngpc',
    name: 'Neo Geo Pocket Color',
    tagline: 'TLCS-900/H',
    crate: 'ngpc-core',
    dir: 'packages/ngpc',
    accent: '#2bb7c4',
    maturity: 'playable',
    status:
      'A from-scratch NGPC with the TLCS-900/H main CPU and the K1GE/K2GE video controller working and tested; the Z80 sound CPU exists but is not yet clocked in the frame loop.',
    cpu: 'TLCS-900/H with register banks, flags, variable-length encoding, and a broad instruction set (loads, arithmetic, logic, shifts, branches, interrupts with ILM masking). Block transfers and some exotic addressing modes are stubbed. A Z80 sound-CPU interpreter is present but not yet clocked.',
    video:
      'The K1GE/K2GE controller (160×152) renders two scroll planes plus 64 sprites with H/V chain + tile flip, a 12-bit RGB palette to RGBA8888 with per-sprite palette select (K2GE), and H/V-blank interrupt latches.',
    audio: 'A T6W28 PSG (SN76489-style dual tone/noise) with stereo L/R attenuation and an 8-bit DAC port @ 3.072 MHz; the Z80 audio CPU is not yet clocked.',
    saves: 'Flash command writes are accepted but ignored — no cartridge save/SRAM emulation.',
    input: 'D-pad + A + B + Option via the system register at $6F82 (active-high).',
    formats: ['.ngp', '.ngc'],
    features: [
      'Unified mono (NGP) and color (NGPC) core via the header byte',
      'Dual ROM windows + 12 KiB work RAM + shared sound RAM',
      'Complete TLCS-900/H instruction decode',
      'Interrupt system with ILM masking + vector dispatch',
      'Sprite H/V chain and tile H/V flip',
      'Crash screen for illegal opcodes',
    ],
    limitations: [
      'Z80 sound CPU not integrated into frame timing — audio synthesis disabled',
      'Block-transfer (LDIR/LDDR) and LINK/UNLK ops stubbed',
      'BIOS HLE only — no real boot ROM / SWI handlers',
      'No cartridge save / flash / SRAM support',
    ],
    tests:
      '68 unit tests across the system, cart, input, CPU exec, video, Z80, and PSG modules — work/sound RAM, ROM windows, input, frame advance, PSG writes, color/mono mode, header parsing, and CPU decode.',
    testedGames: [],
  },
  {
    id: 'wonderswan',
    name: 'WonderSwan',
    tagline: 'NEC V30MZ',
    crate: 'wonderswan-core',
    dir: 'packages/wonderswan',
    accent: '#3aa856',
    maturity: 'playable',
    status:
      'A from-scratch V30MZ CPU, tilemap + sprite video, a 4-channel synth, and battery-backed saves — boots commercial ROMs and reaches title screens.',
    cpu: 'NEC V30MZ (80186/8086-compatible 16-bit x86) with the full 8086 base map plus the 80186 additions (PUSHA/POPA, IMUL imm, ENTER/LEAVE, INS/OUTS), real-mode segmentation, ModR/M decode, prefixes, interrupts, and flags.',
    video:
      'Two scrolling tilemap layers (SCR1 background, SCR2 foreground) plus a 128-sprite layer into a 224×144 frame. Mono uses 2bpp tiles with greyscale palettes; Color uses 4bpp packed tiles with a 12-bit RGB palette RAM. Line-compare + VBlank interrupts.',
    audio: '4-channel mono synth @ 44.1 kHz: square tone, 4-bit PCM voice, tone-with-sweep, and an LFSR noise channel via a phase accumulator. Hyper-voice and some sweep mechanics are stubbed.',
    saves: 'Battery-backed SRAM via the I/O bank register (0–512 KiB per the cart footer), tracked and exposed through the API.',
    input: 'A 7-button key matrix (X/Y direction pads + A/B + Start) scanned via I/O, supporting the rotated vertical/horizontal layouts.',
    formats: ['.ws', '.wsc'],
    features: [
      'Unified mono WonderSwan + Color core',
      'Full V30MZ instruction set with approximate cycle stepping',
      'Priority-encoded interrupt controller with maskable IRQ',
      'Tilemap flip-x/flip-y + a 512-byte sprite table with flip',
      'Mono greyscale LUT + Color 12-bit RGB → RGBA8888',
      'Crash screen on undefined opcodes',
    ],
    limitations: [
      'EEPROM and RTC parsed but non-functional',
      'Sound DMA and hyper-voice modes stubbed',
      'Channel-3 sweep / voice-sample register path only (no sweep step applied)',
      'Some 80186 corner-case flag edges not fully exercised',
    ],
    tests:
      '53 cargo tests across cpu (35), ws integration (12), video (4), audio (5), and cart (4) — opcodes, addressing, interrupts, I/O, framebuffer, sprites, palettes, sound channels, and ROM/SRAM.',
    testedGames: [],
  },
  {
    id: 'virtualboy',
    name: 'Virtual Boy',
    tagline: 'NEC V810',
    crate: 'virtualboy-core',
    dir: 'packages/virtualboy',
    accent: '#d4233b',
    maturity: 'playable',
    status:
      'A from-scratch V810 CPU with on-chip FPU, the VIP video processor, a 6-channel VSU, and battery saves — boots games and renders the 384×224 red-on-black display.',
    cpu: 'NEC V810 (µPD70732) with all Format I–VII integer ops, the ALU (incl. mul/div), conditional branches, system registers, an on-chip FPU (CMPF/CVT/ADDF/SUBF/MULF/DIVF/TRNC), the Nintendo extensions (MPYHW/REV/XB/XH), and exception/interrupt vectoring. Bit-string ops do register bookkeeping but stub memory effects.',
    video:
      'VIP stereo display: 384×224 per eye, red-on-black via four brightness levels, with a 32-world stack (painter\'s order), normal + H-bias BG worlds, OBJ sprite worlds, 2048 8×8 tiles with 4-entry palettes, and the frame/draw interrupts. Outputs the LEFT eye as RGBA8888.',
    audio: '6-channel VSU: channels 1–5 wavetable (32-sample 6-bit RAM each), channel 6 noise (15-bit LFSR), with per-channel enable, frequency stepping, envelope, and L/R volume, mixed to mono @ 41.7 kHz.',
    saves: 'Battery-backed SRAM via the cartridge footer (8 KiB) with a dirty flag for host sync.',
    input: 'Full pad: dual D-pads, A/B, L/R triggers, Start/Select via the 16-bit SDLR/SDHR hardware word.',
    formats: ['.vb', '.vboy'],
    features: [
      'Full V810 RISC CPU with 5-priority exception/interrupt vectoring',
      'On-chip FPU (8 float ops) + 3 Nintendo integer extensions',
      'VIP 32-world painter\'s-order display with palettes + brightness',
      'VSU 6-channel audio with wave RAM, envelope, and noise LFSR',
      'Programmable interval timer (20µs/100µs) with interrupts',
      'Cartridge footer parsing + ROM mirroring',
    ],
    limitations: [
      'LEFT eye only — no right-eye anaglyph or stereo output',
      'VIP affine + H-bias warping approximated as scrolled tilemaps',
      'VIP per-column brightness repeat ignored',
      'VSU channel-5 modulation/sweep + auto-shutoff approximated',
      'Bit-string memory move/search not performed; cycle counts approximate',
    ],
    tests:
      '59 cargo tests: CPU (32 — decode, ALU, FPU, branches, exceptions), VIP (7), vb integration (7), VSU (4), cartridge (3), hardware timer (3), and input (3).',
    testedGames: [],
  },
  {
    id: 'gc',
    name: 'GameCube',
    tagline: 'Gekko (PowerPC 750CXe)',
    crate: 'gc-core',
    dir: 'packages/gc',
    accent: '#5a4fcf',
    maturity: 'foundation',
    status:
      'An early foundation: the Gekko CPU register state, a starter integer instruction interpreter, the memory bus, and a framebuffer stub. Not yet a functional emulator.',
    cpu: 'IBM PowerPC 750CXe (Gekko) @ 486 MHz, partially implemented: the full architectural state (32 GPRs, 32 FPRs, CR/XER/LR/CTR/PC, SPRs) plus a starter interpreter of ~20 integer opcodes (addi, add, or, cmp, branches, load/store, mfspr/mtspr, rlwinm, …). Everything else raises a Program exception. No floating-point or paired-single SIMD yet.',
    video:
      'Flipper GPU stub only: it owns an RGBA8888 framebuffer (640×480 NTSC) and clears it to black per frame. The Command Processor, Transform/Texture/Pixel engines, the embedded framebuffer, and Video Interface scanout are all absent.',
    audio: 'None — the DSP and audio interface are not implemented.',
    saves: 'None.',
    input: 'Stub only — set_keys() exists but does nothing; no real controller handling.',
    formats: ['.iso', '.gcm', '.dol'],
    features: [
      'Gekko architectural state (32 GPRs/FPRs, CR/XER/LR/CTR, exception model)',
      '24 MB main RAM (big-endian) with cached + uncached mirrors',
      '2 MB IPL boot ROM region',
      'Memory bus with BAT-window translation + region classification',
      '~20 PowerPC integer instructions with record-form (`.`) semantics',
      'Exception model with proper vector offsets for 8 exception types',
    ],
    limitations: [
      'Floating-point execution entirely absent (FPRs are storage only)',
      'No paired-single SIMD; no page-table MMU (static BAT model only)',
      'GPU pipeline absent — no CP FIFO, XF, TEV, PE, EFB, or VI scanout',
      'Hardware-register window is a flat backing store with no device behavior',
      'No DSP/ARAM audio, no DVD/DI, no memory-card (EXI) saves',
      'No real timing — the per-frame instruction budget is arbitrary',
    ],
    tests:
      '48 unit tests across CPU state, instruction execution, exception handling, big-endian memory (RAM/IPL), bus region translation, and the GPU framebuffer geometry.',
    testedGames: [],
  },
];

export function coreById(id: string | undefined): Core | undefined {
  return CORES.find((c) => c.id === id);
}
