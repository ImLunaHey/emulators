//! Whole-emulator state snapshot. Ported 1:1 from src/savestate.ts.
//!
//! Captures everything we need to resume execution from this exact byte, on
//! this exact CPU instruction, with this exact PPU scanline progress and IO
//! state.
//!
//! Format: magic + version + size-prefixed sections. Sections are
//! independent so older blobs can grow new fields at the end with a version
//! bump and the loader skips unknown ones.
//!
//! What's NOT captured:
//!   - Audio output queue (Web Audio's playback ring) — gets re-filled on the
//!     next run_frame; a one-second audio gap on resume is fine.
//!   - Recompiler caches — the Rust core ships a pure interpreter, so there
//!     are none to rebuild.
//!   - Link-cable transport — savestates are per-emulator; resume drops back
//!     to LocalLoopback and the user re-connects.
//!
//! Cheats are not captured here; they're stored separately in the UI layer
//! and re-applied per-frame regardless.
//!
//! Blob layout is byte-compatible with the TypeScript core (same MAGIC,
//! VERSION, TAG values, section order and per-section field order/encoding).

const MAGIC: u32 = 0x4742_4153; // 'GBAS' (little-endian)
const VERSION: u32 = 1;

// Section tags — each is a 4-byte LE u32 followed by a 4-byte LE u32 length,
// then `length` bytes of payload. Unknown tags are skipped.
mod tag {
    pub const CPU: u32 = 0x0143_5055; // 'UPC\x01'
    pub const IWRAM: u32 = 0x4d57_4901; // 'IWM\x01'
    pub const EWRAM: u32 = 0x4d57_4501; // 'EWM\x01'
    pub const VRAM: u32 = 0x4d52_5601; // 'VRM\x01'
    pub const PRAM: u32 = 0x4d52_5001; // 'PRM\x01'
    pub const OAM: u32 = 0x4d41_4f01; // 'OAM\x01'
    pub const IO: u32 = 0x4f49_5001; // 'PIO\x01'
    pub const PPU: u32 = 0x5550_5001; // 'PPU\x01'
    pub const TIMERS: u32 = 0x4d49_5401; // 'TIM\x01'
    pub const DMA: u32 = 0x414d_4401; // 'DMA\x01'
    pub const IRQ: u32 = 0x5152_4901; // 'IRQ\x01'
    pub const SIO: u32 = 0x4f49_5301; // 'SIO\x01'
    pub const SAVE: u32 = 0x5641_5301; // 'SAV\x01' — Flash/SRAM/EEPROM chip data
    #[allow(dead_code)]
    pub const CYCLES: u32 = 0x4c43_5901; // 'YCL\x01'
}

/// Builds a `Vec<u8>` of little-endian u32s and raw byte regions, with
/// length-prefixed tagged sections. Mirrors the TS `Writer`.
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { out: Vec::new() }
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }
    /// Write a tagged section: tag, payload length, payload (built by `body`).
    fn section(&mut self, tag: u32, body: impl FnOnce(&mut Writer)) {
        let mut inner = Writer::new();
        body(&mut inner);
        let payload = inner.out;
        self.u32(tag);
        self.u32(payload.len() as u32);
        self.bytes(&payload);
    }
    fn finish(self) -> Vec<u8> {
        self.out
    }
}

/// Cursor over a `&[u8]` reading little-endian u32s + raw byte regions.
/// Mirrors the TS `Reader`. Reads are bounds-checked; out-of-range u32 reads
/// return 0 and byte reads clamp, matching the permissive `subarray` of TS.
struct Reader<'a> {
    buf: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, p: 0 }
    }
    fn u32(&mut self) -> u32 {
        if self.p + 4 > self.buf.len() {
            self.p = self.buf.len();
            return 0;
        }
        let v = u32::from_le_bytes([
            self.buf[self.p],
            self.buf[self.p + 1],
            self.buf[self.p + 2],
            self.buf[self.p + 3],
        ]);
        self.p += 4;
        v
    }
    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let end = (self.p + n).min(self.buf.len());
        let out = &self.buf[self.p..end];
        self.p = end;
        out
    }
    fn eof(&self) -> bool {
        self.p >= self.buf.len()
    }
}

/// Copy `src` into `dst` if the lengths match, else copy what fits. The TS
/// `TypedArray.set` would throw on overflow; we clamp to the region size to
/// stay panic-free on a malformed blob, restoring as much as fits.
fn restore_region(dst: &mut [u8], src: &[u8]) {
    let n = dst.len().min(src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

impl crate::emulator::Gba {
    pub fn save_state(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(MAGIC);
        w.u32(VERSION);

        // CPU registers + banks + CPSR + halt state.
        w.section(tag::CPU, |s| {
            let c = &self.cpu;
            for i in 0..16 {
                s.u32(c.state.r[i]);
            }
            for i in 0..6 {
                s.u32(c.state.bank_r13[i]);
            }
            for i in 0..6 {
                s.u32(c.state.bank_r14[i]);
            }
            for i in 0..6 {
                s.u32(c.state.bank_spsr[i]);
            }
            for i in 0..5 {
                s.u32(c.state.fiq_r8_12[i]);
            }
            for i in 0..5 {
                s.u32(c.state.usr_r8_12[i]);
            }
            s.u32(c.state.usr_r13);
            s.u32(c.state.usr_r14);
            s.u32(c.state.cpsr);
            s.u32(if c.state.halted { 1 } else { 0 });
            s.u32(c.cycles as u32);
            s.u32(if c.irq_line { 1 } else { 0 });
        });

        // Memory regions — straight byte copies. EWRAM is the biggest at
        // 256 KB; everything else is much smaller.
        w.section(tag::IWRAM, |s| s.bytes(&self.mem.iwram));
        w.section(tag::EWRAM, |s| s.bytes(&self.mem.ewram));
        w.section(tag::VRAM, |s| s.bytes(&self.mem.vram));
        w.section(tag::PRAM, |s| s.bytes(&self.mem.pram));
        w.section(tag::OAM, |s| s.bytes(&self.mem.oam));
        w.section(tag::IO, |s| s.bytes(&self.io_raw));

        // PPU — registers + the rolling scanline cycle counter so we resume
        // mid-frame on the same scanline.
        w.section(tag::PPU, |s| {
            let p = &self.ppu;
            s.u32(p.dispcnt);
            s.u32(p.dispstat);
            s.u32(p.vcount);
            for i in 0..4 {
                s.u32(p.bgcnt[i]);
            }
            for i in 0..4 {
                s.u32(p.bg_hofs[i]);
            }
            for i in 0..4 {
                s.u32(p.bg_vofs[i]);
            }
            for i in 0..2 {
                s.u32(p.bg_x[i] as u32);
            }
            for i in 0..2 {
                s.u32(p.bg_y[i] as u32);
            }
            for i in 0..2 {
                s.u32((p.bg_pa[i] & 0xFFFF) as u32);
            }
            for i in 0..2 {
                s.u32((p.bg_pb[i] & 0xFFFF) as u32);
            }
            for i in 0..2 {
                s.u32((p.bg_pc[i] & 0xFFFF) as u32);
            }
            for i in 0..2 {
                s.u32((p.bg_pd[i] & 0xFFFF) as u32);
            }
            s.u32(p.win0_h);
            s.u32(p.win1_h);
            s.u32(p.win0_v);
            s.u32(p.win1_v);
            s.u32(p.win_in);
            s.u32(p.win_out);
            s.u32(p.mosaic);
            s.u32(p.bldcnt);
            s.u32(p.bldalpha);
            s.u32(p.bldy);
            s.u32(p.cycles_accum);
            s.u32(if p.in_hblank { 1 } else { 0 });
            s.u32(p.frame_count);
        });

        // Timers — counter/reload/control + sub_cycles for each channel.
        w.section(tag::TIMERS, |s| {
            for ch in &self.timers.ch {
                s.u32(ch.reload);
                s.u32(ch.counter);
                s.u32(ch.control);
                s.u32(ch.sub_cycles);
                s.u32(if ch.enabled { 1 } else { 0 });
                s.u32(if ch.count_up { 1 } else { 0 });
                s.u32(if ch.irq_enable { 1 } else { 0 });
                s.u32(ch.prescale);
            }
        });

        // DMA — 4 channels of src/dst/count/control plus the internal
        // counters that track in-flight transfers.
        w.section(tag::DMA, |s| {
            for ch in &self.dma.ch {
                s.u32(ch.src);
                s.u32(ch.dst);
                s.u32(ch.count);
                s.u32(ch.control);
                // Internal book-keeping fields the bus needs to resume an
                // in-flight DMA. Same names as in dma.rs.
                s.u32(ch.internal_src);
                s.u32(ch.internal_dst);
                s.u32(ch.internal_count);
            }
        });

        w.section(tag::IRQ, |s| {
            s.u32(self.irq.ie);
            s.u32(self.irq.iflag);
            s.u32(self.irq.ime);
        });

        // SIO — register file + transfer_seq so resume re-syncs with peer.
        // The transport pointer itself is not serialized; we always resume
        // with LocalLoopback so the user must explicitly reconnect.
        w.section(tag::SIO, |s| {
            let sio = &self.sio;
            for i in 0..4 {
                s.u32(sio.multi[i] as u32);
            }
            s.u32(sio.siocnt);
            s.u32(sio.mlt_send);
            s.u32(sio.rcnt);
            s.u32(sio.joycnt);
            s.u32(sio.joy_recv);
            s.u32(sio.joy_trans);
            s.u32(sio.joystat);
            s.u32(sio.transfer_seq);
        });

        // Save chip data — Flash/SRAM/EEPROM all share the save surface; we
        // capture the raw byte buffer so the game's save memory survives the
        // round-trip.
        w.section(tag::SAVE, |s| {
            let data = self.save_ram();
            s.u32(data.len() as u32);
            s.bytes(data);
        });

        w.finish()
    }

    pub fn load_state(&mut self, blob: &[u8]) -> Result<(), String> {
        let mut r = Reader::new(blob);
        let magic = r.u32();
        if magic != MAGIC {
            return Err(format!("bad magic 0x{:x}", magic));
        }
        let version = r.u32();
        if version > VERSION {
            return Err(format!("unsupported version {}", version));
        }

        while !r.eof() {
            let tag = r.u32();
            let len = r.u32() as usize;
            let body = r.bytes(len).to_vec();
            self.apply_section(tag, &body);
        }

        // Defensive cleanup after restore: clear any in-flight pipeline
        // prefetch so the CPU re-fetches from r[15]. (No recompiler to
        // invalidate in the Rust interpreter core.)
        self.cpu.clear_prefetch();
        // PPU should re-render fresh — clear any half-rendered scanline.
        self.ppu.frame_done = false;

        Ok(())
    }

    fn apply_section(&mut self, tag: u32, body: &[u8]) {
        let mut r = Reader::new(body);
        match tag {
            tag::CPU => {
                let c = &mut self.cpu;
                for i in 0..16 {
                    c.state.r[i] = r.u32();
                }
                for i in 0..6 {
                    c.state.bank_r13[i] = r.u32();
                }
                for i in 0..6 {
                    c.state.bank_r14[i] = r.u32();
                }
                for i in 0..6 {
                    c.state.bank_spsr[i] = r.u32();
                }
                for i in 0..5 {
                    c.state.fiq_r8_12[i] = r.u32();
                }
                for i in 0..5 {
                    c.state.usr_r8_12[i] = r.u32();
                }
                c.state.usr_r13 = r.u32();
                c.state.usr_r14 = r.u32();
                c.state.cpsr = r.u32();
                c.state.halted = r.u32() != 0;
                c.cycles = r.u32() as u64;
                c.irq_line = r.u32() != 0;
            }
            tag::IWRAM => restore_region(&mut self.mem.iwram, body),
            tag::EWRAM => restore_region(&mut self.mem.ewram, body),
            tag::VRAM => restore_region(&mut self.mem.vram, body),
            tag::PRAM => restore_region(&mut self.mem.pram, body),
            tag::OAM => restore_region(&mut self.mem.oam, body),
            tag::IO => restore_region(&mut self.io_raw, body),
            tag::PPU => {
                let p = &mut self.ppu;
                p.dispcnt = r.u32();
                p.dispstat = r.u32();
                p.vcount = r.u32();
                for i in 0..4 {
                    p.bgcnt[i] = r.u32();
                }
                for i in 0..4 {
                    p.bg_hofs[i] = r.u32();
                }
                for i in 0..4 {
                    p.bg_vofs[i] = r.u32();
                }
                for i in 0..2 {
                    p.bg_x[i] = r.u32() as i32;
                }
                for i in 0..2 {
                    p.bg_y[i] = r.u32() as i32;
                }
                for i in 0..2 {
                    p.bg_pa[i] = ((r.u32() as u16) as i16) as i32;
                }
                for i in 0..2 {
                    p.bg_pb[i] = ((r.u32() as u16) as i16) as i32;
                }
                for i in 0..2 {
                    p.bg_pc[i] = ((r.u32() as u16) as i16) as i32;
                }
                for i in 0..2 {
                    p.bg_pd[i] = ((r.u32() as u16) as i16) as i32;
                }
                p.win0_h = r.u32();
                p.win1_h = r.u32();
                p.win0_v = r.u32();
                p.win1_v = r.u32();
                p.win_in = r.u32();
                p.win_out = r.u32();
                p.mosaic = r.u32();
                p.bldcnt = r.u32();
                p.bldalpha = r.u32();
                p.bldy = r.u32();
                p.cycles_accum = r.u32();
                p.in_hblank = r.u32() != 0;
                p.frame_count = r.u32();
            }
            tag::TIMERS => {
                for ch in &mut self.timers.ch {
                    ch.reload = r.u32();
                    ch.counter = r.u32();
                    ch.control = r.u32();
                    ch.sub_cycles = r.u32();
                    ch.enabled = r.u32() != 0;
                    ch.count_up = r.u32() != 0;
                    ch.irq_enable = r.u32() != 0;
                    ch.prescale = r.u32();
                }
            }
            tag::DMA => {
                for ch in &mut self.dma.ch {
                    ch.src = r.u32();
                    ch.dst = r.u32();
                    ch.count = r.u32();
                    ch.control = r.u32();
                    ch.internal_src = r.u32();
                    ch.internal_dst = r.u32();
                    ch.internal_count = r.u32();
                }
            }
            tag::IRQ => {
                self.irq.ie = r.u32();
                self.irq.iflag = r.u32();
                self.irq.ime = r.u32();
                // Recompute cached_pending so the CPU hot loop sees the
                // restored IRQ state immediately.
                self.irq.cached_pending =
                    (self.irq.ime & 1) != 0 && (self.irq.ie & self.irq.iflag) != 0;
            }
            tag::SIO => {
                let sio = &mut self.sio;
                for i in 0..4 {
                    sio.multi[i] = r.u32() as u16;
                }
                sio.siocnt = r.u32();
                sio.mlt_send = r.u32();
                sio.rcnt = r.u32();
                sio.joycnt = r.u32();
                sio.joy_recv = r.u32();
                sio.joy_trans = r.u32();
                sio.joystat = r.u32();
                sio.transfer_seq = r.u32();
            }
            tag::SAVE => {
                let len = r.u32() as usize;
                let data = r.bytes(len).to_vec();
                self.load_save_ram(&data);
            }
            _ => {
                // Forward-compat: skip unknown sections silently.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::emulator::Gba;

    #[test]
    fn round_trip() {
        let mut gba = Gba::new();
        // Mutate some representative state across subsystems.
        gba.cpu.state.r[0] = 0xDEAD_BEEF;
        gba.cpu.state.r[15] = 0x0800_0000;
        gba.cpu.state.cpsr = 0x1F;
        gba.cpu.state.halted = true;
        gba.cpu.cycles = 123_456;
        gba.cpu.irq_line = true;
        gba.mem.iwram[0x100] = 0xAB;
        gba.mem.ewram[0x2000] = 0xCD;
        gba.mem.vram[0x10] = 0xEF;
        gba.mem.pram[0x4] = 0x12;
        gba.mem.oam[0x8] = 0x34;
        gba.io_raw[0x130] = 0x56;
        gba.ppu.dispcnt = 0x0123;
        gba.ppu.bg_x[0] = -42;
        gba.ppu.bg_y[1] = 0x07FF_FFFF;
        gba.ppu.bg_pa[0] = -1;
        gba.ppu.bg_pb[1] = 0x1234;
        gba.ppu.cycles_accum = 700;
        gba.ppu.in_hblank = true;
        gba.ppu.frame_count = 9;
        gba.timers.ch[2].reload = 0x8000;
        gba.timers.ch[2].counter = 0x1234;
        gba.timers.ch[2].enabled = true;
        gba.timers.ch[2].prescale = 64;
        gba.dma.ch[1].src = 0x0800_1000;
        gba.dma.ch[1].internal_count = 7;
        gba.irq.ie = 0x3FFF;
        gba.irq.iflag = 0x0001;
        gba.irq.ime = 1;
        gba.sio.multi[3] = 0xBEEF;
        gba.sio.siocnt = 0x4003;
        gba.sio.transfer_seq = 42;

        let snap1 = gba.save_state();

        // Scribble over the state so we can prove load restores it.
        gba.cpu.state.r[0] = 0;
        gba.cpu.state.halted = false;
        gba.cpu.cycles = 0;
        gba.mem.iwram[0x100] = 0;
        gba.ppu.bg_x[0] = 0;
        gba.ppu.bg_pa[0] = 0;
        gba.timers.ch[2].reload = 0;
        gba.dma.ch[1].src = 0;
        gba.irq.ie = 0;
        gba.sio.multi[3] = 0;

        gba.load_state(&snap1).expect("load_state failed");

        let snap2 = gba.save_state();
        assert_eq!(snap1, snap2, "round-trip snapshot mismatch");

        // Spot-check a few restored fields directly.
        assert_eq!(gba.cpu.state.r[0], 0xDEAD_BEEF);
        assert!(gba.cpu.state.halted);
        assert_eq!(gba.cpu.cycles, 123_456);
        assert_eq!(gba.ppu.bg_x[0], -42);
        assert_eq!(gba.ppu.bg_pa[0], -1);
        assert_eq!(gba.irq.ie, 0x3FFF);
        assert!(gba.irq.cached_pending);
    }

    #[test]
    fn bad_magic_errors() {
        let mut gba = Gba::new();
        let err = gba.load_state(&[0, 1, 2, 3, 4, 5, 6, 7]).unwrap_err();
        assert!(err.contains("bad magic"));
    }
}
