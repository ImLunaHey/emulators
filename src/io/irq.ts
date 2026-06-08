// IE/IF/IME — interrupt controller.
// IE bits (matching hardware):
//   0 VBlank, 1 HBlank, 2 VCount, 3 Timer0, 4 Timer1, 5 Timer2, 6 Timer3,
//   7 SIO,    8 DMA0,   9 DMA1, 10 DMA2,  11 DMA3, 12 Keypad, 13 GamePak

export const IRQ_VBLANK   = 1 << 0;
export const IRQ_HBLANK   = 1 << 1;
export const IRQ_VCOUNT   = 1 << 2;
export const IRQ_TIMER0   = 1 << 3;
export const IRQ_TIMER1   = 1 << 4;
export const IRQ_TIMER2   = 1 << 5;
export const IRQ_TIMER3   = 1 << 6;
export const IRQ_SIO      = 1 << 7;
export const IRQ_DMA0     = 1 << 8;
export const IRQ_DMA1     = 1 << 9;
export const IRQ_DMA2     = 1 << 10;
export const IRQ_DMA3     = 1 << 11;
export const IRQ_KEYPAD   = 1 << 12;
export const IRQ_GAMEPAK  = 1 << 13;

export class Irq {
  ie = 0;
  iflag = 0;
  ime = 0;
  // Cached pending bit so the hot CPU loop doesn't have to recompute it
  // every step. Setters keep it in sync.
  cachedPending = false;

  raise(bits: number): void {
    this.iflag = (this.iflag | bits) & 0x3FFF;
    this.cachedPending = (this.ime & 1) !== 0 && (this.ie & this.iflag) !== 0;
  }

  setIe(v: number): void {
    this.ie = v & 0x3FFF;
    this.cachedPending = (this.ime & 1) !== 0 && (this.ie & this.iflag) !== 0;
  }

  setIme(v: number): void {
    this.ime = v & 1;
    this.cachedPending = (this.ime & 1) !== 0 && (this.ie & this.iflag) !== 0;
  }

  // The CPU's irqLine should be driven by (IME & 1) && (IE & IF) and the
  // CPU's own CPSR.I being clear. The IO bridge polls this.
  pending(): boolean {
    return this.cachedPending;
  }

  // Writes to IF clear the corresponding bits.
  ackWrite16(v: number): void {
    this.iflag &= ~(v & 0x3FFF);
    this.cachedPending = (this.ime & 1) !== 0 && (this.ie & this.iflag) !== 0;
  }
}
