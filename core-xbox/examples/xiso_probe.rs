//! XISO/XDVDFS disc probe — mounts an original-Xbox disc image, finds the game
//! executable (`default.xbe`), and parses its XBE header + certificate.
//!
//! This is a native debugging harness (it streams the file with `std::fs`, so it
//! handles full-size 4.7 GB discs that could never fit in a wasm32 address
//! space). It is the first real step of an Xbox disc loader: proving the core can
//! mount a redump XISO and read actual game data. It does NOT execute anything —
//! the CPU/GPU foundation can't boot a game yet.
//!
//! Run: `cargo run --example xiso_probe -- "/path/to/game.xiso.iso"`
//!
//! XDVDFS layout (XboxDevWiki "XDVDFS"): 2048-byte sectors; the volume descriptor
//! lives at sector 32 (offset 0x10000) and begins/ends with the 20-byte magic
//! `MICROSOFT*XBOX*MEDIA`. It points at the root directory, a 4-byte-aligned
//! binary search tree of entries keyed by filename.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const SECTOR: u64 = 2048;
const VOLUME_SECTOR: u64 = 32;
const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

/// One directory entry we recovered from the root tree.
struct Dirent {
    name: String,
    start_sector: u32,
    size: u32,
    attributes: u8,
}

fn read_at(f: &mut File, off: u64, len: usize) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Walk a root-directory binary tree (iteratively, with a visited guard) and
/// collect every entry.
fn walk_dir(buf: &[u8]) -> Vec<Dirent> {
    let mut out = Vec::new();
    let mut stack = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(o) = stack.pop() {
        if o + 14 > buf.len() || !seen.insert(o) {
            continue;
        }
        let l = rd_u16(buf, o);
        let r = rd_u16(buf, o + 2);
        let start = rd_u32(buf, o + 4);
        let size = rd_u32(buf, o + 8);
        let attr = buf[o + 12];
        let nlen = buf[o + 13] as usize;
        if o + 14 + nlen > buf.len() {
            continue;
        }
        let name = String::from_utf8_lossy(&buf[o + 14..o + 14 + nlen]).to_string();
        out.push(Dirent {
            name,
            start_sector: start,
            size,
            attributes: attr,
        });
        // 0xFFFF is the "no child" sentinel; offsets are in 4-byte units.
        if l != 0xFFFF {
            stack.push(l as usize * 4);
        }
        if r != 0xFFFF {
            stack.push(r as usize * 4);
        }
    }
    out
}

/// Decode a UTF-16LE fixed field into a trimmed Rust string.
fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: cargo run --example xiso_probe -- <disc.xiso.iso>");
            std::process::exit(2);
        }
    };

    let mut f = File::open(&path).unwrap_or_else(|e| {
        eprintln!("cannot open {path}: {e}");
        std::process::exit(1);
    });
    let total = f.metadata().map(|m| m.len()).unwrap_or(0);
    println!("disc image: {path}");
    println!("size: {total} bytes ({:.2} GB)\n", total as f64 / 1e9);

    // --- volume descriptor ---
    let vol = read_at(&mut f, VOLUME_SECTOR * SECTOR, 0x800).expect("read volume descriptor");
    if &vol[0..20] != MAGIC {
        eprintln!("not an XDVDFS volume (magic mismatch at sector 32)");
        std::process::exit(1);
    }
    let root_sector = rd_u32(&vol, 0x14);
    let root_size = rd_u32(&vol, 0x18);
    println!("XDVDFS volume OK  (magic '{}')", std::str::from_utf8(MAGIC).unwrap());
    println!("root dir: sector {root_sector}, {root_size} bytes\n");

    // --- root directory ---
    let root = read_at(&mut f, root_sector as u64 * SECTOR, root_size as usize)
        .expect("read root directory");
    let mut entries = walk_dir(&root);
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    println!("root contains {} entries:", entries.len());
    for e in &entries {
        let kind = if e.attributes & 0x10 != 0 { "DIR " } else { "FILE" };
        println!("  {kind} {:<24} {:>10} bytes", e.name, e.size);
    }
    println!();

    // --- find + parse default.xbe ---
    let xbe = match entries.iter().find(|e| e.name.eq_ignore_ascii_case("default.xbe")) {
        Some(e) => e,
        None => {
            println!("no default.xbe in root — cannot identify the title");
            return;
        }
    };
    let xbe_off = xbe.start_sector as u64 * SECTOR;
    let hdr = read_at(&mut f, xbe_off, 0x1000).expect("read XBE header");
    if &hdr[0..4] != b"XBEH" {
        println!("default.xbe has no 'XBEH' magic — unexpected");
        return;
    }
    let base = rd_u32(&hdr, 0x104);
    let cert_addr = rd_u32(&hdr, 0x118);
    let entry_enc = rd_u32(&hdr, 0x128);
    println!("default.xbe ({} bytes)  magic XBEH OK", xbe.size);
    println!("  base address : {base:#010X}");

    // Entry point is XOR-obfuscated; retail and debug use different keys. Pick the
    // key that yields a plausible (within-image) address.
    const XOR_RETAIL: u32 = 0xA8FC_57AB;
    const XOR_DEBUG: u32 = 0x9485_9D4B;
    let (entry, kind) = {
        let r = entry_enc ^ XOR_RETAIL;
        let d = entry_enc ^ XOR_DEBUG;
        if r >= base && r < base + 0x1000_0000 {
            (r, "retail")
        } else if d >= base && d < base + 0x1000_0000 {
            (d, "debug")
        } else {
            (entry_enc, "unknown")
        }
    };
    println!("  entry point  : {entry:#010X}  ({kind} key)");

    // --- certificate (title name + IDs) ---
    let cert_off = cert_addr.wrapping_sub(base) as usize;
    if cert_off + 0xAC <= hdr.len() {
        let cert = &hdr[cert_off..];
        let title_id = rd_u32(cert, 0x08);
        let title = utf16le(&cert[0x0C..0x0C + 80]);
        // Title ID: high 16 bits = 2-char publisher code, low 16 = game number.
        let pub_hi = ((title_id >> 24) & 0xFF) as u8 as char;
        let pub_lo = ((title_id >> 16) & 0xFF) as u8 as char;
        let game_no = title_id & 0xFFFF;
        println!("  title        : \"{title}\"");
        println!("  title id     : {title_id:#010X}  (publisher {pub_hi}{pub_lo}, game {game_no})");
    }

    println!("\nmounted + identified OK. (Execution is not implemented — core-xbox");
    println!("is a CPU/bus/GPU foundation; it cannot boot this game yet.)");
}
