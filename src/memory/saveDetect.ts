// AGB ROMs embed an ASCII signature near the start of code that names
// the save chip the SDK linked against. We scan for the known signatures
// so the emulator can wire the right SaveBridge (SRAM / Flash 64 / Flash
// 128 / EEPROM 512B / EEPROM 8KB). Default fallback is Flash 128 KB,
// which covers all of gen-3 Pokemon.

export type SaveType =
  | 'flash128'   // 128 KB Macronix MX29L1000 — Pokemon FRLG / RSE
  | 'flash64'    // 64 KB Atmel / SST / Panasonic — older AGB titles
  | 'sram'       // 32 KB battery-backed static RAM
  | 'eeprom512'  // 4 Kbit (512 B) serial EEPROM
  | 'eeprom8k'   // 64 Kbit (8 KB) serial EEPROM
  | 'none';

interface Sig { needle: string; type: SaveType; }

// Order matters — match the LONGEST signature for a family first so we
// don't classify "FLASH1M_V102" as plain "FLASH_V" first.
const SIGS: Sig[] = [
  { needle: 'FLASH1M_V',  type: 'flash128'  },
  { needle: 'FLASH512_V', type: 'flash64'   },
  { needle: 'FLASH_V',    type: 'flash64'   },
  { needle: 'EEPROM_V',   type: 'eeprom8k'  }, // 8 KB version is the gen-3+ default; some pre-2003 titles use 512 B
  { needle: 'SRAM_V',     type: 'sram'      },
  { needle: 'SRAM_F_V',   type: 'sram'      },
];

export function detectSaveType(rom: Uint8Array): SaveType {
  for (const sig of SIGS) {
    const needle = sig.needle;
    const n = needle.length;
    // Linear byte-by-byte search. The AGB SDK's save library can end
    // up linked anywhere — Minish Cap (EEPROM_V) sits at ROM offset
    // ~0xEF2F8C, well past the first MB — so we scan the whole ROM.
    // A 32 MB worst-case scan completes in low single-digit ms.
    const limit = rom.length - n;
    outer:
    for (let i = 0; i < limit; i++) {
      for (let k = 0; k < n; k++) {
        if (rom[i + k] !== needle.charCodeAt(k)) continue outer;
      }
      return sig.type;
    }
  }
  // No signature found. Most homebrew (and a tiny handful of stripped
  // commercial dumps) don't have one — fall back to 128 KB Flash since
  // it's the most permissive.
  return 'flash128';
}
