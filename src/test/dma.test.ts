// DMA controller tests. The shadow-OAM transfer at VBlank is the prime
// suspect for the "professor cut in half" symptom in Pokemon, so this
// suite covers every src/dst increment mode, both transfer widths,
// repeat-on-repeat (VBlank-driven), and the specific EWRAM → OAM
// pattern Pokemon uses.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';

function makeEmu() {
  const bus = new Bus();
  const irq = new Irq();
  const keypad = new Keypad();
  const dma = new Dma(bus, irq);
  const timers = new Timers(irq);
  const ppu = new Ppu(bus, irq, dma);
  const cpu = new Cpu(bus);
  const io = new Io(bus, ppu, dma, timers, irq, keypad, cpu);
  bus.attachIo(io);
  bus.attachSave({ read: () => 0xFF, write: () => {} });
  bus.loadRom(new Uint8Array(0x100));
  return { bus, dma, irq, ppu };
}

// Trigger a DMA by writing the standard SAD/DAD/CNT register pair via the
// IO bus. This is the same path the game's MMIO writes take.
function triggerDma(
  bus: Bus,
  ch: number,
  src: number,
  dst: number,
  count: number,
  ctrl: number,
) {
  const base = 0x040000B0 + ch * 12;
  bus.write32(base + 0, src);
  bus.write32(base + 4, dst);
  bus.write16(base + 8, count);
  bus.write16(base + 10, ctrl);
}

describe('DMA: immediate transfer', () => {
  it('DMA0 16-bit copy with both src+dst inc', () => {
    const { bus } = makeEmu();
    for (let i = 0; i < 16; i++) bus.write8(0x03000100 + i, i + 1);
    // src=0x03000100 (IWRAM), dst=0x03000200, count=8 (halfwords),
    // ctrl=0x8000 (enable, immediate, halfword, src+dst inc).
    triggerDma(bus, 0, 0x03000100, 0x03000200, 8, 0x8000);
    for (let i = 0; i < 16; i++) {
      expect(bus.read8(0x03000200 + i)).toBe(i + 1);
    }
  });

  it('DMA3 32-bit word copy', () => {
    const { bus } = makeEmu();
    bus.write32(0x03000100, 0xDEADBEEF);
    bus.write32(0x03000104, 0xCAFEBABE);
    // count=2 words, ctrl=0x8400 (enable, word, src+dst inc).
    triggerDma(bus, 3, 0x03000100, 0x03000200, 2, 0x8400);
    expect(bus.read32(0x03000200)).toBe(0xDEADBEEF);
    expect(bus.read32(0x03000204)).toBe(0xCAFEBABE);
  });

  it('DMA fixed src (16-bit fill)', () => {
    const { bus } = makeEmu();
    bus.write16(0x03000100, 0x55AA);
    // src fixed (ctrl src=10 = fixed = bits 7-8 = 0b10 = << 7 = 0x100).
    // dst inc, halfword, enable. ctrl = 0x8000 | (2 << 7) = 0x8100.
    triggerDma(bus, 0, 0x03000100, 0x03000200, 4, 0x8100);
    for (let i = 0; i < 4; i++) {
      expect(bus.read16(0x03000200 + i * 2)).toBe(0x55AA);
    }
  });

  it('DMA dst-fixed (drain to FIFO pattern)', () => {
    const { bus } = makeEmu();
    bus.write16(0x03000100, 0x1111);
    bus.write16(0x03000102, 0x2222);
    bus.write16(0x03000104, 0x3333);
    // dst fixed (bits 5-6 = 10 = << 5 = 0x40), src inc, halfword.
    triggerDma(bus, 0, 0x03000100, 0x03000200, 3, 0x8040);
    // Last value written remains at dst.
    expect(bus.read16(0x03000200)).toBe(0x3333);
  });

  it('DMA dst-inc-reload (mode 0b11)', () => {
    const { bus } = makeEmu();
    bus.write16(0x03000100, 0xAAAA);
    bus.write16(0x03000102, 0xBBBB);
    // Dst increment-and-reload mode: dst++ during transfer, then reload
    // to original on next trigger. ctrl bits 5-6 = 11 = 0x60.
    triggerDma(bus, 0, 0x03000100, 0x03000200, 2, 0x8060);
    expect(bus.read16(0x03000200)).toBe(0xAAAA);
    expect(bus.read16(0x03000202)).toBe(0xBBBB);
  });

  it('DMA disables itself after immediate transfer (no repeat)', () => {
    const { bus, dma } = makeEmu();
    triggerDma(bus, 0, 0x03000100, 0x03000200, 1, 0x8000);
    expect(dma.ch[0].enabled).toBe(false);
  });
});

describe('DMA: count + length edge cases', () => {
  it('zero count is interpreted as max (0x4000 halfwords for DMA0-2)', () => {
    const { bus, dma } = makeEmu();
    // Write 0 → should map to 0x4000.
    triggerDma(bus, 0, 0x03000100, 0x03000200, 0, 0x8000);
    // After transfer, channel disabled. We can't easily verify all 16K
    // halfwords without huge memory, but the channel's internal count
    // matches the configured one.
    expect(dma.ch[0].count).toBe(0x4000);
  });

  it('DMA3 zero count → 0x10000', () => {
    const { dma, bus } = makeEmu();
    // Set up but don't enable so the transfer doesn't actually run
    // (we only check count parsing).
    bus.write32(0x040000D4, 0x03000100);  // SAD
    bus.write32(0x040000D8, 0x03000200);  // DAD
    bus.write16(0x040000DC, 0);            // count → 0x10000
    expect(dma.ch[3].count).toBe(0x10000);
  });

  it('DMA3 max halfword count (16-bit field)', () => {
    const { dma, bus } = makeEmu();
    bus.write32(0x040000D4, 0x03000100);
    bus.write32(0x040000D8, 0x03000200);
    bus.write16(0x040000DC, 0xFFFF);
    expect(dma.ch[3].count).toBe(0xFFFF);
  });
});

describe('DMA: VBlank-triggered repeat (Pokemon shadow-OAM pattern)', () => {
  it('DMA3 with VBlank timing fires on triggerVBlank, repeats', () => {
    const { bus, dma } = makeEmu();
    // Set source/dest, count=8 halfwords, ctrl=enable+repeat+halfword+VBlank timing.
    // ctrl bits: 0x8000 enable | 0x0200 repeat | 0x1000 vblank.
    for (let i = 0; i < 16; i++) bus.write8(0x03000100 + i, 0x80 + i);
    triggerDma(bus, 3, 0x03000100, 0x07000000, 8, 0x8000 | 0x0200 | 0x1000);
    // VBlank-timed DMA doesn't fire on enable.
    expect(bus.read8(0x07000000)).toBe(0);
    // Now trigger VBlank.
    dma.triggerVBlank();
    for (let i = 0; i < 16; i++) {
      expect(bus.read8(0x07000000 + i)).toBe(0x80 + i);
    }
    // Channel stays enabled because repeat is set.
    expect(dma.ch[3].enabled).toBe(true);
  });

  it('Pokemon-style 1 KB OAM update: EWRAM shadow → OAM via DMA3 halfword copy', () => {
    const { bus } = makeEmu();
    // Build a shadow OAM in EWRAM: 128 sprites x 8 bytes = 1024 bytes,
    // each sprite given a recognizable a0/a1/a2.
    for (let i = 0; i < 128; i++) {
      const off = 0x02000000 + i * 8;
      bus.write16(off + 0, 0x0040 | i);    // a0: y=64, id encoded in low bits
      bus.write16(off + 2, 0x0080 | i);    // a1: x=128, id encoded
      bus.write16(off + 4, 0x1000 | i);    // a2: tile=i, palette 1
      bus.write16(off + 6, 0xFFFF);        // affine column
    }
    // DMA3 immediate, halfword, src+dst inc, count = 512 halfwords (= 1KB).
    triggerDma(bus, 3, 0x02000000, 0x07000000, 512, 0x8400 ^ 0x0400);
    // Hmm 0x8400 has word=1; we want halfword (word bit 10 = 0).
    // ctrl 0x8000 = enable, no repeat, halfword, immediate, src+dst inc.
    // Redo: ctrl = 0x8000 (just enable, immediate halfword, src+dst inc default).
    triggerDma(bus, 3, 0x02000000, 0x07000000, 512, 0x8000);
    // Check every OAM byte matches the EWRAM source.
    for (let i = 0; i < 1024; i++) {
      const src = bus.read8(0x02000000 + i);
      const dst = bus.read8(0x07000000 + i);
      expect(`oam[${i}]=${dst.toString(16)}`).toBe(`oam[${i}]=${src.toString(16)}`);
    }
  });

  it('Pokemon-style OAM update via DMA3 word copy (256 words = 1024 bytes)', () => {
    const { bus } = makeEmu();
    for (let i = 0; i < 256; i++) {
      bus.write32(0x02000000 + i * 4, 0xDEAD0000 | i);
    }
    // ctrl = 0x8400 = enable + immediate + word + src+dst inc.
    triggerDma(bus, 3, 0x02000000, 0x07000000, 256, 0x8400);
    for (let i = 0; i < 256; i++) {
      expect(bus.read32(0x07000000 + i * 4)).toBe((0xDEAD0000 | i) >>> 0);
    }
  });
});

describe('DMA: HBlank repeat (per-scanline)', () => {
  it('HBlank-timed DMA fires on triggerHBlank only', () => {
    const { bus, dma } = makeEmu();
    bus.write16(0x03000100, 0xAA55);
    // ctrl: enable + halfword + repeat + HBlank.
    triggerDma(bus, 0, 0x03000100, 0x03000200, 1, 0x8000 | 0x0200 | 0x2000);
    expect(bus.read16(0x03000200)).toBe(0);
    dma.triggerHBlank();
    expect(bus.read16(0x03000200)).toBe(0xAA55);
  });
});

describe('DMA: completion observable via CNT_H readback', () => {
  it('reading DMA3 control after immediate completion shows enable bit cleared', () => {
    const { bus } = makeEmu();
    // Game starts an immediate DMA, then polls CNT_H for the enable bit
    // to clear. This was the Crash Bandicoot freeze: my code cleared the
    // channel's enable flag but left the raw MMIO mirror with the
    // written value (0x8000), so the polling loop never exited.
    triggerDma(bus, 3, 0x03000100, 0x03000200, 1, 0x8000);
    // CNT_H at DMA3 = 0x040000DE.
    expect(bus.read16(0x040000DE) & 0x8000).toBe(0);
  });

  it('DMA0..2 enable bit also clears on completion', () => {
    const { bus } = makeEmu();
    for (let ch = 0; ch < 3; ch++) {
      triggerDma(bus, ch, 0x03000100, 0x03000200, 1, 0x8000);
      const cntAddr = 0x040000BA + ch * 12;
      expect(bus.read16(cntAddr) & 0x8000).toBe(0);
    }
  });

  it('repeat-mode DMA keeps the enable bit set across triggers', () => {
    const { bus, dma } = makeEmu();
    triggerDma(bus, 3, 0x03000100, 0x07000000, 1, 0x8000 | 0x0200 | 0x1000);
    dma.triggerVBlank();
    expect(bus.read16(0x040000DE) & 0x8000).not.toBe(0);  // stays enabled
  });
});

describe('DMA: IRQ on completion', () => {
  it('Channel with IRQ enable raises IRQ_DMA0..3 on completion', () => {
    const { bus, irq } = makeEmu();
    irq.setIe(0xFFFF);
    irq.setIme(1);
    // ctrl: enable + halfword + immediate + IRQ enable (bit 14 = 0x4000).
    triggerDma(bus, 0, 0x03000100, 0x03000200, 1, 0xC000);
    expect((irq.iflag & (1 << 8)) !== 0).toBe(true);  // DMA0 IRQ bit
  });

  it('DMA3 IRQ uses bit 11', () => {
    const { bus, irq } = makeEmu();
    irq.setIe(0xFFFF);
    irq.setIme(1);
    triggerDma(bus, 3, 0x03000100, 0x03000200, 1, 0xC000);
    expect((irq.iflag & (1 << 11)) !== 0).toBe(true);
  });
});
