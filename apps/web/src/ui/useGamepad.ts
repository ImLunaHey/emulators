import { useEffect, useRef, useState, type MutableRefObject } from 'react';
import { Key, Keypad } from '../io/keypad';
import { loadMap } from './controllerMap';
import { padSelect } from './padSelect';

const STICK_THRESHOLD = 0.5;

// Standard-mapping button indices that should also open the menu: the
// guide/PS button (16) and the DualSense touchpad click (17, where the
// browser exposes it).
const MENU_BUTTONS = [16, 17];

export type MenuNav = 'up' | 'down' | 'left' | 'right' | 'select' | 'back';

interface UseGamepadOptions {
  keypad: Keypad;
  onConnected?: (name: string) => void;
  onDisconnected?: (name: string) => void;
  stickAsDpad?: boolean;
  // Bumping this number forces the rAF loop to reload the binding map
  // from localStorage — used by the ControllerPanel after a remap.
  mapVersion?: number;
  // Fired (rising edge) when the menu hotkey is pressed: Start+Select
  // together, or the guide/touchpad button.
  onHotkey?: () => void;
  // When this ref reads true, the pad drives MENU NAVIGATION (onMenuNav)
  // instead of the game — D-pad moves, A selects, B goes back.
  menuOpenRef?: MutableRefObject<boolean>;
  onMenuNav?: (action: MenuNav) => void;
}

export function useGamepad({
  keypad, onConnected, onDisconnected, stickAsDpad = false, mapVersion = 0,
  onHotkey, menuOpenRef, onMenuNav,
}: UseGamepadOptions) {
  const [, force] = useState(0);
  // Hold every callback in a ref so a parent re-render (which hands us
  // fresh inline closures) does NOT tear down and restart the polling
  // loop. Restarting mid-input reset the edge-detection state, which
  // made held D-pad presses re-fire and the menu skip items.
  const onHotkeyRef = useRef(onHotkey); onHotkeyRef.current = onHotkey;
  const onMenuNavRef = useRef(onMenuNav); onMenuNavRef.current = onMenuNav;
  const onConnectedRef = useRef(onConnected); onConnectedRef.current = onConnected;
  const onDisconnectedRef = useRef(onDisconnected); onDisconnectedRef.current = onDisconnected;

  useEffect(() => {
    const heldByPad = new Set<Key>();
    let raf = 0;
    let stop = false;
    let hotPrev = false;   // menu-hotkey edge state
    let navPrev = { up: false, down: false, left: false, right: false, select: false, back: false };
    // (mapping flavor → lookup) cache. Different pads can report
    // different mappings ('standard' vs 'sony'), so we key the
    // buttonIndex→Key lookup by mapping and build each on demand.
    const lookupCache = new Map<string, Array<{ idx: number; key: Key }>>();
    const getLookup = (mapping: string) => {
      let l = lookupCache.get(mapping);
      if (!l) {
        const m = loadMap(mapping);
        l = Object.entries(m).map(([k, idx]) => ({ idx: idx as number, key: Number(k) as Key }));
        lookupCache.set(mapping, l);
      }
      return l;
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

    // Accumulate one pad's currently-pressed GBA keys into `want`.
    const accumulate = (pad: Gamepad, want: Set<Key>) => {
      const lookup = getLookup(pad.mapping || 'sony');
      for (const { idx, key } of lookup) {
        if (pad.buttons[idx] && pad.buttons[idx].pressed) want.add(key);
      }
      // D-pad fallbacks for pads that don't expose it as discrete
      // buttons: a two-axis hat (axes 6/7) and a single-axis HID hat
      // (axis 4 or 9 — PS5 DualSense on macOS Safari).
      const hx = pad.axes[6] ?? 0;
      const hy = pad.axes[7] ?? 0;
      if (Math.abs(hx) > 0.5 || Math.abs(hy) > 0.5) {
        if (hx < -0.5) want.add(Key.LEFT);
        if (hx >  0.5) want.add(Key.RIGHT);
        if (hy < -0.5) want.add(Key.UP);
        if (hy >  0.5) want.add(Key.DOWN);
      }
      for (const hatAxisIdx of [4, 9]) {
        const v = pad.axes[hatAxisIdx];
        if (typeof v !== 'number' || Math.abs(v) > 1.05) continue;
        const pos = Math.round((v + 1) * 7 / 2);
        switch (pos) {
          case 0: want.add(Key.UP); break;
          case 1: want.add(Key.UP); want.add(Key.RIGHT); break;
          case 2: want.add(Key.RIGHT); break;
          case 3: want.add(Key.RIGHT); want.add(Key.DOWN); break;
          case 4: want.add(Key.DOWN); break;
          case 5: want.add(Key.DOWN); want.add(Key.LEFT); break;
          case 6: want.add(Key.LEFT); break;
          case 7: want.add(Key.LEFT); want.add(Key.UP); break;
        }
      }
      if (stickAsDpad) {
        const ax = pad.axes[0] ?? 0;
        const ay = pad.axes[1] ?? 0;
        if (ax < -STICK_THRESHOLD) want.add(Key.LEFT);
        if (ax >  STICK_THRESHOLD) want.add(Key.RIGHT);
        if (ay < -STICK_THRESHOLD) want.add(Key.UP);
        if (ay >  STICK_THRESHOLD) want.add(Key.DOWN);
      }
    };

    const tick = () => {
      if (stop) return;
      raf = requestAnimationFrame(tick);
      const pads = navigator.getGamepads ? navigator.getGamepads() : [];
      const sel = padSelect.get();   // null = any pad
      const want = new Set<Key>();
      let menuBtn = false;
      for (const pad of pads) {
        if (!pad || !pad.connected) continue;
        // When a specific pad is pinned, only it drives input; otherwise
        // every connected pad contributes (union).
        if (sel !== null && pad.id !== sel) continue;
        accumulate(pad, want);
        for (const b of MENU_BUTTONS) if (pad.buttons[b] && pad.buttons[b].pressed) menuBtn = true;
      }

      // Menu hotkey: Start+Select together, or a guide/touchpad button.
      const combo = want.has(Key.START) && want.has(Key.SELECT);
      const hot = combo || menuBtn;
      if (hot && !hotPrev) onHotkeyRef.current?.();
      hotPrev = hot;
      // Reserve the combo for the menu so the game doesn't also see it
      // (avoids triggering in-game Start+Select soft resets).
      if (combo) { want.delete(Key.START); want.delete(Key.SELECT); }

      if (menuOpenRef?.current) {
        // Menu open: the pad navigates the launcher, not the game.
        for (const k of heldByPad) keypad.release(k);
        heldByPad.clear();
        const nav = {
          up: want.has(Key.UP), down: want.has(Key.DOWN),
          left: want.has(Key.LEFT), right: want.has(Key.RIGHT),
          select: want.has(Key.A), back: want.has(Key.B),
        };
        if (nav.up && !navPrev.up) onMenuNavRef.current?.('up');
        if (nav.down && !navPrev.down) onMenuNavRef.current?.('down');
        if (nav.left && !navPrev.left) onMenuNavRef.current?.('left');
        if (nav.right && !navPrev.right) onMenuNavRef.current?.('right');
        if (nav.select && !navPrev.select) onMenuNavRef.current?.('select');
        if (nav.back && !navPrev.back) onMenuNavRef.current?.('back');
        navPrev = nav;
        return;
      }
      navPrev = { up: false, down: false, left: false, right: false, select: false, back: false };

      for (const k of want) press(k);
      for (const k of heldByPad) if (!want.has(k)) release(k);
    };
    raf = requestAnimationFrame(tick);

    const onConn = (e: GamepadEvent) => {
      onConnectedRef.current?.(`${e.gamepad.id} [${e.gamepad.mapping || 'non-standard'}]`);
    };
    const onDisc = (e: GamepadEvent) => {
      // Releasing every held key is safe: the next tick re-derives the
      // pressed set from whatever pads remain.
      for (const k of heldByPad) keypad.release(k);
      heldByPad.clear();
      // If the pinned pad vanished, fall back to "any" so input keeps
      // working instead of silently dying.
      if (padSelect.get() === e.gamepad.id) padSelect.set(null);
      onDisconnectedRef.current?.(e.gamepad.id);
    };
    window.addEventListener('gamepadconnected', onConn);
    window.addEventListener('gamepaddisconnected', onDisc);

    return () => {
      stop = true;
      cancelAnimationFrame(raf);
      window.removeEventListener('gamepadconnected', onConn);
      window.removeEventListener('gamepaddisconnected', onDisc);
      for (const k of heldByPad) keypad.release(k);
      heldByPad.clear();
    };
    // Only restart on real changes (callbacks are read through refs, so
    // a parent re-render no longer churns this loop). mapVersion bump
    // rebuilds the lookup cache after a remap.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [keypad, stickAsDpad, mapVersion]);

  // Returned no-op state setter is here in case a caller wants to
  // tickle the hook explicitly without going through mapVersion.
  void force;
}
