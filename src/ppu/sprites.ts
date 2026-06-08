import type { Ppu } from './ppu';

// Sprite size table — indexed by (shape, size): width then height.
const SIZE_W = [
  [8,  16, 32, 64],
  [16, 32, 32, 64],
  [8,  8,  16, 32],
];
const SIZE_H = [
  [8,  16, 32, 64],
  [8,  8,  16, 32],
  [16, 32, 32, 64],
];

export function renderSprites(ppu: Ppu, y: number): void {
  const oam = ppu.bus.oam;
  const vram = ppu.bus.vram;
  const pram16 = ppu.bus.pram16;
  const objMappingLinear = (ppu.dispcnt & 0x40) !== 0;
  const out = ppu.objLine;

  // OAM has 128 entries × 8 bytes (3 attrs + affine column).
  for (let i = 0; i < 128; i++) {
    const base = i * 8;
    const a0 = oam[base] | (oam[base + 1] << 8);
    const a1 = oam[base + 2] | (oam[base + 3] << 8);
    const a2 = oam[base + 4] | (oam[base + 5] << 8);

    const mode = (a0 >> 10) & 3;          // 0=normal, 1=semi-trans, 2=window, 3=prohibited
    const disabledBit = (a0 & 0x300) === 0x200; // bit 9 set, bit 8 clear → disabled non-affine
    if (disabledBit) continue;

    const shape = (a0 >> 14) & 3;
    const size = (a1 >> 14) & 3;
    if (shape === 3) continue;
    let w = SIZE_W[shape][size];
    let h = SIZE_H[shape][size];

    const doubleSize = (a0 & 0x200) !== 0 && (a0 & 0x100) !== 0;
    const drawW = doubleSize ? w * 2 : w;
    const drawH = doubleSize ? h * 2 : h;

    let yPos = a0 & 0xFF;
    if (yPos >= 160) yPos -= 256;
    if (y < yPos || y >= yPos + drawH) continue;

    let xPos = a1 & 0x1FF;
    if (xPos >= 240) xPos -= 512;
    if (xPos + drawW <= 0) continue;

    const color8 = (a0 & 0x2000) !== 0;
    const palBank = (a2 >> 12) & 0xF;
    const priority = (a2 >> 10) & 3;
    const tileIdx = a2 & 0x3FF;
    const hflip = !color8 && false; // ignored for affine; handled below
    const hflipFlag = (a1 & 0x1000) !== 0;
    const vflipFlag = (a1 & 0x2000) !== 0;

    const semi = mode === 1;
    const objWindow = mode === 2;
    const layerHi = (4 << 16) | (priority << 18) | (semi ? (1 << 20) : 0) | (objWindow ? (1 << 21) : 0);

    let inSpriteY = y - yPos;
    if (vflipFlag) inSpriteY = h - 1 - inSpriteY;
    const tileRow = inSpriteY >> 3;
    const inTileY = inSpriteY & 7;

    const tileBase = 0x10000;
    const tilesPerTile = color8 ? 2 : 1; // 8bpp tiles occupy 2 adjacent 4bpp slots
    // Row stride differs between 1D and 2D mapping:
    //   1D: each row of the sprite is laid out linearly after the previous,
    //       so row stride = (sprite_width_in_tiles) * tilesPerTile.
    //   2D: tiles are arranged in a fixed 32-wide grid in the OBJ tile area,
    //       so row stride is ALWAYS 32 slots regardless of color depth.
    //       Within a row, an 8bpp tile still occupies 2 slots (col step = 2).
    const rowStride = objMappingLinear ? (w >> 3) * tilesPerTile : 32;

    for (let px = 0; px < w; px++) {
      const screenX = xPos + px;
      if (screenX < 0 || screenX >= 240) continue;
      let inSpriteX = px;
      if (hflipFlag) inSpriteX = w - 1 - inSpriteX;
      const tileCol = inSpriteX >> 3;
      const inTileX = inSpriteX & 7;

      const baseTile = tileIdx + tileRow * rowStride + tileCol * tilesPerTile;
      const tileAddr = tileBase + (baseTile & 0x3FF) * 32 + inTileY * (color8 ? 8 : 4) + (color8 ? inTileX : (inTileX >> 1));
      if (tileAddr >= 0x18000) continue;

      let pix: number;
      if (color8) {
        pix = vram[tileAddr];
        if (pix === 0) continue;
        const cur = out[screenX];
        if ((cur & 0x8000) === 0 && (((cur >> 18) & 3) <= priority)) continue;
        out[screenX] = (pram16[256 + pix] & 0x7FFF) | layerHi;
      } else {
        const byte = vram[tileAddr];
        pix = (inTileX & 1) ? (byte >> 4) : (byte & 0xF);
        if (pix === 0) continue;
        const cur = out[screenX];
        if ((cur & 0x8000) === 0 && (((cur >> 18) & 3) <= priority)) continue;
        out[screenX] = (pram16[256 + palBank * 16 + pix] & 0x7FFF) | layerHi;
      }
    }
  }
}
