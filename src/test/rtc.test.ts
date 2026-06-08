// RTC bit-bang protocol tests. Drive the S-3511A like the AGB SDK does:
// toggle CS high, clock 8 bits of command MSB-first on rising SCK, then
// either clock reply bytes back LSB-first or write data bytes LSB-first.
// Bugs caught by this suite so far:
//   1. Sampling was on the falling SCK edge (must be rising).
//   2. Status writes were silently dropped (host writes then read-back
//      mismatched → game shows "battery has run dry").
//   3. SIO mirror line overwrote the chip's reply bit, so the host
//      always read its own (zero) SIO write back instead of the reply.

import { describe, it, expect } from 'vitest';
import { Rtc } from '../memory/rtc';

function enableAndStart(rtc: Rtc) {
  rtc.write(0xC8, 1);              // GPIO enable
  rtc.write(0xC6, 0x05);           // dir: SCK + CS = OUT, SIO = IN (chip drives)
  rtc.write(0xC4, 0);              // CS=0, SCK=0
  rtc.write(0xC4, 0x04);           // CS=1, SCK=0 (start transaction)
}

// Bit-bang one byte of command (MSB-first) to the chip.
function sendCmdByte(rtc: Rtc, b: number) {
  // Set direction so SIO is an output (host drives).
  rtc.write(0xC6, 0x07);
  for (let i = 0; i < 8; i++) {
    const bit = (b >> (7 - i)) & 1;
    rtc.write(0xC4, 0x04 | (bit << 1) | 0);  // CS=1, SCK=0, SIO=bit
    rtc.write(0xC4, 0x04 | (bit << 1) | 1);  // CS=1, SCK=1, SIO=bit (rising edge)
  }
}

// Bit-bang one byte of data write (LSB-first) — same direction setup.
function sendDataByte(rtc: Rtc, b: number) {
  rtc.write(0xC6, 0x07);
  for (let i = 0; i < 8; i++) {
    const bit = (b >> i) & 1;
    rtc.write(0xC4, 0x04 | (bit << 1) | 0);
    rtc.write(0xC4, 0x04 | (bit << 1) | 1);
  }
}

// Read one byte of reply (LSB-first).
function readByte(rtc: Rtc): number {
  rtc.write(0xC6, 0x05);  // SIO as INPUT to host
  let b = 0;
  for (let i = 0; i < 8; i++) {
    rtc.write(0xC4, 0x04);              // SCK = 0
    rtc.write(0xC4, 0x05);              // SCK = 1 — chip drives SIO bit
    const sio = (rtc.read(0xC4) >> 1) & 1;
    b |= sio << i;
  }
  return b;
}

function endTransaction(rtc: Rtc) {
  rtc.write(0xC4, 0);  // CS = 0
}

describe('RTC: enable + direction', () => {
  it('reads disabled if GPIO not enabled', () => {
    const rtc = new Rtc();
    rtc.write(0xC4, 0x07);
    expect(rtc.read(0xC4)).toBe(0);
  });
  it('GPIO_DIR write is observable', () => {
    const rtc = new Rtc();
    rtc.write(0xC8, 1);
    rtc.write(0xC6, 0x07);
    expect(rtc.read(0xC6)).toBe(0x07);
  });
});

describe('RTC: status register round-trip', () => {
  it('status read returns 0x80 (POW bit set) on fresh chip', () => {
    const rtc = new Rtc();
    enableAndStart(rtc);
    // Command byte: 0110_RRRX = 0x60 | (reg << 1) | R/W. reg=1, R/W=1 (read).
    // → 0110_0011 = 0x63.
    sendCmdByte(rtc, 0x63);
    const b = readByte(rtc);
    endTransaction(rtc);
    // Fresh chip indicates power-on / battery-just-inserted via POW bit.
    // Games (Pokemon Ruby/Sapphire/Emerald in particular) check this to
    // decide whether they need to prompt for date setup.
    expect(b).toBe(0x80);
  });
  it('status write then read back returns the written value', () => {
    const rtc = new Rtc();
    enableAndStart(rtc);
    // Write: reg=1, R/W=0 (write) → 0110_0010 = 0x62.
    sendCmdByte(rtc, 0x62);
    sendDataByte(rtc, 0x42);
    endTransaction(rtc);

    enableAndStart(rtc);
    sendCmdByte(rtc, 0x63);  // read status
    const b = readByte(rtc);
    endTransaction(rtc);
    expect(b).toBe(0x42);
  });
});

describe('RTC: date/time read', () => {
  it('reads 7 BCD bytes that look like a plausible current time', () => {
    const rtc = new Rtc();
    enableAndStart(rtc);
    // Command: reg=2 (date+time), R/W=1 → 0110_0101 = 0x65.
    sendCmdByte(rtc, 0x65);
    const bytes: number[] = [];
    for (let i = 0; i < 7; i++) bytes.push(readByte(rtc));
    endTransaction(rtc);
    // Each byte should be valid BCD (both nibbles < 10).
    for (const b of bytes) {
      expect((b & 0xF)).toBeLessThan(10);
      expect(((b >> 4) & 0xF)).toBeLessThan(10);
    }
    // Year (byte 0) should be < 100.
    expect(bytes[0]).toBeLessThan(0xA0);
    // Month (byte 1) should be 1..12 in BCD (0x01..0x12).
    expect(bytes[1]).toBeGreaterThanOrEqual(0x01);
    expect(bytes[1]).toBeLessThanOrEqual(0x12);
    // Day (byte 2) should be 1..31.
    expect(bytes[2]).toBeGreaterThanOrEqual(0x01);
    expect(bytes[2]).toBeLessThanOrEqual(0x31);
  });
});

describe('RTC: reset command resets status', () => {
  it('cmd 0x60 (reg=0, write) restores 24h mode + clears POW', () => {
    const rtc = new Rtc();
    rtc.status = 0xFF;  // pollute
    enableAndStart(rtc);
    sendCmdByte(rtc, 0x60);
    endTransaction(rtc);
    // After reset command the chip is back to the standard initialized
    // state (24h mode set, POW cleared).
    expect(rtc.status).toBe(0x40);
  });
});
