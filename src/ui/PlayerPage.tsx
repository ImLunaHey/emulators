import { useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Key } from '../io/keypad';
import { Screen } from './Screen';
import { Gamepad } from './Gamepad';
import { LogPane } from './LogPane';
import { useGamepad } from './useGamepad';
import { useKeypadHighlight } from './useKeypadHighlight';
import { ControllerPanel } from './ControllerPanel';
import { DebugPanel } from './DebugPanel';
import { ErrorBoundary } from './ErrorBoundary';
import { CheatsPanel, loadCheatsFor } from './CheatsPanel';
import type { Cheat } from '../io/cheats';
import { getRomBytes, setSelectedRom, type RomMeta } from './romStore';
import { useEmu } from './EmuContext';
import { useConfirm } from './ConfirmModal';

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

// /play/:romId — boots the ROM identified by the URL param, then renders
// the Screen + controls + modal panels. Falls back to / if the ROM
// can't be loaded.
export function PlayerPage() {
  const { emu, audio } = useEmu();
  const navigate = useNavigate();
  const { romId } = useParams<{ romId: string }>();

  const [paused, setPaused] = useState(false);
  const [stats, setStats] = useState('— fps · — Mhz');
  const [log, setLog] = useState<string[]>([]);
  const [headerInfo, setHeaderInfo] = useState<string>('');
  const [showCp, setShowCp] = useState(false);
  const [showDebug, setShowDebug] = useState(false);
  const [showCheats, setShowCheats] = useState(false);
  const [showLog, setShowLog] = useState(false);
  const [showSaveMenu, setShowSaveMenu] = useState(false);
  const [mapVersion, setMapVersion] = useState(0);
  const [cheats, setCheatsState] = useState<Cheat[]>([]);
  const setCheats = (next: Cheat[]) => { setCheatsState(next); emu.cheats = next; };
  const [currentRom, setCurrentRom] = useState<RomMeta | null>(null);
  const romBufRef = useRef<Uint8Array | null>(null);
  const saveKeyRef = useRef<string>('');
  const confirm = useConfirm();

  const append = (...args: unknown[]) => setLog((prev) => [...prev, args.map(String).join(' ')]);

  useGamepad({
    keypad: emu.keypad,
    onConnected: (name) => append(`controller connected: ${name}`),
    onDisconnected: (name) => append(`controller disconnected: ${name}`),
    mapVersion,
  });
  useKeypadHighlight(emu.keypad);

  // Boot a ROM by id (= IndexedDB key).
  const loadRomById = async (id: string) => {
    const bytes = await getRomBytes(id);
    if (!bytes) {
      append(`no ROM stored for "${id}" — back to library`);
      navigate('/', { replace: true });
      return;
    }
    romBufRef.current = bytes;
    const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
    const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
    const saveKey = `gba-recomp:save:${code}`;
    saveKeyRef.current = saveKey;
    setHeaderInfo(`${title.trim()} · ${code}`);
    setCurrentRom({ id, filename: title, title, code, size: bytes.length, addedAt: 0 });
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
    append(`loaded "${title.trim() || code}" (${emu.saveType})`);
    const rehydratedCheats = loadCheatsFor(code);
    setCheats(rehydratedCheats);
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

  // Boot the ROM whenever the URL param changes.
  useEffect(() => {
    if (!romId) { navigate('/', { replace: true }); return; }
    loadRomById(romId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [romId]);

  // Keyboard bindings + Web Audio unlock.
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
    confirm.ask({
      title: 'Clear save data',
      message: `Erase the save file for ${currentRom?.title || 'this ROM'}?\nThe ROM stays in your library; only the save data is deleted.`,
      confirmLabel: 'Clear save',
      danger: true,
      onConfirm: () => {
        localStorage.removeItem(saveKeyRef.current);
        emu.save.data.fill(0xFF);
        append('cleared save');
      },
    });
  };

  return (
    <>
      <header className="w-full max-w-[720px] flex flex-wrap gap-2 justify-between items-baseline px-2">
        <h1 className="text-sm m-0 tracking-wide opacity-80">gba-recomp</h1>
        <div className="text-xs opacity-60 font-mono truncate">{headerInfo || 'no ROM loaded'}</div>
      </header>
      <ErrorBoundary label="Player" resetKey={currentRom?.id ?? null}>
        <Screen emu={emu} paused={paused} audio={audio} onStats={setStats} />
        <div className="w-full max-w-[720px] flex justify-between items-center px-2 text-[11px]">
          <span className="text-[var(--color-accent)] opacity-85 font-mono">{stats}</span>
          <span className="opacity-50 hidden sm:inline">arrows · z/x · a/s · enter/shift</span>
        </div>
      </ErrorBoundary>

      <ErrorBoundary label="Controls">
        <Gamepad keypad={emu.keypad} />

        <div className="flex gap-2 text-xs items-center w-full max-w-[720px] px-2 flex-wrap">
          <button onClick={() => navigate('/')} className="btn-default">📂 Library</button>
          <button onClick={() => setPaused((p) => !p)} className="btn-default" disabled={!currentRom}>{paused ? '▶ Resume' : '❚❚ Pause'}</button>
          <button onClick={onReset} className="btn-default" disabled={!currentRom}>↻ Reset</button>
          {/* Save submenu condenses Export/Import/Clear into one popover. */}
          <div className="relative">
            <button
              onClick={() => setShowSaveMenu(!showSaveMenu)}
              className="btn-default"
              disabled={!currentRom}
            >💾 Save ▾</button>
            {showSaveMenu && currentRom && (
              <div
                className="absolute top-full left-0 mt-1 bg-[#1c1c22] border border-[#2a2a30] rounded-md shadow-xl z-50 min-w-[160px] py-1"
                onMouseLeave={() => setShowSaveMenu(false)}
              >
                <button
                  onClick={() => { onDownloadSave(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[#24242a] text-xs"
                >Export .sav</button>
                <label className="block w-full text-left px-3 py-1.5 hover:bg-[#24242a] cursor-pointer text-xs">
                  Import .sav
                  <input
                    type="file"
                    accept=".sav,.bin"
                    onChange={(e) => { onUploadSave(e); setShowSaveMenu(false); }}
                    className="hidden"
                  />
                </label>
                <button
                  onClick={() => { onClearSave(); setShowSaveMenu(false); }}
                  className="w-full text-left px-3 py-1.5 hover:bg-[#24242a] text-xs text-red-300"
                >Clear save</button>
              </div>
            )}
          </div>
          <div className="flex-1" />
          <button onClick={() => setShowCp(true)} className="btn-default">🎮 Controller</button>
          <button onClick={() => setShowCheats(true)} className="btn-default" disabled={!currentRom}>★ Cheats</button>
          <button onClick={() => setShowDebug(true)} className="btn-default" disabled={!currentRom}>🔍 Debug</button>
          <button onClick={() => setShowLog(!showLog)} className="btn-default">{showLog ? 'Hide Log' : 'Show Log'}</button>
        </div>
      </ErrorBoundary>

      {showLog && <LogPane lines={log} />}

      <div className="w-full max-w-[720px] flex justify-end text-[10px] opacity-50 px-2">
        <a
          href="https://github.com/ImLunaHey/gba-recomp/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:opacity-100 hover:text-[var(--color-accent)]"
        >Report an issue ↗</a>
      </div>

      <ControllerPanel
        open={showCp}
        onClose={() => setShowCp(false)}
        onChange={() => setMapVersion((v) => v + 1)}
      />
      <DebugPanel open={showDebug} emu={emu} onClose={() => setShowDebug(false)} />
      <CheatsPanel
        open={showCheats}
        gameCode={currentRom?.code ?? null}
        cheats={cheats}
        onChange={setCheats}
        onClose={() => setShowCheats(false)}
      />
      {confirm.node}
    </>
  );
}
