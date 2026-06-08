import { SaveBridge } from './bus';

// Flash 128 KB save chip emulation — minimal state machine that's enough
// for FireRed's save layer (Macronix MX29L1000, manufacturer 0xC2, device 0x09).
//
// FireRed probes the chip by issuing the standard Atmel/SST/Macronix command
// sequence at 0xE005555/0xE002AAA. We implement: read mode, ID mode, sector
// erase, byte program, and bank switch (BankSelect cmd 0xB0 at 0xE000000).
//
// The chip is split into two 64 KB banks; only the active bank is visible
// through the 0x0E000000 window.

export enum FlashCmd {
  Normal,
  AwaitFirst,
  AwaitSecond,
  Identify,
  EraseAwaitFirst,
  EraseAwaitSecond,
  EraseSector,
  Program,
  BankSelect,
}

export class Flash128K implements SaveBridge {
  data = new Uint8Array(0x20000);        // 128 KB, 2 banks of 64 KB
  state: FlashCmd = FlashCmd.Normal;
  idMode = false;
  bank = 0;
  // Manufacturer/device pair for Macronix MX29L010 — the only 128 KB
  // Flash variant whose command set our chip implementation matches.
  // (Lying about being Sanyo would route the game's save driver through
  // Sanyo-specific commands we don't handle.)
  static readonly ID_MAKER = 0xC2;
  static readonly ID_DEVICE = 0x09;

  // Called whenever the chip data changes; the host wires this to persist
  // to localStorage / IndexedDB.
  onChange: (() => void) | null = null;

  // Load save data from a serialized 128 KB buffer (or shorter — padded
  // with 0xFF). Used to restore the game's save on page load.
  loadSave(bytes: Uint8Array): void {
    this.data.fill(0xFF);
    this.data.set(bytes.subarray(0, Math.min(bytes.length, this.data.length)));
  }

  read(addr: number): number {
    addr &= 0xFFFF;
    if (this.idMode) {
      if (addr === 0) return Flash128K.ID_MAKER;
      if (addr === 1) return Flash128K.ID_DEVICE;
    }
    return this.data[(this.bank << 16) | addr];
  }

  write(addr: number, v: number): void {
    addr &= 0xFFFF; v &= 0xFF;

    switch (this.state) {
      case FlashCmd.Program:
        this.data[(this.bank << 16) | addr] = v;
        this.state = FlashCmd.Normal;
        if (this.onChange) this.onChange();
        return;

      case FlashCmd.BankSelect:
        this.bank = v & 1;
        this.state = FlashCmd.Normal;
        return;

      case FlashCmd.EraseSector:
        if ((addr & 0xFFF) === 0 && v === 0x30) {
          const base = (this.bank << 16) | (addr & 0xF000);
          this.data.fill(0xFF, base, base + 0x1000);
          this.state = FlashCmd.Normal;
          if (this.onChange) this.onChange();
          return;
        }
        this.state = FlashCmd.Normal;
        return;
    }

    // Command sequencing on 0x5555 / 0x2AAA.
    if (addr === 0x5555 && v === 0xAA) {
      this.state = FlashCmd.AwaitFirst;
      return;
    }
    if (this.state === FlashCmd.AwaitFirst && addr === 0x2AAA && v === 0x55) {
      this.state = FlashCmd.AwaitSecond;
      return;
    }
    if (this.state === FlashCmd.AwaitSecond && addr === 0x5555) {
      switch (v) {
        case 0x90: this.idMode = true; this.state = FlashCmd.Normal; return;
        case 0xF0: this.idMode = false; this.state = FlashCmd.Normal; return;
        case 0x80: this.state = FlashCmd.EraseAwaitFirst; return;
        case 0xA0: this.state = FlashCmd.Program; return;
        case 0xB0: this.state = FlashCmd.BankSelect; return;
        case 0x10:
          this.data.fill(0xFF);
          this.state = FlashCmd.Normal;
          if (this.onChange) this.onChange();
          return;
      }
    }
    if (this.state === FlashCmd.EraseAwaitFirst && addr === 0x5555 && v === 0xAA) {
      this.state = FlashCmd.EraseAwaitSecond; return;
    }
    if (this.state === FlashCmd.EraseAwaitSecond && addr === 0x2AAA && v === 0x55) {
      this.state = FlashCmd.EraseSector; return;
    }
    this.state = FlashCmd.Normal;
  }
}
