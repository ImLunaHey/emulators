//! Nintendo DS banner parsing — extracts the embedded 32×32 icon and the
//! English title from a `.nds` ROM so the launcher can show real artwork even
//! though there's no NDS *core* yet. The future NDS core will reuse this.
//!
//! Layout: the cart header holds the banner offset at 0x68. The banner then
//! has the icon bitmap at +0x20 (512 B, 4bpp, sixteen 8×8 tiles in a 4×4 grid),
//! a 16-color BGR555 palette at +0x220, and six 256-byte UTF-16LE titles at
//! +0x240 (Japanese, English, French, German, Italian, Spanish).

/// Decoded DS banner: a 32×32 RGBA icon plus a display title.
pub struct Banner {
    pub title: String,
    pub icon_rgba: Vec<u8>, // 32 * 32 * 4
}

fn u32le(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

pub fn parse_banner(rom: &[u8]) -> Option<Banner> {
    let banner_off = u32le(rom, 0x68)? as usize;
    if banner_off == 0 {
        return None;
    }
    // Through the six base-language titles: 0x240 + 6*256 = 0x840.
    let b = rom.get(banner_off..banner_off + 0x840)?;

    // Palette: 16 × BGR555 → 0xRRGGBB.
    let pal_raw = &b[0x220..0x240];
    let mut palette = [0u32; 16];
    for (i, slot) in palette.iter_mut().enumerate() {
        let c = u16le(pal_raw, i * 2);
        let r = ((c & 0x1F) as u32) << 3;
        let g = (((c >> 5) & 0x1F) as u32) << 3;
        let bl = (((c >> 10) & 0x1F) as u32) << 3;
        *slot = (r << 16) | (g << 8) | bl;
    }

    // Icon: 4bpp, sixteen 8×8 tiles laid out in a 4×4 grid. Palette index 0 is
    // transparent (alpha 0); the launcher fills it with the tile background.
    let bitmap = &b[0x20..0x220];
    let mut icon = vec![0u8; 32 * 32 * 4];
    for ty in 0..4 {
        for tx in 0..4 {
            let tile = (ty * 4 + tx) * 32; // 32 bytes per 8×8 4bpp tile
            for py in 0..8 {
                for px in 0..8 {
                    let byte = bitmap[tile + py * 4 + px / 2];
                    let idx = if px & 1 == 0 { byte & 0xF } else { byte >> 4 } as usize;
                    let x = tx * 8 + px;
                    let y = ty * 8 + py;
                    let o = (y * 32 + x) * 4;
                    if idx != 0 {
                        let col = palette[idx];
                        icon[o] = (col >> 16) as u8;
                        icon[o + 1] = (col >> 8) as u8;
                        icon[o + 2] = col as u8;
                        icon[o + 3] = 0xFF;
                    }
                }
            }
        }
    }

    // English title (index 1) first, falling back to Japanese (index 0).
    let title = read_title(&b[0x340..0x440])
        .or_else(|| read_title(&b[0x240..0x340]))
        .unwrap_or_default();

    Some(Banner { title, icon_rgba: icon })
}

/// First line of a 256-byte UTF-16LE title field (titles use '\n' to separate
/// game / subtitle / maker — we want the game line).
fn read_title(raw: &[u8]) -> Option<String> {
    let mut s = String::new();
    let mut i = 0;
    while i + 1 < raw.len() {
        let c = u16le(raw, i);
        if c == 0 || c == 0x0A {
            break;
        }
        s.push(char::from_u32(c as u32).unwrap_or(' '));
        i += 2;
    }
    let t = s.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_icon_size() {
        // Synthetic ROM: banner at 0x200, English title "Hi", zeroed icon.
        let banner_off = 0x200usize;
        let mut rom = vec![0u8; banner_off + 0x840];
        rom[0x68..0x6C].copy_from_slice(&(banner_off as u32).to_le_bytes());
        // English title at banner + 0x340.
        let t = banner_off + 0x340;
        for (j, ch) in "Hi".encode_utf16().enumerate() {
            rom[t + j * 2..t + j * 2 + 2].copy_from_slice(&ch.to_le_bytes());
        }
        let b = parse_banner(&rom).expect("banner");
        assert_eq!(b.title, "Hi");
        assert_eq!(b.icon_rgba.len(), 32 * 32 * 4);
    }

    #[test]
    fn no_banner_offset_is_none() {
        let rom = vec![0u8; 0x1000];
        assert!(parse_banner(&rom).is_none());
    }
}
