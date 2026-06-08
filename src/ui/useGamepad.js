import { useEffect, useState } from 'react';
import { Key } from '../io/keypad';
import { loadMap } from './controllerMap';
const STICK_THRESHOLD = 0.5;
export function useGamepad({ keypad, onConnected, onDisconnected, stickAsDpad = false, mapVersion = 0, }) {
    const [, force] = useState(0);
    useEffect(() => {
        const heldByPad = new Set();
        let raf = 0;
        let stop = false;
        // We rebuild the (buttonIndex → Key) lookup whenever the mapping
        // flavor changes (standard vs sony non-standard) — at connect time
        // and whenever the controller is hot-swapped.
        let lookup = [];
        let currentMapping = '';
        const rebuildLookup = (mapping) => {
            currentMapping = mapping;
            const m = loadMap(mapping);
            lookup = Object.entries(m).map(([k, idx]) => ({ idx: idx, key: Number(k) }));
        };
        const press = (k) => {
            if (heldByPad.has(k))
                return;
            heldByPad.add(k);
            keypad.press(k);
        };
        const release = (k) => {
            if (!heldByPad.has(k))
                return;
            heldByPad.delete(k);
            keypad.release(k);
        };
        const tick = () => {
            if (stop)
                return;
            raf = requestAnimationFrame(tick);
            const pads = navigator.getGamepads ? navigator.getGamepads() : [];
            let pad = null;
            for (const p of pads) {
                if (p && p.connected) {
                    pad = p;
                    break;
                }
            }
            if (!pad)
                return;
            // Rebuild the lookup if the connected pad's mapping changed (e.g.
            // first detection after the page loaded).
            if (pad.mapping !== currentMapping)
                rebuildLookup(pad.mapping || 'sony');
            const want = new Set();
            for (const { idx, key } of lookup) {
                if (pad.buttons[idx] && pad.buttons[idx].pressed)
                    want.add(key);
            }
            // D-pad fallback for controllers that don't expose the D-pad as
            // discrete buttons.  Two encodings in the wild:
            //
            //   1) Two-axis hat (X then Y) at axes 6 + 7. We accept any pair
            //      where either axis deflects past ±0.5.
            //
            //   2) Single-axis HID hat at axis 4 or 9 (PS5 DualSense on
            //      macOS Safari does this). One axis encodes all 8 positions
            //      via quantized values: -1.00=Up, -0.71=UR, -0.43=R,
            //      -0.14=DR, 0.14=D, 0.43=DL, 0.71=L, 1.00=UL, with idle
            //      sitting around ±1.29 (above the valid range). We decode
            //      to the 0-7 position bucket and dispatch up to two
            //      directions for the diagonals.
            const hx = pad.axes[6] ?? 0;
            const hy = pad.axes[7] ?? 0;
            if (Math.abs(hx) > 0.5 || Math.abs(hy) > 0.5) {
                if (hx < -0.5)
                    want.add(Key.LEFT);
                if (hx > 0.5)
                    want.add(Key.RIGHT);
                if (hy < -0.5)
                    want.add(Key.UP);
                if (hy > 0.5)
                    want.add(Key.DOWN);
            }
            // Decode single-axis HID hat at common slots (4, 9). Idle value
            // is out-of-range (|v| > 1.05); skip those.
            for (const hatAxisIdx of [4, 9]) {
                const v = pad.axes[hatAxisIdx];
                if (typeof v !== 'number' || Math.abs(v) > 1.05)
                    continue;
                // Map [-1, 1] → 0..7 (8 quantized HID hat positions).
                const pos = Math.round((v + 1) * 7 / 2);
                switch (pos) {
                    case 0:
                        want.add(Key.UP);
                        break;
                    case 1:
                        want.add(Key.UP);
                        want.add(Key.RIGHT);
                        break;
                    case 2:
                        want.add(Key.RIGHT);
                        break;
                    case 3:
                        want.add(Key.RIGHT);
                        want.add(Key.DOWN);
                        break;
                    case 4:
                        want.add(Key.DOWN);
                        break;
                    case 5:
                        want.add(Key.DOWN);
                        want.add(Key.LEFT);
                        break;
                    case 6:
                        want.add(Key.LEFT);
                        break;
                    case 7:
                        want.add(Key.LEFT);
                        want.add(Key.UP);
                        break;
                }
            }
            if (stickAsDpad) {
                const ax = pad.axes[0] ?? 0;
                const ay = pad.axes[1] ?? 0;
                if (ax < -STICK_THRESHOLD)
                    want.add(Key.LEFT);
                if (ax > STICK_THRESHOLD)
                    want.add(Key.RIGHT);
                if (ay < -STICK_THRESHOLD)
                    want.add(Key.UP);
                if (ay > STICK_THRESHOLD)
                    want.add(Key.DOWN);
            }
            for (const k of want)
                press(k);
            for (const k of heldByPad)
                if (!want.has(k))
                    release(k);
        };
        raf = requestAnimationFrame(tick);
        const onConn = (e) => {
            rebuildLookup(e.gamepad.mapping || 'sony');
            onConnected?.(`${e.gamepad.id} [${e.gamepad.mapping || 'non-standard'}]`);
        };
        const onDisc = (e) => {
            for (const k of heldByPad)
                keypad.release(k);
            heldByPad.clear();
            onDisconnected?.(e.gamepad.id);
        };
        window.addEventListener('gamepadconnected', onConn);
        window.addEventListener('gamepaddisconnected', onDisc);
        // Force lookup rebuild now if a pad is already connected (page-reload
        // case — `gamepadconnected` doesn't fire for already-connected pads).
        const initial = navigator.getGamepads?.() ?? [];
        for (const p of initial)
            if (p && p.connected) {
                rebuildLookup(p.mapping || 'sony');
                break;
            }
        return () => {
            stop = true;
            cancelAnimationFrame(raf);
            window.removeEventListener('gamepadconnected', onConn);
            window.removeEventListener('gamepaddisconnected', onDisc);
            for (const k of heldByPad)
                keypad.release(k);
            heldByPad.clear();
        };
        // mapVersion changes → re-effect → re-read mapping from storage.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [keypad, onConnected, onDisconnected, stickAsDpad, mapVersion]);
    // Returned no-op state setter is here in case a caller wants to
    // tickle the hook explicitly without going through mapVersion.
    void force;
}
