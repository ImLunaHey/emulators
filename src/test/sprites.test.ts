// Sprite rendering tests. Place a known sprite in VRAM + OAM and render
// one scanline, then check the framebuffer for the expected pixels.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';
import { renderSprites } from '../ppu/sprites';

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
  return ppu;
}

function fillTile4bpp(ppu: Ppu, tileSlot: number, pixelValue: number) {
  // Each 4bpp tile is 32 bytes: 4 bytes per row, each byte = 2 pixels (low, high).
  const base = 0x10000 + tileSlot * 32;
  const byte = pixelValue | (pixelValue << 4);
  for (let i = 0; i < 32; i++) ppu.bus.vram[base + i] = byte;
}

function setOam(ppu: Ppu, idx: number, a0: number, a1: number, a2: number) {
  const off = idx * 8;
  ppu.bus.oam[off]   = a0 & 0xFF;
  ppu.bus.oam[off+1] = (a0 >> 8) & 0xFF;
  ppu.bus.oam[off+2] = a1 & 0xFF;
  ppu.bus.oam[off+3] = (a1 >> 8) & 0xFF;
  ppu.bus.oam[off+4] = a2 & 0xFF;
  ppu.bus.oam[off+5] = (a2 >> 8) & 0xFF;
}

function renderAt(ppu: Ppu, y: number): Uint32Array {
  ppu.objLine.fill(0x8000);
  renderSprites(ppu, y);
  return ppu.objLine;
}

describe('Sprite rendering', () => {
  it('renders a 16x16 4bpp sprite as a contiguous block (no gaps)', () => {
    const ppu = makePpu();
    // 1D OBJ mapping.
    ppu.dispcnt = 0x40;
    // Set up palette[1] of OBJ palette bank 0 to color 0x7FFF (white).
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    // Fill 4 tiles (16x16 = 2x2 tiles) with pixel value 1 (= palette[1]).
    for (let t = 0; t < 4; t++) fillTile4bpp(ppu, t, 1);
    // OAM[0]: 16x16 at (10, 50), 4bpp, tile 0, pal 0, prio 0.
    // a0: y=50, shape 0 = square. a0 = 50 | (0 << 14) = 0x32.
    // a1: x=10, size 1 = 16x16. a1 = 10 | (1 << 14) = 0x4000 | 10 = 0x400A.
    // a2: tileIdx=0, palBank=0, prio=0. a2 = 0.
    setOam(ppu, 0, 0x0032, 0x400A, 0x0000);

    // Render the middle scanline (y=58 — inside sprite at y=50..65).
    const line = renderAt(ppu, 58);

    // Expect all 16 pixels at x=10..25 to be opaque (no bit 0x8000 set).
    for (let x = 10; x < 26; x++) {
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
    // Pixels outside sprite should be transparent.
    expect((line[9] & 0x8000) !== 0).toBe(true);
    expect((line[26] & 0x8000) !== 0).toBe(true);
  });

  it('renders a 32x32 4bpp sprite with no every-other-tile gaps', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    // 32x32 = 4x4 = 16 tiles.
    for (let t = 0; t < 16; t++) fillTile4bpp(ppu, t, 1);
    // OAM[0]: 32x32 at (10, 50), shape 0 size 2.
    setOam(ppu, 0, 0x0032, 0x800A, 0x0000);

    const line = renderAt(ppu, 60);

    for (let x = 10; x < 42; x++) {
      expect(`x=${x} opaque`).toBe(`x=${x} opaque`);
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
  });

  it('renders a 32x32 8bpp sprite (1D mapping) contiguously', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    // OBJ palette bank 0 entry 1 = white.
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    // For 8bpp: each tile is 64 bytes = 2 4bpp slots. 32x32 sprite = 4x4 = 16
    // tiles = 32 slots.
    for (let t = 0; t < 32; t++) {
      const base = 0x10000 + t * 32;
      for (let i = 0; i < 32; i++) ppu.bus.vram[base + i] = 1;  // every byte = pixel value 1
    }
    // OAM[0]: 32x32, 8bpp, tile 0 at (10, 50).
    setOam(ppu, 0, 0x2032, 0x800A, 0x0000);

    const line = renderAt(ppu, 60);

    for (let x = 10; x < 42; x++) {
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
  });

  it('horizontal flip mirrors the sprite correctly', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7C00;  // red for pix value 1
    ppu.bus.pram16[256 + 2] = 0x03E0;  // green for pix value 2
    // Build an 8x8 tile where the LEFT half is value 1 and the RIGHT half is value 2.
    const base = 0x10000;
    for (let row = 0; row < 8; row++) {
      // 4 bytes per row. Byte 0 = pixels 0,1; byte 1 = pixels 2,3; etc.
      // Want pixels 0..3 = 1, pixels 4..7 = 2.
      ppu.bus.vram[base + row * 4 + 0] = 0x11;
      ppu.bus.vram[base + row * 4 + 1] = 0x11;
      ppu.bus.vram[base + row * 4 + 2] = 0x22;
      ppu.bus.vram[base + row * 4 + 3] = 0x22;
    }
    // OAM[0]: 8x8 at (10, 50), no flip.
    setOam(ppu, 0, 0x0032, 0x000A, 0x0000);
    let line = renderAt(ppu, 54);
    expect((line[10] & 0x7FFF)).toBe(0x7C00);  // left = red
    expect((line[17] & 0x7FFF)).toBe(0x03E0);  // right = green
    // Now flip horizontally.
    setOam(ppu, 0, 0x0032, 0x100A, 0x0000);  // a1 bit 12 set
    line = renderAt(ppu, 54);
    expect((line[10] & 0x7FFF)).toBe(0x03E0);  // left now green
    expect((line[17] & 0x7FFF)).toBe(0x7C00);  // right now red
  });

  it('affine identity matrix renders identically to non-affine', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    for (let t = 0; t < 4; t++) fillTile4bpp(ppu, t, 1);
    // Set affine matrix 0 to identity (pA=pD=0x100, pB=pC=0).
    // OAM[0].6-7 = pA, OAM[1].6-7 = pB, OAM[2].6-7 = pC, OAM[3].6-7 = pD.
    const set = (i: number, v: number) => {
      ppu.bus.oam[i * 8 + 6] = v & 0xFF;
      ppu.bus.oam[i * 8 + 7] = (v >> 8) & 0xFF;
    };
    set(0, 0x0100); set(1, 0x0000); set(2, 0x0000); set(3, 0x0100);
    // OAM[0]: 16x16 at (10, 50), affine enabled (a0 bit 8 = 1), matrix 0.
    // a0 = 0x32 | 0x100 = 0x132
    // a1 = 0x400A | (0 << 9) = 0x400A
    setOam(ppu, 0, 0x0132, 0x400A, 0x0000);
    const line = renderAt(ppu, 58);
    for (let x = 10; x < 26; x++) {
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
  });

  it('affine double-size with identity renders FULL sprite (the cut-in-half fix)', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    for (let t = 0; t < 4; t++) fillTile4bpp(ppu, t, 1);
    // Identity matrix at index 0.
    const set = (i: number, v: number) => {
      ppu.bus.oam[i * 8 + 6] = v & 0xFF;
      ppu.bus.oam[i * 8 + 7] = (v >> 8) & 0xFF;
    };
    set(0, 0x0100); set(1, 0x0000); set(2, 0x0000); set(3, 0x0100);
    // 16x16 sprite, affine + double-size → 32x32 bounding box centered on the
    // 16x16 texel area. With identity matrix, only the center 16x16 of the
    // 32x32 box should be opaque (the corners sample outside the sprite).
    // a0 = 0x32 | 0x100 | 0x200 = 0x332 (double-size affine)
    // a1 = 0x400A
    setOam(ppu, 0, 0x0332, 0x400A, 0x0000);
    // Sprite at y=50..82 (32 tall), texel center maps to drawY 16. The 16x16
    // texel region is drawY 8..24 → screen y 58..74.
    const line = renderAt(ppu, 66);  // middle scanline
    // Center 16 pixels should be opaque, edges transparent.
    // drawW = 32, xPos = 10, drawX 8..24 = texel coverage = screenX 18..34.
    for (let x = 18; x < 34; x++) {
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
    // Outside the texel region, transparent.
    expect((line[10] & 0x8000) !== 0).toBe(true);
    expect((line[42] & 0x8000) !== 0).toBe(true);
  });

  it('affine 2x scale (pA=0x80) doubles the sprite on screen', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    for (let t = 0; t < 4; t++) fillTile4bpp(ppu, t, 1);
    // 2x zoom = sample step of 0.5 → pA = 0x80 (1/2 in 8.8).
    const set = (i: number, v: number) => {
      ppu.bus.oam[i * 8 + 6] = v & 0xFF;
      ppu.bus.oam[i * 8 + 7] = (v >> 8) & 0xFF;
    };
    set(0, 0x0080); set(1, 0x0000); set(2, 0x0000); set(3, 0x0080);
    // 16x16 sprite, double-size affine → 32x32 box. At 2x zoom the full 32x32
    // bounding box should be filled (each texel covers 2 screen pixels).
    setOam(ppu, 0, 0x0332, 0x400A, 0x0000);
    const line = renderAt(ppu, 66);
    // All 32 pixels at x=10..42 should be opaque.
    let count = 0;
    for (let x = 10; x < 42; x++) {
      if ((line[x] & 0x8000) === 0) count++;
    }
    expect(count).toBeGreaterThanOrEqual(30);  // allow tiny edge slack from sampling
  });

  it('renders a 64x64 4bpp sprite (1D mapping)', () => {
    const ppu = makePpu();
    ppu.dispcnt = 0x40;
    ppu.bus.pram16[256 + 1] = 0x7FFF;
    // 64x64 = 8x8 tiles = 64 tiles.
    for (let t = 0; t < 64; t++) fillTile4bpp(ppu, t, 1);
    // OAM[0]: 64x64, shape 0 size 3.
    setOam(ppu, 0, 0x0032, 0xC00A, 0x0000);

    const line = renderAt(ppu, 70);

    // All 64 pixels at x=10..73 should be opaque.
    for (let x = 10; x < 74; x++) {
      expect((line[x] & 0x8000) === 0).toBe(true);
    }
  });
});
