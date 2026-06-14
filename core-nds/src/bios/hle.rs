//! Minimal BIOS high-level emulation. Two halves:
//!
//!   1. HLE boot (`hle_boot`) — replaces the real ARM9/ARM7 BIOS + firmware
//!      handshake: parse the cart header, copy the binaries + overlays into RAM,
//!      mount the cart state machine, seed the BIOS-populated RAM block, install
//!      the IRQ-dispatch stubs, and reset both CPUs to their entry points with
//!      post-BIOS stacks. (TS `Emulator.loadRom` + `bios/stub.ts` boot path.)
//!
//!   2. SWI HLE (`bios_swi`) — catches a `SWI` before it enters the
//!      architectural exception vector and services it directly (decompression,
//!      IntrWait/VBlankIntrWait, Halt, Divide/Sqrt, CpuSet/CpuFastSet, CRC16,
//!      the ARM7 sound tables). (TS `bios/hle.ts`.)
//!
//! Ownership (CONTRACT.md): the TS `BiosHle` stored `cpu` + `irq` and reached
//! through `cpu.bus`. There's no such cycle here — `bios_swi` runs as a method
//! ON `Nds` (which the CPU executor consults first), so it has the whole
//! god-struct in hand: the SWI reads/writes registers via the active core's
//! `CpuState` and memory via the per-core `read*`/`write*` bus accessors. The
//! per-core IntrWait latches that the frame loop services live in this `BiosHle`
//! struct, one instance per core, owned by `Nds`.

use crate::cart::header::NdsHeader;
use crate::cart::{loader, overlays};
use crate::io::irq::IRQ_VBLANK;
use crate::nds::{Core, Nds};
use crate::state::{mode, CpuState, FLAG_F, FLAG_I};

/// Per-core BIOS HLE state: the pending IntrWait/VBlankIntrWait mask the frame
/// loop watches to lift a SWI-induced halt. (TS `BiosHle.pendingWaitMask` /
/// `pendingWaitDiscardOld`.) The sine/pitch/volume sound tables (SWI 0x20-0x22)
/// are built once in `new`.
pub struct BiosHle {
    /// Which core this services — gates the ARM9-only vs ARM7-only SWI sets.
    pub core: Core,
    /// IntrWait mask: the frame loop clears halt + acks IF when a matching IRQ
    /// bit comes up. 0 = not waiting.
    pub pending_wait_mask: u32,
    /// IntrWait "discard old" flag (r0 bit 0 at the SWI).
    pub pending_wait_discard_old: bool,

    /// ARM7 BIOS sound tables (SWI 0x20 sine / 0x21 pitch / 0x22 volume).
    /// Clean-room generated; games use them for audio-frequency math and don't
    /// verify the bytes.
    sine_table: Box<[i16; 64]>,
    pitch_table: Box<[u16; 768]>,
    volume_table: Box<[u8; 128]>,
}

impl BiosHle {
    /// Build a core's BIOS HLE state with the generated sound tables.
    pub fn new(core: Core) -> Self {
        let mut sine_table = Box::new([0i16; 64]);
        for (i, s) in sine_table.iter_mut().enumerate() {
            *s = ((i as f64 / 64.0 * std::f64::consts::FRAC_PI_2).sin() * 0x7FFF as f64).round()
                as i16;
        }
        let mut pitch_table = Box::new([0u16; 768]);
        for (i, p) in pitch_table.iter_mut().enumerate() {
            *p = ((2f64.powf(i as f64 / 768.0)) * 0x1000 as f64).round() as u32 as u16;
        }
        let mut volume_table = Box::new([0u8; 128]);
        for (i, v) in volume_table.iter_mut().enumerate() {
            *v = ((i as f64 / 127.0).powi(2) * 0x7F as f64).round() as u8;
        }
        BiosHle {
            core,
            pending_wait_mask: 0,
            pending_wait_discard_old: false,
            sine_table,
            pitch_table,
            volume_table,
        }
    }
}

impl Nds {
    /// HLE-boot a `.nds` ROM image: the data + reset half of skipping the real
    /// firmware/BIOS. Parses the header, copies the binaries + overlays into
    /// RAM, mounts the cart, seeds the BIOS-populated RAM, installs the
    /// IRQ-dispatch stubs, and resets both CPUs to their entry points with
    /// post-BIOS stacks. This is the seam `load_rom` forwards to.
    pub fn hle_boot(&mut self, rom: &[u8]) {
        // Fresh per-ROM assist state — the deadlock counters and the per-game
        // legacy tick-bump resolution must not leak across a ROM switch.
        self.nitro_os = crate::bios::nitro_os::NitroOsAssist::new();

        // Malformed ROM (shorter than the 512-byte header) — bail without
        // touching state. The TS threw here; we no-op so the caller stays sane.
        let header = match NdsHeader::parse(rom) {
            Ok(h) => h,
            Err(_) => return,
        };

        // Copy the ARM9/ARM7 binaries into RAM + stamp the BIOS-populated
        // shared-work block, then preload all overlays.
        let load = loader::load_rom(self, rom, &header);
        overlays::load_all(self, rom, &header);

        // Mount the cart command/transfer/save state machine over the ROM image.
        let mut cart = self.cart.take().unwrap_or_default();
        cart.mount(rom.to_vec());
        self.cart = Some(cart);

        // Install the canonical IRQ-dispatch stubs into both BIOS regions.
        install_bios_stubs(self);

        // Reset both CPUs to their entry points with post-BIOS stacks. These
        // stacks mirror the real DS firmware handoff (TS `Emulator.resetCpus`):
        // SYS sp = 0x0380FF00, IRQ sp = 0x0380FFA0, SVC sp = 0x0380FFE0.
        reset_cpu_state(&mut self.state9, load.arm9_entry, 0x0380_FF00, 0x0380_FFA0, 0x0380_FFE0);
        reset_cpu_state(&mut self.state7, load.arm7_entry, 0x0380_FF00, 0x0380_FFA0, 0x0380_FFE0);

        // POSTFLG = 1 (boot complete) on both cores — SDK init polls this.
        self.postflg9 = 1;
        self.postflg7 = 1;

        // Clear any stale IntrWait latches.
        self.bios9.pending_wait_mask = 0;
        self.bios9.pending_wait_discard_old = false;
        self.bios7.pending_wait_mask = 0;
        self.bios7.pending_wait_discard_old = false;
    }

    /// BIOS SWI seam. The CPU executor calls this FIRST on a `SWI`; returning
    /// `true` means HLE handled it (the executor skips the architectural vector
    /// entry), `false` means fall through to the real `software_interrupt`.
    /// `is_arm9` selects which core's register file + SWI table to use.
    ///
    /// `comment` is the SWI immediate; the low byte is the SWI number on both
    /// ARM (24-bit field) and THUMB (8-bit field). (TS `BiosHle.handleSwi`.)
    pub fn bios_swi(&mut self, is_arm9: bool, comment: u32) -> bool {
        let swi = comment & 0xFF;

        // Decompression / utility SWIs both CPUs implement identically.
        match swi {
            0x10 => {
                self.swi_bit_un_pack(is_arm9);
                return true;
            }
            0x11 => {
                self.swi_lz77_un_comp(is_arm9, false);
                return true;
            }
            0x12 => {
                self.swi_lz77_un_comp(is_arm9, true);
                return true;
            }
            0x13 => {
                self.swi_huff_un_comp(is_arm9);
                return true;
            }
            0x14 => {
                self.swi_rl_un_comp(is_arm9, false);
                return true;
            }
            0x15 => {
                self.swi_rl_un_comp(is_arm9, true);
                return true;
            }
            _ => {}
        }

        if is_arm9 {
            match swi {
                0x00 => self.swi_soft_reset(true),
                0x04 => {
                    let (r0, r1) = (self.state9.r[0], self.state9.r[1]);
                    self.swi_intr_wait(true, r0, r1);
                }
                0x05 => self.swi_vblank_wait(true),
                0x06 | 0x07 => self.swi_halt(true),
                0x08 => self.state9.r[0] = 0, // SoundBias (stub: ok)
                0x09 => {
                    let (n, d) = (self.state9.r[0] as i32, self.state9.r[1] as i32);
                    bios_divide(n, d, &mut self.state9.r);
                }
                0x0A => {
                    // Diff/mod helper used by some SDKs — signed remainder.
                    let n = self.state9.r[0] as i32;
                    let d = self.state9.r[1] as i32;
                    let d = if d == 0 { 1 } else { d };
                    self.state9.r[0] = n.wrapping_rem(d) as u32;
                }
                0x0B => self.swi_cpu_set(true),
                0x0C => self.swi_cpu_fast_set(true),
                0x0D => {
                    let v = (self.state9.r[0] as f64).sqrt().floor();
                    self.state9.r[0] = v as u32;
                }
                0x0E => self.swi_get_crc16(true),
                0x0F => self.state9.r[0] = if self.state9.r[0] != 0 { 1 } else { 0 },
                0x1F => {} // CustomHalt (stub)
                _ => {}    // unhandled — pretend success (return to user)
            }
            return true;
        }

        // ARM7-only SWI table.
        match swi {
            0x00 => self.swi_soft_reset(false),
            0x03 => self.swi_wait_by_loop(false),
            0x04 => {
                let (r0, r1) = (self.state7.r[0], self.state7.r[1]);
                self.swi_intr_wait(false, r0, r1);
            }
            0x05 => self.swi_vblank_wait(false),
            0x06 | 0x07 => self.swi_halt(false),
            0x08 => self.state7.r[0] = 0, // SoundBias
            0x09 => {
                let (n, d) = (self.state7.r[0] as i32, self.state7.r[1] as i32);
                bios_divide(n, d, &mut self.state7.r);
            }
            0x0A => {
                let n = self.state7.r[0] as i32;
                let d = self.state7.r[1] as i32;
                let d = if d == 0 { 1 } else { d };
                self.state7.r[0] = n.wrapping_rem(d) as u32;
            }
            0x0B => self.swi_cpu_set(false),
            0x0C => self.swi_cpu_fast_set(false),
            0x0E => self.swi_get_crc16(false),
            0x1F => {} // CustomHalt (stub)
            0x20 => {
                let i = (self.state7.r[0] & 0x3F) as usize;
                self.state7.r[0] = self.bios7.sine_table[i] as u16 as u32;
            }
            0x21 => {
                let i = (self.state7.r[0] & 0x2FF) as usize;
                self.state7.r[0] = self.bios7.pitch_table[i] as u32;
            }
            0x22 => {
                let i = (self.state7.r[0] & 0x7F) as usize;
                self.state7.r[0] = self.bios7.volume_table[i] as u32;
            }
            _ => {} // unhandled — pretend success
        }
        true
    }

    /// Per-frame IntrWait service: if a core is halted in an IntrWait and a
    /// masked IRQ has fired, ack it and lift the halt. Called by the frame loop
    /// after the PPU advances. (TS `BiosHle.serviceWait`.)
    pub fn bios_service_wait(&mut self, is_arm9: bool) {
        let (mask, halted) = if is_arm9 {
            (self.bios9.pending_wait_mask, self.state9.halted)
        } else {
            (self.bios7.pending_wait_mask, self.state7.halted)
        };
        if !halted || mask == 0 {
            return;
        }
        let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
        let fired = irq.iflag & mask;
        if fired != 0 {
            irq.ack_if(fired);
            if is_arm9 {
                self.state9.halted = false;
                self.bios9.pending_wait_mask = 0;
            } else {
                self.state7.halted = false;
                self.bios7.pending_wait_mask = 0;
            }
        }
    }

    // ─── SWI implementations (private helpers on Nds) ────────────────────────

    /// SoftReset (SWI 0x00): clear the GP/scratch registers, set the post-reset
    /// stack, and jump to the entry vector. We send both cores back to their
    /// cart entry point (the data is already resident from `hle_boot`).
    fn swi_soft_reset(&mut self, is_arm9: bool) {
        let entry = if is_arm9 {
            // ARM9 reset: high-vector environment uses the ARM9 entry already in
            // RAM; the conventional convention is to re-enter at 0x02000000.
            0x0200_0000
        } else {
            0x0200_0000
        };
        let st = if is_arm9 { &mut self.state9 } else { &mut self.state7 };
        for i in 0..8 {
            st.r[i] = 0;
        }
        st.switch_mode(mode::SVC);
        st.r[13] = 0x0380_FFE0;
        st.switch_mode(mode::SYS);
        st.cpsr = mode::SYS; // F/I clear, ARM
        st.r[13] = 0x0380_FF00;
        st.r[15] = entry;
        st.halted = false;
    }

    /// GBA-style IntrWait (SWI 0x04): r0 = discardOld, r1 = bitmask. If
    /// discardOld and a wanted IF bit is already set, ack it; otherwise halt.
    /// The frame loop's `bios_service_wait` wakes us when a wanted bit appears.
    fn swi_intr_wait(&mut self, is_arm9: bool, discard_old: u32, mask: u32) {
        let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
        if discard_old != 0 && (irq.iflag & mask) != 0 {
            let fired = irq.iflag & mask;
            irq.ack_if(fired);
        }
        // The real BIOS clears CPSR.I and sets IME=1 before halting so the IRQ
        // it's waiting for can fire. Games SWI us with I=1 expecting that.
        irq.set_ime(1);
        if is_arm9 {
            self.bios9.pending_wait_mask = mask;
            self.bios9.pending_wait_discard_old = discard_old & 1 != 0;
            self.state9.cpsr &= !FLAG_I;
            self.state9.halted = true;
        } else {
            self.bios7.pending_wait_mask = mask;
            self.bios7.pending_wait_discard_old = discard_old & 1 != 0;
            self.state7.cpsr &= !FLAG_I;
            self.state7.halted = true;
        }
    }

    /// VBlankIntrWait (SWI 0x05) — IntrWait(discardOld=1, mask=VBLANK).
    fn swi_vblank_wait(&mut self, is_arm9: bool) {
        self.swi_intr_wait(is_arm9, 1, IRQ_VBLANK);
    }

    /// HALT / Sleep (SWI 0x06/0x07) — wait for any IRQ. Like IntrWait but
    /// doesn't filter by mask or touch IF. Must clear CPSR.I and set IME=1 so
    /// the halt-wake path can actually unhalt.
    fn swi_halt(&mut self, is_arm9: bool) {
        let irq = if is_arm9 { &mut self.irq9 } else { &mut self.irq7 };
        irq.set_ime(1);
        if is_arm9 {
            self.state9.cpsr &= !FLAG_I;
            self.state9.halted = true;
        } else {
            self.state7.cpsr &= !FLAG_I;
            self.state7.halted = true;
        }
    }

    /// WaitByLoop (SWI 0x03, ARM7). r0 = iteration count. The real BIOS spins
    /// ~4 ARM cycles per iteration; we have no cycle budget on the SWI seam, so
    /// it returns immediately. (The cycle-stall the TS added lived on the CPU;
    /// the executor's IPC-handshake timing is handled there now.)
    fn swi_wait_by_loop(&mut self, _is_arm9: bool) {
        // No-op: return to caller. (TS parked `cpu.stallCycles`; not modeled.)
    }

    /// GetCRC16 (SWI 0x0E). r0 = initial CRC, r1 = data ptr, r2 = byte length.
    /// Standard NDS BIOS CRC-16 (reflected polynomial 0xA001). Result in r0.
    fn swi_get_crc16(&mut self, is_arm9: bool) {
        let (mut crc, ptr, len) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0] & 0xFFFF, st.r[1], st.r[2])
        };
        for i in 0..len {
            let byte = self.read8(is_arm9, ptr.wrapping_add(i)) & 0xFF;
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xA001
                } else {
                    crc >> 1
                };
            }
        }
        let st = if is_arm9 { &mut self.state9 } else { &mut self.state7 };
        st.r[0] = crc & 0xFFFF;
    }

    /// CpuSet (SWI 0x0B). r0=src, r1=dst, r2=length+mode. Mode bits: 0..20 =
    /// unit count; bit 24 = fixed source (fill); bit 26 = 32-bit (else 16).
    fn swi_cpu_set(&mut self, is_arm9: bool) {
        let (src, dst, mode_w) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1], st.r[2])
        };
        let count = mode_w & 0x1F_FFFF;
        let fixed = (mode_w & 0x0100_0000) != 0;
        let word32 = (mode_w & 0x0400_0000) != 0;
        let mut s = src;
        let mut d = dst;
        let step = if word32 { 4 } else { 2 };
        for _ in 0..count {
            if word32 {
                let v = self.read32(is_arm9, s & !3);
                self.write32(is_arm9, d & !3, v);
            } else {
                let v = self.read16(is_arm9, s & !1);
                self.write16(is_arm9, d & !1, v);
            }
            if !fixed {
                s = s.wrapping_add(step);
            }
            d = d.wrapping_add(step);
        }
    }

    /// CpuFastSet (SWI 0x0C) — like CpuSet but 32-bit only.
    fn swi_cpu_fast_set(&mut self, is_arm9: bool) {
        let (src, dst, mode_w) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1], st.r[2])
        };
        let count = mode_w & 0x1F_FFFF;
        let fixed = (mode_w & 0x0100_0000) != 0;
        let mut s = src;
        let mut d = dst;
        for _ in 0..count {
            let v = self.read32(is_arm9, s & !3);
            self.write32(is_arm9, d & !3, v);
            if !fixed {
                s = s.wrapping_add(4);
            }
            d = d.wrapping_add(4);
        }
    }

    /// LZ77UnComp (SWI 0x11 = WRAM target / 0x12 = VRAM target). Header u32:
    /// low 8 = compression type, high 24 = decompressed length. Stream of flag
    /// bytes (MSB-first) selecting literal (0) or 2-byte backref (1).
    fn swi_lz77_un_comp(&mut self, is_arm9: bool, vram: bool) {
        let (src_addr, dst_addr) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1])
        };
        let header = self.read32(is_arm9, src_addr);
        let size = header >> 8;
        let mut src = src_addr.wrapping_add(4);
        let mut dst = dst_addr;
        let mut written = 0u32;
        while written < size {
            let flags = self.read8(is_arm9, src);
            src = src.wrapping_add(1);
            let mut bit: i32 = 7;
            while bit >= 0 && written < size {
                if (flags >> bit) & 1 != 0 {
                    let hi = self.read8(is_arm9, src);
                    src = src.wrapping_add(1);
                    let lo = self.read8(is_arm9, src);
                    src = src.wrapping_add(1);
                    let len = ((hi >> 4) & 0xF) + 3;
                    let disp = ((hi & 0xF) << 8) | lo;
                    let mut i = 0;
                    while i < len && written < size {
                        let b = self.read8(is_arm9, dst.wrapping_sub(disp).wrapping_sub(1));
                        self.lz_write_byte(is_arm9, vram, &mut dst, b);
                        written += 1;
                        i += 1;
                    }
                } else {
                    let b = self.read8(is_arm9, src);
                    src = src.wrapping_add(1);
                    self.lz_write_byte(is_arm9, vram, &mut dst, b);
                    written += 1;
                }
                bit -= 1;
            }
        }
    }

    /// RLUnComp (SWI 0x14/0x15). Header same as LZ77. Flag byte: MSB=1 → next
    /// byte repeated (low7 + 3) times; MSB=0 → (low7 + 1) literal bytes follow.
    fn swi_rl_un_comp(&mut self, is_arm9: bool, vram: bool) {
        let (src_addr, dst_addr) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1])
        };
        let header = self.read32(is_arm9, src_addr);
        let size = header >> 8;
        let mut src = src_addr.wrapping_add(4);
        let mut dst = dst_addr;
        let mut written = 0u32;
        while written < size {
            let flag = self.read8(is_arm9, src);
            src = src.wrapping_add(1);
            if flag & 0x80 != 0 {
                let data = self.read8(is_arm9, src);
                src = src.wrapping_add(1);
                let count = (flag & 0x7F) + 3;
                let mut i = 0;
                while i < count && written < size {
                    self.lz_write_byte(is_arm9, vram, &mut dst, data);
                    written += 1;
                    i += 1;
                }
            } else {
                let count = (flag & 0x7F) + 1;
                let mut i = 0;
                while i < count && written < size {
                    let b = self.read8(is_arm9, src);
                    src = src.wrapping_add(1);
                    self.lz_write_byte(is_arm9, vram, &mut dst, b);
                    written += 1;
                    i += 1;
                }
            }
        }
    }

    /// HuffUnComp (SWI 0x13). Source: u32 header (low nibble = symbol bit-width
    /// 4 or 8, bits 8-31 = byte size), u8 tree-size word count, the tree nodes,
    /// then an MSB-first 32-bit bitstream traversing the tree.
    fn swi_huff_un_comp(&mut self, is_arm9: bool) {
        let (src_addr, dst_addr) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1])
        };
        let header = self.read32(is_arm9, src_addr);
        let sym_bits = header & 0xF; // 4 or 8
        let size = header >> 8;
        let tree_size_bytes = (self.read8(is_arm9, src_addr.wrapping_add(4)) + 1) * 2;
        let tree_start = src_addr.wrapping_add(5);
        let mut stream_ptr = src_addr.wrapping_add(4).wrapping_add(tree_size_bytes);
        let mut dst = dst_addr;
        let mut written = 0u32;
        let mut buf: u32 = 0;
        let mut buf_bits: u32 = 0;
        let mut out_shift: u32 = 0;
        let mut out_byte: u32 = 0;
        let sym_mask = if sym_bits == 0 { 0 } else { (1u32 << sym_bits) - 1 };

        while written < size {
            // Walk from the root for one symbol.
            let mut node_off: u32 = 0;
            loop {
                if buf_bits == 0 {
                    buf = self.read32(is_arm9, stream_ptr);
                    stream_ptr = stream_ptr.wrapping_add(4);
                    buf_bits = 32;
                }
                let bit = (buf >> 31) & 1;
                buf <<= 1;
                buf_bits -= 1;
                let node = self.read8(is_arm9, tree_start.wrapping_add(node_off));
                let is_leaf = (node & (if bit != 0 { 0x40 } else { 0x80 })) != 0;
                let child_base = ((node_off >> 1) + (node & 0x3F) + 1) * 2;
                node_off = child_base + bit;
                if is_leaf {
                    let v = self.read8(is_arm9, tree_start.wrapping_add(node_off));
                    out_byte |= (v & sym_mask) << out_shift;
                    out_shift += sym_bits;
                    if out_shift == 8 {
                        self.write8(is_arm9, dst, out_byte & 0xFF);
                        dst = dst.wrapping_add(1);
                        out_byte = 0;
                        out_shift = 0;
                        written += 1;
                    }
                    break;
                }
            }
        }
    }

    /// BitUnPack (SWI 0x10). r0=src, r1=dst, r2=param block. Param block: u16
    /// srcLen, u8 srcWidth, u8 dstWidth, u32 dataOffset (bit31 = zero flag).
    fn swi_bit_un_pack(&mut self, is_arm9: bool) {
        let (src_addr, dst_addr, param_addr) = {
            let st = if is_arm9 { &self.state9 } else { &self.state7 };
            (st.r[0], st.r[1], st.r[2])
        };
        let src_len = self.read16(is_arm9, param_addr);
        let src_width = self.read8(is_arm9, param_addr.wrapping_add(2));
        let dst_width = self.read8(is_arm9, param_addr.wrapping_add(3));
        let offset_info = self.read32(is_arm9, param_addr.wrapping_add(4));
        let data_offset = offset_info & 0x7FFF_FFFF;
        let zero_flag = (offset_info >> 31) & 1;
        if src_width == 0 || dst_width == 0 {
            return;
        }
        let src_mask = (1u32 << src_width) - 1;
        let dst_mask = if dst_width >= 32 {
            0xFFFF_FFFF
        } else {
            (1u32 << dst_width) - 1
        };
        let mut dst_acc: u32 = 0;
        let mut dst_bits: u32 = 0;
        let mut dst_ptr = dst_addr;
        for i in 0..src_len {
            let byte = self.read8(is_arm9, src_addr.wrapping_add(i));
            let mut b = 0u32;
            while b < 8 {
                let mut val = (byte >> b) & src_mask;
                if val != 0 || zero_flag != 0 {
                    val = (val + data_offset) & dst_mask;
                }
                dst_acc |= val << dst_bits;
                dst_bits += dst_width;
                if dst_bits >= 32 {
                    self.write32(is_arm9, dst_ptr, dst_acc);
                    dst_ptr = dst_ptr.wrapping_add(4);
                    dst_acc = 0;
                    dst_bits = 0;
                }
                b += src_width;
            }
        }
        if dst_bits > 0 {
            self.write32(is_arm9, dst_ptr, dst_acc);
        }
    }

    // ─── Per-core bus shims (so the SWI bodies stay core-agnostic) ──────────

    #[inline]
    fn read8(&mut self, is_arm9: bool, addr: u32) -> u32 {
        if is_arm9 {
            self.read8_arm9(addr)
        } else {
            self.read8_arm7(addr)
        }
    }
    #[inline]
    fn read16(&mut self, is_arm9: bool, addr: u32) -> u32 {
        if is_arm9 {
            self.read16_arm9(addr)
        } else {
            self.read16_arm7(addr)
        }
    }
    #[inline]
    fn read32(&mut self, is_arm9: bool, addr: u32) -> u32 {
        if is_arm9 {
            self.read32_arm9(addr)
        } else {
            self.read32_arm7(addr)
        }
    }
    #[inline]
    fn write8(&mut self, is_arm9: bool, addr: u32, v: u32) {
        if is_arm9 {
            self.write8_arm9(addr, v)
        } else {
            self.write8_arm7(addr, v)
        }
    }
    #[inline]
    fn write16(&mut self, is_arm9: bool, addr: u32, v: u32) {
        if is_arm9 {
            self.write16_arm9(addr, v)
        } else {
            self.write16_arm7(addr, v)
        }
    }
    #[inline]
    fn write32(&mut self, is_arm9: bool, addr: u32, v: u32) {
        if is_arm9 {
            self.write32_arm9(addr, v)
        } else {
            self.write32_arm7(addr, v)
        }
    }

    /// Shared byte emitter for the byte-oriented decompressors. In WRAM mode it
    /// writes a byte and advances; in VRAM mode it does read-modify-write of the
    /// containing halfword (DS VRAM rejects byte writes).
    #[inline]
    fn lz_write_byte(&mut self, is_arm9: bool, vram: bool, dst: &mut u32, v: u32) {
        let v = v & 0xFF;
        if vram {
            let aligned = *dst & !1;
            let cur = self.read16(is_arm9, aligned);
            let shift = (*dst & 1) * 8;
            let masked = (cur & !(0xFF << shift)) | (v << shift);
            self.write16(is_arm9, aligned, masked & 0xFFFF);
        } else {
            self.write8(is_arm9, *dst, v);
        }
        *dst = dst.wrapping_add(1);
    }
}

/// Reset a `CpuState` to the post-BIOS environment: SYS mode, ARM, with the
/// supplied stacks banked in and the entry PC live. Mirrors `Cpu::reset` but
/// operates on the foundation owner's `CpuState` directly (the executor syncs
/// its live `Cpu.state` from this on the next step).
fn reset_cpu_state(st: &mut CpuState, entry_pc: u32, sys_sp: u32, irq_sp: u32, svc_sp: u32) {
    *st = CpuState::new();
    st.cpsr = mode::SVC | FLAG_I | FLAG_F;
    st.switch_mode(mode::SYS);
    st.r[13] = sys_sp;
    st.bank_r13[2] = irq_sp; // IRQ bank
    st.bank_r13[3] = svc_sp; // SVC bank
    st.r[15] = entry_pc;
    st.halted = false;
}

/// Install the canonical IRQ-dispatch stub into both CPUs' BIOS regions: the
/// IRQ vector at 0x18 saves context, jumps to the user-stored handler pointer
/// (ARM7 at 0x03FFFFFC, ARM9 at 0x027FFFFC via a literal), and returns with
/// CPSR restored. All other vectors branch-to-self. (TS `bios/stub.ts`
/// `installBiosStubs`.)
pub fn install_bios_stubs(nds: &mut Nds) {
    install_arm7_stub(&mut nds.mem.bios_arm7[..]);
    install_arm9_stub(&mut nds.mem.bios_arm9[..]);
}

#[inline]
fn wr32(bios: &mut [u8], off: usize, v: u32) {
    bios[off] = (v & 0xFF) as u8;
    bios[off + 1] = ((v >> 8) & 0xFF) as u8;
    bios[off + 2] = ((v >> 16) & 0xFF) as u8;
    bios[off + 3] = ((v >> 24) & 0xFF) as u8;
}

fn install_arm7_stub(bios: &mut [u8]) {
    // Loop-on-self at all standard vectors except IRQ.
    let mut v = 0;
    while v < 0x18 {
        wr32(bios, v, 0xEAFF_FFFE);
        v += 4;
    }
    wr32(bios, 0x18, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    wr32(bios, 0x1C, 0xE3A0_0301); // MOV R0, #0x4000000
    wr32(bios, 0x20, 0xE28F_E000); // ADR LR, returnLabel (= 0x28)
    wr32(bios, 0x24, 0xE510_F004); // LDR PC, [R0, #-4] — = [0x03FFFFFC]
    wr32(bios, 0x28, 0xE8BD_500F); // returnLabel: LDMFD SP!, {R0-R3, R12, LR}
    wr32(bios, 0x2C, 0xE25E_F004); // SUBS PC, LR, #4 (returns + restores CPSR)
}

fn install_arm9_stub(bios: &mut [u8]) {
    let mut v = 0;
    while v < 0x18 {
        wr32(bios, v, 0xEAFF_FFFE);
        v += 4;
    }
    wr32(bios, 0x18, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    wr32(bios, 0x1C, 0xE59F_0010); // LDR R0, [PC, #0x10] — reads 0x34 literal
    wr32(bios, 0x20, 0xE590_0000); // LDR R0, [R0]
    wr32(bios, 0x24, 0xE28F_E000); // ADR LR, returnLabel (= 0x2C)
    wr32(bios, 0x28, 0xE12F_FF10); // BX R0
    wr32(bios, 0x2C, 0xE8BD_500F); // returnLabel: LDMFD SP!, {R0-R3, R12, LR}
    wr32(bios, 0x30, 0xE25E_F004); // SUBS PC, LR, #4
    wr32(bios, 0x34, 0x027F_FFFC); // literal: user handler pointer location
}

/// 32-bit signed BIOS divide (SWI 0x09): writes quotient/remainder/abs-quotient
/// into r0/r1/r3 of the given register file. Free function — pure on the
/// registers. (TS `divide`.)
pub(crate) fn bios_divide(num: i32, den: i32, r: &mut [u32; 16]) {
    if den == 0 {
        // Hardware-ish behavior: r0 = sign of num, r1 = num, r3 = |num|.
        r[0] = if num < 0 { 0xFFFF_FFFF } else { 1 };
        r[1] = num as u32;
        r[3] = num.unsigned_abs();
        return;
    }
    let q = num.wrapping_div(den);
    let rem = num.wrapping_sub(q.wrapping_mul(den));
    r[0] = q as u32;
    r[1] = rem as u32;
    r[3] = q.unsigned_abs();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::irq::{IRQ_TIMER0, IRQ_VBLANK};
    use crate::state::FLAG_T;

    // ── bios_divide ──────────────────────────────────────────────────────
    #[test]
    fn divide_basic() {
        let mut r = [0u32; 16];
        bios_divide(17, 5, &mut r);
        assert_eq!(r[0], 3); // quotient
        assert_eq!(r[1], 2); // remainder
        assert_eq!(r[3], 3); // |quotient|
    }

    #[test]
    fn divide_negative_truncates_toward_zero() {
        let mut r = [0u32; 16];
        bios_divide(-17, 5, &mut r);
        assert_eq!(r[0] as i32, -3);
        assert_eq!(r[1] as i32, -2);
        assert_eq!(r[3], 3);
    }

    #[test]
    fn divide_by_zero() {
        let mut r = [0u32; 16];
        bios_divide(-9, 0, &mut r);
        assert_eq!(r[0], 0xFFFF_FFFF);
        assert_eq!(r[1] as i32, -9);
        assert_eq!(r[3], 9);
    }

    // ── SWI dispatch via Nds ─────────────────────────────────────────────
    #[test]
    fn swi_divide_arm9() {
        let mut nds = Nds::new();
        nds.state9.r[0] = 100;
        nds.state9.r[1] = 7;
        assert!(nds.bios_swi(true, 0x09));
        assert_eq!(nds.state9.r[0], 14);
        assert_eq!(nds.state9.r[1], 2);
    }

    #[test]
    fn swi_sqrt_arm9() {
        let mut nds = Nds::new();
        nds.state9.r[0] = 144;
        assert!(nds.bios_swi(true, 0x0D));
        assert_eq!(nds.state9.r[0], 12);
    }

    #[test]
    fn swi_sqrt_is_arm9_only() {
        // ARM7 has no 0x0D Sqrt — it falls into the "pretend success" arm and
        // leaves r0 untouched. (We still return true: HLE swallowed it.)
        let mut nds = Nds::new();
        nds.state7.r[0] = 144;
        assert!(nds.bios_swi(false, 0x0D));
        assert_eq!(nds.state7.r[0], 144);
    }

    #[test]
    fn swi_cpu_set_word_copy() {
        let mut nds = Nds::new();
        // Source words in main RAM.
        for i in 0..4u32 {
            nds.write32_arm9(0x0200_0000 + i * 4, 0x1000 + i);
        }
        nds.state9.r[0] = 0x0200_0000; // src
        nds.state9.r[1] = 0x0200_1000; // dst
        nds.state9.r[2] = 4 | 0x0400_0000; // 4 words, 32-bit
        assert!(nds.bios_swi(true, 0x0B));
        for i in 0..4u32 {
            assert_eq!(nds.read32_arm9(0x0200_1000 + i * 4), 0x1000 + i);
        }
    }

    #[test]
    fn swi_cpu_set_fixed_fill() {
        let mut nds = Nds::new();
        nds.write32_arm9(0x0200_0000, 0xABCD_1234);
        nds.state9.r[0] = 0x0200_0000;
        nds.state9.r[1] = 0x0200_1000;
        nds.state9.r[2] = 4 | 0x0100_0000 | 0x0400_0000; // 4 words, fixed src, 32-bit
        assert!(nds.bios_swi(true, 0x0B));
        for i in 0..4u32 {
            assert_eq!(nds.read32_arm9(0x0200_1000 + i * 4), 0xABCD_1234);
        }
    }

    #[test]
    fn swi_get_crc16_arm9() {
        let mut nds = Nds::new();
        let data = [0x01u8, 0x02, 0x03, 0x04];
        for (i, &b) in data.iter().enumerate() {
            nds.write8_arm9(0x0200_0000 + i as u32, b as u32);
        }
        nds.state9.r[0] = 0xFFFF; // init CRC
        nds.state9.r[1] = 0x0200_0000;
        nds.state9.r[2] = 4;
        assert!(nds.bios_swi(true, 0x0E));
        // Reference CRC-16/MODBUS over {01,02,03,04} with init 0xFFFF.
        let mut crc = 0xFFFFu32;
        for &b in &data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        assert_eq!(nds.state9.r[0], crc & 0xFFFF);
    }

    #[test]
    fn swi_lz77_roundtrip_wram() {
        // Compress "AAAAABBBBB" → 5 literal A then a backref? Build a known
        // stream by hand: simplest is all-literal (no backrefs) for clarity.
        let mut nds = Nds::new();
        let src = 0x0200_0000u32;
        let dst = 0x0200_2000u32;
        // Header: size=4 in high 24 bits, type=1 in low byte.
        nds.write32_arm9(src, (4 << 8) | 0x10);
        // Flag byte 0x00 → next 8 chunks are literals (we only need 4).
        nds.write8_arm9(src + 4, 0x00);
        nds.write8_arm9(src + 5, 0xDE);
        nds.write8_arm9(src + 6, 0xAD);
        nds.write8_arm9(src + 7, 0xBE);
        nds.write8_arm9(src + 8, 0xEF);
        nds.state9.r[0] = src;
        nds.state9.r[1] = dst;
        assert!(nds.bios_swi(true, 0x11));
        assert_eq!(nds.read8_arm9(dst), 0xDE);
        assert_eq!(nds.read8_arm9(dst + 1), 0xAD);
        assert_eq!(nds.read8_arm9(dst + 2), 0xBE);
        assert_eq!(nds.read8_arm9(dst + 3), 0xEF);
    }

    #[test]
    fn swi_lz77_backref() {
        // Literal 'X', then a backref len=3 disp=1 → "XXXX".
        let mut nds = Nds::new();
        let src = 0x0200_0000u32;
        let dst = 0x0200_2000u32;
        nds.write32_arm9(src, (4 << 8) | 0x10); // size 4
        nds.write8_arm9(src + 4, 0x40); // flags: bit7=0 (literal), bit6=1 (backref)
        nds.write8_arm9(src + 5, 0x58); // literal 'X'
        // backref: hi = (len-3)<<4 | disp_hi, lo = disp_lo; len=3→hi nibble 0,
        // disp=1 → encoded disp = 0 (disp = field + 1).
        nds.write8_arm9(src + 6, 0x00);
        nds.write8_arm9(src + 7, 0x00);
        nds.state9.r[0] = src;
        nds.state9.r[1] = dst;
        assert!(nds.bios_swi(true, 0x11));
        for i in 0..4u32 {
            assert_eq!(nds.read8_arm9(dst + i), 0x58);
        }
    }

    #[test]
    fn swi_rl_uncomp() {
        // RLE: run of 5 'Q' (0x51). flag = 0x80 | (5-3) = 0x82, data = 0x51.
        let mut nds = Nds::new();
        let src = 0x0200_0000u32;
        let dst = 0x0200_2000u32;
        nds.write32_arm9(src, (5 << 8) | 0x30); // size 5, type 3 (RLE)
        nds.write8_arm9(src + 4, 0x82);
        nds.write8_arm9(src + 5, 0x51);
        nds.state9.r[0] = src;
        nds.state9.r[1] = dst;
        assert!(nds.bios_swi(true, 0x14));
        for i in 0..5u32 {
            assert_eq!(nds.read8_arm9(dst + i), 0x51);
        }
    }

    #[test]
    fn swi_sound_tables_arm7() {
        let mut nds = Nds::new();
        // SWI 0x20 sine[0] = 0.
        nds.state7.r[0] = 0;
        assert!(nds.bios_swi(false, 0x20));
        assert_eq!(nds.state7.r[0], 0);
        // SWI 0x21 pitch[0] = round(2^0 * 0x1000) = 0x1000.
        nds.state7.r[0] = 0;
        assert!(nds.bios_swi(false, 0x21));
        assert_eq!(nds.state7.r[0], 0x1000);
        // SWI 0x22 volume[127] = round(1 * 0x7F) = 0x7F.
        nds.state7.r[0] = 127;
        assert!(nds.bios_swi(false, 0x22));
        assert_eq!(nds.state7.r[0], 0x7F);
    }

    #[test]
    fn swi_intr_wait_halts_and_unmasks() {
        let mut nds = Nds::new();
        nds.state9.cpsr |= FLAG_I; // I masked at SWI entry
        nds.state9.r[0] = 0; // discardOld = 0
        nds.state9.r[1] = IRQ_VBLANK;
        assert!(nds.bios_swi(true, 0x04));
        assert!(nds.state9.halted);
        assert_eq!(nds.state9.cpsr & FLAG_I, 0); // I cleared
        assert!(nds.irq9.ime);
        assert_eq!(nds.bios9.pending_wait_mask, IRQ_VBLANK);
    }

    #[test]
    fn service_wait_lifts_halt_on_matching_irq() {
        let mut nds = Nds::new();
        nds.state9.r[1] = IRQ_VBLANK;
        nds.bios_swi(true, 0x04);
        assert!(nds.state9.halted);
        // Unrelated IRQ doesn't wake.
        nds.irq9.raise(IRQ_TIMER0);
        nds.bios_service_wait(true);
        assert!(nds.state9.halted);
        // Matching IRQ wakes + acks.
        nds.irq9.raise(IRQ_VBLANK);
        nds.bios_service_wait(true);
        assert!(!nds.state9.halted);
        assert_eq!(nds.irq9.iflag & IRQ_VBLANK, 0);
        assert_eq!(nds.bios9.pending_wait_mask, 0);
    }

    #[test]
    fn swi_vblank_wait_discards_old() {
        let mut nds = Nds::new();
        // A stale VBLANK is already pending; VBlankIntrWait discards it.
        nds.irq9.set_ie(IRQ_VBLANK);
        nds.irq9.raise(IRQ_VBLANK);
        assert!(nds.bios_swi(true, 0x05));
        assert_eq!(nds.irq9.iflag & IRQ_VBLANK, 0); // discarded
        assert!(nds.state9.halted);
    }

    #[test]
    fn install_stubs_writes_irq_dispatch() {
        let mut nds = Nds::new();
        install_bios_stubs(&mut nds);
        // ARM9 IRQ vector at 0x18 = STMFD SP!, {R0-R3,R12,LR} (LE bytes).
        let b = &nds.mem.bios_arm9;
        let word = (b[0x18] as u32)
            | ((b[0x19] as u32) << 8)
            | ((b[0x1A] as u32) << 16)
            | ((b[0x1B] as u32) << 24);
        assert_eq!(word, 0xE92D_500F);
        // ARM9 literal at 0x34 = user-handler pointer location.
        let lit = (b[0x34] as u32)
            | ((b[0x35] as u32) << 8)
            | ((b[0x36] as u32) << 16)
            | ((b[0x37] as u32) << 24);
        assert_eq!(lit, 0x027F_FFFC);
        // Reset vector loops to self.
        let v0 = (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24);
        assert_eq!(v0, 0xEAFF_FFFE);
    }

    #[test]
    fn unhandled_swi_returns_true_arm9() {
        // A SWI number with no handler still returns "handled" (we never want to
        // fall into the architectural vector, which has only a self-loop stub).
        let mut nds = Nds::new();
        assert!(nds.bios_swi(true, 0x55));
    }

    // ── BitUnPack (SWI 0x10) — mirrors ds-recomp bios_hle.test.ts ─────────
    #[test]
    fn swi_bit_un_pack_expands_1bpp_to_8bpp() {
        // Source byte 0b10110100, 1bpp → 8bpp, dataOffset=0, zeroFlag=0.
        // Bits are read LSB-first: 0,0,1,0,1,1,0,1.
        let mut nds = Nds::new();
        let src = 0x0200_0000u32;
        let dst = 0x0200_1000u32;
        let param = 0x0200_2000u32;
        nds.write8_arm9(src, 0b1011_0100);
        nds.write16_arm9(param, 1); // srcLen = 1
        nds.write8_arm9(param + 2, 1); // srcWidth = 1
        nds.write8_arm9(param + 3, 8); // dstWidth = 8
        nds.write32_arm9(param + 4, 0); // dataOffset = 0, zeroFlag = 0
        nds.state9.r[0] = src;
        nds.state9.r[1] = dst;
        nds.state9.r[2] = param;
        assert!(nds.bios_swi(true, 0x10));
        let expected = [0u32, 0, 1, 0, 1, 1, 0, 1];
        for (i, &e) in expected.iter().enumerate() {
            assert_eq!(nds.read8_arm9(dst + i as u32), e, "byte {i}");
        }
    }

    // ── End-to-end HLE boot of a SYNTHETIC minimal cart ──────────────────
    //
    // Build a small .nds image with distinct ARM9/ARM7 binaries, run hle_boot,
    // and assert the executables landed in RAM at their load addresses and the
    // entry PCs + post-BIOS stacks are set on both cores.

    /// One core's slice of a synthetic cart: where it lives in the ROM, where it
    /// loads in RAM, its entry point, and the bytes themselves.
    struct SynthBin<'a> {
        rom_off: u32,
        ram: u32,
        entry: u32,
        bytes: &'a [u8],
    }

    /// Assemble a minimal but valid .nds header + two binaries into a ROM image.
    fn synth_cart(arm9: &SynthBin, arm7: &SynthBin) -> Vec<u8> {
        let total = (arm9.rom_off as usize + arm9.bytes.len())
            .max(arm7.rom_off as usize + arm7.bytes.len())
            .max(0x200);
        let mut rom = vec![0u8; total];
        rom[0x00..0x0B].copy_from_slice(b"SYNTHBOOT\0\0");
        rom[0x0C..0x10].copy_from_slice(b"ZZZE");
        rom[0x10..0x12].copy_from_slice(b"01");
        let w32 = |rom: &mut [u8], off: usize, v: u32| {
            rom[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        w32(&mut rom, 0x020, arm9.rom_off);
        w32(&mut rom, 0x024, arm9.entry);
        w32(&mut rom, 0x028, arm9.ram);
        w32(&mut rom, 0x02C, arm9.bytes.len() as u32);
        w32(&mut rom, 0x030, arm7.rom_off);
        w32(&mut rom, 0x034, arm7.entry);
        w32(&mut rom, 0x038, arm7.ram);
        w32(&mut rom, 0x03C, arm7.bytes.len() as u32);
        rom[arm9.rom_off as usize..arm9.rom_off as usize + arm9.bytes.len()]
            .copy_from_slice(arm9.bytes);
        rom[arm7.rom_off as usize..arm7.rom_off as usize + arm7.bytes.len()]
            .copy_from_slice(arm7.bytes);
        rom
    }

    #[test]
    fn hle_boot_lands_binaries_and_sets_entry_and_stacks() {
        let mut nds = Nds::new();
        let arm9: Vec<u8> = (0..64u8).map(|i| i.wrapping_add(0xA0)).collect();
        let arm7: Vec<u8> = (0..48u8).map(|i| i.wrapping_add(0x70)).collect();
        let rom = synth_cart(
            &SynthBin { rom_off: 0x4000, ram: 0x0200_0000, entry: 0x0200_0010, bytes: &arm9 },
            &SynthBin { rom_off: 0x8000, ram: 0x0380_0000, entry: 0x0380_0004, bytes: &arm7 },
        );

        nds.hle_boot(&rom);

        // ARM9 binary landed in Main RAM at 0x02000000.
        for (i, &b) in arm9.iter().enumerate() {
            assert_eq!(nds.read8_arm9(0x0200_0000 + i as u32), b as u32, "arm9 byte {i}");
        }
        // ARM7 binary landed in ARM7 IWRAM at 0x03800000.
        for (i, &b) in arm7.iter().enumerate() {
            assert_eq!(nds.mem.arm7_iwram[i], b, "arm7 byte {i}");
        }

        // Entry PCs set from the header.
        assert_eq!(nds.state9.r[15], 0x0200_0010);
        assert_eq!(nds.state7.r[15], 0x0380_0004);

        // Both cores land in SYS mode, ARM (not THUMB), running (not halted).
        assert_eq!(nds.state9.mode(), mode::SYS);
        assert_eq!(nds.state7.mode(), mode::SYS);
        assert_eq!(nds.state9.cpsr & FLAG_T, 0);
        assert!(!nds.state9.halted);
        assert!(!nds.state7.halted);

        // Post-BIOS stacks: SYS sp live, IRQ/SVC banked.
        assert_eq!(nds.state9.r[13], 0x0380_FF00); // SYS sp
        assert_eq!(nds.state9.bank_r13[2], 0x0380_FFA0); // IRQ bank
        assert_eq!(nds.state9.bank_r13[3], 0x0380_FFE0); // SVC bank
        assert_eq!(nds.state7.r[13], 0x0380_FF00);

        // POSTFLG = boot complete on both cores.
        assert_eq!(nds.postflg9, 1);
        assert_eq!(nds.postflg7, 1);

        // Cart got mounted (game code recoverable from the mounted ROM).
        assert!(nds.cart.is_some());

        // BIOS-RAM block stamped: chip-ID (Macronix 0xC2 low byte) at 0x027FF800.
        let chip_id = nds.read32_arm9(0x027F_F800);
        assert_eq!(chip_id & 0xFF, 0xC2);

        // IRQ-dispatch stub installed: ARM9 IRQ vector at 0x18 (backing array).
        let b = &nds.mem.bios_arm9;
        let irq_vec = (b[0x18] as u32)
            | ((b[0x19] as u32) << 8)
            | ((b[0x1A] as u32) << 16)
            | ((b[0x1B] as u32) << 24);
        assert_eq!(irq_vec, 0xE92D_500F);
    }

    #[test]
    fn hle_boot_rejects_too_small_rom_without_panicking() {
        let mut nds = Nds::new();
        // A 100-byte image is shorter than the 512-byte header → no-op.
        nds.hle_boot(&[0u8; 100]);
        // No cart mounted, entry PCs untouched from a fresh reset.
        assert!(nds.cart.is_none());
    }

    #[test]
    fn hle_boot_then_swis_execute_against_loaded_state() {
        // Confirm Div/Sqrt/CpuFastSet/LZ77 still produce correct results after a
        // real boot has set up the register files + RAM.
        let mut nds = Nds::new();
        let arm9 = [0u8; 16];
        let arm7 = [0u8; 16];
        let rom = synth_cart(
            &SynthBin { rom_off: 0x4000, ram: 0x0200_0000, entry: 0x0200_0000, bytes: &arm9 },
            &SynthBin { rom_off: 0x8000, ram: 0x0380_0000, entry: 0x0380_0000, bytes: &arm7 },
        );
        nds.hle_boot(&rom);

        // Div: 1000 / 7 → q=142, r=6.
        nds.state9.r[0] = 1000;
        nds.state9.r[1] = 7;
        assert!(nds.bios_swi(true, 0x09));
        assert_eq!(nds.state9.r[0], 142);
        assert_eq!(nds.state9.r[1], 6);

        // Sqrt: floor(sqrt(1000)) = 31.
        nds.state9.r[0] = 1000;
        assert!(nds.bios_swi(true, 0x0D));
        assert_eq!(nds.state9.r[0], 31);

        // CpuFastSet: fill 8 words with a fixed source.
        nds.write32_arm9(0x0200_2000, 0x1234_5678);
        nds.state9.r[0] = 0x0200_2000;
        nds.state9.r[1] = 0x0200_3000;
        nds.state9.r[2] = 8 | 0x0100_0000; // 8 words, fixed src
        assert!(nds.bios_swi(true, 0x0C));
        for i in 0..8u32 {
            assert_eq!(nds.read32_arm9(0x0200_3000 + i * 4), 0x1234_5678);
        }

        // LZ77: 'Z' literal then backref len=3 disp=1 → "ZZZZ".
        let src = 0x0200_4000u32;
        let dst = 0x0200_5000u32;
        nds.write32_arm9(src, (4 << 8) | 0x10);
        nds.write8_arm9(src + 4, 0x40); // flags: literal, then backref
        nds.write8_arm9(src + 5, 0x5A); // 'Z'
        nds.write8_arm9(src + 6, 0x00);
        nds.write8_arm9(src + 7, 0x00);
        nds.state9.r[0] = src;
        nds.state9.r[1] = dst;
        assert!(nds.bios_swi(true, 0x11));
        for i in 0..4u32 {
            assert_eq!(nds.read8_arm9(dst + i), 0x5A);
        }
    }
}
