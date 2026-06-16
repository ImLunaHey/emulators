//! Cheat code support. Ported from src/io/cheats.ts.
//
// Cheat code support. Accepts the most common GBA cheat-code shapes
// from cheat databases (GameShark v3 / CodeBreaker / Action Replay
// decrypted form, all variants on the 8-byte "address+type | value"
// layout) and applies them every frame after the game's done running.
//
// The wire format on every cheat database I've looked at:
//   "XXXXXXXX YYYYYYYY"
// 8 hex chars + space + 8 hex chars. The TOP NIBBLE of the first word
// selects the operation. Multiple codes per cheat are separated by
// newlines or '+'.

/// A single user-defined cheat.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cheat {
    pub name: String,
    pub code: String, // raw user input (can be multi-line)
    pub enabled: bool,
}

/// The kind of operation a parsed cheat line performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineType {
    Write8,
    Write16,
    Write32,
    Eq16,
    Eq8,
    Unsupported,
}

/// One decoded cheat line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedLine {
    pub r#type: LineType,
    pub address: u32,
    pub value: u32,
}

/// Result of parsing a single hex token: a valid 32-bit value or NaN-like
/// failure. JS used `NaN` here; we model that with `Option`.
fn parse_hex(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|c| c.is_ascii_hexdigit()) {
        // JS `parseInt(s, 16) >>> 0`: parse and coerce to u32.
        // Tokens are at most 8 hex chars here, but match JS's wrapping
        // (>>> 0) by truncating to the low 32 bits.
        Some((u64::from_str_radix(s, 16).unwrap_or(0) & 0xFFFF_FFFF) as u32)
    } else {
        None
    }
}

// Parse one 16-hex-char line into a ParsedLine. Returns null for
// blank lines or comments; the type field is "unsupported" when the
// opcode isn't one of the supported simple-write / conditional kinds.
fn parse_line(raw: &str) -> Option<ParsedLine> {
    // clean = raw.replace(/[^0-9a-fA-F]/g, '')
    let clean: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.is_empty() {
        return None;
    }
    // We accept both wire formats:
    //   - 16 hex chars: GameShark v3 / Action Replay   (8 addr + 8 value)
    //   - 12 hex chars: CodeBreaker / Pokemon-style    (8 addr + 4 value)
    let a: Option<u32>;
    let b: Option<u32>;
    if clean.len() == 16 {
        a = parse_hex(&clean[0..8]);
        b = parse_hex(&clean[8..16]);
    } else if clean.len() == 12 {
        a = parse_hex(&clean[0..8]);
        b = parse_hex(&clean[8..12]);
    } else {
        return Some(ParsedLine {
            r#type: LineType::Unsupported,
            address: 0,
            value: 0,
        });
    }
    // if (isNaN(a) || isNaN(b)) return unsupported
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Some(ParsedLine {
                r#type: LineType::Unsupported,
                address: 0,
                value: 0,
            })
        }
    };
    // Top nibble of word 1 = opcode. Bottom 24 bits = address (high byte
    // implicit 0x02 = EWRAM for most opcodes, 0x03 = IWRAM via 0x3..., or
    // explicit 0x08 = ROM for read-only conditions).
    // Hybrid decoder that handles GameShark v3 / CodeBreaker / Action
    // Replay all in one. Top NIBBLE is the opcode family; the full
    // bottom 28 bits are the address, which is enough range to reach
    // any GBA memory region. (CB-style codes like 82025BCC encode the
    // region prefix into the second nibble — 0x8 = "write16" opcode,
    // and the rest of the word IS the address starting with 0x02xxxxxx
    // for EWRAM. This decoder treats them uniformly.)
    let op = (a >> 28) & 0xF;
    let addr = a & 0x0FFF_FFFF;
    Some(match op {
        0x0 => ParsedLine { r#type: LineType::Write8, address: addr, value: b & 0xFF },
        0x1 => ParsedLine { r#type: LineType::Write16, address: addr, value: b & 0xFFFF },
        0x2 => ParsedLine { r#type: LineType::Write32, address: addr, value: b },
        0x3 => ParsedLine { r#type: LineType::Write8, address: addr, value: b & 0xFF },
        0x4 => ParsedLine { r#type: LineType::Write32, address: addr, value: b },
        0x8 => ParsedLine { r#type: LineType::Write16, address: addr, value: b & 0xFFFF },
        0xD => ParsedLine { r#type: LineType::Eq16, address: addr, value: b & 0xFFFF },
        0xE => ParsedLine { r#type: LineType::Eq8, address: addr, value: b & 0xFF },
        _ => ParsedLine { r#type: LineType::Unsupported, address: addr, value: b },
    })
}

pub fn parse_cheat(code: &str) -> Vec<ParsedLine> {
    let mut out: Vec<ParsedLine> = Vec::new();
    // code.split(/[\n+]/)
    for raw in code.split(['\n', '+']) {
        let trimmed = raw.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        if let Some(parsed) = parse_line(raw) {
            out.push(parsed);
        }
    }
    out
}

// Apply all enabled cheats once. Conditional opcodes (eq8/eq16) gate
// the very next line, mirroring the GS v3 "if this, then poke that"
// pattern that 99% of code-database entries use.
pub fn apply_cheats(bus: &mut dyn crate::bus::Bus, cheats: &[Cheat]) {
    for cheat in cheats {
        if !cheat.enabled || cheat.code.is_empty() {
            continue;
        }
        let lines = parse_cheat(&cheat.code);
        let mut i = 0usize;
        while i < lines.len() {
            let line = lines[i];
            match line.r#type {
                LineType::Eq16 => {
                    if bus.read16(line.address) != line.value {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                LineType::Eq8 => {
                    if bus.read8(line.address) != line.value {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                LineType::Write8 => bus.write8(line.address, line.value),
                LineType::Write16 => bus.write16(line.address, line.value),
                LineType::Write32 => bus.write32(line.address, line.value),
                LineType::Unsupported => {}
            }
            i += 1;
        }
    }
}
