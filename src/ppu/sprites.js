// Sprite size table — indexed by (shape, size): width then height.
const SIZE_W = [
    [8, 16, 32, 64],
    [16, 32, 32, 64],
    [8, 8, 16, 32],
];
const SIZE_H = [
    [8, 16, 32, 64],
    [8, 8, 16, 32],
    [16, 32, 32, 64],
];
export function renderSprites(ppu, y) {
    const oam = ppu.bus.oam;
    const vram = ppu.bus.vram;
    const pram16 = ppu.bus.pram16;
    const objMappingLinear = (ppu.dispcnt & 0x40) !== 0;
    const out = ppu.objLine;
    for (let i = 0; i < 128; i++) {
        const base = i * 8;
        const a0 = oam[base] | (oam[base + 1] << 8);
        const a1 = oam[base + 2] | (oam[base + 3] << 8);
        const a2 = oam[base + 4] | (oam[base + 5] << 8);
        const mode = (a0 >> 10) & 3; // 0=normal, 1=semi-trans, 2=window, 3=prohibited
        const affine = (a0 & 0x100) !== 0;
        const disabledBit = !affine && (a0 & 0x200) !== 0; // bit 9 only means "disabled" when bit 8 clear
        if (disabledBit)
            continue;
        const shape = (a0 >> 14) & 3;
        const size = (a1 >> 14) & 3;
        if (shape === 3)
            continue;
        const w = SIZE_W[shape][size];
        const h = SIZE_H[shape][size];
        // For affine sprites with bit 9 set ("double size"), the bounding box
        // on screen is 2x the sprite size (gives the matrix room for rotation).
        // The texel coords still range 0..w / 0..h — we just sample over a
        // wider screen window.
        const doubleSize = affine && (a0 & 0x200) !== 0;
        const drawW = doubleSize ? w * 2 : w;
        const drawH = doubleSize ? h * 2 : h;
        let yPos = a0 & 0xFF;
        if (yPos >= 160)
            yPos -= 256;
        if (y < yPos || y >= yPos + drawH)
            continue;
        let xPos = a1 & 0x1FF;
        if (xPos >= 240)
            xPos -= 512;
        if (xPos + drawW <= 0)
            continue;
        const color8 = (a0 & 0x2000) !== 0;
        const palBank = (a2 >> 12) & 0xF;
        const priority = (a2 >> 10) & 3;
        const tileIdx = a2 & 0x3FF;
        const semi = mode === 1;
        const objWindow = mode === 2;
        // No layer bits — OBJ pixels are identified by being in objLine.
        // The OLD code had `4 << 16` here, but layer field is only 2 bits
        // (16-17), so the high bit of 4 spilled into bit 18 (priority's LSB)
        // and corrupted every sprite priority value: prio 0→1, prio 2→3, etc.
        // That manifested as sprites layering wrong vs BGs and other sprites.
        const layerHi = (priority << 18) | (semi ? (1 << 20) : 0) | (objWindow ? (1 << 21) : 0);
        const tileBase = 0x10000;
        const tilesPerTile = color8 ? 2 : 1;
        const rowStride = objMappingLinear ? (w >> 3) * tilesPerTile : 32;
        // Affine path: bits 9-13 of a1 are the matrix index. Pull pA/pB/pC/pD
        // from the affine column bytes 6-7 of OAM entries 4*idx + [0..3].
        let pA = 0x100, pB = 0, pC = 0, pD = 0x100; // identity (8.8 fixed)
        if (affine) {
            const matIdx = (a1 >> 9) & 0x1F;
            const mb = matIdx * 32;
            pA = (oam[mb + 6] | (oam[mb + 7] << 8)) << 16 >> 16;
            pB = (oam[mb + 14] | (oam[mb + 15] << 8)) << 16 >> 16;
            pC = (oam[mb + 22] | (oam[mb + 23] << 8)) << 16 >> 16;
            pD = (oam[mb + 30] | (oam[mb + 31] << 8)) << 16 >> 16;
        }
        const inSpriteY = y - yPos;
        const cx = drawW >> 1;
        const cy = drawH >> 1;
        const halfW = w >> 1;
        const halfH = h >> 1;
        // Non-affine fast path: simple tile fetch with hflip/vflip.
        if (!affine) {
            const hflipFlag = (a1 & 0x1000) !== 0;
            const vflipFlag = (a1 & 0x2000) !== 0;
            let ty = inSpriteY;
            if (vflipFlag)
                ty = h - 1 - ty;
            const tileRow = ty >> 3;
            const inTileY = ty & 7;
            for (let px = 0; px < w; px++) {
                const screenX = xPos + px;
                if (screenX < 0 || screenX >= 240)
                    continue;
                let tx = px;
                if (hflipFlag)
                    tx = w - 1 - tx;
                const tileCol = tx >> 3;
                const inTileX = tx & 7;
                const baseTile = tileIdx + tileRow * rowStride + tileCol * tilesPerTile;
                const tileAddr = tileBase + (baseTile & 0x3FF) * 32 + inTileY * (color8 ? 8 : 4) + (color8 ? inTileX : (inTileX >> 1));
                if (tileAddr >= 0x18000)
                    continue;
                let pix;
                if (color8) {
                    pix = vram[tileAddr];
                    if (pix === 0)
                        continue;
                }
                else {
                    const byte = vram[tileAddr];
                    pix = (inTileX & 1) ? (byte >> 4) : (byte & 0xF);
                    if (pix === 0)
                        continue;
                }
                const cur = out[screenX];
                if ((cur & 0x8000) === 0 && (((cur >> 18) & 3) <= priority))
                    continue;
                const palBase = color8 ? 256 : (256 + palBank * 16);
                out[screenX] = (pram16[palBase + pix] & 0x7FFF) | layerHi;
            }
            continue;
        }
        // Affine path. Source coords are 8.8 fixed-point. For each screen
        // pixel (px, py) in the bounding box, compute:
        //   src_x = pA*(px - cx) + pB*(py - cy) + halfW (in 8.8)
        //   src_y = pC*(px - cx) + pD*(py - cy) + halfH
        // Then if (src_x, src_y) is in [0..w, 0..h) we sample the texel.
        const dy = inSpriteY - cy;
        let srcX0 = (pA * (-cx) + pB * dy) + (halfW << 8);
        let srcY0 = (pC * (-cx) + pD * dy) + (halfH << 8);
        for (let px = 0; px < drawW; px++) {
            const screenX = xPos + px;
            if (screenX < 0 || screenX >= 240) {
                srcX0 += pA;
                srcY0 += pC;
                continue;
            }
            const sx = srcX0 >> 8;
            const sy = srcY0 >> 8;
            srcX0 += pA;
            srcY0 += pC;
            if (sx < 0 || sx >= w || sy < 0 || sy >= h)
                continue;
            const tileCol = sx >> 3;
            const tileRow = sy >> 3;
            const inTileX = sx & 7;
            const inTileY = sy & 7;
            const baseTile = tileIdx + tileRow * rowStride + tileCol * tilesPerTile;
            const tileAddr = tileBase + (baseTile & 0x3FF) * 32 + inTileY * (color8 ? 8 : 4) + (color8 ? inTileX : (inTileX >> 1));
            if (tileAddr >= 0x18000)
                continue;
            let pix;
            if (color8) {
                pix = vram[tileAddr];
                if (pix === 0)
                    continue;
            }
            else {
                const byte = vram[tileAddr];
                pix = (inTileX & 1) ? (byte >> 4) : (byte & 0xF);
                if (pix === 0)
                    continue;
            }
            const cur = out[screenX];
            if ((cur & 0x8000) === 0 && (((cur >> 18) & 3) <= priority))
                continue;
            const palBase = color8 ? 256 : (256 + palBank * 16);
            out[screenX] = (pram16[palBase + pix] & 0x7FFF) | layerHi;
        }
    }
}
