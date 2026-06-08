import { useEffect, useMemo, useRef, useState } from 'react';
import { Emulator } from '../emulator';
import { Key } from '../io/keypad';
import { Screen } from './Screen';
import { Gamepad } from './Gamepad';
import { LogPane } from './LogPane';

const ROMS = [
  { value: '/firered.gba',  label: 'Pokemon FireRed' },
  { value: '/emerald.gba',  label: 'Pokemon Emerald' },
  { value: '/ruby.gba',     label: 'Pokemon Ruby' },
  { value: '/garfield.gba', label: 'Garfield: Search for Pooky' },
  { value: '/crash.gba',    label: 'Crash Bandicoot' },
];

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
  const emuRef = useRef<Emulator>();
  if (!emuRef.current) emuRef.current = new Emulator();
  const emu = emuRef.current;

  const [romPath, setRomPath] = useState(ROMS[0].value);
  const [paused, setPaused] = useState(false);
  const [stats, setStats] = useState('— fps · — Mhz');
  const [log, setLog] = useState<string[]>(['booting GBA WASM recompiler…']);
  const [headerInfo, setHeaderInfo] = useState<string>('');
  const romBufRef = useRef<Uint8Array | null>(null);

  const append = (...args: unknown[]) => setLog((prev) => [...prev, args.map(String).join(' ')]);

  // RTC GPIO interposer at ROM 0x080000C4/C6/C8. Set up once.
  useMemo(() => {
    const origRead16 = emu.bus.read16.bind(emu.bus);
    const origWrite16 = emu.bus.write16.bind(emu.bus);
    const origRead8 = emu.bus.read8.bind(emu.bus);
    const origWrite8 = emu.bus.write8.bind(emu.bus);
    emu.bus.read16 = (addr) => {
      if ((addr & 0xFFFFFFF8) === 0x080000C0) {
        const off = addr & 0xFE;
        if (off === 0xC4 || off === 0xC6 || off === 0xC8) return emu.rtc.read(off);
      }
      return origRead16(addr);
    };
    emu.bus.write16 = (addr, v) => {
      if ((addr & 0xFFFFFFF8) === 0x080000C0) {
        const off = addr & 0xFE;
        if (off === 0xC4 || off === 0xC6 || off === 0xC8) { emu.rtc.write(off, v); return; }
      }
      origWrite16(addr, v);
    };
    emu.bus.read8 = (addr) => {
      if ((addr & 0xFFFFFFF8) === 0x080000C0) {
        const off = addr & 0xFF;
        if (off === 0xC4 || off === 0xC6 || off === 0xC8) return emu.rtc.read(off);
      }
      return origRead8(addr);
    };
    emu.bus.write8 = (addr, v) => {
      if ((addr & 0xFFFFFFF8) === 0x080000C0) {
        const off = addr & 0xFF;
        if (off === 0xC4 || off === 0xC6 || off === 0xC8) { emu.rtc.write(off, v); return; }
      }
      origWrite8(addr, v);
    };
  }, [emu]);

  // ROM load.
  useEffect(() => {
    let cancelled = false;
    append(`fetching ${romPath}…`);
    fetch(romPath).then((r) => r.arrayBuffer()).then((buf) => {
      if (cancelled) return;
      const bytes = new Uint8Array(buf);
      romBufRef.current = bytes;
      const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
      const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
      append(`loaded ${bytes.length} bytes — "${title}" (${code})`);
      setHeaderInfo(`${title} · ${code}`);
      emu.loadRom(bytes);
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
        <span style={{ opacity: 0.5 }}>keys: arrows · z/x · a/s · enter/shift</span>
      </div>
      <LogPane lines={log} />
    </>
  );
}
