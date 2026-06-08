// Affine BG renderer for Mode 1 BG2 and Mode 2 BG2/BG3.
//
// Affine BGs are ALWAYS 8bpp (1 byte per pixel = palette[0..255]).
// The map is a flat byte array where each byte is a single tile index
// (0..255). The map size is set by BGxCNT bits 14-15:
//   00 = 128x128 px = 16x16 tiles
//   01 = 256x256 px = 32x32 tiles
//   10 = 512x512 px = 64x64 tiles
//   11 = 1024x1024 px = 128x128 tiles
//
// Sampling uses the per-frame reference point (BGxX/BGxY, 28-bit signed
// 8.8 fixed) plus the per-row affine matrix (pA/pB/pC/pD, 8.8 signed):
//   src_x = pA * (px - 0) + pB * y + bgX
//   src_y = pC * px         + pD * y + bgY
// Each pixel: sample texel at (src_x >> 8, src_y >> 8) modulo map size.
// (Real hardware updates the reference points across scanlines via the
//  internal "current" registers, but for static affine BGs the per-line
//  matrix application below is a close enough approximation.)
const AFFINE_SIZE_TILES = [16, 32, 64, 128];
export function renderModeAffine(ppu, bg, y) {
    const ctrl = ppu.bgcnt[bg];
    const priority = ctrl & 3;
    const charBase = ((ctrl >> 2) & 3) * 0x4000;
    const screenBase = ((ctrl >> 8) & 0x1F) * 0x800;
    const sizeIdx = (ctrl >> 14) & 3;
    const mapTiles = AFFINE_SIZE_TILES[sizeIdx];
    const mapPx = mapTiles * 8;
    const wrap = (ctrl & 0x2000) !== 0;
    const refIdx = bg - 2; // 0 → BG2, 1 → BG3
    const pA = ppu.bgPA[refIdx];
    const pB = ppu.bgPB[refIdx];
    const pC = ppu.bgPC[refIdx];
    const pD = ppu.bgPD[refIdx];
    const refX = ppu.bgX[refIdx];
    const refY = ppu.bgY[refIdx];
    const layerHi = (bg << 16) | (priority << 18);
    const out = ppu.bgLine[bg];
    const vram = ppu.bus.vram;
    const pram16 = ppu.bus.pram16;
    // Compute starting source coords. Reference is 8.8 fixed-point signed.
    // src_x(0) = refX + 0*pA + y*pB
    // src_y(0) = refY + 0*pC + y*pD
    let srcX = (refX + pB * y) | 0;
    let srcY = (refY + pD * y) | 0;
    for (let x = 0; x < 240; x++) {
        let tx = srcX >> 8;
        let ty = srcY >> 8;
        srcX += pA;
        srcY += pC;
        if (wrap) {
            tx = ((tx % mapPx) + mapPx) % mapPx;
            ty = ((ty % mapPx) + mapPx) % mapPx;
        }
        else {
            if (tx < 0 || tx >= mapPx || ty < 0 || ty >= mapPx) {
                out[x] = 0x8000;
                continue;
            }
        }
        const tileX = tx >> 3;
        const tileY = ty >> 3;
        const inTileX = tx & 7;
        const inTileY = ty & 7;
        const mapAddr = screenBase + tileY * mapTiles + tileX;
        const tileIdx = vram[mapAddr];
        // Affine BG tile data is 8bpp, 64 bytes per tile, addressed from
        // charBase. Unlike text mode there's no flip bit and palette is
        // implicitly bank 0.
        const tileAddr = charBase + tileIdx * 64 + inTileY * 8 + inTileX;
        if (tileAddr >= 0x10000) {
            out[x] = 0x8000;
            continue;
        }
        const pix = vram[tileAddr];
        if (pix === 0) {
            out[x] = 0x8000;
            continue;
        }
        out[x] = (pram16[pix] & 0x7FFF) | layerHi;
    }
}
