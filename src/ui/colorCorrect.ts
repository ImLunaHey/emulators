// GBA LCD color correction. The bare framebuffer is over-saturated and
// over-bright compared to the real handheld's reflective LCD, which had
// a low gamma and noticeable channel bleed. This applies the canonical
// higan/byuu GBA correction (per-channel gamma 4 → channel mix → output
// gamma 2.2) as a precomputed BGR555 → packed-RGBA lookup table.
//
// The PPU expands each 5-bit channel as (c<<3)|(c>>2), so the renderer
// can recover the exact 5-bit index from an 8-bit channel with `>>3`.

let lut: Uint32Array | null = null;

export function getGbaColorLut(): Uint32Array {
  if (lut) return lut;
  const out = new Uint32Array(32768);
  const lcdGamma = 4.0;
  const outGamma = 2.2;
  for (let i = 0; i < 32768; i++) {
    const r5 = i & 0x1F;
    const g5 = (i >> 5) & 0x1F;
    const b5 = (i >> 10) & 0x1F;
    const lr = Math.pow(r5 / 31, lcdGamma);
    const lg = Math.pow(g5 / 31, lcdGamma);
    const lb = Math.pow(b5 / 31, lcdGamma);
    const r = Math.pow(Math.min(1, (255 * lr + 50 * lg + 0 * lb) / 255), 1 / outGamma);
    const g = Math.pow(Math.min(1, (10 * lr + 230 * lg + 30 * lb) / 255), 1 / outGamma);
    const b = Math.pow(Math.min(1, (50 * lr + 10 * lg + 220 * lb) / 255), 1 / outGamma);
    const r8 = Math.round(r * 255);
    const g8 = Math.round(g * 255);
    const b8 = Math.round(b * 255);
    // Packed little-endian RGBA (matches a Uint32 view of the canvas buffer).
    out[i] = ((0xFF << 24) | (b8 << 16) | (g8 << 8) | r8) >>> 0;
  }
  lut = out;
  return out;
}
