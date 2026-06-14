//! VRAM bank router. Ported from ../../ds-recomp/src/memory/vram_router.ts.
//!
//! Each of the 9 NDS VRAM banks (A..I) has a VRAMCNT_x register controlling
//! whether it's enabled, what kind of mapping it has (MST), and which "slot"
//! of that mapping it occupies (OFFSET). This translates an ARM9 or ARM7
//! VRAM-window address into a flat offset into `SharedMemory::vram`, or
//! `None` if no bank covers it.
//!
//! `vram[]` layout (matches VRAM_TOTAL_SIZE = 656 KB):
//!   bank A: vram[0x00000..0x1FFFF]   128 KB
//!   bank B: vram[0x20000..0x3FFFF]   128 KB
//!   bank C: vram[0x40000..0x5FFFF]   128 KB
//!   bank D: vram[0x60000..0x7FFFF]   128 KB
//!   bank E: vram[0x80000..0x8FFFF]    64 KB
//!   bank F: vram[0x90000..0x93FFF]    16 KB
//!   bank G: vram[0x94000..0x97FFF]    16 KB
//!   bank H: vram[0x98000..0x9FFFF]    32 KB
//!   bank I: vram[0xA0000..0xA3FFF]    16 KB
//!
//! Ownership: unlike the TS class, this struct does NOT hold a reference to
//! the VRAMCNT array. The PPU owns `vramcnt: [u8; 9]` (its registers), and
//! every resolve method takes `vramcnt: &[u8; 9]` as a parameter — matching
//! the GBA-core pattern of passing collaborators as `&mut`/`&` arguments
//! rather than storing them.

struct BankInfo {
    start: u32,
    size: u32,
}

const BANK_INFO: [BankInfo; 9] = [
    BankInfo { start: 0x00000, size: 0x20000 }, // A
    BankInfo { start: 0x20000, size: 0x20000 }, // B
    BankInfo { start: 0x40000, size: 0x20000 }, // C
    BankInfo { start: 0x60000, size: 0x20000 }, // D
    BankInfo { start: 0x80000, size: 0x10000 }, // E
    BankInfo { start: 0x90000, size: 0x04000 }, // F
    BankInfo { start: 0x94000, size: 0x04000 }, // G
    BankInfo { start: 0x98000, size: 0x08000 }, // H
    BankInfo { start: 0xA0000, size: 0x04000 }, // I
];

/// Fixed LCDC alias addresses for each bank (A..I).
const LCDC_BASE: [u32; 9] = [
    0x0680_0000, 0x0682_0000, 0x0684_0000, 0x0686_0000, // A, B, C, D
    0x0688_0000, 0x0689_0000, 0x0689_4000, 0x0689_8000, // E, F, G, H
    0x068A_0000, // I
];

/// VRAM bank router. Stateless w.r.t. the VRAMCNT registers — those are
/// passed in by the PPU on each call.
#[derive(Default)]
pub struct VramRouter;

impl VramRouter {
    pub fn new() -> Self {
        VramRouter
    }

    /// ARM9 view of VRAM. Walks the 9 banks and returns the flat `vram[]`
    /// byte index of the first bank that covers `addr`, or `None`.
    pub fn resolve_arm9(&self, addr: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        // LCDC alias range: fixed addresses regardless of MST.
        if (0x0680_0000..0x0680_0000 + 0xA4000).contains(&addr) {
            for i in 0..9 {
                let cnt = vramcnt[i];
                if (cnt & 0x80) == 0 {
                    continue;
                }
                let mst = cnt & 0x7;
                if mst != 0 {
                    continue; // only LCDC mode appears here
                }
                let base = LCDC_BASE[i];
                let info = &BANK_INFO[i];
                if addr >= base && addr < base + info.size {
                    return Some((info.start + (addr - base)) as usize);
                }
            }
            return None;
        }
        // BG window 0x06000000..0x0607FFFF (main engine A BG VRAM).
        if (0x0600_0000..0x0608_0000).contains(&addr) {
            for i in 0..9 {
                let cnt = vramcnt[i];
                if (cnt & 0x80) == 0 {
                    continue;
                }
                let mst = cnt & 0x7;
                // Engine A BG mode: A/B/C/D mst=1; E mst=1; F/G mst=1.
                if i <= 3 && mst == 1 {
                    let ofs = ((cnt >> 3) & 0x3) as u32;
                    let base = 0x0600_0000 + ofs * 0x20000;
                    let info = &BANK_INFO[i];
                    if addr >= base && addr < base + info.size {
                        return Some((info.start + (addr - base)) as usize);
                    }
                }
                if i == 4 && mst == 1 {
                    // Bank E to engine A BG mode is at 0x06000000 (64 KB).
                    if (0x0600_0000..0x0601_0000).contains(&addr) {
                        return Some((BANK_INFO[4].start + (addr - 0x0600_0000)) as usize);
                    }
                }
                if (i == 5 || i == 6) && mst == 1 {
                    // F/G to engine A BG. OFS picks slot.
                    let ofs = ((cnt >> 3) & 0x3) as usize;
                    let slot = [0x0000u32, 0x4000, 0x10000, 0x14000][ofs];
                    let base = 0x0600_0000 + slot;
                    if addr >= base && addr < base + 0x4000 {
                        return Some((BANK_INFO[i].start + (addr - base)) as usize);
                    }
                }
            }
            return None;
        }
        // OBJ window 0x06400000..0x0643FFFF (main engine A OBJ VRAM).
        if (0x0640_0000..0x0644_0000).contains(&addr) {
            for i in 0..9 {
                let cnt = vramcnt[i];
                if (cnt & 0x80) == 0 {
                    continue;
                }
                let mst = cnt & 0x7;
                if i <= 1 && mst == 2 {
                    let ofs = ((cnt >> 3) & 0x1) as u32;
                    let base = 0x0640_0000 + ofs * 0x20000;
                    let info = &BANK_INFO[i];
                    if addr >= base && addr < base + info.size {
                        return Some((info.start + (addr - base)) as usize);
                    }
                }
                if i == 4 && mst == 2 {
                    if (0x0640_0000..0x0641_0000).contains(&addr) {
                        return Some((BANK_INFO[4].start + (addr - 0x0640_0000)) as usize);
                    }
                }
                if (i == 5 || i == 6) && mst == 2 {
                    let ofs = ((cnt >> 3) & 0x3) as usize;
                    let slot = [0x0000u32, 0x4000, 0x10000, 0x14000][ofs];
                    let base = 0x0640_0000 + slot;
                    if addr >= base && addr < base + 0x4000 {
                        return Some((BANK_INFO[i].start + (addr - base)) as usize);
                    }
                }
            }
            return None;
        }
        // Sub-BG window 0x06200000..0x0627FFFF (engine B BG, 512 KB).
        // Banks that can map here: C MST=4 (128 KB), D MST=4 (128 KB),
        // H MST=1 (32 KB at start), I MST=1 (16 KB at 0x06208000).
        if (0x0620_0000..0x0628_0000).contains(&addr) {
            if (vramcnt[2] & 0x87) == 0x84 && addr < 0x0622_0000 {
                return Some((BANK_INFO[2].start + (addr - 0x0620_0000)) as usize);
            }
            if (vramcnt[3] & 0x87) == 0x84 && addr < 0x0622_0000 {
                return Some((BANK_INFO[3].start + (addr - 0x0620_0000)) as usize);
            }
            if (vramcnt[7] & 0x87) == 0x81 && addr < 0x0620_8000 {
                return Some((BANK_INFO[7].start + (addr - 0x0620_0000)) as usize);
            }
            if (vramcnt[8] & 0x87) == 0x81 && (0x0620_8000..0x0620_C000).contains(&addr) {
                return Some((BANK_INFO[8].start + (addr - 0x0620_8000)) as usize);
            }
            return None;
        }
        // Sub-OBJ window 0x06600000..0x0667FFFF (engine B OBJ, 128 KB).
        // Per GBATEK §"VRAM Banks":
        //   Bank D MST=4 → Engine B OBJ (128 KB at 0x06600000)
        //   Bank I MST=2 → Engine B OBJ (16 KB at 0x06600000)
        if (0x0660_0000..0x0668_0000).contains(&addr) {
            if (vramcnt[3] & 0x87) == 0x84 && addr < 0x0662_0000 {
                return Some((BANK_INFO[3].start + (addr - 0x0660_0000)) as usize);
            }
            if (vramcnt[8] & 0x87) == 0x82 && addr < 0x0660_4000 {
                return Some((BANK_INFO[8].start + (addr - 0x0660_0000)) as usize);
            }
            return None;
        }
        None
    }

    /// ARM7 view of VRAM. Only banks C and D, when MST=2, are reachable from
    /// ARM7 (at 0x06000000..0x0603FFFF depending on OFS).
    pub fn resolve_arm7(&self, addr: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if !(0x0600_0000..0x0604_0000).contains(&addr) {
            return None;
        }
        for i in [2usize, 3] {
            let cnt = vramcnt[i];
            if (cnt & 0x87) != 0x82 {
                continue; // enabled + MST=2
            }
            let ofs = ((cnt >> 3) & 0x1) as u32;
            let base = 0x0600_0000 + ofs * 0x20000;
            if addr >= base && addr < base + 0x20000 {
                return Some((BANK_INFO[i].start + (addr - base)) as usize);
            }
        }
        None
    }

    /// VRAMSTAT (ARM7 view of 0x04000240): bit 0 = bank C allocated to ARM7,
    /// bit 1 = bank D allocated to ARM7.
    pub fn read_vram_stat(&self, vramcnt: &[u8; 9]) -> u32 {
        let mut v = 0;
        if (vramcnt[2] & 0x87) == 0x82 {
            v |= 0x01;
        }
        if (vramcnt[3] & 0x87) == 0x82 {
            v |= 0x02;
        }
        v
    }

    // ─── Extended palette resolution ──────────────────────────────────────
    //
    // The renderer consults these directly (they aren't true VRAM-window
    // aliases). Each returns the flat `vram[]` byte index, or `None`.

    /// Engine A BG ext palette. `slot ∈ [0,4)`, `off ∈ [0,0x2000)`.
    pub fn resolve_bg_ext_pal_a(&self, slot: u32, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        // Bank E MST=4 covers all 4 slots (32 KB).
        if (vramcnt[4] & 0x87) == 0x84 {
            return Some((BANK_INFO[4].start + slot * 0x2000 + off) as usize);
        }
        // Banks F and G with MST=4 each contribute a single 8 KB slot.
        // OFFSET bit 0 picks the pair, OFFSET bit 1 picks within the pair.
        for i in [5usize, 6] {
            if (vramcnt[i] & 0x87) != 0x84 {
                continue;
            }
            let ofs_field = ((vramcnt[i] >> 3) & 0x3) as u32;
            let mapped_slot = (ofs_field & 1) * 2 + ((ofs_field >> 1) & 1);
            if mapped_slot == slot {
                return Some((BANK_INFO[i].start + off) as usize);
            }
        }
        None
    }

    /// Engine B BG ext palette. Bank H MST=2 supplies all 4 slots (32 KB).
    pub fn resolve_bg_ext_pal_b(&self, slot: u32, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if (vramcnt[7] & 0x87) == 0x82 {
            return Some((BANK_INFO[7].start + slot * 0x2000 + off) as usize);
        }
        None
    }

    /// Engine A OBJ ext palette. F (MST=5) wins over G if both are mapped.
    pub fn resolve_obj_ext_pal_a(&self, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if (vramcnt[5] & 0x87) == 0x85 {
            return Some((BANK_INFO[5].start + off) as usize);
        }
        if (vramcnt[6] & 0x87) == 0x85 {
            return Some((BANK_INFO[6].start + off) as usize);
        }
        None
    }

    /// Engine B OBJ ext palette. Bank I MST=3 (8 KB).
    pub fn resolve_obj_ext_pal_b(&self, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if (vramcnt[8] & 0x87) == 0x83 {
            return Some((BANK_INFO[8].start + off) as usize);
        }
        None
    }

    // ─── 3D texture resolution ────────────────────────────────────────────

    /// Texture image space offset (0..0x7FFFF) → flat `vram[]` index.
    pub fn resolve_tex_image(&self, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if off >= 0x80000 {
            return None;
        }
        let slot = (off >> 17) & 0x3; // which 128 KB slot
        let within = off & 0x1FFFF;
        for i in 0..4 {
            // banks A..D
            if (vramcnt[i] & 0x87) != 0x83 {
                continue; // enabled + MST=3
            }
            let ofs = ((vramcnt[i] >> 3) & 0x3) as u32;
            if ofs == slot {
                return Some((BANK_INFO[i].start + within) as usize);
            }
        }
        None
    }

    /// Texture palette space offset (0..0x1FFFF) → flat `vram[]` index.
    pub fn resolve_tex_palette(&self, off: u32, vramcnt: &[u8; 9]) -> Option<usize> {
        if off >= 0x20000 {
            return None;
        }
        // Bank E MST=3 supplies the first 64 KB (slots 0..3).
        if (vramcnt[4] & 0x87) == 0x83 && off < 0x10000 {
            return Some((BANK_INFO[4].start + off) as usize);
        }
        // Banks F/G MST=3 each supply one 16 KB slot.
        let slot = (off >> 14) & 0x7;
        let within = off & 0x3FFF;
        for i in [5usize, 6] {
            if (vramcnt[i] & 0x87) != 0x83 {
                continue;
            }
            let ofs_field = ((vramcnt[i] >> 3) & 0x3) as u32;
            let mapped_slot = (ofs_field & 1) + ((ofs_field >> 1) & 1) * 4;
            if mapped_slot == slot {
                return Some((BANK_INFO[i].start + within) as usize);
            }
        }
        None
    }
}
