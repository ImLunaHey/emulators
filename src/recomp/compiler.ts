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
// that to advance its own instruction-count statistics. r[15] is
// updated via the setR import whenever a branch is taken inside the
// block; otherwise the block emits a final setR at the end with the
// next-PC value so the dispatcher knows where to go.

interface CompiledBlock {
  run: () => number;
  startPc: number;
  insnCount: number;
}

const HOT_THRESHOLD = 50;
const MAX_BLOCK_INSNS = 32;

export class Recompiler {
  cache = new Map<number, CompiledBlock | null>();   // null = compile failed
  hits = new Map<number, number>();
  jitInsns = 0;
  intInsns = 0;
  // The JIT translator works correctly but is currently SLOWER than the
  // interpreter because every translated instruction makes 1-3 imported
  // JS function calls (getR/setR/setNZ/etc.), and each WASM↔JS boundary
  // crossing is ~50-150ns. The interpreter spends ~50ns total on simple
  // ALU ops, so the WASM dispatch loses on net. The proper fix is
  // putting register + CPSR state inside the WASM module's linear
  // memory so register access becomes a direct i32.load/store instead
  // of an import — that's a 5-10× speedup and the next milestone.
  // Until then the JIT is opt-in: the Recompiler infrastructure is
  // ready and tests prove it executes correctly, but it's left disabled
  // so the default config doesn't lose performance to it.
  enabled = false;

  // Host hooks bound at construction so each WASM Instance can use the
  // same callbacks. CPU state goes through them; memory I/O goes
  // through them.
  private hooks: {
    getR:  (i: number) => number;
    setR:  (i: number, v: number) => void;
    setNZ: (v: number) => void;
    setFlagsAdd: (a: number, b: number, r: number) => void;
    setFlagsSub: (a: number, b: number, r: number) => void;
    checkCond:   (cond: number) => number;
    r32: (a: number) => number;  w32: (a: number, v: number) => void;
    r16: (a: number) => number;  w16: (a: number, v: number) => void;
    r8:  (a: number) => number;  w8:  (a: number, v: number) => void;
  };

  constructor(public cpu: Cpu) {
    // Resolve cpu.state lazily on every call — cpu.reset() replaces the
    // state object entirely, so capturing it at construction would
    // strand the hooks pointing at a dead object. Bus is stable for the
    // emulator's lifetime so binding bus once is safe.
    const c = cpu;
    const bus = cpu.bus;
    this.hooks = {
      getR:  (i) => c.state.r[i] >>> 0,
      setR:  (i, v) => { c.state.r[i] = v >>> 0; },
      setNZ: (v) => c.state.setNZ(v),
      setFlagsAdd: (a, b, r) => {
        const s = c.state;
        s.setNZ(r);
        // Carry on unsigned add: result < either operand (unsigned).
        if ((r >>> 0) < (a >>> 0)) s.cpsr |= 0x20000000; else s.cpsr &= ~0x20000000;
        // V on signed add: both inputs same sign, result opposite.
        const v = (~(a ^ b) & (a ^ r)) >>> 31;
        if (v) s.cpsr |= 0x10000000; else s.cpsr &= ~0x10000000;
      },
      setFlagsSub: (a, b, r) => {
        const s = c.state;
        s.setNZ(r);
        // Carry on unsigned sub: no borrow when a >= b (unsigned).
        if ((a >>> 0) >= (b >>> 0)) s.cpsr |= 0x20000000; else s.cpsr &= ~0x20000000;
        // V: inputs different sign AND result sign differs from a.
        const v = ((a ^ b) & (a ^ r)) >>> 31;
        if (v) s.cpsr |= 0x10000000; else s.cpsr &= ~0x10000000;
      },
      checkCond: (cond) => c.state.checkCond(cond) ? 1 : 0,
      r32: (a) => bus.read32(a >>> 0),
      w32: (a, v) => bus.write32(a >>> 0, v >>> 0),
      r16: (a) => bus.read16(a >>> 0),
      w16: (a, v) => bus.write16(a >>> 0, v & 0xFFFF),
      r8:  (a) => bus.read8(a >>> 0),
      w8:  (a, v) => bus.write8(a >>> 0, v & 0xFF),
    };
  }

  tryDispatch(): boolean {
    if (!this.enabled) return false;
    const s = this.cpu.state;
    if (!(s.cpsr & 0x20)) return false;        // ARM blocks not jitted
    // Between step() calls, r[15] holds the decode address of the next
    // THUMB instruction (with bit 0 as the THUMB indicator). Strip that
    // bit to get the actual pc; that's the key we cache compiled
    // blocks under.
    const pc = s.r[15] & ~1;
    const cached = this.cache.get(pc);
    if (cached === null) return false;          // known-uncompilable
    if (cached) {
      const n = cached.run();
      this.jitInsns += n;
      return n > 0;
    }
    const c = (this.hits.get(pc) || 0) + 1;
    this.hits.set(pc, c);
    if (c < HOT_THRESHOLD) return false;
    this.hits.delete(pc);
    const block = this.compile(pc);
    this.cache.set(pc, block);
    if (!block) return false;
    const n = block.run();
    this.jitInsns += n;
    return n > 0;
  }

  invalidate(): void { this.cache.clear(); this.hits.clear(); }

  private compile(startPc: number): CompiledBlock | null {
    const bus = this.cpu.bus;
    const builder = new W.WasmModuleBuilder();
    const f = builder.func;

    // Import indices — these match `this.hooks` shape.
    const I_getR        = builder.addImport('h', 'getR',        [W.I32], [W.I32]);
    const I_setR        = builder.addImport('h', 'setR',        [W.I32, W.I32], []);
    const I_setNZ       = builder.addImport('h', 'setNZ',       [W.I32], []);
    const I_setFlagsAdd = builder.addImport('h', 'setFlagsAdd', [W.I32, W.I32, W.I32], []);
    const I_setFlagsSub = builder.addImport('h', 'setFlagsSub', [W.I32, W.I32, W.I32], []);
    const I_checkCond   = builder.addImport('h', 'checkCond',   [W.I32], [W.I32]);
    const I_r32         = builder.addImport('h', 'r32',         [W.I32], [W.I32]);
    const I_w32         = builder.addImport('h', 'w32',         [W.I32, W.I32], []);
    const I_r16         = builder.addImport('h', 'r16',         [W.I32], [W.I32]);
    const I_w16         = builder.addImport('h', 'w16',         [W.I32, W.I32], []);
    const I_r8          = builder.addImport('h', 'r8',          [W.I32], [W.I32]);
    const I_w8          = builder.addImport('h', 'w8',          [W.I32, W.I32], []);

    // Reserve a couple of locals for intermediate values. The function
    // already has 1 i32 parameter (unused), so local indices 1..N are
    // available after we addLocals().
    f.addLocals(4, W.I32);
    const L_A = 1, L_B = 2, L_R = 3, L_TMP = 4;

    // Tiny emitter primitives that hide the import indices.
    const pushGetR = (rd: number) => { f.i32Const(rd); f.call(I_getR); };
    const callSetR = () => { f.call(I_setR); };

    let pc = startPc;
    let count = 0;
    let needsExitPc = true;  // we'll emit setR(15, ...) at end unless a branch already did

    // Translate one THUMB instruction. Returns true if successfully
    // translated, false to bail out (the caller will discard this
    // attempt entirely or end the block depending on whether we've
    // already emitted anything).
    const translate = (insn: number): { ok: true; endsBlock: boolean } | { ok: false } => {
      const top3 = insn >>> 13;

      // -------- Format 3: MOV/CMP/ADD/SUB Rd, #imm8 (8-bit imm)
      if (top3 === 0b001) {
        const op = (insn >>> 11) & 3;
        const rd = (insn >>> 8) & 7;
        const imm = insn & 0xFF;
        if (op === 0) {
          // MOV Rd, #imm: setR(rd, imm); setNZ(imm)
          f.i32Const(rd); f.i32Const(imm); callSetR();
          f.i32Const(imm); f.call(I_setNZ);
        } else if (op === 1) {
          // CMP Rd, #imm: a = getR(rd); r = a - imm; flagsSub(a, imm, r)
          pushGetR(rd); f.localSet(L_A);
          f.localGet(L_A); f.i32Const(imm); f.op(W.OP_I32_SUB); f.localSet(L_R);
          f.localGet(L_A); f.i32Const(imm); f.localGet(L_R); f.call(I_setFlagsSub);
        } else if (op === 2) {
          // ADD Rd, #imm: r = getR(rd) + imm; setR(rd, r); flagsAdd(a, imm, r)
          pushGetR(rd); f.localSet(L_A);
          f.localGet(L_A); f.i32Const(imm); f.op(W.OP_I32_ADD); f.localSet(L_R);
          f.i32Const(rd); f.localGet(L_R); callSetR();
          f.localGet(L_A); f.i32Const(imm); f.localGet(L_R); f.call(I_setFlagsAdd);
        } else {
          // SUB Rd, #imm: r = getR(rd) - imm; setR(rd, r); flagsSub(a, imm, r)
          pushGetR(rd); f.localSet(L_A);
          f.localGet(L_A); f.i32Const(imm); f.op(W.OP_I32_SUB); f.localSet(L_R);
          f.i32Const(rd); f.localGet(L_R); callSetR();
          f.localGet(L_A); f.i32Const(imm); f.localGet(L_R); f.call(I_setFlagsSub);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 4: ALU register ops (4-bit op selector)
      if ((insn & 0xFC00) === 0x4000) {
        const aluOp = (insn >>> 6) & 0xF;
        const rs = (insn >>> 3) & 7;
        const rd = insn & 7;
        // a = getR(rd), b = getR(rs)
        pushGetR(rd); f.localSet(L_A);
        pushGetR(rs); f.localSet(L_B);
        switch (aluOp) {
          case 0x0: // AND
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_AND); f.localSet(L_R);
            f.i32Const(rd); f.localGet(L_R); callSetR();
            f.localGet(L_R); f.call(I_setNZ);
            return { ok: true, endsBlock: false };
          case 0x1: // EOR
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_XOR); f.localSet(L_R);
            f.i32Const(rd); f.localGet(L_R); callSetR();
            f.localGet(L_R); f.call(I_setNZ);
            return { ok: true, endsBlock: false };
          case 0xA: // CMP Rd, Rs
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
            f.localGet(L_A); f.localGet(L_B); f.localGet(L_R); f.call(I_setFlagsSub);
            return { ok: true, endsBlock: false };
          case 0xC: // ORR
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_OR); f.localSet(L_R);
            f.i32Const(rd); f.localGet(L_R); callSetR();
            f.localGet(L_R); f.call(I_setNZ);
            return { ok: true, endsBlock: false };
          case 0xE: // BIC
            f.localGet(L_A); f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR); f.op(W.OP_I32_AND); f.localSet(L_R);
            f.i32Const(rd); f.localGet(L_R); callSetR();
            f.localGet(L_R); f.call(I_setNZ);
            return { ok: true, endsBlock: false };
          case 0xF: // MVN
            f.localGet(L_B); f.i32Const(-1); f.op(W.OP_I32_XOR); f.localSet(L_R);
            f.i32Const(rd); f.localGet(L_R); callSetR();
            f.localGet(L_R); f.call(I_setNZ);
            return { ok: true, endsBlock: false };
          // Other Format 4 ops (LSL/LSR/ASR by reg, ADC/SBC/NEG/MUL/etc.)
          // fall through to interpreter for now.
        }
        return { ok: false };
      }

      // -------- Format 9 (8-bit): LDR/STR Rd, [Rb, #imm5*4 / *1]
      // 011_BL_offset5_Rb_Rd
      if ((insn & 0xE000) === 0x6000) {
        const isByte = (insn & 0x1000) !== 0;
        const isLoad = (insn & 0x0800) !== 0;
        const off5   = (insn >>> 6) & 0x1F;
        const rb     = (insn >>> 3) & 7;
        const rd     = insn & 7;
        const offset = isByte ? off5 : off5 << 2;
        // addr = getR(rb) + offset
        pushGetR(rb); f.i32Const(offset); f.op(W.OP_I32_ADD); f.localSet(L_TMP);
        if (isLoad) {
          if (isByte) { f.localGet(L_TMP); f.call(I_r8); }
          else        { f.localGet(L_TMP); f.call(I_r32); }
          f.localSet(L_R);
          f.i32Const(rd); f.localGet(L_R); callSetR();
        } else {
          pushGetR(rd); f.localSet(L_R);
          f.localGet(L_TMP); f.localGet(L_R);
          if (isByte) f.call(I_w8); else f.call(I_w32);
        }
        return { ok: true, endsBlock: false };
      }

      // -------- Format 16: B<cond> label (8-bit signed offset)
      if ((insn & 0xF000) === 0xD000) {
        const cond = (insn >>> 8) & 0xF;
        if (cond === 0xE || cond === 0xF) return { ok: false }; // SWI / undefined
        let off = insn & 0xFF;
        if (off & 0x80) off -= 0x100;
        const taken    = (pc + 4 + (off << 1)) >>> 0;
        const fallthru = (pc + 2) >>> 0;
        // if (checkCond(cond)) setR(15, taken+4); else setR(15, fallthru+4);
        // r[15] stores the *next decode address + insnSize*; the dispatcher
        // re-reads r[15] - 4 as the decode pc each step. THUMB insnSize=2,
        // prefetchOff=4, so r[15] = decode + 4 means "decode at pc".
        // The dispatcher loop does `r[15] = decode + prefetchOff` before
        // executing, and after execute pulls r[15] - prefetchOff back as
        // the new decode. So a branch sets r[15] = (target & ~1) + 0
        // and the next dispatch will use r[15] - 4 = target - 4... hmm.
        // The interpreter's branch path calls flushPipeline() and writes
        // r[15] = target & ~1. Then `branched` is true so step()'s
        // auto-advance skips. On the next step(), decode = r[15] & ~1.
        // We're not inside step(), we're being called from inside it (the
        // recompiler tryDispatch happens BEFORE the dispatcher's prefetch
        // setup). So for branch handling we want r[15] = target & ~1 just
        // like the interpreter would. The host dispatcher will then
        // continue at the new pc.
        f.i32Const(cond); f.call(I_checkCond);
        f.op(W.OP_IF); f.body.push(0x40); // void block type
        f.i32Const(15); f.i32Const((taken & ~1) >>> 0); callSetR();
        f.op(W.OP_ELSE);
        f.i32Const(15); f.i32Const((fallthru & ~1) >>> 0); callSetR();
        f.op(W.OP_END);
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      // -------- Format 18: B label (11-bit signed offset)
      if ((insn & 0xF800) === 0xE000) {
        let off = insn & 0x7FF;
        if (off & 0x400) off -= 0x800;
        const target = (pc + 4 + (off << 1)) >>> 0;
        f.i32Const(15); f.i32Const((target & ~1) >>> 0); callSetR();
        needsExitPc = false;
        return { ok: true, endsBlock: true };
      }

      // Unsupported — let interpreter take over.
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

    // Emit any post-block bookkeeping: if no branch already wrote PC,
    // do it now so the dispatcher knows to resume at `pc`.
    if (needsExitPc) {
      f.i32Const(15); f.i32Const(pc >>> 0); callSetR();
    }
    // Return count. The WasmModuleBuilder.encode() appends the
    // function-body terminator OP_END for us; don't double-emit it.
    f.i32Const(count);

    let module: WebAssembly.Module;
    try {
      const bytes = builder.encode();
      const arr = new Uint8Array(new ArrayBuffer(bytes.length));
      arr.set(bytes);
      module = new WebAssembly.Module(arr);
    } catch (e) {
      // Failed validation — pretend we never tried.
      return null;
    }
    let instance: WebAssembly.Instance;
    try {
      instance = new WebAssembly.Instance(module, { h: this.hooks });
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
