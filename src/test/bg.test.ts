// Background rendering tests — Mode 0 (text/tile) and Mode 4 (paletted
// bitmap). Tests build a single tile + map entry and render one
// scanline, checking the resulting layer line.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';
import { renderModeText } from '../ppu/modes_text';
import { renderModeBitmap3, renderModeBitmap4 } from '../ppu/modes_bitmap';

function makePpu(): Ppu {
  const bus = new Bus();
  const irq = new Irq();
  const keypad = new Keypad();
  const dma = new Dma(bus, irq);
  const timers = new Timers(irq);
  const ppu = new Ppu(bus, irq, dma);
  const cpu = new Cpu(bus);
  const io = new Io(bus, ppu, dma, timers, irq, keypad, cpu);
  bus.attachIo(io);
  bus.attachSave({ read: () => 0xFF, write: () => {} });
  bus.loadRom(new Uint8Array(0x100));
  for (let b = 0; b < 4; b++) ppu.bgLine[b].fill(0x8000);
  return ppu;
}

// Fill a 4bpp tile at slot `tileSlot` in VRAM with the same nibble pair
// throughout (pixel value `v` everywhere in the tile).
function fillTile4bpp(ppu: Ppu, tileSlot: number, v: number) {
  const base = tileSlot * 32;
  const byte = v | (v << 4);
  for (let i = 0; i < 32; i++) ppu.bus.vram[base + i] = byte;
}

describe('BG text mode rendering', () => {
  it('renders a single tile at offset 0', () => {
    const ppu = makePpu();
    // BG0 control: char base 0, map base 0, prio 0, 4bpp, screen size 0.
    ppu.bgcnt[0] = 0;
    fillTile4bpp(ppu, 0, 1);
    // Map entry at index 0: tile 0, pal 0, no flip.
    ppu.bus.vram[0x800] = 0;  // Wait — wrong. Map base is 0 (= byte offset 0x0000).
    // Actually with charBase=0 and mapBase=0, both share VRAM 0-... Let's
    // separate them: tile data at slot 0..N, map at byte 0x800. But map
    // base is read from bgcnt bits 8-12 (0-31, units of 0x800).
    // For simplicity put map at 0x800 (mapBase 1).
    ppu.bgcnt[0] = (1 << 8);  // mapBase = 1 (= 0x800)
    ppu.bus.vram[0x800] = 0;
    ppu.bus.vram[0x801] = 0;
    // OBJ palette doesn't matter; use BG palette[0].
    ppu.bus.pram16[1] = 0x7FFF;  // BG palette entry 1 = white

    renderModeText(ppu, 0, 0);

    // First 8 pixels of scanline 0 should be opaque (tile 0, pixel value 1).
    for (let x = 0; x < 8; x++) {
      expect((ppu.bgLine[0][x] & 0x8000) === 0).toBe(true);
    }
  });

  it('horizontal flip swaps left and right halves of a tile', () => {
    const ppu = makePpu();
    ppu.bgcnt[0] = (1 << 8);
    // Tile with LEFT half (pixels 0-3) = value 1, RIGHT half = value 2.
    const base = 0;
    for (let row = 0; row < 8; row++) {
      ppu.bus.vram[base + row * 4 + 0] = 0x11;
      ppu.bus.vram[base + row * 4 + 1] = 0x11;
      ppu.bus.vram[base + row * 4 + 2] = 0x22;
      ppu.bus.vram[base + row * 4 + 3] = 0x22;
    }
    ppu.bus.pram16[1] = 0x7C00;  // blue
    ppu.bus.pram16[2] = 0x03E0;  // green
    // Map entry: tile 0, hflip=1, pal 0.
    ppu.bus.vram[0x800] = 0;
    ppu.bus.vram[0x801] = 0x04;  // bit 10 → hflip
    renderModeText(ppu, 0, 0);
    // With hflip, pixel 0 should be the original pixel 7 = green.
    expect((ppu.bgLine[0][0] & 0x7FFF)).toBe(0x03E0);
    // Pixel 7 should be the original pixel 0 = blue.
    expect((ppu.bgLine[0][7] & 0x7FFF)).toBe(0x7C00);
  });

  it('respects HOFS scroll offset', () => {
    const ppu = makePpu();
    ppu.bgcnt[0] = (1 << 8);
    ppu.bgHOFS[0] = 4;
    fillTile4bpp(ppu, 0, 1);
    fillTile4bpp(ppu, 1, 2);
    ppu.bus.pram16[1] = 0x7C00;
    ppu.bus.pram16[2] = 0x03E0;
    // Map entries: tile 0 at column 0, tile 1 at column 1.
    ppu.bus.vram[0x800] = 0; ppu.bus.vram[0x801] = 0;
    ppu.bus.vram[0x802] = 1; ppu.bus.vram[0x803] = 0;
    renderModeText(ppu, 0, 0);
    // With HOFS=4, scanline starts 4 pixels into tile 0.
    // Pixels 0..3 = tile 0 (blue), 4..11 = tile 1 (green), 12..19 = tile 2.
    expect((ppu.bgLine[0][0] & 0x7FFF)).toBe(0x7C00);
    expect((ppu.bgLine[0][3] & 0x7FFF)).toBe(0x7C00);
    expect((ppu.bgLine[0][4] & 0x7FFF)).toBe(0x03E0);
    expect((ppu.bgLine[0][11] & 0x7FFF)).toBe(0x03E0);
  });

  it('respects VOFS scroll offset (renders a different row of tile data)', () => {
    const ppu = makePpu();
    ppu.bgcnt[0] = (1 << 8);
    ppu.bgVOFS[0] = 8;
    // Map entry at the SECOND row (y=8 means second tile row in y direction).
    // mapBase = 0x800, row stride = 32 entries * 2 bytes = 64.
    ppu.bus.vram[0x800 + 64] = 5;  // tile 5
    ppu.bus.vram[0x800 + 65] = 0;
    fillTile4bpp(ppu, 5, 3);
    ppu.bus.pram16[3] = 0x7FFF;
    renderModeText(ppu, 0, 0);
    expect((ppu.bgLine[0][0] & 0x7FFF)).toBe(0x7FFF);
  });

  it('palette bank selects different palette in 4bpp mode', () => {
    const ppu = makePpu();
    ppu.bgcnt[0] = (1 << 8);
    fillTile4bpp(ppu, 0, 1);
    // Palette bank 0 entry 1 = red.
    ppu.bus.pram16[1] = 0x001F;
    // Palette bank 3 entry 1 = blue.
    ppu.bus.pram16[3 * 16 + 1] = 0x7C00;
    // Map entry: tile 0, pal 3.
    ppu.bus.vram[0x800] = 0;
    ppu.bus.vram[0x801] = 0x30;  // palette bank in high nibble of byte 1
    renderModeText(ppu, 0, 0);
    expect((ppu.bgLine[0][0] & 0x7FFF)).toBe(0x7C00);
  });
});

describe('Bitmap mode 4 (paletted 240x160)', () => {
  it('renders palette-indexed pixels for one scanline', () => {
    const ppu = makePpu();
    ppu.bus.pram16[5] = 0x03E0;  // green
    // Set frame 0 (DISPCNT bit 4 = 0).
    ppu.dispcnt = 0;
    // Pixel at (10, 30) = palette index 5.
    ppu.bus.vram[30 * 240 + 10] = 5;
    renderModeBitmap4(ppu, 30);
    expect((ppu.bgLine[2][10] & 0x7FFF)).toBe(0x03E0);
  });

  it('respects double-buffer page select (DISPCNT bit 4)', () => {
    const ppu = makePpu();
    ppu.bus.pram16[7] = 0x7C00;  // blue
    ppu.dispcnt = 0x10;  // page 1 active
    // Page 1 starts at VRAM 0xA000.
    ppu.bus.vram[0xA000 + 50 * 240 + 5] = 7;
    renderModeBitmap4(ppu, 50);
    expect((ppu.bgLine[2][5] & 0x7FFF)).toBe(0x7C00);
  });
});

describe('Bitmap mode 3 (BGR555 240x160)', () => {
  it('renders direct color', () => {
    const ppu = makePpu();
    // Pixel at (10, 30) = green direct BGR555.
    ppu.bus.vram[(30 * 240 + 10) * 2 + 0] = 0xE0;
    ppu.bus.vram[(30 * 240 + 10) * 2 + 1] = 0x03;  // 0x03E0
    renderModeBitmap3(ppu, 30);
    expect((ppu.bgLine[2][10] & 0x7FFF)).toBe(0x03E0);
  });
});
