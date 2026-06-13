// GBA keypad — 10-bit register; 0 = pressed, 1 = released.
//  A  B  Sel Sta R  L  U  D  Rs Ls
// bit0 1  2   3  4  5  6  7  8  9

// NOT `const enum` so we can do reverse name lookups (`Key[k]`) — the
// gamepad/UI code uses string names ("A", "UP") for accessibility
// labels and remapping.
export enum Key {
  A = 0, B = 1, SELECT = 2, START = 3,
  RIGHT = 4, LEFT = 5, UP = 6, DOWN = 7,
  R = 8, L = 9,
}

export class Keypad {
  // Logical held bitmask (1 = held). Inverted on read to match the GBA's
  // "released" polarity. The UI highlight reads this directly so a held
  // turbo button stays lit even while it autofires.
  pressed = 0;
  // Keys that autofire while held: the game sees them pressed only on the
  // "on" phase, which tickTurbo() flips once per emulated frame (~30 Hz).
  turboMask = 0;
  // Public so the wasm-core adapter's structural keypad object remains
  // assignable to this `Keypad` type (TS treats a private member as nominal).
  turboPhase = 0;

  press(k: Key) { this.pressed |= 1 << k; }
  release(k: Key) { this.pressed &= ~(1 << k); }

  // Advance the autofire phase — call once per emulated frame.
  tickTurbo() { this.turboPhase ^= 1; }

  read16(): number {
    let effective = this.pressed;
    // On the "off" phase, drop the held turbo keys so they read released.
    if (this.turboMask && this.turboPhase === 0) effective &= ~this.turboMask;
    return (~effective) & 0x3FF;
  }
}
