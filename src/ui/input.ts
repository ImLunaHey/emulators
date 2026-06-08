import { Key, Keypad } from '../io/keypad';

const MAP: Record<string, Key> = {
  ArrowUp: Key.UP,
  ArrowDown: Key.DOWN,
  ArrowLeft: Key.LEFT,
  ArrowRight: Key.RIGHT,
  z: Key.A, Z: Key.A,
  x: Key.B, X: Key.B,
  a: Key.L, A: Key.L,
  s: Key.R, S: Key.R,
  Enter: Key.START,
  Shift: Key.SELECT,
};

const BTN_TO_KEY: Record<string, Key> = {
  UP: Key.UP,
  DOWN: Key.DOWN,
  LEFT: Key.LEFT,
  RIGHT: Key.RIGHT,
  A: Key.A,
  B: Key.B,
  L: Key.L,
  R: Key.R,
  START: Key.START,
  SELECT: Key.SELECT,
};

export function bindKeys(keypad: Keypad): void {
  window.addEventListener('keydown', (e) => {
    const k = MAP[e.key];
    if (k !== undefined) { keypad.press(k); e.preventDefault(); }
  });
  window.addEventListener('keyup', (e) => {
    const k = MAP[e.key];
    if (k !== undefined) { keypad.release(k); e.preventDefault(); }
  });

  // Bind on-screen gamepad buttons. Each `.gp-btn[data-key=...]` becomes
  // a press-and-hold control. Works for mouse + touch + pointer.
  const buttons = document.querySelectorAll<HTMLButtonElement>('.gp-btn[data-key]');
  for (const btn of Array.from(buttons)) {
    const name = btn.dataset.key!;
    const key = BTN_TO_KEY[name];
    if (key === undefined) continue;
    const press = (e: Event) => {
      e.preventDefault();
      keypad.press(key);
      btn.classList.add('pressed');
    };
    const release = (e: Event) => {
      e.preventDefault();
      keypad.release(key);
      btn.classList.remove('pressed');
    };
    btn.addEventListener('pointerdown', press);
    btn.addEventListener('pointerup', release);
    btn.addEventListener('pointercancel', release);
    btn.addEventListener('pointerleave', release);
  }
}
