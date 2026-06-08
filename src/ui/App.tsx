import { useEffect, useRef, useState } from 'react';
import { Emulator } from '../emulator';
import { Key } from '../io/keypad';
import { Screen } from './Screen';
import { Gamepad } from './Gamepad';
import { LogPane } from './LogPane';
import { useGamepad } from './useGamepad';

const ROMS = [
  { value: '/firered.gba',  label: 'Pokemon FireRed' },
  { value: '/emerald.gba',  label: 'Pokemon Emerald' },
  { value: '/ruby.gba',     label: 'Pokemon Ruby' },
  { value: '/garfield.gba', label: 'Garfield: Search for Pooky' },
  { value: '/crash.gba',    label: 'Crash Bandicoot' },
];

// Base64 helpers for stashing the 128 KB Flash buffer in localStorage.
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

const KEY_MAP: Record<string, Key> = {
  ArrowUp: Key.UP, ArrowDown: Key.DOWN, ArrowLeft: Key.LEFT, ArrowRight: Key.RIGHT,
  z: Key.A, Z: Key.A,
  x: Key.B, X: Key.B,
  a: Key.L, A: Key.L,
  s: Key.R, S: Key.R,
  Enter: Key.START,
  Shift: Key.SELECT,
};

export function App() {
  const emuRef = useRef<Emulator | null>(null);
  if (!emuRef.current) emuRef.current = new Emulator();
  const emu = emuRef.current;

  const [romPath, setRomPath] = useState(ROMS[0].value);
  const [paused, setPaused] = useState(false);
  const [stats, setStats] = useState('— fps · — Mhz');
  const [log, setLog] = useState<string[]>(['booting GBA WASM recompiler…']);
  const [headerInfo, setHeaderInfo] = useState<string>('');
  const romBufRef = useRef<Uint8Array | null>(null);

  const append = (...args: unknown[]) => setLog((prev) => [...prev, args.map(String).join(' ')]);

  useGamepad({
    keypad: emu.keypad,
    onConnected: (name) => append(`controller connected: ${name}`),
    onDisconnected: (name) => append(`controller disconnected: ${name}`),
  });

  // RTC GPIO interposer is now installed inside Emulator.constructor()
  // so it's always active (was previously here in useMemo, which meant
  // headless boot bypassed the RTC entirely).
  const saveKeyRef = useRef<string>('');

  // ROM load + per-ROM Flash persistence wiring.
  useEffect(() => {
    let cancelled = false;
    append(`fetching ${romPath}…`);
    fetch(romPath).then((r) => r.arrayBuffer()).then((buf) => {
      if (cancelled) return;
      const bytes = new Uint8Array(buf);
      romBufRef.current = bytes;
      const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
      const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
      const saveKey = `gba-recomp:save:${code}`;
      saveKeyRef.current = saveKey;
      append(`loaded ${bytes.length} bytes — "${title}" (${code})`);
      setHeaderInfo(`${title} · ${code}`);
      emu.loadRom(bytes);
      // Restore in-game save from localStorage if present.
      try {
        const raw = localStorage.getItem(saveKey);
        if (raw) {
          const arr = base64ToBytes(raw);
          emu.flash.loadSave(arr);
          append(`restored save (${arr.length} bytes)`);
        }
      } catch (e) {
        append('save restore failed:', (e as Error).message);
      }
      // Auto-persist on Flash writes (debounced).
      let writeTimer: number | null = null;
      emu.flash.onChange = () => {
        if (writeTimer !== null) return;
        writeTimer = window.setTimeout(() => {
          writeTimer = null;
          try {
            localStorage.setItem(saveKey, bytesToBase64(emu.flash.data));
          } catch (e) {
            console.warn('Flash persist failed', e);
          }
        }, 250);
      };
    });
    return () => { cancelled = true; };
  }, [romPath, emu]);

  // Keyboard bindings.
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      const k = KEY_MAP[e.key];
      if (k !== undefined) { emu.keypad.press(k); e.preventDefault(); }
    };
    const up = (e: KeyboardEvent) => {
      const k = KEY_MAP[e.key];
      if (k !== undefined) { emu.keypad.release(k); e.preventDefault(); }
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => {
      window.removeEventListener('keydown', down);
      window.removeEventListener('keyup', up);
    };
  }, [emu]);

  const onReset = () => {
    if (!romBufRef.current) return;
    append('reset');
    emu.loadRom(romBufRef.current);
    // Re-restore save after the reset wiped Flash.
    try {
      const raw = localStorage.getItem(saveKeyRef.current);
      if (raw) emu.flash.loadSave(base64ToBytes(raw));
    } catch { /* ignore */ }
  };

  const onDownloadSave = () => {
    const blob = new Blob([emu.flash.data], { type: 'application/octet-stream' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${headerInfo.split(' · ')[1] || 'gba'}.sav`;
    a.click();
    URL.revokeObjectURL(url);
    append('downloaded .sav file');
  };

  const onUploadSave = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    file.arrayBuffer().then((buf) => {
      emu.flash.loadSave(new Uint8Array(buf));
      try {
        localStorage.setItem(saveKeyRef.current, bytesToBase64(emu.flash.data));
      } catch { /* ignore */ }
      append(`uploaded save (${buf.byteLength} bytes)`);
    });
    e.target.value = '';
  };

  const onClearSave = () => {
    if (!confirm('Delete the saved game data for this ROM?')) return;
    localStorage.removeItem(saveKeyRef.current);
    emu.flash.data.fill(0xFF);
    append('cleared save');
  };

  return (
    <>
      <header>
        <h1>GBA-RECOMP · Hybrid WASM</h1>
        <div className="stats">{headerInfo || '—'}</div>
      </header>
      <Screen emu={emu} paused={paused} onStats={setStats} />
      <div className="stats-bar">{stats}</div>
      <Gamepad keypad={emu.keypad} />
      <div className="row">
        <select value={romPath} onChange={(e) => setRomPath(e.target.value)}>
          {ROMS.map((r) => <option key={r.value} value={r.value}>{r.label}</option>)}
        </select>
        <button onClick={() => setPaused((p) => !p)}>{paused ? 'Resume' : 'Pause'}</button>
        <button onClick={onReset}>Reset</button>
        <button onClick={onDownloadSave}>Export .sav</button>
        <label className="upload-btn">
          Import .sav
          <input type="file" accept=".sav,.bin" onChange={onUploadSave} style={{ display: 'none' }} />
        </label>
        <button onClick={onClearSave}>Clear Save</button>
      </div>
      <div className="row">
        <span style={{ opacity: 0.5 }}>keys: arrows · z/x · a/s · enter/shift · saves auto-persist to browser storage</span>
      </div>
      <LogPane lines={log} />
    </>
  );
}
