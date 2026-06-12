import type { Cpu } from '../cpu/cpu';
import * as W from './wasm-emit';

// Basic-block THUMB recompiler. Tracks hot PCs; when one crosses the
// threshold, scan forward emitting WASM ops until we hit a branch or an
// unsupported instruction, then build/instantiate the module once and
// keep calling it whenever the dispatcher sees the same PC again. ARM
// blocks are not jitted in this build — only the much-more-common
// THUMB path.
//
// The compiled function signature is `(unused: i32) -> i32` and it
// returns the number of THUMB instructions it executed. The host uses
// that to advance its own instruction-count statistics.
//
// REGISTER + CPSR STATE LIVES IN WASM LINEAR MEMORY.
//
// All compiled modules share one imported WebAssembly.Memory owned by
// the Recompiler. The first 68 bytes lay out:
//   offset 0x00..0x3F  -- r[0..15]   (16 × u32)
//   offset 0x40        -- cpsr       (u32)
// register access compiles to a single i32.load/i32.store against a
// constant offset; the previous build went through getR/setR import
// callbacks (50-150ns each, ~3× per simple ALU op) so this is where
// the JIT actually starts winning against the interpreter. State syncs
// in/out of the WASM memory at the dispatch boundary (tryDispatch).
// Bus memory I/O (r32/w32/r16/w16/r8/w8) is still imported, since the
// bus does region routing the JIT doesn't model yet.

interface CompiledBlock {
  run: () => number;
  startPc: number;
  insnCount: number;
}

const HOT_THRESHOLD = 50;
const MAX_BLOCK_INSNS = 32;

// Linear memory layout (offsets into the shared WebAssembly.Memory).
const REG_BASE  = 0;
const CPSR_OFF  = 64;

export class Recompiler {
  cache = new Map<number, CompiledBlock | null>();   // null = compile failed
  hits = new Map<number, number>();
  jitInsns = 0;
  intInsns = 0;
  // Opt-in. The linear-memory refactor (see file header) makes the
  // JIT ~5× faster than the interpreter on a pure ALU+branch micro-
  // bench, but real-game performance hasn't been re-measured since;
  // leave the flag off by default until that bake-in is done. Tests
  // enable it explicitly.
  enabled = false;

  // Shared linear memory for all compiled modules. 1 page = 64KB,
  // dwarfs what we use today (68 bytes) but leaves room for future
  // in-memory IWRAM/EWRAM mirroring.
  private mem: WebAssembly.Memory;
  // Uint32Array view onto the same buffer for fast JS-side state sync.
  // Stable as long as we never grow the memory.
  private memU32: Uint32Array;

  // Host hooks bound at construction. Only bus I/O + SWI/exception
  // helpers go through them now; register and CPSR access happens
  // entirely in WASM.
  private hooks: {
    r32: (a: number) => number;  w32: (a: number, v: number) => void;
    r16: (a: number) => number;  w16: (a: number, v: number) => void;
    r8:  (a: number) => number;  w8:  (a: number, v: number) => void;
  };
  // Module-level imports object shape — populated lazily per compile.
  private importsObj: { h: Record<string, Function>; m: { mem: WebAssembly.Memory } };

  constructor(public cpu: Cpu) {
    this.mem = new WebAssembly.Memory({ initial: 1 });
    this.memU32 = new Uint32Array(this.mem.buffer);
    const bus = cpu.bus;
    this.hooks = {
      r32: (a) => bus.read32(a >>> 0),
      w32: (a, v) => bus.write32(a >>> 0, v >>> 0),
      r16: (a) => bus.read16(a >>> 0),
      w16: (a, v) => bus.write16(a >>> 0, v & 0xFFFF),
      r8:  (a) => bus.read8(a >>> 0),
      w8:  (a, v) => bus.write8(a >>> 0, v & 0xFF),
    };
    this.importsObj = {
      h: this.hooks as unknown as Record<string, Function>,
      m: { mem: this.mem },
    };
  }

  // Returns the number of THUMB instructions executed by the JIT block
  // it dispatched, or 0 if no dispatch happened (interpreter must
  // handle this PC). Callers MUST advance their own cycle/instruction
  // counter by the returned value, not by 1 — a compiled block is
  // typically 5-32 instructions, and treating it as a single cycle
  // makes runFrame consume the whole frame budget in far fewer
  // iterations, effectively running ~N× more game code per real-time
  // VBlank than the interpreter. That's the "JIT runs too fast" bug.
  tryDispatch(): number {
    if (!this.enabled) return 0;
    const s = this.cpu.state;
    if (!(s.cpsr & 0x20)) return 0;            // ARM blocks not jitted
    const pc = s.r[15] & ~1;
    const cached = this.cache.get(pc);
    if (cached === null) return 0;              // known-uncompilable
    if (cached) {
      return this.runBlock(cached);
    }
    const c = (this.hits.get(pc) || 0) + 1;
    this.hits.set(pc, c);
    if (c < HOT_THRESHOLD) return 0;
    this.hits.delete(pc);
    const block = this.compile(pc);
    this.cache.set(pc, block);
    if (!block) return 0;
    return this.runBlock(block);
  }

  invalidate(): void { this.cache.clear(); this.hits.clear(); }

  // Push JS-side CpuState into linear memory, run the block, pull state
  // back. The copy is 17 u32 writes either side (~50ns total) — small
  // enough that even short blocks come out ahead of the interpreter.
  private runBlock(block: CompiledBlock): number {
    const s = this.cpu.state;
    const mem = this.memU32;
    // In: r[0..15], cpsr.
    for (let i = 0; i < 16; i++) mem[i] = s.r[i];
    mem[CPSR_OFF >> 2] = s.cpsr >>> 0;
    const n = block.run();
    // Out: same.
    for (let i = 0; i < 16; i++) s.r[i] = mem[i];
    s.cpsr = mem[CPSR_OFF >> 2] | 0;
    this.jitInsns += n;
    return n;
  }

  private compile(startPc: number): CompiledBlock | null {
    const bus = this.cpu.bus;
    const builder = new W.WasmModuleBuilder();
    const f = builder.func;

    // Memory import — shared across every compiled module.
    builder.importMemory('m', 'mem', 1);
    // Bus I/O imports — the bus still does the region routing.
    const I_r32 = builder.addImport('h', 'r32', [W.I32], [W.I32]);
    const I_w32 = builder.addImport('h', 'w32', [W.I32, W.I32], []);
    const I_r16 = builder.addImport('h', 'r16', [W.I32], [W.I32]);
    const I_w16 = builder.addImport('h', 'w16', [W.I32, W.I32], []);
    const I_r8  = builder.addImport('h', 'r8',  [W.I32], [W.I32]);
    const I_w8  = builder.addImport('h', 'w8',  [W.I32, W.I32], []);

    // Locals. f has 1 i32 param (unused), so locals start at index 1.
    f.addLocals(6, W.I32);
    const L_A = 1, L_B = 2, L_R = 3, L_TMP = 4, L_TMP2 = 5, L_C = 6;

    // ----- emitter primitives. All assume linear memory base is at 0.

    // Push r[rd] onto the stack.
    const pushReg = (rd: number) => {
      f.i32Const(0);                          // base addr
      f.i32Load(REG_BASE + rd * 4);
    };

    // Store value-in-local into r[rd]. Caller is responsible for
    // putting the value into `localIdx` (typical pattern: compute,
    // local.tee L; ...; storeRegFromLocal(rd, L)). We don't use the
    // direct setReg(rd, expr) form because i32.store wants
    // [addr, value] on the stack and reordering after computation
    // is fiddly.
    const storeRegFromLocal = (rd: number, localIdx: number) => {
      f.i32Const(0);
      f.localGet(localIdx);
      f.i32Store(REG_BASE + rd * 4);
    };

    // Store the i32 constant `val` into r[rd].
    const storeRegConst = (rd: number, val: number) => {
      f.i32Const(0);
      f.i32Const(val | 0);
      f.i32Store(REG_BASE + rd * 4);
    };

    // Load a word with ARM unaligned-LDR semantics, matching the
    // interpreter: read32(addr & ~3), then rotate the result right by
    // (addr & 3) * 8 bits. rotr(v, 0) is a no-op, so the aligned case
    // needs no branch — the rotate amount is computed unconditionally.
    // `addrLocal` holds the unmasked address; the rotated word lands in
    // `destLocal`. Shared by Formats 7/9/11 word loads.
    const emitLoadWordRotated = (addrLocal: number, destLocal: number) => {
      // read32(addr & ~3)
      f.localGet(addrLocal);
      f.i32Const(~3); f.op(W.OP_I32_AND);
      f.call(I_r32);
      // rotr by (addr & 3) << 3
      f.localGet(addrLocal);
      f.i32Const(3); f.op(W.OP_I32_AND);
      f.i32Const(3); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_ROTR);
      f.localSet(destLocal);
    };

    // Store a word with ARM semantics, matching the interpreter:
    // write32(addr & ~3, value). `addrLocal` holds the unmasked
    // address, `valLocal` the value. Shared by word stores.
    const emitStoreWordMasked = (addrLocal: number, valLocal: number) => {
      f.localGet(addrLocal);
      f.i32Const(~3); f.op(W.OP_I32_AND);
      f.localGet(valLocal);
      f.call(I_w32);
    };

    // Load a halfword with ARM unaligned-LDRH semantics, matching the
    // interpreter: read16(addr & ~1), then rotate the result right by
    // (addr & 1) * 8 bits. r16 returns ≤ 0xFFFF, so
    // rotr(v, 8) == (v >>> 8) | (v << 24) — exactly the interpreter's
    // formula — and rotr(v, 0) is a no-op, so no branch is needed.
    // Shared by Format 8/10 LDRH.
    const emitLoadHalfRotated = (addrLocal: number, destLocal: number) => {
      // read16(addr & ~1)
      f.localGet(addrLocal);
      f.i32Const(~1); f.op(W.OP_I32_AND);
      f.call(I_r16);
      // rotr by (addr & 1) << 3
      f.localGet(addrLocal);
      f.i32Const(1); f.op(W.OP_I32_AND);
      f.i32Const(3); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_ROTR);
      f.localSet(destLocal);
    };

    // Store a halfword with ARM semantics, matching the interpreter:
    // write16(addr & ~1, value). The w16 hook masks the value to 16
    // bits, same as the interpreter's `& 0xFFFF`.
    const emitStoreHalfMasked = (addrLocal: number, valLocal: number) => {
      f.localGet(addrLocal);
      f.i32Const(~1); f.op(W.OP_I32_AND);
      f.localGet(valLocal);
      f.call(I_w16);
    };

    // setNZ(value-in-localIdx) — updates the CPSR's N+Z bits in place.
    // Equivalent to CpuState.setNZ.
    const emitSetNZ = (localIdx: number) => {
      f.i32Const(0);                          // addr for the final store
      // Load CPSR, mask out N+Z.
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(0x3FFFFFFF); f.op(W.OP_I32_AND);
      // OR in N: v & 0x80000000.
      f.localGet(localIdx);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      f.op(W.OP_I32_OR);
      // OR in Z: (v == 0) << 30.
      f.localGet(localIdx);
      f.op(W.OP_I32_EQZ);
      f.i32Const(30); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);
      // Store.
      f.i32Store(CPSR_OFF);
    };

    // setFlagsAdd(a, b, r) — N from r, Z from r, C from r<a (unsigned),
    // V from (~(a^b) & (a^r)) sign. Matches the JS reference exactly.
    const emitSetFlagsAdd = (lA: number, lB: number, lR: number) => {
      f.i32Const(0);                          // addr for store
      // (masked old CPSR) | N | Z | C | V
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(0x0FFFFFFF); f.op(W.OP_I32_AND);   // clear N+Z+C+V
      // N: r & 0x80000000
      f.localGet(lR);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      // Z: (r == 0) << 30
      f.localGet(lR); f.op(W.OP_I32_EQZ);
      f.i32Const(30); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);                            // N|Z
      // C: (r < a) unsigned, << 29
      f.localGet(lR); f.localGet(lA); f.op(W.OP_I32_LT_U);
      f.i32Const(29); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);                            // N|Z|C
      // V: (~(a^b) & (a^r)) sign bit moved to bit 28
      f.localGet(lA); f.localGet(lB); f.op(W.OP_I32_XOR);
      f.i32Const(-1); f.op(W.OP_I32_XOR);            // ~(a^b)
      f.localGet(lA); f.localGet(lR); f.op(W.OP_I32_XOR); // (a^r)
      f.op(W.OP_I32_AND);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      f.i32Const(3); f.op(W.OP_I32_SHR_U);           // → bit 28
      f.op(W.OP_I32_OR);                            // N|Z|C|V
      f.op(W.OP_I32_OR);                            // | masked CPSR
      f.i32Store(CPSR_OFF);
    };

    const emitSetFlagsSub = (lA: number, lB: number, lR: number) => {
      f.i32Const(0);
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(0x0FFFFFFF); f.op(W.OP_I32_AND);
      // N
      f.localGet(lR);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      // Z
      f.localGet(lR); f.op(W.OP_I32_EQZ);
      f.i32Const(30); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);
      // C: (a >= b) unsigned
      f.localGet(lA); f.localGet(lB); f.op(W.OP_I32_GE_U);
      f.i32Const(29); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);
      // V: ((a^b) & (a^r)) sign → bit 28
      f.localGet(lA); f.localGet(lB); f.op(W.OP_I32_XOR);
      f.localGet(lA); f.localGet(lR); f.op(W.OP_I32_XOR);
      f.op(W.OP_I32_AND);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      f.i32Const(3); f.op(W.OP_I32_SHR_U);
      f.op(W.OP_I32_OR);
      f.op(W.OP_I32_OR);
      f.i32Store(CPSR_OFF);
    };

    // Like emitSetFlagsAdd/Sub but the C flag comes pre-computed (0/1)
    // in local `lC` — used by ADC/SBC whose carry isn't a simple
    // unsigned compare of a/b/r. `subForm` selects the V formula:
    //   add form: (~(a^b) & (a^r))    sub form: ((a^b) & (a^r))
    const emitSetFlagsCarryLocal = (lA: number, lB: number, lR: number, lC: number, subForm: boolean) => {
      f.i32Const(0);                          // addr for store
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(0x0FFFFFFF); f.op(W.OP_I32_AND);   // clear N+Z+C+V
      // N: r & 0x80000000
      f.localGet(lR);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      // Z: (r == 0) << 30
      f.localGet(lR); f.op(W.OP_I32_EQZ);
      f.i32Const(30); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);                            // N|Z
      // C: precomputed 0/1 in lC, << 29
      f.localGet(lC);
      f.i32Const(29); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);                            // N|Z|C
      // V: ((~)?(a^b) & (a^r)) sign bit → bit 28
      f.localGet(lA); f.localGet(lB); f.op(W.OP_I32_XOR);
      if (!subForm) { f.i32Const(-1); f.op(W.OP_I32_XOR); }  // ~(a^b)
      f.localGet(lA); f.localGet(lR); f.op(W.OP_I32_XOR);
      f.op(W.OP_I32_AND);
      f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
      f.i32Const(3); f.op(W.OP_I32_SHR_U);
      f.op(W.OP_I32_OR);                            // N|Z|C|V
      f.op(W.OP_I32_OR);                            // | masked CPSR
      f.i32Store(CPSR_OFF);
    };

    // Set the CPSR C flag from a 0/1 value in local `localIdx` —
    // mirrors shifter.ts applyCarry (clear bit 29, OR in carry << 29).
    const emitSetC = (localIdx: number) => {
      f.i32Const(0);                          // addr for the final store
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(~0x20000000); f.op(W.OP_I32_AND);   // clear C
      f.localGet(localIdx);
      f.i32Const(29); f.op(W.OP_I32_SHL);
      f.op(W.OP_I32_OR);
      f.i32Store(CPSR_OFF);
    };

    // Format 4 register shift (LSL/LSR/ASR/ROR Rd, Rs). Expects the
    // value in L_A and the shift register in L_B; mirrors shifter.ts
    // regShift exactly. `shiftOp` is the SHIFT_* constant (0=LSL,
    // 1=LSR, 2=ASR, 3=ROR). amount = r[rs] & 0xFF; amount==0 leaves
    // both value and carry unchanged, but N/Z still update from the
    // (unchanged) value — exactly what the interpreter does.
    const emitRegShift = (shiftOp: number, rd: number) => {
      // amount = b & 0xFF → L_TMP
      f.localGet(L_B); f.i32Const(0xFF); f.op(W.OP_I32_AND); f.localSet(L_TMP);
      f.localGet(L_TMP);
      f.op(W.OP_IF); f.body.push(0x40);       // amount != 0
      if (shiftOp === 0) {                    // LSL
        f.localGet(L_TMP); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        // amount in 1..31 — native wasm shifts are exact here.
        f.localGet(L_A); f.localGet(L_TMP); f.op(W.OP_I32_SHL); f.localSet(L_R);
        // carry = (a >>> (32 - amount)) & 1
        f.localGet(L_A);
        f.i32Const(32); f.localGet(L_TMP); f.op(W.OP_I32_SUB);
        f.op(W.OP_I32_SHR_U);
        f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_R);
        f.localGet(L_TMP); f.i32Const(32); f.op(W.OP_I32_EQ);
        f.op(W.OP_IF); f.body.push(0x40);     // ==32: carry = a & 1
        f.localGet(L_A); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);                      // >32: carry = 0
        f.i32Const(0); f.localSet(L_C);
        f.op(W.OP_END);
        f.op(W.OP_END);
      } else if (shiftOp === 1) {             // LSR
        f.localGet(L_TMP); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_A); f.localGet(L_TMP); f.op(W.OP_I32_SHR_U); f.localSet(L_R);
        // carry = (a >>> (amount - 1)) & 1
        f.localGet(L_A);
        f.localGet(L_TMP); f.i32Const(1); f.op(W.OP_I32_SUB);
        f.op(W.OP_I32_SHR_U);
        f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_R);
        f.localGet(L_TMP); f.i32Const(32); f.op(W.OP_I32_EQ);
        f.op(W.OP_IF); f.body.push(0x40);     // ==32: carry = (a >>> 31) & 1
        f.localGet(L_A); f.i32Const(31); f.op(W.OP_I32_SHR_U); f.localSet(L_C);
        f.op(W.OP_ELSE);                      // >32: carry = 0
        f.i32Const(0); f.localSet(L_C);
        f.op(W.OP_END);
        f.op(W.OP_END);
      } else if (shiftOp === 2) {             // ASR
        f.localGet(L_TMP); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_A); f.localGet(L_TMP); f.op(W.OP_I32_SHR_S); f.localSet(L_R);
        // carry = ((a|0) >> (amount - 1)) & 1
        f.localGet(L_A);
        f.localGet(L_TMP); f.i32Const(1); f.op(W.OP_I32_SUB);
        f.op(W.OP_I32_SHR_S);
        f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        // ≥32: value = all sign bits, carry = sign.
        f.localGet(L_A); f.i32Const(31); f.op(W.OP_I32_SHR_S); f.localSet(L_R);
        f.localGet(L_A); f.i32Const(31); f.op(W.OP_I32_SHR_U); f.localSet(L_C);
        f.op(W.OP_END);
      } else {                                // ROR — branch-free.
        // regShift: a31 = amount & 31; a31==0 (nonzero multiple of 32)
        // → value unchanged, carry = bit 31; else rotate by a31, carry
        // = bit (a31-1). wasm i32.rotr masks the amount to 5 bits, so
        // rotr(a, amount) covers both: rotr by 0 is the identity.
        // Carry: (a >>> ((amount-1) & 31)) & 1 — for a31==0,
        // (amount-1)&31 == 31 → bit 31; for a31>0 it's a31-1. shr_u
        // masks natively, so no explicit & 31 is needed.
        f.localGet(L_A); f.localGet(L_TMP); f.op(W.OP_I32_ROTR); f.localSet(L_R);
        f.localGet(L_A);
        f.localGet(L_TMP); f.i32Const(1); f.op(W.OP_I32_SUB);
        f.op(W.OP_I32_SHR_U);
        f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
      }
      storeRegFromLocal(rd, L_R);
      emitSetC(L_C);
      f.op(W.OP_ELSE);
      // amount == 0: value and carry both unchanged; N/Z still update.
      f.localGet(L_A); f.localSet(L_R);
      f.op(W.OP_END);
      emitSetNZ(L_R);
    };

    // Push CPSR's C bit (0/1) onto the stack.
    const pushCarryIn = () => {
      f.i32Const(0); f.i32Load(CPSR_OFF);
      f.i32Const(29); f.op(W.OP_I32_SHR_U);
      f.i32Const(1); f.op(W.OP_I32_AND);
    };

    // Push 0 or 1 (the condition result) onto the stack. `cond` is
    // known at compile time so we hardcode the bit-test pattern.
    const emitCheckCond = (cond: number) => {
      if (cond === 0xE) { f.i32Const(1); return; }
      if (cond === 0xF) { f.i32Const(0); return; }
      // Push bit `bit` of CPSR onto the stack (as 0/1).
      const pushBit = (bit: number) => {
        f.i32Const(0); f.i32Load(CPSR_OFF);
        f.i32Const(bit); f.op(W.OP_I32_SHR_U);
        f.i32Const(1); f.op(W.OP_I32_AND);
      };
      switch (cond) {
        case 0x0: pushBit(30); return;                                              // EQ: z
        case 0x1: pushBit(30); f.op(W.OP_I32_EQZ); return;                          // NE: !z
        case 0x2: pushBit(29); return;                                              // CS: c
        case 0x3: pushBit(29); f.op(W.OP_I32_EQZ); return;                          // CC: !c
        case 0x4: pushBit(31); return;                                              // MI: n
        case 0x5: pushBit(31); f.op(W.OP_I32_EQZ); return;                          // PL: !n
        case 0x6: pushBit(28); return;                                              // VS: v
        case 0x7: pushBit(28); f.op(W.OP_I32_EQZ); return;                          // VC: !v
        case 0x8: pushBit(29); pushBit(30); f.op(W.OP_I32_EQZ); f.op(W.OP_I32_AND); return; // HI: c && !z
        case 0x9: pushBit(29); f.op(W.OP_I32_EQZ); pushBit(30); f.op(W.OP_I32_OR); return;  // LS: !c || z
        case 0xA: pushBit(31); pushBit(28); f.op(W.OP_I32_EQ); return;              // GE: n==v
        case 0xB: pushBit(31); pushBit(28); f.op(W.OP_I32_NE); return;              // LT: n!=v
        case 0xC: pushBit(30); f.op(W.OP_I32_EQZ);                                  // GT: !z && n==v
                  pushBit(31); pushBit(28); f.op(W.OP_I32_EQ);
                  f.op(W.OP_I32_AND); return;
        case 0xD: pushBit(30);                                                       // LE: z || n!=v
                  pushBit(31); pushBit(28); f.op(W.OP_I32_NE);
                  f.op(W.OP_I32_OR); return;
      }
      // Unhandled — emit 0 (will never branch).
      f.i32Const(0);
    };

    let pc = startPc;
    let count = 0;
    let needsExitPc = true;

    const translate = (insn: number): { ok: true; endsBlock: boolean } | { ok: false } => {
      const top3 = insn >>> 13;

      // -------- Format 2: ADD/SUB Rd, Rs, Rn / #imm3
      // top3 == 0b000 with bits 12:11 == 11 (Format 1's shift ops live
      // in the other three op slots).
      if ((insn & 0xF800) === 0x1800) {
        const I    = (insn & 0x0400) !== 0;
        const sub  = (insn & 0x0200) !== 0;
        const rnRm = (insn >>> 6) & 7;
        const rs   = (insn >>> 3) & 7;
        const rd   = insn & 7;
        pushReg(rs); f.localSet(L_A);
        if (I) f.i32Const(rnRm); else pushReg(rnRm);
        f.localSet(L_B);
        f.localGet(L_A); f.localGet(L_B);
        f.op(sub ? W.OP_I32_SUB : W.OP_I32_ADD); f.localSet(L_R);
        storeRegFromLocal(rd, L_R);
        if (sub) emitSetFlagsSub(L_A, L_B, L_R);
        else     emitSetFlagsAdd(L_A, L_B, L_R);
        return { ok: true, endsBlock: false };
      }

      // -------- Format 1: LSL/LSR/ASR Rd, Rs, #imm5
      // top3 == 0b000 with op != 3 — Format 2 (op == 3) matched above,
      // so anything still here is a shift. The shift amount is a
      // compile-time constant, so every immShift edge case resolves
      // here and the emitted code is straight-line.
      if (top3 === 0b000) {
        const op  = (insn >>> 11) & 3;        // 0=LSL, 1=LSR, 2=ASR
        const imm = (insn >>> 6) & 0x1F;
        const rs  = (insn >>> 3) & 7;
        const rd  = insn & 7;
        pushReg(rs); f.localSet(L_A);
        if (op === 0 && imm === 0) {
          // LSL #0: value unchanged, carry unchanged — do NOT touch C.
          f.localGet(L_A); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          emitSetNZ(L_R);
          return { ok: true, endsBlock: false };
        }
        // Carry from the ORIGINAL value → L_C. Per immShift:
        //   LSL #n: (v >>> (32-n)) & 1
        //   LSR #0 (=#32): (v >>> 31) & 1     LSR #n: (v >>> (n-1)) & 1
        //   ASR #0 (=#32): (v >>> 31) & 1     ASR #n: ((v|0) >> (n-1)) & 1
        f.localGet(L_A);
        if (op === 0) {
          f.i32Const(32 - imm); f.op(W.OP_I32_SHR_U);
        } else if (op === 1) {
          f.i32Const(imm === 0 ? 31 : imm - 1); f.op(W.OP_I32_SHR_U);
        } else if (imm === 0) {
          f.i32Const(31); f.op(W.OP_I32_SHR_U);
        } else {
          f.i32Const(imm - 1); f.op(W.OP_I32_SHR_S);
        }
        f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        // Value → L_R. LSR #0 means #32 (value 0); ASR #0 means #32
        // (all sign bits, same as >> 31).
        if (op === 0) {
          f.localGet(L_A); f.i32Const(imm); f.op(W.OP_I32_SHL);
        } else if (op === 1) {
          if (imm === 0) f.i32Const(0);
          else { f.localGet(L_A); f.i32Const(imm); f.op(W.OP_I32_SHR_U); }
        } else {
          f.localGet(L_A); f.i32Const(imm === 0 ? 31 : imm); f.op(W.OP_I32_SHR_S);
        }
        f.localSet(L_R);
        storeRegFromLocal(rd, L_R);
        emitSetNZ(L_R);
        emitSetC(L_C);
        return { ok: true, endsBlock: false };
      }

      // -------- Format 3: MOV/CMP/ADD/SUB Rd, #imm8
      if (top3 === 0b001) {
        const op = (insn >>> 11) & 3;
        const rd = (insn >>> 8) & 7;
        const imm = insn & 0xFF;
        if (op === 0) {
          // MOV Rd, #imm: setR(rd, imm); setNZ(imm)
          storeRegConst(rd, imm);
          f.i32Const(imm); f.localSet(L_R);
          emitSetNZ(L_R);
        } else if (op === 1) {
          // CMP Rd, #imm: a = r[rd]; r = a - imm; flagsSub(a, imm, r)
          pushReg(rd); f.localSet(L_A);
          f.i32Const(imm); f.localSet(L_B);
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
          emitSetFlagsSub(L_A, L_B, L_R);
        } else if (op === 2) {
          // ADD Rd, #imm: r = a + imm; setR(rd, r); flagsAdd(a, imm, r)
          pushReg(rd); f.localSet(L_A);
          f.i32Const(imm); f.localSet(L_B);
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          emitSetFlagsAdd(L_A, L_B, L_R);
        } else {
          // SUB Rd, #imm
          pushReg(rd); f.localSet(L_A);
          f.i32Const(imm); f.localSet(L_B);
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          emitSetFlagsSub(L_A, L_B, L_R);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 4: ALU register ops
      if ((insn & 0xFC00) === 0x4000) {
        const aluOp = (insn >>> 6) & 0xF;
        const rs = (insn >>> 3) & 7;
        const rd = insn & 7;
        pushReg(rd); f.localSet(L_A);
        pushReg(rs); f.localSet(L_B);
        switch (aluOp) {
          case 0x0: // AND
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_AND); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0x1: // EOR
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_XOR); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0x2: // LSL Rd, Rs (by register)
            emitRegShift(0, rd);
            return { ok: true, endsBlock: false };
          case 0x3: // LSR Rd, Rs
            emitRegShift(1, rd);
            return { ok: true, endsBlock: false };
          case 0x4: // ASR Rd, Rs
            emitRegShift(2, rd);
            return { ok: true, endsBlock: false };
          case 0x7: // ROR Rd, Rs
            emitRegShift(3, rd);
            return { ok: true, endsBlock: false };
          case 0x5: // ADC: r = a + b + cIn
            pushCarryIn(); f.localSet(L_TMP2);
            // t = a + b
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
            // r = t + cIn
            f.localGet(L_TMP); f.localGet(L_TMP2); f.op(W.OP_I32_ADD); f.localSet(L_R);
            // C = carry out of the 33-bit sum: (t <u a) | (r <u t)
            f.localGet(L_TMP); f.localGet(L_A); f.op(W.OP_I32_LT_U);
            f.localGet(L_R); f.localGet(L_TMP); f.op(W.OP_I32_LT_U);
            f.op(W.OP_I32_OR); f.localSet(L_TMP2);
            storeRegFromLocal(rd, L_R);
            emitSetFlagsCarryLocal(L_A, L_B, L_R, L_TMP2, false);
            return { ok: true, endsBlock: false };
          case 0x6: // SBC: r = a - b - (cIn ^ 1)
            pushCarryIn(); f.localSet(L_TMP2);
            // C = (a >u b) | ((a == b) & cIn) — branch-free form of the
            // interpreter's `a >= b + notC` (exact arithmetic).
            // Computed before r so cIn (L_TMP2) is still live.
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_GT_U);
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_EQ);
            f.localGet(L_TMP2); f.op(W.OP_I32_AND);
            f.op(W.OP_I32_OR); f.localSet(L_TMP);
            // r = a - b + cIn - 1
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB);
            f.localGet(L_TMP2); f.op(W.OP_I32_ADD);
            f.i32Const(1); f.op(W.OP_I32_SUB);
            f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetFlagsCarryLocal(L_A, L_B, L_R, L_TMP, true);
            return { ok: true, endsBlock: false };
          case 0x8: // TST: AND, flags only
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_AND); f.localSet(L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0x9: // NEG: r = 0 - b
            f.i32Const(0); f.localSet(L_A);
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetFlagsSub(L_A, L_B, L_R);
            return { ok: true, endsBlock: false };
          case 0xA: // CMP
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
            emitSetFlagsSub(L_A, L_B, L_R);
            return { ok: true, endsBlock: false };
          case 0xB: // CMN: add, flags only
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_R);
            emitSetFlagsAdd(L_A, L_B, L_R);
            return { ok: true, endsBlock: false };
          case 0xD: // MUL — interpreter only touches N+Z, so setNZ only
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_MUL); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0xC: // ORR
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_OR); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0xE: // BIC: a & ~b
            f.localGet(L_A);
            f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR);
            f.op(W.OP_I32_AND); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
          case 0xF: // MVN: ~b
            f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
            emitSetNZ(L_R);
            return { ok: true, endsBlock: false };
        }
        return { ok: false };
      }

      // -------- Format 6: LDR Rd, [PC, #imm8*4]
      // 0x4800/0xF800 shares the `010` top-bits region with Format 4
      // (0x4000/0xFC00, checked above) and Format 5 (0x4400/0xFC00,
      // intentionally unimplemented) — neither range matches this mask,
      // so there's no shadowing either way.
      if ((insn & 0xF800) === 0x4800) {
        const rd  = (insn >>> 8) & 7;
        const imm = (insn & 0xFF) << 2;
        // addr = (arch PC & ~3) + imm. Arch PC = pc + 4 is a compile-
        // time constant, so the whole address folds to a constant.
        // Word-aligned by construction → plain read32, no rotation.
        const addr = (((pc + 4) & ~3) + imm) >>> 0;
        f.i32Const(0);                          // reg-store base addr
        f.i32Const(addr | 0);
        f.call(I_r32);
        f.i32Store(REG_BASE + rd * 4);
        return { ok: true, endsBlock: false };
      }

      // -------- Formats 7+8: load/store with register offset
      if ((insn & 0xF000) === 0x5000) {
        const ro = (insn >>> 6) & 7;
        const rb = (insn >>> 3) & 7;
        const rd = insn & 7;
        // addr = r[rb] + r[ro]
        pushReg(rb); pushReg(ro); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
        if ((insn & 0x0200) === 0) {
          // Format 7: LDR/STR Rd, [Rb, Ro] with byte/word toggle.
          const L = (insn & 0x0800) !== 0;
          const B = (insn & 0x0400) !== 0;
          if (L) {
            if (B) { f.localGet(L_TMP); f.call(I_r8); f.localSet(L_R); }
            else   { emitLoadWordRotated(L_TMP, L_R); }
            storeRegFromLocal(rd, L_R);
          } else {
            pushReg(rd); f.localSet(L_R);
            // The w8 hook masks the value to 8 bits, matching the
            // interpreter's `s.r[rd] & 0xFF`.
            if (B) { f.localGet(L_TMP); f.localGet(L_R); f.call(I_w8); }
            else   { emitStoreWordMasked(L_TMP, L_R); }
          }
        } else {
          // Format 8: STRH/LDSB/LDRH/LDSH via the H + S bits.
          const H = (insn & 0x0800) !== 0;
          const S = (insn & 0x0400) !== 0;
          if (!H && !S) {
            // STRH: write16(addr & ~1, r[rd]) — hook masks the value.
            pushReg(rd); f.localSet(L_R);
            emitStoreHalfMasked(L_TMP, L_R);
          } else if (!H && S) {
            // LDSB: read8(addr), sign-extend 8→32.
            f.localGet(L_TMP); f.call(I_r8);
            f.i32Const(24); f.op(W.OP_I32_SHL);
            f.i32Const(24); f.op(W.OP_I32_SHR_S);
            f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
          } else if (H && !S) {
            // LDRH (unaligned rotated, never sign-extended).
            emitLoadHalfRotated(L_TMP, L_R);
            storeRegFromLocal(rd, L_R);
          } else {
            // LDSH — ARM7TDMI quirk: an odd address reads the BYTE at
            // addr sign-extended 8→32; an even address reads the half-
            // word sign-extended 16→32. Runtime branch on addr & 1.
            f.localGet(L_TMP); f.i32Const(1); f.op(W.OP_I32_AND);
            f.op(W.OP_IF); f.body.push(0x40);
            f.localGet(L_TMP); f.call(I_r8);
            f.i32Const(24); f.op(W.OP_I32_SHL);
            f.i32Const(24); f.op(W.OP_I32_SHR_S);
            f.localSet(L_R);
            f.op(W.OP_ELSE);
            // addr is even here, so no & ~1 needed (interpreter passes
            // the raw address to read16 too).
            f.localGet(L_TMP); f.call(I_r16);
            f.i32Const(16); f.op(W.OP_I32_SHL);
            f.i32Const(16); f.op(W.OP_I32_SHR_S);
            f.localSet(L_R);
            f.op(W.OP_END);
            storeRegFromLocal(rd, L_R);
          }
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 9: LDR/STR Rd, [Rb, #imm5*4 / *1] (word or byte)
      if ((insn & 0xE000) === 0x6000) {
        const isByte = (insn & 0x1000) !== 0;
        const isLoad = (insn & 0x0800) !== 0;
        const off5   = (insn >>> 6) & 0x1F;
        const rb     = (insn >>> 3) & 7;
        const rd     = insn & 7;
        const offset = isByte ? off5 : off5 << 2;
        // addr = r[rb] + offset
        pushReg(rb); f.i32Const(offset); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
        if (isLoad) {
          if (isByte) {
            f.localGet(L_TMP); f.call(I_r8); f.localSet(L_R);
          } else {
            emitLoadWordRotated(L_TMP, L_R);
          }
          storeRegFromLocal(rd, L_R);
        } else {
          pushReg(rd); f.localSet(L_R);
          if (isByte) {
            f.localGet(L_TMP); f.localGet(L_R); f.call(I_w8);
          } else {
            emitStoreWordMasked(L_TMP, L_R);
          }
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 10: STRH/LDRH Rd, [Rb, #imm5*2]
      if ((insn & 0xF000) === 0x8000) {
        const isLoad = (insn & 0x0800) !== 0;
        const imm    = ((insn >>> 6) & 0x1F) << 1;
        const rb     = (insn >>> 3) & 7;
        const rd     = insn & 7;
        // addr = r[rb] + imm
        pushReg(rb); f.i32Const(imm); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
        if (isLoad) {
          emitLoadHalfRotated(L_TMP, L_R);
          storeRegFromLocal(rd, L_R);
        } else {
          pushReg(rd); f.localSet(L_R);
          emitStoreHalfMasked(L_TMP, L_R);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 11: LDR/STR Rd, [SP, #imm8*4]
      if ((insn & 0xF000) === 0x9000) {
        const isLoad = (insn & 0x0800) !== 0;
        const rd     = (insn >>> 8) & 7;
        const imm    = (insn & 0xFF) << 2;
        // addr = r13 + imm
        pushReg(13); f.i32Const(imm); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
        if (isLoad) {
          emitLoadWordRotated(L_TMP, L_R);
          storeRegFromLocal(rd, L_R);
        } else {
          pushReg(rd); f.localSet(L_R);
          emitStoreWordMasked(L_TMP, L_R);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 12: load address (ADD Rd, PC/SP, #imm8*4)
      if ((insn & 0xF000) === 0xA000) {
        const SP  = (insn & 0x0800) !== 0;
        const rd  = (insn >>> 8) & 7;
        const imm = (insn & 0xFF) << 2;
        if (SP) {
          // r[rd] = r13 + imm. No flags.
          pushReg(13); f.i32Const(imm); f.op(W.OP_I32_ADD); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
        } else {
          // r[rd] = (arch PC & ~3) + imm — pure compile-time constant
          // (arch PC = pc + 4).
          storeRegConst(rd, (((pc + 4) & ~3) + imm) >>> 0);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 13: ADD SP, #±imm7*4
      // Exact-match 0xB0xx only — 0xB4/0xB5/0xBC/0xBD are Format 14
      // PUSH/POP, which stay unsupported here.
      if ((insn & 0xFF00) === 0xB000) {
        const imm = (insn & 0x7F) << 2;
        pushReg(13); f.i32Const(imm);
        f.op((insn & 0x80) ? W.OP_I32_SUB : W.OP_I32_ADD);
        f.localSet(L_R);
        storeRegFromLocal(13, L_R);
        return { ok: true, endsBlock: false };
      }

      // -------- Format 16: B<cond> label
      if ((insn & 0xF000) === 0xD000) {
        const cond = (insn >>> 8) & 0xF;
        if (cond === 0xE || cond === 0xF) return { ok: false };
        let off = insn & 0xFF;
        if (off & 0x80) off -= 0x100;
        const taken    = (pc + 4 + (off << 1)) >>> 0;
        const fallthru = (pc + 2) >>> 0;
        emitCheckCond(cond);
        f.op(W.OP_IF); f.body.push(0x40);
        storeRegConst(15, (taken & ~1) >>> 0);
        f.op(W.OP_ELSE);
        storeRegConst(15, (fallthru & ~1) >>> 0);
        f.op(W.OP_END);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      // -------- Format 18: B label
      if ((insn & 0xF800) === 0xE000) {
        let off = insn & 0x7FF;
        if (off & 0x400) off -= 0x800;
        const target = (pc + 4 + (off << 1)) >>> 0;
        storeRegConst(15, (target & ~1) >>> 0);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      return { ok: false };
    };

    for (; count < MAX_BLOCK_INSNS; ) {
      const insn = bus.read16(pc);
      const res = translate(insn);
      if (!res.ok) break;
      pc = (pc + 2) >>> 0;
      count++;
      if (res.endsBlock) break;
    }

    if (count === 0) return null;

    if (needsExitPc) {
      storeRegConst(15, pc >>> 0);
    }
    f.i32Const(count);

    let module: WebAssembly.Module;
    try {
      const bytes = builder.encode();
      const arr = new Uint8Array(new ArrayBuffer(bytes.length));
      arr.set(bytes);
      module = new WebAssembly.Module(arr);
    } catch (e) {
      return null;
    }
    let instance: WebAssembly.Instance;
    try {
      instance = new WebAssembly.Instance(module, this.importsObj);
    } catch {
      return null;
    }
    const exported = instance.exports.run as (pc: number) => number;
    return {
      startPc,
      insnCount: count,
      run: () => exported(0),
    };
  }
}
