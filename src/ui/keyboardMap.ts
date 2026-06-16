// Keyboard → GBA-key binding store. Mirrors controllerMap.ts: a default
// map (matching the previously-hardcoded KEY_MAP in PlayerPage), plus
// load/save/reset to localStorage. The persisted form is
// { [gbaKeyEnum:number]: KeyboardEvent.key }; the player consumes the
// inverse Record<KeyboardEvent.key, Key>.

import { Key } from '../io/keypad';

// The persisted map: each GBA key → a single KeyboardEvent.key string.
// Defaults match the original hardcoded bindings (arrows · z/x · a/s ·
// enter/shift). We store one canonical key per GBA key; lookups in the
// player are case-insensitive so "z" also catches "Z" (see below).
export const DEFAULT_KEYBOARD_MAP: Record<number, string> = {
  [Key.UP]: 'ArrowUp',
  [Key.DOWN]: 'ArrowDown',
  [Key.LEFT]: 'ArrowLeft',
  [Key.RIGHT]: 'ArrowRight',
  [Key.A]: 'z',
  [Key.B]: 'x',
  [Key.L]: 'a',
  [Key.R]: 's',
  [Key.START]: 'Enter',
  [Key.SELECT]: 'Shift',
};

const STORAGE_KEY = 'emulators:keymap';

// Persisted GBA-key → keyboard-key map (or defaults if none/corrupt).
export function loadKeyboardMapRaw(): Record<number, string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, string>;
      const out: Record<number, string> = {};
      for (const [k, v] of Object.entries(parsed)) {
        if (typeof v === 'string' && v.length > 0) out[Number(k)] = v;
      }
      return out;
    }
  } catch { /* ignore */ }
  return { ...DEFAULT_KEYBOARD_MAP };
}

// Inverse lookup for the player: KeyboardEvent.key → GBA Key. We add a
// case-folded alias (e.g. "Z" alongside "z") so a single-character bind
// works regardless of Shift/CapsLock state, matching the old behaviour.
export function loadKeyboardMap(): Record<string, Key> {
  const raw = loadKeyboardMapRaw();
  const out: Record<string, Key> = {};
  for (const [k, evKey] of Object.entries(raw)) {
    const gbaKey = Number(k) as Key;
    out[evKey] = gbaKey;
    if (evKey.length === 1) {
      const lower = evKey.toLowerCase();
      const upper = evKey.toUpperCase();
      if (lower !== evKey) out[lower] = gbaKey;
      if (upper !== evKey) out[upper] = gbaKey;
    }
  }
  return out;
}

// Save a GBA-key → keyboard-key map. Binding a keyboard key that's
// already in use elsewhere is resolved by the caller (ControllerPanel)
// before this is called.
export function saveKeyboardMap(map: Record<number, string>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch { /* ignore */ }
}

export function resetKeyboardMap(): Record<number, string> {
  try { localStorage.removeItem(STORAGE_KEY); } catch { /* ignore */ }
  return { ...DEFAULT_KEYBOARD_MAP };
}

// Human-readable label for a KeyboardEvent.key value (e.g. " " → "Space").
export function keyLabel(evKey: string | undefined): string {
  if (evKey === undefined) return '— unbound —';
  if (evKey === ' ') return 'Space';
  if (evKey.length === 1) return evKey.toUpperCase();
  return evKey;
}
