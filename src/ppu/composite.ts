import type { Ppu } from './ppu';

// Compose layers into the final RGBA frame line.
// Pixel encoding (32-bit):
//   bits 0..14   BGR555
//   bit 15       transparent
//   bits 16..17  layer id (0..3 BG, 4 OBJ, 5 backdrop)
//   bits 18..19  priority
//   bit 20       OBJ semi-transparent
//   bit 21       OBJ window

function bgr555ToRgba(bgr: number, out: Uint8ClampedArray, off: number): void {
  const r = bgr & 0x1F;
  const g = (bgr >> 5) & 0x1F;
  const b = (bgr >> 10) & 0x1F;
  out[off    ] = (r << 3) | (r >> 2);
  out[off + 1] = (g << 3) | (g >> 2);
  out[off + 2] = (b << 3) | (b >> 2);
  out[off + 3] = 0xFF;
}

function bgr555Blend(a: number, b: number, eva: number, evb: number): number {
  const ra = a & 0x1F, ga = (a >> 5) & 0x1F, ba = (a >> 10) & 0x1F;
  const rb = b & 0x1F, gb = (b >> 5) & 0x1F, bb = (b >> 10) & 0x1F;
  const r = Math.min(31, ((ra * eva) >> 4) + ((rb * evb) >> 4));
  const g = Math.min(31, ((ga * eva) >> 4) + ((gb * evb) >> 4));
  const bl = Math.min(31, ((ba * eva) >> 4) + ((bb * evb) >> 4));
  return (bl << 10) | (g << 5) | r;
}

export function compositeScanline(ppu: Ppu, y: number, backdrop: number): void {
  const out = ppu.frame;
  const offBase = y * 240 * 4;

  const bldcnt = ppu.bldcnt;
  const blendMode = (bldcnt >> 6) & 3;
  const top = bldcnt & 0x3F;
  const bot = (bldcnt >> 8) & 0x3F;
  const eva = Math.min(16, ppu.bldalpha & 0x1F);
  const evb = Math.min(16, (ppu.bldalpha >> 8) & 0x1F);
  const evy = Math.min(16, ppu.bldy & 0x1F);

  // Window enable bits in DISPCNT (13=WIN0, 14=WIN1, 15=OBJ_WIN).
  const win0En = (ppu.dispcnt & 0x2000) !== 0;
  const win1En = (ppu.dispcnt & 0x4000) !== 0;
  const objWinEn = (ppu.dispcnt & 0x8000) !== 0;
  const anyWinEn = win0En || win1En || objWinEn;
  // Window 0/1 bounds. H reg: bits 8-15 = X1, bits 0-7 = X2 (exclusive).
  // V reg: bits 8-15 = Y1, bits 0-7 = Y2 (exclusive). Hardware wraps oddly
  // for X2<X1 or Y2<Y1 cases — we approximate the common path.
  const w0x1 = (ppu.win0H >> 8) & 0xFF;
  const w0x2 = ppu.win0H & 0xFF;
  const w0y1 = (ppu.win0V >> 8) & 0xFF;
  const w0y2 = ppu.win0V & 0xFF;
  const w1x1 = (ppu.win1H >> 8) & 0xFF;
  const w1x2 = ppu.win1H & 0xFF;
  const w1y1 = (ppu.win1V >> 8) & 0xFF;
  const w1y2 = ppu.win1V & 0xFF;
  const winInBits = ppu.winIn;
  const winOutBits = ppu.winOut;
  const w0InEnable = winInBits & 0x3F;       // layers + blend enabled inside WIN0
  const w1InEnable = (winInBits >> 8) & 0x3F;
  const wOutEnable = winOutBits & 0x3F;
  const wObjInEnable = (winOutBits >> 8) & 0x3F;
  const y0 = y;
  const win0Hit = win0En && y0 >= w0y1 && y0 < w0y2;
  const win1Hit = win1En && y0 >= w1y1 && y0 < w1y2;

  for (let x = 0; x < 240; x++) {
    // Determine which window region (if any) this pixel belongs to. Higher-
    // priority window: WIN0 > WIN1 > OBJ-window > outside.
    let allowMask = 0x3F;  // default: everything allowed
    if (anyWinEn) {
      const inW0 = win0Hit && x >= w0x1 && x < w0x2;
      const inW1 = !inW0 && win1Hit && x >= w1x1 && x < w1x2;
      const inObjWin = !inW0 && !inW1 && objWinEn && (ppu.objLine[x] & (1 << 21)) !== 0;
      if      (inW0)     allowMask = w0InEnable;
      else if (inW1)     allowMask = w1InEnable;
      else if (inObjWin) allowMask = wObjInEnable;
      else               allowMask = wOutEnable;
    }
    // allowMask bits: 0..3 = BG0..3 enable, 4 = OBJ enable, 5 = blend enable.
    const bgAllow0 = (allowMask & 0x01) !== 0;
    const bgAllow1 = (allowMask & 0x02) !== 0;
    const bgAllow2 = (allowMask & 0x04) !== 0;
    const bgAllow3 = (allowMask & 0x08) !== 0;
    const objAllow = (allowMask & 0x10) !== 0;
    const blendAllow = (allowMask & 0x20) !== 0;
    let bestColor = backdrop;
    let bestPrio = 4;
    let bestLayer = 5;
    let bestSemi = 0;

    for (let b = 0; b < 4; b++) {
      // Window-masked layer disable.
      if (anyWinEn) {
        if (b === 0 && !bgAllow0) continue;
        if (b === 1 && !bgAllow1) continue;
        if (b === 2 && !bgAllow2) continue;
        if (b === 3 && !bgAllow3) continue;
      }
      const px = ppu.bgLine[b][x];
      if (px & 0x8000) continue;
      const prio = (px >> 18) & 3;
      if (prio < bestPrio || (prio === bestPrio && b < bestLayer)) {
        bestPrio = prio; bestColor = px & 0x7FFF; bestLayer = b; bestSemi = 0;
      }
    }
    const obj = ppu.objLine[x];
    const objIsObjWin = (obj & (1 << 21)) !== 0;
    if (!(obj & 0x8000) && !objIsObjWin && (!anyWinEn || objAllow)) {
      const prio = (obj >> 18) & 3;
      if (prio <= bestPrio) {
        bestPrio = prio; bestColor = obj & 0x7FFF; bestLayer = 4; bestSemi = (obj >> 20) & 1;
      }
    }

    // Find next-best for blending.
    let bot1Color = backdrop;
    let bot1Layer = 5;
    let bot1Prio = 4;
    for (let b = 0; b < 4; b++) {
      if (b === bestLayer) continue;
      if (anyWinEn) {
        if (b === 0 && !bgAllow0) continue;
        if (b === 1 && !bgAllow1) continue;
        if (b === 2 && !bgAllow2) continue;
        if (b === 3 && !bgAllow3) continue;
      }
      const px = ppu.bgLine[b][x];
      if (px & 0x8000) continue;
      const prio = (px >> 18) & 3;
      if (prio < bot1Prio || (prio === bot1Prio && b < bot1Layer)) {
        bot1Prio = prio; bot1Color = px & 0x7FFF; bot1Layer = b;
      }
    }
    if (bestLayer !== 4 && !(obj & 0x8000) && !objIsObjWin && (!anyWinEn || objAllow)) {
      const prio = (obj >> 18) & 3;
      if (prio < bot1Prio || (prio === bot1Prio && 4 < bot1Layer)) {
        bot1Prio = prio; bot1Color = obj & 0x7FFF; bot1Layer = 4;
      }
    }

    let color = bestColor;
    const topMask = 1 << bestLayer;
    const botMask = 1 << bot1Layer;
    const topSet = (top & topMask) !== 0;
    const botSet = (bot & botMask) !== 0;

    // Inside a window, blending is gated by the window's blend-enable bit.
    if (anyWinEn && !blendAllow) {
      // Skip blending — just emit the top color.
    } else if (bestSemi && botSet) {
      color = bgr555Blend(bestColor, bot1Color, eva, evb);
    } else if (blendMode === 1 && topSet && botSet) {
      color = bgr555Blend(bestColor, bot1Color, eva, evb);
    } else if (blendMode === 2 && topSet) {
      // Brighten toward white.
      const r = bestColor & 0x1F, g = (bestColor >> 5) & 0x1F, b = (bestColor >> 10) & 0x1F;
      const r2 = r + (((31 - r) * evy) >> 4);
      const g2 = g + (((31 - g) * evy) >> 4);
      const b2 = b + (((31 - b) * evy) >> 4);
      color = (b2 << 10) | (g2 << 5) | r2;
    } else if (blendMode === 3 && topSet) {
      // Darken toward black.
      const r = bestColor & 0x1F, g = (bestColor >> 5) & 0x1F, b = (bestColor >> 10) & 0x1F;
      const r2 = r - ((r * evy) >> 4);
      const g2 = g - ((g * evy) >> 4);
      const b2 = b - ((b * evy) >> 4);
      color = (b2 << 10) | (g2 << 5) | r2;
    }

    bgr555ToRgba(color, out, offBase + x * 4);
  }
}
