import { useEffect, useState } from 'react';
import type { Emulator } from '../emulator';
import { SignalTransport, type LinkBridge } from '../io/sio-signal';
import { LocalLoopback } from '../io/sio';
import { Modal } from './Modal';

interface Props {
  open: boolean;
  emu: Emulator;
  onClose: () => void;
}

// Cache the live transport on the Sio so closing/re-opening the panel
// doesn't drop the connection. There's only ever one link active at a
// time, so a sentinel key is fine.
function getActive(sio: Emulator['io']['sio']): SignalTransport | null {
  const t = sio.transport;
  return t instanceof SignalTransport ? t : null;
}

// Tiny base32-like generator for default room codes. We want something
// readable enough to dictate over Discord — six chars from a 32-char
// alphabet ≈ 30 bits, enough that two random rooms basically never
// collide for the lifetime of a session.
function makeRoomCode(): string {
  const ALPHABET = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789';   // no 0/1/I/L/O
  let s = '';
  const buf = new Uint8Array(6);
  crypto.getRandomValues(buf);
  for (let i = 0; i < 6; i++) s += ALPHABET[buf[i] % ALPHABET.length];
  return s;
}

export function LinkPanel({ open, emu, onClose }: Props) {
  const [roomInput, setRoomInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Re-render on peer events. The transport itself is the source of
  // truth (live in emu.io.sio.transport); we just nudge React.
  const [, setTick] = useState(0);
  const nudge = () => setTick((t) => t + 1);

  // Keep our UI in sync with peer join/leave even if the panel was open
  // when the events happened.
  useEffect(() => {
    if (!open) return;
    const t = getActive(emu.io.sio);
    if (!t) return;
    const prevJoin = t.onPeerJoin;
    const prevLeave = t.onPeerLeave;
    t.onPeerJoin = (id) => { prevJoin?.(id); nudge(); };
    t.onPeerLeave = (id) => { prevLeave?.(id); nudge(); };
    return () => {
      t.onPeerJoin = prevJoin;
      t.onPeerLeave = prevLeave;
    };
  }, [open, emu]);

  const active = getActive(emu.io.sio);
  const connected = active?.isConnected() ?? false;

  const onCreate = async () => {
    setError(null);
    setBusy(true);
    const code = makeRoomCode();
    setRoomInput(code);
    try {
      await connectTo(emu, code, true);
      nudge();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const onJoin = async () => {
    if (!roomInput.trim()) { setError('Enter a room code'); return; }
    setError(null);
    setBusy(true);
    try {
      await connectTo(emu, roomInput.trim().toUpperCase(), false);
      nudge();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const onDisconnect = async () => {
    setBusy(true);
    try {
      await disconnect(emu);
      nudge();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Link Cable"
      subtitle={active ? (connected ? 'peer connected' : 'waiting for peer…') : 'not connected'}
      size="sm"
    >
      {!active && (
        <div className="text-[11px] space-y-3">
          <div className="opacity-70 leading-relaxed">
            Connects two browsers over WebRTC. One side creates a room and
            shares the code; the other side joins with it. Trades and
            turn-based play work; real-time racing is wobbly until the next
            sync upgrade lands.
          </div>
          <button
            onClick={onCreate}
            disabled={busy}
            className="btn btn-primary w-full py-2.5"
          >Create room</button>
          <div className="text-[10px] opacity-40 text-center">or</div>
          <div className="flex gap-2">
            <input
              value={roomInput}
              onChange={(e) => setRoomInput(e.target.value.toUpperCase())}
              placeholder="Room code"
              maxLength={6}
              className="input flex-1 font-mono uppercase tracking-widest"
            />
            <button onClick={onJoin} disabled={busy} className="btn px-4">Join</button>
          </div>
        </div>
      )}

      {active && (
        <div className="text-[11px] space-y-3">
          <div className="flex items-center gap-2">
            <div className="opacity-50">Room</div>
            <div className="font-mono text-base tracking-widest select-all well px-3 py-1.5">
              {roomInput || '—'}
            </div>
            <button
              onClick={() => navigator.clipboard?.writeText(roomInput).catch(() => {})}
              className="btn text-[10px]"
              disabled={!roomInput}
            >Copy</button>
          </div>
          <div className="flex items-center gap-2">
            <div className="opacity-50">Status</div>
            <div className={connected ? 'text-green-400' : 'text-yellow-400'}>
              {connected ? '● connected' : '● searching'}
            </div>
          </div>
          <LinkDebug emu={emu} />

          <button
            onClick={onDisconnect}
            disabled={busy}
            className="btn btn-danger w-full py-2"
          >Disconnect</button>
        </div>
      )}

      {error && <div className="mt-3 text-[10px] text-red-400">{error}</div>}
    </Modal>
  );
}

// Live debug strip — polls the Sio's view of the link state so we can
// see whether SD has actually gone high on the GBA side, what the
// peer's latest SIOMLT_SEND value is, and whether the game is
// actually exercising the link (SIOCNT writes change over time).
function LinkDebug({ emu }: { emu: Emulator }) {
  const [snap, setSnap] = useState({
    siocnt: 0, mlt: 0, m0: 0, m1: 0, master: true,
    ieSio: false, ifSio: false, seq: 0,
  });
  useEffect(() => {
    const id = setInterval(() => {
      const sio = emu.io.sio;
      // Read SIOCNT through the IO bridge so we get the post-overlay
      // value (SI/SD/ID computed from transport state).
      const siocnt = emu.io.read16(0x4000128) & 0xFFFF;
      setSnap({
        siocnt,
        mlt: sio.mltSend & 0xFFFF,
        m0: sio.multi[0] & 0xFFFF,
        m1: sio.multi[1] & 0xFFFF,
        master: sio.transport.isMaster(),
        ieSio: (emu.irq.ie & 0x80) !== 0,
        ifSio: (emu.irq.iflag & 0x80) !== 0,
        seq: sio.transferSeq,
      });
    }, 200);
    return () => clearInterval(id);
  }, [emu]);
  const bit = (b: number) => (snap.siocnt >> b) & 1;
  const Box = ({ label, on }: { label: string; on: boolean }) => (
    <div className={`px-1.5 py-0.5 rounded font-mono text-[10px] ${on ? 'bg-green-900 text-green-300' : 'bg-[#1c1c22] text-gray-500'}`}>
      {label}{on ? '=1' : '=0'}
    </div>
  );
  return (
    <div className="text-[10px] font-mono opacity-80 bg-[#0e0e12] rounded p-2 space-y-1.5">
      <div className="opacity-60 text-[9px] uppercase tracking-wider">GBA view</div>
      <div className="flex gap-1 flex-wrap">
        <Box label="SI" on={bit(2) === 1} />
        <Box label="SD" on={bit(3) === 1} />
        <Box label="START" on={bit(7) === 1} />
        <Box label="IRQ" on={bit(14) === 1} />
        <div className="px-1.5 py-0.5 rounded bg-[#1c1c22] text-gray-400">
          ID={((snap.siocnt >> 4) & 3)}
        </div>
        <div className="px-1.5 py-0.5 rounded bg-[#1c1c22] text-gray-400">
          mode={((snap.siocnt >> 12) & 3)}
        </div>
      </div>
      <div className="flex gap-1 flex-wrap">
        <Box label="IE.sio" on={snap.ieSio} />
        <Box label="IF.sio" on={snap.ifSio} />
      </div>
      <div className="flex gap-3 text-[10px] opacity-70 flex-wrap">
        <div>SIOCNT=0x{snap.siocnt.toString(16).padStart(4, '0')}</div>
        <div>SEND=0x{snap.mlt.toString(16).padStart(4, '0')}</div>
        <div>M0=0x{snap.m0.toString(16).padStart(4, '0')}</div>
        <div>M1=0x{snap.m1.toString(16).padStart(4, '0')}</div>
        <div>seq={snap.seq}</div>
        <div>{snap.master ? 'master' : 'slave'}</div>
      </div>
      <SioTracer emu={emu} />
      <IwramWatch emu={emu} />
    </div>
  );
}

// Watch one specific IWRAM address (8/16/32-bit writes). Used to track
// down the game-internal state byte that Mario Kart's cable detection
// gates on (IWRAM 0x03002af1 — see disassembly of 0x8011ef0). User
// enters a hex address, ticks the box, plays through cable detection,
// then dumps the captured writes to console with the PC + value of
// each one. Writes are logged ad-hoc into a window-scoped buffer so
// the patch doesn't have to grow the bus type. Toggling off removes
// the patch and clears the buffer.
function IwramWatch({ emu }: { emu: Emulator }) {
  const [on, setOn] = useState(false);
  const [addrHex, setAddrHex] = useState('03002af0');
  const [widthHex, setWidthHex] = useState('10');
  // The watch now lives in the wasm core (CPU writes happen inside wasm, so a
  // JS monkey-patch of bus.write would never see them). The adapter exposes
  // set/clear/read of a native write-watch; we drive that.
  const core = emu as unknown as {
    setWatch(lo: number, hi: number): void;
    clearWatch(): void;
    watchLog(): Array<{ pc: number; addr: number; size: number; val: number }>;
  };

  const toggle = () => {
    if (on) {
      core.clearWatch?.();
      setOn(false);
    } else {
      const addr = parseInt(addrHex.trim().replace(/^0x/, ''), 16) >>> 0;
      const width = Math.max(1, parseInt(widthHex.trim().replace(/^0x/, ''), 16) >>> 0);
      if (!Number.isFinite(addr) || !Number.isFinite(width)) return;
      core.setWatch?.(addr, addr + width - 1);
      setOn(true);
    }
  };

  const dump = () => {
    const entries = core.watchLog?.() ?? [];
    // Collapse consecutive identical writes into a run-length count, matching
    // the old monkey-patch dump.
    const rows: { pc: string; addr: string; size: number; val: string; n: number }[] = [];
    for (const e of entries) {
      const last = rows[rows.length - 1];
      const pc = '0x' + (e.pc >>> 0).toString(16);
      const addr = '0x' + (e.addr >>> 0).toString(16).padStart(8, '0');
      const val = '0x' + (e.val >>> 0).toString(16);
      if (last && last.pc === pc && last.addr === addr && last.size === e.size && last.val === val) {
        last.n++;
      } else {
        rows.push({ pc, addr, size: e.size, val, n: 1 });
      }
    }
    if (rows.length === 0) {
      console.log('[iwram-watch] no writes captured at 0x' + addrHex);
      return;
    }
    console.log(`[iwram-watch] ${rows.length} unique writes at 0x${addrHex}`);
    console.table(rows);
  };

  return (
    <div className="flex gap-2 items-center text-[10px] mt-1 flex-wrap">
      <label className="flex items-center gap-1 cursor-pointer">
        <input type="checkbox" checked={on} onChange={toggle} className="w-3 h-3" />
        <span className="opacity-60">IWRAM watch 0x</span>
      </label>
      <input
        value={addrHex}
        onChange={(e) => setAddrHex(e.target.value)}
        disabled={on}
        className="w-20 px-1 py-0 bg-[#1c1c22] border border-[#2a2a30] rounded text-[10px] font-mono"
      />
      <span className="opacity-60">+0x</span>
      <input
        value={widthHex}
        onChange={(e) => setWidthHex(e.target.value)}
        disabled={on}
        className="w-10 px-1 py-0 bg-[#1c1c22] border border-[#2a2a30] rounded text-[10px] font-mono"
      />
      <button onClick={dump} className="btn-default !text-[10px] !py-0.5">Dump</button>
    </div>
  );
}

// Tiny debug helper: toggle the Sio access trace, then dump it to the
// browser console as a console.table. Lets us see exactly which SIO
// register the game is reading/writing at each PC during cable
// detection. Consecutive identical accesses are collapsed to one row
// with an `n=N` run length so busy-polling doesn't drown the output.
function SioTracer({ emu }: { emu: Emulator }) {
  const [on, setOn] = useState(emu.io.sio.traceOn);
  const toggle = () => {
    emu.io.sio.traceOn = !emu.io.sio.traceOn;
    setOn(emu.io.sio.traceOn);
  };
  const dump = () => {
    const t = emu.io.sio.trace;
    if (t.length === 0) {
      // eslint-disable-next-line no-console
      console.log('[sio-trace] empty (enable the trace, then perform the action you want to capture)');
      return;
    }
    const rows = t.map((e) => ({
      seq: e.seq,
      pc: '0x' + (e.pc >>> 0).toString(16),
      op: e.op,
      addr: '0x040001' + e.off.toString(16).padStart(2, '0'),
      val: '0x' + e.val.toString(16).padStart(4, '0'),
      n: e.n,
    }));
    // eslint-disable-next-line no-console
    console.log(`[sio-trace] ${rows.length} unique entries (last ${emu.io.sio.trace[emu.io.sio.trace.length - 1].seq})`);
    // eslint-disable-next-line no-console
    console.table(rows);
  };
  const clear = () => { emu.io.sio.clearTrace(); };
  return (
    <div className="flex gap-2 items-center text-[10px] mt-1">
      <label className="flex items-center gap-1 cursor-pointer">
        <input type="checkbox" checked={on} onChange={toggle} className="w-3 h-3" />
        <span className="opacity-60">SIO trace</span>
      </label>
      <button onClick={dump} className="btn-default !text-[10px] !py-0.5">Dump to console</button>
      <button onClick={clear} className="btn-default !text-[10px] !py-0.5">Clear</button>
    </div>
  );
}

async function connectTo(emu: Emulator, code: string, isMaster: boolean): Promise<void> {
  // Drop any prior transport first — joining a new room while another
  // is live would leak the previous DataChannel + WebSocket.
  await disconnect(emu);
  // The transport drives the wasm core through the LinkBridge surface
  // (linkSetState/linkTakeOutgoing/linkDeliver/linkApplyRemote), which the
  // WasmEmulator adapter implements. `emu` is a WasmEmulator behind the
  // `Emulator` type alias, so reach the bridge methods through a cast.
  const t = new SignalTransport(emu as unknown as LinkBridge);
  // Assign now so isConnected() and the UI panel can already see "this
  // is the active transport" while we wait for the WS handshake. The
  // adapter's runFrame calls transport.pump?.() each frame. The adapter's
  // transport slot is `LinkTransportLike` (isMaster/isConnected/pump), which
  // SignalTransport satisfies; the `Emulator` alias types it as the wider TS
  // `LinkTransport`, so cast the assignment.
  emu.io.sio.transport = t as unknown as typeof emu.io.sio.transport;
  await t.connect({ roomId: code, isMaster });
}

async function disconnect(emu: Emulator): Promise<void> {
  const t = getActive(emu.io.sio);
  if (t) {
    await t.disconnect();
    emu.io.sio.transport = new LocalLoopback();
  }
}
