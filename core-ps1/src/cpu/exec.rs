//! R3000A instruction decode + execution.
//!
//! Built from scratch against nocash's psx-spx ("CPU Specifications" /
//! "CPU Opcode Encoding"). The executor is a plain interpreter: each
//! [`Cpu::step`] commits any pending load-delay write, fetches the instruction
//! at `pc`, advances the branch-delay PC pair, decodes, and executes.
//!
//! Two MIPS quirks dominate the shape of this code (both modelled in
//! [`super::state`]):
//!
//! * **Branch delay slot.** A taken branch/jump rewrites `next_pc`; the
//!   instruction already latched at `pc` (the delay slot) still runs. We set
//!   `branch_taken` so the *next* step records `in_delay_slot` for CAUSE.BD.
//! * **Load delay slot.** A load's result lands in the register file one
//!   instruction later, so the instruction immediately after a load still reads
//!   the destination's OLD value. We carry the in-flight load through the next
//!   instruction's execute (parking a freshly-issued load in `next_load`), then
//!   retire it afterwards — dropping it if that instruction wrote the same
//!   register first (the R3000A load-use hazard).
//!
//! Decoding uses closed enums + exhaustive matches over the primary opcode
//! (bits 31..26), the SPECIAL function field (bits 5..0), and the REGIMM rt
//! field (bits 20..16), per the project idioms. Everything is wrapping `u32`
//! arithmetic, little-endian.

use super::cop0::{self, Exception};
use super::state::Cpu;
use crate::bus::Bus;

/// A decoded 32-bit MIPS instruction word. Field accessors mirror the psx-spx
/// "CPU Opcode Encoding" tables; all are cheap bit extracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Instr(u32);

impl Instr {
    /// Primary opcode, bits 31..26.
    #[inline]
    fn op(self) -> u32 {
        self.0 >> 26
    }
    /// SPECIAL function field, bits 5..0.
    #[inline]
    fn funct(self) -> u32 {
        self.0 & 0x3F
    }
    /// rs source register, bits 25..21.
    #[inline]
    fn rs(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
    /// rt source/target register, bits 20..16.
    #[inline]
    fn rt(self) -> u32 {
        (self.0 >> 16) & 0x1F
    }
    /// rd destination register, bits 15..11.
    #[inline]
    fn rd(self) -> u32 {
        (self.0 >> 11) & 0x1F
    }
    /// shamt shift amount, bits 10..6.
    #[inline]
    fn shamt(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }
    /// Zero-extended 16-bit immediate.
    #[inline]
    fn imm(self) -> u32 {
        self.0 & 0xFFFF
    }
    /// Sign-extended 16-bit immediate.
    #[inline]
    fn imm_se(self) -> u32 {
        (self.0 & 0xFFFF) as i16 as i32 as u32
    }
    /// 26-bit jump target (word index).
    #[inline]
    fn target(self) -> u32 {
        self.0 & 0x03FF_FFFF
    }
    /// Coprocessor sub-opcode (the rs slot for COPn ops), bits 25..21.
    #[inline]
    fn cop_op(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
}

/// The PSX routes its single external interrupt line (the IRQ controller's
/// I_STAT & I_MASK) into CAUSE.IP bit 2 (hardware IRQ 0), i.e. bit 10 of CAUSE.
/// Software interrupts (IP0/IP1) are the two low IP bits, written via COP0 —
/// the CPU only drives this hardware bit from `irq_pending`.
const CAUSE_IP_HW: u32 = 1 << (cop0::CAUSE_IP_SHIFT + 2);

impl Cpu {
    /// Execute one instruction. The order is load-delay commit → IRQ check →
    /// fetch → advance PC pair → decode/execute.
    pub fn step(&mut self, bus: &mut impl Bus) {
        // The load delay slot is a one-instruction pipeline. A load issued by
        // instruction N must become visible to instruction N+2, i.e. the
        // instruction *immediately* after a load (the "load delay slot") still
        // reads the destination register's OLD value.
        //
        // We carry the in-flight load in `self.load` *through* the current
        // instruction's execute (so LWL/LWR can observe a load already targeting
        // the same register — the `LWL rt / LWR rt` idiom). A *new* load issued
        // by this instruction goes into `self.next_load` instead. After execute
        // we commit the carried load (unless the instruction clobbered its
        // target — the load-use hazard) and promote `next_load` to `load`.
        self.next_load = super::state::LoadSlot::default();

        // Latch the branch-delay state for the instruction we are about to run
        // (or defer to an interrupt) BEFORE sampling the IRQ line: an interrupt
        // taken here saves EPC at *this* instruction (so RFE re-runs it), and
        // CAUSE.BD must reflect whether this instruction sits in a delay slot —
        // both read current_pc / in_delay_slot via raise_exception. The previous
        // instruction's values are stale and would back EPC up by one.
        self.current_pc = self.pc;
        self.in_delay_slot = self.branch_taken;
        self.branch_taken = false;

        // Sample the interrupt line at the instruction boundary. If an INT is
        // pending and enabled, retire the in-flight load then enter the handler
        // instead of executing the fetched instruction.
        if self.check_irq() {
            self.retire_carried(0);
            self.raise_exception(Exception::Interrupt);
            return;
        }

        // Instruction fetch must be word-aligned; a misaligned PC is an
        // address error on the fetch.
        if self.pc & 3 != 0 {
            self.retire_carried(0);
            self.cop0.bad_vaddr = self.pc;
            self.raise_exception(Exception::AddressErrorLoad);
            return;
        }
        let word = bus.fetch32(self.pc);

        // Advance the PC pair: the next fetch comes from next_pc, and the one
        // after that is next_pc+4 — unless a branch rewrites next_pc.
        self.pc = self.next_pc;
        self.next_pc = self.next_pc.wrapping_add(4);

        // Clear the write-tracker; `set_reg` records the destination it writes
        // so we can detect whether the delay-slot instruction clobbered the
        // carried load's target (the R3000A load-use hazard: the instruction's
        // own write wins and the stale load is dropped).
        self.wrote_reg = 0;

        // Decode + execute. The instruction reads its operands from the current
        // register file (the carried load is NOT yet visible).
        self.execute(Instr(word), bus);

        // Retire the carried load now that this instruction has read its
        // operands, then promote any load this instruction issued.
        self.retire_carried(self.wrote_reg);
        self.load = self.next_load;
    }

    /// Commit the carried load slot (`self.load`) into the register file,
    /// unless `clobbered` names its destination register (the executing
    /// instruction wrote that register first, so its result wins). Clears the
    /// slot either way.
    #[inline]
    fn retire_carried(&mut self, clobbered: u32) {
        let slot = self.load;
        self.load = super::state::LoadSlot::default();
        if slot.reg != 0 && slot.reg != clobbered {
            self.regs[slot.reg as usize] = slot.value;
            self.regs[0] = 0;
        }
    }

    /// True if a hardware/software interrupt is pending *and* enabled: the
    /// current interrupt-enable bit (SR.IEc) is set and some CAUSE.IP bit is
    /// unmasked by the matching SR.Im bit. The IRQ controller folds its
    /// I_STAT&I_MASK into `irq_pending`, which we reflect into CAUSE.IP2.
    fn check_irq(&mut self) -> bool {
        // Reflect the external IRQ line into CAUSE.IP bit 2 (hardware IRQ),
        // matching the PSX's single interrupt line out of the IRQ controller.
        if self.irq_pending {
            self.cop0.cause |= CAUSE_IP_HW;
        } else {
            self.cop0.cause &= !CAUSE_IP_HW;
        }

        let sr = self.cop0.sr;
        if sr & cop0::SR_IEC == 0 {
            return false;
        }
        let pending = (self.cop0.cause & cop0::CAUSE_IP_MASK) & (sr & cop0::SR_IM);
        pending != 0
    }

    // ===================== top-level decode =====================
    fn execute(&mut self, i: Instr, bus: &mut impl Bus) {
        match i.op() {
            0x00 => self.exec_special(i),
            0x01 => self.exec_regimm(i),
            0x02 => self.op_j(i),
            0x03 => self.op_jal(i),
            0x04 => self.branch_if(i, self.reg(i.rs()) == self.reg(i.rt())), // BEQ
            0x05 => self.branch_if(i, self.reg(i.rs()) != self.reg(i.rt())), // BNE
            0x06 => self.branch_if(i, (self.reg(i.rs()) as i32) <= 0),       // BLEZ
            0x07 => self.branch_if(i, (self.reg(i.rs()) as i32) > 0),        // BGTZ
            0x08 => self.op_addi(i),
            0x09 => self.op_addiu(i),
            0x0A => self.op_slti(i),
            0x0B => self.op_sltiu(i),
            0x0C => self.op_andi(i),
            0x0D => self.op_ori(i),
            0x0E => self.op_xori(i),
            0x0F => self.op_lui(i),
            0x10 => self.exec_cop0(i),
            0x11 => self.cop_unusable(1), // COP1 (FPU) — absent on the PSX.
            0x12 => self.exec_cop2(i),    // COP2 / GTE seam.
            0x13 => self.cop_unusable(3), // COP3 — absent.
            0x20 => self.op_lb(i, bus),
            0x21 => self.op_lh(i, bus),
            0x22 => self.op_lwl(i, bus),
            0x23 => self.op_lw(i, bus),
            0x24 => self.op_lbu(i, bus),
            0x25 => self.op_lhu(i, bus),
            0x26 => self.op_lwr(i, bus),
            0x28 => self.op_sb(i, bus),
            0x29 => self.op_sh(i, bus),
            0x2A => self.op_swl(i, bus),
            0x2B => self.op_sw(i, bus),
            0x2E => self.op_swr(i, bus),
            // LWC2 / SWC2 are the GTE coprocessor load/store; the other COPn
            // load/stores are unusable on the PSX.
            0x32 => self.exec_lwc2(i, bus),
            0x3A => self.exec_swc2(i, bus),
            0x30 => self.cop_unusable(0),
            0x31 => self.cop_unusable(1),
            0x33 => self.cop_unusable(3),
            0x38 => self.cop_unusable(0),
            0x39 => self.cop_unusable(1),
            0x3B => self.cop_unusable(3),
            _ => self.raise_exception(Exception::ReservedInstruction),
        }
    }

    // ===================== SPECIAL (op = 0x00) =====================
    fn exec_special(&mut self, i: Instr) {
        match i.funct() {
            0x00 => self.op_sll(i),
            0x02 => self.op_srl(i),
            0x03 => self.op_sra(i),
            0x04 => self.op_sllv(i),
            0x06 => self.op_srlv(i),
            0x07 => self.op_srav(i),
            0x08 => self.op_jr(i),
            0x09 => self.op_jalr(i),
            0x0C => self.raise_exception(Exception::Syscall),
            0x0D => self.raise_exception(Exception::Breakpoint),
            0x10 => self.op_mfhi(i),
            0x11 => self.op_mthi(i),
            0x12 => self.op_mflo(i),
            0x13 => self.op_mtlo(i),
            0x18 => self.op_mult(i),
            0x19 => self.op_multu(i),
            0x1A => self.op_div(i),
            0x1B => self.op_divu(i),
            0x20 => self.op_add(i),
            0x21 => self.op_addu(i),
            0x22 => self.op_sub(i),
            0x23 => self.op_subu(i),
            0x24 => self.op_and(i),
            0x25 => self.op_or(i),
            0x26 => self.op_xor(i),
            0x27 => self.op_nor(i),
            0x2A => self.op_slt(i),
            0x2B => self.op_sltu(i),
            _ => self.raise_exception(Exception::ReservedInstruction),
        }
    }

    // ===================== REGIMM (op = 0x01) =====================
    // The rt field selects BLTZ/BGEZ/BLTZAL/BGEZAL. Bit 0 of rt is the
    // condition (0 = <0, 1 = >=0); bits 4..1 == 0b1000 request the link.
    fn exec_regimm(&mut self, i: Instr) {
        let rt = i.rt();
        let rs = self.reg(i.rs()) as i32;
        let condition = if rt & 1 != 0 { rs >= 0 } else { rs < 0 };
        let link = (rt & 0x1E) == 0x10;
        // The link writes r31 = return address (instruction after the delay
        // slot) and happens whether or not the branch is taken.
        if link {
            self.set_reg(31, self.next_pc);
        }
        self.branch_if(i, condition);
    }

    // ===================== branches / jumps =====================
    /// Take a relative branch (signed 16-bit word offset from the delay slot)
    /// when `cond`. The target is computed from `pc` (which now holds the
    /// delay-slot address) to honour the branch-delay latch.
    fn branch_if(&mut self, i: Instr, cond: bool) {
        if cond {
            let offset = i.imm_se() << 2;
            self.next_pc = self.pc.wrapping_add(offset);
            self.branch_taken = true;
        }
    }

    fn op_j(&mut self, i: Instr) {
        // 26-bit target shifted left 2, in the 256 MB region of the delay slot.
        self.next_pc = (self.pc & 0xF000_0000) | (i.target() << 2);
        self.branch_taken = true;
    }

    fn op_jal(&mut self, i: Instr) {
        self.set_reg(31, self.next_pc);
        self.op_j(i);
    }

    fn op_jr(&mut self, i: Instr) {
        self.next_pc = self.reg(i.rs());
        self.branch_taken = true;
    }

    fn op_jalr(&mut self, i: Instr) {
        let target = self.reg(i.rs());
        self.set_reg(i.rd(), self.next_pc);
        self.next_pc = target;
        self.branch_taken = true;
    }

    // ===================== ALU immediate =====================
    fn op_addi(&mut self, i: Instr) {
        let a = self.reg(i.rs()) as i32;
        let b = i.imm_se() as i32;
        match a.checked_add(b) {
            Some(r) => self.set_reg(i.rt(), r as u32),
            None => self.raise_exception(Exception::Overflow),
        }
    }

    fn op_addiu(&mut self, i: Instr) {
        let r = self.reg(i.rs()).wrapping_add(i.imm_se());
        self.set_reg(i.rt(), r);
    }

    fn op_slti(&mut self, i: Instr) {
        let r = ((self.reg(i.rs()) as i32) < (i.imm_se() as i32)) as u32;
        self.set_reg(i.rt(), r);
    }

    fn op_sltiu(&mut self, i: Instr) {
        let r = (self.reg(i.rs()) < i.imm_se()) as u32;
        self.set_reg(i.rt(), r);
    }

    fn op_andi(&mut self, i: Instr) {
        let r = self.reg(i.rs()) & i.imm();
        self.set_reg(i.rt(), r);
    }

    fn op_ori(&mut self, i: Instr) {
        let r = self.reg(i.rs()) | i.imm();
        self.set_reg(i.rt(), r);
    }

    fn op_xori(&mut self, i: Instr) {
        let r = self.reg(i.rs()) ^ i.imm();
        self.set_reg(i.rt(), r);
    }

    fn op_lui(&mut self, i: Instr) {
        self.set_reg(i.rt(), i.imm() << 16);
    }

    // ===================== ALU register =====================
    fn op_add(&mut self, i: Instr) {
        let a = self.reg(i.rs()) as i32;
        let b = self.reg(i.rt()) as i32;
        match a.checked_add(b) {
            Some(r) => self.set_reg(i.rd(), r as u32),
            None => self.raise_exception(Exception::Overflow),
        }
    }

    fn op_addu(&mut self, i: Instr) {
        let r = self.reg(i.rs()).wrapping_add(self.reg(i.rt()));
        self.set_reg(i.rd(), r);
    }

    fn op_sub(&mut self, i: Instr) {
        let a = self.reg(i.rs()) as i32;
        let b = self.reg(i.rt()) as i32;
        match a.checked_sub(b) {
            Some(r) => self.set_reg(i.rd(), r as u32),
            None => self.raise_exception(Exception::Overflow),
        }
    }

    fn op_subu(&mut self, i: Instr) {
        let r = self.reg(i.rs()).wrapping_sub(self.reg(i.rt()));
        self.set_reg(i.rd(), r);
    }

    fn op_and(&mut self, i: Instr) {
        let r = self.reg(i.rs()) & self.reg(i.rt());
        self.set_reg(i.rd(), r);
    }

    fn op_or(&mut self, i: Instr) {
        let r = self.reg(i.rs()) | self.reg(i.rt());
        self.set_reg(i.rd(), r);
    }

    fn op_xor(&mut self, i: Instr) {
        let r = self.reg(i.rs()) ^ self.reg(i.rt());
        self.set_reg(i.rd(), r);
    }

    fn op_nor(&mut self, i: Instr) {
        let r = !(self.reg(i.rs()) | self.reg(i.rt()));
        self.set_reg(i.rd(), r);
    }

    fn op_slt(&mut self, i: Instr) {
        let r = ((self.reg(i.rs()) as i32) < (self.reg(i.rt()) as i32)) as u32;
        self.set_reg(i.rd(), r);
    }

    fn op_sltu(&mut self, i: Instr) {
        let r = (self.reg(i.rs()) < self.reg(i.rt())) as u32;
        self.set_reg(i.rd(), r);
    }

    // ===================== shifts =====================
    fn op_sll(&mut self, i: Instr) {
        let r = self.reg(i.rt()) << i.shamt();
        self.set_reg(i.rd(), r);
    }

    fn op_srl(&mut self, i: Instr) {
        let r = self.reg(i.rt()) >> i.shamt();
        self.set_reg(i.rd(), r);
    }

    fn op_sra(&mut self, i: Instr) {
        let r = ((self.reg(i.rt()) as i32) >> i.shamt()) as u32;
        self.set_reg(i.rd(), r);
    }

    fn op_sllv(&mut self, i: Instr) {
        // Only the low 5 bits of rs are the shift count.
        let r = self.reg(i.rt()) << (self.reg(i.rs()) & 0x1F);
        self.set_reg(i.rd(), r);
    }

    fn op_srlv(&mut self, i: Instr) {
        let r = self.reg(i.rt()) >> (self.reg(i.rs()) & 0x1F);
        self.set_reg(i.rd(), r);
    }

    fn op_srav(&mut self, i: Instr) {
        let r = ((self.reg(i.rt()) as i32) >> (self.reg(i.rs()) & 0x1F)) as u32;
        self.set_reg(i.rd(), r);
    }

    // ===================== HI/LO move =====================
    fn op_mfhi(&mut self, i: Instr) {
        self.set_reg(i.rd(), self.hi);
    }
    fn op_mflo(&mut self, i: Instr) {
        self.set_reg(i.rd(), self.lo);
    }
    fn op_mthi(&mut self, i: Instr) {
        self.hi = self.reg(i.rs());
    }
    fn op_mtlo(&mut self, i: Instr) {
        self.lo = self.reg(i.rs());
    }

    // ===================== mult / div =====================
    fn op_mult(&mut self, i: Instr) {
        let a = (self.reg(i.rs()) as i32) as i64;
        let b = (self.reg(i.rt()) as i32) as i64;
        let p = (a * b) as u64;
        self.lo = p as u32;
        self.hi = (p >> 32) as u32;
    }

    fn op_multu(&mut self, i: Instr) {
        let a = self.reg(i.rs()) as u64;
        let b = self.reg(i.rt()) as u64;
        let p = a * b;
        self.lo = p as u32;
        self.hi = (p >> 32) as u32;
    }

    fn op_div(&mut self, i: Instr) {
        let n = self.reg(i.rs()) as i32;
        let d = self.reg(i.rt()) as i32;
        // psx-spx divide edge cases — the R3000A produces these fixed garbage
        // results rather than trapping.
        if d == 0 {
            // lo = -1 (or +1 when dividend is negative); hi = dividend.
            self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
            self.hi = n as u32;
        } else if n as u32 == 0x8000_0000 && d == -1 {
            // The only signed-overflow case: quotient would be 0x80000000.
            self.lo = 0x8000_0000;
            self.hi = 0;
        } else {
            self.lo = (n / d) as u32;
            self.hi = (n % d) as u32;
        }
    }

    fn op_divu(&mut self, i: Instr) {
        let n = self.reg(i.rs());
        let d = self.reg(i.rt());
        if d == 0 {
            self.lo = 0xFFFF_FFFF;
            self.hi = n;
        } else {
            self.lo = n / d;
            self.hi = n % d;
        }
    }

    // ===================== loads =====================
    // Loads queue a load-delay write rather than updating the register file
    // directly; the destination becomes visible on the *next* step.
    fn op_lb(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let v = bus.read8(addr) as u8 as i8 as i32 as u32;
        self.issue_load(i.rt(), v);
    }

    fn op_lbu(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let v = bus.read8(addr) & 0xFF;
        self.issue_load(i.rt(), v);
    }

    fn op_lh(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 1 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorLoad);
            return;
        }
        let v = bus.read16(addr) as u16 as i16 as i32 as u32;
        self.issue_load(i.rt(), v);
    }

    fn op_lhu(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 1 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorLoad);
            return;
        }
        let v = bus.read16(addr) & 0xFFFF;
        self.issue_load(i.rt(), v);
    }

    fn op_lw(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 3 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorLoad);
            return;
        }
        let v = bus.read32(addr);
        self.issue_load(i.rt(), v);
    }

    /// LWL — load the upper bytes of the word straddled by `addr` into the
    /// upper bytes of rt, leaving the rest of rt intact. Unaligned: no address
    /// error. Merges with the *pending* load value if one targets rt (the
    /// R3000A LWL/LWR pair share the in-flight register without a delay
    /// between them).
    fn op_lwl(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let aligned = bus.read32(addr & !3);
        let cur = self.pending_or_reg(i.rt());
        // Number of bytes from the addressed byte up to the word boundary,
        // shifted into the high end of the register.
        let shift = (addr & 3) * 8;
        let mask = 0x00FF_FFFFu32 >> shift; // bytes of rt to keep
        let v = (cur & mask) | (aligned << (24 - shift));
        self.issue_load(i.rt(), v);
    }

    /// LWR — load the lower bytes of the straddled word into the lower bytes
    /// of rt.
    fn op_lwr(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let aligned = bus.read32(addr & !3);
        let cur = self.pending_or_reg(i.rt());
        let shift = (addr & 3) * 8;
        // Bytes of rt to keep are the high (32-shift) bits; for shift==0 we
        // replace the whole register, so the keep-mask is empty.
        let mask = if shift == 0 {
            0
        } else {
            0xFFFF_FFFFu32 << (32 - shift)
        };
        let v = (cur & mask) | (aligned >> shift);
        self.issue_load(i.rt(), v);
    }

    /// The value LWL/LWR merge into: the carried load already in flight to the
    /// same register (the common `LWL rt / LWR rt` idiom) is observed, otherwise
    /// the register file's current value.
    #[inline]
    fn pending_or_reg(&self, rt: u32) -> u32 {
        if self.load.reg == rt && rt != 0 {
            self.load.value
        } else {
            self.reg(rt)
        }
    }

    /// Issue a load whose result is visible one instruction later. The load is
    /// parked in `next_load` (not the carried `load`) so it does not become
    /// visible to the very next instruction (the load delay slot); the step
    /// loop promotes it after the carried load retires.
    #[inline]
    fn issue_load(&mut self, reg: u32, value: u32) {
        self.next_load = super::state::LoadSlot {
            reg: reg & 0x1F,
            value,
        };
    }

    // ===================== stores =====================
    fn op_sb(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        bus.write8(addr, self.reg(i.rt()) & 0xFF);
    }

    fn op_sh(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 1 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorStore);
            return;
        }
        bus.write16(addr, self.reg(i.rt()) & 0xFFFF);
    }

    fn op_sw(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 3 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorStore);
            return;
        }
        bus.write32(addr, self.reg(i.rt()));
    }

    fn op_swl(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let aligned_addr = addr & !3;
        let cur = bus.read32(aligned_addr);
        let val = self.reg(i.rt());
        let shift = (addr & 3) * 8;
        // Store the upper bytes of rt into the low end of the word; keep the
        // memory bytes above them.
        let mask = if shift == 24 {
            0
        } else {
            0xFFFF_FF00u32 << shift
        };
        let v = (cur & mask) | (val >> (24 - shift));
        bus.write32(aligned_addr, v);
    }

    fn op_swr(&mut self, i: Instr, bus: &mut impl Bus) {
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        let aligned_addr = addr & !3;
        let cur = bus.read32(aligned_addr);
        let val = self.reg(i.rt());
        let shift = (addr & 3) * 8;
        // Store the lower bytes of rt into the high end of the word; keep the
        // memory bytes below them.
        let mask = if shift == 0 {
            0
        } else {
            0xFFFF_FFFFu32 >> (32 - shift)
        };
        let v = (cur & mask) | (val << shift);
        bus.write32(aligned_addr, v);
    }

    // ===================== COP0 =====================
    fn exec_cop0(&mut self, i: Instr) {
        match i.cop_op() {
            0x00 => self.op_mfc0(i), // MFC0 rt, rd
            0x04 => self.op_mtc0(i), // MTC0 rt, rd
            // COP0 "CO" instructions (bit 25 set); only RFE (funct 0x10) is used.
            op if op & 0x10 != 0 => {
                if i.funct() == 0x10 {
                    self.cop0.return_from_exception();
                } else {
                    self.raise_exception(Exception::ReservedInstruction);
                }
            }
            _ => self.raise_exception(Exception::ReservedInstruction),
        }
    }

    fn op_mfc0(&mut self, i: Instr) {
        // MFC0 has the same one-instruction load delay as a memory load.
        let v = self.cop0.read(i.rd() as usize);
        self.issue_load(i.rt(), v);
    }

    fn op_mtc0(&mut self, i: Instr) {
        self.cop0.write(i.rd() as usize, self.reg(i.rt()));
    }

    // ===================== COP2 / GTE seam =====================
    // The GTE (geometry transformation engine) is COP2. Real register/command
    // handling lands in the `gte` subsystem; here we decode the COP2 ops so the
    // CPU stream stays in sync and the seam is explicit. MFC2/CFC2 still honour
    // the load delay; data ops are no-ops until the GTE struct exists.
    fn exec_cop2(&mut self, i: Instr) {
        // COP2 is only usable if SR.CU2 is set, otherwise a coprocessor-unusable
        // exception fires (the BIOS sets CU2 before issuing GTE ops).
        if self.cop0.sr & cop0::SR_CU2 == 0 {
            self.cop_unusable(2);
            return;
        }
        match i.cop_op() {
            // MFC2 / CFC2: read GTE data/control register into rt (load-delayed).
            0x00 | 0x02 => self.issue_load(i.rt(), 0), // GTE read stub
            // MTC2 / CTC2: write rt into a GTE register — no-op stub.
            0x04 | 0x06 => {}
            // COP2 command (bit 25 set): a GTE operation — no-op stub.
            op if op & 0x10 != 0 => {}
            _ => {}
        }
    }

    fn exec_lwc2(&mut self, i: Instr, bus: &mut impl Bus) {
        // LWC2: load a word into a GTE register. Stubbed: perform the bus read
        // (for side-effect fidelity) but drop the value until the GTE lands.
        if self.cop0.sr & cop0::SR_CU2 == 0 {
            self.cop_unusable(2);
            return;
        }
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 3 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorLoad);
            return;
        }
        let _ = bus.read32(addr);
    }

    fn exec_swc2(&mut self, i: Instr, bus: &mut impl Bus) {
        // SWC2: store a GTE register to memory. Stubbed: write zero until the
        // GTE provides the source register.
        if self.cop0.sr & cop0::SR_CU2 == 0 {
            self.cop_unusable(2);
            return;
        }
        let addr = self.reg(i.rs()).wrapping_add(i.imm_se());
        if addr & 3 != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise_exception(Exception::AddressErrorStore);
            return;
        }
        bus.write32(addr, 0);
    }

    /// Raise a coprocessor-unusable exception, recording the offending COPn in
    /// CAUSE.CE.
    fn cop_unusable(&mut self, cop: u32) {
        self.cop0.cause = (self.cop0.cause & !(0x3 << cop0::CAUSE_CE_SHIFT))
            | ((cop & 0x3) << cop0::CAUSE_CE_SHIFT);
        self.raise_exception(Exception::CoprocessorUnusable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::cop0::{SR_IEC, SR_IM};
    use crate::psx::Psx;

    // ---- instruction assembly helpers ----
    fn r_type(funct: u32, rs: u32, rt: u32, rd: u32, shamt: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | (shamt << 6) | funct
    }
    fn i_type(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
        (op << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF)
    }
    fn j_type(op: u32, target: u32) -> u32 {
        (op << 26) | (target & 0x03FF_FFFF)
    }

    /// A test harness: a Psx with a small program written into RAM and the CPU
    /// pointed at it via KSEG0 (0x8000_0000) so SR.IsC (which would drop RAM
    /// writes) is irrelevant.
    fn harness(program: &[u32]) -> Psx {
        let mut psx = Psx::new();
        let base = 0x8000_0000u32;
        for (idx, &w) in program.iter().enumerate() {
            psx.write32(base + (idx as u32) * 4, w);
        }
        psx.cpu.pc = base;
        psx.cpu.next_pc = base + 4;
        psx.cpu.current_pc = base;
        psx
    }

    /// Step the CPU `n` times. The bus IS the Psx that also owns the cpu, so we
    /// split the borrow by swapping the cpu out for the duration of the step.
    fn run(psx: &mut Psx, n: usize) {
        for _ in 0..n {
            let mut cpu = std::mem::replace(&mut psx.cpu, Cpu::new());
            cpu.step(psx);
            psx.cpu = cpu;
        }
    }

    fn get(psx: &Psx, r: u32) -> u32 {
        psx.cpu.reg(r)
    }

    #[test]
    fn addiu_and_addi_overflow() {
        let mut psx = harness(&[i_type(0x09, 0, 1, 5), i_type(0x09, 1, 2, 0xFFFB)]);
        run(&mut psx, 2);
        assert_eq!(get(&psx, 1), 5);
        assert_eq!(get(&psx, 2), 0); // 5 + (-5)
    }

    #[test]
    fn addi_traps_on_overflow() {
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x7FFF), // LUI r1, 0x7FFF
            i_type(0x0D, 1, 1, 0xFFFF), // ORI r1 -> 0x7FFFFFFF
            i_type(0x08, 1, 2, 1),      // ADDI r2, r1, 1 (overflow)
        ]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV; // RAM vector
        run(&mut psx, 3);
        assert_eq!(get(&psx, 1), 0x7FFF_FFFF);
        assert_eq!(get(&psx, 2), 0); // unchanged
        assert_eq!(psx.cpu.pc, cop0::VECTOR_RAM);
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::Overflow.code()
        );
    }

    #[test]
    fn logical_and_lui() {
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0xABCD), // LUI r1 -> 0xABCD0000
            i_type(0x0D, 1, 1, 0x1234), // ORI -> 0xABCD1234
            i_type(0x0C, 1, 2, 0x00FF), // ANDI -> 0x34
            i_type(0x0E, 1, 3, 0xFFFF), // XORI -> 0xABCDEDCB
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 1), 0xABCD_1234);
        assert_eq!(get(&psx, 2), 0x34);
        assert_eq!(get(&psx, 3), 0xABCD_EDCB);
    }

    #[test]
    fn slt_and_sltu() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0xFFFF), // r1 = -1
            i_type(0x09, 0, 2, 1),      // r2 = 1
            r_type(0x2A, 1, 2, 3, 0),   // SLT  signed   -> 1
            r_type(0x2B, 1, 2, 4, 0),   // SLTU unsigned -> 0
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 3), 1);
        assert_eq!(get(&psx, 4), 0);
    }

    #[test]
    fn shifts() {
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x8000), // r1 = 0x80000000
            r_type(0x00, 0, 1, 2, 4),   // SLL << 4 = 0
            r_type(0x02, 0, 1, 3, 4),   // SRL >> 4 = 0x08000000
            r_type(0x03, 0, 1, 4, 4),   // SRA >>a 4 = 0xF8000000
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 2), 0);
        assert_eq!(get(&psx, 3), 0x0800_0000);
        assert_eq!(get(&psx, 4), 0xF800_0000);
    }

    #[test]
    fn variable_shifts() {
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x1000), // r1 = 0x10000000
            i_type(0x09, 0, 5, 4),      // r5 = 4
            r_type(0x04, 5, 1, 2, 0),   // SLLV << 4 = 0
            r_type(0x06, 5, 1, 3, 0),   // SRLV >> 4
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 2), 0);
        assert_eq!(get(&psx, 3), 0x0100_0000);
    }

    #[test]
    fn mult_and_mflo_mfhi() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0xFFFF), // r1 = -1
            i_type(0x09, 0, 2, 7),      // r2 = 7
            r_type(0x18, 1, 2, 0, 0),   // MULT -> hi:lo = -7
            r_type(0x10, 0, 0, 3, 0),   // MFHI r3
            r_type(0x12, 0, 0, 4, 0),   // MFLO r4
        ]);
        run(&mut psx, 5);
        assert_eq!(get(&psx, 4), (-7i32) as u32);
        assert_eq!(get(&psx, 3), 0xFFFF_FFFF);
    }

    #[test]
    fn div_normal_and_by_zero() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 17),
            i_type(0x09, 0, 2, 5),
            r_type(0x1A, 1, 2, 0, 0), // DIV 17/5
            r_type(0x12, 0, 0, 3, 0), // MFLO -> 3
            r_type(0x10, 0, 0, 4, 0), // MFHI -> 2
        ]);
        run(&mut psx, 5);
        assert_eq!(get(&psx, 3), 3);
        assert_eq!(get(&psx, 4), 2);

        // signed divide by zero, positive dividend: lo = -1, hi = dividend.
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 100),
            r_type(0x1A, 1, 0, 0, 0), // DIV r1, r0
            r_type(0x12, 0, 0, 3, 0),
            r_type(0x10, 0, 0, 4, 0),
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 3), 0xFFFF_FFFF);
        assert_eq!(get(&psx, 4), 100);
    }

    #[test]
    fn divu_by_zero() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 50),
            r_type(0x1B, 1, 0, 0, 0), // DIVU r1, r0
            r_type(0x12, 0, 0, 3, 0),
            r_type(0x10, 0, 0, 4, 0),
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 3), 0xFFFF_FFFF);
        assert_eq!(get(&psx, 4), 50);
    }

    #[test]
    fn load_delay_visibility() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x23, 0, 1, 0x100), // LW r1, 0x100(r0)
            i_type(0x09, 1, 2, 0),     // ADDIU r2, r1, 0 (sees OLD r1)
            i_type(0x09, 1, 3, 0),     // ADDIU r3, r1, 0 (sees NEW r1)
        ]);
        psx.write32(base + 0x100, 0x1234);
        run(&mut psx, 3);
        assert_eq!(get(&psx, 2), 0, "load delay: slot sees old value");
        assert_eq!(get(&psx, 1), 0x1234);
        assert_eq!(get(&psx, 3), 0x1234, "value visible one instr later");
    }

    #[test]
    fn load_use_shadow_same_register() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x23, 0, 1, 0x100),  // LW r1
            i_type(0x0D, 0, 1, 0xBEEF), // ORI r1, r0, 0xBEEF (shadows the load)
            i_type(0x09, 1, 2, 0),      // ADDIU r2, r1, 0
        ]);
        psx.write32(base + 0x100, 0x1234);
        run(&mut psx, 3);
        assert_eq!(get(&psx, 1), 0xBEEF, "ORI wins over shadowed load");
        assert_eq!(get(&psx, 2), 0xBEEF);
    }

    #[test]
    fn byte_and_half_loads_sign_extend() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x20, 0, 1, 0x100), // LB
            i_type(0x24, 0, 2, 0x100), // LBU
            i_type(0x21, 0, 3, 0x102), // LH
            i_type(0x25, 0, 4, 0x102), // LHU
            i_type(0x00, 0, 0, 0),     // NOP to settle the last load
        ]);
        psx.write32(base + 0x100, 0xF0F0_80FF); // [0x100]=0xFF, [0x102]=0xF0F0
        run(&mut psx, 5);
        assert_eq!(get(&psx, 1), 0xFFFF_FFFF);
        assert_eq!(get(&psx, 2), 0x0000_00FF);
        assert_eq!(get(&psx, 3), 0xFFFF_F0F0);
        assert_eq!(get(&psx, 4), 0x0000_F0F0);
    }

    #[test]
    fn stores_byte_half_word() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0xAABB), // LUI
            i_type(0x0D, 1, 1, 0xCCDD), // r1 = 0xAABBCCDD
            i_type(0x2B, 0, 1, 0x200),  // SW
            i_type(0x28, 0, 1, 0x204),  // SB -> 0xDD
            i_type(0x29, 0, 1, 0x206),  // SH -> 0xCCDD
        ]);
        run(&mut psx, 5);
        assert_eq!(psx.read32(base + 0x200), 0xAABB_CCDD);
        assert_eq!(psx.read8(base + 0x204), 0xDD);
        assert_eq!(psx.read16(base + 0x206), 0xCCDD);
    }

    #[test]
    fn unaligned_load_word() {
        // Load the 32-bit word that begins at byte 0x303 using the canonical
        // LWR addr / LWL addr+3 idiom. The two halves merge via the carried
        // load (LWL observes LWR's in-flight value targeting the same register).
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x26, 0, 1, 0x303), // LWR r1, 0x303
            i_type(0x22, 0, 1, 0x306), // LWL r1, 0x303+3
            i_type(0x00, 0, 0, 0),     // NOP (settle the load)
        ]);
        psx.write32(base + 0x300, 0x1122_3344);
        psx.write32(base + 0x304, 0x5566_7788);
        run(&mut psx, 3);
        // little-endian bytes [0x303,0x304,0x305,0x306] = 0x11,0x88,0x77,0x66
        assert_eq!(get(&psx, 1), 0x6677_8811);
    }

    #[test]
    fn unaligned_store_word_roundtrip() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0xDEAD), // LUI
            i_type(0x0D, 1, 1, 0xBEEF), // r1 = 0xDEADBEEF
            i_type(0x2E, 0, 1, 0x403),  // SWR r1, 0x403  (store word at byte 0x403)
            i_type(0x2A, 0, 1, 0x406),  // SWL r1, 0x403+3
            // reload via the matching LWR/LWL pair into r2
            i_type(0x26, 0, 2, 0x403),  // LWR r2, 0x403
            i_type(0x22, 0, 2, 0x406),  // LWL r2, 0x403+3
            i_type(0x00, 0, 0, 0),      // NOP to settle r2
        ]);
        run(&mut psx, 7);
        assert_eq!(get(&psx, 2), 0xDEAD_BEEF);
    }

    #[test]
    fn branch_delay_slot_executes() {
        let mut psx = harness(&[
            i_type(0x04, 0, 0, 2),    // BEQ r0,r0,+2 (from delay slot)
            i_type(0x09, 0, 1, 0xAA), // delay slot RUNS
            i_type(0x09, 0, 2, 0xBB), // SKIPPED
            i_type(0x09, 0, 3, 0xCC), // target
        ]);
        run(&mut psx, 3);
        assert_eq!(get(&psx, 1), 0xAA, "delay slot runs");
        assert_eq!(get(&psx, 2), 0, "branch skipped this");
        assert_eq!(get(&psx, 3), 0xCC, "landed at target");
    }

    #[test]
    fn jal_links_return_address() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            j_type(0x03, (base + 0x40) >> 2), // JAL base+0x40
            i_type(0x09, 0, 1, 0x11),         // delay slot runs
        ]);
        run(&mut psx, 2);
        assert_eq!(get(&psx, 1), 0x11);
        assert_eq!(get(&psx, 31), base + 8, "r31 = instr after delay slot");
        assert_eq!(psx.cpu.pc, base + 0x40);
    }

    #[test]
    fn jr_jumps() {
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, (base + 0x80) >> 16),    // LUI r1, hi
            i_type(0x0D, 1, 1, (base + 0x80) & 0xFFFF), // ORI lo
            r_type(0x08, 1, 0, 0, 0),                   // JR r1
            i_type(0x00, 0, 0, 0),                      // delay slot NOP
        ]);
        run(&mut psx, 4);
        assert_eq!(psx.cpu.pc, base + 0x80);
    }

    #[test]
    fn regimm_bltzal_links_and_branches() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0xFFFF), // r1 = -1
            i_type(0x01, 1, 0x10, 2),   // BLTZAL r1, +2 (rt=0x10)
            i_type(0x09, 0, 2, 0xDD),   // delay slot runs
            i_type(0x09, 0, 4, 0xEE),   // skipped
            i_type(0x09, 0, 5, 0xFF),   // target
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 2), 0xDD);
        assert_eq!(get(&psx, 4), 0, "branch taken over this");
        assert_eq!(get(&psx, 5), 0xFF);
        assert_ne!(get(&psx, 31), 0, "BLTZAL linked");
    }

    #[test]
    fn bgez_not_taken_when_negative() {
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0xFFFF), // r1 = -1
            i_type(0x01, 1, 0x01, 2),   // BGEZ r1, +2 (rt=1) -> NOT taken
            i_type(0x09, 0, 2, 0xDD),   // delay slot runs
            i_type(0x09, 0, 3, 0xEE),   // falls through here
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 2), 0xDD);
        assert_eq!(get(&psx, 3), 0xEE, "fell through, not branched");
    }

    #[test]
    fn syscall_enters_exception() {
        let mut psx = harness(&[r_type(0x0C, 0, 0, 0, 0)]); // SYSCALL
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 1);
        assert_eq!(psx.cpu.pc, cop0::VECTOR_RAM);
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::Syscall.code()
        );
    }

    #[test]
    fn syscall_in_delay_slot_sets_bd() {
        // J target ; SYSCALL in the delay slot -> CAUSE.BD set, EPC at the J.
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            j_type(0x02, (base + 0x40) >> 2), // J
            r_type(0x0C, 0, 0, 0, 0),         // SYSCALL (delay slot)
        ]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 2);
        assert_ne!(psx.cpu.cop0.cause & cop0::CAUSE_BD, 0, "BD set");
        assert_eq!(psx.cpu.cop0.epc, base, "EPC points at the branch");
    }

    #[test]
    fn mtc0_mfc0_roundtrip() {
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x0000),                          // LUI r1, 0
            i_type(0x0D, 1, 1, 0x1234),                          // r1 = 0x1234
            (0x10 << 26) | (0x04 << 21) | (1 << 16) | (3 << 11), // MTC0 r1 -> cop0 r3
            (0x10 << 26) | (0x00 << 21) | (2 << 16) | (3 << 11), // MFC0 r2 <- cop0 r3
            i_type(0x00, 0, 0, 0),                               // NOP (settle MFC0 delay)
        ]);
        run(&mut psx, 5);
        assert_eq!(get(&psx, 2), 0x1234);
    }

    #[test]
    fn rfe_pops_mode_stack() {
        let mut psx = harness(&[(0x10 << 26) | (0x10 << 21) | 0x10]); // RFE
        psx.cpu.cop0.sr = cop0::SR_IEP; // IEc=0, IEp=1
        run(&mut psx, 1);
        assert_ne!(psx.cpu.cop0.sr & cop0::SR_IEC, 0, "IEc restored from IEp");
    }

    #[test]
    fn interrupt_taken_at_boundary() {
        let mut psx = harness(&[i_type(0x09, 0, 1, 0x55)]); // would set r1
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        psx.cpu.cop0.sr |= SR_IEC | (CAUSE_IP_HW & SR_IM); // enable IP2
        psx.cpu.irq_pending = true;
        run(&mut psx, 1);
        assert_eq!(get(&psx, 1), 0, "interrupted before executing");
        assert_eq!(psx.cpu.pc, cop0::VECTOR_RAM);
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::Interrupt.code()
        );
    }

    #[test]
    fn interrupt_masked_when_iec_clear() {
        let mut psx = harness(&[i_type(0x09, 0, 1, 0x55)]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        psx.cpu.cop0.sr &= !SR_IEC; // interrupts globally disabled
        psx.cpu.cop0.sr |= CAUSE_IP_HW & SR_IM;
        psx.cpu.irq_pending = true;
        run(&mut psx, 1);
        assert_eq!(get(&psx, 1), 0x55, "instruction ran; IRQ ignored");
    }

    #[test]
    fn reserved_instruction_traps() {
        // op 0x1F is not a valid primary opcode.
        let mut psx = harness(&[0x1F << 26]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 1);
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::ReservedInstruction.code()
        );
    }

    #[test]
    fn back_to_back_loads_same_register() {
        // Two consecutive LWs to the SAME register: per psx-spx the first
        // load's value never becomes visible to user code; the second wins.
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x23, 0, 1, 0x100), // LW r1, [0x100]
            i_type(0x23, 0, 1, 0x104), // LW r1, [0x104]
            i_type(0x09, 1, 2, 0),     // ADDIU r2, r1, 0
            i_type(0x00, 0, 0, 0),     // NOP
        ]);
        psx.write32(base + 0x100, 0xAAAA_AAAA);
        psx.write32(base + 0x104, 0xBBBB_BBBB);
        run(&mut psx, 4);
        // r1 ends up with the second load; the first value never reaches an
        // observing instruction.
        assert_eq!(get(&psx, 1), 0xBBBB_BBBB, "second load wins");
        // The ADDIU executes in the delay slot of the *second* load, so it sees
        // the value present before that load retires — which is the FIRST load's
        // value (0xAAAA_AAAA), the documented one-deep pipeline.
        assert_eq!(get(&psx, 2), 0xAAAA_AAAA);
    }

    #[test]
    fn load_in_branch_delay_slot_commits() {
        // A load sitting in a branch delay slot still has a normal load delay:
        // its value is visible at the branch target's first instruction's
        // successor, not at the target itself.
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x04, 0, 0, 2),     // 0x00 BEQ r0,r0,+2 -> target 0x0C
            i_type(0x23, 0, 1, 0x200), // 0x04 LW r1 (delay slot, runs)
            i_type(0x09, 0, 9, 0xEE),  // 0x08 skipped
            i_type(0x09, 1, 2, 0),     // 0x0C target: ADDIU r2,r1 (sees OLD r1)
            i_type(0x09, 1, 3, 0),     // 0x10 ADDIU r3,r1 (sees NEW r1)
        ]);
        psx.write32(base + 0x200, 0x1234_5678);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 9), 0, "branch skipped this");
        assert_eq!(get(&psx, 2), 0, "load-delay carries across the branch");
        assert_eq!(get(&psx, 1), 0x1234_5678);
        assert_eq!(get(&psx, 3), 0x1234_5678, "visible one instr after the slot");
    }

    #[test]
    fn interrupt_epc_points_at_pending_instruction() {
        // When an IRQ is taken at an instruction boundary, EPC must point at the
        // instruction that WOULD have executed (so RFE returns and runs it),
        // and CAUSE.BD must be clear (that instruction is not in a delay slot).
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0x11), // 0x00 runs normally
            i_type(0x09, 0, 2, 0x22), // 0x04 IRQ fires before this one
        ]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 1); // execute instr at 0x00; now pc = 0x04
        // Raise the IRQ at the boundary before the instruction at 0x04.
        psx.cpu.cop0.sr |= SR_IEC | (CAUSE_IP_HW & SR_IM);
        psx.cpu.irq_pending = true;
        run(&mut psx, 1);
        assert_eq!(get(&psx, 2), 0, "instr at 0x04 did not execute");
        assert_eq!(psx.cpu.cop0.epc, base + 4, "EPC = the deferred instruction");
        assert_eq!(psx.cpu.cop0.cause & cop0::CAUSE_BD, 0, "BD clear");
    }

    #[test]
    fn interrupt_in_branch_delay_sets_bd() {
        // An IRQ sampled when the next instruction sits in a branch delay slot
        // must set CAUSE.BD and back EPC up to the branch.
        let base = 0x8000_0000u32;
        let mut psx = harness(&[
            j_type(0x02, (base + 0x40) >> 2), // 0x00 J
            i_type(0x09, 0, 1, 0x11),         // 0x04 delay slot
        ]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 1); // execute the J; next instr (0x04) is the delay slot
        psx.cpu.cop0.sr |= SR_IEC | (CAUSE_IP_HW & SR_IM);
        psx.cpu.irq_pending = true;
        run(&mut psx, 1); // IRQ sampled with the delay-slot instruction pending
        assert_ne!(psx.cpu.cop0.cause & cop0::CAUSE_BD, 0, "BD set in delay slot");
        assert_eq!(psx.cpu.cop0.epc, base, "EPC backs up to the branch");
    }

    #[test]
    fn div_intmin_by_neg_one() {
        // div -80000000h / -1 -> Hi=0, Lo=-80000000h (psx-spx defined garbage).
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x8000), // LUI r1 = 0x80000000
            i_type(0x09, 0, 2, 0xFFFF), // r2 = -1
            r_type(0x1A, 1, 2, 0, 0),   // DIV r1, r2
            r_type(0x12, 0, 0, 3, 0),   // MFLO -> r3
            r_type(0x10, 0, 0, 4, 0),   // MFHI -> r4
        ]);
        run(&mut psx, 5);
        assert_eq!(get(&psx, 3), 0x8000_0000, "LO = INT_MIN");
        assert_eq!(get(&psx, 4), 0, "HI = 0");
    }

    #[test]
    fn div_by_zero_negative_dividend() {
        // div (negative) / 0 -> Lo = +1, Hi = dividend.
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0xFFCE), // r1 = -50
            r_type(0x1A, 1, 0, 0, 0),   // DIV r1, r0
            r_type(0x12, 0, 0, 3, 0),   // MFLO
            r_type(0x10, 0, 0, 4, 0),   // MFHI
        ]);
        run(&mut psx, 4);
        assert_eq!(get(&psx, 3), 1, "LO = +1 for negative dividend / 0");
        assert_eq!(get(&psx, 4), (-50i32) as u32, "HI = dividend");
    }

    #[test]
    fn break_opcode_uses_general_vector() {
        // The BREAK opcode jumps to the normal exception handler (0x80000080
        // when BEV=0), NOT the 0x80000040 hardware-breakpoint vector.
        let mut psx = harness(&[r_type(0x0D, 0, 0, 0, 0)]); // BREAK
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 1);
        assert_eq!(psx.cpu.pc, cop0::VECTOR_RAM, "0x80000080, not 0x80000040");
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::Breakpoint.code()
        );
    }

    #[test]
    fn mfc0_has_load_delay() {
        // MFC0's destination is not usable by the immediately following opcode
        // (same one-instruction delay as a memory load).
        let mut psx = harness(&[
            i_type(0x0F, 0, 1, 0x0000),                          // LUI r1, 0
            i_type(0x0D, 1, 1, 0xABCD),                          // r1 = 0xABCD
            (0x10 << 26) | (0x04 << 21) | (1 << 16) | (3 << 11), // MTC0 r1 -> cop0 r3
            (0x10 << 26) | (0x00 << 21) | (2 << 16) | (3 << 11), // MFC0 r2 <- cop0 r3
            i_type(0x09, 2, 5, 0),                               // ADDIU r5,r2 (delay slot: OLD r2)
            i_type(0x09, 2, 6, 0),                               // ADDIU r6,r2 (NEW r2)
        ]);
        run(&mut psx, 6);
        assert_eq!(get(&psx, 5), 0, "MFC0 delay slot sees old r2");
        assert_eq!(get(&psx, 6), 0xABCD, "MFC0 value visible one instr later");
    }

    #[test]
    fn store_address_error_traps() {
        // SW to a misaligned address raises AddressErrorStore and sets BadVaddr.
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 0x201), // r1 = 0x201 (misaligned for SW)
            i_type(0x2B, 1, 2, 0),     // SW r2, 0(r1)
        ]);
        psx.cpu.cop0.sr &= !cop0::SR_BEV;
        run(&mut psx, 2);
        assert_eq!(
            (psx.cpu.cop0.cause & cop0::CAUSE_EXCCODE_MASK) >> cop0::CAUSE_EXCCODE_SHIFT,
            Exception::AddressErrorStore.code()
        );
        assert_eq!(psx.cpu.cop0.bad_vaddr, 0x201);
    }

    #[test]
    fn tiny_program_sum_loop() {
        // Sum 1..=5 into r2 with a countdown loop.
        //   r1 = 5 ; r2 = 0
        // loop: r2 += r1 ; r1 -= 1 ; BNE r1,r0,loop ; NOP
        let mut psx = harness(&[
            i_type(0x09, 0, 1, 5),      // 0x00
            i_type(0x09, 0, 2, 0),      // 0x04
            r_type(0x21, 2, 1, 2, 0),   // 0x08: ADDU r2 += r1
            i_type(0x09, 1, 1, 0xFFFF), // 0x0C: r1 -= 1
            i_type(0x05, 1, 0, 0xFFFD), // 0x10: BNE r1,r0,loop (-3)
            i_type(0x00, 0, 0, 0),      // 0x14: delay slot NOP
        ]);
        run(&mut psx, 40);
        assert_eq!(get(&psx, 2), 15, "1+2+3+4+5");
        assert_eq!(get(&psx, 1), 0);
    }
}
