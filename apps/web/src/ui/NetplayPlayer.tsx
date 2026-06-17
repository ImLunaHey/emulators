import { useEffect, useRef, useState } from 'react';
import { WasmEmulator } from './wasmEmulator';
import { AudioSink } from './audio';
import { listRoms, getRomBytes, type RomMeta } from './romStore';
import { loadKeyboardMap } from './keyboardMap';
import { WirelessTransport } from '../io/wireless-transport';

// Online GBA play/trade via the Wireless Adapter relayed over the network.
//
// ONE GBA per machine — your own. The wireless adapter (RFU) is packet-based and
// latency-tolerant (unlike the bit-synchronous link cable), so we just forward
// its packets to the peer over the same WS room. Works LAN → WAN. For Pokémon
// FR/LG/Emerald this is the native online path (Union Room). Reachable at
// /?netplay.

const GBA_FRAME_MS = 1000 / 59.7275;
const FB_W = 240;
const FB_H = 160;
const MAX_FRAMES_PER_TICK = 4;

function gbaCode(bytes: Uint8Array): string {
  return new TextDecoder('ascii').decode(bytes.subarray(0xac, 0xb0));
}
function loadSaveFor(code: string): Uint8Array {
  try {
    const raw = localStorage.getItem(`emulators:save:${code}`);
    if (raw) {
      const bin = atob(raw);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    }
  } catch { /* ignore */ }
  return new Uint8Array(0);
}

interface Props { onExit: () => void; }

export function NetplayPlayer({ onExit }: Props) {
  const [rom, setRom] = useState<{ bytes: Uint8Array } | null>(null);
  if (!rom) return <RomPicker onPick={(bytes) => setRom({ bytes })} onExit={onExit} />;
  return <WirelessRunner romBytes={rom.bytes} onExit={() => setRom(null)} />;
}

function RomPicker({ onPick, onExit }: { onPick: (bytes: Uint8Array) => void; onExit: () => void }) {
  const [roms, setRoms] = useState<RomMeta[] | null>(null);
  useEffect(() => {
    listRoms().then((all) => setRoms(all.filter((r) => r.system === 'gba'))).catch(() => setRoms([]));
  }, []);
  const pick = async (r: RomMeta) => { const b = await getRomBytes(r.id); if (b) onPick(b); };
  return (
    <div className="min-h-screen bg-[#0b0b0f] text-gray-200 p-6">
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-lg font-semibold">Online (Wireless) — beta</h1>
        <button onClick={onExit} className="btn">← Home</button>
      </div>
      <p className="text-[12px] opacity-70 mb-4 max-w-prose">
        Play or trade online via the GBA Wireless Adapter. Pick your game, create
        a room and share the code (or join one), then use the in-game
        <b> Union Room</b> (Pokémon Center 2F, middle desk) to connect.
      </p>
      {!roms && <div className="opacity-60 text-sm">Loading library…</div>}
      {roms && roms.length === 0 && <div className="opacity-60 text-sm">No GBA ROMs in your library.</div>}
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
        {roms?.map((r) => (
          <button key={r.id} onClick={() => pick(r)} className="well px-3 py-2 text-left hover:bg-[#1c1c22] rounded">
            <div className="text-sm truncate">{r.title || r.filename}</div>
            <div className="text-[10px] opacity-50">{r.code}</div>
          </button>
        ))}
      </div>
    </div>
  );
}

interface NetSnap { connected: boolean; packetsIn: number; packetsOut: number; lastEvent: string; rcnt: number; siocnt: number; }
const EMPTY: NetSnap = { connected: false, packetsIn: 0, packetsOut: 0, lastEvent: '', rcnt: 0, siocnt: 0 };

function WirelessRunner({ romBytes, onExit }: { romBytes: Uint8Array; onExit: () => void }) {
  const [roomInput, setRoomInput] = useState('');
  const [phase, setPhase] = useState<'lobby' | 'connecting' | 'live'>('lobby');
  const [status, setStatus] = useState('');
  const [snap, setSnap] = useState<NetSnap>(EMPTY);
  const [customSaveName, setCustomSaveName] = useState<string | null>(null);

  const emuRef = useRef<WasmEmulator | null>(null);
  const transportRef = useRef<WirelessTransport | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const customSaveRef = useRef<Uint8Array | null>(null);

  // Keyboard → the single local core.
  useEffect(() => {
    const keymap = loadKeyboardMap();
    const down = (e: KeyboardEvent) => { const k = keymap[e.key]; if (k === undefined) return; e.preventDefault(); keysRef.current |= 1 << k; };
    const up = (e: KeyboardEvent) => { const k = keymap[e.key]; if (k === undefined) return; keysRef.current &= ~(1 << k); };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, []);

  useEffect(() => () => { transportRef.current?.disconnect(); }, []);

  const start = async (code: string, isHost: boolean) => {
    setPhase('connecting');
    const c = code.trim().toUpperCase();
    setRoomInput(c);
    try {
      const emu = new WasmEmulator();
      emuRef.current = emu;
      await emu.ready;
      emu.loadRom(romBytes);
      const save = customSaveRef.current ?? loadSaveFor(gbaCode(romBytes));
      if (save.length) emu.save.loadSave(save);
      emu.setWirelessAdapter(true);

      const transport = new WirelessTransport(emu);
      transportRef.current = transport;
      transport.onPeerJoin = () => setStatus('peer connected — open the Union Room');
      transport.onPeerLeave = () => setStatus('peer left');
      transport.onError = (e) => setStatus('error: ' + e.message);
      // Drive the transport each frame via the existing pump hook.
      emu.io.sio.transport = transport as unknown as typeof emu.io.sio.transport;

      await transport.connect({ roomId: c, isHost });
      setStatus('waiting for peer…');
      setPhase('live');
    } catch (e) {
      setStatus('connect failed: ' + (e as Error).message);
      setPhase('lobby');
    }
  };

  // Run loop — starts once we're live and the canvas is mounted.
  useEffect(() => {
    if (phase !== 'live') return;
    const emu = emuRef.current;
    const transport = transportRef.current;
    const canvas = canvasRef.current;
    if (!emu || !transport || !canvas) return;
    const ctx = canvas.getContext('2d')!;
    const img = ctx.createImageData(FB_W, FB_H);
    const audio = new AudioSink();
    let alive = true;
    let raf = 0;
    let lastTs = performance.now();
    let accum = 0;
    const loop = (ts: number) => {
      if (!alive) return;
      raf = requestAnimationFrame(loop);
      accum += ts - lastTs;
      lastTs = ts;
      if (accum > GBA_FRAME_MS * MAX_FRAMES_PER_TICK) accum = GBA_FRAME_MS * MAX_FRAMES_PER_TICK;
      let ran = 0;
      while (accum >= GBA_FRAME_MS && ran < MAX_FRAMES_PER_TICK) {
        emu.keypad.pressed = keysRef.current & 0x3ff;
        emu.runFrame(); // pumps the wireless transport via pumpLink
        accum -= GBA_FRAME_MS;
        ran++;
      }
      if (ran > 0) {
        img.data.set(emu.ppu.frame);
        ctx.putImageData(img, 0, 0);
        const s = emu.sound.drainOutput();
        if (s.length) audio.push(s, emu.sound.sampleRate);
        setSnap({
          connected: transport.isConnected(),
          packetsIn: transport.packetsIn,
          packetsOut: transport.packetsOut,
          lastEvent: transport.lastEvent,
          rcnt: emu.io.read16(0x4000134) & 0xffff,
          siocnt: emu.io.read16(0x4000128) & 0xffff,
        });
      }
    };
    const resume = () => audio.resume();
    window.addEventListener('pointerdown', resume);
    raf = requestAnimationFrame(loop);
    return () => { alive = false; cancelAnimationFrame(raf); window.removeEventListener('pointerdown', resume); };
  }, [phase]);

  const leave = () => {
    transportRef.current?.disconnect();
    transportRef.current = null;
    onExit();
  };

  if (phase === 'live') {
    return (
      <div className="min-h-screen bg-[#0b0b0f] text-gray-200 p-4">
        <div className="flex items-center justify-between mb-3">
          <div className="text-sm font-semibold">Online (Wireless) · <span className="font-mono text-[12px] opacity-60">{status}</span></div>
          <div className="flex items-center gap-3 text-[11px] font-mono">
            <span className="opacity-50">room {roomInput}</span>
            <span className={snap.connected ? 'text-green-400' : 'text-yellow-400'}>{snap.connected ? '● peer' : '● solo'}</span>
            <button onClick={leave} className="btn">Leave</button>
          </div>
        </div>
        <div className="flex justify-center">
          <canvas ref={canvasRef} width={FB_W} height={FB_H} className="w-[600px] max-w-[92vw] rounded ring-1 ring-[#2a2a30]" style={{ imageRendering: 'pixelated' }} />
        </div>
        <p className="text-[11px] opacity-50 text-center mt-3">
          One player creates a Union Room in-game, the other joins it. (Pokémon Center 2F → middle desk.)
        </p>
        <div className="max-w-md mx-auto mt-4 text-[10px] font-mono bg-[#0e0e12] rounded p-3 space-y-1">
          <div className="opacity-60 uppercase tracking-wider text-[9px]">wireless debug</div>
          <div className="flex flex-wrap gap-x-4 gap-y-1 opacity-80">
            <span className={snap.connected ? 'text-green-400' : 'text-gray-500'}>peer={String(snap.connected)}</span>
            <span>pkt out={snap.packetsOut}</span>
            <span>pkt in={snap.packetsIn}</span>
            <span>RCNT=0x{snap.rcnt.toString(16).padStart(4, '0')}</span>
            <span>SIOCNT=0x{snap.siocnt.toString(16).padStart(4, '0')}</span>
          </div>
          <div className="opacity-70">last: {snap.lastEvent || '—'}</div>
          <SioTrace emuRef={emuRef} />
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-[#0b0b0f] text-gray-200 p-6">
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-lg font-semibold">Online (Wireless) — beta</h1>
        <button onClick={onExit} className="btn">← Games</button>
      </div>
      <div className="max-w-sm space-y-3 text-[12px]">
        <div className="opacity-70">Both players must run the same game. Create a room and share the code, or join one. Then connect via the in-game Union Room.</div>
        <div className="flex items-center gap-2 text-[11px]">
          <span className="opacity-50">Save:</span>
          <span className="opacity-80">{customSaveName ?? 'your single-player save'}</span>
          <label className="btn !text-[10px] !py-0.5 cursor-pointer">
            Load .sav
            <input type="file" accept=".sav,.dat,.srm" className="hidden" onChange={async (e) => {
              const f = e.target.files?.[0]; if (!f) return;
              customSaveRef.current = new Uint8Array(await f.arrayBuffer());
              setCustomSaveName(f.name); e.target.value = '';
            }} />
          </label>
        </div>
        <button onClick={() => start(makeRoomCode(), true)} disabled={phase === 'connecting'} className="btn btn-primary w-full py-2.5">Create room</button>
        <div className="text-[10px] opacity-40 text-center">or</div>
        <div className="flex gap-2">
          <input value={roomInput} onChange={(e) => setRoomInput(e.target.value.toUpperCase())} placeholder="Room code" maxLength={6} className="input flex-1 font-mono uppercase tracking-widest" />
          <button onClick={() => start(roomInput, false)} disabled={phase === 'connecting' || !roomInput.trim()} className="btn px-4">Join</button>
        </div>
        {status && <div className="text-[11px] opacity-70">{status}</div>}
      </div>
    </div>
  );
}

function makeRoomCode(): string {
  const A = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789';
  let s = '';
  const buf = new Uint8Array(6);
  crypto.getRandomValues(buf);
  for (let i = 0; i < 6; i++) s += A[buf[i] % A.length];
  return s;
}

// Drains the wireless adapter's (sent → reply) SPI word log, so we can see
// exactly what the game shifts to the adapter and how the HLE replies — and
// thus where detection diverges. The adapter always logs; just reproduce the
// error, then Copy. (Copy drains the buffer, so copy right after the error.)
function SioTrace({ emuRef }: { emuRef: React.MutableRefObject<WasmEmulator | null> }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    const emu = emuRef.current;
    if (!emu) return;
    const t = emu.wlTrace();
    const hex = (v: number) => '0x' + (v >>> 0).toString(16).padStart(8, '0');
    const lines = t.map(([sent, reply], i) => `${i}\t${hex(sent)}\t→ ${hex(reply)}`);
    const text = `# wireless adapter SPI trace (${t.length} exchanges)\n#\tsent\treply\n${lines.join('\n')}`;
    navigator.clipboard?.writeText(text).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200); }).catch(() => {});
  };
  return (
    <div className="flex items-center gap-2 pt-1">
      <span className="opacity-60">adapter SPI log:</span>
      <button onClick={copy} className="btn !text-[10px] !py-0.5">{copied ? 'Copied!' : 'Copy adapter trace'}</button>
    </div>
  );
}
