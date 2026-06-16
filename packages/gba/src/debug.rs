//! Debug / introspection surface for the host (JS adapter).
//!
//! The React `DebugPanel` and `LinkPanel` read a flat snapshot of CPU / PPU /
//! DMA / timer / IRQ / sound / SIO state, do live `read8`/`read16` memory
//! probes, drive the SIO access trace, manage the active cheat list, and run
//! an IWRAM write-watch. The wasm/JS adapter can't reach into the private
//! sub-struct fields, so we expose everything it needs here as `pub fn` on
//! `Gba`, returning JSON strings hand-rolled with `format!` (no serde, no new
//! deps). All emitted numbers are decimal and all values are numeric/bool, so
//! no string escaping is ever required.

use crate::bus::Bus;
use crate::cheats::Cheat;
use crate::emulator::Gba;

impl Gba {
    // ---- aggregate state snapshot (DebugPanel CPU/IO tabs) ----

    /// Flat JSON object of the most-watched CPU/PPU/DMA/timer/IRQ/sound/SIO
    /// registers. Keys are snake_case; the adapter maps to the camelCase the
    /// panels read. Numbers decimal, bools `true`/`false`.
    pub fn debug_state_json(&self) -> String {
        let s = &self.cpu.state;
        let p = &self.ppu;

        let mut out = String::with_capacity(1024);
        out.push('{');

        // CPU
        out.push_str("\"r\":[");
        for (i, v) in s.r.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(&v.to_string());
        }
        out.push(']');
        out.push_str(&format!(",\"cpsr\":{}", s.cpsr));
        out.push_str(&format!(",\"halted\":{}", s.halted));

        // PPU
        out.push_str(&format!(",\"dispcnt\":{}", p.dispcnt));
        out.push_str(&format!(",\"dispstat\":{}", p.dispstat));
        out.push_str(&format!(",\"vcount\":{}", p.vcount));
        out.push_str(&format!(",\"mosaic\":{}", p.mosaic));
        out.push_str(&format!(",\"bgcnt\":{}", u32_arr4(&p.bgcnt)));
        out.push_str(&format!(",\"bg_hofs\":{}", u32_arr4(&p.bg_hofs)));
        out.push_str(&format!(",\"bg_vofs\":{}", u32_arr4(&p.bg_vofs)));
        out.push_str(&format!(",\"win0_h\":{}", p.win0_h));
        out.push_str(&format!(",\"win0_v\":{}", p.win0_v));
        out.push_str(&format!(",\"win1_h\":{}", p.win1_h));
        out.push_str(&format!(",\"win1_v\":{}", p.win1_v));
        out.push_str(&format!(",\"win_in\":{}", p.win_in));
        out.push_str(&format!(",\"win_out\":{}", p.win_out));
        out.push_str(&format!(",\"bldcnt\":{}", p.bldcnt));
        out.push_str(&format!(",\"bldalpha\":{}", p.bldalpha));

        // DMA
        out.push_str(",\"dma\":[");
        for (i, c) in self.dma.ch.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"enabled\":{},\"src\":{},\"dst\":{},\"count\":{}}}",
                c.enabled, c.src, c.dst, c.count
            ));
        }
        out.push(']');

        // Timers
        out.push_str(",\"timers\":[");
        for (i, c) in self.timers.ch.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"enabled\":{},\"reload\":{},\"counter\":{},\"prescale\":{}}}",
                c.enabled, c.reload, c.counter, c.prescale
            ));
        }
        out.push(']');

        // IRQ
        out.push_str(&format!(",\"ie\":{}", self.irq.ie));
        out.push_str(&format!(",\"iflag\":{}", self.irq.iflag));
        out.push_str(&format!(",\"ime\":{}", self.irq.ime));

        // Sound (count_a/count_b are i32; soundcnt_* are u32)
        out.push_str(&format!(",\"soundcnt_x\":{}", self.sound.soundcnt_x));
        out.push_str(&format!(",\"soundcnt_h\":{}", self.sound.soundcnt_h));
        out.push_str(&format!(",\"count_a\":{}", self.sound.count_a));
        out.push_str(&format!(",\"count_b\":{}", self.sound.count_b));

        // SIO (LinkPanel debug strip)
        out.push_str(&format!(",\"mlt_send\":{}", self.sio.mlt_send));
        out.push_str(&format!(",\"multi0\":{}", self.sio.multi[0]));
        out.push_str(&format!(",\"multi1\":{}", self.sio.multi[1]));
        out.push_str(&format!(",\"transfer_seq\":{}", self.sio.transfer_seq));

        out.push('}');
        out
    }

    // ---- live memory probes (DebugPanel MemoryView / LinkPanel SIOCNT) ----

    /// 8-bit bus read (the MemoryView hex dump uses this). Routed through the
    /// real `Bus` impl so IO overlays / save chips behave exactly as in-game.
    pub fn dbg_read8(&mut self, addr: u32) -> u32 {
        <Self as Bus>::read8(self, addr)
    }

    /// 16-bit bus read (LinkPanel reads SIOCNT via `read16(0x4000128)` to get
    /// the post-overlay SI/SD/ID bits).
    pub fn dbg_read16(&mut self, addr: u32) -> u32 {
        <Self as Bus>::read16(self, addr)
    }

    // ---- SIO access trace (LinkPanel SioTracer) ----

    pub fn sio_set_trace(&mut self, on: bool) {
        self.sio.trace_on = on;
    }

    pub fn sio_clear_trace(&mut self) {
        self.sio.clear_trace();
    }

    /// JSON array of the SIO trace entries. Keys mirror `SioTraceEntry`:
    /// `seq,pc,op,off,val,n`. `op` is the entry's `char` ('R'|'W') rendered as
    /// a one-char string.
    pub fn sio_trace_json(&self) -> String {
        let mut out = String::with_capacity(64 + self.sio.trace.len() * 48);
        out.push('[');
        for (i, e) in self.sio.trace.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"seq\":{},\"pc\":{},\"op\":\"{}\",\"off\":{},\"val\":{},\"n\":{}}}",
                e.seq, e.pc, e.op, e.off, e.val, e.n
            ));
        }
        out.push(']');
        out
    }

    // ---- cheats (CheatsPanel — active codes only) ----

    /// Replace the active cheat list from a newline-joined block of raw codes.
    /// Each non-empty trimmed line becomes an enabled `Cheat` with an empty
    /// name (the app keeps its own display list; we only need the live codes).
    pub fn set_cheats(&mut self, codes_newline_joined: &str) {
        let cheats: Vec<Cheat> = codes_newline_joined
            .split('\n')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| Cheat {
                name: String::new(),
                code: l.to_string(),
                enabled: true,
            })
            .collect();
        self.cheats = cheats;
    }

    // ---- async WebRTC link-cable bridge (sio-signal.ts SignalTransport) ----
    //
    // The host drives multiplay completion over WebRTC; these forward to the
    // matching `Sio` methods, threading `&mut self.irq` for the ones that
    // raise the SIO IRQ. See `sio.rs` for the protocol.

    /// Set the live link state. `connected` → SIOCNT.SD; `master` → SIOCNT
    /// SI/ID. `connected == false` (default) restores single-player.
    pub fn sio_set_link(&mut self, connected: bool, master: bool) {
        self.sio.set_link(connected, master);
    }

    /// Poll the master's outgoing multiplay payload. `Some(mlt_send)` exactly
    /// once after a transfer starts over a connected link; `None` otherwise.
    pub fn sio_take_outgoing(&mut self) -> Option<u32> {
        self.sio.take_outgoing()
    }

    /// Master-side completion: latch the synchronized 4-slot result, bump
    /// transfer_seq, clear START, raise the SIO IRQ if enabled.
    pub fn sio_deliver_multiplay(&mut self, m0: u32, m1: u32, m2: u32, m3: u32, error: bool) {
        self.sio
            .deliver_multiplay(m0, m1, m2, m3, error, &mut self.irq);
    }

    /// Slave-side: apply the remote master's broadcast (latch slots, clear
    /// START, raise the SIO IRQ if enabled).
    pub fn sio_apply_remote_multiplay(&mut self, m0: u32, m1: u32, m2: u32, m3: u32, error: bool) {
        self.sio
            .apply_remote_multiplay(m0, m1, m2, m3, error, &mut self.irq);
    }

    // ---- IWRAM write-watch (LinkPanel IwramWatch) ----

    /// Arm the write-watch over the inclusive byte range `[lo, hi]`. Every bus
    /// write whose access range overlaps is logged (pc, addr, size, val).
    pub fn set_watch(&mut self, lo: u32, hi: u32) {
        self.watch = Some((lo, hi));
    }

    /// Disarm the write-watch and drop the captured log.
    pub fn clear_watch(&mut self) {
        self.watch = None;
        self.watch_log.clear();
    }

    /// JSON array of the captured write-watch log: `[{"pc","addr","size","val"}]`.
    /// Reading does not clear the log (use `clear_watch` for that).
    pub fn take_watch_log(&self) -> String {
        let mut out = String::with_capacity(2 + self.watch_log.len() * 48);
        out.push('[');
        for (i, (pc, addr, size, val)) in self.watch_log.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"pc\":{},\"addr\":{},\"size\":{},\"val\":{}}}",
                pc, addr, size, val
            ));
        }
        out.push(']');
        out
    }
}

#[inline]
fn u32_arr4(a: &[u32; 4]) -> String {
    format!("[{},{},{},{}]", a[0], a[1], a[2], a[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_state_json_is_well_formed() {
        let gba = Gba::new();
        let json = gba.debug_state_json();
        assert!(!json.is_empty());
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"dispcnt\""));
        assert!(json.contains("\"r\":["));
        assert!(json.contains("\"dma\":["));
        assert!(json.contains("\"transfer_seq\""));
    }

    #[test]
    fn set_cheats_populates_active_list() {
        let mut gba = Gba::new();
        gba.set_cheats("  02000000 00000001 \n\n   \n12345678 0000ABCD");
        assert_eq!(gba.cheats.len(), 2);
        assert_eq!(gba.cheats[0].code, "02000000 00000001");
        assert_eq!(gba.cheats[1].code, "12345678 0000ABCD");
        assert!(gba.cheats.iter().all(|c| c.enabled && c.name.is_empty()));
    }

    #[test]
    fn watch_log_captures_overlapping_writes_and_clears() {
        let mut gba = Gba::new();
        gba.set_watch(0x0300_0010, 0x0300_0013);
        // In-range 16-bit write to IWRAM.
        <Gba as Bus>::write16(&mut gba, 0x0300_0010, 0xBEEF);
        // Out-of-range write.
        <Gba as Bus>::write8(&mut gba, 0x0300_0000, 0x12);
        let log = gba.take_watch_log();
        assert!(log.contains("\"addr\":50331664")); // 0x03000010
        assert!(!log.contains("\"size\":1")); // the out-of-range 8-bit write
        gba.clear_watch();
        assert_eq!(gba.take_watch_log(), "[]");
    }
}
