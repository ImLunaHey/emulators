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

  tryDispatch(): boolean {
    if (!this.enabled) return false;
    const s = this.cpu.state;
    if (!(s.cpsr & 0x20)) return false;        // ARM blocks not jitted
    const pc = s.r[15] & ~1;
    const cached = this.cache.get(pc);
    if (cached === null) return false;          // known-uncompilable
    if (cached) {
      return this.runBlock(cached);
    }
    const c = (this.hits.get(pc) || 0) + 1;
    this.hits.set(pc, c);
    if (c < HOT_THRESHOLD) return false;
    this.hits.delete(pc);
    const block = this.compile(pc);
    this.cache.set(pc, block);
    if (!block) return false;
    return this.runBlock(block);
  }

  invalidate(): void { this.cache.clear(); this.hits.clear(); }

  // Push JS-side CpuState into linear memory, run the block, pull state
  // back. The copy is 17 u32 writes either side (~50ns total) — small
  // enough that even short blocks come out ahead of the interpreter.
  private runBlock(block: CompiledBlock): boolean {
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
    return n > 0;
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
    f.addLocals(5, W.I32);
    const L_A = 1, L_B = 2, L_R = 3, L_TMP = 4, L_TMP2 = 5;

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
          case 0xA: // CMP
            f.localGet(L_A); f.localGet(L_B); f.op(W.OP_I32_SUB); f.localSet(L_R);
            emitSetFlagsSub(L_A, L_B, L_R);
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
          f.localGet(L_TMP);
          if (isByte) f.call(I_r8); else f.call(I_r32);
          f.localSet(L_R);
          storeRegFromLocal(rd, L_R);
        } else {
          pushReg(rd); f.localSet(L_R);
          f.localGet(L_TMP); f.localGet(L_R);
          if (isByte) f.call(I_w8); else f.call(I_w32);
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
