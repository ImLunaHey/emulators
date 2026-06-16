import { useEffect } from 'react';
import { Keypad, Key } from '../io/keypad';

// Watches the Keypad's `pressed` bitmask each animation frame and
// toggles `.pressed` on each on-screen gamepad button so they light up
// for ALL input sources (keyboard, touch, controller) — not just the
// pointer-down path that originally drove the class. Direct DOM
// manipulation so a 60Hz tick doesn't spam React renders.
export function useKeypadHighlight(keypad: Keypad) {
  useEffect(() => {
    let raf = 0;
    let stop = false;
    let lastMask = -1;

    const tick = () => {
      if (stop) return;
      raf = requestAnimationFrame(tick);
      const mask = keypad.pressed;
      if (mask === lastMask) return;
      lastMask = mask;
      const btns = document.querySelectorAll<HTMLButtonElement>('.gp-btn[data-key]');
      for (const b of Array.from(btns)) {
        const name = b.dataset.key!;
        const k = (Key as any)[name];
        if (typeof k !== 'number') continue;
        const isDown = (mask & (1 << k)) !== 0;
        b.classList.toggle('pressed', isDown);
      }
    };
    raf = requestAnimationFrame(tick);
    return () => { stop = true; cancelAnimationFrame(raf); };
  }, [keypad]);
}
