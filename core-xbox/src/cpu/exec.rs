//! IA-32 (x86) instruction decode + execution — a starter interpreter.
//!
//! Built from scratch against the Intel IA-32 SDM Vol. 2 (instruction set). The
//! executor is a plain interpreter: each [`Cpu::step`] consumes any legacy
//! prefixes, fetches the opcode at CS:EIP, decodes the ModR/M + SIB + immediate
//! operands (16- and 32-bit addressing), executes, and advances EIP. x86 is a
//! variable-length, **little-endian** CISC ISA, so unlike the fixed-width
//! PowerPC/MIPS cores the fetch length is data-dependent.
//!
//! # Coverage (this foundation)
//!
//! A meaningful *starter* slice of the integer ISA, enough to single-step real
//! BIOS/boot code a fair way before it needs an unimplemented feature:
//!
//! * the full 8-op ALU group (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) in all six
//!   encodings + the `0x80/0x81/0x83` immediate group,
//! * MOV in every common form (r/m↔r, imm→r/m, imm→reg, moffs, **Sreg**, and
//!   `mov CRn` so boot code can flip into protected mode), plus MOVZX/MOVSX,
//! * INC/DEC/NEG/NOT/TEST, XCHG, LEA, PUSH/POP (reg, imm, r/m, segment),
//! * the shift/rotate group (SHL/SHR/SAR/ROL/ROR),
//! * JMP (short/near/far), Jcc (short + near), SETcc, CALL/RET (near), LOOP,
//!   the flag ops (CLI/STI/CLD/STD/CLC/STC/CMC, PUSHF/POPF, SAHF/LAHF),
//! * HLT, NOP, CPUID, RDTSC, and MUL/DIV (unsigned, with #DE on divide-by-zero).
//!
//! Everything else decodes to the documented [`Decoded::Unimplemented`] seam,
//! which raises an #UD (invalid-opcode) exception — **never** a silent no-op.
//! Protected-mode descriptor loads, paging, privilege checks, and string/REP
//! ops are explicit seams for later phases.

use super::state::*;
use crate::bus::Bus;

// ---------------------------------------------------------------------------
// Branch-trace ring buffer (debug aid, gated on `XBOX_TRACE_BRANCH`).
//
// Records a window of recently-decided conditional branches (EIP, condition,
// taken/not-taken, EFLAGS, and the integer register file at the decision) so a
// reboot-loop diagnosis can dump the deciding Jcc that led to a reboot. See
// `xbox.rs`'s reboot path, which calls `dump_branch_trace()`.
// ---------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub struct BranchRec {
    pub eip: u32,
    pub cond: u8,
    pub taken: bool,
    pub eflags: u32,
    pub regs: [u32; 8],
}

static BRANCH_TRACE_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BRANCH_TRACE_INIT: std::sync::Once = std::sync::Once::new();
static BRANCH_RING: std::sync::Mutex<Vec<BranchRec>> = std::sync::Mutex::new(Vec::new());
const BRANCH_RING_CAP: usize = 128;

#[inline]
fn branch_trace_on() -> bool {
    BRANCH_TRACE_INIT.call_once(|| {
        if std::env::var_os("XBOX_TRACE_BRANCH").is_some() {
            BRANCH_TRACE_ON.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    BRANCH_TRACE_ON.load(std::sync::atomic::Ordering::Relaxed)
}

fn record_branch(rec: BranchRec) {
    let mut g = BRANCH_RING.lock().unwrap();
    if g.len() >= BRANCH_RING_CAP {
        g.remove(0);
    }
    g.push(rec);
}

/// Dump (and clear) the recorded branch ring buffer — called at the reboot seam.
pub fn dump_branch_trace() {
    let mut g = BRANCH_RING.lock().unwrap();
    eprintln!("[branch] --- last {} conditional branches ---", g.len());
    for r in g.iter() {
        let cc_name = COND_NAME[(r.cond & 0xF) as usize];
        eprintln!(
            "[branch] eip={:08X} j{:<3} {} ZF={} CF={} SF={} OF={} | eax={:08X} ecx={:08X} edx={:08X} ebx={:08X} esi={:08X} edi={:08X} ebp={:08X} esp={:08X}",
            r.eip,
            cc_name,
            if r.taken { "TAKEN" } else { "not  " },
            (r.eflags >> 6) & 1,
            r.eflags & 1,
            (r.eflags >> 7) & 1,
            (r.eflags >> 11) & 1,
            r.regs[0], r.regs[1], r.regs[2], r.regs[3], r.regs[6], r.regs[7], r.regs[5], r.regs[4],
        );
    }
    g.clear();
}

const COND_NAME: [&str; 16] = [
    "o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g",
];

/// Legacy instruction prefixes gathered before the opcode.
#[derive(Default, Clone, Copy)]
struct Prefixes {
    /// 0x66 — operand-size override.
    opsize: bool,
    /// 0x67 — address-size override.
    addrsize: bool,
    /// Segment-override prefix (2E/36/3E/26/64/65), if any.
    seg: Option<usize>,
    /// 0xF2/0xF3 — REP/REPNE (recorded; string ops are a future seam).
    rep: u8,
}

/// A decoded ModR/M operand: either a register encoding or a resolved linear
/// memory address (plus the raw effective offset, which `LEA` wants).
#[derive(Clone, Copy)]
enum Ea {
    Reg(u8),
    Mem { lin: u32, off: u32 },
}

/// The eight ALU sub-operations selected by the high opcode bits / group-1 reg
/// field, in their x86 numeric order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Alu {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Cmp,
}

/// The four bit-test sub-operations (BT/BTS/BTR/BTC).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BitOp {
    Test,
    Set,
    Reset,
    Comp,
}

const ALU_BY_INDEX: [Alu; 8] = [
    Alu::Add,
    Alu::Or,
    Alu::Adc,
    Alu::Sbb,
    Alu::And,
    Alu::Sub,
    Alu::Xor,
    Alu::Cmp,
];

/// Marker for the outcome of dispatch — purely for documentation/tests of the
/// decode boundary. The interpreter itself executes inline; this names what a
/// byte decoded to (mirrors the GC core's `Decoded` seam enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Alu,
    Mov,
    Stack,
    IncDec,
    Shift,
    Branch,
    Flags,
    System,
    Nop,
    /// Any opcode not yet handled by this foundation (raises #UD).
    Unimplemented,
}

impl Cpu {
    // ============================ fetch ============================
    /// Linear address of a code offset within CS (real mode masks the offset to
    /// 16 bits — IP wraps inside the 64 KB segment).
    #[inline]
    fn code_linear(&self, off: u32) -> u32 {
        let off = if self.real_mode() { off & 0xFFFF } else { off };
        self.seg_base[CS].wrapping_add(off)
    }

    #[inline]
    fn fetch_u8(&mut self, bus: &mut impl Bus) -> u8 {
        let b = bus.fetch8(self.code_linear(self.eip));
        self.eip = self.eip.wrapping_add(1);
        if self.real_mode() {
            self.eip &= 0xFFFF;
        }
        b
    }
    #[inline]
    fn fetch_u16(&mut self, bus: &mut impl Bus) -> u32 {
        let lo = self.fetch_u8(bus) as u32;
        let hi = self.fetch_u8(bus) as u32;
        lo | (hi << 8)
    }
    #[inline]
    fn fetch_u32(&mut self, bus: &mut impl Bus) -> u32 {
        let b0 = self.fetch_u8(bus) as u32;
        let b1 = self.fetch_u8(bus) as u32;
        let b2 = self.fetch_u8(bus) as u32;
        let b3 = self.fetch_u8(bus) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }
    /// Fetch an 8-bit immediate, sign-extended to 32 bits.
    #[inline]
    fn fetch_i8(&mut self, bus: &mut impl Bus) -> u32 {
        self.fetch_u8(bus) as i8 as i32 as u32
    }
    /// Fetch an operand-size immediate (zero-extended).
    #[inline]
    fn fetch_imm(&mut self, bus: &mut impl Bus, size: u8) -> u32 {
        match size {
            1 => self.fetch_u8(bus) as u32,
            2 => self.fetch_u16(bus),
            _ => self.fetch_u32(bus),
        }
    }

    // ============================ memory ============================
    #[inline]
    fn read_mem(&mut self, bus: &mut impl Bus, lin: u32, size: u8) -> u32 {
        match size {
            1 => bus.read8(lin),
            2 => bus.read16(lin),
            _ => bus.read32(lin),
        }
    }
    #[inline]
    fn write_mem(&mut self, bus: &mut impl Bus, lin: u32, size: u8, v: u32) {
        match size {
            1 => bus.write8(lin, v),
            2 => bus.write16(lin, v),
            _ => bus.write32(lin, v),
        }
    }
    #[inline]
    fn read_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) -> u32 {
        match ea {
            Ea::Reg(r) => self.reg(r as usize, size),
            Ea::Mem { lin, .. } => self.read_mem(bus, lin, size),
        }
    }
    #[inline]
    fn write_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, v: u32) {
        match ea {
            Ea::Reg(r) => self.set_reg(r as usize, size, v),
            Ea::Mem { lin, .. } => self.write_mem(bus, lin, size, v),
        }
    }

    // ============================ ModR/M ============================
    /// Decode a ModR/M byte (and any SIB/displacement) into the reg field and an
    /// effective operand, honouring the address size and segment override.
    fn modrm(&mut self, bus: &mut impl Bus, p: &Prefixes, asize: u8) -> (u8, Ea) {
        let b = self.fetch_u8(bus);
        let md = b >> 6;
        let reg = (b >> 3) & 7;
        let rm = b & 7;
        if md == 3 {
            return (reg, Ea::Reg(rm));
        }
        let (off, seg_def) = if asize == 2 {
            self.ea16(bus, md, rm)
        } else {
            self.ea32(bus, md, rm)
        };
        let seg = p.seg.unwrap_or(seg_def);
        let lin = self.seg_base[seg].wrapping_add(off);
        (reg, Ea::Mem { lin, off })
    }

    /// 32-bit effective-address computation (with SIB). Returns (offset, default
    /// segment).
    fn ea32(&mut self, bus: &mut impl Bus, md: u8, rm: u8) -> (u32, usize) {
        let mut seg = DS;
        let off;
        if rm == 4 {
            let sib = self.fetch_u8(bus);
            let scale = sib >> 6;
            let index = (sib >> 3) & 7;
            let base = sib & 7;
            let mut addr = 0u32;
            if base == 5 && md == 0 {
                addr = addr.wrapping_add(self.fetch_u32(bus)); // disp32, no base
            } else {
                addr = addr.wrapping_add(self.reg32(base as usize));
                if base == 4 || base == 5 {
                    seg = SS; // ESP/EBP base defaults to the stack segment
                }
            }
            if index != 4 {
                addr = addr.wrapping_add(self.reg32(index as usize) << scale);
            }
            match md {
                1 => addr = addr.wrapping_add(self.fetch_i8(bus)),
                2 => addr = addr.wrapping_add(self.fetch_u32(bus)),
                _ => {}
            }
            off = addr;
        } else if rm == 5 && md == 0 {
            off = self.fetch_u32(bus); // disp32 absolute
        } else {
            let mut addr = self.reg32(rm as usize);
            if rm == 5 {
                seg = SS; // [EBP] defaults to SS
            }
            match md {
                1 => addr = addr.wrapping_add(self.fetch_i8(bus)),
                2 => addr = addr.wrapping_add(self.fetch_u32(bus)),
                _ => {}
            }
            off = addr;
        }
        (off, seg)
    }

    /// 16-bit effective-address computation (the classic [bx+si] table).
    fn ea16(&mut self, bus: &mut impl Bus, md: u8, rm: u8) -> (u32, usize) {
        let mut seg = DS;
        let mut off = match rm {
            0 => self.reg16(EBX).wrapping_add(self.reg16(ESI)),
            1 => self.reg16(EBX).wrapping_add(self.reg16(EDI)),
            2 => {
                seg = SS;
                self.reg16(EBP).wrapping_add(self.reg16(ESI))
            }
            3 => {
                seg = SS;
                self.reg16(EBP).wrapping_add(self.reg16(EDI))
            }
            4 => self.reg16(ESI),
            5 => self.reg16(EDI),
            6 => {
                if md == 0 {
                    0 // disp16 absolute (filled below)
                } else {
                    seg = SS;
                    self.reg16(EBP)
                }
            }
            _ => self.reg16(EBX),
        };
        if rm == 6 && md == 0 {
            off = self.fetch_u16(bus);
        } else {
            match md {
                1 => off = off.wrapping_add(self.fetch_i8(bus)),
                2 => off = off.wrapping_add(self.fetch_u16(bus)),
                _ => {}
            }
        }
        (off & 0xFFFF, seg)
    }

    // ============================ stack ============================
    fn push(&mut self, bus: &mut impl Bus, v: u32, size: u8) {
        if self.real_mode() {
            let sp = self.reg16(ESP).wrapping_sub(size as u32) & 0xFFFF;
            self.set_reg16(ESP, sp);
            let lin = self.seg_base[SS].wrapping_add(sp);
            self.write_mem(bus, lin, size, v);
        } else {
            let esp = self.reg32(ESP).wrapping_sub(size as u32);
            self.set_reg32(ESP, esp);
            let lin = self.seg_base[SS].wrapping_add(esp);
            self.write_mem(bus, lin, size, v);
        }
    }
    fn pop(&mut self, bus: &mut impl Bus, size: u8) -> u32 {
        if self.real_mode() {
            let sp = self.reg16(ESP);
            let lin = self.seg_base[SS].wrapping_add(sp);
            let v = self.read_mem(bus, lin, size);
            self.set_reg16(ESP, sp.wrapping_add(size as u32) & 0xFFFF);
            v
        } else {
            let esp = self.reg32(ESP);
            let lin = self.seg_base[SS].wrapping_add(esp);
            let v = self.read_mem(bus, lin, size);
            self.set_reg32(ESP, esp.wrapping_add(size as u32));
            v
        }
    }

    // ============================ string ops ============================
    /// Read the (E)CX iteration counter for the current address size.
    #[inline]
    fn str_count(&self, asize: u8) -> u32 {
        if asize == 2 { self.reg16(ECX) } else { self.reg32(ECX) }
    }
    #[inline]
    fn set_str_count(&mut self, asize: u8, v: u32) {
        if asize == 2 {
            self.set_reg16(ECX, v);
        } else {
            self.set_reg32(ECX, v);
        }
    }
    /// Read (E)SI / (E)DI honouring the address size.
    #[inline]
    fn str_idx(&self, reg: usize, asize: u8) -> u32 {
        if asize == 2 { self.reg16(reg) } else { self.reg32(reg) }
    }
    #[inline]
    fn set_str_idx(&mut self, reg: usize, asize: u8, v: u32) {
        if asize == 2 {
            self.set_reg16(reg, v);
        } else {
            self.set_reg32(reg, v);
        }
    }
    /// Advance (E)SI/(E)DI by the operand size, respecting the direction flag.
    #[inline]
    fn str_advance(&mut self, reg: usize, asize: u8, size: u8) {
        let cur = self.str_idx(reg, asize);
        let step = size as u32;
        let next = if self.flag(DF) {
            cur.wrapping_sub(step)
        } else {
            cur.wrapping_add(step)
        };
        self.set_str_idx(reg, asize, next);
    }
    /// Linear address from DS:(E)SI (source; segment override applies).
    #[inline]
    fn src_lin(&self, p: &Prefixes, asize: u8) -> u32 {
        let seg = p.seg.unwrap_or(DS);
        self.seg_base[seg].wrapping_add(self.str_idx(ESI, asize))
    }
    /// Linear address from ES:(E)DI (destination; never overridden).
    #[inline]
    fn dst_lin(&self, asize: u8) -> u32 {
        self.seg_base[ES].wrapping_add(self.str_idx(EDI, asize))
    }

    fn string_movs(&mut self, bus: &mut impl Bus, size: u8, asize: u8, p: &Prefixes) {
        // MOVS: ES:[EDI] <- DS:[ESI], advance both. REP repeats (E)CX times.
        let rep = p.rep != 0;
        let mut count = if rep { self.str_count(asize) } else { 1 };
        while count > 0 {
            if rep && self.str_count(asize) == 0 {
                break;
            }
            let s = self.src_lin(p, asize);
            let v = self.read_mem(bus, s, size);
            let d = self.dst_lin(asize);
            self.write_mem(bus, d, size, v);
            self.str_advance(ESI, asize, size);
            self.str_advance(EDI, asize, size);
            if rep {
                self.set_str_count(asize, self.str_count(asize).wrapping_sub(1));
            }
            count -= 1;
        }
    }

    fn string_stos(&mut self, bus: &mut impl Bus, size: u8, asize: u8, p: &Prefixes) {
        // STOS: ES:[EDI] <- AL/AX/EAX, advance EDI.
        let rep = p.rep != 0;
        let mut count = if rep { self.str_count(asize) } else { 1 };
        let v = self.reg(EAX, size);
        while count > 0 {
            let d = self.dst_lin(asize);
            self.write_mem(bus, d, size, v);
            self.str_advance(EDI, asize, size);
            if rep {
                self.set_str_count(asize, self.str_count(asize).wrapping_sub(1));
            }
            count -= 1;
        }
    }

    fn string_lods(&mut self, bus: &mut impl Bus, size: u8, asize: u8, p: &Prefixes) {
        // LODS: AL/AX/EAX <- DS:[ESI], advance ESI.
        let rep = p.rep != 0;
        let mut count = if rep { self.str_count(asize) } else { 1 };
        while count > 0 {
            let s = self.src_lin(p, asize);
            let v = self.read_mem(bus, s, size);
            self.set_reg(EAX, size, v);
            self.str_advance(ESI, asize, size);
            if rep {
                self.set_str_count(asize, self.str_count(asize).wrapping_sub(1));
            }
            count -= 1;
        }
    }

    fn string_scas(&mut self, bus: &mut impl Bus, size: u8, asize: u8, p: &Prefixes) {
        // SCAS: compare AL/AX/EAX with ES:[EDI], advance EDI, set flags.
        // REPE (F3) repeats while ZF=1, REPNE (F2) while ZF=0; both stop at CX=0.
        let acc = self.reg(EAX, size);
        if p.rep == 0 {
            let d = self.dst_lin(asize);
            let v = self.read_mem(bus, d, size);
            self.flags_sub(acc, v, size);
            self.str_advance(EDI, asize, size);
            return;
        }
        let want_zf = p.rep == 0xF3; // REPE/REPZ
        while self.str_count(asize) != 0 {
            let d = self.dst_lin(asize);
            let v = self.read_mem(bus, d, size);
            self.flags_sub(acc, v, size);
            self.str_advance(EDI, asize, size);
            self.set_str_count(asize, self.str_count(asize).wrapping_sub(1));
            if self.flag(ZF) != want_zf {
                break;
            }
        }
    }

    fn string_cmps(&mut self, bus: &mut impl Bus, size: u8, asize: u8, p: &Prefixes) {
        // CMPS: compare DS:[ESI] with ES:[EDI] (sets flags as SI - DI? Intel:
        // compares [ESI] to [EDI], i.e. SUB src,dst), advance both.
        if p.rep == 0 {
            let s = self.src_lin(p, asize);
            let a = self.read_mem(bus, s, size);
            let d = self.dst_lin(asize);
            let b = self.read_mem(bus, d, size);
            self.flags_sub(a, b, size);
            self.str_advance(ESI, asize, size);
            self.str_advance(EDI, asize, size);
            return;
        }
        let want_zf = p.rep == 0xF3; // REPE/REPZ
        while self.str_count(asize) != 0 {
            let s = self.src_lin(p, asize);
            let a = self.read_mem(bus, s, size);
            let d = self.dst_lin(asize);
            let b = self.read_mem(bus, d, size);
            self.flags_sub(a, b, size);
            self.str_advance(ESI, asize, size);
            self.str_advance(EDI, asize, size);
            self.set_str_count(asize, self.str_count(asize).wrapping_sub(1));
            if self.flag(ZF) != want_zf {
                break;
            }
        }
    }

    // ============================ branch helpers ============================
    /// Set EIP, masking to 16 bits in real mode.
    #[inline]
    fn set_eip(&mut self, v: u32) {
        self.eip = if self.real_mode() { v & 0xFFFF } else { v };
    }
    /// Take a relative branch (`disp` already sign-extended) from the current
    /// (post-instruction) EIP.
    #[inline]
    fn jump_rel(&mut self, disp: u32) {
        let t = self.eip.wrapping_add(disp);
        self.set_eip(t);
    }

    /// Evaluate an x86 condition code (the low nibble of a Jcc/SETcc opcode).
    fn cc(&self, c: u8) -> bool {
        let f = |m: u32| self.flag(m);
        match c & 0xF {
            0x0 => f(OF),
            0x1 => !f(OF),
            0x2 => f(CF),
            0x3 => !f(CF),
            0x4 => f(ZF),
            0x5 => !f(ZF),
            0x6 => f(CF) || f(ZF),
            0x7 => !f(CF) && !f(ZF),
            0x8 => f(SF),
            0x9 => !f(SF),
            0xA => f(PF),
            0xB => !f(PF),
            0xC => f(SF) != f(OF),
            0xD => f(SF) == f(OF),
            0xE => f(ZF) || (f(SF) != f(OF)),
            _ => !f(ZF) && (f(SF) == f(OF)),
        }
    }

    // ============================ ALU ============================
    /// Apply an ALU op, returning (result, should-write-back).
    fn alu(&mut self, op: Alu, a: u32, b: u32, size: u8) -> (u32, bool) {
        match op {
            Alu::Add => (self.flags_add(a, b, size), true),
            Alu::Sub => (self.flags_sub(a, b, size), true),
            Alu::Cmp => (self.flags_sub(a, b, size), false),
            Alu::And => (self.flags_logic(a & b, size), true),
            Alu::Or => (self.flags_logic(a | b, size), true),
            Alu::Xor => (self.flags_logic(a ^ b, size), true),
            Alu::Adc => (self.flags_adc(a, b, size), true),
            Alu::Sbb => (self.flags_sbb(a, b, size), true),
        }
    }

    /// ADD-with-carry flags (`a + b + CF`).
    fn flags_adc(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Cpu::size_mask(size);
        let cf = self.flag(CF) as u64;
        let sum = (a & m) as u64 + (b & m) as u64 + cf;
        let res = (sum as u32) & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, res == 0);
        self.set_flag(SF, res & sign != 0);
        self.set_flag(PF, (res as u8).count_ones() % 2 == 0);
        self.set_flag(CF, sum > m as u64);
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(OF, ((!(a ^ b)) & (a ^ res) & sign) != 0);
        res
    }
    /// SUB-with-borrow flags (`a - b - CF`).
    fn flags_sbb(&mut self, a: u32, b: u32, size: u8) -> u32 {
        let m = Cpu::size_mask(size);
        let cf = self.flag(CF) as i64;
        let diff = (a & m) as i64 - (b & m) as i64 - cf;
        let res = (diff as u32) & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, res == 0);
        self.set_flag(SF, res & sign != 0);
        self.set_flag(PF, (res as u8).count_ones() % 2 == 0);
        self.set_flag(CF, diff < 0);
        self.set_flag(AF, ((a ^ b ^ res) & 0x10) != 0);
        self.set_flag(OF, ((a ^ b) & (a ^ res) & sign) != 0);
        res
    }

    // ============================ step ============================
    /// Execute one instruction. Consume prefixes, fetch + decode the opcode,
    /// execute, and advance EIP. Faults/HLT freeze the core (checked first).
    pub fn step(&mut self, bus: &mut impl Bus) {
        if self.halted || self.fault.is_some() {
            return;
        }
        let start_eip = self.eip;

        // ---- legacy prefixes ----
        let mut p = Prefixes::default();
        let mut op;
        let mut guard = 0;
        loop {
            op = self.fetch_u8(bus);
            match op {
                0x66 => p.opsize = true,
                0x67 => p.addrsize = true,
                0x2E => p.seg = Some(CS),
                0x36 => p.seg = Some(SS),
                0x3E => p.seg = Some(DS),
                0x26 => p.seg = Some(ES),
                0x64 => p.seg = Some(FS),
                0x65 => p.seg = Some(GS),
                0xF0 => {} // LOCK — no-op for a single-threaded interpreter
                0xF2 | 0xF3 => p.rep = op,
                _ => break,
            }
            guard += 1;
            if guard > 15 {
                self.eip = start_eip;
                self.raise(Exception::GeneralProtection, 0, op);
                return;
            }
        }

        // Operand / address size after the override prefixes.
        let osize = match (self.default_opsize(), p.opsize) {
            (2, false) => 2,
            (2, true) => 4,
            (4, true) => 2,
            _ => 4,
        };
        let asize = match (self.default_opsize(), p.addrsize) {
            (2, false) => 2,
            (2, true) => 4,
            (4, true) => 2,
            _ => 4,
        };

        self.instret = self.instret.wrapping_add(1);

        // ---- ALU group (0x00..0x3F, low 3 bits < 6) ----
        if op < 0x40 && (op & 7) < 6 {
            let alu = ALU_BY_INDEX[(op >> 3) as usize];
            self.exec_alu_group(bus, op, alu, osize, asize, &p);
            return;
        }

        match op {
            // ---- INC/DEC reg (operand size) ----
            0x40..=0x47 => {
                let r = (op - 0x40) as usize;
                let v = self.flags_inc(self.reg(r, osize), osize);
                self.set_reg(r, osize, v);
            }
            0x48..=0x4F => {
                let r = (op - 0x48) as usize;
                let v = self.flags_dec(self.reg(r, osize), osize);
                self.set_reg(r, osize, v);
            }

            // ---- PUSH/POP reg ----
            0x50..=0x57 => {
                let v = self.reg((op - 0x50) as usize, osize);
                self.push(bus, v, osize);
            }
            0x58..=0x5F => {
                let v = self.pop(bus, osize);
                self.set_reg((op - 0x58) as usize, osize, v);
            }

            // ---- PUSH imm ----
            0x68 => {
                let v = self.fetch_imm(bus, osize);
                self.push(bus, v, osize);
            }
            0x6A => {
                let v = self.fetch_i8(bus);
                self.push(bus, v, osize);
            }

            // ---- IMUL r, r/m, imm (0x69 imm-z, 0x6B imm8 sign-extended) ----
            0x69 | 0x6B => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let src = sign_ext(self.read_ea(bus, ea, osize), osize) as i32 as i64;
                let imm = if op == 0x6B {
                    self.fetch_i8(bus) as i32 as i64
                } else {
                    sign_ext(self.fetch_imm(bus, osize), osize) as i32 as i64
                };
                let full = src * imm;
                let res = (full as u32) & Cpu::size_mask(osize);
                let trunc = sign_ext(res, osize) as i32 as i64;
                let of = trunc != full;
                self.set_reg(reg as usize, osize, res);
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }

            // ---- PUSHA / PUSHAD (0x60) ----
            0x60 => {
                let sp0 = self.reg(ESP, osize);
                for r in [EAX, ECX, EDX, EBX] {
                    let v = self.reg(r, osize);
                    self.push(bus, v, osize);
                }
                self.push(bus, sp0, osize); // original (E)SP
                for r in [EBP, ESI, EDI] {
                    let v = self.reg(r, osize);
                    self.push(bus, v, osize);
                }
            }
            // ---- POPA / POPAD (0x61) ----
            0x61 => {
                // pop order: EDI,ESI,EBP,(skip ESP),EBX,EDX,ECX,EAX
                for r in [EDI, ESI, EBP] {
                    let v = self.pop(bus, osize);
                    self.set_reg(r, osize, v);
                }
                let _esp = self.pop(bus, osize); // discard the saved (E)SP slot
                for r in [EBX, EDX, ECX, EAX] {
                    let v = self.pop(bus, osize);
                    self.set_reg(r, osize, v);
                }
            }

            // ---- PUSH/POP segment (one-byte forms) ----
            0x06 => {
                let v = self.seg_sel[ES] as u32;
                self.push(bus, v, osize);
            }
            0x0E => {
                let v = self.seg_sel[CS] as u32;
                self.push(bus, v, osize);
            }
            0x16 => {
                let v = self.seg_sel[SS] as u32;
                self.push(bus, v, osize);
            }
            0x1E => {
                let v = self.seg_sel[DS] as u32;
                self.push(bus, v, osize);
            }
            0x07 => {
                let v = self.pop(bus, osize);
                self.set_seg(ES, v as u16);
            }
            0x17 => {
                let v = self.pop(bus, osize);
                self.set_seg(SS, v as u16);
            }
            0x1F => {
                let v = self.pop(bus, osize);
                self.set_seg(DS, v as u16);
            }

            // ---- group 1: ALU r/m, imm (0x80/0x81/0x83) ----
            0x80 | 0x81 | 0x83 => {
                let size = if op == 0x80 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let imm = if op == 0x83 {
                    self.fetch_i8(bus) // sign-extended imm8
                } else {
                    self.fetch_imm(bus, size)
                };
                let a = self.read_ea(bus, ea, size);
                let (res, wr) = self.alu(ALU_BY_INDEX[reg as usize], a, imm, size);
                if wr {
                    self.write_ea(bus, ea, size, res);
                }
            }

            // ---- TEST r/m, r ----
            0x84 | 0x85 => {
                let size = if op == 0x84 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                self.flags_logic(a & b, size);
            }
            // ---- XCHG r/m, r ----
            0x86 | 0x87 => {
                let size = if op == 0x86 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                self.write_ea(bus, ea, size, b);
                self.set_reg(reg as usize, size, a);
            }

            // ---- MOV r/m, r and r, r/m ----
            0x88 | 0x89 => {
                let size = if op == 0x88 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.reg(reg as usize, size);
                self.write_ea(bus, ea, size, v);
            }
            0x8A | 0x8B => {
                let size = if op == 0x8A { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, size);
                self.set_reg(reg as usize, size, v);
            }
            // ---- MOV r/m16, Sreg  and  MOV Sreg, r/m16 ----
            0x8C => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.seg_sel[(reg & 7) as usize] as u32;
                self.write_ea(bus, ea, 2, v);
            }
            0x8E => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, 2);
                self.set_seg((reg & 7) as usize, v as u16);
            }
            // ---- LEA r, m ----
            0x8D => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                match ea {
                    Ea::Mem { off, .. } => self.set_reg(reg as usize, osize, off),
                    Ea::Reg(_) => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }
            // ---- POP r/m ----
            0x8F => {
                let (_reg, ea) = self.modrm(bus, &p, asize);
                let v = self.pop(bus, osize);
                self.write_ea(bus, ea, osize, v);
            }

            // ---- NOP / XCHG eAX, reg ----
            0x90 => { /* NOP (xchg eAX,eAX) */ }
            // FWAIT/WAIT — wait for pending x87 exceptions; none modeled, so nop.
            0x9B => {}
            0x91..=0x97 => {
                let r = (op - 0x90) as usize;
                let a = self.reg(EAX, osize);
                let b = self.reg(r, osize);
                self.set_reg(EAX, osize, b);
                self.set_reg(r, osize, a);
            }
            // CBW / CWDE — sign-extend AL->AX (osize 2) or AX->EAX (osize 4).
            0x98 => {
                if osize == 2 {
                    self.set_reg16(EAX, self.reg8(EAX) as i8 as i16 as u16 as u32);
                } else {
                    self.set_reg32(EAX, self.reg16(EAX) as i16 as i32 as u32);
                }
            }
            // CWD / CDQ — sign-extend AX->DX:AX (osize 2) or EAX->EDX:EAX (osize 4).
            0x99 => {
                if osize == 2 {
                    let s = if self.reg16(EAX) & 0x8000 != 0 { 0xFFFF } else { 0 };
                    self.set_reg16(EDX, s);
                } else {
                    let s = if self.reg32(EAX) & 0x8000_0000 != 0 { 0xFFFF_FFFF } else { 0 };
                    self.set_reg32(EDX, s);
                }
            }

            // ---- MOV moffs (AL/eAX ↔ [disp]) ----
            0xA0 | 0xA1 | 0xA2 | 0xA3 => {
                let size = if op & 1 == 0 { 1 } else { osize };
                let off = if asize == 2 {
                    self.fetch_u16(bus)
                } else {
                    self.fetch_u32(bus)
                };
                let seg = p.seg.unwrap_or(DS);
                let lin = self.seg_base[seg].wrapping_add(off);
                if op <= 0xA1 {
                    let v = self.read_mem(bus, lin, size);
                    self.set_reg(EAX, size, v);
                } else {
                    let v = self.reg(EAX, size);
                    self.write_mem(bus, lin, size, v);
                }
            }
            // ---- TEST AL/eAX, imm ----
            0xA8 => {
                let imm = self.fetch_u8(bus) as u32;
                let a = self.reg8(EAX);
                self.flags_logic(a & imm, 1);
            }
            0xA9 => {
                let imm = self.fetch_imm(bus, osize);
                let a = self.reg(EAX, osize);
                self.flags_logic(a & imm, osize);
            }

            // ---- string ops (MOVS/CMPS/STOS/LODS/SCAS) with REP ----
            0xA4 | 0xA5 => {
                let size = if op == 0xA4 { 1 } else { osize };
                self.string_movs(bus, size, asize, &p);
            }
            0xA6 | 0xA7 => {
                let size = if op == 0xA6 { 1 } else { osize };
                self.string_cmps(bus, size, asize, &p);
            }
            0xAA | 0xAB => {
                let size = if op == 0xAA { 1 } else { osize };
                self.string_stos(bus, size, asize, &p);
            }
            0xAC | 0xAD => {
                let size = if op == 0xAC { 1 } else { osize };
                self.string_lods(bus, size, asize, &p);
            }
            0xAE | 0xAF => {
                let size = if op == 0xAE { 1 } else { osize };
                self.string_scas(bus, size, asize, &p);
            }

            // ---- XLAT/XLATB: AL <- [DS:(E)BX + AL] ----
            0xD7 => {
                let seg = p.seg.unwrap_or(DS);
                let bx = if asize == 2 { self.reg16(EBX) } else { self.reg32(EBX) };
                let off = bx.wrapping_add(self.reg8(EAX));
                let off = if asize == 2 { off & 0xFFFF } else { off };
                let lin = self.seg_base[seg].wrapping_add(off);
                let v = self.read_mem(bus, lin, 1);
                self.set_reg8(EAX, v);
            }

            // ---- ENTER imm16, imm8 (0xC8) ----
            0xC8 => {
                let alloc = self.fetch_u16(bus);
                let level = (self.fetch_u8(bus) & 0x1F) as u32;
                let fp = self.reg(EBP, osize);
                self.push(bus, fp, osize);
                let frame_temp = self.reg(ESP, osize);
                if level > 0 {
                    for _ in 1..level {
                        let new_bp = self.reg(EBP, osize).wrapping_sub(osize as u32);
                        self.set_reg(EBP, osize, new_bp);
                        let v = self.reg(EBP, osize);
                        self.push(bus, v, osize);
                    }
                    self.push(bus, frame_temp, osize);
                }
                self.set_reg(EBP, osize, frame_temp);
                let new_sp = frame_temp.wrapping_sub(alloc);
                self.set_reg(ESP, osize, new_sp);
            }
            // ---- LEAVE (0xC9): ESP <- EBP ; pop EBP ----
            0xC9 => {
                let fp = self.reg(EBP, osize);
                self.set_reg(ESP, osize, fp);
                let v = self.pop(bus, osize);
                self.set_reg(EBP, osize, v);
            }

            // ---- MOV r8/r, imm ----
            0xB0..=0xB7 => {
                let imm = self.fetch_u8(bus) as u32;
                self.set_reg8((op - 0xB0) as usize, imm);
            }
            0xB8..=0xBF => {
                let imm = self.fetch_imm(bus, osize);
                self.set_reg((op - 0xB8) as usize, osize, imm);
            }
            // ---- MOV r/m, imm ----
            0xC6 | 0xC7 => {
                let size = if op == 0xC6 { 1 } else { osize };
                let (_reg, ea) = self.modrm(bus, &p, asize);
                let imm = self.fetch_imm(bus, size);
                self.write_ea(bus, ea, size, imm);
            }

            // ---- shift/rotate group 2 ----
            0xC0 | 0xC1 | 0xD0 | 0xD1 | 0xD2 | 0xD3 => {
                let size = if op & 1 == 0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                let count = match op {
                    0xC0 | 0xC1 => self.fetch_u8(bus) as u32,
                    0xD0 | 0xD1 => 1,
                    _ => self.reg8(ECX),
                };
                let v = self.read_ea(bus, ea, size);
                match self.do_shift(reg, v, count, size) {
                    Some(res) => self.write_ea(bus, ea, size, res),
                    None => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }

            // ---- RET near ----
            0xC3 => {
                let v = self.pop(bus, osize);
                self.set_eip(v);
            }
            0xC2 => {
                let n = self.fetch_u16(bus);
                let v = self.pop(bus, osize);
                self.set_eip(v);
                // pop the imm16 bytes off the caller's stack
                if self.real_mode() {
                    let sp = self.reg16(ESP).wrapping_add(n) & 0xFFFF;
                    self.set_reg16(ESP, sp);
                } else {
                    let esp = self.reg32(ESP).wrapping_add(n);
                    self.set_reg32(ESP, esp);
                }
            }

            // ---- group 3: TEST/NOT/NEG/MUL/DIV ----
            0xF6 | 0xF7 => {
                let size = if op == 0xF6 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, &p, asize);
                self.exec_group3(bus, reg, ea, size, op, start_eip);
            }
            // ---- group 4/5: INC/DEC/CALL/JMP/PUSH r/m ----
            0xFE => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                let v = self.read_ea(bus, ea, 1);
                match reg {
                    0 => {
                        let r = self.flags_inc(v, 1);
                        self.write_ea(bus, ea, 1, r);
                    }
                    1 => {
                        let r = self.flags_dec(v, 1);
                        self.write_ea(bus, ea, 1, r);
                    }
                    _ => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op);
                    }
                }
            }
            0xFF => {
                let (reg, ea) = self.modrm(bus, &p, asize);
                self.exec_group5(bus, reg, ea, osize, op, start_eip);
            }

            // ---- relative jumps / calls ----
            0xEB => {
                let d = self.fetch_i8(bus);
                self.jump_rel(d);
            }
            0xE9 => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                self.jump_rel(d);
            }
            0xEA => {
                // far JMP ptr16:16/32 — new EIP then new CS selector.
                let off = self.fetch_imm(bus, osize);
                let sel = self.fetch_u16(bus) as u16;
                self.set_seg(CS, sel);
                self.set_eip(off);
            }
            0x70..=0x7F => {
                let d = self.fetch_i8(bus);
                let taken = self.cc(op - 0x70);
                if branch_trace_on() {
                    record_branch(BranchRec {
                        eip: start_eip,
                        cond: op - 0x70,
                        taken,
                        eflags: self.eflags,
                        regs: self.regs,
                    });
                }
                if taken {
                    self.jump_rel(d);
                }
            }
            0xE8 => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                let ret = self.eip;
                self.push(bus, ret, osize);
                self.jump_rel(d);
            }
            // ---- port I/O ----
            0xE4 => {
                let p = self.fetch_u8(bus) as u16;
                let v = bus.port_in(p, 1);
                self.set_reg8(EAX, v);
            }
            0xE5 => {
                let p = self.fetch_u8(bus) as u16;
                let v = bus.port_in(p, osize);
                self.set_reg(EAX, osize, v);
            }
            0xE6 => {
                let p = self.fetch_u8(bus) as u16;
                bus.port_out(p, 1, self.reg8(EAX));
            }
            0xE7 => {
                let p = self.fetch_u8(bus) as u16;
                bus.port_out(p, osize, self.reg(EAX, osize));
            }
            0xEC => {
                let p = self.reg16(EDX) as u16;
                let v = bus.port_in(p, 1);
                self.set_reg8(EAX, v);
            }
            0xED => {
                let p = self.reg16(EDX) as u16;
                let v = bus.port_in(p, osize);
                self.set_reg(EAX, osize, v);
            }
            0xEE => {
                let p = self.reg16(EDX) as u16;
                bus.port_out(p, 1, self.reg8(EAX));
            }
            0xEF => {
                let p = self.reg16(EDX) as u16;
                bus.port_out(p, osize, self.reg(EAX, osize));
            }
            0xE3 => {
                // JCXZ / JECXZ
                let d = self.fetch_i8(bus);
                let cx = if asize == 2 { self.reg16(ECX) } else { self.reg32(ECX) };
                if cx == 0 {
                    self.jump_rel(d);
                }
            }
            0xE0 | 0xE1 | 0xE2 => {
                let d = self.fetch_i8(bus);
                let cx = if asize == 2 {
                    let c = self.reg16(ECX).wrapping_sub(1) & 0xFFFF;
                    self.set_reg16(ECX, c);
                    c
                } else {
                    let c = self.reg32(ECX).wrapping_sub(1);
                    self.set_reg32(ECX, c);
                    c
                };
                let take = match op {
                    0xE0 => cx != 0 && !self.flag(ZF), // LOOPNE
                    0xE1 => cx != 0 && self.flag(ZF),  // LOOPE
                    _ => cx != 0,                      // LOOP
                };
                if take {
                    self.jump_rel(d);
                }
            }

            // ---- flag ops ----
            0xF4 => self.halted = true, // HLT
            0xF5 => self.eflags ^= CF,  // CMC
            0xF8 => self.set_flag(CF, false),
            0xF9 => self.set_flag(CF, true),
            0xFA => self.set_flag(IF, false), // CLI
            0xFB => self.set_flag(IF, true),  // STI
            0xFC => self.set_flag(DF, false), // CLD
            0xFD => self.set_flag(DF, true),  // STD
            0x9C => {
                let v = self.eflags;
                self.push(bus, v, osize);
            }
            0x9D => {
                let v = self.pop(bus, osize);
                self.eflags = (v | EFLAGS_ALWAYS_ONE) & 0x003F_7FD5 | EFLAGS_ALWAYS_ONE;
            }
            0x9E => {
                // SAHF: AH -> low byte of EFLAGS (SF ZF xx AF xx PF xx CF).
                let ah = self.reg8(4);
                self.eflags = (self.eflags & 0xFFFF_FF00) | (ah & 0xD5) | EFLAGS_ALWAYS_ONE;
            }
            0x9F => {
                // LAHF: low byte of EFLAGS -> AH.
                let lo = (self.eflags & 0xD5) | 0x02;
                self.set_reg8(4, lo);
            }

            // ---- x87 FPU ESC opcodes (0xD8..0xDF) ----
            0xD8..=0xDF => self.exec_fpu(bus, op, asize, &p, start_eip),

            // ---- two-byte (0x0F) ----
            0x0F => self.exec_0f(bus, osize, asize, &p, start_eip),

            // ---- everything else: documented #UD seam ----
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// ALU group dispatch for the six register/immediate-accumulator encodings.
    fn exec_alu_group(
        &mut self,
        bus: &mut impl Bus,
        op: u8,
        alu: Alu,
        osize: u8,
        asize: u8,
        p: &Prefixes,
    ) {
        match op & 7 {
            0 | 1 => {
                let size = if op & 7 == 0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let a = self.read_ea(bus, ea, size);
                let b = self.reg(reg as usize, size);
                let (res, wr) = self.alu(alu, a, b, size);
                if wr {
                    self.write_ea(bus, ea, size, res);
                }
            }
            2 | 3 => {
                let size = if op & 7 == 2 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let a = self.reg(reg as usize, size);
                let b = self.read_ea(bus, ea, size);
                let (res, wr) = self.alu(alu, a, b, size);
                if wr {
                    self.set_reg(reg as usize, size, res);
                }
            }
            4 => {
                let imm = self.fetch_u8(bus) as u32;
                let a = self.reg8(EAX);
                let (res, wr) = self.alu(alu, a, imm, 1);
                if wr {
                    self.set_reg8(EAX, res);
                }
            }
            _ => {
                let imm = self.fetch_imm(bus, osize);
                let a = self.reg(EAX, osize);
                let (res, wr) = self.alu(alu, a, imm, osize);
                if wr {
                    self.set_reg(EAX, osize, res);
                }
            }
        }
    }

    /// Group 3 (0xF6/0xF7): TEST imm / NOT / NEG / MUL / DIV (unsigned).
    /// IMUL/IDIV are documented seams (#UD).
    fn exec_group3(
        &mut self,
        bus: &mut impl Bus,
        reg: u8,
        ea: Ea,
        size: u8,
        op: u8,
        start_eip: u32,
    ) {
        match reg {
            0 | 1 => {
                let imm = self.fetch_imm(bus, size);
                let a = self.read_ea(bus, ea, size);
                self.flags_logic(a & imm, size);
            }
            2 => {
                let v = self.read_ea(bus, ea, size);
                self.write_ea(bus, ea, size, !v); // NOT — no flags
            }
            3 => {
                let v = self.read_ea(bus, ea, size);
                let res = self.flags_sub(0, v, size); // NEG = 0 - v
                self.write_ea(bus, ea, size, res);
            }
            4 => self.do_mul(bus, ea, size), // MUL (unsigned)
            5 => self.do_imul1(bus, ea, size), // IMUL (one-operand, signed)
            6 => self.do_div(bus, ea, size, start_eip), // DIV (unsigned)
            7 => self.do_idiv(bus, ea, size, start_eip), // IDIV (signed)
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// One-operand signed multiply (IMUL r/m): AL/AX/EAX * src into AX/DX:AX/
    /// EDX:EAX. CF=OF when the high half isn't the sign-extension of the low half.
    fn do_imul1(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) {
        let src = sign_ext(self.read_ea(bus, ea, size), size) as i32 as i64;
        match size {
            1 => {
                let r = (self.reg8(EAX) as i8 as i64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                let of = r as i8 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
            2 => {
                let r = (self.reg16(EAX) as i16 as i64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                self.set_reg16(EDX, (r as u32 >> 16) & 0xFFFF);
                let of = r as i16 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
            _ => {
                let r = (self.reg32(EAX) as i32 as i64) * src;
                self.set_reg32(EAX, r as u32);
                self.set_reg32(EDX, (r >> 32) as u32);
                let of = r as i32 as i64 != r;
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }
        }
    }

    /// Signed divide (IDIV r/m); raises #DE on divide-by-zero or quotient
    /// overflow.
    fn do_idiv(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, start_eip: u32) {
        let d = sign_ext(self.read_ea(bus, ea, size), size) as i32 as i64;
        if d == 0 {
            self.eip = start_eip;
            self.raise(Exception::DivideError, 0, 0);
            return;
        }
        match size {
            1 => {
                let n = self.reg16(EAX) as i16 as i64;
                let (q, r) = (n / d, n % d);
                if !(-128..=127).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg8(EAX, q as u32);
                self.set_reg8(4, r as u32); // AH
            }
            2 => {
                let n = (((self.reg16(EDX)) << 16) | self.reg16(EAX)) as i32 as i64;
                let (q, r) = (n / d, n % d);
                if !(i16::MIN as i64..=i16::MAX as i64).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg16(EAX, q as u32);
                self.set_reg16(EDX, r as u32);
            }
            _ => {
                let n = (((self.reg32(EDX) as u64) << 32) | self.reg32(EAX) as u64) as i64;
                let (q, r) = (n / d, n % d);
                if !(i32::MIN as i64..=i32::MAX as i64).contains(&q) {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg32(EAX, q as u32);
                self.set_reg32(EDX, r as u32);
            }
        }
    }

    /// Group 5 (0xFF): INC/DEC/CALL near/JMP near/PUSH r/m. Far call/jmp are
    /// documented seams (#UD).
    fn exec_group5(
        &mut self,
        bus: &mut impl Bus,
        reg: u8,
        ea: Ea,
        osize: u8,
        op: u8,
        start_eip: u32,
    ) {
        match reg {
            0 => {
                let v = self.read_ea(bus, ea, osize);
                let r = self.flags_inc(v, osize);
                self.write_ea(bus, ea, osize, r);
            }
            1 => {
                let v = self.read_ea(bus, ea, osize);
                let r = self.flags_dec(v, osize);
                self.write_ea(bus, ea, osize, r);
            }
            2 => {
                // CALL near indirect
                let target = self.read_ea(bus, ea, osize);
                let ret = self.eip;
                self.push(bus, ret, osize);
                self.set_eip(target);
            }
            4 => {
                // JMP near indirect
                let target = self.read_ea(bus, ea, osize);
                self.set_eip(target);
            }
            6 => {
                let v = self.read_ea(bus, ea, osize);
                self.push(bus, v, osize);
            }
            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op);
            }
        }
    }

    /// Unsigned MUL: AX = AL*r/m8, DX:AX = AX*r/m16, EDX:EAX = EAX*r/m32. CF/OF
    /// set when the upper half is non-zero.
    fn do_mul(&mut self, bus: &mut impl Bus, ea: Ea, size: u8) {
        let src = self.read_ea(bus, ea, size) as u64;
        match size {
            1 => {
                let r = (self.reg8(EAX) as u64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                let upper = (r >> 8) & 0xFF != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
            2 => {
                let r = (self.reg16(EAX) as u64) * src;
                self.set_reg16(EAX, r as u32 & 0xFFFF);
                self.set_reg16(EDX, (r >> 16) as u32 & 0xFFFF);
                let upper = (r >> 16) & 0xFFFF != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
            _ => {
                let r = (self.reg32(EAX) as u64) * src;
                self.set_reg32(EAX, r as u32);
                self.set_reg32(EDX, (r >> 32) as u32);
                let upper = (r >> 32) != 0;
                self.set_flag(CF, upper);
                self.set_flag(OF, upper);
            }
        }
    }

    /// Unsigned DIV: raises #DE on divide-by-zero or quotient overflow.
    fn do_div(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, start_eip: u32) {
        let d = self.read_ea(bus, ea, size) as u64;
        if d == 0 {
            self.eip = start_eip;
            self.raise(Exception::DivideError, 0, 0);
            return;
        }
        match size {
            1 => {
                let n = self.reg16(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg8(EAX, q as u32); // AL
                self.set_reg8(4, r as u32); // AH
            }
            2 => {
                let n = ((self.reg16(EDX) as u64) << 16) | self.reg16(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFFFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg16(EAX, q as u32);
                self.set_reg16(EDX, r as u32);
            }
            _ => {
                let n = ((self.reg32(EDX) as u64) << 32) | self.reg32(EAX) as u64;
                let q = n / d;
                let r = n % d;
                if q > 0xFFFF_FFFF {
                    self.eip = start_eip;
                    self.raise(Exception::DivideError, 0, 0);
                    return;
                }
                self.set_reg32(EAX, q as u32);
                self.set_reg32(EDX, r as u32);
            }
        }
    }

    /// Shift/rotate group-2 sub-op (`reg` field). Returns None for the
    /// not-yet-implemented through-carry rotates (RCL/RCR) so the caller raises
    /// #UD. SHL/SHR/SAR set SZP+CF (+OF for count 1); ROL/ROR set CF (+OF for
    /// count 1) but leave SZP.
    fn do_shift(&mut self, reg: u8, val: u32, count: u32, size: u8) -> Option<u32> {
        let bits = (size as u32) * 8;
        let count = count & 0x1F;
        let m = Cpu::size_mask(size);
        let sign = m ^ (m >> 1);
        let v = val & m;
        if count == 0 {
            return Some(v);
        }
        let res = match reg {
            4 | 6 => {
                // SHL / SAL
                let r = (v << count) & m;
                let cf = count <= bits && (v >> (bits - count)) & 1 != 0;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, (r & sign != 0) ^ cf);
                }
                r
            }
            5 => {
                // SHR
                let cf = (v >> (count - 1)) & 1 != 0;
                let r = v >> count;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, v & sign != 0);
                }
                r
            }
            7 => {
                // SAR (arithmetic — sign-extend then signed shift)
                let sv = sign_ext(v, size) as i32;
                let r = ((sv >> count.min(31)) as u32) & m;
                let cf = (sv >> (count - 1).min(31)) & 1 != 0;
                self.set_szp_pub(r, size);
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, false);
                }
                r
            }
            0 => {
                // ROL
                let c = count % bits;
                let r = if c == 0 { v } else { ((v << c) | (v >> (bits - c))) & m };
                let cf = r & 1 != 0;
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, (r & sign != 0) ^ cf);
                }
                r
            }
            1 => {
                // ROR
                let c = count % bits;
                let r = if c == 0 { v } else { ((v >> c) | (v << (bits - c))) & m };
                let cf = r & sign != 0;
                self.set_flag(CF, cf);
                if count == 1 {
                    self.set_flag(OF, ((r >> (bits - 1)) ^ (r >> (bits - 2))) & 1 != 0);
                }
                r
            }
            // RCL (2) / RCR (3): through-carry rotates — future seam.
            _ => return None,
        };
        Some(res)
    }

    /// BT/BTS/BTR/BTC with a register bit offset (0F A3/AB/B3/BB). For a memory
    /// operand the bit index addresses outside the nominal operand size, so we
    /// resolve the byte holding the bit; for a register the index is masked.
    fn do_bit(&mut self, bus: &mut impl Bus, p: &Prefixes, asize: u8, osize: u8, bop: BitOp) {
        let (reg, ea) = self.modrm(bus, p, asize);
        let bit = self.reg(reg as usize, osize);
        match ea {
            Ea::Reg(_) => {
                self.bit_apply(bus, ea, osize, bit, bop);
            }
            Ea::Mem { lin, off } => {
                // The bit offset selects which byte (signed for register-offset).
                let bit_i = bit as i32;
                let byte_off = bit_i.div_euclid(8);
                let bit_in_byte = bit_i.rem_euclid(8) as u32;
                let lin = lin.wrapping_add(byte_off as u32);
                let mem = Ea::Mem { lin, off };
                self.bit_apply(bus, mem, 1, bit_in_byte, bop);
            }
        }
    }

    /// Apply a single BT-family op at `bit` (masked to the operand size) of the
    /// operand at `ea`. Sets CF from the tested bit; writes back for S/R/C.
    fn bit_apply(&mut self, bus: &mut impl Bus, ea: Ea, size: u8, bit: u32, bop: BitOp) {
        let nbits = size as u32 * 8;
        let b = bit % nbits;
        let v = self.read_ea(bus, ea, size);
        let mask = 1u32 << b;
        self.set_flag(CF, v & mask != 0);
        let nv = match bop {
            BitOp::Test => return,
            BitOp::Set => v | mask,
            BitOp::Reset => v & !mask,
            BitOp::Comp => v ^ mask,
        };
        self.write_ea(bus, ea, size, nv & Cpu::size_mask(size));
    }

    /// SHLD: shift `dst` left by `count`, feeding in the high bits of `src`.
    fn do_shld(&mut self, dst: u32, src: u32, count: u32, size: u8) -> u32 {
        let bits = size as u32 * 8;
        let count = count & 0x1F;
        let m = Cpu::size_mask(size);
        let dst = dst & m;
        if count == 0 {
            return dst;
        }
        if count >= bits {
            // Officially undefined; do nothing observable but mimic real CPUs by
            // leaving SZP from a best-effort result. We just return dst.
            return dst;
        }
        let res = ((dst << count) | ((src & m) >> (bits - count))) & m;
        let cf = (dst >> (bits - count)) & 1 != 0;
        self.set_szp_pub(res, size);
        self.set_flag(CF, cf);
        if count == 1 {
            let sign = m ^ (m >> 1);
            self.set_flag(OF, ((res ^ dst) & sign) != 0);
        }
        res
    }

    /// SHRD: shift `dst` right by `count`, feeding in the low bits of `src`.
    fn do_shrd(&mut self, dst: u32, src: u32, count: u32, size: u8) -> u32 {
        let bits = size as u32 * 8;
        let count = count & 0x1F;
        let m = Cpu::size_mask(size);
        let dst = dst & m;
        if count == 0 {
            return dst;
        }
        if count >= bits {
            return dst;
        }
        let res = ((dst >> count) | ((src & m) << (bits - count))) & m;
        let cf = (dst >> (count - 1)) & 1 != 0;
        self.set_szp_pub(res, size);
        self.set_flag(CF, cf);
        if count == 1 {
            let sign = m ^ (m >> 1);
            self.set_flag(OF, ((res ^ dst) & sign) != 0);
        }
        res
    }

    /// Two-byte (0x0F-prefixed) opcodes: a small system/utility slice.
    fn exec_0f(
        &mut self,
        bus: &mut impl Bus,
        osize: u8,
        asize: u8,
        p: &Prefixes,
        start_eip: u32,
    ) {
        let op2 = self.fetch_u8(bus);
        match op2 {
            // MOV r32, CRn  /  MOV CRn, r32
            0x20 => {
                let b = self.fetch_u8(bus);
                let cr = ((b >> 3) & 7) as usize;
                let rm = (b & 7) as usize;
                self.set_reg32(rm, self.cr.get(cr).copied().unwrap_or(0));
            }
            0x22 => {
                let b = self.fetch_u8(bus);
                let cr = ((b >> 3) & 7) as usize;
                let rm = (b & 7) as usize;
                if cr < self.cr.len() {
                    self.cr[cr] = self.reg32(rm);
                }
            }
            // RDTSC: EDX:EAX <- retired-instruction counter (our clock proxy).
            0x31 => {
                self.set_reg32(EAX, self.instret as u32);
                self.set_reg32(EDX, (self.instret >> 32) as u32);
            }
            // INVD / WBINVD — cache invalidate/flush; no cache modeled, so nop.
            0x08 | 0x09 => {}
            // CPUID: a minimal, plausible Pentium III response.
            0xA2 => self.do_cpuid(),
            // Jcc near
            0x80..=0x8F => {
                let d = self.fetch_imm(bus, osize);
                let d = if osize == 2 { d as u16 as i16 as i32 as u32 } else { d };
                let taken = self.cc(op2 - 0x80);
                if branch_trace_on() {
                    record_branch(BranchRec {
                        eip: start_eip,
                        cond: op2 - 0x80,
                        taken,
                        eflags: self.eflags,
                        regs: self.regs,
                    });
                }
                if taken {
                    self.jump_rel(d);
                }
            }
            // SETcc r/m8
            0x90..=0x9F => {
                let (_reg, ea) = self.modrm(bus, p, asize);
                let v = self.cc(op2 - 0x90) as u32;
                self.write_ea(bus, ea, 1, v);
            }
            // MOVZX
            0xB6 | 0xB7 => {
                let src = if op2 == 0xB6 { 1 } else { 2 };
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = self.read_ea(bus, ea, src);
                self.set_reg(reg as usize, osize, v);
            }
            // MOVSX
            0xBE | 0xBF => {
                let src = if op2 == 0xBE { 1 } else { 2 };
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = sign_ext(self.read_ea(bus, ea, src), src);
                self.set_reg(reg as usize, osize, v);
            }

            // CMOVcc r, r/m — conditional move (no flags affected).
            0x40..=0x4F => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = self.read_ea(bus, ea, osize);
                if self.cc(op2 - 0x40) {
                    self.set_reg(reg as usize, osize, v);
                }
            }

            // IMUL r, r/m (two-operand, signed) — result truncated, CF=OF on ovf.
            0xAF => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let a = sign_ext(self.reg(reg as usize, osize), osize) as i32 as i64;
                let b = sign_ext(self.read_ea(bus, ea, osize), osize) as i32 as i64;
                let full = a * b;
                let res = (full as u32) & Cpu::size_mask(osize);
                let of = sign_ext(res, osize) as i32 as i64 != full;
                self.set_reg(reg as usize, osize, res);
                self.set_flag(CF, of);
                self.set_flag(OF, of);
            }

            // BT/BTS/BTR/BTC r/m, r  (reg form)
            0xA3 => self.do_bit(bus, p, asize, osize, BitOp::Test),
            0xAB => self.do_bit(bus, p, asize, osize, BitOp::Set),
            0xB3 => self.do_bit(bus, p, asize, osize, BitOp::Reset),
            0xBB => self.do_bit(bus, p, asize, osize, BitOp::Comp),
            // group 8: BT/BTS/BTR/BTC r/m, imm8 (reg field selects the op)
            0xBA => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let imm = self.fetch_u8(bus) as u32;
                let bop = match reg {
                    4 => BitOp::Test,
                    5 => BitOp::Set,
                    6 => BitOp::Reset,
                    7 => BitOp::Comp,
                    _ => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op2);
                        return;
                    }
                };
                self.bit_apply(bus, ea, osize, imm, bop);
            }

            // BSF / BSR
            0xBC | 0xBD => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let src = self.read_ea(bus, ea, osize) & Cpu::size_mask(osize);
                if src == 0 {
                    // ZF set; destination is left undefined (we leave it).
                    self.set_flag(ZF, true);
                } else {
                    self.set_flag(ZF, false);
                    let idx = if op2 == 0xBC {
                        src.trailing_zeros() // BSF: lowest set bit
                    } else {
                        31 - src.leading_zeros() // BSR: highest set bit
                    };
                    self.set_reg(reg as usize, osize, idx);
                }
            }

            // SHLD r/m, r, imm8 (0xA4) / CL (0xA5)
            0xA4 | 0xA5 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let count = if op2 == 0xA4 {
                    self.fetch_u8(bus) as u32
                } else {
                    self.reg8(ECX)
                };
                let dst = self.read_ea(bus, ea, osize);
                let src = self.reg(reg as usize, osize);
                let r = self.do_shld(dst, src, count, osize);
                self.write_ea(bus, ea, osize, r);
            }
            // SHRD r/m, r, imm8 (0xAC) / CL (0xAD)
            0xAC | 0xAD => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let count = if op2 == 0xAC {
                    self.fetch_u8(bus) as u32
                } else {
                    self.reg8(ECX)
                };
                let dst = self.read_ea(bus, ea, osize);
                let src = self.reg(reg as usize, osize);
                let r = self.do_shrd(dst, src, count, osize);
                self.write_ea(bus, ea, osize, r);
            }

            // BSWAP r32 (0xC8..0xCF) — byte-swap a 32-bit register.
            0xC8..=0xCF => {
                let r = (op2 - 0xC8) as usize;
                if osize == 2 {
                    // BSWAP r16 is officially undefined; common CPUs zero it.
                    self.set_reg16(r, 0);
                } else {
                    self.set_reg32(r, self.reg32(r).swap_bytes());
                }
            }

            // XADD r/m, r (0xC0 byte, 0xC1 word/dword)
            0xC0 | 0xC1 => {
                let size = if op2 == 0xC0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let dst = self.read_ea(bus, ea, size);
                let src = self.reg(reg as usize, size);
                let sum = self.flags_add(dst, src, size);
                self.set_reg(reg as usize, size, dst); // old dst -> reg
                self.write_ea(bus, ea, size, sum); // sum -> dst
            }

            // CMPXCHG r/m, r (0xB0 byte, 0xB1 word/dword)
            0xB0 | 0xB1 => {
                let size = if op2 == 0xB0 { 1 } else { osize };
                let (reg, ea) = self.modrm(bus, p, asize);
                let dst = self.read_ea(bus, ea, size);
                let acc = self.reg(EAX, size);
                // Compare accumulator with dst (sets flags like CMP acc, dst).
                self.flags_sub(acc, dst, size);
                if self.flag(ZF) {
                    let src = self.reg(reg as usize, size);
                    self.write_ea(bus, ea, size, src);
                } else {
                    self.set_reg(EAX, size, dst);
                }
            }
            // ---- SSE / SSE2 (real XMM register file) ----
            //
            // The Pentium III has the full SSE set. These opcodes use the
            // "mandatory prefix" decode scheme: the same second opcode byte
            // means a packed-single (no prefix), packed-double (0x66), scalar-
            // single (0xF3) or scalar-double (0xF2) instruction depending on the
            // legacy prefix that came before the 0x0F. The XMM register file
            // lives in the thread-local in `super::sse`; see [`Cpu::exec_sse`].
            //
            // This list mirrors the dispatch in `exec_sse`. Anything not handled
            // there falls through to the documented #UD seam.
            0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x16 | 0x17 | 0x28 | 0x29 | 0x2A
            | 0x2C | 0x2D | 0x2E | 0x2F | 0x50 | 0x51 | 0x52 | 0x53 | 0x54 | 0x55 | 0x56
            | 0x57 | 0x58 | 0x59 | 0x5A | 0x5B | 0x5C | 0x5D | 0x5E | 0x5F | 0x6E | 0x6F
            | 0x7E | 0x7F | 0xAE | 0xC2 | 0xC6 | 0xD6 | 0xEF => {
                self.exec_sse(bus, op2, asize, p, start_eip);
            }

            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op2);
            }
        }
    }

    // ============================ SSE / SSE2 ============================
    //
    // Helpers for moving 128/64-bit XMM operands through the 32-bit memory
    // accessors (`read_mem`/`write_mem` top out at a dword), and the big
    // mandatory-prefix dispatch table.

    /// Read 16 bytes (a full XMM register's worth) from guest memory as four
    /// little-endian dwords.
    fn read_xmm_mem(&mut self, bus: &mut impl Bus, lin: u32) -> [u8; 16] {
        let mut b = [0u8; 16];
        for i in 0..4 {
            let dw = self.read_mem(bus, lin.wrapping_add(i * 4), 4);
            b[(i * 4) as usize..(i * 4 + 4) as usize].copy_from_slice(&dw.to_le_bytes());
        }
        b
    }
    /// Write 16 bytes to guest memory as four little-endian dwords.
    fn write_xmm_mem(&mut self, bus: &mut impl Bus, lin: u32, b: [u8; 16]) {
        for i in 0..4u32 {
            let j = (i * 4) as usize;
            let dw = u32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]);
            self.write_mem(bus, lin.wrapping_add(i * 4), 4, dw);
        }
    }
    /// Read 8 bytes (a qword) from guest memory as two little-endian dwords.
    fn read_qword_mem(&mut self, bus: &mut impl Bus, lin: u32) -> u64 {
        let lo = self.read_mem(bus, lin, 4) as u64;
        let hi = self.read_mem(bus, lin.wrapping_add(4), 4) as u64;
        lo | (hi << 32)
    }
    /// Write 8 bytes (a qword) to guest memory as two little-endian dwords.
    fn write_qword_mem(&mut self, bus: &mut impl Bus, lin: u32, v: u64) {
        self.write_mem(bus, lin, 4, v as u32);
        self.write_mem(bus, lin.wrapping_add(4), 4, (v >> 32) as u32);
    }

    /// Read a packed 128-bit source operand: an XMM register (mod==3) or 16
    /// bytes of memory.
    fn sse_src128(&mut self, bus: &mut impl Bus, ea: Ea) -> [u8; 16] {
        match ea {
            Ea::Reg(r) => super::sse::with_xmm(|x| x.bytes(r as usize)),
            Ea::Mem { lin, .. } => self.read_xmm_mem(bus, lin),
        }
    }

    /// Dispatch one SSE/SSE2 instruction (the second opcode byte after 0x0F).
    ///
    /// `p.opsize` (0x66), `p.rep == 0xF3`, `p.rep == 0xF2` and "no prefix"
    /// select among the four variants of each opcode. `asize` is the effective
    /// address size for the ModR/M decode. Genuinely rare encodings fall through
    /// to the documented #UD seam.
    fn exec_sse(
        &mut self,
        bus: &mut impl Bus,
        op2: u8,
        asize: u8,
        p: &Prefixes,
        start_eip: u32,
    ) {
        // Mandatory-prefix selector.
        let pfx66 = p.opsize;
        let f3 = p.rep == 0xF3;
        let f2 = p.rep == 0xF2;

        match op2 {
            // ---- MOVUPS/MOVUPD/MOVSS/MOVSD (load: 0x10, store: 0x11) ----
            0x10 | 0x11 => {
                let store = op2 == 0x11;
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if f3 {
                    // MOVSS — scalar single (32-bit lane 0).
                    if store {
                        match ea {
                            Ea::Reg(r) => {
                                let v = super::sse::with_xmm(|x| x.lane0_f32(reg));
                                super::sse::with_xmm(|x| x.set_lane0_f32(r as usize, v));
                            }
                            Ea::Mem { lin, .. } => {
                                let v = super::sse::with_xmm(|x| x.dword0(reg));
                                self.write_mem(bus, lin, 4, v);
                            }
                        }
                    } else {
                        match ea {
                            // reg<-reg preserves upper lanes
                            Ea::Reg(r) => {
                                let v = super::sse::with_xmm(|x| x.dword0(r as usize));
                                super::sse::with_xmm(|x| x.set_dword0(reg, v));
                            }
                            // reg<-mem zeroes the upper 96 bits
                            Ea::Mem { lin, .. } => {
                                let v = self.read_mem(bus, lin, 4);
                                let mut b = [0u8; 16];
                                b[0..4].copy_from_slice(&v.to_le_bytes());
                                super::sse::with_xmm(|x| x.set_bytes(reg, b));
                            }
                        }
                    }
                } else if f2 {
                    // MOVSD — scalar double (64-bit lane 0).
                    if store {
                        match ea {
                            Ea::Reg(r) => {
                                let v = super::sse::with_xmm(|x| x.qword_lo(reg));
                                super::sse::with_xmm(|x| x.set_qword_lo(r as usize, v));
                            }
                            Ea::Mem { lin, .. } => {
                                let v = super::sse::with_xmm(|x| x.qword_lo(reg));
                                self.write_qword_mem(bus, lin, v);
                            }
                        }
                    } else {
                        match ea {
                            Ea::Reg(r) => {
                                let v = super::sse::with_xmm(|x| x.qword_lo(r as usize));
                                super::sse::with_xmm(|x| x.set_qword_lo(reg, v));
                            }
                            Ea::Mem { lin, .. } => {
                                let v = self.read_qword_mem(bus, lin);
                                super::sse::with_xmm(|x| x.set_u64s(reg, [v, 0]));
                            }
                        }
                    }
                } else {
                    // MOVUPS/MOVUPD — full 128-bit move (alignment not enforced).
                    if store {
                        let b = super::sse::with_xmm(|x| x.bytes(reg));
                        match ea {
                            Ea::Reg(r) => super::sse::with_xmm(|x| x.set_bytes(r as usize, b)),
                            Ea::Mem { lin, .. } => self.write_xmm_mem(bus, lin, b),
                        }
                    } else {
                        let b = self.sse_src128(bus, ea);
                        super::sse::with_xmm(|x| x.set_bytes(reg, b));
                    }
                }
            }

            // ---- MOVLPS/MOVLPD (0x12 load, 0x13 store) ----
            // Move the low 64 bits to/from memory (or movhlps for reg form).
            0x12 | 0x13 => {
                let store = op2 == 0x13;
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if store {
                    let lo = super::sse::with_xmm(|x| x.qword_lo(reg));
                    match ea {
                        Ea::Mem { lin, .. } => self.write_qword_mem(bus, lin, lo),
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.set_qword_lo(r as usize, lo)),
                    }
                } else {
                    match ea {
                        // MOVHLPS xmm1, xmm2: low(dest) <- high(src)
                        Ea::Reg(r) => {
                            let hi = super::sse::with_xmm(|x| x.qword_hi(r as usize));
                            super::sse::with_xmm(|x| x.set_qword_lo(reg, hi));
                        }
                        // MOVLPS xmm, m64: low 64 <- mem, high preserved
                        Ea::Mem { lin, .. } => {
                            let v = self.read_qword_mem(bus, lin);
                            super::sse::with_xmm(|x| x.set_qword_lo(reg, v));
                        }
                    }
                }
            }

            // ---- MOVHPS/MOVHPD (0x16 load, 0x17 store) ----
            0x16 | 0x17 => {
                let store = op2 == 0x17;
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if store {
                    let hi = super::sse::with_xmm(|x| x.qword_hi(reg));
                    match ea {
                        Ea::Mem { lin, .. } => self.write_qword_mem(bus, lin, hi),
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.set_qword_hi(r as usize, hi)),
                    }
                } else {
                    match ea {
                        // MOVLHPS xmm1, xmm2: high(dest) <- low(src)
                        Ea::Reg(r) => {
                            let lo = super::sse::with_xmm(|x| x.qword_lo(r as usize));
                            super::sse::with_xmm(|x| x.set_qword_hi(reg, lo));
                        }
                        // MOVHPS xmm, m64: high 64 <- mem, low preserved
                        Ea::Mem { lin, .. } => {
                            let v = self.read_qword_mem(bus, lin);
                            super::sse::with_xmm(|x| x.set_qword_hi(reg, v));
                        }
                    }
                }
            }

            // ---- UNPCKLPS/UNPCKHPS (0x14/0x15) ----
            0x14 | 0x15 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let d = super::sse::with_xmm(|x| x.f32s(reg));
                let sb = self.sse_src128(bus, ea);
                let s = bytes_to_f32s(sb);
                let r = if op2 == 0x14 {
                    // low: d0,s0,d1,s1
                    [d[0], s[0], d[1], s[1]]
                } else {
                    // high: d2,s2,d3,s3
                    [d[2], s[2], d[3], s[3]]
                };
                super::sse::with_xmm(|x| x.set_f32s(reg, r));
            }

            // ---- MOVAPS/MOVAPD (0x28 load, 0x29 store) ----
            0x28 | 0x29 => {
                let store = op2 == 0x29;
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if store {
                    let b = super::sse::with_xmm(|x| x.bytes(reg));
                    match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.set_bytes(r as usize, b)),
                        Ea::Mem { lin, .. } => self.write_xmm_mem(bus, lin, b),
                    }
                } else {
                    let b = self.sse_src128(bus, ea);
                    super::sse::with_xmm(|x| x.set_bytes(reg, b));
                }
            }

            // ---- CVTSI2SS/CVTSI2SD (F3/F2 0x2A) ----
            0x2A => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                // Source is an r/m32 integer (signed).
                let src = self.read_ea(bus, ea, 4) as i32;
                if f3 {
                    super::sse::with_xmm(|x| x.set_lane0_f32(reg, src as f32));
                } else if f2 {
                    super::sse::with_xmm(|x| x.set_lane0_f64(reg, src as f64));
                } else {
                    // Packed MMX integer->float (CVTPI2PS) is rare; #UD it.
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                }
            }

            // ---- CVTTSS2SI/CVTTSD2SI (0x2C) and CVTSS2SI/CVTSD2SI (0x2D) ----
            0x2C | 0x2D => {
                let truncate = op2 == 0x2C;
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if f3 {
                    let v = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f32(r as usize)),
                        Ea::Mem { lin, .. } => {
                            f32::from_bits(self.read_mem(bus, lin, 4))
                        }
                    } as f64;
                    let r = convert_to_i32(v, truncate);
                    self.set_reg32(reg, r as u32);
                } else if f2 {
                    let v = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f64(r as usize)),
                        Ea::Mem { lin, .. } => self.read_f64(bus, lin),
                    };
                    let r = convert_to_i32(v, truncate);
                    self.set_reg32(reg, r as u32);
                } else {
                    // CVTPS2PI / CVTTPS2PI (MMX dest) — rare; #UD.
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                }
            }

            // ---- UCOMISS/UCOMISD (0x2E) and COMISS/COMISD (0x2F) ----
            0x2E | 0x2F => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let (a, b) = if pfx66 {
                    let a = super::sse::with_xmm(|x| x.lane0_f64(reg));
                    let b = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f64(r as usize)),
                        Ea::Mem { lin, .. } => self.read_f64(bus, lin),
                    };
                    (a, b)
                } else {
                    let a = super::sse::with_xmm(|x| x.lane0_f32(reg)) as f64;
                    let b = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f32(r as usize)) as f64,
                        Ea::Mem { lin, .. } => {
                            f32::from_bits(self.read_mem(bus, lin, 4)) as f64
                        }
                    };
                    (a, b)
                };
                // EFLAGS: OF/SF/AF cleared; ZF/PF/CF set from the comparison.
                self.set_flag(OF, false);
                self.set_flag(SF, false);
                self.set_flag(AF, false);
                if a.is_nan() || b.is_nan() {
                    // unordered: ZF=PF=CF=1
                    self.set_flag(ZF, true);
                    self.set_flag(PF, true);
                    self.set_flag(CF, true);
                } else if a < b {
                    self.set_flag(ZF, false);
                    self.set_flag(PF, false);
                    self.set_flag(CF, true);
                } else if a > b {
                    self.set_flag(ZF, false);
                    self.set_flag(PF, false);
                    self.set_flag(CF, false);
                } else {
                    self.set_flag(ZF, true);
                    self.set_flag(PF, false);
                    self.set_flag(CF, false);
                }
            }

            // ---- MOVMSKPS/MOVMSKPD (0x50) ----
            0x50 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let src = match ea {
                    Ea::Reg(r) => r as usize,
                    // MOVMSK requires a register source; mem form is illegal.
                    Ea::Mem { .. } => {
                        self.eip = start_eip;
                        self.raise(Exception::InvalidOpcode, 0, op2);
                        return;
                    }
                };
                let bits = super::sse::with_xmm(|x| x.u32s(src));
                let mask = if pfx66 {
                    // PD: sign bits of the two doubles (lanes 1 and 3 hold them).
                    ((bits[1] >> 31) & 1) | (((bits[3] >> 31) & 1) << 1)
                } else {
                    // PS: sign bits of the four singles.
                    (bits[0] >> 31 & 1)
                        | ((bits[1] >> 31 & 1) << 1)
                        | ((bits[2] >> 31 & 1) << 2)
                        | ((bits[3] >> 31 & 1) << 3)
                };
                self.set_reg32(reg as usize, mask);
            }

            // ---- arithmetic & SQRT/RCP/RSQRT (0x51..0x5F except logic/conv) ----
            0x51 | 0x52 | 0x53 | 0x58 | 0x59 | 0x5C | 0x5D | 0x5E | 0x5F => {
                self.sse_arith(bus, op2, asize, p);
            }

            // ---- ANDPS/ANDNPS/ORPS/XORPS (0x54..0x57) — bitwise 128-bit ----
            0x54..=0x57 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let d = super::sse::with_xmm(|x| x.u64s(reg));
                let s = bytes_to_u64s(self.sse_src128(bus, ea));
                let r = match op2 {
                    0x54 => [d[0] & s[0], d[1] & s[1]],         // AND
                    0x55 => [!d[0] & s[0], !d[1] & s[1]],       // ANDN: ~dest & src
                    0x56 => [d[0] | s[0], d[1] | s[1]],         // OR
                    _ => [d[0] ^ s[0], d[1] ^ s[1]],            // XOR
                };
                super::sse::with_xmm(|x| x.set_u64s(reg, r));
            }

            // ---- CVTPS2PD/CVTPD2PS, CVTSS2SD/CVTSD2SS (0x5A) ----
            0x5A => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                if f3 {
                    // CVTSS2SD: scalar single -> double (lane 0).
                    let v = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f32(r as usize)),
                        Ea::Mem { lin, .. } => f32::from_bits(self.read_mem(bus, lin, 4)),
                    };
                    super::sse::with_xmm(|x| x.set_lane0_f64(reg, v as f64));
                } else if f2 {
                    // CVTSD2SS: scalar double -> single (lane 0).
                    let v = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f64(r as usize)),
                        Ea::Mem { lin, .. } => self.read_f64(bus, lin),
                    };
                    super::sse::with_xmm(|x| x.set_lane0_f32(reg, v as f32));
                } else if pfx66 {
                    // CVTPD2PS: 2 doubles -> 2 singles (upper lanes zeroed).
                    let s = bytes_to_f64s(self.sse_src128(bus, ea));
                    super::sse::with_xmm(|x| x.set_f32s(reg, [s[0] as f32, s[1] as f32, 0.0, 0.0]));
                } else {
                    // CVTPS2PD: low 2 singles -> 2 doubles.
                    let s = bytes_to_f32s(self.sse_src128(bus, ea));
                    super::sse::with_xmm(|x| x.set_f64s(reg, [s[0] as f64, s[1] as f64]));
                }
            }

            // ---- CVTDQ2PS / CVTPS2DQ / CVTTPS2DQ (0x5B) ----
            0x5B => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let sb = self.sse_src128(bus, ea);
                if f3 {
                    // CVTTPS2DQ: 4 singles -> 4 i32 (truncate).
                    let s = bytes_to_f32s(sb);
                    let r = [
                        convert_to_i32(s[0] as f64, true) as u32,
                        convert_to_i32(s[1] as f64, true) as u32,
                        convert_to_i32(s[2] as f64, true) as u32,
                        convert_to_i32(s[3] as f64, true) as u32,
                    ];
                    super::sse::with_xmm(|x| x.set_u32s(reg, r));
                } else if pfx66 {
                    // CVTPS2DQ: 4 singles -> 4 i32 (round).
                    let s = bytes_to_f32s(sb);
                    let r = [
                        convert_to_i32(s[0] as f64, false) as u32,
                        convert_to_i32(s[1] as f64, false) as u32,
                        convert_to_i32(s[2] as f64, false) as u32,
                        convert_to_i32(s[3] as f64, false) as u32,
                    ];
                    super::sse::with_xmm(|x| x.set_u32s(reg, r));
                } else {
                    // CVTDQ2PS: 4 i32 -> 4 singles.
                    let s = bytes_to_u32s(sb);
                    let r = [
                        s[0] as i32 as f32,
                        s[1] as i32 as f32,
                        s[2] as i32 as f32,
                        s[3] as i32 as f32,
                    ];
                    super::sse::with_xmm(|x| x.set_f32s(reg, r));
                }
            }

            // ---- MOVD (66 0x6E load r/m32 -> xmm) ----
            0x6E => {
                if !pfx66 {
                    // MMX MOVD (mm dest) — out of scope; #UD.
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                    return;
                }
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = self.read_ea(bus, ea, 4);
                // Zero-extend into the full 128-bit register.
                super::sse::with_xmm(|x| x.set_u32s(reg as usize, [v, 0, 0, 0]));
            }

            // ---- MOVDQA/MOVDQU (66/F3 0x6F load) ----
            0x6F => {
                if !pfx66 && !f3 {
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                    return;
                }
                let (reg, ea) = self.modrm(bus, p, asize);
                let b = self.sse_src128(bus, ea);
                super::sse::with_xmm(|x| x.set_bytes(reg as usize, b));
            }

            // ---- MOVD store (66 0x7E xmm -> r/m32) and MOVQ load (F3 0x7E) ----
            0x7E => {
                if f3 {
                    // MOVQ xmm <- xmm/m64, zero-extending the upper 64 bits.
                    let (reg, ea) = self.modrm(bus, p, asize);
                    let reg = reg as usize;
                    let v = match ea {
                        Ea::Reg(r) => super::sse::with_xmm(|x| x.qword_lo(r as usize)),
                        Ea::Mem { lin, .. } => self.read_qword_mem(bus, lin),
                    };
                    super::sse::with_xmm(|x| x.set_u64s(reg, [v, 0]));
                } else if pfx66 {
                    // MOVD r/m32 <- xmm (low dword).
                    let (reg, ea) = self.modrm(bus, p, asize);
                    let v = super::sse::with_xmm(|x| x.dword0(reg as usize));
                    self.write_ea(bus, ea, 4, v);
                } else {
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                }
            }

            // ---- MOVDQA/MOVDQU store (66/F3 0x7F) ----
            0x7F => {
                if !pfx66 && !f3 {
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                    return;
                }
                let (reg, ea) = self.modrm(bus, p, asize);
                let b = super::sse::with_xmm(|x| x.bytes(reg as usize));
                match ea {
                    Ea::Reg(r) => super::sse::with_xmm(|x| x.set_bytes(r as usize, b)),
                    Ea::Mem { lin, .. } => self.write_xmm_mem(bus, lin, b),
                }
            }

            // ---- LDMXCSR/STMXCSR + fences/prefetch (0xAE group) ----
            0xAE => {
                let (reg, ea) = self.modrm(bus, p, asize);
                match (reg, ea) {
                    // /2 LDMXCSR m32
                    (2, Ea::Mem { lin, .. }) => {
                        let v = self.read_mem(bus, lin, 4);
                        super::sse::with_xmm(|x| x.mxcsr = v);
                    }
                    // /3 STMXCSR m32
                    (3, Ea::Mem { lin, .. }) => {
                        let v = super::sse::with_xmm(|x| x.mxcsr);
                        self.write_mem(bus, lin, 4, v);
                    }
                    // /5 LFENCE, /6 MFENCE, /7 SFENCE (reg form) — no-ops.
                    // /0,/1 PREFETCH* (mem form) — no-ops.
                    _ => {}
                }
            }

            // ---- CMPPS/CMPPD/CMPSS/CMPSD (0xC2, imm8 predicate) ----
            0xC2 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let sb = self.sse_src128(bus, ea);
                let imm = self.fetch_u8(bus) & 0x7;
                if f3 {
                    // scalar single
                    let a = super::sse::with_xmm(|x| x.lane0_f32(reg));
                    let b = f32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]);
                    let m = if cmp_predicate(a as f64, b as f64, imm) {
                        0xFFFF_FFFFu32
                    } else {
                        0
                    };
                    super::sse::with_xmm(|x| x.set_dword0(reg, m));
                } else if f2 {
                    // scalar double
                    let a = super::sse::with_xmm(|x| x.lane0_f64(reg));
                    let b = bytes_to_f64s(sb)[0];
                    let m = if cmp_predicate(a, b, imm) {
                        0xFFFF_FFFF_FFFF_FFFFu64
                    } else {
                        0
                    };
                    super::sse::with_xmm(|x| x.set_qword_lo(reg, m));
                } else if pfx66 {
                    // packed double
                    let a = super::sse::with_xmm(|x| x.f64s(reg));
                    let s = bytes_to_f64s(sb);
                    let mut r = [0u64; 2];
                    for i in 0..2 {
                        r[i] = if cmp_predicate(a[i], s[i], imm) {
                            0xFFFF_FFFF_FFFF_FFFF
                        } else {
                            0
                        };
                    }
                    super::sse::with_xmm(|x| x.set_u64s(reg, r));
                } else {
                    // packed single
                    let a = super::sse::with_xmm(|x| x.f32s(reg));
                    let s = bytes_to_f32s(sb);
                    let mut r = [0u32; 4];
                    for i in 0..4 {
                        r[i] = if cmp_predicate(a[i] as f64, s[i] as f64, imm) {
                            0xFFFF_FFFF
                        } else {
                            0
                        };
                    }
                    super::sse::with_xmm(|x| x.set_u32s(reg, r));
                }
            }

            // ---- SHUFPS/SHUFPD (0xC6, imm8) ----
            0xC6 => {
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let sb = self.sse_src128(bus, ea);
                let imm = self.fetch_u8(bus);
                if pfx66 {
                    // SHUFPD: bit0 picks dest lane, bit1 picks src lane.
                    let d = super::sse::with_xmm(|x| x.f64s(reg));
                    let s = bytes_to_f64s(sb);
                    let r0 = d[(imm & 1) as usize];
                    let r1 = s[((imm >> 1) & 1) as usize];
                    super::sse::with_xmm(|x| x.set_f64s(reg, [r0, r1]));
                } else {
                    // SHUFPS: lanes 0,1 from dest; lanes 2,3 from src.
                    let d = super::sse::with_xmm(|x| x.f32s(reg));
                    let s = bytes_to_f32s(sb);
                    let r = [
                        d[(imm & 3) as usize],
                        d[((imm >> 2) & 3) as usize],
                        s[((imm >> 4) & 3) as usize],
                        s[((imm >> 6) & 3) as usize],
                    ];
                    super::sse::with_xmm(|x| x.set_f32s(reg, r));
                }
            }

            // ---- MOVQ store (66 0xD6 xmm -> xmm/m64) ----
            0xD6 => {
                if !pfx66 {
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                    return;
                }
                let (reg, ea) = self.modrm(bus, p, asize);
                let v = super::sse::with_xmm(|x| x.qword_lo(reg as usize));
                match ea {
                    // reg dest: low 64 <- src low 64, upper 64 zeroed.
                    Ea::Reg(r) => super::sse::with_xmm(|x| x.set_u64s(r as usize, [v, 0])),
                    Ea::Mem { lin, .. } => self.write_qword_mem(bus, lin, v),
                }
            }

            // ---- PXOR (66 0xEF) — 128-bit integer XOR ----
            0xEF => {
                if !pfx66 {
                    // MMX PXOR — out of scope; #UD.
                    self.eip = start_eip;
                    self.raise(Exception::InvalidOpcode, 0, op2);
                    return;
                }
                let (reg, ea) = self.modrm(bus, p, asize);
                let reg = reg as usize;
                let d = super::sse::with_xmm(|x| x.u64s(reg));
                let s = bytes_to_u64s(self.sse_src128(bus, ea));
                super::sse::with_xmm(|x| x.set_u64s(reg, [d[0] ^ s[0], d[1] ^ s[1]]));
            }

            _ => {
                self.eip = start_eip;
                self.raise(Exception::InvalidOpcode, 0, op2);
            }
        }
    }

    /// The packed/scalar floating arithmetic ops (ADD/MUL/SUB/DIV/MIN/MAX and
    /// the unary SQRT/RCP/RSQRT), selected by the mandatory prefix into PS/PD/
    /// SS/SD variants.
    fn sse_arith(&mut self, bus: &mut impl Bus, op2: u8, asize: u8, p: &Prefixes) {
        let pfx66 = p.opsize;
        let f3 = p.rep == 0xF3;
        let f2 = p.rep == 0xF2;
        let (reg, ea) = self.modrm(bus, p, asize);
        let reg = reg as usize;
        let unary = matches!(op2, 0x51..=0x53);

        if f3 {
            // scalar single — operate on lane 0 only, preserve upper lanes.
            let a = super::sse::with_xmm(|x| x.lane0_f32(reg)) as f64;
            let b = match ea {
                Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f32(r as usize)) as f64,
                Ea::Mem { lin, .. } => f32::from_bits(self.read_mem(bus, lin, 4)) as f64,
            };
            let r = sse_op(op2, a, b, unary) as f32;
            super::sse::with_xmm(|x| x.set_lane0_f32(reg, r));
        } else if f2 {
            // scalar double — lane 0 only.
            let a = super::sse::with_xmm(|x| x.lane0_f64(reg));
            let b = match ea {
                Ea::Reg(r) => super::sse::with_xmm(|x| x.lane0_f64(r as usize)),
                Ea::Mem { lin, .. } => self.read_f64(bus, lin),
            };
            let r = sse_op(op2, a, b, unary);
            super::sse::with_xmm(|x| x.set_lane0_f64(reg, r));
        } else if pfx66 {
            // packed double — both lanes.
            let a = super::sse::with_xmm(|x| x.f64s(reg));
            let s = bytes_to_f64s(self.sse_src128(bus, ea));
            let mut r = [0f64; 2];
            for i in 0..2 {
                r[i] = sse_op(op2, a[i], s[i], unary);
            }
            super::sse::with_xmm(|x| x.set_f64s(reg, r));
        } else {
            // packed single — all four lanes.
            let a = super::sse::with_xmm(|x| x.f32s(reg));
            let s = bytes_to_f32s(self.sse_src128(bus, ea));
            let mut r = [0f32; 4];
            for i in 0..4 {
                r[i] = sse_op(op2, a[i] as f64, s[i] as f64, unary) as f32;
            }
            super::sse::with_xmm(|x| x.set_f32s(reg, r));
        }
    }

    /// A minimal CPUID: leaf 0 returns the vendor string + max leaf; leaf 1
    /// returns the reset signature. Enough to satisfy a feature probe without
    /// pretending to a full feature set.
    fn do_cpuid(&mut self) {
        match self.reg32(EAX) {
            0 => {
                self.set_reg32(EAX, 1); // max standard leaf
                self.set_reg32(EBX, 0x756E_6547); // "Genu"
                self.set_reg32(EDX, 0x4969_6E65); // "ineI"
                self.set_reg32(ECX, 0x6C65_746E); // "ntel"
            }
            _ => {
                self.set_reg32(EAX, RESET_EDX); // family/model/stepping
                self.set_reg32(EBX, 0);
                self.set_reg32(ECX, 0);
                self.set_reg32(EDX, 0x0000_0001); // FPU present (token feature bit)
            }
        }
    }

    // ============================ x87 FPU ============================
    //
    // The 0xD8..0xDF "ESC" opcodes encode the floating-point unit. Each uses the
    // ModR/M byte two ways: `mod != 3` is a memory operand (load/store of a
    // 32/64-bit float or 16/32-bit integer); `mod == 3` is a register-stack op
    // selected by the (opcode, reg, rm) triple. The architectural FPU state
    // (register stack + control/status words) lives in the thread-local in
    // `super::fpu` — see that module's docs for why.

    /// Read a 32-bit float (m32fp) from guest memory.
    fn read_f32(&mut self, bus: &mut impl Bus, lin: u32) -> f64 {
        let bits = self.read_mem(bus, lin, 4);
        f32::from_bits(bits) as f64
    }
    /// Write a 32-bit float (m32fp) to guest memory.
    fn write_f32(&mut self, bus: &mut impl Bus, lin: u32, v: f64) {
        self.write_mem(bus, lin, 4, (v as f32).to_bits());
    }
    /// Read a 64-bit float (m64fp) — two little-endian dwords.
    fn read_f64(&mut self, bus: &mut impl Bus, lin: u32) -> f64 {
        let lo = self.read_mem(bus, lin, 4) as u64;
        let hi = self.read_mem(bus, lin.wrapping_add(4), 4) as u64;
        f64::from_bits(lo | (hi << 32))
    }
    /// Write a 64-bit float (m64fp) — two little-endian dwords.
    fn write_f64(&mut self, bus: &mut impl Bus, lin: u32, v: f64) {
        let bits = v.to_bits();
        self.write_mem(bus, lin, 4, bits as u32);
        self.write_mem(bus, lin.wrapping_add(4), 4, (bits >> 32) as u32);
    }

    /// Dispatch an x87 ESC opcode (0xD8..0xDF). Implements the common,
    /// high-frequency arithmetic/load/store/compare/control set; genuinely rare
    /// sub-encodings fall through to the documented #UD seam.
    fn exec_fpu(
        &mut self,
        bus: &mut impl Bus,
        op: u8,
        asize: u8,
        p: &Prefixes,
        start_eip: u32,
    ) {
        // Peek the ModR/M to split memory vs. register-stack forms; `modrm`
        // consumes the byte (and any SIB/disp) so reg/rm/Ea are resolved.
        let modrm_byte = bus.fetch8(self.code_linear(self.eip));
        let md = modrm_byte >> 6;
        let reg = (modrm_byte >> 3) & 7;
        let rm = modrm_byte & 7;

        if md == 3 {
            // Register-stack form: the ModR/M byte is just a selector — advance
            // EIP past it (no SIB/disp on a register operand).
            self.eip = self.eip.wrapping_add(1);
            self.fpu_reg_form(op, reg, rm, start_eip);
            return;
        }

        // Memory form: resolve the effective address through the normal path.
        let (_reg, ea) = self.modrm(bus, p, asize);
        let lin = match ea {
            Ea::Mem { lin, .. } => lin,
            Ea::Reg(_) => unreachable!("md != 3 yields a memory operand"),
        };
        self.fpu_mem_form(bus, op, reg, lin, start_eip);
    }

    /// Memory-operand x87 forms. `reg` is the ModR/M reg field (the sub-op).
    fn fpu_mem_form(
        &mut self,
        bus: &mut impl Bus,
        op: u8,
        reg: u8,
        lin: u32,
        start_eip: u32,
    ) {
        use super::fpu::with_fpu;
        match op {
            // D8 /r — arithmetic with m32fp, result into ST(0).
            0xD8 => {
                let src = self.read_f32(bus, lin);
                self.fpu_arith_mem(reg, src);
            }
            // D9 — m32fp loads/stores + control.
            0xD9 => match reg {
                0 => {
                    let v = self.read_f32(bus, lin);
                    with_fpu(|f| f.push(v)); // FLD m32fp
                }
                2 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_f32(bus, lin, v); // FST m32fp
                }
                3 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_f32(bus, lin, v);
                    with_fpu(|f| {
                        f.pop();
                    }); // FSTP m32fp
                }
                5 => {
                    // FLDCW m16
                    let cw = self.read_mem(bus, lin, 2) as u16;
                    with_fpu(|f| f.set_control_word(cw));
                }
                7 => {
                    // FNSTCW m16
                    let cw = with_fpu(|f| f.control_word());
                    self.write_mem(bus, lin, 2, cw as u32);
                }
                _ => self.fpu_ud(start_eip, op),
            },
            // DA /r — arithmetic with m32 integer.
            0xDA => {
                let src = self.read_mem(bus, lin, 4) as i32 as f64;
                self.fpu_arith_mem(reg, src);
            }
            // DB — m32int load/store, m80 (extended) load/store best-effort.
            0xDB => match reg {
                0 => {
                    let v = self.read_mem(bus, lin, 4) as i32 as f64;
                    with_fpu(|f| f.push(v)); // FILD m32int
                }
                2 => {
                    // FIST m32int
                    let v = with_fpu(|f| f.st(0));
                    self.write_mem(bus, lin, 4, fpu_to_i32(v) as u32);
                }
                3 => {
                    // FISTP m32int
                    let v = with_fpu(|f| f.st(0));
                    self.write_mem(bus, lin, 4, fpu_to_i32(v) as u32);
                    with_fpu(|f| {
                        f.pop();
                    });
                }
                5 => {
                    // FLD m80fp — read 80-bit extended, narrow to f64.
                    let v = self.read_f80(bus, lin);
                    with_fpu(|f| f.push(v));
                }
                7 => {
                    // FSTP m80fp — widen ST(0) to 80-bit, pop.
                    let v = with_fpu(|f| f.st(0));
                    self.write_f80(bus, lin, v);
                    with_fpu(|f| {
                        f.pop();
                    });
                }
                _ => self.fpu_ud(start_eip, op),
            },
            // DC /r — arithmetic with m64fp, result into ST(0).
            0xDC => {
                let src = self.read_f64(bus, lin);
                self.fpu_arith_mem(reg, src);
            }
            // DD — m64fp loads/stores + FNSTSW m16.
            0xDD => match reg {
                0 => {
                    let v = self.read_f64(bus, lin);
                    with_fpu(|f| f.push(v)); // FLD m64fp
                }
                2 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_f64(bus, lin, v); // FST m64fp
                }
                3 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_f64(bus, lin, v);
                    with_fpu(|f| {
                        f.pop();
                    }); // FSTP m64fp
                }
                7 => {
                    // FNSTSW m16
                    let sw = with_fpu(|f| f.status_word());
                    self.write_mem(bus, lin, 2, sw as u32);
                }
                _ => self.fpu_ud(start_eip, op),
            },
            // DE /r — arithmetic with m16 integer.
            0xDE => {
                let src = self.read_mem(bus, lin, 2) as i16 as f64;
                self.fpu_arith_mem(reg, src);
            }
            // DF — m16int load/store + m64int.
            0xDF => match reg {
                0 => {
                    let v = self.read_mem(bus, lin, 2) as i16 as f64;
                    with_fpu(|f| f.push(v)); // FILD m16int
                }
                2 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_mem(bus, lin, 2, fpu_to_i32(v) as u32); // FIST m16int
                }
                3 => {
                    let v = with_fpu(|f| f.st(0));
                    self.write_mem(bus, lin, 2, fpu_to_i32(v) as u32);
                    with_fpu(|f| {
                        f.pop();
                    }); // FISTP m16int
                }
                5 => {
                    // FILD m64int
                    let lo = self.read_mem(bus, lin, 4) as u64;
                    let hi = self.read_mem(bus, lin.wrapping_add(4), 4) as u64;
                    let v = (lo | (hi << 32)) as i64 as f64;
                    with_fpu(|f| f.push(v));
                }
                7 => {
                    // FISTP m64int
                    let v = with_fpu(|f| f.st(0));
                    let i = fpu_to_i64(v) as u64;
                    self.write_mem(bus, lin, 4, i as u32);
                    self.write_mem(bus, lin.wrapping_add(4), 4, (i >> 32) as u32);
                    with_fpu(|f| {
                        f.pop();
                    });
                }
                _ => self.fpu_ud(start_eip, op),
            },
            _ => self.fpu_ud(start_eip, op),
        }
    }

    /// The shared `D8/DA/DC/DE /r` arithmetic dispatch on the reg field, with a
    /// source already converted to f64. Result goes into ST(0).
    fn fpu_arith_mem(&mut self, reg: u8, src: f64) {
        use super::fpu::with_fpu;
        with_fpu(|f| {
            let st0 = f.st(0);
            match reg {
                0 => f.set_st(0, st0 + src),        // FADD
                1 => f.set_st(0, st0 * src),        // FMUL
                2 => f.set_compare(st0, src),       // FCOM
                3 => {
                    f.set_compare(st0, src);
                    f.pop();
                } // FCOMP
                4 => f.set_st(0, st0 - src),        // FSUB
                5 => f.set_st(0, src - st0),        // FSUBR
                6 => f.set_st(0, st0 / src),        // FDIV
                _ => f.set_st(0, src / st0),        // FDIVR
            }
        });
    }

    /// Register-stack x87 forms (`mod == 3`). Selected by (op, reg, rm).
    fn fpu_reg_form(&mut self, op: u8, reg: u8, rm: u8, start_eip: u32) {
        use super::fpu::{reset_fpu, with_fpu, SW_C0, SW_C2, SW_C3};
        let i = rm as usize;
        match op {
            // D8: arithmetic ST(0) op= ST(i)  +  FCOM/FCOMP ST(i)
            0xD8 => with_fpu(|f| {
                let st0 = f.st(0);
                let sti = f.st(i);
                match reg {
                    0 => f.set_st(0, st0 + sti), // FADD ST,ST(i)
                    1 => f.set_st(0, st0 * sti), // FMUL ST,ST(i)
                    2 => f.set_compare(st0, sti), // FCOM ST(i)
                    3 => {
                        f.set_compare(st0, sti);
                        f.pop();
                    } // FCOMP ST(i)
                    4 => f.set_st(0, st0 - sti), // FSUB
                    5 => f.set_st(0, sti - st0), // FSUBR
                    6 => f.set_st(0, st0 / sti), // FDIV
                    _ => f.set_st(0, sti / st0), // FDIVR
                }
            }),
            // D9: FLD ST(i), FXCH, and the no-operand utility group (reg 4..7).
            0xD9 => match reg {
                0 => with_fpu(|f| {
                    let v = f.st(i);
                    f.push(v);
                }), // FLD ST(i)
                1 => with_fpu(|f| f.xch(i)), // FXCH ST(i)
                4 => with_fpu(|f| match rm {
                    0 => {
                        // FCHS (D9 E0)
                        let v = -f.st(0);
                        f.set_st(0, v);
                    }
                    1 => {
                        // FABS (D9 E1)
                        let v = f.st(0).abs();
                        f.set_st(0, v);
                    }
                    _ => {}
                }),
                5 => with_fpu(|f| {
                    // Constant loads.
                    let c = match rm {
                        0 => 1.0,                                   // FLD1
                        1 => std::f64::consts::LOG2_10,             // FLDL2T
                        2 => std::f64::consts::LOG2_E,              // FLDL2E
                        3 => std::f64::consts::PI,                  // FLDPI
                        4 => std::f64::consts::LOG10_2,             // FLDLG2
                        5 => std::f64::consts::LN_2,                // FLDLN2
                        _ => 0.0,                                   // FLDZ (rm=6)
                    };
                    f.push(c);
                }),
                6 => with_fpu(|f| match rm {
                    0 => {} // F2XM1 (rare) — leave ST(0)
                    4 => {
                        // FTST: compare ST(0) with 0.0
                        let st0 = f.st(0);
                        f.set_compare(st0, 0.0);
                    }
                    _ => {}
                }),
                7 => with_fpu(|f| match rm {
                    2 => {
                        // FSQRT (D9 FA)
                        let v = f.st(0).sqrt();
                        f.set_st(0, v);
                    }
                    _ => {} // FPREM/FYL2X/FSINCOS/... left as no-ops for now
                }),
                _ => self.fpu_ud(start_eip, op),
            },
            // DA: FCMOVcc (conditional move on integer EFLAGS) + FUCOMPP.
            0xDA => {
                let cond = match reg {
                    0 => self.flag(CF),                  // FCMOVB  (below)
                    1 => self.flag(ZF),                  // FCMOVE  (equal)
                    2 => self.flag(CF) || self.flag(ZF), // FCMOVBE
                    3 => self.flag(PF),                  // FCMOVU  (unordered)
                    5 if rm == 1 => {
                        // FUCOMPP (DA E9): compare ST(0),ST(1); pop twice.
                        with_fpu(|f| {
                            let (a, b) = (f.st(0), f.st(1));
                            f.set_compare(a, b);
                            f.pop();
                            f.pop();
                        });
                        return;
                    }
                    _ => {
                        self.fpu_ud(start_eip, op);
                        return;
                    }
                };
                if cond {
                    with_fpu(|f| {
                        let v = f.st(i);
                        f.set_st(0, v);
                    });
                }
            }
            // DB: FNINIT / FNCLEX / FUCOMI etc.
            0xDB => match (reg, rm) {
                (4, 2) => with_fpu(|f| f.clear_exceptions()), // FNCLEX
                (4, 3) => reset_fpu(),                         // FNINIT
                _ => self.fpu_ud(start_eip, op),
            },
            // DC: arithmetic with the destination being ST(i) (reverse sense for
            // the non-commutative ops, per the SDM).
            0xDC => with_fpu(|f| {
                let st0 = f.st(0);
                let sti = f.st(i);
                match reg {
                    0 => f.set_st(i, sti + st0), // FADD ST(i),ST
                    1 => f.set_st(i, sti * st0), // FMUL ST(i),ST
                    4 => f.set_st(i, sti - st0), // FSUB ST(i),ST  (ST(i)=ST(i)-ST(0))
                    5 => f.set_st(i, st0 - sti), // FSUBR ST(i),ST (ST(i)=ST(0)-ST(i))
                    6 => f.set_st(i, sti / st0), // FDIV ST(i),ST
                    7 => f.set_st(i, st0 / sti), // FDIVR ST(i),ST
                    _ => {}
                }
            }),
            // DD: FFREE ST(i), FST/FSTP ST(i), FUCOM/FUCOMP ST(i).
            0xDD => match reg {
                0 => with_fpu(|f| f.free(i)), // FFREE ST(i)
                2 => with_fpu(|f| {
                    let v = f.st(0);
                    f.set_st(i, v);
                }), // FST ST(i)
                3 => with_fpu(|f| {
                    let v = f.st(0);
                    f.set_st(i, v);
                    f.pop();
                }), // FSTP ST(i)
                4 => with_fpu(|f| {
                    let st0 = f.st(0);
                    let sti = f.st(i);
                    f.set_compare(st0, sti);
                }), // FUCOM ST(i)
                5 => with_fpu(|f| {
                    let st0 = f.st(0);
                    let sti = f.st(i);
                    f.set_compare(st0, sti);
                    f.pop();
                }), // FUCOMP ST(i)
                _ => self.fpu_ud(start_eip, op),
            },
            // DE: arithmetic-and-pop (FADDP/FMULP/FSUBP/FDIVP) + FCOMPP.
            0xDE => match reg {
                0 => with_fpu(|f| {
                    let v = f.st(i) + f.st(0);
                    f.set_st(i, v);
                    f.pop();
                }), // FADDP
                1 => with_fpu(|f| {
                    let v = f.st(i) * f.st(0);
                    f.set_st(i, v);
                    f.pop();
                }), // FMULP
                3 => {
                    // FCOMPP (only valid with rm==1): compare then pop twice.
                    if rm == 1 {
                        with_fpu(|f| {
                            let st0 = f.st(0);
                            let st1 = f.st(1);
                            f.set_compare(st0, st1);
                            f.pop();
                            f.pop();
                        });
                    } else {
                        self.fpu_ud(start_eip, op);
                    }
                }
                4 => with_fpu(|f| {
                    let v = f.st(i) - f.st(0);
                    f.set_st(i, v);
                    f.pop();
                }), // FSUBP (ST(i)=ST(i)-ST(0))
                5 => with_fpu(|f| {
                    let v = f.st(0) - f.st(i);
                    f.set_st(i, v);
                    f.pop();
                }), // FSUBRP
                6 => with_fpu(|f| {
                    let v = f.st(i) / f.st(0);
                    f.set_st(i, v);
                    f.pop();
                }), // FDIVP
                7 => with_fpu(|f| {
                    let v = f.st(0) / f.st(i);
                    f.set_st(i, v);
                    f.pop();
                }), // FDIVRP
                _ => self.fpu_ud(start_eip, op),
            },
            // DF: FNSTSW AX, FUCOMIP/FCOMIP (rare).
            0xDF => match (reg, rm) {
                (4, 0) => {
                    // FNSTSW AX
                    let sw = with_fpu(|f| f.status_word());
                    self.set_reg16(EAX, sw as u32);
                }
                _ => {
                    // FCOMIP/FUCOMIP ST(0),ST(i): compare and set EFLAGS, pop.
                    if reg == 5 || reg == 6 {
                        let (c0, c2, c3) = with_fpu(|f| {
                            let st0 = f.st(0);
                            let sti = f.st(i);
                            f.set_compare(st0, sti);
                            f.pop();
                            let sw = f.status_word();
                            (sw & SW_C0 != 0, sw & SW_C2 != 0, sw & SW_C3 != 0)
                        });
                        // EFLAGS: ZF<-C3, PF<-C2, CF<-C0 (SDM mapping).
                        self.set_flag(ZF, c3);
                        self.set_flag(PF, c2);
                        self.set_flag(CF, c0);
                    } else {
                        self.fpu_ud(start_eip, op);
                    }
                }
            },
            _ => self.fpu_ud(start_eip, op),
        }
    }

    /// Read an 80-bit extended-precision float and narrow to f64 (best effort).
    fn read_f80(&mut self, bus: &mut impl Bus, lin: u32) -> f64 {
        let m_lo = self.read_mem(bus, lin, 4) as u64;
        let m_hi = self.read_mem(bus, lin.wrapping_add(4), 4) as u64;
        let se = self.read_mem(bus, lin.wrapping_add(8), 2) as u16;
        let mantissa = m_lo | (m_hi << 32);
        let sign = (se >> 15) & 1;
        let exp = (se & 0x7FFF) as i32;
        if exp == 0 && mantissa == 0 {
            return if sign == 1 { -0.0 } else { 0.0 };
        }
        // value = (-1)^sign * mantissa/2^63 * 2^(exp-16383)
        let frac = mantissa as f64 / (1u64 << 63) as f64;
        let val = frac * 2f64.powi(exp - 16383);
        if sign == 1 { -val } else { val }
    }

    /// Write an f64 as an 80-bit extended-precision float (best effort).
    fn write_f80(&mut self, bus: &mut impl Bus, lin: u32, v: f64) {
        let (mantissa, se) = f64_to_f80(v);
        self.write_mem(bus, lin, 4, mantissa as u32);
        self.write_mem(bus, lin.wrapping_add(4), 4, (mantissa >> 32) as u32);
        self.write_mem(bus, lin.wrapping_add(8), 2, se as u32);
    }

    /// Raise the documented #UD seam for an unimplemented FPU encoding.
    #[inline]
    fn fpu_ud(&mut self, start_eip: u32, op: u8) {
        self.eip = start_eip;
        self.raise(Exception::InvalidOpcode, 0, op);
    }

    /// Public wrapper so [`do_shift`] can set SZP (the inherent helper is
    /// private to `state.rs`); recomputes the same SF/ZF/PF subset.
    fn set_szp_pub(&mut self, res: u32, size: u8) {
        let m = Cpu::size_mask(size);
        let r = res & m;
        let sign = m ^ (m >> 1);
        self.set_flag(ZF, r == 0);
        self.set_flag(SF, r & sign != 0);
        self.set_flag(PF, (r as u8).count_ones() % 2 == 0);
    }
}

/// Convert an x87 register value to a 32-bit integer with round-to-nearest
/// (the default rounding mode). NaN/out-of-range collapse to the "integer
/// indefinite" value, as a real FPU would store on an unmasked invalid-op.
#[inline]
fn fpu_to_i32(v: f64) -> i32 {
    let r = v.round_ties_even();
    if r.is_nan() || r > i32::MAX as f64 || r < i32::MIN as f64 {
        i32::MIN // integer indefinite
    } else {
        r as i32
    }
}

/// Convert an x87 register value to a 64-bit integer (round-to-nearest).
#[inline]
fn fpu_to_i64(v: f64) -> i64 {
    let r = v.round_ties_even();
    if r.is_nan() || r >= 9.223_372_036_854_776e18 || r < -9.223_372_036_854_776e18 {
        i64::MIN
    } else {
        r as i64
    }
}

/// Encode an f64 as an 80-bit extended float: returns (64-bit mantissa with the
/// explicit integer bit, 16-bit sign+exponent). Best effort — normal finite
/// values only; zero/sub-normal collapse to a clean zero.
fn f64_to_f80(v: f64) -> (u64, u16) {
    if v == 0.0 {
        let sign = if v.is_sign_negative() { 0x8000 } else { 0 };
        return (0, sign);
    }
    let sign = if v < 0.0 { 0x8000u16 } else { 0 };
    let a = v.abs();
    let e = a.log2().floor() as i32; // unbiased exponent
    let exp80 = (e + 16383) as u16 & 0x7FFF;
    // mantissa normalized to [1,2): a / 2^e, scaled into the explicit-1 64-bit field.
    let frac = a / 2f64.powi(e); // in [1,2)
    let mantissa = (frac * (1u64 << 63) as f64) as u64;
    (mantissa, sign | exp80)
}

/// Sign-extend a value of `size` bytes to a full 32-bit word.
#[inline]
fn sign_ext(v: u32, size: u8) -> u32 {
    match size {
        1 => v as u8 as i8 as i32 as u32,
        2 => v as u16 as i16 as i32 as u32,
        _ => v,
    }
}

// ============================ SSE free helpers ============================

/// Reinterpret 16 raw little-endian bytes as four f32 lanes.
fn bytes_to_f32s(b: [u8; 16]) -> [f32; 4] {
    let mut out = [0f32; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let j = i * 4;
        *o = f32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]);
    }
    out
}
/// Reinterpret 16 raw little-endian bytes as two f64 lanes.
fn bytes_to_f64s(b: [u8; 16]) -> [f64; 2] {
    [
        f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        f64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
    ]
}
/// Reinterpret 16 raw little-endian bytes as four u32 lanes.
fn bytes_to_u32s(b: [u8; 16]) -> [u32; 4] {
    let mut out = [0u32; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let j = i * 4;
        *o = u32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]);
    }
    out
}
/// Reinterpret 16 raw little-endian bytes as two u64 lanes.
fn bytes_to_u64s(b: [u8; 16]) -> [u64; 2] {
    [
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
    ]
}

/// Apply one SSE binary/unary float op (selected by the 0x0F opcode byte) at
/// f64 precision. For packed-single callers the caller down-converts the result
/// to f32. `unary` ops (SQRT/RCP/RSQRT) ignore `a` and operate on `b`.
fn sse_op(op2: u8, a: f64, b: f64, _unary: bool) -> f64 {
    match op2 {
        0x58 => a + b,            // ADD
        0x59 => a * b,            // MUL
        0x5C => a - b,            // SUB
        0x5E => a / b,            // DIV
        // MIN/MAX: the SDM returns the *second* operand if either is NaN or
        // they are equal; we approximate with the host min/max which is close
        // enough for game branch behaviour.
        0x5D => {
            if a.is_nan() || b.is_nan() {
                b
            } else if a < b {
                a
            } else {
                b
            }
        }
        0x5F => {
            if a.is_nan() || b.is_nan() {
                b
            } else if a > b {
                a
            } else {
                b
            }
        }
        0x51 => b.sqrt(),         // SQRT (operates on the source)
        0x53 => 1.0 / b,          // RCP  (approximate reciprocal)
        0x52 => 1.0 / b.sqrt(),   // RSQRT (approximate)
        _ => b,
    }
}

/// Evaluate a CMPxx imm8 predicate (low 3 bits) per the SDM.
fn cmp_predicate(a: f64, b: f64, imm: u8) -> bool {
    let unord = a.is_nan() || b.is_nan();
    match imm & 7 {
        0 => a == b,                 // EQ (ordered)
        1 => a < b,                  // LT
        2 => a <= b,                 // LE
        3 => unord,                  // UNORD
        4 => a != b || unord,        // NEQ (unordered)
        5 => a >= b || unord,        // NLT  (not-less-than, true if unordered)
        6 => a > b || unord,         // NLE  (not-less-or-equal)
        _ => !unord,                 // ORD
    }
}

/// Convert an f64 to a signed i32 for the CVT*2SI family. `truncate` selects
/// round-toward-zero (CVTT*); otherwise round-to-nearest (the default MXCSR
/// mode, which is what game code uses). Out-of-range / NaN yields the SSE
/// "integer indefinite" 0x8000_0000.
fn convert_to_i32(v: f64, truncate: bool) -> i32 {
    if v.is_nan() {
        return i32::MIN;
    }
    let r = if truncate { v.trunc() } else { v.round_ties_even() };
    if r >= i32::MAX as f64 + 1.0 || r < i32::MIN as f64 {
        i32::MIN
    } else {
        r as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xbox::Xbox;

    /// A harness: an `Xbox` with a small program in RAM and the CPU pointed at
    /// it in flat 32-bit protected mode (so we exercise 32-bit decoding without
    /// modelling a GDT).
    fn harness(program: &[u8]) -> Xbox {
        let mut xb = Xbox::new();
        let base = 0x1_0000u32;
        for (i, &b) in program.iter().enumerate() {
            xb.mem.ram_write8(base + i as u32, b as u32);
        }
        // Flat protected mode: PE=1, all segment bases 0, CS:EIP -> program.
        xb.cpu.cr[0] |= CR0_PE;
        for s in 0..6 {
            xb.cpu.seg_base[s] = 0;
            xb.cpu.seg_sel[s] = 0x08;
        }
        xb.cpu.eip = base;
        xb.cpu.set_reg32(ESP, 0x2_0000);
        xb
    }

    fn run(xb: &mut Xbox, n: usize) {
        for _ in 0..n {
            let mut cpu = std::mem::take(&mut xb.cpu);
            cpu.step(xb);
            xb.cpu = cpu;
        }
    }

    #[test]
    fn mov_imm32_and_add() {
        // mov eax, 5 ; mov ebx, 9 ; add eax, ebx
        let mut xb = harness(&[
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
            0xBB, 0x09, 0x00, 0x00, 0x00, // mov ebx,9
            0x01, 0xD8, // add eax,ebx
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 14);
    }

    #[test]
    fn sub_sets_zero_flag() {
        // mov eax,7 ; sub eax,7
        let mut xb = harness(&[0xB8, 0x07, 0x00, 0x00, 0x00, 0x29, 0xC0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(ZF));
    }

    #[test]
    fn xor_self_clears_register() {
        // xor eax,eax
        let mut xb = harness(&[0xB8, 0xFF, 0x00, 0x00, 0x00, 0x31, 0xC0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(ZF));
    }

    #[test]
    fn push_pop_round_trips_through_stack() {
        // mov eax,0xCAFEBABE ; push eax ; pop ebx
        let mut xb = harness(&[
            0xB8, 0xBE, 0xBA, 0xFE, 0xCA, // mov eax,0xCAFEBABE
            0x50, // push eax
            0x5B, // pop ebx
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 0xCAFE_BABE);
        assert_eq!(xb.cpu.reg32(ESP), 0x2_0000, "stack balanced");
    }

    #[test]
    fn inc_dec_preserve_carry() {
        // stc ; mov eax,0 ; inc eax  — CF must survive INC.
        let mut xb = harness(&[0xF9, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x40]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 1);
        assert!(xb.cpu.flag(CF));
    }

    #[test]
    fn jmp_short_skips_instruction() {
        // jmp +2 (skip the mov) ; mov eax,0xAA ; mov eax,0xBB
        let mut xb = harness(&[
            0xEB, 0x05, // jmp over the 5-byte mov
            0xB8, 0xAA, 0x00, 0x00, 0x00, // mov eax,0xAA (skipped)
            0xB8, 0xBB, 0x00, 0x00, 0x00, // mov eax,0xBB
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0xBB);
    }

    #[test]
    fn conditional_branch_taken_on_zero() {
        // mov eax,0 ; test eax,eax ; jz +5 ; mov eax,1 ; (target) hlt
        let mut xb = harness(&[
            0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax,0
            0x85, 0xC0, // test eax,eax
            0x74, 0x05, // jz +5
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1 (skipped)
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 0, "jz taken, mov skipped");
    }

    #[test]
    fn call_and_ret_near() {
        // call +5 (to ret) ; (filler) hlt ; ret
        // layout: E8 disp32 (5 bytes) -> target at +5 which is the RET.
        let mut xb = harness(&[
            0xE8, 0x00, 0x00, 0x00, 0x00, // call +0 -> next instr (the ret)
            0xC3, // ret
        ]);
        let sp0 = xb.cpu.reg32(ESP);
        run(&mut xb, 2); // call, then ret
        assert_eq!(xb.cpu.reg32(ESP), sp0, "stack balanced after call/ret");
    }

    #[test]
    fn shift_left_sets_carry() {
        // mov eax,0x80000000 ; shl eax,1  -> 0, CF=1
        let mut xb = harness(&[0xB8, 0x00, 0x00, 0x00, 0x80, 0xD1, 0xE0]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0);
        assert!(xb.cpu.flag(CF));
    }

    #[test]
    fn unsigned_mul_and_div() {
        // mov eax,200 ; mov ebx,3 ; mul ebx (EAX*EBX) ; mov ebx,7 ; div ebx
        let mut xb = harness(&[
            0xB8, 0xC8, 0x00, 0x00, 0x00, // mov eax,200
            0xBB, 0x03, 0x00, 0x00, 0x00, // mov ebx,3
            0xF7, 0xE3, // mul ebx -> EDX:EAX = 600
            0xBB, 0x07, 0x00, 0x00, 0x00, // mov ebx,7
            0xF7, 0xF3, // div ebx -> 600/7
        ]);
        run(&mut xb, 5);
        assert_eq!(xb.cpu.reg32(EAX), 600 / 7);
        assert_eq!(xb.cpu.reg32(EDX), 600 % 7);
    }

    #[test]
    fn divide_by_zero_faults() {
        // mov eax,1 ; xor edx,edx ; xor ebx,ebx ; div ebx -> #DE
        let mut xb = harness(&[
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
            0x31, 0xD2, // xor edx,edx
            0x31, 0xDB, // xor ebx,ebx
            0xF7, 0xF3, // div ebx
        ]);
        run(&mut xb, 4);
        assert_eq!(xb.cpu.fault.unwrap().vector, 0, "#DE raised");
    }

    #[test]
    fn unimplemented_opcode_raises_ud() {
        // 0x82 is an invalid opcode in the modern ISA.
        let mut xb = harness(&[0x82]);
        run(&mut xb, 1);
        let f = xb.cpu.fault.unwrap();
        assert_eq!(f.vector, 6, "#UD");
        assert_eq!(f.opcode, 0x82);
    }

    #[test]
    fn mov_to_cr0_enables_protected_mode_bit() {
        // We start in flat protected mode already; verify mov CR0 round-trips.
        // mov eax, cr0 ; mov cr0, eax
        let mut xb = harness(&[0x0F, 0x20, 0xC0, 0x0F, 0x22, 0xC0]);
        run(&mut xb, 2);
        assert!(xb.cpu.cr[0] & CR0_PE != 0);
    }

    #[test]
    fn movzx_zero_extends_byte() {
        // mov eax,0xFFFFFFFF ; movzx ebx, al  -> 0xFF
        let mut xb = harness(&[
            0xB8, 0xFF, 0xFF, 0xFF, 0xFF, // mov eax,-1
            0x0F, 0xB6, 0xD8, // movzx ebx, al
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EBX), 0xFF);
    }

    #[test]
    fn rep_movs_copies_buffer() {
        // Set up source bytes at 0x1000, copy 4 dwords to 0x2000 with rep movsd.
        // mov esi,0x1000 ; mov edi,0x2000 ; mov ecx,4 ; cld ; rep movsd
        let mut xb = harness(&[
            0xBE, 0x00, 0x10, 0x00, 0x00, // mov esi,0x1000
            0xBF, 0x00, 0x20, 0x00, 0x00, // mov edi,0x2000
            0xB9, 0x04, 0x00, 0x00, 0x00, // mov ecx,4
            0xFC, // cld
            0xF3, 0xA5, // rep movsd
        ]);
        for i in 0..4u32 {
            xb.mem.ram_write8(0x1000 + i * 4, (0x10 + i) & 0xFF);
            xb.mem.ram_write8(0x1000 + i * 4 + 1, 0);
            xb.mem.ram_write8(0x1000 + i * 4 + 2, 0);
            xb.mem.ram_write8(0x1000 + i * 4 + 3, 0);
        }
        run(&mut xb, 5);
        for i in 0..4u32 {
            assert_eq!(xb.mem.ram_read8(0x2000 + i * 4), 0x10 + i);
        }
        assert_eq!(xb.cpu.reg32(ECX), 0, "ecx drained");
        assert_eq!(xb.cpu.reg32(ESI), 0x1010);
        assert_eq!(xb.cpu.reg32(EDI), 0x2010);
    }

    #[test]
    fn rep_stos_fills_buffer() {
        // mov eax,0xAB ; mov edi,0x3000 ; mov ecx,5 ; cld ; rep stosb
        let mut xb = harness(&[
            0xB8, 0xAB, 0x00, 0x00, 0x00, // mov eax,0xAB
            0xBF, 0x00, 0x30, 0x00, 0x00, // mov edi,0x3000
            0xB9, 0x05, 0x00, 0x00, 0x00, // mov ecx,5
            0xFC, // cld
            0xF3, 0xAA, // rep stosb
        ]);
        run(&mut xb, 5);
        for i in 0..5u32 {
            assert_eq!(xb.mem.ram_read8(0x3000 + i), 0xAB);
        }
        assert_eq!(xb.cpu.reg32(ECX), 0);
        assert_eq!(xb.cpu.reg32(EDI), 0x3005);
    }

    #[test]
    fn repne_scas_finds_byte() {
        // Find byte 0x07 in a buffer. al=0x07, edi=0x4000, ecx=8, cld, repne scasb.
        let mut xb = harness(&[
            0xB8, 0x07, 0x00, 0x00, 0x00, // mov eax,0x07
            0xBF, 0x00, 0x40, 0x00, 0x00, // mov edi,0x4000
            0xB9, 0x08, 0x00, 0x00, 0x00, // mov ecx,8
            0xFC, // cld
            0xF2, 0xAE, // repne scasb
        ]);
        let data = [1u32, 2, 3, 7, 9, 9, 9, 9];
        for (i, &b) in data.iter().enumerate() {
            xb.mem.ram_write8(0x4000 + i as u32, b);
        }
        run(&mut xb, 5);
        // 0x07 is at index 3; scan advances EDI past it -> 0x4004, ecx=8-4=4.
        assert_eq!(xb.cpu.reg32(EDI), 0x4004);
        assert_eq!(xb.cpu.reg32(ECX), 4);
        assert!(xb.cpu.flag(ZF), "match sets ZF");
    }

    #[test]
    fn repe_cmps_compares_buffers() {
        // Compare two equal 4-byte buffers, then differ. esi=0x5000, edi=0x6000.
        let mut xb = harness(&[
            0xBE, 0x00, 0x50, 0x00, 0x00, // mov esi,0x5000
            0xBF, 0x00, 0x60, 0x00, 0x00, // mov edi,0x6000
            0xB9, 0x04, 0x00, 0x00, 0x00, // mov ecx,4
            0xFC, // cld
            0xF3, 0xA6, // repe cmpsb
        ]);
        let a = [1u32, 2, 3, 4];
        let b = [1u32, 2, 9, 4];
        for i in 0..4 {
            xb.mem.ram_write8(0x5000 + i as u32, a[i]);
            xb.mem.ram_write8(0x6000 + i as u32, b[i]);
        }
        run(&mut xb, 5);
        // Equal at idx 0,1; mismatch at idx 2 -> stop after comparing 3 bytes.
        assert_eq!(xb.cpu.reg32(ECX), 1, "stopped at first mismatch");
        assert!(!xb.cpu.flag(ZF), "mismatch clears ZF");
    }

    #[test]
    fn cmovcc_moves_on_condition() {
        // mov eax,1 ; mov ebx,0x55 ; test eax,eax ; cmovnz ecx,ebx
        let mut xb = harness(&[
            0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
            0xBB, 0x55, 0x00, 0x00, 0x00, // mov ebx,0x55
            0x85, 0xC0, // test eax,eax
            0x0F, 0x45, 0xCB, // cmovnz ecx,ebx
        ]);
        run(&mut xb, 4);
        assert_eq!(xb.cpu.reg32(ECX), 0x55, "moved because nz");
    }

    #[test]
    fn cmovcc_skips_when_false() {
        // mov eax,0 ; mov ebx,0x55 ; test eax,eax ; cmovnz ecx,ebx (skipped)
        let mut xb = harness(&[
            0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax,0
            0xBB, 0x55, 0x00, 0x00, 0x00, // mov ebx,0x55
            0x85, 0xC0, // test eax,eax
            0x0F, 0x45, 0xCB, // cmovnz ecx,ebx
        ]);
        run(&mut xb, 4);
        assert_eq!(xb.cpu.reg32(ECX), 0, "not moved because zero");
    }

    #[test]
    fn bt_and_bts_set_carry_and_bit() {
        // mov eax,0x04 ; bt eax,2 (CF=1) ; bts eax,3 (set bit3)
        let mut xb = harness(&[
            0xB8, 0x04, 0x00, 0x00, 0x00, // mov eax,4 (bit2 set)
            0x0F, 0xBA, 0xE0, 0x02, // bt eax,2
        ]);
        run(&mut xb, 2);
        assert!(xb.cpu.flag(CF), "bit2 was set");
        // Now BTS bit 3.
        let mut xb = harness(&[
            0xB8, 0x04, 0x00, 0x00, 0x00, // mov eax,4
            0x0F, 0xBA, 0xE8, 0x03, // bts eax,3
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0x0C, "bit3 set");
        assert!(!xb.cpu.flag(CF), "bit3 was clear before");
    }

    #[test]
    fn shld_shifts_in_high_bits() {
        // eax=0xF0000000, ebx=0x12345678 ; shld eax,ebx,4 -> 0x00000001
        let mut xb = harness(&[
            0xB8, 0x00, 0x00, 0x00, 0xF0, // mov eax,0xF0000000
            0xBB, 0x78, 0x56, 0x34, 0x12, // mov ebx,0x12345678
            0x0F, 0xA4, 0xD8, 0x04, // shld eax,ebx,4
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 0x0000_0001);
    }

    #[test]
    fn shrd_shifts_in_low_bits() {
        // eax=0x0000000F, ebx=0x12345678 ; shrd eax,ebx,4 -> 0x80000000
        let mut xb = harness(&[
            0xB8, 0x0F, 0x00, 0x00, 0x00, // mov eax,0x0000000F
            0xBB, 0x78, 0x56, 0x34, 0x12, // mov ebx,0x12345678
            0x0F, 0xAC, 0xD8, 0x04, // shrd eax,ebx,4
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EAX), 0x8000_0000);
    }

    #[test]
    fn bswap_reverses_bytes() {
        // mov eax,0x11223344 ; bswap eax -> 0x44332211
        let mut xb = harness(&[
            0xB8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
            0x0F, 0xC8, // bswap eax
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 0x4433_2211);
    }

    #[test]
    fn imul_two_operand() {
        // mov eax,7 ; imul eax,eax,6 -> 42
        let mut xb = harness(&[
            0xB8, 0x07, 0x00, 0x00, 0x00, // mov eax,7
            0x6B, 0xC0, 0x06, // imul eax,eax,6
        ]);
        run(&mut xb, 2);
        assert_eq!(xb.cpu.reg32(EAX), 42);
        // imul r,r/m form (0F AF): mov ebx,5 ; imul ebx,eax (5*42=210)
        let mut xb = harness(&[
            0xB8, 0x2A, 0x00, 0x00, 0x00, // mov eax,42
            0xBB, 0x05, 0x00, 0x00, 0x00, // mov ebx,5
            0x0F, 0xAF, 0xD8, // imul ebx,eax
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 210);
    }

    #[test]
    fn leave_restores_frame() {
        // Simulate: push EBP frame, set ESP below, then LEAVE.
        // mov ebp,0x1FF0 ; write saved-ebp at [0x1FF0]=0xAAAA ; mov esp,0x1F00 ; leave
        let mut xb = harness(&[
            0xBD, 0xF0, 0x1F, 0x00, 0x00, // mov ebp,0x1FF0
            0xBC, 0x00, 0x1F, 0x00, 0x00, // mov esp,0x1F00
            0xC9, // leave
        ]);
        xb.mem.ram_write8(0x1FF0, 0xAA);
        xb.mem.ram_write8(0x1FF1, 0xAA);
        xb.mem.ram_write8(0x1FF2, 0x00);
        xb.mem.ram_write8(0x1FF3, 0x00);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(ESP), 0x1FF4, "esp = old ebp + 4");
        assert_eq!(xb.cpu.reg32(EBP), 0x0000_AAAA, "ebp popped");
    }

    #[test]
    fn pusha_popa_round_trip() {
        // Set distinct regs, pusha, clobber, popa, verify restored.
        // mov eax,0x11 ; mov ecx,0x22 ; pusha ; mov eax,0 ; mov ecx,0 ; popa
        let mut xb = harness(&[
            0xB8, 0x11, 0x00, 0x00, 0x00, // mov eax,0x11
            0xB9, 0x22, 0x00, 0x00, 0x00, // mov ecx,0x22
            0x60, // pusha
            0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax,0
            0xB9, 0x00, 0x00, 0x00, 0x00, // mov ecx,0
            0x61, // popa
        ]);
        let sp0 = xb.cpu.reg32(ESP);
        run(&mut xb, 6);
        assert_eq!(xb.cpu.reg32(EAX), 0x11);
        assert_eq!(xb.cpu.reg32(ECX), 0x22);
        assert_eq!(xb.cpu.reg32(ESP), sp0, "stack balanced");
    }

    #[test]
    fn xadd_swaps_and_adds() {
        // mov eax,5 ; mov ebx,3 ; xadd ebx,eax -> ebx=8, eax=3(old ebx)
        let mut xb = harness(&[
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
            0xBB, 0x03, 0x00, 0x00, 0x00, // mov ebx,3
            0x0F, 0xC1, 0xC3, // xadd ebx,eax
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 8, "sum -> dst");
        assert_eq!(xb.cpu.reg32(EAX), 3, "old dst -> src");
    }

    #[test]
    fn cmpxchg_matches_and_swaps() {
        // eax=5, ebx(dst)=5, ecx=9 ; cmpxchg ebx,ecx -> equal so ebx=9, ZF=1
        let mut xb = harness(&[
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
            0xBB, 0x05, 0x00, 0x00, 0x00, // mov ebx,5
            0xB9, 0x09, 0x00, 0x00, 0x00, // mov ecx,9
            0x0F, 0xB1, 0xCB, // cmpxchg ebx,ecx
        ]);
        run(&mut xb, 4);
        assert!(xb.cpu.flag(ZF), "acc == dst");
        assert_eq!(xb.cpu.reg32(EBX), 9, "src stored");
        // Mismatch case: eax=5, ebx=7 ; cmpxchg loads ebx into eax.
        let mut xb = harness(&[
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
            0xBB, 0x07, 0x00, 0x00, 0x00, // mov ebx,7
            0xB9, 0x09, 0x00, 0x00, 0x00, // mov ecx,9
            0x0F, 0xB1, 0xCB, // cmpxchg ebx,ecx
        ]);
        run(&mut xb, 4);
        assert!(!xb.cpu.flag(ZF));
        assert_eq!(xb.cpu.reg32(EAX), 7, "dst loaded into acc");
        assert_eq!(xb.cpu.reg32(EBX), 7, "dst unchanged");
    }

    #[test]
    fn bsf_bsr_find_bits() {
        // mov eax,0x100 ; bsf ebx,eax -> 8 ; bsr ecx,eax -> 8
        let mut xb = harness(&[
            0xB8, 0x00, 0x01, 0x00, 0x00, // mov eax,0x100
            0x0F, 0xBC, 0xD8, // bsf ebx,eax
            0x0F, 0xBD, 0xC8, // bsr ecx,eax
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 8);
        assert_eq!(xb.cpu.reg32(ECX), 8);
        assert!(!xb.cpu.flag(ZF));
    }

    // ============================ x87 FPU tests ============================
    //
    // The FPU register stack lives in a thread-local (see `super::fpu`), shared
    // across tests on the same thread; each test calls `reset_fpu()` first to
    // start from a clean FNINIT state.
    use super::super::fpu::{reset_fpu, with_fpu, SW_C0, SW_C2, SW_C3};

    #[test]
    fn fld_fstp_round_trips_m32() {
        reset_fpu();
        // mov dword[0x1000]=3.5(f32 bits=0x40600000) ; fld dword[0x1000] ;
        // fstp dword[0x1004]
        let mut xb = harness(&[
            0xD9, 0x05, 0x00, 0x10, 0x00, 0x00, // fld dword [0x1000]
            0xD9, 0x1D, 0x04, 0x10, 0x00, 0x00, // fstp dword [0x1004]
        ]);
        let bits = 3.5f32.to_bits();
        xb.mem.ram_write8(0x1000, bits & 0xFF);
        xb.mem.ram_write8(0x1001, (bits >> 8) & 0xFF);
        xb.mem.ram_write8(0x1002, (bits >> 16) & 0xFF);
        xb.mem.ram_write8(0x1003, (bits >> 24) & 0xFF);
        run(&mut xb, 2);
        let out = xb.mem.ram_read8(0x1004)
            | (xb.mem.ram_read8(0x1005) << 8)
            | (xb.mem.ram_read8(0x1006) << 16)
            | (xb.mem.ram_read8(0x1007) << 24);
        assert_eq!(f32::from_bits(out), 3.5);
    }

    #[test]
    fn fld_fstp_round_trips_m64() {
        reset_fpu();
        // fld qword[0x1000] ; fstp qword[0x1008]
        let mut xb = harness(&[
            0xDD, 0x05, 0x00, 0x10, 0x00, 0x00, // fld qword [0x1000]
            0xDD, 0x1D, 0x08, 0x10, 0x00, 0x00, // fstp qword [0x1008]
        ]);
        let bits = 12.75f64.to_bits();
        for i in 0..8u32 {
            xb.mem.ram_write8(0x1000 + i, ((bits >> (i * 8)) & 0xFF) as u32);
        }
        run(&mut xb, 2);
        let mut out = 0u64;
        for i in 0..8u32 {
            out |= (xb.mem.ram_read8(0x1008 + i) as u64) << (i * 8);
        }
        assert_eq!(f64::from_bits(out), 12.75);
    }

    #[test]
    fn fadd_fmul_fdiv_m64() {
        reset_fpu();
        // fld qword[0x1000] (=10) ; fadd qword[0x1008] (=4) -> 14
        // then fmul by 2 -> 28, fdiv by 7 -> 4
        let mut xb = harness(&[
            0xDD, 0x05, 0x00, 0x10, 0x00, 0x00, // fld qword [0x1000]
            0xDC, 0x05, 0x08, 0x10, 0x00, 0x00, // fadd qword [0x1008]
            0xDC, 0x0D, 0x10, 0x10, 0x00, 0x00, // fmul qword [0x1010]
            0xDC, 0x35, 0x18, 0x10, 0x00, 0x00, // fdiv qword [0x1018]
        ]);
        let put = |xb: &mut Xbox, addr: u32, v: f64| {
            let b = v.to_bits();
            for i in 0..8u32 {
                xb.mem.ram_write8(addr + i, ((b >> (i * 8)) & 0xFF) as u32);
            }
        };
        put(&mut xb, 0x1000, 10.0);
        put(&mut xb, 0x1008, 4.0);
        put(&mut xb, 0x1010, 2.0);
        put(&mut xb, 0x1018, 7.0);
        run(&mut xb, 4);
        let got = with_fpu(|f| f.st(0));
        assert_eq!(got, 4.0, "(10+4)*2/7 = 4");
    }

    #[test]
    fn fild_fistp_integer_round_trip() {
        reset_fpu();
        // fild dword[0x1000] (=42) ; fistp dword[0x1004]
        let mut xb = harness(&[
            0xDB, 0x05, 0x00, 0x10, 0x00, 0x00, // fild dword [0x1000]
            0xDB, 0x1D, 0x04, 0x10, 0x00, 0x00, // fistp dword [0x1004]
        ]);
        xb.mem.ram_write8(0x1000, 42);
        run(&mut xb, 2);
        let out = xb.mem.ram_read8(0x1004)
            | (xb.mem.ram_read8(0x1005) << 8)
            | (xb.mem.ram_read8(0x1006) << 16)
            | (xb.mem.ram_read8(0x1007) << 24);
        assert_eq!(out, 42);
    }

    #[test]
    fn fcom_fnstsw_sets_condition_bits() {
        reset_fpu();
        // Load 1.0 (less than mem 2.0): fld1 ; fcom qword[0x1000] ; fnstsw ax
        // ST(0)=1.0 < 2.0 -> C0 set, C3 clear.
        let mut xb = harness(&[
            0xD9, 0xE8, // fld1
            0xDC, 0x15, 0x00, 0x10, 0x00, 0x00, // fcom qword [0x1000]
            0xDF, 0xE0, // fnstsw ax
        ]);
        let b = 2.0f64.to_bits();
        for i in 0..8u32 {
            xb.mem.ram_write8(0x1000 + i, ((b >> (i * 8)) & 0xFF) as u32);
        }
        run(&mut xb, 3);
        let ax = xb.cpu.reg16(EAX) as u16;
        assert_eq!(ax & SW_C0, SW_C0, "less -> C0 set");
        assert_eq!(ax & SW_C3, 0, "less -> C3 clear");
        assert_eq!(ax & SW_C2, 0, "ordered -> C2 clear");

        // Equal case: fld1 ; fld1 ; fcompp ; fnstsw ax -> C3 set.
        reset_fpu();
        let mut xb = harness(&[
            0xD9, 0xE8, // fld1
            0xD9, 0xE8, // fld1
            0xDE, 0xD9, // fcompp
            0xDF, 0xE0, // fnstsw ax
        ]);
        run(&mut xb, 4);
        let ax = xb.cpu.reg16(EAX) as u16;
        assert_eq!(ax & SW_C3, SW_C3, "equal -> C3 set");
        assert_eq!(ax & SW_C0, 0, "equal -> C0 clear");
    }

    #[test]
    fn fcom_greater_clears_all() {
        reset_fpu();
        // fld qword[0x1000] (=5) ; fcom qword[0x1008] (=2) -> all C clear
        let mut xb = harness(&[
            0xDD, 0x05, 0x00, 0x10, 0x00, 0x00, // fld qword [0x1000]
            0xDC, 0x15, 0x08, 0x10, 0x00, 0x00, // fcom qword [0x1008]
            0xDF, 0xE0, // fnstsw ax
        ]);
        let put = |xb: &mut Xbox, addr: u32, v: f64| {
            let b = v.to_bits();
            for i in 0..8u32 {
                xb.mem.ram_write8(addr + i, ((b >> (i * 8)) & 0xFF) as u32);
            }
        };
        put(&mut xb, 0x1000, 5.0);
        put(&mut xb, 0x1008, 2.0);
        run(&mut xb, 3);
        let ax = xb.cpu.reg16(EAX) as u16;
        assert_eq!(ax & (SW_C0 | SW_C2 | SW_C3), 0, "greater -> C0=C2=C3=0");
    }

    #[test]
    fn fldcw_fnstcw_round_trip() {
        reset_fpu();
        // mov [0x1000]=0x027F ; fldcw [0x1000] ; fnstcw [0x1004]
        let mut xb = harness(&[
            0xD9, 0x2D, 0x00, 0x10, 0x00, 0x00, // fldcw [0x1000]
            0xD9, 0x3D, 0x04, 0x10, 0x00, 0x00, // fnstcw [0x1004]
        ]);
        xb.mem.ram_write8(0x1000, 0x7F);
        xb.mem.ram_write8(0x1001, 0x02);
        run(&mut xb, 2);
        let cw = xb.mem.ram_read8(0x1004) | (xb.mem.ram_read8(0x1005) << 8);
        assert_eq!(cw, 0x027F);
        assert_eq!(with_fpu(|f| f.control_word()), 0x027F);
    }

    #[test]
    fn fninit_resets_fpu() {
        reset_fpu();
        // Dirty the control word, push values, then FNINIT (DB E3).
        with_fpu(|f| {
            f.set_control_word(0x1234);
            f.push(9.0);
        });
        let mut xb = harness(&[0xDB, 0xE3]); // fninit
        run(&mut xb, 1);
        assert_eq!(with_fpu(|f| f.control_word()), super::super::fpu::CW_DEFAULT);
        assert_eq!(with_fpu(|f| f.tag_word()), 0xFFFF, "stack empty");
    }

    #[test]
    fn fxch_swaps_top() {
        reset_fpu();
        // fld1 (ST0=1) ; fldz (ST0=0,ST1=1) ; fxch ST(1) -> ST0=1
        let mut xb = harness(&[
            0xD9, 0xE8, // fld1
            0xD9, 0xEE, // fldz
            0xD9, 0xC9, // fxch st(1)
        ]);
        run(&mut xb, 3);
        assert_eq!(with_fpu(|f| f.st(0)), 1.0);
        assert_eq!(with_fpu(|f| f.st(1)), 0.0);
    }

    #[test]
    fn faddp_pops_and_adds() {
        reset_fpu();
        // fld1 ; fld1 ; faddp st(1),st0 -> ST0 = 2, stack depth 1
        let mut xb = harness(&[
            0xD9, 0xE8, // fld1
            0xD9, 0xE8, // fld1
            0xDE, 0xC1, // faddp st(1),st  (DE C1)
        ]);
        run(&mut xb, 3);
        assert_eq!(with_fpu(|f| f.st(0)), 2.0);
    }

    #[test]
    fn fsqrt_and_fabs_fchs() {
        reset_fpu();
        // fld qword[0x1000] (=9) ; fsqrt -> 3 ; fchs -> -3 ; fabs -> 3
        let mut xb = harness(&[
            0xDD, 0x05, 0x00, 0x10, 0x00, 0x00, // fld qword [0x1000]
            0xD9, 0xFA, // fsqrt
            0xD9, 0xE0, // fchs
            0xD9, 0xE1, // fabs
        ]);
        let b = 9.0f64.to_bits();
        for i in 0..8u32 {
            xb.mem.ram_write8(0x1000 + i, ((b >> (i * 8)) & 0xFF) as u32);
        }
        run(&mut xb, 4);
        assert_eq!(with_fpu(|f| f.st(0)), 3.0);
    }

    #[test]
    fn fcom_fnstsw_sahf_branch() {
        reset_fpu();
        // The real game idiom: fld1 ; fcom mem(=2) ; fnstsw ax ; sahf ; jb +5.
        // 1.0 < 2.0 -> C0 set -> CF set after SAHF -> JB taken (skip the mov).
        let mut xb = harness(&[
            0xD9, 0xE8, // fld1
            0xDC, 0x15, 0x00, 0x10, 0x00, 0x00, // fcom qword [0x1000]
            0xDF, 0xE0, // fnstsw ax
            0x9E, // sahf
            0x72, 0x05, // jb +5
            0xB8, 0xAA, 0x00, 0x00, 0x00, // mov eax,0xAA (should be skipped)
        ]);
        let b = 2.0f64.to_bits();
        for i in 0..8u32 {
            xb.mem.ram_write8(0x1000 + i, ((b >> (i * 8)) & 0xFF) as u32);
        }
        // Note: FNSTSW writes AX; SAHF then loads AH into flags. AH holds the
        // status word's high byte, where C0 lives at bit 0 of that byte (bit 8
        // of the word) -> maps to CF. Step through fld1/fcom/fnstsw/sahf and
        // verify CF; then step the JB and confirm the mov was skipped.
        run(&mut xb, 4);
        assert!(xb.cpu.flag(CF), "C0 -> CF via SAHF");
        run(&mut xb, 1); // jb +5 — taken
        assert_ne!(xb.cpu.reg32(EAX) & 0xFF, 0xAA, "JB taken, mov skipped");
    }

    #[test]
    fn memory_store_load_via_modrm() {
        // mov eax,0x12345678 ; mov [0x1000], eax ; mov ebx,[0x1000]
        let mut xb = harness(&[
            0xB8, 0x78, 0x56, 0x34, 0x12, // mov eax,0x12345678
            0xA3, 0x00, 0x10, 0x00, 0x00, // mov [0x1000], eax
            0x8B, 0x1D, 0x00, 0x10, 0x00, 0x00, // mov ebx, [0x1000]
        ]);
        run(&mut xb, 3);
        assert_eq!(xb.cpu.reg32(EBX), 0x1234_5678);
        // Verify little-endian byte order in RAM.
        assert_eq!(xb.mem.ram_read8(0x1000), 0x78);
        assert_eq!(xb.mem.ram_read8(0x1003), 0x12);
    }

    // ============================ SSE ============================

    use super::super::sse::{reset_xmm, with_xmm};

    /// Seed 16 bytes of guest RAM (little-endian f32 lanes) and return the
    /// absolute address used.
    fn seed_f32s(xb: &mut Xbox, addr: u32, lanes: [f32; 4]) {
        for (i, v) in lanes.iter().enumerate() {
            for (k, &byte) in v.to_le_bytes().iter().enumerate() {
                xb.mem.ram_write8(addr + i as u32 * 4 + k as u32, byte as u32);
            }
        }
    }

    fn read_ram_f32(xb: &Xbox, addr: u32) -> f32 {
        let b = [
            xb.mem.ram_read8(addr) as u8,
            xb.mem.ram_read8(addr + 1) as u8,
            xb.mem.ram_read8(addr + 2) as u8,
            xb.mem.ram_read8(addr + 3) as u8,
        ];
        f32::from_le_bytes(b)
    }

    #[test]
    fn movaps_reg_mem_roundtrip() {
        reset_xmm();
        // movaps xmm0, [0x1000] ; movaps [0x1010], xmm0
        // 0F 28 /r with disp32 = ModRM 05 -> [disp32]
        let mut xb = harness(&[
            0x0F, 0x28, 0x05, 0x00, 0x10, 0x00, 0x00, // movaps xmm0,[0x1000]
            0x0F, 0x29, 0x05, 0x10, 0x10, 0x00, 0x00, // movaps [0x1010],xmm0
        ]);
        seed_f32s(&mut xb, 0x1000, [1.0, 2.0, 3.0, 4.0]);
        run(&mut xb, 2);
        assert_eq!(with_xmm(|x| x.f32s(0)), [1.0, 2.0, 3.0, 4.0]);
        // Round-tripped 16 bytes out to RAM.
        for (i, want) in [1.0f32, 2.0, 3.0, 4.0].iter().enumerate() {
            assert_eq!(read_ram_f32(&xb, 0x1010 + i as u32 * 4), *want);
        }
    }

    #[test]
    fn movss_load_zeroes_upper_lanes() {
        reset_xmm();
        // Pre-fill xmm0 with junk, then MOVSS xmm0, [mem] must zero lanes 1..3.
        with_xmm(|x| x.set_f32s(0, [9.0, 9.0, 9.0, 9.0]));
        // F3 0F 10 /r, ModRM 05 -> [disp32]
        let mut xb = harness(&[0xF3, 0x0F, 0x10, 0x05, 0x00, 0x10, 0x00, 0x00]);
        seed_f32s(&mut xb, 0x1000, [7.5, 0.0, 0.0, 0.0]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [7.5, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn movss_reg_reg_preserves_upper_lanes() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]); // dest
            x.set_f32s(1, [9.0, 8.0, 7.0, 6.0]); // src
        });
        // F3 0F 10 C1 -> movss xmm0, xmm1 (mod=11, reg=0, rm=1)
        let mut xb = harness(&[0xF3, 0x0F, 0x10, 0xC1]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [9.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn addps_packed_lanes() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]);
            x.set_f32s(1, [10.0, 20.0, 30.0, 40.0]);
        });
        // 0F 58 C1 -> addps xmm0, xmm1
        let mut xb = harness(&[0x0F, 0x58, 0xC1]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [11.0, 22.0, 33.0, 44.0]);
    }

    #[test]
    fn mulps_packed_lanes() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]);
            x.set_f32s(1, [2.0, 3.0, 4.0, 5.0]);
        });
        // 0F 59 C1 -> mulps xmm0, xmm1
        let mut xb = harness(&[0x0F, 0x59, 0xC1]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [2.0, 6.0, 12.0, 20.0]);
    }

    #[test]
    fn addss_scalar_only_touches_lane0() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]);
            x.set_f32s(1, [10.0, 99.0, 99.0, 99.0]);
        });
        // F3 0F 58 C1 -> addss xmm0, xmm1
        let mut xb = harness(&[0xF3, 0x0F, 0x58, 0xC1]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [11.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn xorps_self_zeroes_register() {
        reset_xmm();
        with_xmm(|x| x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]));
        // 0F 57 C0 -> xorps xmm0, xmm0
        let mut xb = harness(&[0x0F, 0x57, 0xC0]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.u64s(0)), [0, 0]);
    }

    #[test]
    fn pxor_self_zeroes_register() {
        reset_xmm();
        with_xmm(|x| x.set_u64s(0, [0xDEAD, 0xBEEF]));
        // 66 0F EF C0 -> pxor xmm0, xmm0
        let mut xb = harness(&[0x66, 0x0F, 0xEF, 0xC0]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.u64s(0)), [0, 0]);
    }

    #[test]
    fn cvtsi2ss_then_cvttss2si_roundtrip() {
        reset_xmm();
        // mov eax, 42 ; cvtsi2ss xmm0, eax ; cvttss2si ebx, xmm0
        let mut xb = harness(&[
            0xB8, 0x2A, 0x00, 0x00, 0x00, // mov eax,42
            0xF3, 0x0F, 0x2A, 0xC0, // cvtsi2ss xmm0, eax (rm=0=EAX)
            0xF3, 0x0F, 0x2C, 0xD8, // cvttss2si ebx, xmm0 (reg=3=EBX, rm=0)
        ]);
        run(&mut xb, 3);
        assert_eq!(with_xmm(|x| x.lane0_f32(0)), 42.0);
        assert_eq!(xb.cpu.reg32(EBX), 42);
    }

    #[test]
    fn cvttss2si_truncates() {
        reset_xmm();
        with_xmm(|x| x.set_lane0_f32(0, 3.9));
        // F3 0F 2C C0 -> cvttss2si eax, xmm0
        let mut xb = harness(&[0xF3, 0x0F, 0x2C, 0xC0]);
        run(&mut xb, 1);
        assert_eq!(xb.cpu.reg32(EAX), 3, "truncation toward zero");
    }

    #[test]
    fn shufps_selects_lanes() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [1.0, 2.0, 3.0, 4.0]); // dest
            x.set_f32s(1, [5.0, 6.0, 7.0, 8.0]); // src
        });
        // shufps xmm0, xmm1, imm8. imm = 0b11_10_01_00 = 0xE4 (identity-ish):
        // lane0=d[0]=1, lane1=d[1]=2, lane2=s[2]=7, lane3=s[3]=8
        let mut xb = harness(&[0x0F, 0xC6, 0xC1, 0xE4]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [1.0, 2.0, 7.0, 8.0]);
    }

    #[test]
    fn comiss_sets_eflags() {
        reset_xmm();
        // a < b -> CF=1, ZF=0
        with_xmm(|x| {
            x.set_lane0_f32(0, 1.0);
            x.set_lane0_f32(1, 2.0);
        });
        // 0F 2F C1 -> comiss xmm0, xmm1
        let mut xb = harness(&[0x0F, 0x2F, 0xC1]);
        run(&mut xb, 1);
        assert!(xb.cpu.flag(CF), "1.0 < 2.0 -> CF");
        assert!(!xb.cpu.flag(ZF));

        // a == b -> ZF=1, CF=0
        reset_xmm();
        with_xmm(|x| {
            x.set_lane0_f32(0, 5.0);
            x.set_lane0_f32(1, 5.0);
        });
        let mut xb = harness(&[0x0F, 0x2F, 0xC1]);
        run(&mut xb, 1);
        assert!(xb.cpu.flag(ZF), "equal -> ZF");
        assert!(!xb.cpu.flag(CF));

        // a > b -> ZF=0, CF=0
        reset_xmm();
        with_xmm(|x| {
            x.set_lane0_f32(0, 9.0);
            x.set_lane0_f32(1, 1.0);
        });
        let mut xb = harness(&[0x0F, 0x2F, 0xC1]);
        run(&mut xb, 1);
        assert!(!xb.cpu.flag(ZF));
        assert!(!xb.cpu.flag(CF), "greater -> CF clear");
    }

    #[test]
    fn movd_to_and_from_gpr() {
        reset_xmm();
        // mov eax,0xCAFEBABE ; movd xmm0,eax ; movd ebx,xmm0
        let mut xb = harness(&[
            0xB8, 0xBE, 0xBA, 0xFE, 0xCA, // mov eax,0xCAFEBABE
            0x66, 0x0F, 0x6E, 0xC0, // movd xmm0, eax
            0x66, 0x0F, 0x7E, 0xC3, // movd ebx, xmm0 (reg=0=xmm0, rm=3=EBX)
        ]);
        run(&mut xb, 3);
        assert_eq!(with_xmm(|x| x.u32s(0)), [0xCAFE_BABE, 0, 0, 0]);
        assert_eq!(xb.cpu.reg32(EBX), 0xCAFE_BABE);
    }

    #[test]
    fn divps_packed_lanes() {
        reset_xmm();
        with_xmm(|x| {
            x.set_f32s(0, [10.0, 20.0, 30.0, 40.0]);
            x.set_f32s(1, [2.0, 4.0, 5.0, 8.0]);
        });
        // 0F 5E C1 -> divps xmm0, xmm1
        let mut xb = harness(&[0x0F, 0x5E, 0xC1]);
        run(&mut xb, 1);
        assert_eq!(with_xmm(|x| x.f32s(0)), [5.0, 5.0, 6.0, 5.0]);
    }

    #[test]
    fn ldmxcsr_stmxcsr_roundtrip() {
        reset_xmm();
        // mov dword [0x1000], 0x1FA0 ; ldmxcsr [0x1000] ; stmxcsr [0x1004]
        let mut xb = harness(&[
            0xC7, 0x05, 0x00, 0x10, 0x00, 0x00, 0xA0, 0x1F, 0x00,
            0x00, // mov dword [0x1000],0x1FA0
            0x0F, 0xAE, 0x15, 0x00, 0x10, 0x00, 0x00, // ldmxcsr [0x1000] (/2)
            0x0F, 0xAE, 0x1D, 0x04, 0x10, 0x00, 0x00, // stmxcsr [0x1004] (/3)
        ]);
        run(&mut xb, 3);
        assert_eq!(with_xmm(|x| x.mxcsr), 0x1FA0);
        let stored = {
            let b = [
                xb.mem.ram_read8(0x1004) as u8,
                xb.mem.ram_read8(0x1005) as u8,
                xb.mem.ram_read8(0x1006) as u8,
                xb.mem.ram_read8(0x1007) as u8,
            ];
            u32::from_le_bytes(b)
        };
        assert_eq!(stored, 0x1FA0);
    }
}
