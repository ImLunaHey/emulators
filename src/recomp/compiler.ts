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
  // CPU mode the block was compiled for. A given PC almost always holds
  // code of one kind, but we verify on dispatch so a THUMB block never
  // runs in ARM mode (or vice-versa) after an unusual remap.
  thumb: boolean;
  // For blocks compiled from writable memory (EWRAM/IWRAM): a live
  // Uint16Array view onto the backing store plus a snapshot of the
  // halfwords at compile time. Verified on every dispatch — games
  // routinely copy fresh code into RAM (Pokemon's flash routines land
  // on the IWRAM stack), and running a stale block silently executes
  // the OLD instructions. ROM blocks set both to null (immutable).
  liveCode: Uint16Array | null;
  snapshot: Uint16Array | null;
  // Register-sync masks (bit i = r[i]). Only the registers a block
  // actually reads need copying IN, and only those it writes need
  // copying OUT — typical blocks touch a handful, so syncing the live
  // set instead of all 16 every dispatch cuts the per-block overhead.
  inMask: number;
  outMask: number;
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
  // Enabled by default. The JIT covers every THUMB format (1-16, 18,
  // 19) plus ARM data-processing (any condition, any operand2 form
  // incl. register-specified shifts and R15 operands, rd=PC ALU
  // branches), ARM branches (B/BL/BX), ARM loads/stores (LDR/STR
  // byte+word, LDRH/STRH/LDRSB/LDRSH), block transfer (LDM/STM), and
  // multiply (MUL/MLA + long UMULL/SMULL/UMLAL/SMLAL). SWI,
  // empty-register-list LDM/STM, MRS/MSR, and SWP fall back to the
  // interpreter. Set false to force pure interpretation.
  enabled = true;

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
    // The interpreter's step() handles halt wake-up and IRQ entry
    // BEFORE executing an instruction. A JIT block must not run in
    // either situation — otherwise a fully-jitted wait loop (e.g.
    // Emerald's `ldrb r0, [VCOUNT]; cmp r0, #n; beq .-4`) starves IRQ
    // delivery forever and the game soft-locks on a black screen.
    if (s.halted) return 0;
    if (this.cpu.irqLine && !(s.cpsr & 0x80)) return 0;
    const isThumb = (s.cpsr & 0x20) !== 0;
    const pc = s.r[15] & (isThumb ? ~1 : ~3);
    const cached = this.cache.get(pc);
    if (cached === null) return 0;              // known-uncompilable
    if (cached) {
      // The same PC could in principle be entered in the other CPU mode
      // after a remap — never run a mismatched block; drop + recompile.
      if (cached.thumb !== isThumb) { this.cache.delete(pc); return 0; }
      // RAM blocks: verify the code bytes haven't changed since
      // compilation. On mismatch drop the block and fall back to the
      // interpreter; the hit counter will recompile it if the new
      // code turns out to be hot too.
      const snap = cached.snapshot;
      if (snap) {
        const live = cached.liveCode!;
        for (let i = 0; i < snap.length; i++) {
          if (live[i] !== snap[i]) { this.cache.delete(pc); return 0; }
        }
      }
      return this.runBlock(cached);
    }
    const c = (this.hits.get(pc) || 0) + 1;
    this.hits.set(pc, c);
    if (c < HOT_THRESHOLD) return 0;
    this.hits.delete(pc);
    const block = this.compile(pc, isThumb);
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
    const r = s.r;
    // In: only the registers this block reads (plus PC), and cpsr.
    // Iterate just the set bits via the classic x & -x lowest-bit trick.
    let im = block.inMask;
    while (im !== 0) { const i = 31 - Math.clz32(im & -im); mem[i] = r[i]; im &= im - 1; }
    mem[CPSR_OFF >> 2] = s.cpsr >>> 0;
    const n = block.run();
    // Out: only the registers this block writes (plus PC), and cpsr.
    let om = block.outMask;
    while (om !== 0) { const i = 31 - Math.clz32(om & -om); r[i] = mem[i]; om &= om - 1; }
    s.cpsr = mem[CPSR_OFF >> 2] | 0;
    this.jitInsns += n;
    return n;
  }

  private compile(startPc: number, thumb: boolean): CompiledBlock | null {
    const bus = this.cpu.bus;
    // Only compile from EWRAM (0x02), IWRAM (0x03) or cart ROM
    // (0x08..0x0D). Other regions either can't sensibly hold THUMB
    // code (IO/PRAM/OAM) or have mirroring quirks we don't want to
    // re-validate (VRAM, BIOS).
    const region = startPc >>> 24;
    const inRam = region === 0x02 || region === 0x03;
    if (!inRam && !(region >= 0x08 && region <= 0x0D)) return null;
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
    f.addLocals(8, W.I32);
    const L_A = 1, L_B = 2, L_R = 3, L_TMP = 4, L_TMP2 = 5, L_C = 6;
    // L_X / L_Y: scratch for the ARM register-specified-shift operand
    // (value + amount) so the DP path can still use L_A for op1.
    const L_X = 7, L_Y = 8;
    // One i64 scratch for the ARM long-multiply product (index 9, after
    // the eight i32 locals). Unused by every other path.
    f.addLocals(1, W.I64);
    const L_I64 = 9;

    // Live register sets accumulated as instructions are emitted. Reads
    // all funnel through pushReg(); writes through the store helpers
    // plus a handful of direct-store sites that mark themselves.
    let readMask = 0;
    let writeMask = 0;
    // Registers written only inside a runtime condition (ARM conditional
    // execution). If the condition is false the store is skipped, so the
    // register's pre-value must be synced IN — otherwise the block copies
    // out stale shared-memory data. Folded into inMask at the end.
    let condWrites = 0;

    // ----- emitter primitives. All assume linear memory base is at 0.

    // Push r[rd] onto the stack.
    const pushReg = (rd: number) => {
      readMask |= 1 << rd;
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
      writeMask |= 1 << rd;
      f.i32Const(0);
      f.localGet(localIdx);
      f.i32Store(REG_BASE + rd * 4);
    };

    // Store the i32 constant `val` into r[rd].
    const storeRegConst = (rd: number, val: number) => {
      writeMask |= 1 << rd;
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

    // ARM barrel shifter by IMMEDIATE amount (operand2 register form,
    // bit4=0). Fully constant-folded per (op, imm) since imm5 is known
    // at compile time. Pushes the shifted value; mirrors shifter.immShift.
    const emitImmShiftValue = (op: number, imm: number, src: number) => {
      if (op === 0) {                                   // LSL
        if (imm === 0) { f.localGet(src); }
        else { f.localGet(src); f.i32Const(imm); f.op(W.OP_I32_SHL); }
      } else if (op === 1) {                            // LSR (#0 == #32 → 0)
        if (imm === 0) { f.i32Const(0); }
        else { f.localGet(src); f.i32Const(imm); f.op(W.OP_I32_SHR_U); }
      } else if (op === 2) {                            // ASR (#0 == #32)
        f.localGet(src); f.i32Const(imm === 0 ? 31 : imm); f.op(W.OP_I32_SHR_S);
      } else {                                          // ROR (#0 == RRX)
        if (imm === 0) {
          pushCarryIn(); f.i32Const(31); f.op(W.OP_I32_SHL);
          f.localGet(src); f.i32Const(1); f.op(W.OP_I32_SHR_U);
          f.op(W.OP_I32_OR);
        } else { f.localGet(src); f.i32Const(imm); f.op(W.OP_I32_ROTR); }
      }
    };
    // Matching shifter carry-out (0/1). Caller must NOT invoke this for
    // LSL #0 (carry unchanged) — it handles every other (op, imm).
    const emitImmShiftCarry = (op: number, imm: number, src: number) => {
      let amt: number;
      if (op === 0) amt = 32 - imm;                     // LSL #imm>0
      else if (op === 3 && imm === 0) amt = 0;          // RRX → bit 0
      else amt = imm === 0 ? 31 : imm - 1;              // LSR/ASR/ROR
      f.localGet(src); f.i32Const(amt); f.op(W.OP_I32_SHR_U);
      f.i32Const(1); f.op(W.OP_I32_AND);
    };

    // ARM barrel shifter by REGISTER amount (operand2 register form,
    // bit4=1). Value must already be in L_X; reads the shift amount from
    // r[rsReg] & 0xFF. Produces value → L_B, carry → L_C (= carry-in when
    // amount is 0). Mirrors shifter.regShift exactly.
    const emitArmRegShift = (shiftOp: number, rsReg: number) => {
      pushReg(rsReg); f.i32Const(0xFF); f.op(W.OP_I32_AND); f.localSet(L_Y);   // amount
      pushCarryIn(); f.localSet(L_C);                                          // default carry
      f.localGet(L_X); f.localSet(L_B);                                        // default value (amount 0)
      f.localGet(L_Y);
      f.op(W.OP_IF); f.body.push(0x40);                                        // amount != 0
      if (shiftOp === 0) {                                                     // LSL
        f.localGet(L_Y); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_X); f.localGet(L_Y); f.op(W.OP_I32_SHL); f.localSet(L_B);
        f.localGet(L_X); f.i32Const(32); f.localGet(L_Y); f.op(W.OP_I32_SUB); f.op(W.OP_I32_SHR_U); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_B);
        f.localGet(L_Y); f.i32Const(32); f.op(W.OP_I32_EQ);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_X); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_C);
        f.op(W.OP_END);
        f.op(W.OP_END);
      } else if (shiftOp === 1) {                                             // LSR
        f.localGet(L_Y); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_X); f.localGet(L_Y); f.op(W.OP_I32_SHR_U); f.localSet(L_B);
        f.localGet(L_X); f.localGet(L_Y); f.i32Const(1); f.op(W.OP_I32_SUB); f.op(W.OP_I32_SHR_U); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_B);
        f.localGet(L_Y); f.i32Const(32); f.op(W.OP_I32_EQ);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_X); f.i32Const(31); f.op(W.OP_I32_SHR_U); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.i32Const(0); f.localSet(L_C);
        f.op(W.OP_END);
        f.op(W.OP_END);
      } else if (shiftOp === 2) {                                            // ASR
        f.localGet(L_Y); f.i32Const(32); f.op(W.OP_I32_LT_U);
        f.op(W.OP_IF); f.body.push(0x40);
        f.localGet(L_X); f.localGet(L_Y); f.op(W.OP_I32_SHR_S); f.localSet(L_B);
        f.localGet(L_X); f.localGet(L_Y); f.i32Const(1); f.op(W.OP_I32_SUB); f.op(W.OP_I32_SHR_S); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
        f.op(W.OP_ELSE);
        f.localGet(L_X); f.i32Const(31); f.op(W.OP_I32_SHR_S); f.localSet(L_B);
        f.localGet(L_X); f.i32Const(31); f.op(W.OP_I32_SHR_U); f.localSet(L_C);
        f.op(W.OP_END);
      } else {                                                               // ROR (branch-free)
        f.localGet(L_X); f.localGet(L_Y); f.op(W.OP_I32_ROTR); f.localSet(L_B);
        f.localGet(L_X); f.localGet(L_Y); f.i32Const(1); f.op(W.OP_I32_SUB); f.op(W.OP_I32_SHR_U); f.i32Const(1); f.op(W.OP_I32_AND); f.localSet(L_C);
      }
      f.op(W.OP_END);
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

      // -------- Format 5: hi-register ops / BX
      if ((insn & 0xFC00) === 0x4400) {
        const op = (insn >>> 8) & 3;
        const H1 = (insn & 0x80) !== 0;
        const H2 = (insn & 0x40) !== 0;
        const rs = ((insn >>> 3) & 7) | (H2 ? 8 : 0);
        const rd = (insn & 7) | (H1 ? 8 : 0);
        // PC in THUMB hi ops reads as the aligned architectural PC — a
        // compile-time constant. The interpreter masks bit 0; pc + 4 is
        // already even but mask anyway for byte-for-byte parity.
        const pcRead = ((pc + 4) & ~1) >>> 0;
        const loadA = () => { if (rd === 15) f.i32Const(pcRead | 0); else pushReg(rd); f.localSet(L_A); };
        const loadB = () => { if (rs === 15) f.i32Const(pcRead | 0); else pushReg(rs); f.localSet(L_B); };
        if (op === 0) {                       // ADD (no flags)
          loadA(); loadB();
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD);
          if (rd === 15) {
            // ADD PC, Rs: r15 = (a + b) & ~1 — block-ender.
            f.i32Const(~1); f.op(W.OP_I32_AND); f.localSet(L_R);
            storeRegFromLocal(15, L_R);
            needsExitPc = false;
            return { ok: true, endsBlock: true };
          }
          f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          return { ok: true, endsBlock: false };
        }
        if (op === 1) {                       // CMP (flags only)
          loadA(); loadB();
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
          emitSetFlagsSub(L_A, L_B, L_R);
          return { ok: true, endsBlock: false };
        }
        if (op === 2) {                       // MOV (no flags)
          if (rd === 15) {
            // MOV PC, Rs: r15 = b & ~1 — block-ender.
            if (rs === 15) {
              storeRegConst(15, pcRead);
            } else {
              pushReg(rs); f.i32Const(~1); f.op(W.OP_I32_AND); f.localSet(L_R);
              storeRegFromLocal(15, L_R);
            }
            needsExitPc = false;
            return { ok: true, endsBlock: true };
          }
          if (rs === 15) {
            storeRegConst(rd, pcRead);
          } else {
            pushReg(rs); f.localSet(L_R);
            storeRegFromLocal(rd, L_R);
          }
          return { ok: true, endsBlock: false };
        }
        // op == 3: BX Rs. H1 must be 0 in this encoding (H1 set is BLX
        // on later cores / undefined on the ARM7TDMI) — leave it to the
        // interpreter.
        if (H1) return { ok: false };
        loadB();
        f.localGet(L_B); f.i32Const(1); f.op(W.OP_I32_AND);
        f.op(W.OP_IF); f.body.push(0x40);
        // bit 0 set: stay THUMB (cpsr |= T, matching the interpreter's
        // redundant-but-exact OR), r15 = b & ~1.
        f.i32Const(0);
        f.i32Const(0); f.i32Load(CPSR_OFF);
        f.i32Const(0x20); f.op(W.OP_I32_OR);
        f.i32Store(CPSR_OFF);
        f.i32Const(0);
        f.localGet(L_B); f.i32Const(~1); f.op(W.OP_I32_AND);
        f.i32Store(REG_BASE + 15 * 4);
        f.op(W.OP_ELSE);
        // bit 0 clear: switch to ARM (cpsr &= ~T), r15 = b & ~3. The
        // dispatcher re-checks T before every dispatch, so the next
        // block safely falls back to the (ARM) interpreter.
        f.i32Const(0);
        f.i32Const(0); f.i32Load(CPSR_OFF);
        f.i32Const(~0x20); f.op(W.OP_I32_AND);
        f.i32Store(CPSR_OFF);
        f.i32Const(0);
        f.localGet(L_B); f.i32Const(~3); f.op(W.OP_I32_AND);
        f.i32Store(REG_BASE + 15 * 4);
        f.op(W.OP_END);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      // -------- Format 6: LDR Rd, [PC, #imm8*4]
      // 0x4800/0xF800 shares the `010` top-bits region with Format 4
      // (0x4000/0xFC00) and Format 5 (0x4400/0xFC00), both checked
      // above — neither range matches this mask, so there's no
      // shadowing either way.
      if ((insn & 0xF800) === 0x4800) {
        const rd  = (insn >>> 8) & 7;
        writeMask |= 1 << rd;                   // direct store at end of this case
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
      // PUSH/POP, handled below.
      if ((insn & 0xFF00) === 0xB000) {
        const imm = (insn & 0x7F) << 2;
        pushReg(13); f.i32Const(imm);
        f.op((insn & 0x80) ? W.OP_I32_SUB : W.OP_I32_ADD);
        f.localSet(L_R);
        storeRegFromLocal(13, L_R);
        return { ok: true, endsBlock: false };
      }

      // -------- Format 14: PUSH/POP
      // The register list is a compile-time constant, so the whole
      // transfer unrolls into straight-line bus calls. Addresses mask
      // & ~3 like the interpreter; since each slot is start + 4k, the
      // mask is applied once to the base and the +4k offsets folded in
      // as constants. r13 keeps its UNMASKED value, exactly like the
      // interpreter.
      if ((insn & 0xF600) === 0xB400) {
        const isPop = (insn & 0x0800) !== 0;
        const R = (insn & 0x0100) !== 0;
        const list = insn & 0xFF;
        let n = 0;
        for (let i = 0; i < 8; i++) if (list & (1 << i)) n++;
        if (isPop) {
          // POP writes the listed low regs, the SP writeback, and PC
          // when R is set — all via direct stores below.
          writeMask |= (list & 0xFF) | (1 << 13) | (R ? 1 << 15 : 0);
          // POP — read ascending from sp.
          pushReg(13); f.localSet(L_TMP);                       // unmasked sp
          f.localGet(L_TMP); f.i32Const(~3); f.op(W.OP_I32_AND); f.localSet(L_TMP2);
          let off = 0;
          for (let i = 0; i < 8; i++) {
            if (!(list & (1 << i))) continue;
            f.i32Const(0);
            f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
            f.call(I_r32);
            f.i32Store(REG_BASE + i * 4);
            off += 4;
          }
          if (R) {
            // POP {PC} — ARMv4T (GBA) does NOT interwork here: bit 0 of
            // the loaded value is masked off and T stays THUMB (the only
            // way to switch to ARM from THUMB is BX). ARMv5T+ flips T
            // from bit 0, which on the ARM7TDMI would let THUMB-internal
            // returns accidentally hop to ARM.
            f.i32Const(0);
            f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
            f.call(I_r32);
            f.i32Const(~1); f.op(W.OP_I32_AND);
            f.i32Store(REG_BASE + 15 * 4);
            off += 4;
          }
          // r13 = original sp + 4*count (unmasked), after the loads —
          // same order as the interpreter.
          f.i32Const(0);
          f.localGet(L_TMP); f.i32Const(off); f.op(W.OP_I32_ADD);
          f.i32Store(REG_BASE + 13 * 4);
          if (R) { needsExitPc = false; return { ok: true, endsBlock: true }; }
          return { ok: true, endsBlock: false };
        }
        // PUSH — pre-decrement to newSp, then write ascending (r14, if
        // requested, lands last/highest).
        const count = n + (R ? 1 : 0);
        pushReg(13); f.i32Const(count << 2); f.op(W.OP_I32_SUB); f.localSet(L_TMP);
        f.localGet(L_TMP); f.i32Const(~3); f.op(W.OP_I32_AND); f.localSet(L_TMP2);
        let off = 0;
        for (let i = 0; i < 8; i++) {
          if (!(list & (1 << i))) continue;
          f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
          pushReg(i);
          f.call(I_w32);
          off += 4;
        }
        if (R) {
          f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
          pushReg(14);
          f.call(I_w32);
        }
        storeRegFromLocal(13, L_TMP);
        return { ok: true, endsBlock: false };
      }

      // -------- Format 15: LDMIA/STMIA Rb!, {list}
      if ((insn & 0xF000) === 0xC000) {
        const isLoad = (insn & 0x0800) !== 0;
        const rb = (insn >>> 8) & 7;
        const list = insn & 0xFF;
        // Empty-list quirk loads/stores PC and bumps the base by 0x40 —
        // rare enough to leave to the interpreter.
        if (list === 0) return { ok: false };
        const baseInList = (list & (1 << rb)) !== 0;
        const baseFirst = baseInList && (list & ((1 << rb) - 1)) === 0;
        // LDM writes the loaded regs; STM writes none. Both write the
        // base back unless it's an LDM whose base is in the list.
        if (isLoad) writeMask |= list & 0xFF;
        if (!isLoad || !baseInList) writeMask |= 1 << rb;
        let count = 0;
        for (let i = 0; i < 8; i++) if (list & (1 << i)) count++;
        // startAddr (unmasked) → L_TMP; per-access mask & ~3 folds into
        // a masked base in L_TMP2 (slots are start + 4k).
        pushReg(rb); f.localSet(L_TMP);
        f.localGet(L_TMP); f.i32Const(~3); f.op(W.OP_I32_AND); f.localSet(L_TMP2);
        let off = 0;
        for (let i = 0; i < 8; i++) {
          if (!(list & (1 << i))) continue;
          if (isLoad) {
            f.i32Const(0);
            f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
            f.call(I_r32);
            f.i32Store(REG_BASE + i * 4);
          } else {
            f.localGet(L_TMP2); f.i32Const(off); f.op(W.OP_I32_ADD);
            if (i === rb && !baseFirst) {
              // STM with the base in the list but not FIRST stores the
              // written-back base (startAddr + count*4), not old r[rb].
              f.localGet(L_TMP); f.i32Const(count << 2); f.op(W.OP_I32_ADD);
            } else {
              pushReg(i);
            }
            f.call(I_w32);
          }
          off += 4;
        }
        // Writeback r[rb] = endAddr (unmasked) — always for STM; for
        // LDM only when rb is NOT in the list (the loaded value wins).
        if (!isLoad || !baseInList) {
          f.i32Const(0);
          f.localGet(L_TMP); f.i32Const(off); f.op(W.OP_I32_ADD);
          f.i32Store(REG_BASE + rb * 4);
        }
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

      // -------- Format 19: BL, two halfwords (H=0b00 is Format 18 above)
      if ((insn & 0xF800) === 0xF000) {
        // High half (H=0b10): r14 = archPC + signExtend23(off11 << 12) —
        // a pure compile-time constant. NOT a block-ender: the low half
        // usually follows in the same block and reads r14 back from
        // linear memory, which this store keeps current.
        let off = (insn & 0x7FF) << 12;
        if (off & 0x00400000) off |= 0xFF800000;
        storeRegConst(14, ((pc + 4) + off) >>> 0);
        return { ok: true, endsBlock: false };
      }
      if ((insn & 0xF800) === 0xF800) {
        // Low half (H=0b11): r15 = (r14 + (off11 << 1)) & ~1, THEN
        // r14 = (pc + 2) | 1. Order matters — the branch target comes
        // from the OLD r14, computed before r14 is overwritten.
        pushReg(14);
        f.i32Const((insn & 0x7FF) << 1); f.op(W.OP_I32_ADD);
        f.i32Const(~1); f.op(W.OP_I32_AND);
        f.localSet(L_R);
        storeRegFromLocal(15, L_R);
        storeRegConst(14, ((pc + 2) | 1) >>> 0);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      return { ok: false };
    };

    // ----- ARM translation. Mirrors the interpreter's decode priority
    // (arm.ts armExecute); anything not yet handled returns ok:false so
    // the interpreter takes that instruction. Phase B covers AL-condition
    // data-processing with an immediate or immediate-shifted register
    // operand (no R15 operand/dest, no register-specified shift — those
    // arrive in later phases).
    const translateArm = (insn: number): { ok: true; endsBlock: boolean } | { ok: false } => {
      const cond = (insn >>> 28) & 0xF;

      // -------- B / BL: 101L + signed 24-bit offset.
      if ((insn & 0x0E000000) === 0x0A000000) {
        // NV branch never taken — falls through like a NOP (the
        // interpreter returns early and step() advances PC).
        if (cond === 0xF) return { ok: true, endsBlock: false };
        let offset = (insn & 0x00FFFFFF) << 2;
        if (offset & 0x02000000) offset |= 0xFC000000;          // sign-extend bit25
        const link = (insn & 0x01000000) !== 0;
        const target = (pc + 8 + offset) >>> 0;                 // arch PC = decode + 8
        const fallthru = (pc + 4) >>> 0;
        if (cond === 0xE) {
          if (link) storeRegConst(14, (pc + 4) >>> 0);          // LR = next insn
          storeRegConst(15, target);
        } else {
          emitCheckCond(cond);
          f.op(W.OP_IF); f.body.push(0x40);
          if (link) storeRegConst(14, (pc + 4) >>> 0);
          storeRegConst(15, target);
          f.op(W.OP_ELSE);
          storeRegConst(15, fallthru);
          f.op(W.OP_END);
          // BL writes LR only when taken → preserve it on the else path.
          if (link) condWrites |= 1 << 14;
        }
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      // -------- BX Rn: branch + exchange (mode switch on bit0).
      if ((insn & 0x0FFFFFF0) === 0x012FFF10) {
        const rn = insn & 0xF;
        if (rn === 15) return { ok: false };                    // BX PC — rare edge (interp)
        if (cond === 0xF) return { ok: true, endsBlock: false };
        const conditional = cond !== 0xE;
        if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }
        pushReg(rn); f.localSet(L_B);
        f.localGet(L_B); f.i32Const(1); f.op(W.OP_I32_AND);
        f.op(W.OP_IF); f.body.push(0x40);
        // bit0 set: stay/switch to THUMB (cpsr |= T), r15 = b & ~1.
        f.i32Const(0); f.i32Const(0); f.i32Load(CPSR_OFF); f.i32Const(0x20); f.op(W.OP_I32_OR); f.i32Store(CPSR_OFF);
        f.i32Const(0); f.localGet(L_B); f.i32Const(~1); f.op(W.OP_I32_AND); f.i32Store(REG_BASE + 15 * 4);
        f.op(W.OP_ELSE);
        // bit0 clear: ARM (cpsr &= ~T), r15 = b & ~3.
        f.i32Const(0); f.i32Const(0); f.i32Load(CPSR_OFF); f.i32Const(~0x20); f.op(W.OP_I32_AND); f.i32Store(CPSR_OFF);
        f.i32Const(0); f.localGet(L_B); f.i32Const(~3); f.op(W.OP_I32_AND); f.i32Store(REG_BASE + 15 * 4);
        f.op(W.OP_END);
        if (conditional) {
          f.op(W.OP_ELSE);
          storeRegConst(15, (pc + 4) >>> 0);                    // not taken → next insn
          f.op(W.OP_END);
        }
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      if ((insn & 0x0F000000) === 0x0F000000) return { ok: false };      // SWI
      // -------- Block data transfer: LDM / STM.
      if ((insn & 0x0E000000) === 0x08000000) {
        if (cond === 0xF) return { ok: true, endsBlock: false };          // NV → NOP
        const P = (insn & 0x01000000) !== 0;
        const U = (insn & 0x00800000) !== 0;
        const Sbit = (insn & 0x00400000) !== 0;
        const Wb = (insn & 0x00200000) !== 0;
        const L = (insn & 0x00100000) !== 0;
        const rn = (insn >>> 16) & 0xF;
        const list = insn & 0xFFFF;
        if (Sbit) return { ok: false };                                   // PSR/user-bank transfer — interp
        if (rn === 15) return { ok: false };                              // PC base — edge
        const regs: number[] = [];
        for (let i = 0; i < 16; i++) if (list & (1 << i)) regs.push(i);
        const count = regs.length;
        if (count === 0) return { ok: false };                            // empty-list quirk — interp
        const pcInList = L && (list & 0x8000) !== 0;
        const conditional = cond !== 0xE;
        if (conditional && pcInList) return { ok: false };
        // Offsets (from base) for the first slot + the writeback value.
        const firstOff = U ? (P ? 4 : 0) : (P ? -count * 4 : -count * 4 + 4);
        const wbOff = U ? count * 4 : -count * 4;

        const wBefore = writeMask;
        if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }

        pushReg(rn); f.localSet(L_A);                                     // base
        if (L) {
          for (let t = 0; t < count; t++) {
            const i = regs[t];
            // v = read32((base + off) & ~3)
            f.localGet(L_A); f.i32Const((firstOff + t * 4) | 0); f.op(W.OP_I32_ADD);
            f.i32Const(~3); f.op(W.OP_I32_AND); f.call(I_r32);
            if (i === 15) {
              f.i32Const(~3); f.op(W.OP_I32_AND); f.localSet(L_R);
              f.i32Const(0); f.localGet(L_R); f.i32Store(REG_BASE + 15 * 4);
            } else {
              f.localSet(L_R); storeRegFromLocal(i, L_R);
            }
          }
        } else {
          for (let t = 0; t < count; t++) {
            const i = regs[t];
            // write32((base + off) & ~3, value)
            f.localGet(L_A); f.i32Const((firstOff + t * 4) | 0); f.op(W.OP_I32_ADD);
            f.i32Const(~3); f.op(W.OP_I32_AND);                           // addr
            if (i === 15) {
              f.i32Const((pc + 12) | 0);                                  // PC stores decode+12
            } else if (i === rn && t !== 0) {
              f.localGet(L_A); f.i32Const(wbOff | 0); f.op(W.OP_I32_ADD); // base-in-list, not first → writeback value
            } else {
              pushReg(i);
            }
            f.call(I_w32);
          }
        }
        // Writeback last (so for LDM with base in list, it wins).
        if (Wb) {
          f.localGet(L_A); f.i32Const(wbOff | 0); f.op(W.OP_I32_ADD); f.localSet(L_R);
          storeRegFromLocal(rn, L_R);
        }

        if (conditional) { condWrites |= writeMask & ~wBefore; f.op(W.OP_END); }
        if (pcInList) { needsExitPc = false; return { ok: true, endsBlock: true }; }
        return { ok: true, endsBlock: false };
      }

      // -------- Single data transfer: LDR / STR (byte/word).
      if ((insn & 0x0C000000) === 0x04000000) {
        if (cond === 0xF) return { ok: true, endsBlock: false };          // NV → NOP
        const I = (insn & 0x02000000) !== 0;                              // 1 = register offset
        const P = (insn & 0x01000000) !== 0;                              // pre-index
        const U = (insn & 0x00800000) !== 0;                              // add (vs subtract)
        const Bb = (insn & 0x00400000) !== 0;                             // byte (vs word)
        const Wb = (insn & 0x00200000) !== 0;                             // writeback
        const L = (insn & 0x00100000) !== 0;                              // load (vs store)
        const rn = (insn >>> 16) & 0xF;
        const rd = (insn >>> 12) & 0xF;
        const writeback = !P || Wb;
        const ldrPc = L && rd === 15;                                     // block-ender
        const conditional = cond !== 0xE;
        if (writeback && rn === 15) return { ok: false };                 // PC-base writeback — edge
        if (conditional && ldrPc) return { ok: false };                   // conditional LDR PC — interp
        let rm = 0, shiftType = 0, imm5 = 0, imm12 = 0;
        if (I) { rm = insn & 0xF; shiftType = (insn >>> 5) & 3; imm5 = (insn >>> 7) & 0x1F; }
        else { imm12 = insn & 0xFFF; }

        const wBefore = writeMask;
        if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }

        // base → L_A  (R15 reads as decode+8)
        if (rn === 15) { f.i32Const((pc + 8) | 0); f.localSet(L_A); }
        else { pushReg(rn); f.localSet(L_A); }
        // offset → L_B
        if (I) {
          if (rm === 15) { f.i32Const((pc + 8) | 0); f.localSet(L_TMP); }
          else { pushReg(rm); f.localSet(L_TMP); }
          emitImmShiftValue(shiftType, imm5, L_TMP); f.localSet(L_B);
        } else {
          f.i32Const(imm12); f.localSet(L_B);
        }
        // eff = base ± offset → L_R ; addr = P ? eff : base → L_TMP
        f.localGet(L_A); f.localGet(L_B); f.op(U ? W.OP_I32_ADD : W.OP_I32_SUB); f.localSet(L_R);
        if (P) { f.localGet(L_R); f.localSet(L_TMP); } else { f.localGet(L_A); f.localSet(L_TMP); }

        if (L) {
          // Load value → L_TMP2.
          if (Bb) { f.localGet(L_TMP); f.call(I_r8); f.localSet(L_TMP2); }
          else { emitLoadWordRotated(L_TMP, L_TMP2); }
          // Writeback (skipped when rn === rd: the loaded value wins).
          if (writeback && rn !== rd) storeRegFromLocal(rn, L_R);
          if (rd === 15) {
            f.i32Const(0); f.localGet(L_TMP2); f.i32Const(~3); f.op(W.OP_I32_AND); f.i32Store(REG_BASE + 15 * 4);
          } else {
            storeRegFromLocal(rd, L_TMP2);
          }
        } else {
          // Store value = r[rd] (rd==15 → decode+12) → L_TMP2.
          if (rd === 15) { f.i32Const((pc + 12) | 0); f.localSet(L_TMP2); }
          else { pushReg(rd); f.localSet(L_TMP2); }
          if (Bb) { f.localGet(L_TMP); f.localGet(L_TMP2); f.call(I_w8); }
          else { emitStoreWordMasked(L_TMP, L_TMP2); }
          if (writeback) storeRegFromLocal(rn, L_R);
        }

        if (conditional) { condWrites |= writeMask & ~wBefore; f.op(W.OP_END); }
        if (ldrPc) { needsExitPc = false; return { ok: true, endsBlock: true }; }
        return { ok: true, endsBlock: false };
      }

      // -------- Multiply / swap / halfword (extension space).
      if ((insn & 0x0E000090) === 0x00000090) {
        if ((insn & 0x60) === 0) {
          // Multiply / swap space (bit24: 0 = multiply, 1 = swap).
          if (insn & 0x01000000) return { ok: false };                    // SWP/SWPB — interp
          if (cond === 0xF) return { ok: true, endsBlock: false };        // NV → NOP
          if (insn & 0x00800000) {
            // Long multiply: UMULL / SMULL / UMLAL / SMLAL (64-bit).
            const signed = (insn & 0x00400000) !== 0;
            const accumulate = (insn & 0x00200000) !== 0;
            const setFlags = (insn & 0x00100000) !== 0;
            const rdHi = (insn >>> 16) & 0xF;
            const rdLo = (insn >>> 12) & 0xF;
            const rs = (insn >>> 8) & 0xF;
            const rm = insn & 0xF;
            if (rdHi === 15 || rdLo === 15 || rs === 15 || rm === 15) return { ok: false };
            const conditional = cond !== 0xE;
            const wBefore = writeMask;
            if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }
            const ext = signed ? W.OP_I64_EXTEND_I32_S : W.OP_I64_EXTEND_I32_U;
            // product (i64) = extend(rm) * extend(rs)
            pushReg(rm); f.op(ext);
            pushReg(rs); f.op(ext);
            f.op(W.OP_I64_MUL);
            f.localSet(L_I64);
            if (accumulate) {
              // product += (RdHi << 32) | RdLo  (compose the 64-bit acc)
              f.localGet(L_I64);
              pushReg(rdHi); f.op(W.OP_I64_EXTEND_I32_U); f.i64Const(32); f.op(W.OP_I64_SHL);
              pushReg(rdLo); f.op(W.OP_I64_EXTEND_I32_U);
              f.op(W.OP_I64_OR);
              f.op(W.OP_I64_ADD);
              f.localSet(L_I64);
            }
            // Split: lo → L_R, hi → L_TMP.
            f.localGet(L_I64); f.op(W.OP_I32_WRAP_I64); f.localSet(L_R);
            f.localGet(L_I64); f.i64Const(32); f.op(W.OP_I64_SHR_U); f.op(W.OP_I32_WRAP_I64); f.localSet(L_TMP);
            storeRegFromLocal(rdLo, L_R);
            storeRegFromLocal(rdHi, L_TMP);
            if (setFlags) {
              // N = hi sign bit; Z = (hi == 0 && lo == 0).
              f.i32Const(0);
              f.i32Const(0); f.i32Load(CPSR_OFF); f.i32Const(0x3FFFFFFF); f.op(W.OP_I32_AND);
              f.localGet(L_TMP); f.i32Const(0x80000000 | 0); f.op(W.OP_I32_AND);
              f.op(W.OP_I32_OR);
              f.localGet(L_TMP); f.op(W.OP_I32_EQZ); f.localGet(L_R); f.op(W.OP_I32_EQZ); f.op(W.OP_I32_AND);
              f.i32Const(30); f.op(W.OP_I32_SHL);
              f.op(W.OP_I32_OR);
              f.i32Store(CPSR_OFF);
            }
            if (conditional) { condWrites |= writeMask & ~wBefore; f.op(W.OP_END); }
            return { ok: true, endsBlock: false };
          }
          const setFlags = (insn & 0x00100000) !== 0;
          const accumulate = (insn & 0x00200000) !== 0;
          const rd = (insn >>> 16) & 0xF;                                 // multiply: dest is 19:16
          const rnAcc = (insn >>> 12) & 0xF;
          const rs = (insn >>> 8) & 0xF;
          const rm = insn & 0xF;
          // R15 operands/dest are unpredictable for multiply — leave them.
          if (rd === 15 || rm === 15 || rs === 15 || (accumulate && rnAcc === 15)) return { ok: false };
          const conditional = cond !== 0xE;
          const wBefore = writeMask;
          if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }
          pushReg(rm); pushReg(rs); f.op(W.OP_I32_MUL); f.localSet(L_R);   // low 32 bits (= Math.imul)
          if (accumulate) { f.localGet(L_R); pushReg(rnAcc); f.op(W.OP_I32_ADD); f.localSet(L_R); }
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetNZ(L_R);
          if (conditional) { condWrites |= writeMask & ~wBefore; f.op(W.OP_END); }
          return { ok: true, endsBlock: false };
        }
        if (cond === 0xF) return { ok: true, endsBlock: false };          // NV → NOP
        const P = (insn & 0x01000000) !== 0;
        const U = (insn & 0x00800000) !== 0;
        const Iimm = (insn & 0x00400000) !== 0;                           // immediate offset
        const Wb = (insn & 0x00200000) !== 0;
        const L = (insn & 0x00100000) !== 0;
        const rn = (insn >>> 16) & 0xF;
        const rd = (insn >>> 12) & 0xF;
        const sh = (insn >>> 5) & 3;                                      // 1=H, 2=SB, 3=SH
        if (!L && sh !== 1) return { ok: false };                         // LDRD/STRD — not on ARM7
        const writeback = !P || Wb;
        if (writeback && rn === 15) return { ok: false };                 // PC-base writeback — edge
        const ldrPc = L && rd === 15;
        const conditional = cond !== 0xE;
        if (conditional && ldrPc) return { ok: false };
        let rm = 0, offImm = 0;
        if (Iimm) offImm = ((insn >>> 4) & 0xF0) | (insn & 0xF);
        else rm = insn & 0xF;

        const wBefore = writeMask;
        if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }

        if (rn === 15) { f.i32Const((pc + 8) | 0); f.localSet(L_A); } else { pushReg(rn); f.localSet(L_A); }
        if (Iimm) { f.i32Const(offImm); f.localSet(L_B); }
        else if (rm === 15) { f.i32Const((pc + 8) | 0); f.localSet(L_B); }
        else { pushReg(rm); f.localSet(L_B); }
        f.localGet(L_A); f.localGet(L_B); f.op(U ? W.OP_I32_ADD : W.OP_I32_SUB); f.localSet(L_R);
        if (P) { f.localGet(L_R); f.localSet(L_TMP); } else { f.localGet(L_A); f.localSet(L_TMP); }

        if (L) {
          if (sh === 1) {                                                 // LDRH (rotated)
            emitLoadHalfRotated(L_TMP, L_TMP2);
          } else if (sh === 2) {                                          // LDRSB
            f.localGet(L_TMP); f.call(I_r8); f.i32Const(24); f.op(W.OP_I32_SHL); f.i32Const(24); f.op(W.OP_I32_SHR_S); f.localSet(L_TMP2);
          } else {                                                        // LDRSH (odd addr → LDRSB)
            f.localGet(L_TMP); f.i32Const(1); f.op(W.OP_I32_AND);
            f.op(W.OP_IF); f.body.push(0x40);
            f.localGet(L_TMP); f.call(I_r8); f.i32Const(24); f.op(W.OP_I32_SHL); f.i32Const(24); f.op(W.OP_I32_SHR_S); f.localSet(L_TMP2);
            f.op(W.OP_ELSE);
            f.localGet(L_TMP); f.i32Const(~1); f.op(W.OP_I32_AND); f.call(I_r16); f.i32Const(16); f.op(W.OP_I32_SHL); f.i32Const(16); f.op(W.OP_I32_SHR_S); f.localSet(L_TMP2);
            f.op(W.OP_END);
          }
          if (writeback && rn !== rd) storeRegFromLocal(rn, L_R);
          if (rd === 15) {
            f.i32Const(0); f.localGet(L_TMP2); f.i32Const(~3); f.op(W.OP_I32_AND); f.i32Store(REG_BASE + 15 * 4);
          } else {
            storeRegFromLocal(rd, L_TMP2);
          }
        } else {                                                          // STRH
          if (rd === 15) { f.i32Const((pc + 8) | 0); f.localSet(L_TMP2); } else { pushReg(rd); f.localSet(L_TMP2); }
          emitStoreHalfMasked(L_TMP, L_TMP2);
          if (writeback) storeRegFromLocal(rn, L_R);
        }

        if (conditional) { condWrites |= writeMask & ~wBefore; f.op(W.OP_END); }
        if (ldrPc) { needsExitPc = false; return { ok: true, endsBlock: true }; }
        return { ok: true, endsBlock: false };
      }
      if ((insn & 0x0F900000) === 0x01000000 && (insn & 0x90) !== 0x90) return { ok: false }; // MRS/MSR
      if ((insn & 0x0FB00000) === 0x03200000) return { ok: false };      // MSR immediate

      // ---- Data processing
      const opcode = (insn >>> 21) & 0xF;
      const setFlags = (insn & 0x00100000) !== 0;
      const rn = (insn >>> 16) & 0xF;
      const rd = (insn >>> 12) & 0xF;
      const writes = !(opcode >= 0x8 && opcode <= 0xB);                 // TST/TEQ/CMP/CMN write no rd
      const conditional = cond !== 0xE;
      // rd==15 = ALU write to PC (a branch). The S-bit form restores
      // SPSR (mode change) and a conditional PC-write needs an else →
      // fall-through; both are left to the interpreter for now.
      if (rd === 15 && writes && (setFlags || conditional)) return { ok: false };

      // Decode operand2 form + reject unsupported variants BEFORE any
      // code is emitted — bailing mid-emit would leave a half-open
      // conditional block in the module.
      const immForm = (insn & 0x02000000) !== 0;
      const regShiftForm = !immForm && (insn & 0x10) !== 0;
      let rm = 0, rs = 0, shiftType = 0, imm5 = 0;
      let op2Const = 0, carryKind = 0, carryConst = 0;   // carryKind: 0 unchanged, 1 const, 2 in L_C
      if (immForm) {
        const imm = insn & 0xFF;
        const rot = ((insn >>> 8) & 0xF) << 1;
        const rr = rot & 31;
        op2Const = rr === 0 ? (imm >>> 0) : (((imm >>> rr) | (imm << (32 - rr))) >>> 0);
        if (rot !== 0) { carryKind = 1; carryConst = (op2Const >>> 31) & 1; }
      } else {
        rm = insn & 0xF;
        shiftType = (insn >>> 5) & 3;
        if (regShiftForm) {
          rs = (insn >>> 8) & 0xF;
          if (rs === 15) return { ok: false };                          // R15 shift amount — edge
          carryKind = 2;                                                // reg shift always yields a carry
        } else {
          imm5 = (insn >>> 7) & 0x1F;
          if (!(shiftType === 0 && imm5 === 0)) carryKind = 2;          // LSL #0 leaves C alone
        }
      }
      // Architectural PC when R15 is an operand: decode+8, or decode+12
      // with a register-specified shift (matches the interpreter).
      const pcOperand = (regShiftForm ? (pc + 12) : (pc + 8)) >>> 0;

      // Conditionally-executed (non-AL) instructions wrap their whole
      // side-effecting body in a WASM if-block; AL runs unconditionally.
      const wBefore = writeMask;
      if (conditional) { emitCheckCond(cond); f.op(W.OP_IF); f.body.push(0x40); }

      // operand1 → L_A
      if (rn === 15) { f.i32Const(pcOperand | 0); f.localSet(L_A); }
      else { pushReg(rn); f.localSet(L_A); }

      // operand2 → L_B (+ shifter carry into L_C when carryKind === 2).
      if (immForm) {
        f.i32Const(op2Const | 0); f.localSet(L_B);
      } else if (regShiftForm) {
        if (rm === 15) { f.i32Const(pcOperand | 0); f.localSet(L_X); }
        else { pushReg(rm); f.localSet(L_X); }
        emitArmRegShift(shiftType, rs);                                 // value L_X → L_B, carry → L_C
      } else {
        if (rm === 15) { f.i32Const(pcOperand | 0); f.localSet(L_TMP); }
        else { pushReg(rm); f.localSet(L_TMP); }
        emitImmShiftValue(shiftType, imm5, L_TMP); f.localSet(L_B);
        if (carryKind === 2) { emitImmShiftCarry(shiftType, imm5, L_TMP); f.localSet(L_C); }
      }

      // Push CPSR.C from the shifter result (logical S ops + TST/TEQ).
      const applyShiftCarry = () => {
        if (carryKind === 0) return;
        if (carryKind === 1) { f.i32Const(carryConst); f.localSet(L_C); }
        emitSetC(L_C);
      };

      switch (opcode) {
        case 0x0: // AND
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_AND); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0x1: // EOR
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_XOR); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0xC: // ORR
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_OR); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0xD: // MOV
          f.localGet(L_B); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0xE: // BIC: a & ~b
          f.localGet(L_A); f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR); f.op(W.OP_I32_AND); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0xF: // MVN: ~b
          f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) { emitSetNZ(L_R); applyShiftCarry(); }
          break;
        case 0x8: // TST (always flags, no write)
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_AND); f.localSet(L_R);
          emitSetNZ(L_R); applyShiftCarry();
          break;
        case 0x9: // TEQ
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_XOR); f.localSet(L_R);
          emitSetNZ(L_R); applyShiftCarry();
          break;
        case 0x2: // SUB
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetFlagsSub(L_A, L_B, L_R);
          break;
        case 0x3: // RSB: b - a
          f.localGet(L_B); f.localGet(L_A); f.op(W.OP_I32_SUB); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetFlagsSub(L_B, L_A, L_R);
          break;
        case 0x4: // ADD
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetFlagsAdd(L_A, L_B, L_R);
          break;
        case 0xA: // CMP (always flags, no write)
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
          emitSetFlagsSub(L_A, L_B, L_R);
          break;
        case 0xB: // CMN
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_R);
          emitSetFlagsAdd(L_A, L_B, L_R);
          break;
        case 0x5: // ADC: r = a + b + cIn
          pushCarryIn(); f.localSet(L_TMP2);
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_ADD); f.localSet(L_TMP);   // t = a+b
          f.localGet(L_TMP); f.localGet(L_TMP2); f.op(W.OP_I32_ADD); f.localSet(L_R); // r = t+cIn
          storeRegFromLocal(rd, L_R);
          if (setFlags) {
            // C = (t <u a) | (r <u t)
            f.localGet(L_TMP); f.localGet(L_A); f.op(W.OP_I32_LT_U);
            f.localGet(L_R); f.localGet(L_TMP); f.op(W.OP_I32_LT_U);
            f.op(W.OP_I32_OR); f.localSet(L_TMP2);
            emitSetFlagsCarryLocal(L_A, L_B, L_R, L_TMP2, false);
          }
          break;
        case 0x6: // SBC: r = a - b - (cIn^1)
          pushCarryIn(); f.localSet(L_TMP2);
          if (setFlags) {
            // C = (a >u b) | ((a==b) & cIn)
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_GT_U);
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_EQ);
            f.localGet(L_TMP2); f.op(W.OP_I32_AND);
            f.op(W.OP_I32_OR); f.localSet(L_TMP);
          }
          f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB);
          f.localGet(L_TMP2); f.op(W.OP_I32_ADD); f.i32Const(1); f.op(W.OP_I32_SUB); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetFlagsCarryLocal(L_A, L_B, L_R, L_TMP, true);
          break;
        case 0x7: // RSC: r = b - a - (cIn^1)
          pushCarryIn(); f.localSet(L_TMP2);
          if (setFlags) {
            f.localGet(L_B); f.localGet(L_A); f.op(W.OP_I32_GT_U);
            f.localGet(L_B); f.localGet(L_A); f.op(W.OP_I32_EQ);
            f.localGet(L_TMP2); f.op(W.OP_I32_AND);
            f.op(W.OP_I32_OR); f.localSet(L_TMP);
          }
          f.localGet(L_B); f.localGet(L_A); f.op(W.OP_I32_SUB);
          f.localGet(L_TMP2); f.op(W.OP_I32_ADD); f.i32Const(1); f.op(W.OP_I32_SUB); f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
          if (setFlags) emitSetFlagsCarryLocal(L_B, L_A, L_R, L_TMP, true);
          break;
        default:
          return { ok: false };
      }
      // rd==15 (only reached for AL, non-S, writing opcodes): the case
      // above stored the raw result into r15 — re-align to ARM (& ~3)
      // and end the block (an ALU branch).
      if (rd === 15 && writes) {
        f.i32Const(0); f.i32Const(0); f.i32Load(REG_BASE + 15 * 4);
        f.i32Const(~3); f.op(W.OP_I32_AND); f.i32Store(REG_BASE + 15 * 4);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }
      if (conditional) {
        // Any register this instruction newly wrote is written only when
        // the condition holds — sync it in so the skip path preserves it.
        condWrites |= writeMask & ~wBefore;
        f.op(W.OP_END);
      }
      return { ok: true, endsBlock: false };
    };

    const step = thumb ? 2 : 4;
    for (; count < MAX_BLOCK_INSNS; ) {
      const insn = thumb ? bus.read16(pc) : bus.read32(pc);
      const res = thumb ? translate(insn) : translateArm(insn);
      if (!res.ok) break;
      pc = (pc + step) >>> 0;
      count++;
      if (res.endsBlock) break;
    }

    if (count === 0) return null;

    // RAM blocks get a live view + snapshot of their source halfwords
    // so tryDispatch can detect the game overwriting the code. Blocks
    // that would straddle a region mirror boundary are refused (the
    // compile loop's bus reads wrapped, so a flat subarray can't
    // represent what was compiled).
    let liveCode: Uint16Array | null = null;
    let snapshot: Uint16Array | null = null;
    if (inRam) {
      const arr16 = region === 0x02 ? bus.ewram16 : bus.iwram16;
      const mask = region === 0x02 ? 0x3FFFF : 0x7FFF;
      const off = (startPc & mask) >>> 1;
      // Halfwords spanned: 1 per THUMB insn, 2 per ARM insn.
      const hwCount = count * (thumb ? 1 : 2);
      if (off + hwCount > arr16.length) return null;
      liveCode = arr16.subarray(off, off + hwCount);
      snapshot = liveCode.slice();
    }

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
      thumb,
      liveCode,
      snapshot,
      // PC (r15) is always written (block epilogue / branch) and may be
      // read; force it into both sets so a missed mark can't strand it.
      // condWrites are conditionally-written regs that must be synced in.
      inMask: (readMask | condWrites | (1 << 15)) & 0xFFFF,
      outMask: (writeMask | (1 << 15)) & 0xFFFF,
      run: () => exported(0),
    };
  }
}
