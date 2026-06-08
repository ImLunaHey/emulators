import { useEffect, useRef, useState } from 'react';
import { Emulator } from '../emulator';
import { Key } from '../io/keypad';
import { Screen } from './Screen';
import { Gamepad } from './Gamepad';
import { LogPane } from './LogPane';
import { useGamepad } from './useGamepad';
import { useKeypadHighlight } from './useKeypadHighlight';
import { ControllerPanel } from './ControllerPanel';
import { RomLibrary } from './RomLibrary';
import { getRomBytes, getSelectedRom, setSelectedRom, type RomMeta } from './romStore';
import { AudioSink } from './audio';

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
  const audioRef = useRef<AudioSink | null>(null);
  if (!audioRef.current) audioRef.current = new AudioSink();
  const audio = audioRef.current;

  const [paused, setPaused] = useState(false);
  const [stats, setStats] = useState('— fps · — Mhz');
  const [log, setLog] = useState<string[]>(['gba-recomp — pick a ROM to start']);
  const [headerInfo, setHeaderInfo] = useState<string>('');
  const [showCp, setShowCp] = useState(false);
  const [mapVersion, setMapVersion] = useState(0);
  const [showLib, setShowLib] = useState(false);
  const [currentRom, setCurrentRom] = useState<RomMeta | null>(null);
  const romBufRef = useRef<Uint8Array | null>(null);
  const saveKeyRef = useRef<string>('');

  const append = (...args: unknown[]) => setLog((prev) => [...prev, args.map(String).join(' ')]);

  useGamepad({
    keypad: emu.keypad,
    onConnected: (name) => append(`controller connected: ${name}`),
    onDisconnected: (name) => append(`controller disconnected: ${name}`),
    mapVersion,
  });
  useKeypadHighlight(emu.keypad);

  // Boot a ROM by id (= IndexedDB key).
  const loadRomById = async (id: string, meta?: RomMeta) => {
    const bytes = await getRomBytes(id);
    if (!bytes) { append(`no ROM stored for "${id}"`); return; }
    romBufRef.current = bytes;
    const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
    const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
    const saveKey = `gba-recomp:save:${code}`;
    saveKeyRef.current = saveKey;
    setHeaderInfo(`${title.trim()} · ${code}`);
    setCurrentRom(meta ?? { id, filename: title, title, code, size: bytes.length, addedAt: 0 });
    emu.loadRom(bytes);
    // emu.save is now whichever backend Emulator.loadRom picked from the
    // ROM signature (Flash 128 KB, SRAM 32 KB, or — eventually — EEPROM).
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
    append(`loaded "${title.trim() || code}" (${emu.saveType})`);
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

  // On first mount, auto-load the previously selected ROM if any.
  useEffect(() => {
    const id = getSelectedRom();
    if (id) loadRomById(id);
    else setShowLib(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keyboard bindings + Web Audio unlock. Browsers refuse to start an
  // AudioContext without a user gesture, so we resume on any keypress
  // or pointer-down — the first interaction triggers it transparently.
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      audio.resume();
      const k = KEY_MAP[e.key];
      if (k !== undefined) { emu.keypad.press(k); e.preventDefault(); }
    };
    const up = (e: KeyboardEvent) => {
      const k = KEY_MAP[e.key];
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
  }, [emu, audio]);

  const onReset = () => {
    if (!romBufRef.current) return;
    append('reset');
    emu.loadRom(romBufRef.current);
    try {
      const raw = localStorage.getItem(saveKeyRef.current);
      if (raw) emu.save.loadSave(base64ToBytes(raw));
    } catch { /* ignore */ }
  };

  const onDownloadSave = () => {
    // Re-wrap the Uint8Array into a fresh one with explicit ArrayBuffer
    // backing — TypeScript can't guarantee `emu.save.data` isn't backed
    // by a SharedArrayBuffer, even though we never allocate one.
    const bytes = new Uint8Array(emu.save.data);
    const blob = new Blob([bytes], { type: 'application/octet-stream' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${currentRom?.code || 'gba'}.sav`;
    a.click();
    URL.revokeObjectURL(url);
    append(`downloaded ${emu.save.data.length}-byte .sav`);
  };
  const onUploadSave = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    file.arrayBuffer().then((buf) => {
      emu.save.loadSave(new Uint8Array(buf));
      try {
        localStorage.setItem(saveKeyRef.current, bytesToBase64(emu.save.data));
      } catch { /* ignore */ }
      append(`uploaded save (${buf.byteLength} bytes)`);
    });
    e.target.value = '';
  };
  const onClearSave = () => {
    if (!confirm('Delete the saved game data for this ROM?')) return;
    localStorage.removeItem(saveKeyRef.current);
    emu.save.data.fill(0xFF);
    append('cleared save');
  };

  return (
    <>
      <header className="w-full max-w-[720px] flex justify-between items-baseline">
        <h1 className="text-sm m-0 tracking-wide opacity-80">GBA-RECOMP · Hybrid WASM</h1>
        <div className="text-xs opacity-60">{headerInfo || 'no ROM loaded'}</div>
      </header>
      <Screen emu={emu} paused={paused} audio={audio} onStats={setStats} />
      <div className="w-[720px] px-2 py-1 text-xs text-[var(--color-accent)] opacity-85 text-left">{stats}</div>
      <Gamepad keypad={emu.keypad} />
      <div className="flex gap-3 text-xs opacity-90 items-center w-[720px] flex-wrap">
        <button onClick={() => setShowLib(true)} className="btn-default">ROM Library…</button>
        <button onClick={() => setPaused((p) => !p)} className="btn-default" disabled={!currentRom}>{paused ? 'Resume' : 'Pause'}</button>
        <button onClick={onReset} className="btn-default" disabled={!currentRom}>Reset</button>
        <button onClick={onDownloadSave} className="btn-default" disabled={!currentRom}>Export .sav</button>
        <label className={`btn-default cursor-pointer ${!currentRom ? 'opacity-50 pointer-events-none' : ''}`}>
          Import .sav
          <input type="file" accept=".sav,.bin" onChange={onUploadSave} className="hidden" />
        </label>
        <button onClick={onClearSave} className="btn-default" disabled={!currentRom}>Clear Save</button>
        <button onClick={() => setShowCp(true)} className="btn-default">Controller…</button>
      </div>
      <div className="flex gap-3 text-xs opacity-70 items-center w-[720px] justify-between">
        <span>keys: arrows · z/x · a/s · enter/shift · saves auto-persist to browser storage</span>
        <a
          href="https://github.com/ImLunaHey/gba-recomp/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="text-[var(--color-accent)] opacity-80 hover:opacity-100 underline decoration-dotted"
        >Report an issue ↗</a>
      </div>
      <LogPane lines={log} />
      <ControllerPanel
        open={showCp}
        onClose={() => setShowCp(false)}
        onChange={() => setMapVersion((v) => v + 1)}
      />
      <RomLibrary
        open={showLib}
        currentId={currentRom?.id ?? null}
        onClose={() => setShowLib(false)}
        onSelect={(meta) => { setShowLib(false); loadRomById(meta.id, meta); }}
        onAppend={append}
      />
    </>
  );
}
