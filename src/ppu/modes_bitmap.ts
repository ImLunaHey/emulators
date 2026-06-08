import type { Ppu } from './ppu';

// BG2 in bitmap modes (3, 4, 5) is always AFFINE — same matrix +
// reference-point math as Mode 1 BG2 / Mode 2 BG2-3, just with a flat
// linear bitmap framebuffer instead of a tile+map fetch. Many homebrew
// engines (Quake, voxel renderers, raycasters) set PA / PD to non-
// identity values to stretch a sub-region of the bitmap across the
// screen. Without applying the matrix, the unused regions of the
// bitmap (which often contain leftover scratch data) leak through as
// "noise" alongside the intended scene.
//
// All three modes use the same outer scaffold: walk per-pixel through
// the affine source coords, then sample at (sx>>8, sy>>8) from the
// mode-specific bitmap layout. The wrap bit (BG2CNT 0x2000) decides
// whether out-of-range samples wrap or read transparent.

interface BitmapSampler {
  sample(ppu: Ppu, sx: number, sy: number, layerHi: number): number;
  width: number;
  height: number;
}

// Mode 3: 240x160 BGR555 direct color, no double buffering.
const mode3Sampler: BitmapSampler = {
  width: 240, height: 160,
  sample(ppu, sx, sy, layerHi) {
    if (sx < 0 || sx >= 240 || sy < 0 || sy >= 160) return 0x8000;
    return (ppu.bus.vram16[sy * 240 + sx] & 0x7FFF) | layerHi;
  },
};

// Mode 4: 240x160 paletted, double-buffered.
const mode4Sampler: BitmapSampler = {
  width: 240, height: 160,
  sample(ppu, sx, sy, layerHi) {
    if (sx < 0 || sx >= 240 || sy < 0 || sy >= 160) return 0x8000;
    const page = (ppu.dispcnt & 0x10) ? 0xA000 : 0x0000;
    const idx = ppu.bus.vram[page + sy * 240 + sx];
    if (idx === 0) return 0x8000;
    return (ppu.bus.pram16[idx] & 0x7FFF) | layerHi;
  },
};

// Mode 5: 160x128 BGR555 direct, double-buffered.
const mode5Sampler: BitmapSampler = {
  width: 160, height: 128,
  sample(ppu, sx, sy, layerHi) {
    if (sx < 0 || sx >= 160 || sy < 0 || sy >= 128) return 0x8000;
    const page = (ppu.dispcnt & 0x10) ? 0x5000 : 0x0000;
    return (ppu.bus.vram16[(page >>> 1) + sy * 160 + sx] & 0x7FFF) | layerHi;
  },
};

function renderBitmapAffine(ppu: Ppu, y: number, sampler: BitmapSampler): void {
  const layerHi = (2 << 16) | ((ppu.bgcnt[2] & 3) << 18);
  const out = ppu.bgLine[2];
  const pA = ppu.bgPA[0];
  const pB = ppu.bgPB[0];
  const pC = ppu.bgPC[0];
  const pD = ppu.bgPD[0];
  const refX = ppu.bgX[0];
  const refY = ppu.bgY[0];
  // Per-scanline source coord: ref + Y * (pB, pD), then step by (pA, pC)
  // each pixel. Coords are 8.8 fixed, so sx >> 8 / sy >> 8 give the
  // bitmap-space integer sample position.
  let sx = (refX + pB * y) | 0;
  let sy = (refY + pD * y) | 0;
  for (let x = 0; x < 240; x++) {
    out[x] = sampler.sample(ppu, sx >> 8, sy >> 8, layerHi);
    sx += pA;
    sy += pC;
  }
}

export function renderModeBitmap3(ppu: Ppu, y: number): void { renderBitmapAffine(ppu, y, mode3Sampler); }
export function renderModeBitmap4(ppu: Ppu, y: number): void { renderBitmapAffine(ppu, y, mode4Sampler); }
export function renderModeBitmap5(ppu: Ppu, y: number): void { renderBitmapAffine(ppu, y, mode5Sampler); }
