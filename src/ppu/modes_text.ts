import type { Ppu } from './ppu';

// Text-mode BG renderer for one scanline.
// Outputs into ppu.bgLine[bg].

const SIZE_W = [256, 512, 256, 512];
const SIZE_H = [256, 256, 512, 512];

export function renderModeText(ppu: Ppu, bg: number, y: number): void {
  const ctrl = ppu.bgcnt[bg];
  const priority = ctrl & 3;
  const charBase = ((ctrl >> 2) & 3) * 0x4000;
  const screenBase = ((ctrl >> 8) & 0x1F) * 0x800;
  const colorMode8 = (ctrl & 0x80) !== 0;
  const sizeIdx = (ctrl >> 14) & 3;
  const mapW = SIZE_W[sizeIdx];
  const mapH = SIZE_H[sizeIdx];

  const hofs = ppu.bgHOFS[bg];
  const vofs = ppu.bgVOFS[bg];
  const yEff = (y + vofs) & (mapH - 1);

  const layerHi = (bg << 16) | (priority << 18);
  const out = ppu.bgLine[bg];
  const vram = ppu.bus.vram;
  const pram16 = ppu.bus.pram16;

  for (let x = 0; x < 240; x++) {
    const xEff = (x + hofs) & (mapW - 1);

    // Map quadrant selection (32x32 tiles per quadrant).
    let mapOff = screenBase;
    if (mapW === 512) mapOff += (xEff >= 256) ? 0x800 : 0;
    if (mapH === 512) mapOff += (yEff >= 256) ? (mapW === 512 ? 0x1000 : 0x800) : 0;

    const tileX = (xEff & 0xFF) >> 3;
    const tileY = (yEff & 0xFF) >> 3;
    const mapAddr = mapOff + (tileY * 32 + tileX) * 2;
    const entry = vram[mapAddr] | (vram[mapAddr + 1] << 8);
    const tileIdx = entry & 0x3FF;
    const hflip = (entry & 0x400) !== 0;
    const vflip = (entry & 0x800) !== 0;
    const palBank = (entry >>> 12) & 0xF;

    let inTileX = xEff & 7;
    let inTileY = yEff & 7;
    if (hflip) inTileX = 7 - inTileX;
    if (vflip) inTileY = 7 - inTileY;

    let pix: number;
    if (colorMode8) {
      const tileAddr = charBase + tileIdx * 64 + inTileY * 8 + inTileX;
      if (tileAddr >= 0x10000) { out[x] = 0x8000; continue; } // BG can't access OBJ tile area
      pix = vram[tileAddr];
      if (pix === 0) { out[x] = 0x8000; continue; }
      out[x] = (pram16[pix] & 0x7FFF) | layerHi;
    } else {
      const tileAddr = charBase + tileIdx * 32 + inTileY * 4 + (inTileX >> 1);
      if (tileAddr >= 0x10000) { out[x] = 0x8000; continue; }
      const byte = vram[tileAddr];
      pix = (inTileX & 1) ? (byte >> 4) : (byte & 0xF);
      if (pix === 0) { out[x] = 0x8000; continue; }
      out[x] = (pram16[palBank * 16 + pix] & 0x7FFF) | layerHi;
    }
  }
}
