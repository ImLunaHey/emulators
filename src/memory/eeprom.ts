import { SaveBridge } from './bus';

// EEPROM bit-serial state machine.
//
// EEPROM is wired into the cart bus at 0x0D000000-0x0DFFFFFF. The host
// accesses it via DMA3 to/from 0x0DFFFF00; each 16-bit DMA transfer
// carries exactly ONE bit, in bit 0 of the value.
//
// Commands (MSB first):
//   READ   1 1 [N addr bits] [0 terminator]            (host then reads 4 zero bits + 64 data bits)
//   WRITE  1 0 [N addr bits] [64 data bits] [0 term]
//
// N = 6 for the 512-byte chip, 14 for the 8K chip (only the low 10 of
// those 14 are meaningful — high 4 ignored). EEPROM_V signature alone
// doesn't say which size; we default to 8K which is what every modern
// title uses, including Minish Cap.

export class Eeprom implements SaveBridge {
  data: Uint8Array;
  onChange: (() => void) | null = null;

  private addrBits: number;
  // Total bits received so far in the current command. Reset to 0 when
  // we either finish a command or abort one.
  private cmdLen = 0;
  // For the first two bits we just remember them in `cmdBits`; the
  // address is collected separately into `addr`.
  private cmdBits = 0;
  private isRead = false;
  private addr = 0;
  private writeBuf = new Uint8Array(8);

  // Read-response state. After a successful READ command we feed bits
  // back: 4 zero bits followed by 64 data bits (MSB first).
  private inResponse = false;
  private respIdx = 0;

  constructor(size: 512 | 8192 = 8192) {
    this.data = new Uint8Array(size);
    this.data.fill(0xFF);
    this.addrBits = size === 512 ? 6 : 14;
  }

  loadSave(bytes: Uint8Array): void {
    this.data.fill(0xFF);
    this.data.set(bytes.slice(0, this.data.length));
  }

  // Returns the next response bit in bit 0. When not in response phase
  // we return 1 (open-bus pull-up on real hardware).
  read(_addr: number): number {
    if (!this.inResponse) return 1;
    let bit = 0;
    if (this.respIdx >= 4) {
      const dataBit = this.respIdx - 4;        // 0..63
      const block = this.addr & ((this.data.length / 8) - 1);
      const byteOff = block * 8 + (dataBit >>> 3);
      const bitOff  = 7 - (dataBit & 7);
      bit = (this.data[byteOff] >> bitOff) & 1;
    }
    this.respIdx++;
    if (this.respIdx >= 68) {
      this.inResponse = false;
      this.respIdx = 0;
    }
    return bit;
  }

  // Consume the next command bit (bit 0 of v).
  write(_addr: number, v: number): void {
    const bit = v & 1;
    this.cmdLen++;

    // Bit 1: must be 1 to start a command.
    if (this.cmdLen === 1) {
      if (bit === 1) this.cmdBits = 1;
      else this.cmdLen = 0;
      return;
    }
    // Bit 2: 1 = read, 0 = write.
    if (this.cmdLen === 2) {
      this.isRead = (bit === 1);
      this.cmdBits = (this.cmdBits << 1) | bit;
      this.addr = 0;
      return;
    }
    // Bits 3..2+addrBits: address (MSB first).
    if (this.cmdLen <= 2 + this.addrBits) {
      this.addr = (this.addr << 1) | bit;
      return;
    }
    // For READ: one more bit (terminator, ignored) and we transition to
    // response. The host typically sends a 0 here. Either way, we move
    // on after one extra bit.
    if (this.isRead) {
      this.inResponse = true;
      this.respIdx = 0;
      this.cmdLen = 0;
      this.cmdBits = 0;
      return;
    }
    // For WRITE: next 64 bits are data, bit-by-bit, then 1 terminator.
    const dataBit = this.cmdLen - (2 + this.addrBits) - 1;     // 0..64
    if (dataBit < 64) {
      const byteOff = dataBit >>> 3;
      const bitOff  = 7 - (dataBit & 7);
      if (bit) this.writeBuf[byteOff] |=  (1 << bitOff);
      else     this.writeBuf[byteOff] &= ~(1 << bitOff);
      return;
    }
    // 65th post-address bit = terminator. Commit the 8-byte block.
    if (dataBit === 64) {
      const block = this.addr & ((this.data.length / 8) - 1);
      this.data.set(this.writeBuf, block * 8);
      if (this.onChange) this.onChange();
      this.cmdLen = 0;
      this.cmdBits = 0;
      this.writeBuf.fill(0);
    }
  }
}
