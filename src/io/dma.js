import { IRQ_DMA0 } from './irq';
// 4 DMA channels. Start timing:
//   00 Immediate    01 VBlank    10 HBlank    11 Special
// Special: ch1/ch2 sound FIFO, ch3 video capture.
//
// DMAxSAD: source        (channel 0: 27-bit, 1-3: 28-bit)
// DMAxDAD: destination   (channel 0-2: 27-bit, 3: 28-bit)
// DMAxCNT_L: count       (channel 0-2: 14-bit, 3: 16-bit)
// DMAxCNT_H: control 16-bit:
//   05 dest control (00 inc, 01 dec, 10 fix, 11 inc/reload)
//   07 src control  (00 inc, 01 dec, 10 fix)
//   09 repeat       10 word/halfword  11 gamepak DRQ
//   12-13 start timing
//   14 irq enable   15 enable
export const DMA_TIMING_IMMEDIATE = 0;
export const DMA_TIMING_VBLANK = 1;
export const DMA_TIMING_HBLANK = 2;
export const DMA_TIMING_SPECIAL = 3;
export class DmaChannel {
    src = 0;
    dst = 0;
    count = 0;
    control = 0;
    internalSrc = 0;
    internalDst = 0;
    internalCount = 0;
    enabled = false;
    timing = 0;
    word = false;
    repeat = false;
    irqEnable = false;
    dstCtrl = 0;
    srcCtrl = 0;
}
export class Dma {
    bus;
    irq;
    ch = [new DmaChannel(), new DmaChannel(), new DmaChannel(), new DmaChannel()];
    constructor(bus, irq) {
        this.bus = bus;
        this.irq = irq;
    }
    // Setter helpers — masked per-channel.
    writeSrc(i, v) {
        const mask = i === 0 ? 0x07FFFFFF : 0x0FFFFFFF;
        this.ch[i].src = (v & mask) >>> 0;
    }
    writeDst(i, v) {
        const mask = i === 3 ? 0x0FFFFFFF : 0x07FFFFFF;
        this.ch[i].dst = (v & mask) >>> 0;
    }
    writeCount(i, v) {
        const mask = i === 3 ? 0xFFFF : 0x3FFF;
        let c = v & mask;
        if (c === 0)
            c = i === 3 ? 0x10000 : 0x4000;
        this.ch[i].count = c;
    }
    writeControl(i, v) {
        const c = this.ch[i];
        const wasEnabled = c.enabled;
        c.control = v & 0xFFFF;
        c.dstCtrl = (v >> 5) & 3;
        c.srcCtrl = (v >> 7) & 3;
        c.repeat = (v & 0x0200) !== 0;
        c.word = (v & 0x0400) !== 0;
        c.timing = (v >> 12) & 3;
        c.irqEnable = (v & 0x4000) !== 0;
        c.enabled = (v & 0x8000) !== 0;
        if (!wasEnabled && c.enabled) {
            c.internalSrc = c.src;
            c.internalDst = c.dst;
            c.internalCount = c.count;
            if (c.timing === DMA_TIMING_IMMEDIATE)
                this.runChannel(i);
        }
    }
    // Hook called by PPU at the appropriate transition.
    triggerVBlank() { for (let i = 0; i < 4; i++)
        if (this.ch[i].enabled && this.ch[i].timing === DMA_TIMING_VBLANK)
            this.runChannel(i); }
    triggerHBlank() { for (let i = 0; i < 4; i++)
        if (this.ch[i].enabled && this.ch[i].timing === DMA_TIMING_HBLANK)
            this.runChannel(i); }
    // Special-timing helper for sound FIFO (channels 1 and 2 only).
    triggerSoundFifo(channel) {
        const c = this.ch[channel];
        if (!c.enabled || c.timing !== DMA_TIMING_SPECIAL)
            return;
        // Sound FIFO DMAs always do 4 words to a fixed destination.
        const dst = c.internalDst;
        let src = c.internalSrc;
        for (let i = 0; i < 4; i++) {
            this.bus.write32(dst, this.bus.read32(src));
            src = (src + 4) >>> 0;
        }
        c.internalSrc = src;
        // Sound FIFO repeats automatically — don't disable.
        if (c.irqEnable)
            this.irq.raise(IRQ_DMA0 << channel);
    }
    runChannel(i) {
        const c = this.ch[i];
        const word = c.word;
        const step = word ? 4 : 2;
        let src = c.internalSrc;
        let dst = c.internalDst;
        const start = dst;
        const count = c.internalCount;
        for (let n = 0; n < count; n++) {
            if (word)
                this.bus.write32(dst, this.bus.read32(src));
            else
                this.bus.write16(dst, this.bus.read16(src));
            switch (c.srcCtrl) {
                case 0:
                    src = (src + step) >>> 0;
                    break;
                case 1:
                    src = (src - step) >>> 0;
                    break;
            }
            switch (c.dstCtrl) {
                case 0:
                case 3:
                    dst = (dst + step) >>> 0;
                    break;
                case 1:
                    dst = (dst - step) >>> 0;
                    break;
            }
        }
        c.internalSrc = src;
        if (c.dstCtrl === 3)
            c.internalDst = start; // increment-reload restores
        else
            c.internalDst = dst;
        if (c.irqEnable)
            this.irq.raise(IRQ_DMA0 << i);
        if (!c.repeat) {
            c.enabled = false;
            c.control &= ~0x8000;
        }
        else {
            c.internalCount = c.count;
            if (c.dstCtrl === 3)
                c.internalDst = c.dst;
        }
    }
}
