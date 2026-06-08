import { FLAG_C } from './state';
export const SHIFT_LSL = 0;
export const SHIFT_LSR = 1;
export const SHIFT_ASR = 2;
export const SHIFT_ROR = 3;
// Shift by an immediate amount as encoded in ARM data processing.
// `op` is bits 6:5, `imm` is bits 11:7.
export function immShift(op, imm, value, carryIn) {
    switch (op) {
        case SHIFT_LSL:
            if (imm === 0)
                return { value: value >>> 0, carry: carryIn };
            return { value: (value << imm) >>> 0, carry: (value >>> (32 - imm)) & 1 };
        case SHIFT_LSR:
            if (imm === 0)
                return { value: 0, carry: (value >>> 31) & 1 }; // LSR #32
            return { value: value >>> imm, carry: (value >>> (imm - 1)) & 1 };
        case SHIFT_ASR:
            if (imm === 0) {
                const s = (value | 0) >> 31;
                return { value: s >>> 0, carry: (value >>> 31) & 1 };
            }
            return { value: ((value | 0) >> imm) >>> 0, carry: ((value | 0) >> (imm - 1)) & 1 };
        case SHIFT_ROR:
            if (imm === 0) { // RRX
                const carry = value & 1;
                return { value: ((carryIn << 31) | (value >>> 1)) >>> 0, carry };
            }
            return { value: ((value >>> imm) | (value << (32 - imm))) >>> 0, carry: (value >>> (imm - 1)) & 1 };
    }
    return { value: value >>> 0, carry: carryIn };
}
// Shift by a register amount — amount is the bottom 8 bits of Rs.
export function regShift(op, amount, value, carryIn) {
    amount &= 0xFF;
    if (amount === 0)
        return { value: value >>> 0, carry: carryIn };
    switch (op) {
        case SHIFT_LSL:
            if (amount < 32)
                return { value: (value << amount) >>> 0, carry: (value >>> (32 - amount)) & 1 };
            if (amount === 32)
                return { value: 0, carry: value & 1 };
            return { value: 0, carry: 0 };
        case SHIFT_LSR:
            if (amount < 32)
                return { value: value >>> amount, carry: (value >>> (amount - 1)) & 1 };
            if (amount === 32)
                return { value: 0, carry: (value >>> 31) & 1 };
            return { value: 0, carry: 0 };
        case SHIFT_ASR:
            if (amount < 32)
                return { value: ((value | 0) >> amount) >>> 0, carry: ((value | 0) >> (amount - 1)) & 1 };
            {
                const sign = (value >>> 31) & 1;
                return { value: sign ? 0xFFFFFFFF : 0, carry: sign };
            }
        case SHIFT_ROR: {
            const a = amount & 31;
            if (a === 0)
                return { value: value >>> 0, carry: (value >>> 31) & 1 };
            return { value: ((value >>> a) | (value << (32 - a))) >>> 0, carry: (value >>> (a - 1)) & 1 };
        }
    }
    return { value: value >>> 0, carry: carryIn };
}
// Apply a shift result back to the CPU C flag.
export function applyCarry(state, carry) {
    if (carry)
        state.cpsr |= FLAG_C;
    else
        state.cpsr &= ~FLAG_C;
}
// Rotate-right immediate, used for data-processing immediate operand encoding.
export function rorImm32(value, amount) {
    amount &= 31;
    if (amount === 0)
        return value >>> 0;
    return ((value >>> amount) | (value << (32 - amount))) >>> 0;
}
