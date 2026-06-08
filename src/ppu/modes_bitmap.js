// Mode 3: 240x160 BGR555 direct color.
export function renderModeBitmap3(ppu, y) {
    const layerHi = (2 << 16) | ((ppu.bgcnt[2] & 3) << 18);
    const out = ppu.bgLine[2];
    const vram16 = ppu.bus.vram16;
    const rowOff = y * 240;
    for (let x = 0; x < 240; x++) {
        out[x] = (vram16[rowOff + x] & 0x7FFF) | layerHi;
    }
}
// Mode 4: 240x160 indexed, double-buffered (page select).
export function renderModeBitmap4(ppu, y) {
    const layerHi = (2 << 16) | ((ppu.bgcnt[2] & 3) << 18);
    const out = ppu.bgLine[2];
    const page = (ppu.dispcnt & 0x10) ? 0xA000 : 0x0000;
    const vram = ppu.bus.vram;
    const pram16 = ppu.bus.pram16;
    const rowOff = page + y * 240;
    for (let x = 0; x < 240; x++) {
        const idx = vram[rowOff + x];
        if (idx === 0) {
            out[x] = 0x8000;
            continue;
        }
        out[x] = (pram16[idx] & 0x7FFF) | layerHi;
    }
}
// Mode 5: 160x128 BGR555 direct, double-buffered.
export function renderModeBitmap5(ppu, y) {
    const layerHi = (2 << 16) | ((ppu.bgcnt[2] & 3) << 18);
    const out = ppu.bgLine[2];
    if (y >= 128) {
        out.fill(0x8000);
        return;
    }
    const page = (ppu.dispcnt & 0x10) ? 0xA000 : 0x0000;
    const vram16 = ppu.bus.vram16;
    const rowOff = (page >>> 1) + y * 160;
    for (let x = 0; x < 240; x++) {
        if (x >= 160) {
            out[x] = 0x8000;
            continue;
        }
        out[x] = (vram16[rowOff + x] & 0x7FFF) | layerHi;
    }
}
