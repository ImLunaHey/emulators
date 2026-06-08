// Minimal WebAssembly binary emitter — just enough to produce modules
// holding a single exported function that does some ALU work on i32
// registers and returns an exit-reason code.

// LEB128 helpers.
function uleb(n: number, out: number[]): void {
  do {
    let byte = n & 0x7F;
    n >>>= 7;
    if (n !== 0) byte |= 0x80;
    out.push(byte);
  } while (n !== 0);
}
function sleb(n: number, out: number[]): void {
  let more = true;
  while (more) {
    let byte = n & 0x7F;
    const signBit = byte & 0x40;
    n >>= 7;
    if ((n === 0 && !signBit) || (n === -1 && signBit)) more = false;
    else byte |= 0x80;
    out.push(byte);
  }
}
function emitU32(n: number, out: number[]): void {
  for (let i = 0; i < 4; i++) out.push((n >>> (i * 8)) & 0xFF);
}

export const WASM_HEADER = [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];

// Section ids
export const SEC_TYPE   = 1;
export const SEC_IMPORT = 2;
export const SEC_FUNC   = 3;
export const SEC_MEM    = 5;
export const SEC_GLOBAL = 6;
export const SEC_EXPORT = 7;
export const SEC_CODE   = 10;

// Value types
export const I32 = 0x7F;
export const I64 = 0x7E;

// Opcodes we use.
export const OP_END         = 0x0B;
export const OP_BLOCK       = 0x02;
export const OP_LOOP        = 0x03;
export const OP_IF          = 0x04;
export const OP_ELSE        = 0x05;
export const OP_BR          = 0x0C;
export const OP_BR_IF       = 0x0D;
export const OP_RETURN      = 0x0F;
export const OP_CALL        = 0x10;
export const OP_DROP        = 0x1A;
export const OP_LOCAL_GET   = 0x20;
export const OP_LOCAL_SET   = 0x21;
export const OP_LOCAL_TEE   = 0x22;
export const OP_GLOBAL_GET  = 0x23;
export const OP_GLOBAL_SET  = 0x24;
export const OP_I32_CONST   = 0x41;
export const OP_I32_EQZ     = 0x45;
export const OP_I32_EQ      = 0x46;
export const OP_I32_NE      = 0x47;
export const OP_I32_LT_S    = 0x48;
export const OP_I32_LT_U    = 0x49;
export const OP_I32_GT_S    = 0x4A;
export const OP_I32_GT_U    = 0x4B;
export const OP_I32_LE_S    = 0x4C;
export const OP_I32_LE_U    = 0x4D;
export const OP_I32_GE_S    = 0x4E;
export const OP_I32_GE_U    = 0x4F;
export const OP_I32_ADD     = 0x6A;
export const OP_I32_SUB     = 0x6B;
export const OP_I32_MUL     = 0x6C;
export const OP_I32_AND     = 0x71;
export const OP_I32_OR      = 0x72;
export const OP_I32_XOR     = 0x73;
export const OP_I32_SHL     = 0x74;
export const OP_I32_SHR_S   = 0x75;
export const OP_I32_SHR_U   = 0x76;
export const OP_I32_ROTL    = 0x77;
export const OP_I32_ROTR    = 0x78;
export const OP_I32_LOAD    = 0x28;
export const OP_I32_LOAD8_U = 0x2D;
export const OP_I32_STORE   = 0x36;
export const OP_I32_STORE8  = 0x3A;
export const OP_I32_STORE16 = 0x3B;

export class WasmFunction {
  locals: number[] = []; // count, type, count, type, ...
  body: number[] = [];

  addLocals(count: number, type: number): void {
    // Find existing matching group or append.
    for (let i = 0; i < this.locals.length; i += 2) {
      if (this.locals[i + 1] === type) { this.locals[i] += count; return; }
    }
    this.locals.push(count, type);
  }

  i32Const(v: number): this { this.body.push(OP_I32_CONST); sleb(v | 0, this.body); return this; }
  localGet(i: number): this { this.body.push(OP_LOCAL_GET); uleb(i, this.body); return this; }
  localSet(i: number): this { this.body.push(OP_LOCAL_SET); uleb(i, this.body); return this; }
  localTee(i: number): this { this.body.push(OP_LOCAL_TEE); uleb(i, this.body); return this; }
  globalGet(i: number): this { this.body.push(OP_GLOBAL_GET); uleb(i, this.body); return this; }
  globalSet(i: number): this { this.body.push(OP_GLOBAL_SET); uleb(i, this.body); return this; }
  call(fi: number): this { this.body.push(OP_CALL); uleb(fi, this.body); return this; }
  drop(): this { this.body.push(OP_DROP); return this; }
  op(opcode: number): this { this.body.push(opcode); return this; }
  end(): this { this.body.push(OP_END); return this; }
  ret(): this { this.body.push(OP_RETURN); return this; }

  // memarg = align (uleb) + offset (uleb). align is log2 of natural size.
  i32Load(offset: number, align = 2): this {
    this.body.push(OP_I32_LOAD); uleb(align, this.body); uleb(offset, this.body); return this;
  }
  i32Store(offset: number, align = 2): this {
    this.body.push(OP_I32_STORE); uleb(align, this.body); uleb(offset, this.body); return this;
  }
  i32Load8U(offset: number): this {
    this.body.push(OP_I32_LOAD8_U); uleb(0, this.body); uleb(offset, this.body); return this;
  }
  i32Store8(offset: number): this {
    this.body.push(OP_I32_STORE8); uleb(0, this.body); uleb(offset, this.body); return this;
  }
}

// Builds a module:
//   - imports: i32 helpers (e.g. ALU + memory)
//   - exports: one function "run" that takes i32 pc, returns i32 exit reason
export class WasmModuleBuilder {
  // Param types per function type. We use a single signature: (i32) -> i32.
  private funcSig = { params: [I32], results: [I32] };
  // Imported helpers — each has the SAME signature for simplicity.
  // Caller provides: read8, read16, read32, write8, write16, write32, getR, setR,
  //                  setNZ, exec_thumb, exec_arm
  imports: { module: string; field: string; params: number[]; results: number[] }[] = [];
  // Optional memory import. When set, the module declares an imported
  // memory (kind=0x02) instead of defining its own. Limits: min pages
  // only (max omitted).
  memImport: { module: string; field: string; minPages: number } | null = null;
  func: WasmFunction;
  exportName = 'run';

  constructor() {
    this.func = new WasmFunction();
  }

  addImport(module: string, field: string, params: number[], results: number[]): number {
    const idx = this.imports.length;
    this.imports.push({ module, field, params, results });
    return idx;
  }

  importMemory(module: string, field: string, minPages: number): void {
    this.memImport = { module, field, minPages };
  }

  encode(): Uint8Array {
    const out: number[] = [];
    out.push(...WASM_HEADER);

    // Collect unique signatures.
    type Sig = { params: number[]; results: number[] };
    const sigs: Sig[] = [];
    const sigIdx = (s: Sig) => {
      for (let i = 0; i < sigs.length; i++) {
        if (sigEq(sigs[i], s)) return i;
      }
      sigs.push(s);
      return sigs.length - 1;
    };
    for (const imp of this.imports) sigIdx({ params: imp.params, results: imp.results });
    const mainSig = sigIdx({ params: [I32], results: [I32] });

    // Type section.
    {
      const body: number[] = [];
      uleb(sigs.length, body);
      for (const s of sigs) {
        body.push(0x60);
        uleb(s.params.length, body);
        for (const p of s.params) body.push(p);
        uleb(s.results.length, body);
        for (const r of s.results) body.push(r);
      }
      writeSection(out, SEC_TYPE, body);
    }

    // Import section.
    {
      const body: number[] = [];
      const totalImports = this.imports.length + (this.memImport ? 1 : 0);
      uleb(totalImports, body);
      for (const imp of this.imports) {
        const mod = stringBytes(imp.module);
        const fld = stringBytes(imp.field);
        uleb(mod.length, body); body.push(...mod);
        uleb(fld.length, body); body.push(...fld);
        body.push(0x00);                       // import kind: function
        uleb(sigIdx({ params: imp.params, results: imp.results }), body);
      }
      if (this.memImport) {
        const mod = stringBytes(this.memImport.module);
        const fld = stringBytes(this.memImport.field);
        uleb(mod.length, body); body.push(...mod);
        uleb(fld.length, body); body.push(...fld);
        body.push(0x02);                       // import kind: memory
        body.push(0x00);                       // limits: flags = 0 (no max)
        uleb(this.memImport.minPages, body);
      }
      writeSection(out, SEC_IMPORT, body);
    }

    // Function section (declares the local function's type).
    {
      const body: number[] = [];
      uleb(1, body);
      uleb(mainSig, body);
      writeSection(out, SEC_FUNC, body);
    }

    // Export section — export the local function.
    {
      const body: number[] = [];
      uleb(1, body);
      const name = stringBytes(this.exportName);
      uleb(name.length, body); body.push(...name);
      body.push(0x00);                         // function export
      uleb(this.imports.length, body);         // index = importCount + 0
      writeSection(out, SEC_EXPORT, body);
    }

    // Code section.
    {
      const body: number[] = [];
      uleb(1, body);
      const fnBody: number[] = [];
      // Local declarations.
      const decls = this.func.locals.length >> 1;
      uleb(decls, fnBody);
      for (let i = 0; i < decls; i++) {
        uleb(this.func.locals[i * 2], fnBody);
        fnBody.push(this.func.locals[i * 2 + 1]);
      }
      fnBody.push(...this.func.body);
      fnBody.push(OP_END);

      uleb(fnBody.length, body);
      body.push(...fnBody);
      writeSection(out, SEC_CODE, body);
    }

    return new Uint8Array(out);
  }
}

function sigEq(a: { params: number[]; results: number[] }, b: { params: number[]; results: number[] }): boolean {
  if (a.params.length !== b.params.length || a.results.length !== b.results.length) return false;
  for (let i = 0; i < a.params.length; i++) if (a.params[i] !== b.params[i]) return false;
  for (let i = 0; i < a.results.length; i++) if (a.results[i] !== b.results[i]) return false;
  return true;
}

function stringBytes(s: string): number[] {
  const out: number[] = [];
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    out.push(c & 0xFF);
  }
  return out;
}

function writeSection(out: number[], id: number, body: number[]): void {
  out.push(id);
  uleb(body.length, out);
  out.push(...body);
}
