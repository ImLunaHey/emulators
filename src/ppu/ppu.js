import { IRQ_VBLANK, IRQ_HBLANK, IRQ_VCOUNT } from '../io/irq';
import { renderModeText } from './modes_text';
import { renderModeBitmap3, renderModeBitmap4, renderModeBitmap5 } from './modes_bitmap';
import { renderModeAffine } from './modes_affine';
import { renderSprites } from './sprites';
import { compositeScanline } from './composite';
// Cycle counts (1 dot = 4 CPU cycles).
const DOTS_VISIBLE = 240;
const DOTS_HBLANK = 68;
const DOTS_PER_LINE = DOTS_VISIBLE + DOTS_HBLANK;
const CYC_VISIBLE = DOTS_VISIBLE * 4;
const CYC_PER_LINE = DOTS_PER_LINE * 4;
const LINES_VISIBLE = 160;
const LINES_TOTAL = 228;
// Buffers per scanline — composer outputs into `frame` (RGBA8888).
// Layer pixel format (packed 32-bit):
//   bits 0..14   = BGR555 color
//   bit  15      = transparent
//   bits 16..17  = layer source (0..3 = BG0..3, 4 = OBJ, 5 = backdrop)
//   bits 18..19  = priority (0..3)
//   bit  20      = OBJ semi-transparent
//   bit  21      = OBJ window
// Render functions write a per-layer line buffer; compositor picks per pixel.
export class Ppu {
    bus;
    irq;
    dma;
    // PPU registers (we keep canonical copies; IO mirrors to raw too).
    dispcnt = 0;
    dispstat = 0;
    vcount = 0;
    bgcnt = new Uint16Array(4);
    bgHOFS = new Uint16Array(4);
    bgVOFS = new Uint16Array(4);
    bgX = new Int32Array(2); // BG2/3 reference X (28-bit signed)
    bgY = new Int32Array(2); // BG2/3 reference Y
    bgPA = new Int16Array(2);
    bgPB = new Int16Array(2);
    bgPC = new Int16Array(2);
    bgPD = new Int16Array(2);
    win0H = 0;
    win1H = 0;
    win0V = 0;
    win1V = 0;
    winIn = 0;
    winOut = 0;
    mosaic = 0;
    bldcnt = 0;
    bldalpha = 0;
    bldy = 0;
    // RGBA frame buffer (240x160).
    frame = new Uint8ClampedArray(240 * 160 * 4);
    scanline = new Uint32Array(240);
    bgLine = [new Uint32Array(240), new Uint32Array(240), new Uint32Array(240), new Uint32Array(240)];
    objLine = new Uint32Array(240);
    cyclesAccum = 0;
    inHBlank = false;
    frameDone = false;
    frameCount = 0;
    constructor(bus, irq, dma) {
        this.bus = bus;
        this.irq = irq;
        this.dma = dma;
    }
    readDispstat() {
        let v = this.dispstat & 0xFF38;
        if (this.vcount >= LINES_VISIBLE && this.vcount !== LINES_TOTAL - 1)
            v |= 0x01;
        if (this.inHBlank)
            v |= 0x02;
        if (((this.dispstat >> 8) & 0xFF) === this.vcount)
            v |= 0x04;
        return v;
    }
    writeReg(addr, v) {
        switch (addr) {
            case 0x00:
                this.dispcnt = v;
                return;
            case 0x04: {
                // Only bits 3-7 and 8-15 of DISPSTAT are writable; status bits are RO.
                this.dispstat = (this.dispstat & 0x07) | (v & 0xFFF8);
                return;
            }
            case 0x08:
                this.bgcnt[0] = v;
                return;
            case 0x0A:
                this.bgcnt[1] = v;
                return;
            case 0x0C:
                this.bgcnt[2] = v;
                return;
            case 0x0E:
                this.bgcnt[3] = v;
                return;
            case 0x10:
                this.bgHOFS[0] = v & 0x1FF;
                return;
            case 0x12:
                this.bgVOFS[0] = v & 0x1FF;
                return;
            case 0x14:
                this.bgHOFS[1] = v & 0x1FF;
                return;
            case 0x16:
                this.bgVOFS[1] = v & 0x1FF;
                return;
            case 0x18:
                this.bgHOFS[2] = v & 0x1FF;
                return;
            case 0x1A:
                this.bgVOFS[2] = v & 0x1FF;
                return;
            case 0x1C:
                this.bgHOFS[3] = v & 0x1FF;
                return;
            case 0x1E:
                this.bgVOFS[3] = v & 0x1FF;
                return;
            case 0x20:
                this.bgPA[0] = (v << 16) >> 16;
                return;
            case 0x22:
                this.bgPB[0] = (v << 16) >> 16;
                return;
            case 0x24:
                this.bgPC[0] = (v << 16) >> 16;
                return;
            case 0x26:
                this.bgPD[0] = (v << 16) >> 16;
                return;
            case 0x28:
                this.bgX[0] = (this.bgX[0] & 0xFFFF0000) | v;
                return;
            case 0x2A:
                this.bgX[0] = (this.bgX[0] & 0xFFFF) | (((v << 16) >> 16) << 16);
                return;
            case 0x2C:
                this.bgY[0] = (this.bgY[0] & 0xFFFF0000) | v;
                return;
            case 0x2E:
                this.bgY[0] = (this.bgY[0] & 0xFFFF) | (((v << 16) >> 16) << 16);
                return;
            case 0x30:
                this.bgPA[1] = (v << 16) >> 16;
                return;
            case 0x32:
                this.bgPB[1] = (v << 16) >> 16;
                return;
            case 0x34:
                this.bgPC[1] = (v << 16) >> 16;
                return;
            case 0x36:
                this.bgPD[1] = (v << 16) >> 16;
                return;
            case 0x38:
                this.bgX[1] = (this.bgX[1] & 0xFFFF0000) | v;
                return;
            case 0x3A:
                this.bgX[1] = (this.bgX[1] & 0xFFFF) | (((v << 16) >> 16) << 16);
                return;
            case 0x3C:
                this.bgY[1] = (this.bgY[1] & 0xFFFF0000) | v;
                return;
            case 0x3E:
                this.bgY[1] = (this.bgY[1] & 0xFFFF) | (((v << 16) >> 16) << 16);
                return;
            case 0x40:
                this.win0H = v;
                return;
            case 0x42:
                this.win1H = v;
                return;
            case 0x44:
                this.win0V = v;
                return;
            case 0x46:
                this.win1V = v;
                return;
            case 0x48:
                this.winIn = v;
                return;
            case 0x4A:
                this.winOut = v;
                return;
            case 0x4C:
                this.mosaic = v;
                return;
            case 0x50:
                this.bldcnt = v;
                return;
            case 0x52:
                this.bldalpha = v;
                return;
            case 0x54:
                this.bldy = v & 0x1F;
                return;
        }
    }
    // Advance PPU by `cycles` CPU cycles. Drives line transitions, HBlank
    // and VBlank IRQs + DMAs, and renders each visible scanline.
    step(cycles) {
        this.cyclesAccum += cycles;
        while (this.cyclesAccum >= CYC_PER_LINE) {
            this.cyclesAccum -= CYC_PER_LINE;
            // We model the line as: render visible at start, then HBlank trigger.
            if (this.vcount < LINES_VISIBLE) {
                this.renderScanline(this.vcount);
                this.inHBlank = true;
                if (this.dispstat & 0x10)
                    this.irq.raise(IRQ_HBLANK);
                this.dma.triggerHBlank();
            }
            this.vcount++;
            if (this.vcount === LINES_VISIBLE) {
                this.inHBlank = false;
                this.frameDone = true;
                this.frameCount++;
                if (this.dispstat & 0x08)
                    this.irq.raise(IRQ_VBLANK);
                this.dma.triggerVBlank();
            }
            else if (this.vcount >= LINES_TOTAL) {
                this.vcount = 0;
                this.inHBlank = false;
                // Reload affine reference points at frame start.
                // (Strictly hardware reloads them at the end of VBlank.)
            }
            else {
                this.inHBlank = false;
            }
            // VCOUNT match.
            if (((this.dispstat >> 8) & 0xFF) === this.vcount && (this.dispstat & 0x20)) {
                this.irq.raise(IRQ_VCOUNT);
            }
        }
    }
    renderScanline(y) {
        // Forced blank → white.
        if (this.dispcnt & 0x80) {
            const off = y * 240 * 4;
            this.frame.fill(0xFF, off, off + 240 * 4);
            return;
        }
        // Backdrop from PRAM index 0.
        const backdrop = this.bus.pram16[0] & 0x7FFF;
        const mode = this.dispcnt & 0x7;
        // Reset BG layer outputs (mark transparent).
        for (let b = 0; b < 4; b++)
            this.bgLine[b].fill(0x8000);
        this.objLine.fill(0x8000);
        if (mode <= 2) {
            // Tile / text / affine modes — only the relevant BGs are valid.
            if (mode === 0) {
                for (let b = 0; b < 4; b++)
                    if (this.dispcnt & (1 << (8 + b)))
                        renderModeText(this, b, y);
            }
            else if (mode === 1) {
                if (this.dispcnt & 0x100)
                    renderModeText(this, 0, y);
                if (this.dispcnt & 0x200)
                    renderModeText(this, 1, y);
                // BG2 in mode 1 is AFFINE — different map layout (1 byte per
                // tile index, no palette/flip bits), always 8bpp tile data, and
                // sampled through the BGxPA..D matrix + reference point.
                if (this.dispcnt & 0x400)
                    renderModeAffine(this, 2, y);
            }
            else {
                // Mode 2: BG2 and BG3 are both affine.
                if (this.dispcnt & 0x400)
                    renderModeAffine(this, 2, y);
                if (this.dispcnt & 0x800)
                    renderModeAffine(this, 3, y);
            }
        }
        else if (mode === 3) {
            if (this.dispcnt & 0x400)
                renderModeBitmap3(this, y);
        }
        else if (mode === 4) {
            if (this.dispcnt & 0x400)
                renderModeBitmap4(this, y);
        }
        else if (mode === 5) {
            if (this.dispcnt & 0x400)
                renderModeBitmap5(this, y);
        }
        if (this.dispcnt & 0x1000)
            renderSprites(this, y);
        compositeScanline(this, y, backdrop);
    }
}
