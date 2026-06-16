//! XBE (Xbox executable) parsing + loading.
//!
//! The XBE is the Xbox's PE-like executable format. This module parses the
//! header (entry point, image base, section table, kernel-import thunk) and maps
//! the sections into emulated RAM so the CPU can start executing the game's real
//! x86 code. Built from the XboxDevWiki "XBE" notes.
//!
//! Two fields are XOR-obfuscated with per-variant keys: the entry point and the
//! kernel-image thunk address (the keys differ between the two fields). We pick
//! the key whose result lands inside the image.
//!
//! This loads + starts a game; it does NOT provide the Xbox kernel or GPU, so
//! execution runs the CRT/startup code until it calls an OS import or hits an
//! instruction the interpreter doesn't implement yet.

/// One XBE section (a region copied from the file to a virtual address).
#[derive(Debug, Clone)]
pub struct Section {
    pub flags: u32,
    pub vaddr: u32,
    pub vsize: u32,
    pub raw_addr: u32,
    pub raw_size: u32,
}

/// Parsed XBE image header.
#[derive(Debug, Clone)]
pub struct XbeImage {
    pub base: u32,
    pub entry: u32,
    pub entry_key: &'static str,
    pub size_of_headers: u32,
    /// Virtual address of the kernel-import thunk table (array of `0x8000_0000 |
    /// ordinal` entries, terminated by 0; the kernel patches in real addresses).
    pub kernel_thunk: u32,
    pub stack_commit: u32,
    pub sections: Vec<Section>,
}

#[inline]
fn rd32(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// De-obfuscate an XOR'd address field: try each key, keep the result that lands
/// inside the image `[base, base+256MB)`; fall back to the retail key.
fn deob(enc: u32, keys: [u32; 2], base: u32) -> (u32, &'static str) {
    let names = ["retail", "debug"];
    for (i, &k) in keys.iter().enumerate() {
        let v = enc ^ k;
        if v >= base && v < base.wrapping_add(0x1000_0000) {
            return (v, names[i]);
        }
    }
    (enc ^ keys[0], "unknown")
}

/// Parse an XBE header. Returns `None` if the magic is wrong or it's malformed.
pub fn parse(xbe: &[u8]) -> Option<XbeImage> {
    if xbe.get(0..4)? != b"XBEH" {
        return None;
    }
    let base = rd32(xbe, 0x104)?;
    let size_of_headers = rd32(xbe, 0x108)?;
    let entry_enc = rd32(xbe, 0x128)?;
    let stack_commit = rd32(xbe, 0x130)?;
    let kernel_thunk_enc = rd32(xbe, 0x158)?;
    let num_sections = rd32(xbe, 0x11C)? as usize;
    let sec_hdr_addr = rd32(xbe, 0x120)?; // vaddr

    // Entry point and kernel-thunk use different XOR keys.
    let (entry, entry_key) = deob(entry_enc, [0xA8FC_57AB, 0x9485_9D4B], base);
    let (kernel_thunk, _) = deob(kernel_thunk_enc, [0x5B6D_40B6, 0xEFB1_F152], base);

    let sh_off = sec_hdr_addr.checked_sub(base)? as usize;
    let mut sections = Vec::new();
    for i in 0..num_sections.min(64) {
        let o = sh_off + i * 0x38;
        sections.push(Section {
            flags: rd32(xbe, o)?,
            vaddr: rd32(xbe, o + 4)?,
            vsize: rd32(xbe, o + 8)?,
            raw_addr: rd32(xbe, o + 0x0C)?,
            raw_size: rd32(xbe, o + 0x10)?,
        });
    }

    Some(XbeImage {
        base,
        entry,
        entry_key,
        size_of_headers,
        kernel_thunk,
        stack_commit,
        sections,
    })
}

/// Map the XBE's headers + sections into `ram` at their virtual addresses.
/// Paging is off at boot, so a virtual address equals a physical RAM offset
/// (the image base `0x0001_0000` is well inside the 64 MB RAM).
pub fn load_into(img: &XbeImage, xbe: &[u8], ram: &mut [u8]) {
    // Headers load at the image base.
    let hdr_n = (img.size_of_headers as usize).min(xbe.len());
    copy(ram, img.base, &xbe[..hdr_n]);
    // Each section's raw bytes load at its virtual address.
    for s in &img.sections {
        let start = s.raw_addr as usize;
        let end = start.saturating_add(s.raw_size as usize);
        if end <= xbe.len() {
            copy(ram, s.vaddr, &xbe[start..end]);
        }
    }
}

#[inline]
fn copy(ram: &mut [u8], vaddr: u32, data: &[u8]) {
    let off = vaddr as usize;
    if off.checked_add(data.len()).map_or(false, |e| e <= ram.len()) {
        ram[off..off + data.len()].copy_from_slice(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_xbe() {
        let mut x = vec![0u8; 0x400];
        x[0..4].copy_from_slice(b"XBEH");
        let base = 0x0001_0000u32;
        x[0x104..0x108].copy_from_slice(&base.to_le_bytes());
        x[0x108..0x10C].copy_from_slice(&0x200u32.to_le_bytes()); // size of headers
        x[0x11C..0x120].copy_from_slice(&1u32.to_le_bytes()); // 1 section
        x[0x120..0x124].copy_from_slice(&(base + 0x180).to_le_bytes()); // sec hdr vaddr
        let entry = 0x0001_2000u32 ^ 0xA8FC_57AB;
        x[0x128..0x12C].copy_from_slice(&entry.to_le_bytes());
        let thunk = (base + 0x100) ^ 0x5B6D_40B6;
        x[0x158..0x15C].copy_from_slice(&thunk.to_le_bytes());
        // one section header at file offset 0x180 (vaddr base+0x180)
        let o = 0x180;
        x[o + 4..o + 8].copy_from_slice(&(base + 0x1000).to_le_bytes()); // vaddr
        x[o + 8..o + 12].copy_from_slice(&0x10u32.to_le_bytes()); // vsize
        x[o + 0x0C..o + 0x10].copy_from_slice(&0x000u32.to_le_bytes()); // raw addr
        x[o + 0x10..o + 0x14].copy_from_slice(&0x10u32.to_le_bytes()); // raw size

        let img = parse(&x).expect("parse");
        assert_eq!(img.base, 0x0001_0000);
        assert_eq!(img.entry, 0x0001_2000);
        assert_eq!(img.entry_key, "retail");
        assert_eq!(img.kernel_thunk, base + 0x100);
        assert_eq!(img.sections.len(), 1);
        assert_eq!(img.sections[0].vaddr, base + 0x1000);
    }
}
