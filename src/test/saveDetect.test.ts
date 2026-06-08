// Save-type detection scans the ROM for the AGB-SDK's embedded
// "FLASH1M_V" / "FLASH_V" / "EEPROM_V" / "SRAM_V" signature strings.
// This test fakes each one in a 1 MB buffer at a representative offset
// and confirms the right SaveType comes back.

import { describe, it, expect } from 'vitest';
import { detectSaveType } from '../memory/saveDetect';

function makeRom(signature: string, offset = 0x1000): Uint8Array {
  const rom = new Uint8Array(1 << 20);
  // Fill with something that isn't another signature.
  rom.fill(0xAA);
  for (let i = 0; i < signature.length; i++) {
    rom[offset + i] = signature.charCodeAt(i);
  }
  return rom;
}

describe('Save type detection', () => {
  it('recognises FLASH1M_V128 as flash128', () => {
    expect(detectSaveType(makeRom('FLASH1M_V128'))).toBe('flash128');
  });
  it('recognises FLASH512_V103 as flash64', () => {
    expect(detectSaveType(makeRom('FLASH512_V103'))).toBe('flash64');
  });
  it('recognises plain FLASH_V123 as flash64', () => {
    expect(detectSaveType(makeRom('FLASH_V123'))).toBe('flash64');
  });
  it('recognises EEPROM_V124 as eeprom8k', () => {
    expect(detectSaveType(makeRom('EEPROM_V124'))).toBe('eeprom8k');
  });
  it('recognises SRAM_V103 as sram', () => {
    expect(detectSaveType(makeRom('SRAM_V103'))).toBe('sram');
  });
  it('FLASH1M wins over FLASH_V when both substrings would match', () => {
    // The longer FLASH1M_V signature contains "FLASH" as a prefix; the
    // detector must prefer the more specific match for correctness.
    expect(detectSaveType(makeRom('FLASH1M_V100'))).toBe('flash128');
  });
  it('unknown ROM falls back to flash128', () => {
    const rom = new Uint8Array(0x10000);
    rom.fill(0xAA);
    expect(detectSaveType(rom)).toBe('flash128');
  });
});
