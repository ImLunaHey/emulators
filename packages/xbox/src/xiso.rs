//! XISO / XDVDFS disc parsing — mount an original-Xbox disc image (held as a
//! byte slice) and identify the game: walk the filesystem, find `default.xbe`,
//! and read its XBE header + certificate (title, IDs, entry point).
//!
//! This is the slice-based sibling of the streaming `examples/xiso_probe.rs`
//! harness, used by the core itself ([`crate::Xbox::load_rom`]) so loading a disc
//! actually mounts it. Built from the XboxDevWiki "XDVDFS" notes: 2048-byte
//! sectors, a volume descriptor at sector 32 (offset `0x10000`) bracketed by the
//! 20-byte magic `MICROSOFT*XBOX*MEDIA`, pointing at the root directory — a
//! 4-byte-aligned binary search tree of entries keyed by filename.
//!
//! This identifies the disc; it does NOT execute it. Booting a game needs the
//! kernel/NV2A layers the foundation core doesn't have.

use std::collections::HashSet;

const SECTOR: usize = 2048;
const VOLUME_OFFSET: usize = 32 * SECTOR; // 0x10000
const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

/// One entry recovered from the root directory.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: u32,
    pub is_dir: bool,
}

/// What we learned about a mounted disc.
#[derive(Debug, Clone)]
pub struct DiscInfo {
    /// Certificate title name (e.g. "Halo 2").
    pub title: String,
    /// 32-bit title id (high 16 = 2-char publisher code, low 16 = game number).
    pub title_id: u32,
    /// XBE image base address.
    pub base: u32,
    /// De-obfuscated entry point.
    pub entry: u32,
    /// Which XOR key de-obfuscated the entry point ("retail"/"debug"/"unknown").
    pub entry_key: &'static str,
    /// Byte offset + size of default.xbe within the disc image (0 if absent), so
    /// the loader can extract the executable.
    pub xbe_offset: usize,
    pub xbe_size: usize,
    /// Root-directory listing.
    pub files: Vec<FileEntry>,
}

impl DiscInfo {
    /// The 2-character publisher code from the title id (e.g. "MS").
    pub fn publisher(&self) -> String {
        let hi = ((self.title_id >> 24) & 0xFF) as u8;
        let lo = ((self.title_id >> 16) & 0xFF) as u8;
        let s: String = [hi, lo]
            .iter()
            .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
            .collect();
        s
    }
    /// The low-16 game number from the title id.
    pub fn game_number(&self) -> u32 {
        self.title_id & 0xFFFF
    }
}

#[inline]
fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
#[inline]
fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    let s = b.get(o..o + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Decode a UTF-16LE field into a trimmed string.
fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Is this byte slice an XDVDFS (Xbox) disc image?
pub fn is_xdvdfs(disc: &[u8]) -> bool {
    disc.get(VOLUME_OFFSET..VOLUME_OFFSET + 20) == Some(&MAGIC[..])
}

/// Walk a root-directory tree (iteratively, with a visited guard) and collect
/// every entry.
fn walk_dir(buf: &[u8]) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let mut stack = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(o) = stack.pop() {
        if o + 14 > buf.len() || !seen.insert(o) {
            continue;
        }
        let l = rd_u16(buf, o).unwrap_or(0xFFFF);
        let r = rd_u16(buf, o + 2).unwrap_or(0xFFFF);
        let size = rd_u32(buf, o + 8).unwrap_or(0);
        let attr = buf[o + 12];
        let nlen = buf[o + 13] as usize;
        if let Some(name_bytes) = buf.get(o + 14..o + 14 + nlen) {
            out.push(FileEntry {
                name: String::from_utf8_lossy(name_bytes).to_string(),
                size,
                is_dir: attr & 0x10 != 0,
            });
        }
        // 0xFFFF is the "no child" sentinel; offsets are in 4-byte units.
        if l != 0xFFFF {
            stack.push(l as usize * 4);
        }
        if r != 0xFFFF {
            stack.push(r as usize * 4);
        }
        if out.len() > 4096 {
            break; // pathological tree guard
        }
    }
    out
}

/// Parse an XDVDFS disc image. Returns `None` if it isn't a valid Xbox disc or
/// the structure is malformed.
pub fn probe(disc: &[u8]) -> Option<DiscInfo> {
    if !is_xdvdfs(disc) {
        return None;
    }
    let root_sector = rd_u32(disc, VOLUME_OFFSET + 0x14)? as usize;
    let root_size = rd_u32(disc, VOLUME_OFFSET + 0x18)? as usize;
    let root_off = root_sector.checked_mul(SECTOR)?;
    let root = disc.get(root_off..root_off + root_size)?;

    let mut files = walk_dir(root);
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    // Locate default.xbe to read the title certificate.
    let xbe = files
        .iter()
        .find(|f| !f.is_dir && f.name.eq_ignore_ascii_case("default.xbe"));

    // We need the start sector, which walk_dir doesn't keep — re-scan the raw
    // tree for the entry's start sector (cheap; root dirs are tiny).
    let (mut title, mut title_id, mut base, mut entry, mut key) =
        (String::new(), 0u32, 0u32, 0u32, "unknown");
    let mut xbe_offset = 0usize;
    let mut xbe_size = 0usize;
    if let Some(xe) = xbe {
        if let Some(start) = find_start_sector(root, "default.xbe") {
            let xbe_off = (start as usize).checked_mul(SECTOR)?;
            xbe_offset = xbe_off;
            xbe_size = xe.size as usize;
            if let Some(hdr) = disc.get(xbe_off..xbe_off + 0x1000) {
                if &hdr[0..4] == b"XBEH" {
                    base = rd_u32(hdr, 0x104)?;
                    let cert_addr = rd_u32(hdr, 0x118)?;
                    let entry_enc = rd_u32(hdr, 0x128)?;
                    const XOR_RETAIL: u32 = 0xA8FC_57AB;
                    const XOR_DEBUG: u32 = 0x9485_9D4B;
                    let r = entry_enc ^ XOR_RETAIL;
                    let d = entry_enc ^ XOR_DEBUG;
                    let (e, k) = if r >= base && r < base + 0x1000_0000 {
                        (r, "retail")
                    } else if d >= base && d < base + 0x1000_0000 {
                        (d, "debug")
                    } else {
                        (entry_enc, "unknown")
                    };
                    entry = e;
                    key = k;
                    let cert_off = cert_addr.wrapping_sub(base) as usize;
                    if let Some(cert) = hdr.get(cert_off..cert_off + 0x5C) {
                        title_id = rd_u32(cert, 0x08).unwrap_or(0);
                        title = utf16le(&cert[0x0C..0x0C + 80]);
                    }
                }
            }
        }
    }

    Some(DiscInfo {
        title,
        title_id,
        base,
        entry,
        entry_key: key,
        xbe_offset,
        xbe_size,
        files,
    })
}

/// Find a named entry within one directory table. Returns `(start_sector,
/// size_bytes, is_dir)`. Case-insensitive (XDVDFS is).
fn find_in_dir(dir: &[u8], name: &str) -> Option<(u32, u32, bool)> {
    let mut stack = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(o) = stack.pop() {
        if o + 14 > dir.len() || !seen.insert(o) {
            continue;
        }
        let l = rd_u16(dir, o).unwrap_or(0xFFFF);
        let r = rd_u16(dir, o + 2).unwrap_or(0xFFFF);
        let start = rd_u32(dir, o + 4).unwrap_or(0);
        let size = rd_u32(dir, o + 8).unwrap_or(0);
        let attr = dir[o + 12];
        let nlen = dir[o + 13] as usize;
        if let Some(nb) = dir.get(o + 14..o + 14 + nlen) {
            if nb.eq_ignore_ascii_case(name.as_bytes()) {
                return Some((start, size, attr & 0x10 != 0));
            }
        }
        if l != 0xFFFF {
            stack.push(l as usize * 4);
        }
        if r != 0xFFFF {
            stack.push(r as usize * 4);
        }
    }
    None
}

/// Resolve a disc-relative path (components separated by `/` or `\`, no device
/// prefix) to `(byte_offset, size)` of the file within the disc image. Descends
/// subdirectories. Returns `None` if any component is missing or the target is a
/// directory.
pub fn resolve_path(disc: &[u8], path: &str) -> Option<(usize, usize)> {
    let comps: Vec<&str> = path.split(['/', '\\']).filter(|c| !c.is_empty()).collect();
    if comps.is_empty() {
        return None;
    }
    let mut dir_off = (rd_u32(disc, VOLUME_OFFSET + 0x14)? as usize).checked_mul(SECTOR)?;
    let mut dir_size = rd_u32(disc, VOLUME_OFFSET + 0x18)? as usize;
    for (i, comp) in comps.iter().enumerate() {
        let dir = disc.get(dir_off..dir_off + dir_size)?;
        let (start, size, is_dir) = find_in_dir(dir, comp)?;
        if i == comps.len() - 1 {
            return if is_dir {
                None
            } else {
                Some(((start as usize).checked_mul(SECTOR)?, size as usize))
            };
        }
        if !is_dir {
            return None;
        }
        dir_off = (start as usize).checked_mul(SECTOR)?;
        dir_size = size as usize;
    }
    None
}

/// Re-scan the raw root tree for a named entry's start sector.
fn find_start_sector(buf: &[u8], name: &str) -> Option<u32> {
    let mut stack = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(o) = stack.pop() {
        if o + 14 > buf.len() || !seen.insert(o) {
            continue;
        }
        let l = rd_u16(buf, o).unwrap_or(0xFFFF);
        let r = rd_u16(buf, o + 2).unwrap_or(0xFFFF);
        let start = rd_u32(buf, o + 4).unwrap_or(0);
        let nlen = buf[o + 13] as usize;
        if let Some(nb) = buf.get(o + 14..o + 14 + nlen) {
            if nb.eq_ignore_ascii_case(name.as_bytes()) {
                return Some(start);
            }
        }
        if l != 0xFFFF {
            stack.push(l as usize * 4);
        }
        if r != 0xFFFF {
            stack.push(r as usize * 4);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny synthetic XDVDFS image with one file (default.xbe) so the
    /// parser can be tested without a real 4.7 GB disc.
    fn synth() -> Vec<u8> {
        // Layout: volume @ sector 32, root dir @ sector 33, xbe @ sector 34.
        let mut disc = vec![0u8; 40 * SECTOR];
        // volume descriptor
        disc[VOLUME_OFFSET..VOLUME_OFFSET + 20].copy_from_slice(MAGIC);
        disc[VOLUME_OFFSET + 0x14..VOLUME_OFFSET + 0x18].copy_from_slice(&33u32.to_le_bytes());
        let root_size = 32u32;
        disc[VOLUME_OFFSET + 0x18..VOLUME_OFFSET + 0x1C].copy_from_slice(&root_size.to_le_bytes());
        // root dir entry @ sector 33
        let rd = 33 * SECTOR;
        disc[rd..rd + 2].copy_from_slice(&0xFFFFu16.to_le_bytes()); // no left
        disc[rd + 2..rd + 4].copy_from_slice(&0xFFFFu16.to_le_bytes()); // no right
        disc[rd + 4..rd + 8].copy_from_slice(&34u32.to_le_bytes()); // start sector
        disc[rd + 8..rd + 12].copy_from_slice(&0x1000u32.to_le_bytes()); // size
        disc[rd + 12] = 0x20; // archive attr (file)
        disc[rd + 13] = 11; // name len
        disc[rd + 14..rd + 25].copy_from_slice(b"default.xbe");
        // XBE @ sector 34
        let xb = 34 * SECTOR;
        disc[xb..xb + 4].copy_from_slice(b"XBEH");
        let base = 0x0001_0000u32;
        disc[xb + 0x104..xb + 0x108].copy_from_slice(&base.to_le_bytes());
        let cert_addr = base + 0x200;
        disc[xb + 0x118..xb + 0x11C].copy_from_slice(&cert_addr.to_le_bytes());
        let entry_enc = 0x0020_0000u32 ^ 0xA8FC_57AB; // retail-keyed 0x200000
        disc[xb + 0x128..xb + 0x12C].copy_from_slice(&entry_enc.to_le_bytes());
        // certificate @ base+0x200 -> file offset 0x200
        let cert = xb + 0x200;
        disc[cert + 0x08..cert + 0x0C].copy_from_slice(&0x4D53_0064u32.to_le_bytes()); // MS-100
        for (i, ch) in "Test Game".encode_utf16().enumerate() {
            disc[cert + 0x0C + i * 2..cert + 0x0C + i * 2 + 2].copy_from_slice(&ch.to_le_bytes());
        }
        disc
    }

    #[test]
    fn detects_and_parses_synthetic_disc() {
        let disc = synth();
        assert!(is_xdvdfs(&disc));
        let info = probe(&disc).expect("parse");
        assert_eq!(info.title, "Test Game");
        assert_eq!(info.title_id, 0x4D53_0064);
        assert_eq!(info.publisher(), "MS");
        assert_eq!(info.game_number(), 0x64);
        assert_eq!(info.base, 0x0001_0000);
        assert_eq!(info.entry, 0x0020_0000);
        assert_eq!(info.entry_key, "retail");
        assert!(info.files.iter().any(|f| f.name == "default.xbe"));
    }

    #[test]
    fn rejects_non_xbox_data() {
        let junk = vec![0u8; 0x20000];
        assert!(!is_xdvdfs(&junk));
        assert!(probe(&junk).is_none());
    }

    #[test]
    fn resolve_path_finds_root_file() {
        let disc = synth();
        // default.xbe is at sector 34, size 0x1000 (see synth()).
        assert_eq!(
            resolve_path(&disc, "default.xbe"),
            Some((34 * SECTOR, 0x1000))
        );
        // Case-insensitive, and tolerant of a leading slash / backslashes.
        assert_eq!(resolve_path(&disc, "DEFAULT.XBE"), Some((34 * SECTOR, 0x1000)));
        assert_eq!(resolve_path(&disc, "\\default.xbe"), Some((34 * SECTOR, 0x1000)));
        // Missing files and bad subpaths resolve to None.
        assert_eq!(resolve_path(&disc, "nope.bin"), None);
        assert_eq!(resolve_path(&disc, "default.xbe/inner"), None);
    }
}
