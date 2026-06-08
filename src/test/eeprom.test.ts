// EEPROM bit-serial round-trip. Encode a write command (1 cmd-bit + 1
// op-bit + 14 addr bits + 64 data bits + 1 terminator), drive it bit by
// bit into the chip, then encode a read command and pull the 64 bits
// back. The data should round-trip cleanly.

import { describe, it, expect } from 'vitest';
import { Eeprom } from '../memory/eeprom';

function sendBits(eep: Eeprom, bits: number[]): void {
  for (const b of bits) eep.write(0, b);
}

function recvBits(eep: Eeprom, n: number): number[] {
  const out: number[] = [];
  for (let i = 0; i < n; i++) out.push(eep.read(0) & 1);
  return out;
}

function bitsFromBytes(bytes: number[]): number[] {
  const out: number[] = [];
  for (const byte of bytes) {
    for (let i = 7; i >= 0; i--) out.push((byte >> i) & 1);
  }
  return out;
}

function bytesFromBits(bits: number[]): number[] {
  const out: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let v = 0;
    for (let k = 0; k < 8; k++) v = (v << 1) | (bits[i + k] & 1);
    out.push(v);
  }
  return out;
}

function addrBits(addr: number, n: number): number[] {
  const out: number[] = [];
  for (let i = n - 1; i >= 0; i--) out.push((addr >> i) & 1);
  return out;
}

describe('Eeprom 8K', () => {
  it('round-trips an 8-byte block through write then read', () => {
    const eep = new Eeprom(8192);
    const block = 0x123;          // arbitrary address in 0..1023
    const payload = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x23, 0x45, 0x67];

    // WRITE: 1 0 [14 addr] [64 data] 0
    sendBits(eep, [1, 0, ...addrBits(block, 14), ...bitsFromBytes(payload), 0]);

    // READ: 1 1 [14 addr] 0
    sendBits(eep, [1, 1, ...addrBits(block, 14), 0]);

    // Response: 4 dummy zeros + 64 data bits.
    const resp = recvBits(eep, 68);
    const dummies = resp.slice(0, 4);
    const data = bytesFromBits(resp.slice(4));

    expect(dummies).toEqual([0, 0, 0, 0]);
    expect(data).toEqual(payload);
  });

  it('separates writes to different blocks', () => {
    const eep = new Eeprom(8192);
    const a = 0x010, b = 0x020;
    const pa = [1, 2, 3, 4, 5, 6, 7, 8];
    const pb = [9, 10, 11, 12, 13, 14, 15, 16];

    sendBits(eep, [1, 0, ...addrBits(a, 14), ...bitsFromBytes(pa), 0]);
    sendBits(eep, [1, 0, ...addrBits(b, 14), ...bitsFromBytes(pb), 0]);

    sendBits(eep, [1, 1, ...addrBits(a, 14), 0]);
    const da = bytesFromBits(recvBits(eep, 68).slice(4));
    sendBits(eep, [1, 1, ...addrBits(b, 14), 0]);
    const db = bytesFromBits(recvBits(eep, 68).slice(4));

    expect(da).toEqual(pa);
    expect(db).toEqual(pb);
  });
});
