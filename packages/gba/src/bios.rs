//! High-level emulation of GBA BIOS syscalls. Ported 1:1 from
//! src/bios/hle.ts.
//!
//! `handle_swi` returns true if we already handled the syscall; otherwise the
//! CPU falls through to the normal SVC vector entry.
//!
//! Ownership (per the Rust port contract): the TS `BiosHle` constructor took
//! `cpu` and `bus`. We do NOT store them — the entry points take them as
//! `&mut` PARAMETERS. The TS class also reached through `this.cpu.bus.io` to
//! poke IRQ / SIO / Sound / PPU registers directly; in Rust those are not
//! reachable from `&mut dyn Bus`, so every such poke is routed through the
//! Bus IO window (`bus.write16(0x04000000 + offset, v)`), which dispatches to
//! the same devices. The behaviour is identical:
//!   - IF (0x04000202) write-1-to-clear == `irq.iflag &= ~bits`
//!   - IME (0x04000208) write == `irq.ime = v & 1`
//!   - PPU affine PA/PB/PC/PD live at IO offsets 0x20/0x22/0x24/0x26 (BG2)
//!     and 0x30/0x32/0x34/0x36 (BG3).
//! Raw-memory region clears (TS `bus.ewram.fill(0)` etc.) are done with
//! `bus.write32` loops, which `Mem` routes back into the same regions.

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::irq::IRQ_VBLANK;

// Absolute base of the IO register window.
const IO_BASE: u32 = 0x0400_0000;
// Absolute bases of the raw memory regions (for RegisterRamReset clears).
const EWRAM_BASE: u32 = 0x0200_0000;
const IWRAM_BASE: u32 = 0x0300_0000;
const PRAM_BASE: u32 = 0x0500_0000;
const VRAM_BASE: u32 = 0x0600_0000;
const OAM_BASE: u32 = 0x0700_0000;

/// JS `Math.round` rounds half toward +Infinity (i.e. `floor(x + 0.5)`),
/// which differs from Rust's `f64::round` (half away from zero) for negative
/// `.5` cases. The BIOS affine math relies on JS semantics, so replicate it.
#[inline]
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

#[derive(Default)]
pub struct BiosHle {}

impl BiosHle {
    pub fn new() -> Self {
        BiosHle {}
    }

    pub fn handle_swi(&mut self, comment: u32, cpu: &mut Cpu, bus: &mut dyn Bus) -> bool {
        match comment {
            0x00 => {
                self.soft_reset(cpu, bus);
                return true;
            }
            0x01 => {
                let mask = cpu.state.r[0];
                self.register_ram_reset(mask, bus);
                return true;
            }
            0x02 => {
                cpu.halt();
                return true;
            }
            0x03 => {
                cpu.halt();
                return true;
            }
            0x04 => {
                let discard_old = cpu.state.r[0];
                let wanted = cpu.state.r[1];
                self.intr_wait(discard_old, wanted, cpu, bus);
                return true;
            }
            0x05 => {
                self.v_blank_intr_wait(cpu, bus);
                return true;
            }
            0x06 => {
                self.div(cpu);
                return true;
            }
            0x07 => {
                self.div_arm(cpu);
                return true;
            }
            0x08 => {
                self.sqrt(cpu);
                return true;
            }
            0x09 => {
                self.arc_tan(cpu);
                return true;
            }
            0x0A => {
                self.arc_tan2(cpu);
                return true;
            }
            0x0B => {
                self.cpu_set(cpu, bus);
                return true;
            }
            0x0C => {
                self.cpu_fast_set(cpu, bus);
                return true;
            }
            0x0D => {
                cpu.state.r[0] = 0xBAAE_187F;
                return true;
            } // BiosChecksum
            0x0E => {
                self.bg_affine_set(cpu, bus);
                return true;
            }
            0x0F => {
                self.obj_affine_set(cpu, bus);
                return true;
            }
            0x10 => {
                self.bit_un_pack(cpu, bus);
                return true;
            }
            0x11 => {
                self.lz77_un_comp(false, cpu, bus);
                return true;
            }
            0x12 => {
                self.lz77_un_comp(true, cpu, bus);
                return true;
            }
            0x13 => {
                self.huff_un_comp(cpu, bus);
                return true;
            }
            0x14 => {
                self.rl_un_comp(false, cpu, bus);
                return true;
            }
            0x15 => {
                self.rl_un_comp(true, cpu, bus);
                return true;
            }
            0x16 => {
                self.diff8(false, cpu, bus);
                return true;
            }
            0x17 => {
                self.diff8(true, cpu, bus);
                return true;
            }
            0x18 => {
                self.diff16(cpu, bus);
                return true;
            }
            0x19 => return true, // SoundBias
            0x1A | 0x1B | 0x1C | 0x1D | 0x1E | 0x1F | 0x26 => {
                return true; // sound drivers — silent stub
            }
            0x25 => {
                self.multi_boot(cpu, bus);
                return true;
            }
            _ => {}
        }
        // SWI numbers outside 0x00-0x2A are not defined by the GBA BIOS;
        // it dispatches them through a fixed-size jump table and effectively
        // returns immediately for out-of-range numbers. Some games (e.g. Doom
        // II via SWIEQ #0x890000) hit these as conditional no-ops the BIOS
        // is expected to swallow. Falling through to the SVC vector here
        // would otherwise land on our BIOS infinite-loop stub and hang.
        if comment > 0x2A {
            return true;
        }
        false
    }

    // -------- Reset / RAM clear --------
    // SWI 0x25 — MultiBoot. r0 = pointer to a MultiBootParam struct, r1 =
    // transfer mode (0/2 = Normal-32, 1 = Multi-play-16). On hardware this ships
    // the cartridge's multiboot image to every connected child and returns r0=0
    // on success, r0=1 on failure.
    //
    // We are a single unit with no child attached over the link transport, so a
    // real transfer can't complete: we parse and validate the parameter block
    // (exercising the multiboot crypto/length checks) and report failure (r0=1),
    // which is the faithful "no slaves responded" result. Games take their
    // transfer-failed path instead of hanging on an undefined return. The
    // encryption/CRC primitives in `crate::multiboot` are ready for the day a
    // child peer is wired over the link transport.
    fn multi_boot(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let param_ptr = cpu.state.r[0];
        let mode = crate::multiboot::Mode::from_swi_arg(cpu.state.r[1]);
        // Pull the 0x28-byte parameter block out of GBA memory.
        let mut raw = [0u8; 0x28];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = bus.read8(param_ptr.wrapping_add(i as u32)) as u8;
        }
        // Validate; an out-of-range payload length is itself a failure. (`mode`
        // and the parsed params would seed `multiboot::Session` for the actual
        // encrypted transfer once a child peer exists.)
        let ok = crate::multiboot::MultiBootParam::parse(&raw)
            .and_then(|p| p.payload_len())
            .is_some();
        let _ = (mode, ok);
        // No child responded → failure.
        cpu.state.r[0] = 1;
    }

    fn soft_reset(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &mut cpu.state;
        // BIOS soft reset reads flag from 0x03007FFA: 0 = ROM, !=0 = EWRAM entry.
        let flag = bus.read8(0x0300_7FFA);
        s.r[0] = 0;
        s.r[1] = 0;
        s.r[2] = 0;
        s.r[3] = 0;
        s.r[4] = 0;
        s.r[5] = 0;
        s.r[6] = 0;
        s.r[7] = 0;
        s.r[13] = 0x0300_7F00;
        s.cpsr = 0x1F; // SYS mode, F/I clear, ARM
        s.r[15] = if flag != 0 { 0x0200_0000 } else { 0x0800_0000 };
        cpu.flush_pipeline();
    }

    fn register_ram_reset(&mut self, mask: u32, bus: &mut dyn Bus) {
        if mask & 0x01 != 0 {
            self.fill32(bus, EWRAM_BASE, crate::regions::EWRAM_SIZE as u32, 0);
        }
        if mask & 0x02 != 0 {
            self.fill32(bus, IWRAM_BASE, 0x7E00, 0); // BIOS leaves stack area
        }
        if mask & 0x04 != 0 {
            self.fill32(bus, PRAM_BASE, crate::regions::PRAM_SIZE as u32, 0);
        }
        if mask & 0x08 != 0 {
            self.fill32(bus, VRAM_BASE, crate::regions::VRAM_SIZE as u32, 0);
        }
        if mask & 0x10 != 0 {
            self.fill32(bus, OAM_BASE, crate::regions::OAM_SIZE as u32, 0);
        }
        // bit 5 — SIO regs. The real BIOS clears SIODATA0..3, SIOCNT, JOYCNT,
        // etc. and flips RCNT into "general purpose" mode (0x8000). Games
        // (Doom II's RegisterRamReset(0xFD) retry path is the trigger we saw)
        // expect this baseline; without it RCNT lingers at 0 = serial mode
        // and the game's link-cable probe never disengages.
        if mask & 0x20 != 0 {
            // 0x120-0x12C: SIODATA / SIOMULTI / SIODATA8
            let mut a = 0x120u32;
            while a <= 0x12C {
                bus.write16(IO_BASE + a, 0);
                a += 2;
            }
            bus.write16(IO_BASE + 0x128, 0); // SIOCNT
            bus.write16(IO_BASE + 0x134, 0x8000); // RCNT — general purpose
            bus.write16(IO_BASE + 0x140, 0); // JOYCNT
            bus.write16(IO_BASE + 0x150, 0);
            bus.write16(IO_BASE + 0x152, 0); // JOY_RECV
            bus.write16(IO_BASE + 0x154, 0);
            bus.write16(IO_BASE + 0x156, 0); // JOY_TRANS
            bus.write16(IO_BASE + 0x158, 0); // JOYSTAT
        }
        // bit 6 — Sound. Clear sound channels 1-4 + DirectSound control,
        // then re-enable master (SOUNDCNT_X = 0x80) and set SOUNDBIAS to
        // the BIOS default of 0x200.
        if mask & 0x40 != 0 {
            let mut a = 0x060u32;
            while a <= 0x0A6 {
                bus.write16(IO_BASE + a, 0);
                a += 2;
            }
            bus.write16(IO_BASE + 0x084, 0x0080); // SOUNDCNT_X master enable
            bus.write16(IO_BASE + 0x088, 0x0200); // SOUNDBIAS default
            // Wave RAM banks (0x90-0x9F) — clear both banks.
            // No external waveRam in our HLE-only sound module; the writes
            // above already covered the registers we expose.
        }
        // bit 7 — "everything else". GBATEK lists DISPSTAT, BG control/scroll,
        // BG2/3 affine, mosaic, window, blend, DMA, timer, IRQ, WAITCNT,
        // POSTFLG, HALTCNT. DISPCNT is documented to get force-blank (bit 7
        // set, all else 0). The affine BG defaults (PA/PD = 0x100) we used
        // to set unconditionally are part of this bit's contract — Pokemon
        // FireRed's Oak intro relies on them.
        if mask & 0x80 != 0 {
            bus.write16(IO_BASE + 0x000, 0x0080); // DISPCNT force blank
            let mut a = 0x004u32;
            while a <= 0x056 {
                bus.write16(IO_BASE + a, 0);
                a += 2;
            }
            let mut a = 0x0B0u32;
            while a <= 0x0DE {
                bus.write16(IO_BASE + a, 0);
                a += 2;
            }
            let mut a = 0x100u32;
            while a <= 0x10E {
                bus.write16(IO_BASE + a, 0);
                a += 2;
            }
            bus.write16(IO_BASE + 0x200, 0); // IE
            bus.write16(IO_BASE + 0x202, 0xFFFF); // IF (write 1s to clear)
            bus.write16(IO_BASE + 0x204, 0); // WAITCNT
            bus.write16(IO_BASE + 0x208, 0); // IME
            // BG2 identity / BG3 identity. In TS these set ppu.bgPA/bgPD/bgPB/
            // bgPC directly; here we route through the affine IO registers
            // (offset 0x20-0x26 for BG2, 0x30-0x36 for BG3). The loop above
            // already zeroed them; we now restore the identity matrix.
            bus.write16(IO_BASE + 0x020, 0x100); // bgPA[0]
            bus.write16(IO_BASE + 0x026, 0x100); // bgPD[0]
            bus.write16(IO_BASE + 0x030, 0x100); // bgPA[1]
            bus.write16(IO_BASE + 0x036, 0x100); // bgPD[1]
            bus.write16(IO_BASE + 0x022, 0); // bgPB[0]
            bus.write16(IO_BASE + 0x024, 0); // bgPC[0]
            bus.write16(IO_BASE + 0x032, 0); // bgPB[1]
            bus.write16(IO_BASE + 0x034, 0); // bgPC[1]
        }
    }

    /// Fill `len` bytes starting at `base` with `val` repeated, using 32-bit
    /// writes through the bus (mirrors TS `region.fill(0, 0, len)`).
    fn fill32(&self, bus: &mut dyn Bus, base: u32, len: u32, val: u32) {
        let mut a = base;
        let end = base.wrapping_add(len);
        while a < end {
            bus.write32(a, val);
            a = a.wrapping_add(4);
        }
    }

    // Public hook so emulator.loadRom() can run the same defaults at boot
    // even if the game never explicitly invokes RegisterRamReset(0x80).
    pub fn reset_affine_defaults(&mut self, bus: &mut dyn Bus) {
        // TS set ppu.bgPA[0]=ppu.bgPD[0]=ppu.bgPA[1]=ppu.bgPD[1]=0x100. We
        // route through the affine IO registers (BG2 PA/PD at 0x20/0x26,
        // BG3 PA/PD at 0x30/0x36).
        bus.write16(IO_BASE + 0x020, 0x100); // bgPA[0]
        bus.write16(IO_BASE + 0x026, 0x100); // bgPD[0]
        bus.write16(IO_BASE + 0x030, 0x100); // bgPA[1]
        bus.write16(IO_BASE + 0x036, 0x100); // bgPD[1]
    }

    // -------- Interrupt waits --------
    fn intr_wait(&mut self, discard_old: u32, wanted: u32, cpu: &mut Cpu, bus: &mut dyn Bus) {
        // TS reached irq directly: `if (discardOld) irq.iflag &= ~wanted;
        // irq.ime = 1;`. We route through IF (write-1-to-clear) and IME.
        if discard_old != 0 {
            bus.write16(IO_BASE + 0x202, wanted & 0xFFFF); // IF clears these bits
        }
        bus.write16(IO_BASE + 0x208, 1); // IME = 1
        cpu.halt();
        // CPU step loop will wake us on next matching IRQ. To make the
        // matching condition correct we leave the SWI to "return"; the
        // game's caller will recheck the flag if needed.
    }

    fn v_blank_intr_wait(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        // TS: `io.irq.iflag &= ~IRQ_VBLANK; io.irq.ime = 1;`
        bus.write16(IO_BASE + 0x202, IRQ_VBLANK & 0xFFFF); // IF clears VBLANK
        bus.write16(IO_BASE + 0x208, 1); // IME = 1
        cpu.halt();
    }

    // -------- Math --------
    fn div(&mut self, cpu: &mut Cpu) {
        let s = &mut cpu.state;
        let num = s.r[0] as i32;
        let den = s.r[1] as i32;
        if den == 0 {
            return;
        }
        // JS: (num / den) | 0 truncates toward zero.
        let quot = num.wrapping_div(den);
        s.r[0] = quot as u32;
        // num - (num/den|0)*den
        s.r[1] = (num.wrapping_sub(quot.wrapping_mul(den))) as u32;
        // Math.abs(s.r[0] | 0) >>> 0
        s.r[3] = (quot as i32).wrapping_abs() as u32;
    }

    fn div_arm(&mut self, cpu: &mut Cpu) {
        let s = &mut cpu.state;
        let a = s.r[0];
        s.r[0] = s.r[1];
        s.r[1] = a;
        self.div(cpu);
    }

    fn sqrt(&mut self, cpu: &mut Cpu) {
        let s = &mut cpu.state;
        // Math.floor(Math.sqrt(s.r[0] >>> 0)) >>> 0
        let v = (s.r[0] as f64).sqrt().floor();
        s.r[0] = v as u32;
    }

    fn arc_tan(&mut self, cpu: &mut Cpu) {
        let s = &mut cpu.state;
        // const tan = (s.r[0] << 16) >> 16; // signed q1.14
        let tan = ((s.r[0] << 16) as i32) >> 16;
        let a = ((tan as f64) / (0x4000 as f64)).atan();
        // s.r[0] = ((a * 0x8000) / Math.PI) >>> 0 & 0xFFFF;
        // JS `>>> 0` of a float truncates toward zero into u32, then & 0xFFFF.
        let v = ((a * (0x8000 as f64)) / std::f64::consts::PI) as i64 as u32;
        s.r[0] = v & 0xFFFF;
    }

    fn arc_tan2(&mut self, cpu: &mut Cpu) {
        let s = &mut cpu.state;
        let x = ((s.r[0] << 16) as i32) >> 16;
        let y = ((s.r[1] << 16) as i32) >> 16;
        let a = (y as f64).atan2(x as f64);
        // let v = Math.round((a * 0x8000) / Math.PI);
        let mut v = js_round((a * (0x8000 as f64)) / std::f64::consts::PI) as i64;
        if v < 0 {
            v += 0x10000;
        }
        s.r[0] = (v as u32) & 0xFFFF;
    }

    // -------- CPU memory ops --------
    fn cpu_set(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let len = s.r[2] & 0x1FFFFF;
        let fixed = (s.r[2] & 0x01000000) != 0;
        let word = (s.r[2] & 0x04000000) != 0;
        for _ in 0..len {
            if word {
                let v = bus.read32(src);
                bus.write32(dst, v);
                dst = dst.wrapping_add(4);
                if !fixed {
                    src = src.wrapping_add(4);
                }
            } else {
                let v = bus.read16(src);
                bus.write16(dst, v);
                dst = dst.wrapping_add(2);
                if !fixed {
                    src = src.wrapping_add(2);
                }
            }
        }
    }

    fn cpu_fast_set(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        // Word count is in R2[20:0], rounded up to the next multiple of 8 words.
        let mut words = ((s.r[2] & 0x1FFFFF) + 7) & !7;
        if words == 0 {
            words = 8;
        }
        let fixed = (s.r[2] & 0x01000000) != 0;
        for _ in 0..words {
            let v = bus.read32(src);
            bus.write32(dst, v);
            dst = dst.wrapping_add(4);
            if !fixed {
                src = src.wrapping_add(4);
            }
        }
    }

    // -------- Affine matrix helpers --------
    fn bg_affine_set(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let n = s.r[2] as i32;
        for _ in 0..n {
            let ox = bus.read32(src) as i32;
            let oy = bus.read32(src + 4) as i32;
            let dx = ((bus.read16(src + 8) << 16) as i32) >> 16;
            let dy = ((bus.read16(src + 10) << 16) as i32) >> 16;
            let sx = ((bus.read16(src + 12) << 16) as i32) >> 16;
            let sy = ((bus.read16(src + 14) << 16) as i32) >> 16;
            let ang = ((bus.read16(src + 16) >> 8) as f64) * 2.0 * std::f64::consts::PI / 256.0;
            src += 20;
            let cos = ang.cos();
            let sin = ang.sin();
            // pa/pb/pc/pd as JS `Math.round(...) & 0xFFFF` (u32 truncation).
            let pa = (js_round((sx as f64) * cos) as i64 as u32) & 0xFFFF;
            let pb = (js_round(-(sx as f64) * sin) as i64 as u32) & 0xFFFF;
            let pc = (js_round((sy as f64) * sin) as i64 as u32) & 0xFFFF;
            let pd = (js_round((sy as f64) * cos) as i64 as u32) & 0xFFFF;
            // startX = ox - dx*(pa|0) - dy*(pb|0); the (pa|0) is a sign-extend
            // of the 16-bit pa back to int32. pa/pb/pc/pd are 0..0xFFFF here,
            // so `| 0` (int32) keeps them as-is — JS multiplies them as 32-bit
            // ints. Replicate with i32 wrapping arithmetic.
            let start_x = (ox as i32)
                .wrapping_sub((dx as i32).wrapping_mul(pa as i32))
                .wrapping_sub((dy as i32).wrapping_mul(pb as i32));
            let start_y = (oy as i32)
                .wrapping_sub((dx as i32).wrapping_mul(pc as i32))
                .wrapping_sub((dy as i32).wrapping_mul(pd as i32));
            bus.write16(dst, pa);
            bus.write16(dst + 2, pb);
            bus.write16(dst + 4, pc);
            bus.write16(dst + 6, pd);
            bus.write32(dst + 8, start_x as u32);
            bus.write32(dst + 12, start_y as u32);
            dst += 16;
        }
    }

    fn obj_affine_set(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let n = s.r[2] as i32;
        let off = s.r[3] as i32 as u32; // dst stride, added as JS would (>>>0 wraps)
        for _ in 0..n {
            let sx = ((bus.read16(src) << 16) as i32) >> 16;
            let sy = ((bus.read16(src + 2) << 16) as i32) >> 16;
            let ang = ((bus.read16(src + 4) >> 8) as f64) * 2.0 * std::f64::consts::PI / 256.0;
            src += 8;
            let cos = ang.cos();
            let sin = ang.sin();
            bus.write16(dst, (js_round((sx as f64) * cos) as i64 as u32) & 0xFFFF);
            dst = dst.wrapping_add(off);
            bus.write16(dst, (js_round(-(sx as f64) * sin) as i64 as u32) & 0xFFFF);
            dst = dst.wrapping_add(off);
            bus.write16(dst, (js_round((sy as f64) * sin) as i64 as u32) & 0xFFFF);
            dst = dst.wrapping_add(off);
            bus.write16(dst, (js_round((sy as f64) * cos) as i64 as u32) & 0xFFFF);
            dst = dst.wrapping_add(off);
        }
    }

    // -------- BitUnPack --------
    fn bit_un_pack(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let src = s.r[0];
        let mut dst = s.r[1];
        let info = s.r[2];
        let src_len = bus.read16(info);
        let src_bits = bus.read8(info + 2);
        let dst_bits = bus.read8(info + 3);
        let offset_w = bus.read32(info + 4);
        let base = offset_w & 0x7FFFFFFF;
        let zero_off = (offset_w & 0x80000000) != 0;
        let mask = (1u32 << src_bits) - 1;
        let mut buffer: u32 = 0;
        let mut buf_bits: u32 = 0;
        for i in 0..src_len {
            let byte = bus.read8(src + i);
            let mut b = 0u32;
            while b < 8 {
                let chunk = (byte >> b) & mask;
                let mut out_val = 0u32;
                if chunk != 0 || zero_off {
                    out_val = (chunk + base) & ((1u32 << dst_bits) - 1);
                }
                buffer |= out_val << buf_bits;
                buf_bits += dst_bits;
                if buf_bits >= 32 {
                    bus.write32(dst, buffer);
                    dst = dst.wrapping_add(4);
                    buffer = 0;
                    buf_bits = 0;
                }
                b += src_bits;
            }
        }
        if buf_bits > 0 {
            bus.write32(dst, buffer);
        }
    }

    // -------- LZ77 --------
    fn lz77_un_comp(&mut self, vram: bool, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let header = bus.read32(src);
        let mut length = (header >> 8) as i64;
        src = src.wrapping_add(4);
        // VRAM mode requires halfword writes — we buffer pairs.
        let mut half_buf: u32 = 0;
        let mut half_buf_has: u32 = 0;
        // (writeByte is inlined as a closure-like helper via a macro/fn below.)

        while length > 0 {
            let mut flags = bus.read8(src);
            src = src.wrapping_add(1);
            let mut i = 0;
            while i < 8 && length > 0 {
                if flags & 0x80 != 0 {
                    let a = bus.read8(src);
                    src = src.wrapping_add(1);
                    let b = bus.read8(src);
                    src = src.wrapping_add(1);
                    let len = ((a >> 4) & 0xF) + 3;
                    let disp = (((a & 0xF) << 8) | b) + 1;
                    let mut k = 0;
                    while k < len && length > 0 {
                        let back = (dst.wrapping_add(half_buf_has)).wrapping_sub(disp);
                        // In VRAM mode, the current byte may be sitting in the
                        // halfword buffer (not yet flushed). Reading via bus.read
                        // would return stale memory; sense that case and pull from
                        // the buffer instead. This fixes sprites that LZ77-self-
                        // reference with disp=1 (every-other-byte-corrupt symptom).
                        let byte: u32;
                        if vram && half_buf_has == 1 && back == dst {
                            byte = half_buf;
                        } else {
                            byte = bus.read8(back);
                        }
                        Self::lz_write_byte(
                            vram,
                            bus,
                            &mut dst,
                            &mut half_buf,
                            &mut half_buf_has,
                            byte,
                        );
                        length -= 1;
                        k += 1;
                    }
                } else {
                    let byte = bus.read8(src);
                    src = src.wrapping_add(1);
                    Self::lz_write_byte(
                        vram,
                        bus,
                        &mut dst,
                        &mut half_buf,
                        &mut half_buf_has,
                        byte,
                    );
                    length -= 1;
                }
                flags <<= 1;
                i += 1;
            }
        }
        if half_buf_has != 0 {
            bus.write16(dst, half_buf);
        }
    }

    // Shared byte-emitter for the byte-oriented decompressors (LZ77/RLE/diff8).
    // Mirrors the TS local `writeByte` closure: in non-VRAM mode it writes a
    // byte and advances; in VRAM mode it buffers pairs into halfword writes.
    fn lz_write_byte(
        vram: bool,
        bus: &mut dyn Bus,
        dst: &mut u32,
        half_buf: &mut u32,
        half_buf_has: &mut u32,
        b: u32,
    ) {
        if !vram {
            bus.write8(*dst, b);
            *dst = dst.wrapping_add(1);
            return;
        }
        if *half_buf_has == 0 {
            *half_buf = b;
            *half_buf_has = 1;
        } else {
            bus.write16(*dst, *half_buf | (b << 8));
            *dst = dst.wrapping_add(2);
            *half_buf_has = 0;
        }
    }

    // -------- Huffman --------
    fn huff_un_comp(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let src = s.r[0];
        let mut dst = s.r[1];
        let header = bus.read32(src);
        // Header: bits 0-3 data size in bits (4 or 8), bits 4-7 type (2 =
        // Huffman), bits 8-31 decompressed size in bytes. If the type nibble
        // isn't Huffman, bail without touching the destination.
        if ((header >> 4) & 0xF) != 2 {
            return;
        }
        let data_size = header & 0xF;
        let mut remaining = (header >> 8) as i64;
        // Tree table: size byte at src+4; the tree occupies (treeSize+1)*2
        // bytes including that byte. Root node is the byte at src+5.
        let tree_size = bus.read8(src + 4);
        let root_addr = src.wrapping_add(5);
        let mut bit_src = src.wrapping_add(4).wrapping_add((tree_size + 1) * 2);
        let sym_mask = (1u32 << data_size) - 1;

        let mut node_addr = root_addr;
        let mut node_val = bus.read8(node_addr);
        let mut out_buf: u32 = 0;
        let mut out_bits: u32 = 0;
        while remaining > 0 {
            // Bitstream is consumed as 32-bit words, MSB first.
            let word = bus.read32(bit_src);
            bit_src = bit_src.wrapping_add(4);
            let mut b: i32 = 31;
            while b >= 0 && remaining > 0 {
                let bit = (word >> b) & 1;
                // Node byte: bits 0-5 offset; bit 6 = node1 is data; bit 7 =
                // node0 is data. Children pair lives at (nodeAddr&~1)+offset*2+2.
                let is_leaf = (node_val & (if bit != 0 { 0x40 } else { 0x80 })) != 0;
                let child_addr =
                    ((node_addr & !1).wrapping_add((node_val & 0x3F) * 2 + 2 + bit)) & 0xFFFF_FFFF;
                let child_val = bus.read8(child_addr);
                if is_leaf {
                    // Pack symbols LSB-first into 32-bit units; the BIOS writes
                    // the destination in word units only.
                    out_buf |= (child_val & sym_mask) << out_bits;
                    out_bits += data_size;
                    if out_bits >= 32 {
                        bus.write32(dst, out_buf);
                        dst = dst.wrapping_add(4);
                        remaining -= 4;
                        out_buf = 0;
                        out_bits = 0;
                    }
                    node_addr = root_addr;
                    node_val = bus.read8(root_addr);
                } else {
                    node_addr = child_addr;
                    node_val = child_val;
                }
                b -= 1;
            }
        }
    }

    // -------- Run-length --------
    fn rl_un_comp(&mut self, vram: bool, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let header = bus.read32(src);
        let mut length = (header >> 8) as i64;
        src = src.wrapping_add(4);
        let mut half_buf: u32 = 0;
        let mut half_buf_has: u32 = 0;
        while length > 0 {
            let flag = bus.read8(src);
            src = src.wrapping_add(1);
            if flag & 0x80 != 0 {
                let len = (flag & 0x7F) + 3;
                let byte = bus.read8(src);
                src = src.wrapping_add(1);
                let mut i = 0;
                while i < len && length > 0 {
                    Self::lz_write_byte(
                        vram,
                        bus,
                        &mut dst,
                        &mut half_buf,
                        &mut half_buf_has,
                        byte,
                    );
                    length -= 1;
                    i += 1;
                }
            } else {
                let len = (flag & 0x7F) + 1;
                let mut i = 0;
                while i < len && length > 0 {
                    let byte = bus.read8(src);
                    src = src.wrapping_add(1);
                    Self::lz_write_byte(
                        vram,
                        bus,
                        &mut dst,
                        &mut half_buf,
                        &mut half_buf_has,
                        byte,
                    );
                    length -= 1;
                    i += 1;
                }
            }
        }
        if half_buf_has != 0 {
            bus.write16(dst, half_buf);
        }
    }

    // -------- Diff-Filter (8 / 16) --------
    fn diff8(&mut self, vram: bool, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let header = bus.read32(src);
        let mut length = (header >> 8) as i64;
        src = src.wrapping_add(4);
        let mut prev = bus.read8(src);
        src = src.wrapping_add(1);
        let mut half_buf: u32 = 0;
        let mut half_buf_has: u32 = 0;
        Self::lz_write_byte(
            vram,
            bus,
            &mut dst,
            &mut half_buf,
            &mut half_buf_has,
            prev,
        );
        length -= 1;
        while length > 0 {
            let d = bus.read8(src);
            src = src.wrapping_add(1);
            prev = (prev + d) & 0xFF;
            Self::lz_write_byte(
                vram,
                bus,
                &mut dst,
                &mut half_buf,
                &mut half_buf_has,
                prev,
            );
            length -= 1;
        }
        if half_buf_has != 0 {
            bus.write16(dst, half_buf);
        }
    }

    fn diff16(&mut self, cpu: &mut Cpu, bus: &mut dyn Bus) {
        let s = &cpu.state;
        let mut src = s.r[0];
        let mut dst = s.r[1];
        let header = bus.read32(src);
        let mut length = ((header >> 8) >> 1) as i64; // in halfwords
        src = src.wrapping_add(4);
        let mut prev = bus.read16(src);
        src = src.wrapping_add(2);
        bus.write16(dst, prev);
        dst = dst.wrapping_add(2);
        length -= 1;
        while length > 0 {
            let d = bus.read16(src);
            src = src.wrapping_add(2);
            prev = (prev + d) & 0xFFFF;
            bus.write16(dst, prev);
            dst = dst.wrapping_add(2);
            length -= 1;
        }
    }
}
