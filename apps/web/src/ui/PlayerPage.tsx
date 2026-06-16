import { useEffect, useRef, useState } from 'react';
import { Screen } from './Screen';
import { Gamepad } from './Gamepad';
import { LogPane } from './LogPane';
import { useGamepad, type MenuNav } from './useGamepad';
import { useKeypadHighlight } from './useKeypadHighlight';
import { ControllerPanel } from './ControllerPanel';
import { DebugPanel } from './DebugPanel';
import { ErrorBoundary } from './ErrorBoundary';
import { CheatsPanel, loadCheatsFor } from './CheatsPanel';
import { LinkPanel } from './LinkPanel';
import { SettingsPanel } from './SettingsPanel';
import { SaveStatesPanel, captureThumb } from './SaveStatesPanel';
import { Modal } from './Modal';
import { usePersistedBool, usePersistedNumber } from './usePersistedState';
import { putState, getStateBlob } from './stateStore';
import type { Cheat } from '../io/cheats';
import { getRomBytes, setSelectedRom, type RomMeta } from './romStore';
import { useEmu } from './EmuContext';
import { useConfirm } from './ConfirmModal';
import { useToast, type ToastKind } from './Toast';
import { saveState, loadState } from '../savestate';
import { loadKeyboardMap } from './keyboardMap';

function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
function base64ToBytes(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// Set an input/select value through the native prototype setter so a
// controlled React component still sees the change (React installs its
// own `value` setter + tracker; assigning `el.value` directly bypasses
// onChange). Used by controller-driven slider/select adjustment.
function setNativeValue(el: HTMLInputElement | HTMLSelectElement, value: string): void {
  const desc = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value');
  if (desc?.set) desc.set.call(el, value);
  else el.value = value;
}

// Rewind ring: snapshot every CAPTURE_MS of play, keep the last MAX.
// At 120 ms × 100 that's ~12 s of scrub-back. Each snapshot is a full
// savestate (~400 KB+), so this trades memory for the feature — hence
// it's opt-in.
const REWIND_CAPTURE_MS = 120;
const REWIND_MAX = 100;
const REWIND_STEP_MS = 50;   // playback tick while rewinding

// Auto-save writes to a reserved slot, distinct from quick (0) and the
// manual grid (1..6), every AUTO_SAVE_MS while running.
const AUTO_SLOT = 99;
const AUTO_SAVE_MS = 30000;

// Boots the ROM identified by `romId`, then renders the Screen + controls +
// modal panels. `onExit` returns to the home launcher (the Home button) and is
// also the fallback if the ROM can't be loaded.
export function PlayerPage({ romId, onExit }: { romId: string; onExit: () => void }) {
  const { emu, audio } = useEmu();

  const [paused, setPaused] = useState(false);
  const [stats, setStats] = useState('— fps · — Mhz');
  const [log, setLog] = useState<string[]>([]);
  const [headerInfo, setHeaderInfo] = useState<string>('');
  const [showCp, setShowCp] = useState(false);
  const [showDebug, setShowDebug] = useState(false);
  const [showCheats, setShowCheats] = useState(false);
  const [showLink, setShowLink] = useState(false);
  const [showLog, setShowLog] = useState(false);
  const [showSaveMenu, setShowSaveMenu] = useState(false);
  const [showMore, setShowMore] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showStates, setShowStates] = useState(false);
  // Controller-driven menu launcher (opened by a hotkey combo / touchpad).
  const [showMenu, setShowMenu] = useState(false);
  // Video recording (MediaRecorder over the canvas).
  const [recording, setRecording] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recChunksRef = useRef<Blob[]>([]);
  // Base emulation speed (set via Settings / fast-forward toggle) and a
  // momentary turbo while a key is held. Effective speed feeds Screen.
  const [speed, setSpeed] = useState(1);
  const [turbo, setTurbo] = useState(false);
  const effectiveSpeed = turbo ? Math.max(4, speed) : speed;
  // Persisted video/feature settings (localStorage-backed via the hook).
  const [smooth, changeSmooth] = usePersistedBool('emulators:smooth', false);
  const [crt, changeCrt] = usePersistedBool('emulators:crt', false);
  const [colorCorrect, changeColorCorrect] = usePersistedBool('emulators:colorcorrect', false);
  const [autoResumeOn, changeAutoResume] = usePersistedBool('emulators:autoresume', false);
  // Turbo/autofire: bitmask of GBA keys that auto-fire while held.
  const [turboMask, changeTurbo] = usePersistedNumber('emulators:turbo', 0);
  useEffect(() => { emu.keypad.turboMask = turboMask; }, [emu, turboMask]);
  // Rewind: opt-in ring of recent savestates the user can scrub back
  // through. rewinding pauses the forward loop and drives frames from it.
  const [rewindOn, setRewindOn] = usePersistedBool('emulators:rewind', false);
  const rewindBufRef = useRef<Uint8Array[]>([]);
  const [rewinding, setRewinding] = useState(false);
  const changeRewind = (v: boolean) => {
    setRewindOn(v);
    if (!v) { rewindBufRef.current = []; setRewinding(false); }
  };
  const effectivePaused = paused || rewinding;
  const [mapVersion, setMapVersion] = useState(0);
  const [cheats, setCheatsState] = useState<Cheat[]>([]);
  const setCheats = (next: Cheat[]) => { setCheatsState(next); emu.cheats = next; };
  const [currentRom, setCurrentRom] = useState<RomMeta | null>(null);
  const romBufRef = useRef<Uint8Array | null>(null);
  const saveKeyRef = useRef<string>('');
  const confirm = useConfirm();
  const toast = useToast();

  const append = (...args: unknown[]) => setLog((prev) => [...prev, args.map(String).join(' ')]);
  // notify = append to the debug log AND show a transient toast. Use it
  // for user-facing actions; plain append() stays for noisy debug lines.
  const notify = (msg: string, kind: ToastKind = 'info') => { append(msg); toast.show(msg, kind); };

  // Controller UI navigation. When any menu/panel is open the pad drives
  // DOM focus inside the top-most modal (D-pad moves focus, A clicks, B
  // closes) instead of the game — so every panel is controller-usable,
  // not just the launcher. The Controller panel is excluded: it captures
  // raw button presses for remapping/diagnostics. Refs keep the gamepad
  // poll in sync without restarting its loop.
  const closeAllModals = () => {
    setShowMenu(false); setShowSettings(false); setShowStates(false);
    setShowCheats(false); setShowLink(false); setShowDebug(false); setShowCp(false);
  };
  const navModalOpen = showMenu || showSettings || showStates || showCheats || showLink || showDebug;
  const anyModalOpen = navModalOpen || showCp;
  const uiNavRef = useRef(false); uiNavRef.current = navModalOpen;
  const anyModalRef = useRef(false); anyModalRef.current = anyModalOpen;
  // Hotkey toggles between the game and the menu: open the launcher from
  // gameplay, or back out to the game from any open panel.
  const onHotkey = () => { if (anyModalRef.current) closeAllModals(); else setShowMenu(true); };
  const onMenuNav = (action: MenuNav) => {
    const panels = document.querySelectorAll<HTMLElement>('.modal-panel');
    const panel = panels[panels.length - 1];   // top-most
    if (!panel) return;
    if (action === 'back') {
      panel.querySelector<HTMLElement>('[aria-label="Close"]')?.click();
      return;
    }
    const active = document.activeElement as HTMLElement | null;

    // Left/Right adjust the focused control's VALUE (slider/select) so
    // sliders like volume are usable with a pad; on other elements they
    // fall through to focus movement.
    if ((action === 'left' || action === 'right') && active) {
      const dir = action === 'right' ? 1 : -1;
      if (active instanceof HTMLInputElement && active.type === 'range') {
        const min = parseFloat(active.min || '0');
        const max = parseFloat(active.max || '1');
        const step = parseFloat(active.step) || 0;
        const delta = Math.max(step, (max - min) / 10) * dir;   // ~10 presses end-to-end
        const v = Math.min(max, Math.max(min, parseFloat(active.value) + delta));
        setNativeValue(active, String(v));
        active.dispatchEvent(new Event('input', { bubbles: true }));
        return;
      }
      if (active instanceof HTMLSelectElement && active.options.length) {
        const n = active.options.length;
        setNativeValue(active, active.options[(active.selectedIndex + dir + n) % n].value);
        active.dispatchEvent(new Event('change', { bubbles: true }));
        return;
      }
    }

    const sel = 'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';
    const items = Array.from(panel.querySelectorAll<HTMLElement>(sel)).filter((el) => el.offsetParent !== null);
    if (!items.length) return;
    const cur = items.indexOf(active as HTMLElement);
    if (action === 'select') {
      const el = cur >= 0 ? items[cur] : items[0];
      // A on a slider does nothing useful; everything else clicks.
      if (!(el instanceof HTMLInputElement && el.type === 'range')) el.click();
      return;
    }
    let next: number;
    if (cur < 0) next = 0;
    else if (action === 'down' || action === 'right') next = (cur + 1) % items.length;
    else next = (cur - 1 + items.length) % items.length;
    items[next].focus();
    items[next].scrollIntoView({ block: 'nearest' });
  };

  useGamepad({
    keypad: emu.keypad,
    onConnected: (name) => append(`controller connected: ${name}`),
    onDisconnected: (name) => append(`controller disconnected: ${name}`),
    mapVersion,
    onHotkey,
    menuOpenRef: uiNavRef,
    onMenuNav,
  });
  useKeypadHighlight(emu.keypad);

  // Boot a ROM by id (= IndexedDB key).
  const loadRomById = async (id: string) => {
    const bytes = await getRomBytes(id);
    if (!bytes) {
      append(`no ROM stored for "${id}" — back to home`);
      onExit();
      return;
    }
    romBufRef.current = bytes;
    const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
    const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
    const saveKey = `emulators:save:${code}`;
    saveKeyRef.current = saveKey;
    setHeaderInfo(`${title.trim()} · ${code}`);
    setCurrentRom({ id, filename: title, title, code, system: 'gba', size: bytes.length, addedAt: 0 });
    emu.loadRom(bytes);
    try {
      const raw = localStorage.getItem(saveKey);
      if (raw) {
        const arr = base64ToBytes(raw);
        emu.save.loadSave(arr);
        append(`restored save (${arr.length} bytes, ${emu.saveType})`);
      }
    } catch (e) {
      append('save restore failed:', (e as Error).message);
    }
    setSelectedRom(id);
    const rehydratedCheats = loadCheatsFor(code);
    setCheats(rehydratedCheats);
    // Auto-resume: if enabled and a hidden auto-state exists for this
    // game, restore it on top of the cold boot so the user picks up
    // where they left off. The auto-state carries its own save data.
    let resumed = false;
    if (autoResumeOn) {
      try {
        const auto = await getStateBlob(code, AUTO_SLOT);
        if (auto) { loadState(emu, auto); resumed = true; }
      } catch { /* fall through to a fresh boot */ }
    }
    notify(resumed ? `Resumed ${title.trim() || code}` : `Loaded ${title.trim() || code}`, 'success');
    let writeTimer: number | null = null;
    emu.save.onChange = () => {
      if (writeTimer !== null) return;
      writeTimer = window.setTimeout(() => {
        writeTimer = null;
        try {
          localStorage.setItem(saveKey, bytesToBase64(emu.save.data));
        } catch (e) {
          console.warn('Save persist failed', e);
        }
      }, 250);
    };
  };

  // Boot the ROM whenever the URL param changes. The guard ref makes
  // this idempotent per romId so React 18/19 StrictMode's double-invoke
  // in dev doesn't boot (and toast) twice.
  const bootedRomRef = useRef<string | null>(null);
  useEffect(() => {
    if (!romId) { onExit(); return; }
    if (bootedRomRef.current === romId) return;
    bootedRomRef.current = romId;
    loadRomById(romId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [romId]);

  // Keyboard bindings + Web Audio unlock. The GBA-key portion of the
  // mapping is user-configurable (ControllerPanel → Keyboard) and
  // persisted; we re-read it when mapVersion bumps. The other key
  // handlers (Tab fast-forward, F2/F4 state, "." step, Backspace
  // rewind) live in their own effects below and are NOT remappable.
  useEffect(() => {
    const keyMap = loadKeyboardMap();
    const down = (e: KeyboardEvent) => {
      audio.resume();
      // Hold Tab for momentary fast-forward (turbo). Ignore auto-repeat.
      if (e.key === 'Tab') { e.preventDefault(); if (!e.repeat) setTurbo(true); return; }
      const k = keyMap[e.key];
      if (k !== undefined) { emu.keypad.press(k); e.preventDefault(); }
    };
    const up = (e: KeyboardEvent) => {
      if (e.key === 'Tab') { e.preventDefault(); setTurbo(false); return; }
      const k = keyMap[e.key];
      if (k !== undefined) { emu.keypad.release(k); e.preventDefault(); }
    };
    const ptr = () => audio.resume();
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    window.addEventListener('pointerdown', ptr);
    return () => {
      window.removeEventListener('keydown', down);
      window.removeEventListener('keyup', up);
      window.removeEventListener('pointerdown', ptr);
    };
  }, [emu, audio, mapVersion]);

  const onReset = () => {
    if (!romBufRef.current) return;
    emu.loadRom(romBufRef.current);
    try {
      const raw = localStorage.getItem(saveKeyRef.current);
      if (raw) emu.save.loadSave(base64ToBytes(raw));
    } catch { /* ignore */ }
    notify('Game reset');
  };

  const onDownloadSave = () => {
    const bytes = new Uint8Array(emu.save.data);
    const blob = new Blob([bytes], { type: 'application/octet-stream' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${currentRom?.code || 'gba'}.sav`;
    a.click();
    URL.revokeObjectURL(url);
    notify(`Exported ${emu.save.data.length}-byte .sav`, 'success');
  };
  const onUploadSave = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) { append('upload cancelled (no file)'); return; }
    file.arrayBuffer().then((buf) => {
      emu.save.loadSave(new Uint8Array(buf));
      try {
        localStorage.setItem(saveKeyRef.current, bytesToBase64(emu.save.data));
      } catch (err) {
        notify('localStorage write failed: ' + (err as Error).message, 'error');
      }
      // Auto-reset so the game's cold boot picks up the freshly
      // imported save — otherwise the new bytes just sit in Flash
      // while the title screen still reflects the old state.
      if (romBufRef.current) emu.loadRom(romBufRef.current);
      notify(`Imported save (${buf.byteLength} bytes) — game reset`, 'success');
    }).catch((err) => {
      notify('Save import failed: ' + (err as Error).message, 'error');
    });
    e.target.value = '';
  };
  // The file input has to live OUTSIDE the dropdown menu, because the
  // moment the user clicks the import label the native file picker
  // opens and the mouse leaves the dropdown area — onMouseLeave then
  // unmounts the menu and the <input>, so when the user finally picks
  // a file there's no listener left to receive the onChange. Mounting
  // the input at the top level + triggering it via a ref dodges that
  // entirely.
  const saveInputRef = useRef<HTMLInputElement>(null);
  // Same trick for savestate import.
  const stateInputRef = useRef<HTMLInputElement>(null);
  // Screen writes its single-frame-advance fn here for frame-stepping.
  const stepRef = useRef<(() => void) | null>(null);

  const onSaveState = () => {
    try {
      const blob = saveState(emu);
      // Copy into a fresh ArrayBuffer so the Blob ctor accepts the
      // payload (the TS DOM types don't accept SharedArrayBuffer-
      // backed views even when the runtime would).
      const copy = new Uint8Array(new ArrayBuffer(blob.length));
      copy.set(blob);
      const url = URL.createObjectURL(new Blob([copy], { type: 'application/octet-stream' }));
      const a = document.createElement('a');
      a.href = url;
      a.download = `${currentRom?.code || 'gba'}.state`;
      a.click();
      URL.revokeObjectURL(url);
      notify(`Saved state (${blob.length} bytes)`, 'success');
    } catch (err) {
      notify('Save state failed: ' + (err as Error).message, 'error');
    }
  };

  const onLoadState = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) { append('state load cancelled'); return; }
    file.arrayBuffer().then((buf) => {
      try {
        loadState(emu, new Uint8Array(buf));
        notify(`Loaded state (${buf.byteLength} bytes)`, 'success');
      } catch (err) {
        notify('Load state failed: ' + (err as Error).message, 'error');
      }
    }).catch((err) => {
      notify('State read failed: ' + (err as Error).message, 'error');
    });
    e.target.value = '';
  };
  // Fullscreen target wraps just the Screen (+ stats line) so the
  // toolbar and modals don't render while fullscreen is active. The
  // `:fullscreen` CSS rule on canvas#screen handles the aspect-
  // preserving scale to viewport.
  const fsContainerRef = useRef<HTMLDivElement>(null);
  const [isFs, setIsFs] = useState(false);
  useEffect(() => {
    const onFsChange = () => setIsFs(document.fullscreenElement === fsContainerRef.current);
    document.addEventListener('fullscreenchange', onFsChange);
    return () => document.removeEventListener('fullscreenchange', onFsChange);
  }, []);
  const toggleFullscreen = () => {
    if (document.fullscreenElement) {
      document.exitFullscreen().catch(() => { /* user-cancelled */ });
      return;
    }
    fsContainerRef.current?.requestFullscreen().catch((e) => {
      notify('Fullscreen blocked: ' + (e as Error).message, 'error');
    });
  };

  const onClearSave = () => {
    confirm.ask({
      title: 'Clear save data',
      message: `Erase the save file for ${currentRom?.title || 'this ROM'}?\nThe ROM stays in your library; only the save data is deleted.`,
      confirmLabel: 'Clear save',
      danger: true,
      onConfirm: () => {
        localStorage.removeItem(saveKeyRef.current);
        // Write-through erase of the actual save chip (the wasm core); the
        // localStorage mirror is removed above.
        (emu.save as { reset?: () => void }).reset?.();
        notify('Save data cleared');
      },
    });
  };

  // Quick save/load use slot 0 (distinct from the manual 1..6 grid in
  // the Save States panel). Bound to F2 / F4 and the Save menu.
  const quickSave = async () => {
    if (!currentRom) return;
    try {
      await putState(currentRom.code, 0, saveState(emu), captureThumb(emu));
      notify('Quick-saved', 'success');
    } catch (e) {
      notify('Quick save failed: ' + (e as Error).message, 'error');
    }
  };
  const quickLoad = async () => {
    if (!currentRom) return;
    try {
      const blob = await getStateBlob(currentRom.code, 0);
      if (!blob) { notify('No quick save yet — F2 to save', 'error'); return; }
      loadState(emu, blob);
      notify('Quick-loaded', 'success');
    } catch (e) {
      notify('Quick load failed: ' + (e as Error).message, 'error');
    }
  };
  // F2 = quick save, F4 = quick load. Re-bound when the ROM changes so
  // the closures capture the current game's code.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'F2') { e.preventDefault(); quickSave(); }
      else if (e.key === 'F4') { e.preventDefault(); quickLoad(); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentRom]);

  // While paused, "." (and ">") advance exactly one frame.
  useEffect(() => {
    if (!paused) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === '.' || e.key === '>') { e.preventDefault(); stepRef.current?.(); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [paused]);

  // Rewind capture: while enabled and running forward, snapshot the
  // emulator into the ring at a fixed cadence.
  useEffect(() => {
    if (!rewindOn || effectivePaused || !currentRom) return;
    const id = window.setInterval(() => {
      try {
        const buf = rewindBufRef.current;
        buf.push(saveState(emu));
        if (buf.length > REWIND_MAX) buf.shift();
      } catch { /* skip this snapshot */ }
    }, REWIND_CAPTURE_MS);
    return () => window.clearInterval(id);
  }, [rewindOn, effectivePaused, currentRom, emu]);

  // Rewind playback: while the user holds rewind, pop snapshots and
  // restore them, rendering each via the Screen's frame stepper (the
  // forward loop is paused because effectivePaused is true).
  useEffect(() => {
    if (!rewinding) return;
    const id = window.setInterval(() => {
      const blob = rewindBufRef.current.pop();
      if (!blob) return;   // buffer drained — hold on the oldest frame
      try { loadState(emu, blob); stepRef.current?.(); } catch { /* ignore */ }
    }, REWIND_STEP_MS);
    return () => window.clearInterval(id);
  }, [rewinding, emu]);

  // Backspace holds rewind (when enabled); release resumes.
  useEffect(() => {
    if (!rewindOn) return;
    const down = (e: KeyboardEvent) => {
      if (e.key === 'Backspace') { e.preventDefault(); if (!e.repeat) setRewinding(true); }
    };
    const up = (e: KeyboardEvent) => {
      if (e.key === 'Backspace') { e.preventDefault(); setRewinding(false); }
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, [rewindOn]);

  // Auto-save to the reserved slot. Fire-and-forget; saveState captures
  // synchronously so the data is consistent even if we're unmounting.
  const autoSave = () => {
    if (!autoResumeOn || !currentRom) return;
    try { putState(currentRom.code, AUTO_SLOT, saveState(emu), captureThumb(emu)); } catch { /* ignore */ }
  };
  const autoSaveRef = useRef(autoSave);
  autoSaveRef.current = autoSave;

  // Periodic auto-save while a game is running forward.
  useEffect(() => {
    if (!autoResumeOn || !currentRom) return;
    const id = window.setInterval(() => autoSaveRef.current(), AUTO_SAVE_MS);
    return () => window.clearInterval(id);
  }, [autoResumeOn, currentRom]);

  // Save the moment the game is paused.
  useEffect(() => { if (paused) autoSaveRef.current(); }, [paused]);

  // Save on tab-hide and when leaving the player (unmount).
  useEffect(() => {
    const onHide = () => { if (document.visibilityState === 'hidden') autoSaveRef.current(); };
    document.addEventListener('visibilitychange', onHide);
    return () => {
      document.removeEventListener('visibilitychange', onHide);
      autoSaveRef.current();
    };
  }, []);

  // Render the current frame to a 4× nearest-neighbour canvas, shared by
  // screenshot download + clipboard copy.
  const renderFrameCanvas = (): HTMLCanvasElement | null => {
    const src = document.createElement('canvas');
    src.width = 240; src.height = 160;
    const sctx = src.getContext('2d');
    if (!sctx) return null;
    const img = sctx.createImageData(240, 160);
    img.data.set(emu.ppu.frame);
    sctx.putImageData(img, 0, 0);
    const scale = 4;
    const out = document.createElement('canvas');
    out.width = 240 * scale; out.height = 160 * scale;
    const octx = out.getContext('2d');
    if (!octx) return null;
    octx.imageSmoothingEnabled = false;
    octx.drawImage(src, 0, 0, out.width, out.height);
    return out;
  };

  const takeScreenshot = () => {
    if (!currentRom) return;
    const out = renderFrameCanvas();
    if (!out) { notify('Screenshot failed', 'error'); return; }
    out.toBlob((blob) => {
      if (!blob) { notify('Screenshot failed', 'error'); return; }
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url; a.download = `${currentRom.code || 'gba'}-${Date.now()}.png`; a.click();
      URL.revokeObjectURL(url);
      notify('Screenshot saved', 'success');
    }, 'image/png');
  };

  const copyScreenshot = () => {
    if (!currentRom) return;
    if (typeof ClipboardItem === 'undefined' || !navigator.clipboard?.write) {
      notify('Clipboard images not supported here', 'error'); return;
    }
    const out = renderFrameCanvas();
    if (!out) { notify('Screenshot failed', 'error'); return; }
    // ClipboardItem accepts a Promise<Blob>, keeping us inside the
    // user-gesture window despite toBlob being async.
    const blob = new Promise<Blob>((resolve, reject) =>
      out.toBlob((b) => (b ? resolve(b) : reject(new Error('encode failed'))), 'image/png'));
    navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })])
      .then(() => notify('Screenshot copied', 'success'))
      .catch((e) => notify('Copy failed: ' + (e as Error).message, 'error'));
  };

  // Record the canvas to a .webm via MediaRecorder.
  const startRecording = () => {
    const canvas = document.getElementById('screen') as HTMLCanvasElement | null;
    if (!canvas || typeof canvas.captureStream !== 'function' || typeof MediaRecorder === 'undefined') {
      notify('Recording not supported here', 'error'); return;
    }
    try {
      const stream = canvas.captureStream(60);
      const mime = MediaRecorder.isTypeSupported('video/webm;codecs=vp9')
        ? 'video/webm;codecs=vp9' : 'video/webm';
      const rec = new MediaRecorder(stream, { mimeType: mime });
      recChunksRef.current = [];
      rec.ondataavailable = (e) => { if (e.data.size) recChunksRef.current.push(e.data); };
      rec.onstop = () => {
        const blob = new Blob(recChunksRef.current, { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url; a.download = `${currentRom?.code || 'gba'}-${Date.now()}.webm`; a.click();
        URL.revokeObjectURL(url);
        notify(`Recording saved (${(blob.size / 1e6).toFixed(1)} MB)`, 'success');
      };
      rec.start();
      recorderRef.current = rec;
      setRecording(true);
      notify('Recording…');
    } catch (e) {
      notify('Recording failed: ' + (e as Error).message, 'error');
    }
  };
  const stopRecording = () => {
    try { recorderRef.current?.stop(); } catch { /* ignore */ }
    recorderRef.current = null;
    setRecording(false);
  };
  const toggleRecord = () => (recording ? stopRecording() : startRecording());
  // Flush any in-progress recording when leaving the player.
  useEffect(() => () => { try { recorderRef.current?.stop(); } catch { /* ignore */ } }, []);

  // Secondary actions (panel toggles). Rendered inline on wide screens
  // and folded into a "⋯ More" dropdown on phones to keep the toolbar
  // to a single row above the gamepad.
  const panelActions = [
    { key: 'settings', icon: '⚙', label: 'Settings', onClick: () => setShowSettings(true), disabled: false },
    { key: 'states', icon: '🗂', label: 'States', onClick: () => setShowStates(true), disabled: !currentRom },
    { key: 'shot', icon: '📸', label: 'Screenshot', onClick: takeScreenshot, disabled: !currentRom },
    { key: 'copy', icon: '🖼', label: 'Copy frame', onClick: copyScreenshot, disabled: !currentRom },
    { key: 'record', icon: recording ? '⏹' : '⏺', label: recording ? 'Stop recording' : 'Record video', onClick: toggleRecord, disabled: !currentRom },
    { key: 'cp', icon: '🎮', label: 'Controller', onClick: () => setShowCp(true), disabled: false },
    { key: 'cheats', icon: '★', label: 'Cheats', onClick: () => setShowCheats(true), disabled: !currentRom },
    { key: 'link', icon: '🔗', label: 'Link', onClick: () => setShowLink(true), disabled: !currentRom },
    { key: 'debug', icon: '🔍', label: 'Debug', onClick: () => setShowDebug(true), disabled: !currentRom },
    { key: 'log', icon: '📋', label: showLog ? 'Hide Log' : 'Show Log', onClick: () => setShowLog((v) => !v), disabled: false },
  ];
  // Menu launcher offers the enabled actions; the gamepad navigates it
  // (and every other panel) via DOM focus, so no per-item index here.
  const menuItems = panelActions.filter((a) => !a.disabled);

  return (
    <>
      <header className="w-full max-w-[720px] flex flex-wrap gap-2 justify-between items-center px-2">
        <button
          onClick={onExit}
          className="flex items-center gap-2 bg-transparent border-0 cursor-pointer group p-0"
          title="Home"
        >
          <span className="grid place-items-center w-7 h-7 rounded-md bg-[var(--color-accent-deep)] text-[var(--color-accent)] text-[11px] font-bold shadow-[inset_0_0_0_1px_rgba(95,208,255,0.25)] group-hover:brightness-125 transition">⌂</span>
          <span className="text-sm tracking-wide opacity-80 group-hover:opacity-100 transition">Home</span>
        </button>
        <div className="text-xs opacity-60 font-mono truncate">{headerInfo || 'no ROM loaded'}</div>
      </header>
      <ErrorBoundary label="Player" resetKey={currentRom?.id ?? null}>
        <div ref={fsContainerRef} className="fs-container">
          <div className="screen-wrap">
            <Screen emu={emu} paused={effectivePaused} audio={audio} onStats={setStats} speed={effectiveSpeed} smooth={smooth} colorCorrect={colorCorrect} stepRef={stepRef} />
            {crt && <div className="crt-overlay" aria-hidden />}
          </div>
        </div>
        <div className="w-full max-w-[720px] flex justify-between items-center px-2 text-[11px]">
          <span className="inline-flex items-center gap-2 font-mono text-[var(--color-accent)]">
            <span className={`w-1.5 h-1.5 rounded-full ${paused ? 'bg-[var(--color-warn)]' : 'bg-[var(--color-success)] shadow-[0_0_6px_var(--color-success)]'}`} />
            {rewinding ? <span className="text-[var(--color-accent)]">◀◀ rewinding</span> : paused ? <span className="text-[var(--color-warn)]">paused</span> : stats}
            {!effectivePaused && effectiveSpeed !== 1 && (
              <span className="text-[var(--color-warn)]">· {effectiveSpeed}×{turbo ? ' turbo' : ''}</span>
            )}
          </span>
          <span className="opacity-50 hidden sm:inline">arrows · z/x · a/s · enter/shift · tab=ff · f2/f4=state · .=step</span>
        </div>
      </ErrorBoundary>

      <ErrorBoundary label="Controls">
        <Gamepad keypad={emu.keypad} />

        <div className="flex gap-2 text-xs items-center w-full max-w-[720px] px-2 flex-wrap">
          <button onClick={onExit} className="btn" title="Home">⌂<span className="hidden sm:inline">Home</span></button>
          <button onClick={() => setPaused((p) => !p)} className="btn btn-primary" disabled={!currentRom} title={paused ? 'Resume' : 'Pause'}>{paused ? '▶' : '❚❚'}<span className="hidden sm:inline">{paused ? 'Resume' : 'Pause'}</span></button>
          {paused && (
            <button onClick={() => stepRef.current?.()} className="btn" disabled={!currentRom} title="Step one frame (.)">⏭<span className="hidden sm:inline">Step</span></button>
          )}
          <button onClick={onReset} className="btn" disabled={!currentRom} title="Reset">↻<span className="hidden sm:inline">Reset</span></button>
          <button onClick={toggleFullscreen} className="btn" disabled={!currentRom} title="Fullscreen">{isFs ? '↙' : '⛶'}<span className="hidden sm:inline">{isFs ? 'Exit FS' : 'Fullscreen'}</span></button>
          <button
            onClick={() => setSpeed((s) => (s === 1 ? 2 : 1))}
            className={speed > 1 ? 'btn btn-primary' : 'btn'}
            disabled={!currentRom}
            title="Fast-forward (or hold Tab)"
          >⏩<span className="hidden sm:inline">{speed > 1 ? `${speed}×` : 'FF'}</span></button>
          {rewindOn && (
            <button
              className={rewinding ? 'btn btn-primary' : 'btn'}
              disabled={!currentRom}
              title="Hold to rewind (or hold Backspace)"
              onPointerDown={(e) => { e.preventDefault(); setRewinding(true); }}
              onPointerUp={() => setRewinding(false)}
              onPointerLeave={() => setRewinding(false)}
              onPointerCancel={() => setRewinding(false)}
            >⏪<span className="hidden sm:inline">Rewind</span></button>
          )}
          {/* Save submenu condenses Export/Import/Clear into one popover. */}
          <div className="relative">
            <button
              onClick={() => setShowSaveMenu(!showSaveMenu)}
              className="btn"
              disabled={!currentRom}
              title="Save"
            >💾<span className="hidden sm:inline">Save</span> ▾</button>
            {showSaveMenu && currentRom && (
              <div
                className="absolute top-full left-0 mt-1.5 bg-[var(--color-elevated)] border border-[var(--color-border)] rounded-lg shadow-[var(--shadow-panel)] z-50 min-w-[170px] py-1.5"
                onMouseLeave={() => setShowSaveMenu(false)}
              >
                <button
                  onClick={() => { onDownloadSave(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs"
                >Export .sav</button>
                <button
                  onClick={() => { saveInputRef.current?.click(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs"
                >Import .sav</button>
                <div className="border-t border-[var(--color-border)] my-1.5"></div>
                <button
                  onClick={() => { quickSave(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs flex justify-between gap-3"
                ><span>Quick save</span><span className="opacity-40">F2</span></button>
                <button
                  onClick={() => { quickLoad(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs flex justify-between gap-3"
                ><span>Quick load</span><span className="opacity-40">F4</span></button>
                <button
                  onClick={() => { setShowStates(true); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs"
                >Save state slots…</button>
                <div className="border-t border-[var(--color-border)] my-1.5"></div>
                <button
                  onClick={() => { onSaveState(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs"
                >Export state file ↓</button>
                <button
                  onClick={() => { stateInputRef.current?.click(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[var(--color-card-hover)] text-xs"
                >Import state file ↑</button>
                <div className="border-t border-[var(--color-border)] my-1.5"></div>
                <button
                  onClick={() => { onClearSave(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-red-900/25 text-xs text-red-300"
                >Clear save</button>
              </div>
            )}
          </div>
          <div className="flex-1" />

          {/* Wide screens: secondary actions inline. */}
          <div className="hidden sm:flex gap-2 items-center">
            {panelActions.map((a) => (
              <button key={a.key} onClick={a.onClick} className="btn" disabled={a.disabled}>{a.icon} {a.label}</button>
            ))}
          </div>

          {/* Phones: fold secondary actions into a popover. */}
          <div className="relative sm:hidden">
            <button onClick={() => setShowMore((v) => !v)} className="btn" title="More">⋯ More</button>
            {showMore && (
              <>
                {/* Tap-outside catcher — touch screens don't fire mouseleave. */}
                <div className="fixed inset-0 z-40" onClick={() => setShowMore(false)} />
              <div
                className="absolute top-full right-0 mt-1.5 bg-[var(--color-elevated)] border border-[var(--color-border)] rounded-lg shadow-[var(--shadow-panel)] z-50 min-w-[170px] py-1.5"
              >
                {panelActions.map((a) => (
                  <button
                    key={a.key}
                    onClick={() => { a.onClick(); setShowMore(false); }}
                    disabled={a.disabled}
                    className="w-full text-left px-3 py-2 hover:bg-[var(--color-card-hover)] text-xs disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-2"
                  ><span className="w-4 text-center">{a.icon}</span>{a.label}</button>
                ))}
              </div>
              </>
            )}
          </div>
        </div>
      </ErrorBoundary>

      {showLog && <LogPane lines={log} />}

      <div className="w-full max-w-[720px] flex justify-end text-[10px] opacity-50 px-2">
        <a
          href="https://github.com/ImLunaHey/emulators/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:opacity-100 hover:text-[var(--color-accent)]"
        >Report an issue ↗</a>
      </div>

      <Modal open={showMenu} onClose={() => setShowMenu(false)} title="Menu" subtitle="D-pad move · A select · B close" size="sm" scrollBody={false}>
        <div className="flex flex-col gap-1.5">
          {menuItems.map((a) => (
            <button
              key={a.key}
              onClick={() => { setShowMenu(false); a.onClick(); }}
              className="flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm text-left border bg-[var(--color-card)] border-[var(--color-border)] hover:bg-[var(--color-card-hover)] transition-colors"
            >
              <span className="w-5 text-center text-base">{a.icon}</span>
              {a.label}
            </button>
          ))}
        </div>
      </Modal>
      <SettingsPanel
        open={showSettings}
        onClose={() => setShowSettings(false)}
        audio={audio}
        speed={speed}
        onSpeedChange={setSpeed}
        smooth={smooth}
        onSmoothChange={changeSmooth}
        crt={crt}
        onCrtChange={changeCrt}
        colorCorrect={colorCorrect}
        onColorCorrectChange={changeColorCorrect}
        turboMask={turboMask}
        onTurboChange={changeTurbo}
        rewind={rewindOn}
        onRewindChange={changeRewind}
        autoResume={autoResumeOn}
        onAutoResumeChange={changeAutoResume}
      />
      <SaveStatesPanel
        open={showStates}
        emu={emu}
        gameCode={currentRom?.code ?? null}
        onNotify={notify}
        onClose={() => setShowStates(false)}
      />
      <ControllerPanel
        open={showCp}
        onClose={() => setShowCp(false)}
        onChange={() => setMapVersion((v) => v + 1)}
      />
      <DebugPanel open={showDebug} emu={emu} onClose={() => setShowDebug(false)} />
      <LinkPanel open={showLink} emu={emu} onClose={() => setShowLink(false)} />
      <CheatsPanel
        open={showCheats}
        emu={emu}
        gameCode={currentRom?.code ?? null}
        romId={currentRom?.id ?? null}
        romTitle={currentRom?.title ?? null}
        cheats={cheats}
        onChange={setCheats}
        onClose={() => setShowCheats(false)}
      />
      {/* Top-level hidden file input for save imports — see saveInputRef
        * for why this is mounted here rather than inside the Save
        * dropdown. */}
      <input
        ref={saveInputRef}
        type="file"
        accept=".sav,.bin"
        onChange={onUploadSave}
        className="hidden"
      />
      <input
        ref={stateInputRef}
        type="file"
        accept=".state,.bin"
        onChange={onLoadState}
        className="hidden"
      />
      {confirm.node}
    </>
  );
}
