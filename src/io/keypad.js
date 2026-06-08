// GBA keypad — 10-bit register; 0 = pressed, 1 = released.
//  A  B  Sel Sta R  L  U  D  Rs Ls
// bit0 1  2   3  4  5  6  7  8  9
// NOT `const enum` so we can do reverse name lookups (`Key[k]`) — the
// gamepad/UI code uses string names ("A", "UP") for accessibility
// labels and remapping.
export var Key;
(function (Key) {
    Key[Key["A"] = 0] = "A";
    Key[Key["B"] = 1] = "B";
    Key[Key["SELECT"] = 2] = "SELECT";
    Key[Key["START"] = 3] = "START";
    Key[Key["RIGHT"] = 4] = "RIGHT";
    Key[Key["LEFT"] = 5] = "LEFT";
    Key[Key["UP"] = 6] = "UP";
    Key[Key["DOWN"] = 7] = "DOWN";
    Key[Key["R"] = 8] = "R";
    Key[Key["L"] = 9] = "L";
})(Key || (Key = {}));
export class Keypad {
    // Live bitmask of pressed keys (1 = pressed). We invert on read to match
    // the GBA's "released" polarity.
    pressed = 0;
    press(k) { this.pressed |= 1 << k; }
    release(k) { this.pressed &= ~(1 << k); }
    read16() { return (~this.pressed) & 0x3FF; }
}
