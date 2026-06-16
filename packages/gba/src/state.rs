//! ARM7TDMI processor state — banked register file, CPSR, SPSR.
//! Ported 1:1 from src/cpu/state.ts.

// Mode field values (CPSR[4:0]).
pub mod mode {
    pub const USR: u32 = 0x10;
    pub const FIQ: u32 = 0x11;
    pub const IRQ: u32 = 0x12;
    pub const SVC: u32 = 0x13;
    pub const ABT: u32 = 0x17;
    pub const UND: u32 = 0x1B;
    pub const SYS: u32 = 0x1F;
}

pub const FLAG_N: u32 = 0x8000_0000;
pub const FLAG_Z: u32 = 0x4000_0000;
pub const FLAG_C: u32 = 0x2000_0000;
pub const FLAG_V: u32 = 0x1000_0000;
pub const FLAG_I: u32 = 0x80;
pub const FLAG_F: u32 = 0x40;
pub const FLAG_T: u32 = 0x20;

const BANK_USR: usize = 0;
const BANK_FIQ: usize = 1;
const BANK_IRQ: usize = 2;
const BANK_SVC: usize = 3;
const BANK_ABT: usize = 4;
const BANK_UND: usize = 5;

fn mode_bank(mode: u32) -> usize {
    match mode {
        mode::FIQ => BANK_FIQ,
        mode::IRQ => BANK_IRQ,
        mode::SVC => BANK_SVC,
        mode::ABT => BANK_ABT,
        mode::UND => BANK_UND,
        _ => BANK_USR,
    }
}

pub struct CpuState {
    /// Visible register file (R0-R15).
    pub r: [u32; 16],

    /// Banked R13, R14, SPSR for each non-user mode.
    pub bank_r13: [u32; 6],
    pub bank_r14: [u32; 6],
    pub bank_spsr: [u32; 6],
    /// FIQ also banks R8..R12; we also store the user copies when in FIQ mode.
    pub fiq_r8_12: [u32; 5],
    pub usr_r8_12: [u32; 5],
    /// Saved USR R13/R14 so they're untouched while in non-USR mode.
    pub usr_r13: u32,
    pub usr_r14: u32,

    pub cpsr: u32,

    pub halted: bool,
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuState {
    pub fn new() -> Self {
        CpuState {
            r: [0; 16],
            bank_r13: [0; 6],
            bank_r14: [0; 6],
            bank_spsr: [0; 6],
            fiq_r8_12: [0; 5],
            usr_r8_12: [0; 5],
            usr_r13: 0,
            usr_r14: 0,
            // Start in SVC mode after reset, IRQ+FIQ disabled, ARM state.
            cpsr: mode::SVC | FLAG_I | FLAG_F,
            halted: false,
        }
    }

    #[inline]
    pub fn mode(&self) -> u32 {
        self.cpsr & 0x1F
    }
    #[inline]
    pub fn in_thumb(&self) -> bool {
        (self.cpsr & FLAG_T) != 0
    }
    #[inline]
    pub fn irq_disabled(&self) -> bool {
        (self.cpsr & FLAG_I) != 0
    }

    pub fn set_nz(&mut self, value: u32) {
        let mut cpsr = self.cpsr;
        cpsr &= !(FLAG_N | FLAG_Z);
        if (value & 0x8000_0000) != 0 {
            cpsr |= FLAG_N;
        }
        if value == 0 {
            cpsr |= FLAG_Z;
        }
        self.cpsr = cpsr;
    }

    pub fn set_nz64_hi(&mut self, hi: u32, lo: u32) {
        let mut cpsr = self.cpsr;
        cpsr &= !(FLAG_N | FLAG_Z);
        if (hi & 0x8000_0000) != 0 {
            cpsr |= FLAG_N;
        }
        if hi == 0 && lo == 0 {
            cpsr |= FLAG_Z;
        }
        self.cpsr = cpsr;
    }

    pub fn set_c(&mut self, c: bool) {
        if c {
            self.cpsr |= FLAG_C;
        } else {
            self.cpsr &= !FLAG_C;
        }
    }
    pub fn set_v(&mut self, v: bool) {
        if v {
            self.cpsr |= FLAG_V;
        } else {
            self.cpsr &= !FLAG_V;
        }
    }
    #[inline]
    pub fn c(&self) -> u32 {
        (self.cpsr >> 29) & 1
    }

    /// Condition code check — used by every ARM instruction.
    pub fn check_cond(&self, cond: u32) -> bool {
        let cpsr = self.cpsr;
        let n = (cpsr & FLAG_N) != 0;
        let z = (cpsr & FLAG_Z) != 0;
        let c = (cpsr & FLAG_C) != 0;
        let v = (cpsr & FLAG_V) != 0;
        match cond {
            0x0 => z,             // EQ
            0x1 => !z,            // NE
            0x2 => c,             // CS / HS
            0x3 => !c,            // CC / LO
            0x4 => n,             // MI
            0x5 => !n,            // PL
            0x6 => v,             // VS
            0x7 => !v,            // VC
            0x8 => c && !z,       // HI
            0x9 => !c || z,       // LS
            0xA => n == v,        // GE
            0xB => n != v,        // LT
            0xC => !z && n == v,  // GT
            0xD => z || n != v,   // LE
            0xE => true,          // AL
            _ => false,           // NV
        }
    }

    /// Switch CPU mode, performing bank save/restore. The new CPSR's M field
    /// determines the destination.
    pub fn switch_mode(&mut self, new_mode: u32) {
        let old_mode = self.mode();
        if old_mode == new_mode {
            return;
        }

        let old_bank = mode_bank(old_mode);
        let new_bank = mode_bank(new_mode);

        // --- Save old banked regs.
        if old_bank == BANK_USR {
            self.usr_r13 = self.r[13];
            self.usr_r14 = self.r[14];
        } else {
            self.bank_r13[old_bank] = self.r[13];
            self.bank_r14[old_bank] = self.r[14];
        }
        // R8..R12 only bank for FIQ.
        if old_bank == BANK_FIQ {
            for i in 0..5 {
                self.fiq_r8_12[i] = self.r[8 + i];
            }
        } else {
            for i in 0..5 {
                self.usr_r8_12[i] = self.r[8 + i];
            }
        }

        // --- Restore new banked regs.
        if new_bank == BANK_USR {
            self.r[13] = self.usr_r13;
            self.r[14] = self.usr_r14;
        } else {
            self.r[13] = self.bank_r13[new_bank];
            self.r[14] = self.bank_r14[new_bank];
        }
        if new_bank == BANK_FIQ {
            for i in 0..5 {
                self.r[8 + i] = self.fiq_r8_12[i];
            }
        } else {
            for i in 0..5 {
                self.r[8 + i] = self.usr_r8_12[i];
            }
        }

        self.cpsr = (self.cpsr & !0x1F) | (new_mode & 0x1F);
    }

    /// SPSR access for the current mode (USR/SYS have none, fall back to CPSR).
    pub fn get_spsr(&self) -> u32 {
        let b = mode_bank(self.mode());
        if b == BANK_USR {
            return self.cpsr;
        }
        self.bank_spsr[b]
    }
    pub fn set_spsr(&mut self, v: u32) {
        let b = mode_bank(self.mode());
        if b == BANK_USR {
            return;
        }
        self.bank_spsr[b] = v;
    }

    /// Enter an exception: save PC + CPSR into the target mode's banked
    /// LR/SPSR, switch mode, clear T, set I (and F for reset/FIQ), set PC to
    /// vector.
    pub fn enter_exception(&mut self, target_mode: u32, vector: u32, saved_pc: u32, set_f: bool) {
        let old_cpsr = self.cpsr;
        let target_bank = mode_bank(target_mode);
        self.switch_mode(target_mode);
        self.r[14] = saved_pc;
        self.bank_spsr[target_bank] = old_cpsr;
        self.cpsr = (self.cpsr & !FLAG_T) | FLAG_I;
        if set_f {
            self.cpsr |= FLAG_F;
        }
        self.r[15] = vector;
    }
}
