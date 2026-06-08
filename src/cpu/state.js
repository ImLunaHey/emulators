// ARM7TDMI processor state — banked register file, CPSR, SPSR.
export var Mode;
(function (Mode) {
    Mode[Mode["USR"] = 16] = "USR";
    Mode[Mode["FIQ"] = 17] = "FIQ";
    Mode[Mode["IRQ"] = 18] = "IRQ";
    Mode[Mode["SVC"] = 19] = "SVC";
    Mode[Mode["ABT"] = 23] = "ABT";
    Mode[Mode["UND"] = 27] = "UND";
    Mode[Mode["SYS"] = 31] = "SYS";
})(Mode || (Mode = {}));
export const FLAG_N = 0x80000000 | 0;
export const FLAG_Z = 0x40000000 | 0;
export const FLAG_C = 0x20000000 | 0;
export const FLAG_V = 0x10000000 | 0;
export const FLAG_I = 0x80;
export const FLAG_F = 0x40;
export const FLAG_T = 0x20;
// Bank indices into the banked-store arrays.
const BANK_USR = 0;
const BANK_FIQ = 1;
const BANK_IRQ = 2;
const BANK_SVC = 3;
const BANK_ABT = 4;
const BANK_UND = 5;
function modeBank(mode) {
    switch (mode) {
        case Mode.FIQ: return BANK_FIQ;
        case Mode.IRQ: return BANK_IRQ;
        case Mode.SVC: return BANK_SVC;
        case Mode.ABT: return BANK_ABT;
        case Mode.UND: return BANK_UND;
        default: return BANK_USR;
    }
}
export class CpuState {
    // Visible register file (R0-R15).
    r = new Uint32Array(16);
    // Banked R13, R14, SPSR for each non-user mode.
    bank_r13 = new Uint32Array(6);
    bank_r14 = new Uint32Array(6);
    bank_spsr = new Uint32Array(6);
    // FIQ also banks R8..R12; we also store the user copies when in FIQ mode.
    fiq_r8_12 = new Uint32Array(5);
    usr_r8_12 = new Uint32Array(5);
    // Saved USR R13/R14 so they're untouched while in non-USR mode.
    usr_r13 = 0;
    usr_r14 = 0;
    cpsr = 0;
    // SPSR is read from the bank corresponding to current mode (no SPSR in USR/SYS).
    // We model the current SPSR by mirroring the bank.
    halted = false;
    // Pipeline state — PC visible to ARM instructions is r[15] which points to
    // the instruction *2 ahead* in ARM (PC+8 from fetched insn). The interpreter
    // keeps r[15] equal to the address of the instruction being decoded plus 8
    // (or +4 in THUMB) so any read of R15 yields the architectural PC.
    constructor() {
        // Start in SVC mode after reset, IRQ+FIQ disabled, ARM state.
        this.cpsr = Mode.SVC | FLAG_I | FLAG_F;
    }
    mode() { return this.cpsr & 0x1F; }
    inThumb() { return (this.cpsr & FLAG_T) !== 0; }
    irqDisabled() { return (this.cpsr & FLAG_I) !== 0; }
    setNZ(value) {
        let cpsr = this.cpsr;
        cpsr &= ~(FLAG_N | FLAG_Z);
        if ((value | 0) < 0)
            cpsr |= FLAG_N;
        if ((value & 0xFFFFFFFF) === 0)
            cpsr |= FLAG_Z;
        this.cpsr = cpsr;
    }
    setNZ64Hi(hi, lo) {
        let cpsr = this.cpsr;
        cpsr &= ~(FLAG_N | FLAG_Z);
        if ((hi | 0) < 0)
            cpsr |= FLAG_N;
        if (hi === 0 && lo === 0)
            cpsr |= FLAG_Z;
        this.cpsr = cpsr;
    }
    setC(c) {
        if (c)
            this.cpsr |= FLAG_C;
        else
            this.cpsr &= ~FLAG_C;
    }
    setV(v) {
        if (v)
            this.cpsr |= FLAG_V;
        else
            this.cpsr &= ~FLAG_V;
    }
    c() { return (this.cpsr >>> 29) & 1; }
    // Condition code check — used by every ARM instruction.
    checkCond(cond) {
        const cpsr = this.cpsr;
        const n = (cpsr & FLAG_N) !== 0;
        const z = (cpsr & FLAG_Z) !== 0;
        const c = (cpsr & FLAG_C) !== 0;
        const v = (cpsr & FLAG_V) !== 0;
        switch (cond) {
            case 0x0: return z; // EQ
            case 0x1: return !z; // NE
            case 0x2: return c; // CS / HS
            case 0x3: return !c; // CC / LO
            case 0x4: return n; // MI
            case 0x5: return !n; // PL
            case 0x6: return v; // VS
            case 0x7: return !v; // VC
            case 0x8: return c && !z; // HI
            case 0x9: return !c || z; // LS
            case 0xA: return n === v; // GE
            case 0xB: return n !== v; // LT
            case 0xC: return !z && n === v; // GT
            case 0xD: return z || n !== v; // LE
            case 0xE: return true; // AL
            default: return false; // NV (or v5+ extensions)
        }
    }
    // Switch CPU mode, performing bank save/restore. The new CPSR's M field
    // determines the destination.
    switchMode(newMode) {
        const oldMode = this.mode();
        if (oldMode === newMode)
            return;
        const oldBank = modeBank(oldMode);
        const newBank = modeBank(newMode);
        // --- Save old banked regs.
        if (oldBank === BANK_USR) {
            this.usr_r13 = this.r[13];
            this.usr_r14 = this.r[14];
        }
        else {
            this.bank_r13[oldBank] = this.r[13];
            this.bank_r14[oldBank] = this.r[14];
        }
        // R8..R12 only bank for FIQ.
        if (oldBank === BANK_FIQ) {
            for (let i = 0; i < 5; i++)
                this.fiq_r8_12[i] = this.r[8 + i];
        }
        else {
            for (let i = 0; i < 5; i++)
                this.usr_r8_12[i] = this.r[8 + i];
        }
        // --- Restore new banked regs.
        if (newBank === BANK_USR) {
            this.r[13] = this.usr_r13;
            this.r[14] = this.usr_r14;
        }
        else {
            this.r[13] = this.bank_r13[newBank];
            this.r[14] = this.bank_r14[newBank];
        }
        if (newBank === BANK_FIQ) {
            for (let i = 0; i < 5; i++)
                this.r[8 + i] = this.fiq_r8_12[i];
        }
        else {
            for (let i = 0; i < 5; i++)
                this.r[8 + i] = this.usr_r8_12[i];
        }
        this.cpsr = (this.cpsr & ~0x1F) | (newMode & 0x1F);
    }
    // SPSR access for the current mode (USR/SYS have none, fall back to CPSR).
    getSpsr() {
        const b = modeBank(this.mode());
        if (b === BANK_USR)
            return this.cpsr;
        return this.bank_spsr[b];
    }
    setSpsr(v) {
        const b = modeBank(this.mode());
        if (b === BANK_USR)
            return;
        this.bank_spsr[b] = v >>> 0;
    }
    // Enter an exception: save PC + CPSR into the target mode's banked LR/SPSR,
    // switch mode, clear T, set I (and F for reset/FIQ), set PC to vector.
    enterException(targetMode, vector, savedPc, setF) {
        const oldCpsr = this.cpsr;
        const targetBank = modeBank(targetMode);
        // Save current banked regs before changing mode.
        this.switchMode(targetMode);
        // After switchMode, r[14] is the destination LR.
        this.r[14] = savedPc >>> 0;
        this.bank_spsr[targetBank] = oldCpsr >>> 0;
        this.cpsr = (this.cpsr & ~FLAG_T) | FLAG_I;
        if (setF)
            this.cpsr |= FLAG_F;
        this.r[15] = vector >>> 0;
    }
}
