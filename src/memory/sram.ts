import { SaveBridge } from './bus';

// 32 KB battery-backed SRAM. No state machine, no command sequencing —
// every read returns the stored byte, every write stores one. Used by
// older AGB titles (Mario Kart Super Circuit, Final Fantasy 4/5/6
// Advance, F-Zero Maximum Velocity, lots of homebrew).
//
// On real hardware SRAM is wired 8-bit only; reads through halfword
// and word accesses get the byte broadcast across the wider field.
// That mirror is implemented at the Bus layer (read16/read32 on the
// SRAM region return `(b | b << 8) & 0xFFFF` etc.), so this class
// only sees byte-granular addresses.
export class Sram32K implements SaveBridge {
  data = new Uint8Array(0x8000);
  onChange: (() => void) | null = null;

  loadSave(bytes: Uint8Array): void {
    this.data.fill(0xFF);
    this.data.set(bytes.subarray(0, Math.min(bytes.length, this.data.length)));
  }

  read(addr: number): number {
    return this.data[addr & 0x7FFF];
  }
  write(addr: number, v: number): void {
    this.data[addr & 0x7FFF] = v & 0xFF;
    if (this.onChange) this.onChange();
  }
}
