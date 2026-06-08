// Controller → GBA-key binding store. The default mapping depends on
// whether the browser reports the pad as W3C "standard" or Sony-native
// non-standard; both defaults are below. Users can overwrite a binding
// by clicking a GBA row in the panel and pressing a button.
import { Key } from '../io/keypad';
// All ten GBA keys, in the order the panel displays them.
export const GBA_KEYS = [
    { key: Key.UP, name: 'D-pad Up' },
    { key: Key.DOWN, name: 'D-pad Down' },
    { key: Key.LEFT, name: 'D-pad Left' },
    { key: Key.RIGHT, name: 'D-pad Right' },
    { key: Key.A, name: 'A' },
    { key: Key.B, name: 'B' },
    { key: Key.L, name: 'L' },
    { key: Key.R, name: 'R' },
    { key: Key.START, name: 'Start' },
    { key: Key.SELECT, name: 'Select' },
];
// Labels for the W3C "standard" mapping (Xbox-derived).
const STANDARD_LABELS = {
    0: 'A', // bottom of right cluster (Cross on PS5)
    1: 'B', // right (Circle)
    2: 'X', // left (Square)
    3: 'Y', // top (Triangle)
    4: 'L1',
    5: 'R1',
    6: 'L2',
    7: 'R2',
    8: 'Select / Share',
    9: 'Start / Options',
    10: 'L3',
    11: 'R3',
    12: 'D-pad Up',
    13: 'D-pad Down',
    14: 'D-pad Left',
    15: 'D-pad Right',
    16: 'Home / PS',
};
// Labels for Sony's non-standard order (Square=0..Triangle=3).
const SONY_LABELS = {
    0: 'Square',
    1: 'Cross',
    2: 'Circle',
    3: 'Triangle',
    4: 'L1',
    5: 'R1',
    6: 'L2',
    7: 'R2',
    8: 'Share',
    9: 'Options',
    10: 'L3',
    11: 'R3',
    12: 'PS',
    13: 'Touchpad',
    14: 'D-pad Up',
    15: 'D-pad Down',
    16: 'D-pad Left',
    17: 'D-pad Right',
};
export function labelFor(buttonIndex, mapping) {
    const table = mapping === 'standard' ? STANDARD_LABELS : SONY_LABELS;
    return table[buttonIndex] ?? `Button ${buttonIndex}`;
}
// Defaults. Cross → A, Circle → B in both layouts.
export const DEFAULT_STANDARD = {
    [Key.A]: 0, [Key.B]: 1,
    [Key.L]: 4, [Key.R]: 5,
    [Key.SELECT]: 8, [Key.START]: 9,
    [Key.UP]: 12, [Key.DOWN]: 13, [Key.LEFT]: 14, [Key.RIGHT]: 15,
};
export const DEFAULT_SONY = {
    [Key.A]: 1, [Key.B]: 2,
    [Key.L]: 6, [Key.R]: 7,
    [Key.SELECT]: 8, [Key.START]: 9,
    [Key.UP]: 14, [Key.DOWN]: 15, [Key.LEFT]: 16, [Key.RIGHT]: 17,
};
const STORAGE_KEY = 'gba-recomp:controllerMap';
export function loadMap(mapping) {
    try {
        const raw = localStorage.getItem(`${STORAGE_KEY}:${mapping || 'sony'}`);
        if (raw)
            return JSON.parse(raw);
    }
    catch { /* ignore */ }
    return mapping === 'standard' ? { ...DEFAULT_STANDARD } : { ...DEFAULT_SONY };
}
export function saveMap(mapping, map) {
    localStorage.setItem(`${STORAGE_KEY}:${mapping || 'sony'}`, JSON.stringify(map));
}
export function resetMap(mapping) {
    localStorage.removeItem(`${STORAGE_KEY}:${mapping || 'sony'}`);
    return mapping === 'standard' ? { ...DEFAULT_STANDARD } : { ...DEFAULT_SONY };
}
