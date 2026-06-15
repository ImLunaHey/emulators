//! HLE disc boot — parse the ISO9660 filesystem to find the game's boot
//! executable. Used as a fallback when the BIOS shell doesn't auto-boot the
//! disc: the orchestrator loads the returned PS-X EXE into RAM and jumps to it,
//! while the (already initialised) BIOS kernel services the game's A/B/C calls.

use crate::cdrom::Cdrom;

const SECTOR: usize = 2048;

/// Find and read the game's boot executable: PVD → root directory →
/// `SYSTEM.CNF` → its `BOOT = cdrom:\NAME` → the named EXE file. Returns the raw
/// PS-X EXE bytes, or `None` if the disc isn't a valid bootable ISO9660 PSX disc.
pub fn find_boot_exe(cd: &Cdrom) -> Option<Vec<u8>> {
    // Primary Volume Descriptor at LBA 16: "CD001" magic + the root directory
    // record at byte 156 (extent LBA at +2 LE, data length at +10 LE).
    let pvd = cd.sector_user_data(16)?;
    if pvd.get(1..6)? != b"CD001" {
        return None;
    }
    let root_lba = rd_u32(pvd, 158)?;
    let root_size = rd_u32(pvd, 166)?;

    // SYSTEM.CNF → the boot file name.
    let (cnf_lba, cnf_size) = find_in_dir(cd, root_lba, root_size, "SYSTEM.CNF")?;
    let cnf = read_file(cd, cnf_lba, cnf_size)?;
    let name = parse_boot_name(&cnf)?;

    // The boot EXE itself.
    let (exe_lba, exe_size) = find_in_dir(cd, root_lba, root_size, &name)?;
    read_file(cd, exe_lba, exe_size)
}

/// Read a little-endian u32 at `off` from `buf`.
fn rd_u32(buf: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?))
}

/// Normalise an ISO9660 file identifier for comparison: uppercase, and drop the
/// `;1` version suffix (the disc stores `SYSTEM.CNF;1`; `BOOT=` may omit it).
fn norm(name: &str) -> String {
    name.split(';').next().unwrap_or(name).to_ascii_uppercase()
}

/// Walk the directory at (`lba`,`size`) and return the (lba,size) of `target`.
fn find_in_dir(cd: &Cdrom, lba: u32, size: u32, target: &str) -> Option<(u32, u32)> {
    let want = norm(target);
    let sectors = size.div_ceil(SECTOR as u32);
    for s in 0..sectors {
        let data = cd.sector_user_data(lba + s)?;
        let mut off = 0usize;
        while off + 33 < data.len() {
            let rlen = data[off] as usize;
            if rlen == 0 {
                break; // no more records in this sector; padding to the boundary
            }
            let namelen = data[off + 32] as usize;
            if let Some(raw) = data.get(off + 33..off + 33 + namelen) {
                if let Ok(name) = std::str::from_utf8(raw) {
                    if norm(name) == want {
                        let e_lba = rd_u32(data, off + 2)?;
                        let e_size = rd_u32(data, off + 10)?;
                        return Some((e_lba, e_size));
                    }
                }
            }
            off += rlen;
        }
    }
    None
}

/// Read `size` bytes of a file starting at `lba` (sector by sector).
fn read_file(cd: &Cdrom, lba: u32, size: u32) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(size as usize);
    let sectors = size.div_ceil(SECTOR as u32);
    for s in 0..sectors {
        out.extend_from_slice(cd.sector_user_data(lba + s)?);
    }
    out.truncate(size as usize);
    Some(out)
}

/// Parse the `BOOT = cdrom:\NAME;1` line of SYSTEM.CNF, returning `NAME;1`.
fn parse_boot_name(cnf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(cnf);
    for line in text.lines() {
        let line = line.trim();
        let mut parts = line.splitn(2, '=');
        if parts.next()?.trim().eq_ignore_ascii_case("BOOT") {
            let val = parts.next()?.trim();
            // Strip a leading "cdrom:" / "cdrom:\" / "\" device prefix.
            let path = val.rsplit([':', '\\']).next()?.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}
