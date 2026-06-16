// AGB ROMs embed an ASCII signature near the start of code that names
// the save chip the SDK linked against. We scan for the known signatures
// so the emulator can wire the right SaveBridge (SRAM / Flash 64 / Flash
// 128 / EEPROM 512B / EEPROM 8KB). Default fallback is Flash 128 KB,
// which covers all of gen-3 Pokemon.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveType {
    Flash128,  // 128 KB Macronix MX29L1000 — Pokemon FRLG / RSE
    Flash64,   // 64 KB Atmel / SST / Panasonic — older AGB titles
    Sram,      // 32 KB battery-backed static RAM
    Eeprom512, // 4 Kbit (512 B) serial EEPROM
    Eeprom8k,  // 64 Kbit (8 KB) serial EEPROM
    None,
}

struct Sig {
    needle: &'static str,
    ty: SaveType,
}

// Order matters — match the LONGEST signature for a family first so we
// don't classify "FLASH1M_V102" as plain "FLASH_V" first.
const SIGS: &[Sig] = &[
    Sig { needle: "FLASH1M_V",  ty: SaveType::Flash128  },
    Sig { needle: "FLASH512_V", ty: SaveType::Flash64   },
    Sig { needle: "FLASH_V",    ty: SaveType::Flash64   },
    Sig { needle: "EEPROM_V",   ty: SaveType::Eeprom8k  }, // 8 KB version is the gen-3+ default; some pre-2003 titles use 512 B
    Sig { needle: "SRAM_V",     ty: SaveType::Sram      },
    Sig { needle: "SRAM_F_V",   ty: SaveType::Sram      },
];

pub fn detect_save_type(rom: &[u8]) -> SaveType {
    for sig in SIGS {
        let needle = sig.needle.as_bytes();
        let n = needle.len();
        // Linear byte-by-byte search. The AGB SDK's save library can end
        // up linked anywhere — Minish Cap (EEPROM_V) sits at ROM offset
        // ~0xEF2F8C, well past the first MB — so we scan the whole ROM.
        // A 32 MB worst-case scan completes in low single-digit ms.
        if rom.len() < n {
            continue;
        }
        let limit = rom.len() - n;
        'outer: for i in 0..limit {
            for k in 0..n {
                if rom[i + k] != needle[k] {
                    continue 'outer;
                }
            }
            return sig.ty;
        }
    }
    // No signature found. Most homebrew (and a tiny handful of stripped
    // commercial dumps) don't have one — fall back to 128 KB Flash since
    // it's the most permissive.
    SaveType::Flash128
}
