// 32 KB battery-backed SRAM. No state machine, no command sequencing —
// every read returns the stored byte, every write stores one. Used by
// older AGB titles (Mario Kart Super Circuit, Final Fantasy 4/5/6
// Advance, F-Zero Maximum Velocity, lots of homebrew).
//
// On real hardware SRAM is wired 8-bit only; reads through halfword
// and word accesses get the byte broadcast across the wider field.
// That mirror is implemented at the Bus layer (read16/read32 on the
// SRAM region return `(b | b << 8) & 0xFFFF` etc.), so this class
// only sees byte-granular addresses.
pub struct Sram32 {
    pub data: [u8; 0x8000],
    // TS exposed `onChange: (() => void) | null` — a dirty callback fired on
    // every write. Modeled here as a plain dirty flag the owner can poll/clear.
    pub dirty: bool,
}

impl Default for Sram32 {
    fn default() -> Self {
        Self {
            data: [0; 0x8000],
            dirty: false,
        }
    }
}

impl Sram32 {
    pub fn new() -> Self {
        Self::default()
    }
}

impl crate::Save for Sram32 {
    fn load_save(&mut self, bytes: &[u8]) {
        self.data.fill(0xFF);
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
    }

    fn read(&mut self, addr: u32) -> u32 {
        self.data[(addr & 0x7FFF) as usize] as u32
    }

    fn write(&mut self, addr: u32, v: u32) {
        self.data[(addr & 0x7FFF) as usize] = (v & 0xFF) as u8;
        self.dirty = true;
    }

    fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Save;

    #[test]
    fn read_write_roundtrip_and_byte_mask() {
        let mut s = Sram32::new();
        s.write(0x0000, 0x1234);
        assert_eq!(s.read(0x0000), 0x34); // only low byte stored
        assert!(s.dirty);
    }

    #[test]
    fn address_masking() {
        let mut s = Sram32::new();
        s.write(0x8000, 0xAB); // wraps to 0x0000
        assert_eq!(s.read(0x0000), 0xAB);
        assert_eq!(s.read(0x8000), 0xAB);
    }

    #[test]
    fn load_save_fills_ff_and_truncates() {
        let mut s = Sram32::new();
        s.load_save(&[1, 2, 3]);
        assert_eq!(s.read(0), 1);
        assert_eq!(s.read(2), 3);
        assert_eq!(s.read(3), 0xFF); // remainder filled with 0xFF
    }
}
