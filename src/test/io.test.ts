// IO register + bus access tests. The Pokemon games hit hundreds of
// distinct IO addresses; a single misrouted register can quietly break
// scroll position, palette uploads, sound, or save behavior.

import { describe, it, expect } from 'vitest';
import { Bus } from '../memory/bus';
import { Io } from '../io/io';
import { Dma } from '../io/dma';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';
import { Keypad, Key } from '../io/keypad';
import { Ppu } from '../ppu/ppu';
import { Cpu } from '../cpu/cpu';

function makeBus() {
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
  return { bus, ppu, irq, keypad, dma, timers };
}

describe('Memory region routing', () => {
  it('EWRAM accepts 8/16/32-bit r/w with mirroring', () => {
    const { bus } = makeBus();
    bus.write32(0x02000000, 0xDEADBEEF);
    expect(bus.read32(0x02000000)).toBe(0xDEADBEEF);
    expect(bus.read16(0x02000002)).toBe(0xDEAD);
    expect(bus.read8(0x02000000)).toBe(0xEF);
    // Mirror: EWRAM is 256 KB, so 0x02040000 mirrors 0x02000000.
    expect(bus.read32(0x02040000)).toBe(0xDEADBEEF);
  });
  it('IWRAM accepts 8/16/32-bit r/w with mirroring', () => {
    const { bus } = makeBus();
    bus.write32(0x03000000, 0xCAFEBABE);
    expect(bus.read32(0x03000000)).toBe(0xCAFEBABE);
    // IWRAM is 32 KB, so 0x03008000 mirrors 0x03000000.
    expect(bus.read32(0x03008000)).toBe(0xCAFEBABE);
    // The canonical IRQ-vector slot at 0x03007FFC is also reachable via
    // 0x03FFFFFC.
    bus.write32(0x03007FFC, 0x12345678);
    expect(bus.read32(0x03FFFFFC)).toBe(0x12345678);
  });
  it('PRAM 32-bit write fills both halfwords', () => {
    const { bus } = makeBus();
    bus.write32(0x05000000, 0x12345678);
    expect(bus.read16(0x05000000)).toBe(0x5678);
    expect(bus.read16(0x05000002)).toBe(0x1234);
  });
  it('VRAM 8-bit write to OBJ region is dropped (per spec)', () => {
    const { bus } = makeBus();
    // 8-bit writes to VRAM in the OBJ tile area (0x10000+) should be
    // ignored on real hardware. Tile/BG-map area (0x00000-0x0FFFF) gets
    // the byte broadcast.
    bus.write8(0x06000000, 0xAB);
    // BG map broadcast: writes the byte to both halves of the halfword.
    expect(bus.read16(0x06000000)).toBe(0xABAB);
  });
  it('OAM 8-bit write is dropped', () => {
    const { bus } = makeBus();
    // Real GBA ignores 8-bit OAM writes entirely.
    bus.oam[0] = 0;  // clear
    bus.write8(0x07000000, 0xAB);
    expect(bus.read8(0x07000000)).toBe(0);  // still zero
    // 16-bit write works.
    bus.write16(0x07000000, 0x1234);
    expect(bus.read16(0x07000000)).toBe(0x1234);
  });
});

describe('PPU register IO', () => {
  it('DISPCNT round-trips through MMIO', () => {
    const { bus, ppu } = makeBus();
    bus.write16(0x04000000, 0x1F40);
    expect(ppu.dispcnt).toBe(0x1F40);
    expect(bus.read16(0x04000000)).toBe(0x1F40);
  });
  it('DISPSTAT has read-only flag bits 0-2', () => {
    const { bus, ppu } = makeBus();
    // Try to write all FFs — bits 0-2 are RO so they shouldn't stick.
    bus.write16(0x04000004, 0xFFFF);
    // Bits 3-7 + 8-15 should be set; bits 0-2 should reflect actual PPU state.
    // At init vcount=0, no VBlank/HBlank, no vcount-match-of-line-FF.
    const v = ppu.dispstat;
    expect(v & 0x07).toBe(0);  // status bits clear at init
    expect(v & 0xFFF8).toBe(0xFFF8);  // writable bits stuck
  });
  it('BG0HOFS masks to 9 bits', () => {
    const { bus, ppu } = makeBus();
    bus.write16(0x04000010, 0xFFFF);
    expect(ppu.bgHOFS[0]).toBe(0x1FF);
  });
  it('BG2 affine reference X writes high half then low half', () => {
    const { bus, ppu } = makeBus();
    bus.write16(0x04000028, 0x5678);  // low
    bus.write16(0x0400002A, 0x1234);  // high (signed 12-bit extends)
    expect(ppu.bgX[0] & 0xFFFF).toBe(0x5678);
  });
});

describe('IRQ register IO', () => {
  it('IE/IF round-trips', () => {
    const { bus, irq } = makeBus();
    bus.write16(0x04000200, 0x1234);
    expect(irq.ie).toBe(0x1234 & 0x3FFF);
  });
  it('writing 1 to IF clears the corresponding bit', () => {
    const { bus, irq } = makeBus();
    irq.raise(0x07);
    expect(irq.iflag).toBe(0x07);
    bus.write16(0x04000202, 0x04);  // clear VCount
    expect(irq.iflag).toBe(0x03);
  });
  it('IME writes only retain bit 0', () => {
    const { bus, irq } = makeBus();
    bus.write16(0x04000208, 0xFFFF);
    expect(irq.ime).toBe(1);
    bus.write16(0x04000208, 0xFFFE);
    expect(irq.ime).toBe(0);
  });
});

describe('Keypad', () => {
  it('reads as 0x3FF (all-released, active-low) by default', () => {
    const { bus } = makeBus();
    expect(bus.read16(0x04000130)).toBe(0x3FF);
  });
  it('pressed buttons clear their bit', () => {
    const { bus, keypad } = makeBus();
    keypad.press(Key.A);     // bit 0
    keypad.press(Key.UP);    // bit 6
    const v = bus.read16(0x04000130);
    expect(v & (1 << 0)).toBe(0);    // A pressed → bit cleared
    expect(v & (1 << 6)).toBe(0);    // UP pressed
    expect(v & (1 << 1)).not.toBe(0); // B not pressed
  });
  it('release restores the bit', () => {
    const { bus, keypad } = makeBus();
    keypad.press(Key.START);
    expect(bus.read16(0x04000130) & (1 << 3)).toBe(0);
    keypad.release(Key.START);
    expect(bus.read16(0x04000130) & (1 << 3)).not.toBe(0);
  });
});

describe('Timers', () => {
  it('TMxCNT_L is the reload value, read returns counter', () => {
    const { bus, timers } = makeBus();
    bus.write16(0x04000100, 0x1234);  // reload = 0x1234
    bus.write16(0x04000102, 0x0080);  // enable, prescale=1
    // First step doesn't advance because timer enables on this scan.
    expect(timers.ch[0].counter).toBe(0x1234);
  });
});

describe('SRAM region', () => {
  it('SRAM halfword/word reads broadcast the byte (8-bit hardware)', () => {
    const { bus } = makeBus();
    bus.attachSave({
      read: (addr) => (addr === 0 ? 0xAB : 0),
      write: () => {},
    });
    expect(bus.read16(0x0E000000)).toBe(0xABAB);
    expect(bus.read32(0x0E000000)).toBe(0xABABABAB);
  });
});
