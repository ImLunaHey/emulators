import { useEffect, useState } from 'react';
import { Key, Keypad } from '../io/keypad';
import { loadMap } from './controllerMap';

const STICK_THRESHOLD = 0.5;

interface UseGamepadOptions {
  keypad: Keypad;
  onConnected?: (name: string) => void;
  onDisconnected?: (name: string) => void;
  stickAsDpad?: boolean;
  // Bumping this number forces the rAF loop to reload the binding map
  // from localStorage — used by the ControllerPanel after a remap.
  mapVersion?: number;
}

export function useGamepad({
  keypad, onConnected, onDisconnected, stickAsDpad = false, mapVersion = 0,
}: UseGamepadOptions) {
  const [, force] = useState(0);

  useEffect(() => {
    const heldByPad = new Set<Key>();
    let raf = 0;
    let stop = false;
    // We rebuild the (buttonIndex → Key) lookup whenever the mapping
    // flavor changes (standard vs sony non-standard) — at connect time
    // and whenever the controller is hot-swapped.
    let lookup: Array<{ idx: number; key: Key }> = [];
    let currentMapping = '';

    const rebuildLookup = (mapping: string) => {
      currentMapping = mapping;
      const m = loadMap(mapping);
      lookup = Object.entries(m).map(([k, idx]) => ({ idx: idx as number, key: Number(k) as Key }));
    };

    const press = (k: Key) => {
      if (heldByPad.has(k)) return;
      heldByPad.add(k);
      keypad.press(k);
    };
    const release = (k: Key) => {
      if (!heldByPad.has(k)) return;
      heldByPad.delete(k);
      keypad.release(k);
    };

    const tick = () => {
      if (stop) return;
      raf = requestAnimationFrame(tick);
      const pads = navigator.getGamepads ? navigator.getGamepads() : [];
      let pad: Gamepad | null = null;
      for (const p of pads) { if (p && p.connected) { pad = p; break; } }
      if (!pad) return;

      // Rebuild the lookup if the connected pad's mapping changed (e.g.
      // first detection after the page loaded).
      if (pad.mapping !== currentMapping) rebuildLookup(pad.mapping || 'sony');

      const want = new Set<Key>();
      for (const { idx, key } of lookup) {
        if (pad.buttons[idx] && pad.buttons[idx].pressed) want.add(key);
      }
      // D-pad fallback: many controllers (PS5 on macOS Safari in
      // particular) don't expose the D-pad as four discrete buttons —
      // they expose it as the HID hat at axes 6/7 (X then Y). If those
      // axes deflect past the threshold, synthesize D-pad presses so
      // games actually receive direction input.
      const hx = pad.axes[6] ?? 0;
      const hy = pad.axes[7] ?? 0;
      if (Math.abs(hx) > 0.5 || Math.abs(hy) > 0.5) {
        if (hx < -0.5) want.add(Key.LEFT);
        if (hx >  0.5) want.add(Key.RIGHT);
        if (hy < -0.5) want.add(Key.UP);
        if (hy >  0.5) want.add(Key.DOWN);
      }
      if (stickAsDpad) {
        const ax = pad.axes[0] ?? 0;
        const ay = pad.axes[1] ?? 0;
        if (ax < -STICK_THRESHOLD) want.add(Key.LEFT);
        if (ax >  STICK_THRESHOLD) want.add(Key.RIGHT);
        if (ay < -STICK_THRESHOLD) want.add(Key.UP);
        if (ay >  STICK_THRESHOLD) want.add(Key.DOWN);
      }

      for (const k of want) press(k);
      for (const k of heldByPad) if (!want.has(k)) release(k);
    };
    raf = requestAnimationFrame(tick);

    const onConn = (e: GamepadEvent) => {
      rebuildLookup(e.gamepad.mapping || 'sony');
      onConnected?.(`${e.gamepad.id} [${e.gamepad.mapping || 'non-standard'}]`);
    };
    const onDisc = (e: GamepadEvent) => {
      for (const k of heldByPad) keypad.release(k);
      heldByPad.clear();
      onDisconnected?.(e.gamepad.id);
    };
    window.addEventListener('gamepadconnected', onConn);
    window.addEventListener('gamepaddisconnected', onDisc);

    // Force lookup rebuild now if a pad is already connected (page-reload
    // case — `gamepadconnected` doesn't fire for already-connected pads).
    const initial = navigator.getGamepads?.() ?? [];
    for (const p of initial) if (p && p.connected) { rebuildLookup(p.mapping || 'sony'); break; }

    return () => {
      stop = true;
      cancelAnimationFrame(raf);
      window.removeEventListener('gamepadconnected', onConn);
      window.removeEventListener('gamepaddisconnected', onDisc);
      for (const k of heldByPad) keypad.release(k);
      heldByPad.clear();
    };
    // mapVersion changes → re-effect → re-read mapping from storage.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [keypad, onConnected, onDisconnected, stickAsDpad, mapVersion]);

  // Returned no-op state setter is here in case a caller wants to
  // tickle the hook explicitly without going through mapVersion.
  void force;
}
