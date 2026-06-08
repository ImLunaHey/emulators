import { Bus, IoBridge } from '../memory/bus';
import { Dma } from './dma';
import { Timers } from './timers';
import { Irq } from './irq';
import { Keypad } from './keypad';
import type { Ppu } from '../ppu/ppu';
import type { Cpu } from '../cpu/cpu';

export class Io implements IoBridge {
  // Generic backing store for IO regs (1 KB). Most reads/writes that don't
  // need side effects hit this directly.
  raw = new Uint8Array(0x400);
  raw16: Uint16Array;
  raw32: Uint32Array;

  postflg = 0;
  haltcnt = 0;
  waitcnt = 0;

  constructor(
    public bus: Bus,
    public ppu: Ppu,
    public dma: Dma,
    public timers: Timers,
    public irq: Irq,
    public keypad: Keypad,
    public cpu: Cpu,
  ) {
    this.raw16 = new Uint16Array(this.raw.buffer);
    this.raw32 = new Uint32Array(this.raw.buffer);
  }

  read8(addr: number): number {
    addr &= 0x3FF;
    const v16 = this.read16(addr & ~1);
    return (addr & 1) ? (v16 >>> 8) & 0xFF : v16 & 0xFF;
  }
  read32(addr: number): number {
    return ((this.read16(addr) | (this.read16(addr + 2) << 16)) >>> 0);
  }
  write8(addr: number, v: number): void {
    addr &= 0x3FF; v &= 0xFF;
    // 8-bit writes to most halfword regs are usually low-byte; we read-modify-write
    // to preserve the other byte. A handful of regs allow byte writes (POSTFLG, HALTCNT).
    if (addr === 0x300) { this.postflg = v; return; }
    if (addr === 0x301) {
      this.haltcnt = v;
      // HALTCNT bit 7: 0 = halt, 1 = stop. We treat both as halt.
      this.cpu.halt();
      return;
    }
    const cur = this.read16(addr & ~1);
    const nv = (addr & 1) ? ((cur & 0x00FF) | (v << 8)) : ((cur & 0xFF00) | v);
    this.write16(addr & ~1, nv);
  }
  write32(addr: number, v: number): void {
    this.write16(addr,     v & 0xFFFF);
    this.write16(addr + 2, (v >>> 16) & 0xFFFF);
  }

  read16(addr: number): number {
    addr &= 0x3FE;
    switch (addr) {
      case 0x000: return this.ppu.dispcnt;
      case 0x004: return this.ppu.readDispstat();
      case 0x006: return this.ppu.vcount & 0xFF;

      case 0x100: case 0x104: case 0x108: case 0x10C:
        return this.timers.readCounter((addr - 0x100) >>> 2);
      case 0x102: case 0x106: case 0x10A: case 0x10E:
        return this.timers.readControl((addr - 0x102) >>> 2);

      case 0x130: return this.keypad.read16();

      case 0x200: return this.irq.ie & 0x3FFF;
      case 0x202: return this.irq.iflag & 0x3FFF;
      case 0x204: return this.waitcnt;
      case 0x208: return this.irq.ime & 1;
    }
    return this.raw16[addr >>> 1];
  }

  write16(addr: number, v: number): void {
    addr &= 0x3FE; v &= 0xFFFF;
    // PPU register block 0x000-0x056.
    if (addr <= 0x056) {
      this.ppu.writeReg(addr, v);
      this.raw16[addr >>> 1] = v;
      return;
    }
    // DMA block 0x0B0-0x0DE.
    if (addr >= 0x0B0 && addr <= 0x0DE) {
      const ch = ((addr - 0x0B0) / 12) | 0;
      const off = (addr - 0x0B0) - ch * 12;
      switch (off) {
        case 0x0: this.dma.writeSrc(ch, (this.dma.ch[ch].src & 0xFFFF0000) | v); break;
        case 0x2: this.dma.writeSrc(ch, (this.dma.ch[ch].src & 0x0000FFFF) | (v << 16)); break;
        case 0x4: this.dma.writeDst(ch, (this.dma.ch[ch].dst & 0xFFFF0000) | v); break;
        case 0x6: this.dma.writeDst(ch, (this.dma.ch[ch].dst & 0x0000FFFF) | (v << 16)); break;
        case 0x8: this.dma.writeCount(ch, v); break;
        case 0xA: this.dma.writeControl(ch, v); break;
      }
      this.raw16[addr >>> 1] = v;
      return;
    }
    // Timers 0x100-0x10E.
    if (addr >= 0x100 && addr <= 0x10E) {
      const i = (addr - 0x100) >>> 2;
      const isReload = (addr & 2) === 0;
      if (isReload) this.timers.writeReload(i, v);
      else          this.timers.writeControl(i, v);
      this.raw16[addr >>> 1] = v;
      return;
    }
    // Interrupt + system.
    switch (addr) {
      case 0x200: this.irq.setIe(v); this.raw16[0x200 >>> 1] = this.irq.ie; return;
      case 0x202: this.irq.ackWrite16(v); this.raw16[0x202 >>> 1] = this.irq.iflag; return;
      case 0x204: this.waitcnt = v; this.raw16[0x204 >>> 1] = v; return;
      case 0x208: this.irq.setIme(v); this.raw16[0x208 >>> 1] = this.irq.ime; return;
    }
    this.raw16[addr >>> 1] = v;
  }
}
