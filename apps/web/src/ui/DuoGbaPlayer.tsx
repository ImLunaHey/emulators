import { useEffect, useRef, useState } from 'react';
import { WasmEmulator } from './wasmEmulator';
import { AudioSink } from './audio';
import { listRoms, getRomBytes, type RomMeta } from './romStore';
import { loadKeyboardMap } from './keyboardMap';
import { stepDualFrame } from '../io/duoLink';

// Single-page local two-player GBA link ("duo"). Two independent WasmEmulator
// cores run the same ROM in the same tab, driven by ONE rAF clock, and are
// linked by a direct in-memory transport: each visual frame we run both cores
// one frame, then resolve exactly one Multi-play transfer (master's SIOMLT_SEND
// ↔ slave's SIOMLT_SEND). One clock means perfect lockstep — no network, no
// drift, no tab-throttling — which is what the cross-tab WebRTC path can't
// guarantee. This is the deterministic harness that proves the trade protocol
// (and a genuinely useful local-2P / self-trade-to-evolve mode on its own).
//
// Saves: each pane is fully isolated. Pane A persists to emulators:duo:A:<code>
// and pane B to emulators:duo:B:<code>, seeded (first run) from the regular
// single-player save emulators:save:<code> so you start where you left off.
// A per-pane "Load .sav" drops a different trainer into either side. Neither
// pane ever touches the other's slot or the single-player save.

const GBA_FRAME_MS = 1000 / 59.7275;
const FB_W = 240;
const FB_H = 160;

function bytesToBase64(bytes: Uint8Array): string {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}
function base64ToBytes(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function gbaCode(bytes: Uint8Array): string {
  return new TextDecoder('ascii').decode(bytes.subarray(0xac, 0xb0));
}

// Seed a pane's save: prefer its own persisted duo slot, else fall back to the
// single-player save for this game so both panes start already progressed.
function seedSaveFor(pane: 'A' | 'B', code: string): Uint8Array | null {
  try {
    const own = localStorage.getItem(`emulators:duo:${pane}:${code}`);
    if (own) return base64ToBytes(own);
    const sp = localStorage.getItem(`emulators:save:${code}`);
    if (sp) return base64ToBytes(sp);
  } catch { /* ignore */ }
  return null;
}

interface Props {
  onExit: () => void;
}

export function DuoGbaPlayer({ onExit }: Props) {
  const [romId, setRomId] = useState<string | null>(null);
  if (!romId) return <DuoRomPicker onPick={setRomId} onExit={onExit} />;
  return <DuoRunner romId={romId} onExit={() => setRomId(null)} />;
}

// ---- ROM picker -----------------------------------------------------------

function DuoRomPicker({ onPick, onExit }: { onPick: (id: string) => void; onExit: () => void }) {
  const [roms, setRoms] = useState<RomMeta[] | null>(null);
  useEffect(() => {
    listRoms().then((all) => setRoms(all.filter((r) => r.system === 'gba'))).catch(() => setRoms([]));
  }, []);
  return (
    <div className="min-h-screen bg-[#0b0b0f] text-gray-200 p-6">
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-lg font-semibold">Local 2-Player Link (GBA)</h1>
        <button onClick={onExit} className="btn">← Home</button>
      </div>
      <p className="text-[12px] opacity-70 mb-4 max-w-prose">
        Runs two copies of one GBA game side-by-side, linked locally. Pick a game; both
        panes seed from its save so they start at the same point. For a real trade,
        load a second trainer's <code>.sav</code> into one pane.
      </p>
      {!roms && <div className="opacity-60 text-sm">Loading library…</div>}
      {roms && roms.length === 0 && (
        <div className="opacity-60 text-sm">No GBA ROMs in your library. Add one from Home first.</div>
      )}
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
        {roms?.map((r) => (
          <button
            key={r.id}
            onClick={() => onPick(r.id)}
            className="well px-3 py-2 text-left hover:bg-[#1c1c22] rounded"
          >
            <div className="text-sm truncate">{r.title || r.filename}</div>
            <div className="text-[10px] opacity-50">{r.code}</div>
          </button>
        ))}
      </div>
    </div>
  );
}

// ---- The running duo ------------------------------------------------------

function DuoRunner({ romId, onExit }: { romId: string; onExit: () => void }) {
  const canvasA = useRef<HTMLCanvasElement>(null);
  const canvasB = useRef<HTMLCanvasElement>(null);
  const emuA = useRef<WasmEmulator | null>(null);
  const emuB = useRef<WasmEmulator | null>(null);
  const focus = useRef<'A' | 'B'>('A');
  const [focusUi, setFocusUi] = useState<'A' | 'B'>('A');
  const [status, setStatus] = useState('booting…');
  const [seqA, setSeqA] = useState(0);
  const codeRef = useRef('');
  // Link diagnostics. `log` keeps only transitions (a new master/slave word
  // pair) so it shows the handshake state machine walking, not 400/s of spam.
  const logRef = useRef<LinkLogEntry[]>([]);
  const lastPairRef = useRef<{ m: number; s: number }>({ m: -1, s: -1 });
  const [snap, setSnap] = useState<LinkSnap>({ siocntA: 0, siocntB: 0, mltA: 0, mltB: 0, m0a: 0, m1a: 0, m0b: 0, m1b: 0, logLen: 0 });

  useEffect(() => {
    let alive = true;
    let raf = 0;
    const a = new WasmEmulator();
    const b = new WasmEmulator();
    emuA.current = a;
    emuB.current = b;
    const audio = new AudioSink();
    const keymap = loadKeyboardMap();

    (async () => {
      await Promise.all([a.ready, b.ready]);
      if (!alive) return;
      const bytes = await getRomBytes(romId);
      if (!bytes || !alive) { setStatus('ROM not found'); return; }
      const code = gbaCode(bytes);
      codeRef.current = code;

      a.loadRom(bytes);
      b.loadRom(bytes);
      const sa = seedSaveFor('A', code);
      const sb = seedSaveFor('B', code);
      if (sa) a.save.loadSave(sa);
      if (sb) b.save.loadSave(sb);

      // Link roles: A is the cable master (parent, ID 0), B is the slave
      // (child, ID 1). This drives SIOCNT.SD/SI/ID and the multiplay path.
      a.linkSetState(true, true);
      b.linkSetState(true, false);

      // Persist each pane to its own isolated slot.
      let timerA: number | null = null;
      let timerB: number | null = null;
      a.save.onChange = () => {
        if (timerA !== null) return;
        timerA = window.setTimeout(() => {
          timerA = null;
          try { localStorage.setItem(`emulators:duo:A:${code}`, bytesToBase64(a.save.data)); } catch { /* quota */ }
        }, 400);
      };
      b.save.onChange = () => {
        if (timerB !== null) return;
        timerB = window.setTimeout(() => {
          timerB = null;
          try { localStorage.setItem(`emulators:duo:B:${code}`, bytesToBase64(b.save.data)); } catch { /* quota */ }
        }, 400);
      };

      const ctxA = canvasA.current!.getContext('2d')!;
      const ctxB = canvasB.current!.getContext('2d')!;
      const imgA = ctxA.createImageData(FB_W, FB_H);
      const imgB = ctxB.createImageData(FB_W, FB_H);

      setStatus('linked · A=master B=slave');

      let lastTs = performance.now();
      let accum = 0;
      const loop = (ts: number) => {
        if (!alive) return;
        raf = requestAnimationFrame(loop);
        const dt = ts - lastTs;
        lastTs = ts;
        accum += dt;
        if (accum > GBA_FRAME_MS * 4) accum = GBA_FRAME_MS * 4; // don't spiral
        let didFrame = false;
        while (accum >= GBA_FRAME_MS) {
          accum -= GBA_FRAME_MS;
          stepLinked(a, b, logRef.current, lastPairRef.current);
          didFrame = true;
        }
        if (didFrame) {
          imgA.data.set(a.ppu.frame);
          imgB.data.set(b.ppu.frame);
          ctxA.putImageData(imgA, 0, 0);
          ctxB.putImageData(imgB, 0, 0);
          // Play only the focused pane; drain the other so its FIFO doesn't grow.
          const focused = focus.current === 'A' ? a : b;
          const other = focus.current === 'A' ? b : a;
          const s = focused.sound.drainOutput();
          other.sound.drainOutput();
          if (s.length) audio.push(s, focused.sound.sampleRate);
          setSeqA(a.io.sio.transferSeq);
          // Sample the live SIO registers from both cores for the debug panel.
          setSnap({
            siocntA: a.io.read16(0x4000128) & 0xffff,
            siocntB: b.io.read16(0x4000128) & 0xffff,
            mltA: a.io.sio.mltSend & 0xffff,
            mltB: b.io.sio.mltSend & 0xffff,
            m0a: a.io.sio.multi[0] & 0xffff,
            m1a: a.io.sio.multi[1] & 0xffff,
            m0b: b.io.sio.multi[0] & 0xffff,
            m1b: b.io.sio.multi[1] & 0xffff,
            logLen: logRef.current.length,
          });
        }
      };
      raf = requestAnimationFrame(loop);
    })().catch((e) => setStatus('error: ' + (e as Error).message));

    // Keyboard → focused pane.
    const down = (e: KeyboardEvent) => {
      const k = keymap[e.key];
      if (k === undefined) return;
      e.preventDefault();
      (focus.current === 'A' ? emuA.current : emuB.current)?.keypad.press(k);
    };
    const up = (e: KeyboardEvent) => {
      const k = keymap[e.key];
      if (k === undefined) return;
      (focus.current === 'A' ? emuA.current : emuB.current)?.keypad.release(k);
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    const resumeAudio = () => audio.resume();
    window.addEventListener('pointerdown', resumeAudio);

    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      window.removeEventListener('keydown', down);
      window.removeEventListener('keyup', up);
      window.removeEventListener('pointerdown', resumeAudio);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [romId]);

  const setFocus = (p: 'A' | 'B') => {
    // Release any held keys on the pane losing focus so they don't stick.
    const losing = focus.current === 'A' ? emuA.current : emuB.current;
    if (losing) losing.keypad.pressed = 0;
    focus.current = p;
    setFocusUi(p);
  };

  const importSav = (pane: 'A' | 'B') => async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const buf = new Uint8Array(await file.arrayBuffer());
    const emu = pane === 'A' ? emuA.current : emuB.current;
    emu?.save.loadSave(buf);
    try { localStorage.setItem(`emulators:duo:${pane}:${codeRef.current}`, bytesToBase64(buf)); } catch { /* quota */ }
    e.target.value = '';
    setStatus(`loaded ${file.name} → pane ${pane}`);
  };

  return (
    <div className="min-h-screen bg-[#0b0b0f] text-gray-200 p-4">
      <div className="flex items-center justify-between mb-3">
        <div className="text-sm font-semibold">Local 2-Player Link · <span className="opacity-60 font-mono text-[12px]">{status}</span></div>
        <div className="flex items-center gap-2 text-[11px]">
          <span className="opacity-50">transfers: {seqA}</span>
          <button onClick={onExit} className="btn">← Games</button>
        </div>
      </div>
      <div className="flex gap-4 justify-center items-start flex-wrap">
        {(['A', 'B'] as const).map((pane) => (
          <div key={pane} className="flex flex-col gap-2">
            <div className="flex items-center justify-between text-[11px]">
              <span className={focusUi === pane ? 'text-green-400' : 'opacity-50'}>
                Pane {pane} {pane === 'A' ? '(master)' : '(slave)'} {focusUi === pane ? '· active' : ''}
              </span>
              <label className="btn !text-[10px] !py-0.5 cursor-pointer">
                Load .sav
                <input type="file" accept=".sav,.dat,.srm" className="hidden" onChange={importSav(pane)} />
              </label>
            </div>
            <canvas
              ref={pane === 'A' ? canvasA : canvasB}
              width={FB_W}
              height={FB_H}
              onClick={() => setFocus(pane)}
              className={`w-[480px] max-w-[44vw] cursor-pointer rounded ${focusUi === pane ? 'ring-2 ring-green-500' : 'ring-1 ring-[#2a2a30]'}`}
              style={{ imageRendering: 'pixelated' }}
            />
          </div>
        ))}
      </div>
      <p className="text-[11px] opacity-50 text-center mt-3">
        Click a screen to control it. Walk both trainers to the Pokémon Center 2F → rightmost desk (Trade Center).
      </p>

      <LinkDebug snap={snap} seq={seqA} log={logRef.current} />
    </div>
  );
}

// Live link diagnostics. SIOCNT bits per core, current send words, latched
// SIOMULTI slots, and the tail of the word-pair transition log — enough to see
// at a glance whether the handshake is exchanging sane values (mode=2, SD high,
// changing master/slave words) or stuck/garbage.
function LinkDebug({ snap, seq, log }: { snap: LinkSnap; seq: number; log: LinkLogEntry[] }) {
  const [copied, setCopied] = useState(false);
  const bit = (v: number, b: number) => (v >> b) & 1;
  const hex = (v: number, w = 4) => '0x' + (v >>> 0).toString(16).padStart(w, '0');
  const copy = () => {
    navigator.clipboard?.writeText(linkLogText(log, snap)).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    }).catch(() => {});
  };
  const Core = ({ name, cnt, mlt, m0, m1 }: { name: string; cnt: number; mlt: number; m0: number; m1: number }) => (
    <div className="flex flex-col gap-0.5">
      <div className="opacity-60">{name}</div>
      <div className="flex gap-2 flex-wrap">
        <span>mode={(cnt >> 12) & 3}</span>
        <span className={bit(cnt, 3) ? 'text-green-400' : 'text-gray-500'}>SD={bit(cnt, 3)}</span>
        <span>SI={bit(cnt, 2)}</span>
        <span>ID={(cnt >> 4) & 3}</span>
        <span className={bit(cnt, 7) ? 'text-yellow-400' : 'text-gray-500'}>START={bit(cnt, 7)}</span>
        <span className={bit(cnt, 6) ? 'text-red-400' : 'text-gray-500'}>ERR={bit(cnt, 6)}</span>
      </div>
      <div className="flex gap-2 opacity-80">
        <span>SIOCNT={hex(cnt)}</span><span>SEND={hex(mlt)}</span><span>M0={hex(m0)}</span><span>M1={hex(m1)}</span>
      </div>
    </div>
  );
  // Show the tail newest-last; the buffer is change-only so it stays small.
  const rows = log.slice(-80);
  return (
    <div className="max-w-3xl mx-auto mt-4 text-[10px] font-mono bg-[#0e0e12] rounded p-3 space-y-2">
      <div className="flex items-center justify-between">
        <span className="opacity-60 uppercase tracking-wider text-[9px]">link debug · transfers {seq} · transitions {snap.logLen}</span>
        <button onClick={copy} className="btn !text-[10px] !py-0.5">{copied ? 'Copied!' : 'Copy log'}</button>
      </div>
      <div className="grid grid-cols-2 gap-4">
        <Core name="A (master)" cnt={snap.siocntA} mlt={snap.mltA} m0={snap.m0a} m1={snap.m1a} />
        <Core name="B (slave)" cnt={snap.siocntB} mlt={snap.mltB} m0={snap.m0b} m1={snap.m1b} />
      </div>
      <div>
        <div className="opacity-60 mb-1">word-pair transitions — master / slave (newest last, last 80)</div>
        <div className="max-h-40 overflow-y-auto bg-black/30 rounded p-2 leading-relaxed">
          {rows.length === 0 && <span className="opacity-40">none yet — enter the Trade Center</span>}
          <div className="flex flex-wrap gap-x-3 gap-y-0.5 opacity-80">
            {rows.map((e, i) => (
              <span key={i}><span className="opacity-40">{e.seq}:</span> {hex(e.m)}/{hex(e.s)}</span>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

interface LinkLogEntry { t: number; m: number; s: number; seq: number; }
interface LinkSnap {
  siocntA: number; siocntB: number; mltA: number; mltB: number;
  m0a: number; m1a: number; m0b: number; m1b: number; logLen: number;
}
const LINK_LOG_CAP = 500;

// Advance both cores through one visual frame via the shared dual-core lockstep
// kernel (io/duoLink). We log only *transitions* (a new master/slave word pair)
// so the buffer shows the handshake state machine progressing rather than
// hundreds of identical rows/second.
function stepLinked(a: WasmEmulator, b: WasmEmulator, log: LinkLogEntry[], last: { m: number; s: number }): void {
  stepDualFrame(a, b, (master, slave) => {
    if (master !== last.m || slave !== last.s) {
      last.m = master; last.s = slave;
      log.push({ t: log.length, m: master, s: slave, seq: a.io.sio.transferSeq });
      if (log.length > LINK_LOG_CAP) log.splice(0, log.length - LINK_LOG_CAP);
    }
  });
}

function linkLogText(log: LinkLogEntry[], snap: LinkSnap): string {
  const hex = (v: number, w = 4) => '0x' + (v >>> 0).toString(16).padStart(w, '0');
  const head = [
    `A(master) SIOCNT=${hex(snap.siocntA)} SEND=${hex(snap.mltA)} M0=${hex(snap.m0a)} M1=${hex(snap.m1a)}`,
    `B(slave)  SIOCNT=${hex(snap.siocntB)} SEND=${hex(snap.mltB)} M0=${hex(snap.m0b)} M1=${hex(snap.m1b)}`,
    `transitions=${log.length}`,
    'seq\tmaster\tslave',
  ];
  const rows = log.map((e) => `${e.seq}\t${hex(e.m)}\t${hex(e.s)}`);
  return head.concat(rows).join('\n');
}
