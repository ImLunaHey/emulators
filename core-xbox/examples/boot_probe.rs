//! Headless boot probe: stream an XISO (or take a raw .xbe), load default.xbe,
//! run the CPU, and report how far it got. Streams the disc so a full 4.7 GB
//! image never lands in RAM. Used to iterate the interpreter toward booting.
//!
//!   cargo run --example boot_probe -- <disc.xiso.iso | game.xbe> [frames]

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use xbox_core::Xbox;

const SECTOR: u64 = 2048;
const VOLUME: u64 = 32 * SECTOR;
const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn read_at(f: &mut File, off: u64, len: usize) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v)?;
    Ok(v)
}

/// Find (start_sector, size) of a file in the XDVDFS root directory.
fn find_file(root: &[u8], name: &str) -> Option<(u32, u32)> {
    let mut stack = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(o) = stack.pop() {
        if o + 14 > root.len() || !seen.insert(o) {
            continue;
        }
        let l = rd16(root, o);
        let r = rd16(root, o + 2);
        let start = rd32(root, o + 4);
        let size = rd32(root, o + 8);
        let nlen = root[o + 13] as usize;
        if let Some(nb) = root.get(o + 14..o + 14 + nlen) {
            if nb.eq_ignore_ascii_case(name.as_bytes()) {
                return Some((start, size));
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

fn extract_xbe(path: &str) -> Vec<u8> {
    // Raw .xbe?
    let mut head = vec![0u8; 4];
    let mut f = File::open(path).expect("open");
    let _ = f.read(&mut head);
    if &head == b"XBEH" {
        f.seek(SeekFrom::Start(0)).unwrap();
        let mut all = Vec::new();
        f.read_to_end(&mut all).unwrap();
        return all;
    }
    // XDVDFS disc.
    let vol = read_at(&mut f, VOLUME, 0x800).expect("read volume");
    assert_eq!(&vol[0..20], MAGIC, "not an XDVDFS disc");
    let root_sector = rd32(&vol, 0x14) as u64;
    let root_size = rd32(&vol, 0x18) as usize;
    let root = read_at(&mut f, root_sector * SECTOR, root_size).expect("read root");
    let (start, size) = find_file(&root, "default.xbe").expect("no default.xbe");
    println!("default.xbe @ sector {start}, {size} bytes");
    read_at(&mut f, start as u64 * SECTOR, size as usize).expect("read xbe")
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: boot_probe <disc|xbe> [frames] [full]");
    let frames: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    // "full" mode reads the entire disc into RAM (4.7 GB) and goes through the
    // real load_rom path, so the HLE filesystem can serve file reads. Default
    // mode streams just default.xbe (fast; FS opens return not-found).
    let full = std::env::args().nth(3).as_deref() == Some("full");

    let mut xb = Xbox::new();
    if full {
        println!("reading full disc into memory…");
        let disc = std::fs::read(&path).expect("read disc");
        xb.load_rom(disc);
    } else {
        let xbe = extract_xbe(&path);
        let ok = xb.boot_xbe(&xbe, "GAME");
        println!("boot_xbe = {ok}");
        if !ok {
            return;
        }
    }
    for _ in 0..frames {
        xb.run_frame();
    }
    println!("--- boot diagnostic ---");
    for line in xb.boot_diagnostic() {
        println!("  {line}");
    }
    // Dump the bytes around the current EIP (to decode a spin loop).
    let eip = xb.cpu.eip;
    print!("  bytes @ {:08X}:", eip.saturating_sub(16));
    for i in eip.saturating_sub(16)..eip.wrapping_add(24) {
        if i == eip {
            print!(" |");
        }
        print!("{:02X} ", xb.mem.ram_read8(i));
    }
    println!();
    let imports = xb.kernel_imports();
    println!("--- {} kernel imports (ordinals) ---", imports.len());
    for chunk in imports.chunks(16) {
        let s: Vec<String> = chunk.iter().map(|o| format!("{o:3}")).collect();
        println!("  {}", s.join(" "));
    }
}
