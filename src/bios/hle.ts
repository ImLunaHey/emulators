import type { Cpu } from '../cpu/cpu';
import type { Bus } from '../memory/bus';
import { IRQ_VBLANK } from '../io/irq';

// High-level emulation of GBA BIOS syscalls. Returning true tells the CPU
// that we already handled the syscall; otherwise it falls through to the
// normal SVC vector entry.
export class BiosHle {
  constructor(public cpu: Cpu, public bus: Bus) {}

  handleSwi(comment: number): boolean {
    const s = this.cpu.state;
    switch (comment) {
      case 0x00: this.softReset(); return true;
      case 0x01: this.registerRamReset(s.r[0]); return true;
      case 0x02: this.cpu.halt(); return true;
      case 0x03: this.cpu.halt(); return true;
      case 0x04: this.intrWait(s.r[0], s.r[1]); return true;
      case 0x05: this.vBlankIntrWait(); return true;
      case 0x06: this.div(); return true;
      case 0x07: this.divArm(); return true;
      case 0x08: this.sqrt(); return true;
      case 0x09: this.arcTan(); return true;
      case 0x0A: this.arcTan2(); return true;
      case 0x0B: this.cpuSet(); return true;
      case 0x0C: this.cpuFastSet(); return true;
      case 0x0D: s.r[0] = 0xBAAE187F; return true; // BiosChecksum
      case 0x0E: this.bgAffineSet(); return true;
      case 0x0F: this.objAffineSet(); return true;
      case 0x10: this.bitUnPack(); return true;
      case 0x11: this.lz77UnComp(false); return true;
      case 0x12: this.lz77UnComp(true);  return true;
      case 0x13: this.huffUnComp(); return true;
      case 0x14: this.rlUnComp(false); return true;
      case 0x15: this.rlUnComp(true);  return true;
      case 0x16: this.diff8(false); return true;
      case 0x17: this.diff8(true);  return true;
      case 0x18: this.diff16(); return true;
      case 0x19: return true;                       // SoundBias
      case 0x1A: case 0x1B: case 0x1C: case 0x1D:
      case 0x1E: case 0x1F: case 0x25: case 0x26:
        return true;                                // sound drivers — silent stub
    }
    return false;
  }

  // -------- Reset / RAM clear --------
  private softReset(): void {
    const s = this.cpu.state;
    // BIOS soft reset reads flag from 0x03007FFA: 0 = ROM, !=0 = EWRAM entry.
    const flag = this.bus.read8(0x03007FFA);
    s.r[0] = s.r[1] = s.r[2] = s.r[3] = s.r[4] = s.r[5] = s.r[6] = s.r[7] = 0;
    s.r[13] = 0x03007F00;
    s.cpsr = 0x1F; // SYS mode, F/I clear, ARM
    s.r[15] = flag ? 0x02000000 : 0x08000000;
    this.cpu.flushPipeline();
  }
  private registerRamReset(mask: number): void {
    if (mask & 0x01) this.bus.ewram.fill(0);
    if (mask & 0x02) this.bus.iwram.fill(0, 0, 0x7E00); // BIOS leaves stack area
    if (mask & 0x04) this.bus.pram.fill(0);
    if (mask & 0x08) this.bus.vram.fill(0);
    if (mask & 0x10) this.bus.oam.fill(0);
    // 0x20 sio, 0x40 sound, 0x80 other IO — we ignore in HLE.
  }

  // -------- Interrupt waits --------
  private intrWait(discardOld: number, wanted: number): void {
    const irq = this.cpu.bus.io && (this.cpu.bus.io as any).irq;
    if (!irq) return;
    if (discardOld) irq.iflag &= ~wanted;
    irq.ime = 1;
    this.cpu.halt();
    // CPU step loop will wake us on next matching IRQ. To make the
    // matching condition correct we leave the SWI to "return"; the
    // game's caller will recheck the flag if needed.
  }
  private vBlankIntrWait(): void {
    const io = this.cpu.bus.io as any;
    if (io && io.irq) {
      io.irq.iflag &= ~IRQ_VBLANK;
      io.irq.ime = 1;
    }
    this.cpu.halt();
  }

  // -------- Math --------
  private div(): void {
    const s = this.cpu.state;
    const num = s.r[0] | 0;
    const den = s.r[1] | 0;
    if (den === 0) return;
    s.r[0] = ((num / den) | 0) >>> 0;
    s.r[1] = (num - (num / den | 0) * den) >>> 0;
    s.r[3] = Math.abs(s.r[0] | 0) >>> 0;
  }
  private divArm(): void {
    const s = this.cpu.state;
    const a = s.r[0]; s.r[0] = s.r[1]; s.r[1] = a;
    this.div();
  }
  private sqrt(): void {
    const s = this.cpu.state;
    s.r[0] = Math.floor(Math.sqrt(s.r[0] >>> 0)) >>> 0;
  }
  private arcTan(): void {
    const s = this.cpu.state;
    const tan = (s.r[0] << 16) >> 16; // signed q1.14
    const a = Math.atan(tan / 0x4000);
    s.r[0] = ((a * 0x8000) / Math.PI) >>> 0 & 0xFFFF;
  }
  private arcTan2(): void {
    const s = this.cpu.state;
    const x = (s.r[0] << 16) >> 16;
    const y = (s.r[1] << 16) >> 16;
    const a = Math.atan2(y, x);
    let v = Math.round((a * 0x8000) / Math.PI);
    if (v < 0) v += 0x10000;
    s.r[0] = v & 0xFFFF;
  }

  // -------- CPU memory ops --------
  private cpuSet(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0, dst = s.r[1] >>> 0;
    const len = s.r[2] & 0x1FFFFF;
    const fixed = (s.r[2] & 0x01000000) !== 0;
    const word = (s.r[2] & 0x04000000) !== 0;
    for (let i = 0; i < len; i++) {
      if (word) {
        this.bus.write32(dst, this.bus.read32(src));
        dst = (dst + 4) >>> 0;
        if (!fixed) src = (src + 4) >>> 0;
      } else {
        this.bus.write16(dst, this.bus.read16(src));
        dst = (dst + 2) >>> 0;
        if (!fixed) src = (src + 2) >>> 0;
      }
    }
  }
  private cpuFastSet(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0, dst = s.r[1] >>> 0;
    // Word count is in R2[20:0], rounded up to the next multiple of 8 words.
    let words = ((s.r[2] & 0x1FFFFF) + 7) & ~7;
    if (words === 0) words = 8;
    const fixed = (s.r[2] & 0x01000000) !== 0;
    for (let i = 0; i < words; i++) {
      this.bus.write32(dst, this.bus.read32(src));
      dst = (dst + 4) >>> 0;
      if (!fixed) src = (src + 4) >>> 0;
    }
  }

  // -------- Affine matrix helpers --------
  private bgAffineSet(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const n = s.r[2] | 0;
    for (let i = 0; i < n; i++) {
      const ox  = this.bus.read32(src) | 0;
      const oy  = this.bus.read32(src + 4) | 0;
      const dx  = (this.bus.read16(src + 8) << 16) >> 16;
      const dy  = (this.bus.read16(src + 10) << 16) >> 16;
      const sx  = (this.bus.read16(src + 12) << 16) >> 16;
      const sy  = (this.bus.read16(src + 14) << 16) >> 16;
      const ang = ((this.bus.read16(src + 16) >>> 8) * 2 * Math.PI) / 256;
      src += 20;
      const cos = Math.cos(ang), sin = Math.sin(ang);
      const pa = Math.round(sx * cos) & 0xFFFF;
      const pb = Math.round(-sx * sin) & 0xFFFF;
      const pc = Math.round(sy * sin) & 0xFFFF;
      const pd = Math.round(sy * cos) & 0xFFFF;
      const startX = ox - dx * (pa | 0) - dy * (pb | 0);
      const startY = oy - dx * (pc | 0) - dy * (pd | 0);
      this.bus.write16(dst, pa); this.bus.write16(dst + 2, pb);
      this.bus.write16(dst + 4, pc); this.bus.write16(dst + 6, pd);
      this.bus.write32(dst + 8, startX >>> 0);
      this.bus.write32(dst + 12, startY >>> 0);
      dst += 16;
    }
  }
  private objAffineSet(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const n = s.r[2] | 0;
    const off = s.r[3] | 0;
    for (let i = 0; i < n; i++) {
      const sx = (this.bus.read16(src) << 16) >> 16;
      const sy = (this.bus.read16(src + 2) << 16) >> 16;
      const ang = ((this.bus.read16(src + 4) >>> 8) * 2 * Math.PI) / 256;
      src += 8;
      const cos = Math.cos(ang), sin = Math.sin(ang);
      this.bus.write16(dst, Math.round(sx * cos) & 0xFFFF);            dst += off;
      this.bus.write16(dst, Math.round(-sx * sin) & 0xFFFF);           dst += off;
      this.bus.write16(dst, Math.round(sy * sin) & 0xFFFF);            dst += off;
      this.bus.write16(dst, Math.round(sy * cos) & 0xFFFF);            dst += off;
    }
  }

  // -------- BitUnPack --------
  private bitUnPack(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const info = s.r[2] >>> 0;
    const srcLen   = this.bus.read16(info);
    const srcBits  = this.bus.read8(info + 2);
    const dstBits  = this.bus.read8(info + 3);
    const offsetW  = this.bus.read32(info + 4);
    const base     = offsetW & 0x7FFFFFFF;
    const zeroOff  = (offsetW & 0x80000000) !== 0;
    const mask = (1 << srcBits) - 1;
    let buffer = 0, bufBits = 0;
    for (let i = 0; i < srcLen; i++) {
      let byte = this.bus.read8(src + i);
      for (let b = 0; b < 8; b += srcBits) {
        const chunk = (byte >> b) & mask;
        let outVal = 0;
        if (chunk !== 0 || zeroOff) outVal = (chunk + base) & ((1 << dstBits) - 1);
        buffer |= outVal << bufBits;
        bufBits += dstBits;
        if (bufBits >= 32) {
          this.bus.write32(dst, buffer >>> 0);
          dst = (dst + 4) >>> 0;
          buffer = 0; bufBits = 0;
        }
      }
    }
    if (bufBits > 0) this.bus.write32(dst, buffer >>> 0);
  }

  // -------- LZ77 --------
  private lz77UnComp(vram: boolean): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const header = this.bus.read32(src);
    let length = header >>> 8;
    src = (src + 4) >>> 0;
    // VRAM mode requires halfword writes — we buffer pairs.
    let halfBuf = 0; let halfBufHas = 0;
    const writeByte = (b: number) => {
      if (!vram) {
        this.bus.write8(dst, b);
        dst = (dst + 1) >>> 0;
        return;
      }
      if (halfBufHas === 0) { halfBuf = b; halfBufHas = 1; }
      else { this.bus.write16(dst, halfBuf | (b << 8)); dst = (dst + 2) >>> 0; halfBufHas = 0; }
    };

    while (length > 0) {
      let flags = this.bus.read8(src); src = (src + 1) >>> 0;
      for (let i = 0; i < 8 && length > 0; i++) {
        if (flags & 0x80) {
          const a = this.bus.read8(src); src = (src + 1) >>> 0;
          const b = this.bus.read8(src); src = (src + 1) >>> 0;
          const len = ((a >> 4) & 0xF) + 3;
          const disp = (((a & 0xF) << 8) | b) + 1;
          for (let k = 0; k < len && length > 0; k++) {
            const back = (dst + halfBufHas) - disp;
            // In VRAM mode, the current byte may be sitting in the
            // halfword buffer (not yet flushed). Reading via bus.read
            // would return stale memory; sense that case and pull from
            // the buffer instead. This fixes sprites that LZ77-self-
            // reference with disp=1 (every-other-byte-corrupt symptom).
            let byte: number;
            if (vram && halfBufHas === 1 && back === dst) {
              byte = halfBuf;
            } else {
              byte = this.bus.read8(back);
            }
            writeByte(byte);
            length--;
          }
        } else {
          const byte = this.bus.read8(src); src = (src + 1) >>> 0;
          writeByte(byte);
          length--;
        }
        flags <<= 1;
      }
    }
    if (halfBufHas) this.bus.write16(dst, halfBuf);
  }

  // -------- Huffman (best-effort: not used by FireRed core path) --------
  private huffUnComp(): void {
    // Minimal stub — FireRed doesn't rely on this for boot.
  }

  // -------- Run-length --------
  private rlUnComp(vram: boolean): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const header = this.bus.read32(src);
    let length = header >>> 8;
    src = (src + 4) >>> 0;
    let halfBuf = 0; let halfBufHas = 0;
    const writeByte = (b: number) => {
      if (!vram) { this.bus.write8(dst, b); dst = (dst + 1) >>> 0; return; }
      if (halfBufHas === 0) { halfBuf = b; halfBufHas = 1; }
      else { this.bus.write16(dst, halfBuf | (b << 8)); dst = (dst + 2) >>> 0; halfBufHas = 0; }
    };
    while (length > 0) {
      const flag = this.bus.read8(src); src = (src + 1) >>> 0;
      if (flag & 0x80) {
        const len = (flag & 0x7F) + 3;
        const byte = this.bus.read8(src); src = (src + 1) >>> 0;
        for (let i = 0; i < len && length > 0; i++) { writeByte(byte); length--; }
      } else {
        const len = (flag & 0x7F) + 1;
        for (let i = 0; i < len && length > 0; i++) {
          writeByte(this.bus.read8(src)); src = (src + 1) >>> 0; length--;
        }
      }
    }
    if (halfBufHas) this.bus.write16(dst, halfBuf);
  }

  // -------- Diff-Filter (8 / 16) --------
  private diff8(vram: boolean): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const header = this.bus.read32(src);
    let length = header >>> 8;
    src = (src + 4) >>> 0;
    let prev = this.bus.read8(src); src = (src + 1) >>> 0;
    let halfBuf = 0; let halfBufHas = 0;
    const writeByte = (b: number) => {
      if (!vram) { this.bus.write8(dst, b); dst = (dst + 1) >>> 0; return; }
      if (halfBufHas === 0) { halfBuf = b; halfBufHas = 1; }
      else { this.bus.write16(dst, halfBuf | (b << 8)); dst = (dst + 2) >>> 0; halfBufHas = 0; }
    };
    writeByte(prev); length--;
    while (length > 0) {
      const d = this.bus.read8(src); src = (src + 1) >>> 0;
      prev = (prev + d) & 0xFF;
      writeByte(prev); length--;
    }
    if (halfBufHas) this.bus.write16(dst, halfBuf);
  }
  private diff16(): void {
    const s = this.cpu.state;
    let src = s.r[0] >>> 0;
    let dst = s.r[1] >>> 0;
    const header = this.bus.read32(src);
    let length = (header >>> 8) >>> 1; // in halfwords
    src = (src + 4) >>> 0;
    let prev = this.bus.read16(src); src = (src + 2) >>> 0;
    this.bus.write16(dst, prev); dst = (dst + 2) >>> 0;
    length--;
    while (length > 0) {
      const d = this.bus.read16(src); src = (src + 2) >>> 0;
      prev = (prev + d) & 0xFFFF;
      this.bus.write16(dst, prev); dst = (dst + 2) >>> 0;
      length--;
    }
  }
}
