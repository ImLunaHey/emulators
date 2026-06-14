//! ARM barrel shifter. Ported from ../../ds-recomp/src/cpu/shifter.ts, which
//! is itself the GBA core's tested `shifter.rs` verbatim (the DS reused it).
//!
//! Shared by both the ARM9 (ARMv5TE) and ARM7 (ARMv4T) — the barrel shifter is
//! architecturally identical across the two cores.

use crate::state::{CpuState, FLAG_C};

/// Barrel shifter result: the 32-bit value plus the new carry-out (0 or 1).
pub struct ShiftResult {
    pub value: u32,
    pub carry: u32,
}

pub const SHIFT_LSL: u32 = 0;
pub const SHIFT_LSR: u32 = 1;
pub const SHIFT_ASR: u32 = 2;
pub const SHIFT_ROR: u32 = 3;

/// Shift by an immediate amount as encoded in ARM data processing.
/// `op` is bits 6:5, `imm` is bits 11:7.
pub fn imm_shift(op: u32, imm: u32, value: u32, carry_in: u32) -> ShiftResult {
    match op {
        SHIFT_LSL => {
            if imm == 0 {
                return ShiftResult {
                    value,
                    carry: carry_in,
                };
            }
            ShiftResult {
                value: value << imm,
                carry: (value >> (32 - imm)) & 1,
            }
        }
        SHIFT_LSR => {
            if imm == 0 {
                // LSR #0 encodes LSR #32.
                return ShiftResult {
                    value: 0,
                    carry: (value >> 31) & 1,
                };
            }
            ShiftResult {
                value: value >> imm,
                carry: (value >> (imm - 1)) & 1,
            }
        }
        SHIFT_ASR => {
            if imm == 0 {
                // ASR #0 encodes ASR #32.
                let s = (value as i32) >> 31;
                return ShiftResult {
                    value: s as u32,
                    carry: (value >> 31) & 1,
                };
            }
            ShiftResult {
                value: ((value as i32) >> imm) as u32,
                carry: (((value as i32) >> (imm - 1)) as u32) & 1,
            }
        }
        SHIFT_ROR => {
            if imm == 0 {
                // ROR #0 encodes RRX (rotate right through carry).
                let carry = value & 1;
                return ShiftResult {
                    value: (carry_in << 31) | (value >> 1),
                    carry,
                };
            }
            ShiftResult {
                value: (value >> imm) | (value << (32 - imm)),
                carry: (value >> (imm - 1)) & 1,
            }
        }
        _ => ShiftResult {
            value,
            carry: carry_in,
        },
    }
}

/// Shift by a register amount — `amount` is the bottom 8 bits of Rs.
pub fn reg_shift(op: u32, amount: u32, value: u32, carry_in: u32) -> ShiftResult {
    let amount = amount & 0xFF;
    if amount == 0 {
        return ShiftResult {
            value,
            carry: carry_in,
        };
    }
    match op {
        SHIFT_LSL => {
            if amount < 32 {
                return ShiftResult {
                    value: value << amount,
                    carry: (value >> (32 - amount)) & 1,
                };
            }
            if amount == 32 {
                return ShiftResult {
                    value: 0,
                    carry: value & 1,
                };
            }
            ShiftResult { value: 0, carry: 0 }
        }
        SHIFT_LSR => {
            if amount < 32 {
                return ShiftResult {
                    value: value >> amount,
                    carry: (value >> (amount - 1)) & 1,
                };
            }
            if amount == 32 {
                return ShiftResult {
                    value: 0,
                    carry: (value >> 31) & 1,
                };
            }
            ShiftResult { value: 0, carry: 0 }
        }
        SHIFT_ASR => {
            if amount < 32 {
                return ShiftResult {
                    value: ((value as i32) >> amount) as u32,
                    carry: (((value as i32) >> (amount - 1)) as u32) & 1,
                };
            }
            let sign = (value >> 31) & 1;
            ShiftResult {
                value: if sign != 0 { 0xFFFF_FFFF } else { 0 },
                carry: sign,
            }
        }
        SHIFT_ROR => {
            let a = amount & 31;
            if a == 0 {
                return ShiftResult {
                    value,
                    carry: (value >> 31) & 1,
                };
            }
            ShiftResult {
                value: (value >> a) | (value << (32 - a)),
                carry: (value >> (a - 1)) & 1,
            }
        }
        _ => ShiftResult {
            value,
            carry: carry_in,
        },
    }
}

/// Fold a shifter carry-out back into the CPU `C` flag.
pub fn apply_carry(state: &mut CpuState, carry: u32) {
    if carry != 0 {
        state.cpsr |= FLAG_C;
    } else {
        state.cpsr &= !FLAG_C;
    }
}

/// Rotate-right immediate, used for the data-processing immediate operand.
pub fn ror_imm32(value: u32, amount: u32) -> u32 {
    let amount = amount & 31;
    if amount == 0 {
        return value;
    }
    (value >> amount) | (value << (32 - amount))
}
