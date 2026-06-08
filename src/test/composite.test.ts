// Compositor priority tests — verify the BG vs BG, OBJ vs BG, and
// blending interactions match the GBA spec. User reports "assets
// stacking wrong where they're drawing behind things" which would
// surface here as wrong-layer winning at a given priority pair.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';
import { compositeScanline } from '../ppu/composite';

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
  // The real renderScanline path fills these with 0x8000 (transparent
  // sentinel) before invoking the layer renderers. Mirror that for tests
  // so individual layers/pixels can be set in isolation.
  for (let b = 0; b < 4; b++) ppu.bgLine[b].fill(0x8000);
  ppu.objLine.fill(0x8000);
  return ppu;
}

// Encode a layer pixel: BGR555 color in bits 0-14, prio in 18-19, layer
// id in 16-17 (we tag using the BG array index already), opaque (no bit 15).
function bgPixel(color: number, prio: number): number {
  return (color & 0x7FFF) | (prio << 18);
}

// Same encoding for OBJ pixels; layer id is implicit (OBJ pixels live in
// objLine). Earlier we wrote `(4 << 16)` here, which incorrectly set the
// LSB of the priority field — fix matched in sprites.ts.
function objPixel(color: number, prio: number, semi = 0): number {
  return (color & 0x7FFF) | (prio << 18) | (semi << 20);
}

function pixelAt(ppu: Ppu, x: number, y: number): { r: number; g: number; b: number } {
  const off = (y * 240 + x) * 4;
  return { r: ppu.frame[off], g: ppu.frame[off + 1], b: ppu.frame[off + 2] };
}

const RED   = 0x001F;          // BGR555: red
const GREEN = 0x03E0;
const BLUE  = 0x7C00;
const WHITE = 0x7FFF;
const BLACK = 0x0000;

describe('Compositor: BG vs BG priority', () => {
  it('BG0 prio 0 wins over BG1 prio 1 (lower priority number wins)', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 0);
    ppu.bgLine[1][50] = bgPixel(GREEN, 1);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(200);  // red
    expect(p.g).toBeLessThan(20);
  });

  it('BG0 prio 1 wins over BG2 prio 1 (same priority → lower BG index wins)', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 1);
    ppu.bgLine[2][50] = bgPixel(GREEN, 1);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(200);
  });

  it('BG2 prio 0 wins over BG0 prio 1 (priority beats BG index)', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 1);
    ppu.bgLine[2][50] = bgPixel(GREEN, 0);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.g).toBeGreaterThan(200);
  });

  it('transparent BG pixels fall through to backdrop', () => {
    const ppu = makePpu();
    ppu.bus.pram16[0] = WHITE;
    // All BG transparent (bit 15 set in default).
    compositeScanline(ppu, 0, WHITE);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(240);
    expect(p.g).toBeGreaterThan(240);
    expect(p.b).toBeGreaterThan(240);
  });
});

describe('Compositor: OBJ vs BG priority', () => {
  it('OBJ prio 1 wins over BG0 prio 1 (tie → OBJ wins per spec)', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 1);
    ppu.objLine[50] = objPixel(GREEN, 1);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.g).toBeGreaterThan(200);
  });

  it('OBJ prio 2 LOSES to BG0 prio 1 (lower priority number wins)', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 1);
    ppu.objLine[50] = objPixel(GREEN, 2);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(200);
    expect(p.g).toBeLessThan(20);
  });

  it('OBJ prio 0 wins over everything', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 0);
    ppu.bgLine[1][50] = bgPixel(GREEN, 0);
    ppu.objLine[50] = objPixel(BLUE, 0);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.b).toBeGreaterThan(200);
  });

  it('OBJ prio 3 LOSES to BG3 prio 2', () => {
    const ppu = makePpu();
    ppu.bgLine[3][50] = bgPixel(RED, 2);
    ppu.objLine[50] = objPixel(GREEN, 3);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(200);
  });

  it('transparent OBJ falls through to BG underneath', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(RED, 1);
    // OBJ pixel transparent (default).
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    expect(p.r).toBeGreaterThan(200);
  });
});

describe('Compositor: alpha blending', () => {
  it('semi-transparent OBJ blends with the layer below', () => {
    const ppu = makePpu();
    ppu.bgLine[2][50] = bgPixel(RED, 2);
    ppu.objLine[50] = objPixel(BLUE, 1, /*semi*/ 1);
    // Configure BLDCNT/ALPHA: OBJ as top (bit 4), BG2 as bot (bit 2).
    // eva = 8, evb = 8 (50/50 mix).
    ppu.bldcnt = 0x10 | (0x4 << 8);
    ppu.bldalpha = 8 | (8 << 8);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    // Should be roughly half red half blue.
    expect(p.r).toBeGreaterThan(50);
    expect(p.b).toBeGreaterThan(50);
  });

  it('brighten (mode 2) lightens top layer when configured', () => {
    const ppu = makePpu();
    // Dark red BG0 → brighten 50% should give brighter red.
    ppu.bgLine[0][50] = bgPixel(0x000F, 0);  // half-bright red
    ppu.bldcnt = (2 << 6) | 0x01;  // mode 2 (brighten), top = BG0
    ppu.bldy = 8;  // 50% toward white
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    // R should be lifted from 15/31 toward 31. With evy=8/16, increase by 50% of (31-15) = 8.
    // So R = 15 + 8 = 23. Scaled to 8-bit: (23<<3)|(23>>2) ≈ 189.
    expect(p.r).toBeGreaterThan(170);
    expect(p.r).toBeLessThan(220);
  });

  it('darken (mode 3) darkens top layer toward black', () => {
    const ppu = makePpu();
    ppu.bgLine[0][50] = bgPixel(WHITE, 0);  // 31,31,31
    ppu.bldcnt = (3 << 6) | 0x01;
    ppu.bldy = 8;  // 50% toward black
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 50, 0);
    // R should drop from 31 to ~15. Scaled to 8-bit: ~127.
    expect(p.r).toBeLessThan(150);
    expect(p.r).toBeGreaterThan(100);
  });
});

describe('Compositor: windows', () => {
  it('WIN0 enables BG0 inside its rect, disables outside', () => {
    const ppu = makePpu();
    // Put RED on BG0 across the whole scanline.
    for (let x = 0; x < 240; x++) ppu.bgLine[0][x] = bgPixel(RED, 0);
    // Enable WIN0, with BG0 enabled INSIDE, nothing enabled OUTSIDE.
    ppu.dispcnt = 0x2000;  // WIN0 on
    ppu.win0H = (50 << 8) | 100;   // x range [50, 100)
    ppu.win0V = (0 << 8) | 160;    // y range full screen
    ppu.winIn = 0x01;              // inside WIN0: BG0 allowed
    ppu.winOut = 0x00;             // outside: nothing allowed (only backdrop)
    ppu.bus.pram16[0] = WHITE;     // backdrop = white
    compositeScanline(ppu, 10, WHITE);
    // Inside: red.
    const inside = pixelAt(ppu, 75, 10);
    expect(inside.r).toBeGreaterThan(240);
    expect(inside.g).toBeLessThan(20);
    // Outside: backdrop (white).
    const outside = pixelAt(ppu, 25, 10);
    expect(outside.r).toBeGreaterThan(240);
    expect(outside.g).toBeGreaterThan(240);
    expect(outside.b).toBeGreaterThan(240);
  });

  it('WIN0 vertical bounds clip — outside Y range is "outside"', () => {
    const ppu = makePpu();
    for (let x = 0; x < 240; x++) ppu.bgLine[0][x] = bgPixel(RED, 0);
    ppu.dispcnt = 0x2000;
    ppu.win0H = (0 << 8) | 240;    // full X
    ppu.win0V = (50 << 8) | 100;   // y [50, 100)
    ppu.winIn = 0x01;
    ppu.winOut = 0x00;
    ppu.bus.pram16[0] = WHITE;
    // Inside Y range.
    compositeScanline(ppu, 75, WHITE);
    expect(pixelAt(ppu, 100, 75).r).toBeGreaterThan(240);
    // Outside Y range.
    compositeScanline(ppu, 110, WHITE);
    expect(pixelAt(ppu, 100, 110).r).toBeGreaterThan(240);
    expect(pixelAt(ppu, 100, 110).g).toBeGreaterThan(240);  // white
  });

  it('WIN1 has lower priority than WIN0 (WIN0 wins at overlap)', () => {
    const ppu = makePpu();
    for (let x = 0; x < 240; x++) ppu.bgLine[0][x] = bgPixel(RED, 0);
    ppu.dispcnt = 0x6000;  // WIN0 + WIN1 on
    ppu.win0H = (10 << 8) | 50;
    ppu.win0V = (0 << 8) | 160;
    ppu.win1H = (40 << 8) | 100;
    ppu.win1V = (0 << 8) | 160;
    ppu.winIn = 0x01 | (0x00 << 8);  // WIN0 allows BG0, WIN1 allows nothing
    ppu.winOut = 0x00;
    ppu.bus.pram16[0] = WHITE;
    compositeScanline(ppu, 10, WHITE);
    // x=45 is in both windows — WIN0 wins, BG0 visible.
    expect(pixelAt(ppu, 45, 10).r).toBeGreaterThan(240);
    expect(pixelAt(ppu, 45, 10).g).toBeLessThan(20);
    // x=75 is only in WIN1 — backdrop (because WIN1 disables BG0).
    expect(pixelAt(ppu, 75, 10).g).toBeGreaterThan(240);
  });
});

describe('Compositor: BGR555 → RGBA conversion accuracy', () => {
  it('pure red (0x001F) decodes to full red', () => {
    const ppu = makePpu();
    ppu.bgLine[0][0] = bgPixel(0x001F, 0);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 0, 0);
    expect(p.r).toBeGreaterThan(240);
    expect(p.g).toBe(0);
    expect(p.b).toBe(0);
  });

  it('pure green (0x03E0) decodes to full green', () => {
    const ppu = makePpu();
    ppu.bgLine[0][0] = bgPixel(0x03E0, 0);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 0, 0);
    expect(p.r).toBe(0);
    expect(p.g).toBeGreaterThan(240);
    expect(p.b).toBe(0);
  });

  it('pure blue (0x7C00) decodes to full blue', () => {
    const ppu = makePpu();
    ppu.bgLine[0][0] = bgPixel(0x7C00, 0);
    compositeScanline(ppu, 0, BLACK);
    const p = pixelAt(ppu, 0, 0);
    expect(p.r).toBe(0);
    expect(p.g).toBe(0);
    expect(p.b).toBeGreaterThan(240);
  });
});
