import { useEffect, useRef, useState } from 'react';
import { GBA_KEYS, labelFor, loadMap, resetMap, saveMap } from './controllerMap';
import {
  keyLabel,
  loadKeyboardMapRaw,
  resetKeyboardMap,
  saveKeyboardMap,
} from './keyboardMap';
import { ErrorBoundary } from './ErrorBoundary';
import { Modal } from './Modal';
import { padSelect } from './padSelect';

interface PadInfo { index: number; id: string; mapping: string; }
// Trim the long HID descriptor down to something readable for a chip.
function shortPadName(id: string): string {
  return id.replace(/\s*\(.*\)\s*/, '').trim() || id;
}

interface PadSnapshot {
  id: string;
  mapping: string;
  buttons: boolean[];
  buttonValues: number[];
  axes: number[];
}

interface Props {
  open: boolean;
  onClose: () => void;
  onChange?: () => void;
}

export function ControllerPanel({ open, onClose, onChange }: Props) {
  const [snap, setSnap] = useState<PadSnapshot | null>(null);
  const [editingKey, setEditingKey] = useState<number | null>(null);
  const [mapping, setMapping] = useState<string>('sony');
  const [bindings, setBindings] = useState<Record<number, number>>({});
  const [pads, setPads] = useState<PadInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(padSelect.get());
  // Keyboard remapping: GBA-key → KeyboardEvent.key. Separate from the
  // gamepad bindings above; persisted via keyboardMap.ts.
  const [kbBindings, setKbBindings] = useState<Record<number, string>>(() => loadKeyboardMapRaw());
  const [kbEditingKey, setKbEditingKey] = useState<number | null>(null);
  const rafRef = useRef(0);
  const lastPressedRef = useRef<Set<number>>(new Set());

  // Keep the local highlight of the active selection in sync with the
  // shared store (also updated by auto-fallback on disconnect).
  useEffect(() => padSelect.subscribe(() => setActiveId(padSelect.get())), []);

  // Live poll the gamepad while open.
  useEffect(() => {
    if (!open) return;
    let stop = false;
    const tick = () => {
      if (stop) return;
      rafRef.current = requestAnimationFrame(tick);
      const raw = navigator.getGamepads ? navigator.getGamepads() : [];
      const connected: Gamepad[] = [];
      for (const p of raw) if (p && p.connected) connected.push(p);
      setPads(connected.map((p) => ({ index: p.index, id: p.id, mapping: p.mapping || 'sony' })));
      // Inspect the pinned pad if it's connected; otherwise the first.
      const wantId = padSelect.get();
      const pad: Gamepad | null = (wantId && connected.find((p) => p.id === wantId)) || connected[0] || null;
      if (!pad) { setSnap(null); return; }
      const pressed = pad.buttons.map((b) => b.pressed);
      const values  = pad.buttons.map((b) => b.value);
      setSnap({
        id: pad.id,
        mapping: pad.mapping || 'sony',
        buttons: pressed,
        buttonValues: values,
        axes: Array.from(pad.axes),
      });
      // If we're in remap mode, look for any newly-pressed button to bind.
      // If that button was already bound to a different GBA key, unbind it
      // there first — the same physical button can't drive two actions.
      if (editingKey !== null) {
        for (let i = 0; i < pressed.length; i++) {
          if (pressed[i] && !lastPressedRef.current.has(i)) {
            const newBindings: Record<number, number> = {};
            for (const [k, v] of Object.entries(bindings)) {
              if (v !== i) newBindings[Number(k)] = v;
            }
            newBindings[editingKey] = i;
            setBindings(newBindings);
            saveMap(pad.mapping || 'sony', newBindings);
            setEditingKey(null);
            onChange?.();
            break;
          }
        }
      }
      lastPressedRef.current = new Set(pressed.map((p, i) => p ? i : -1).filter(i => i >= 0));
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => { stop = true; cancelAnimationFrame(rafRef.current); };
  }, [open, editingKey, bindings, onChange]);

  // Re-load bindings whenever the mapping flavor (standard/sony) changes.
  useEffect(() => {
    if (!snap) return;
    const m = snap.mapping;
    if (m !== mapping) {
      setMapping(m);
      setBindings(loadMap(m));
    }
  }, [snap?.mapping, mapping, snap]);

  // Esc while editing UNBINDS the key being edited (and exits edit mode).
  useEffect(() => {
    if (editingKey === null) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      const next: Record<number, number> = {};
      for (const [k, v] of Object.entries(bindings)) {
        if (Number(k) !== editingKey) next[Number(k)] = v;
      }
      setBindings(next);
      saveMap(mapping, next);
      setEditingKey(null);
      onChange?.();
    };
    window.addEventListener('keydown', onEsc);
    return () => window.removeEventListener('keydown', onEsc);
  }, [editingKey, bindings, mapping, onChange]);

  const onReset = () => {
    const def = resetMap(mapping);
    setBindings(def);
    onChange?.();
  };

  // Reload the keyboard bindings whenever the panel re-opens, in case
  // they changed elsewhere.
  useEffect(() => {
    if (open) setKbBindings(loadKeyboardMapRaw());
  }, [open]);

  // While editing a keyboard binding, the next keydown binds that key.
  // Esc cancels (leaves the binding untouched). If the pressed key was
  // already bound elsewhere, drop it there first — one key per action.
  useEffect(() => {
    if (kbEditingKey === null) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === 'Escape') { setKbEditingKey(null); return; }
      const next: Record<number, string> = {};
      for (const [k, v] of Object.entries(kbBindings)) {
        if (v !== e.key && Number(k) !== kbEditingKey) next[Number(k)] = v;
      }
      next[kbEditingKey] = e.key;
      setKbBindings(next);
      saveKeyboardMap(next);
      setKbEditingKey(null);
      onChange?.();
    };
    // Capture phase so we win over the player's gameplay handler.
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [kbEditingKey, kbBindings, onChange]);

  const onKbReset = () => {
    setKbBindings(resetKeyboardMap());
    setKbEditingKey(null);
    onChange?.();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Controller"
      subtitle={snap ? `${snap.id} · ${snap.mapping}` : 'No controller detected'}
      size="lg"
    >
      <ErrorBoundary label="Controller" onClose={onClose} variant="inline">
        {pads.length > 0 && (
          <div className="mb-4">
            <div className="flex items-center justify-between mb-2">
              <div className="eyebrow">Active controller</div>
              <div className="text-[10px] opacity-50">{pads.length} connected</div>
            </div>
            <div className="flex flex-wrap gap-2">
              <button
                onClick={() => padSelect.set(null)}
                className={activeId === null ? 'btn btn-primary !text-[10px]' : 'btn !text-[10px]'}
                title="Any connected controller drives input"
              >Any</button>
              {pads.map((p) => (
                <button
                  key={p.index}
                  onClick={() => padSelect.set(p.id)}
                  className={activeId === p.id ? 'btn btn-primary !text-[10px]' : 'btn !text-[10px]'}
                  title={p.id}
                >P{p.index + 1} · {shortPadName(p.id)}</button>
              ))}
            </div>
            <div className="text-[10px] opacity-50 mt-2 leading-relaxed">
              “Any” lets every connected pad control the game (pass the
              controller around). Pick one to pin input to it. The diagram
              and bindings below follow the selected pad.
            </div>
          </div>
        )}

        {!snap ? (
          <div className="py-12 text-center opacity-50 text-xs leading-relaxed">
            Connect a gamepad and press any button to wake it.<br />
            <span className="text-[10px] opacity-70">PS5 DualSense, Xbox controller, or any USB/Bluetooth pad will work.</span>
          </div>
        ) : (
          <>
            <PadDiagram snap={snap} />
            <BindingTable
              snap={snap}
              bindings={bindings}
              editingKey={editingKey}
              onEdit={setEditingKey}
              onReset={onReset}
            />
            <RawSignals snap={snap} />
          </>
        )}

        <KeyboardBindingTable
          bindings={kbBindings}
          editingKey={kbEditingKey}
          onEdit={setKbEditingKey}
          onReset={onKbReset}
        />
      </ErrorBoundary>
    </Modal>
  );
}

// HID single-axis hat decoder. Standard 8-position encoding:
//   -1.00=U, -0.71=UR, -0.43=R, -0.14=DR, 0.14=D, 0.43=DL, 0.71=L, 1.00=UL
// Idle sits out-of-range (|v|>1.05). Returns null when the axis is
// either idle, missing, or doesn't look like a hat.
function decodeHat(v: number | undefined): 'U' | 'D' | 'L' | 'R' | 'UR' | 'DR' | 'DL' | 'UL' | null {
  if (typeof v !== 'number' || Math.abs(v) > 1.05) return null;
  const pos = Math.round((v + 1) * 7 / 2);
  return (['U', 'UR', 'R', 'DR', 'D', 'DL', 'L', 'UL'] as const)[pos] ?? null;
}

// Stylized PS5/Xbox-like layout. Buttons light up in their physical
// positions when pressed, instead of a flat grid of numbered cells.
function PadDiagram({ snap }: { snap: PadSnapshot }) {
  const isStandard = snap.mapping === 'standard';
  const map = (std: number, sony: number) => isStandard ? std : sony;
  const lit = (idx: number) => snap.buttons[idx];

  // D-pad direction is detected from THREE possible encodings:
  //   - discrete buttons (standard / sony) at the canonical indices
  //   - two-axis hat at axes 6/7 (X then Y)
  //   - single-axis HID hat at axis 4 or 9 (PS5 on Safari most notably)
  const hx = snap.axes[6] ?? 0;
  const hy = snap.axes[7] ?? 0;
  const hatPos = decodeHat(snap.axes[4]) ?? decodeHat(snap.axes[9]);
  const dUp    = lit(map(12, 14)) || hy < -0.5 || hatPos === 'U' || hatPos === 'UR' || hatPos === 'UL';
  const dDown  = lit(map(13, 15)) || hy >  0.5 || hatPos === 'D' || hatPos === 'DR' || hatPos === 'DL';
  const dLeft  = lit(map(14, 16)) || hx < -0.5 || hatPos === 'L' || hatPos === 'UL' || hatPos === 'DL';
  const dRight = lit(map(15, 17)) || hx >  0.5 || hatPos === 'R' || hatPos === 'UR' || hatPos === 'DR';

  // Stick deflection (for visualization).
  const ax = snap.axes[0] ?? 0;
  const ay = snap.axes[1] ?? 0;
  const rx = snap.axes[2] ?? 0;
  const ry = snap.axes[3] ?? 0;
  const sx = Math.max(-1, Math.min(1, ax)) * 8;
  const sy = Math.max(-1, Math.min(1, ay)) * 8;
  const sxr = Math.max(-1, Math.min(1, rx)) * 8;
  const syr = Math.max(-1, Math.min(1, ry)) * 8;

  return (
    <div className="my-4 px-6 py-8 bg-gradient-to-b from-[#0d0d12] to-[#0a0a0e] rounded-lg border border-[#1c1c20]">
      <div className="flex justify-between items-start gap-4">
        {/* Left cluster: D-pad + L1/L2 */}
        <div className="flex flex-col items-center gap-6 pt-2">
          <div className="flex gap-3">
            <PadBtn lit={lit(map(6, 6))} label="L2" small />
            <PadBtn lit={lit(map(4, 4))} label="L1" small />
          </div>
          <div className="relative w-[80px] h-[80px]">
            <div className="absolute left-1/2 top-0 -translate-x-1/2"><DpadBtn lit={dUp} dir="up" /></div>
            <div className="absolute left-0 top-1/2 -translate-y-1/2"><DpadBtn lit={dLeft} dir="left" /></div>
            <div className="absolute right-0 top-1/2 -translate-y-1/2"><DpadBtn lit={dRight} dir="right" /></div>
            <div className="absolute left-1/2 bottom-0 -translate-x-1/2"><DpadBtn lit={dDown} dir="down" /></div>
          </div>
        </div>

        {/* Center: select, start, sticks */}
        <div className="flex flex-col items-center gap-3 flex-1">
          <div className="flex gap-3 mt-4">
            <PadBtn lit={lit(map(8, 8))} label={isStandard ? 'View' : 'Share'} pill />
            <PadBtn lit={lit(map(9, 9))} label={isStandard ? 'Menu' : 'Options'} pill />
          </div>
          <div className="flex gap-6 mt-4">
            <Stick x={sx} y={sy} pressed={lit(map(10, 10))} />
            <Stick x={sxr} y={syr} pressed={lit(map(11, 11))} />
          </div>
        </div>

        {/* Right cluster: face + R1/R2 */}
        <div className="flex flex-col items-center gap-6 pt-2">
          <div className="flex gap-3">
            <PadBtn lit={lit(map(5, 5))} label="R1" small />
            <PadBtn lit={lit(map(7, 7))} label="R2" small />
          </div>
          <div className="relative w-[80px] h-[80px]">
            <div className="absolute left-1/2 top-0 -translate-x-1/2"><FaceBtn lit={lit(map(3, 3))} sym={isStandard ? 'Y' : '△'} color="green" /></div>
            <div className="absolute left-0 top-1/2 -translate-y-1/2"><FaceBtn lit={lit(map(2, 0))} sym={isStandard ? 'X' : '□'} color="purple" /></div>
            <div className="absolute right-0 top-1/2 -translate-y-1/2"><FaceBtn lit={lit(map(1, 2))} sym={isStandard ? 'B' : '○'} color="red" /></div>
            <div className="absolute left-1/2 bottom-0 -translate-x-1/2"><FaceBtn lit={lit(map(0, 1))} sym={isStandard ? 'A' : '✕'} color="blue" /></div>
          </div>
        </div>
      </div>
    </div>
  );
}

function PadBtn({ lit, label, small, pill }: { lit: boolean; label: string; small?: boolean; pill?: boolean }) {
  return (
    <div
      className={`flex items-center justify-center font-bold transition-all ${
        small ? 'w-12 h-7 text-[10px]' : 'h-7 px-3 text-[10px]'
      } ${pill ? 'rounded-full' : 'rounded-md'} ${
        lit ? 'bg-[#4a8aff] border border-[#6ea2ff] text-white shadow-[0_0_12px_rgba(74,138,255,0.6)]' : 'bg-[#1c1c22] border border-[#2a2a30] text-[#9a9aa6]'
      }`}
    >
      {label}
    </div>
  );
}

function DpadBtn({ lit, dir }: { lit: boolean; dir: 'up' | 'down' | 'left' | 'right' }) {
  const sym = dir === 'up' ? '▲' : dir === 'down' ? '▼' : dir === 'left' ? '◀' : '▶';
  return (
    <div
      className={`w-7 h-7 flex items-center justify-center text-xs font-bold rounded-sm transition-all ${
        lit ? 'bg-[#4a8aff] border border-[#6ea2ff] text-white shadow-[0_0_8px_rgba(74,138,255,0.6)]' : 'bg-[#1c1c22] border border-[#2a2a30] text-[#9a9aa6]'
      }`}
    >
      {sym}
    </div>
  );
}

const FACE_COLORS: Record<string, [string, string, string]> = {
  blue:   ['#2a4060', '#5080c0', 'rgba(80,128,192,0.7)'],
  red:    ['#602a2a', '#c05050', 'rgba(192,80,80,0.7)'],
  purple: ['#402a60', '#8050c0', 'rgba(128,80,192,0.7)'],
  green:  ['#2a6030', '#50c060', 'rgba(80,192,96,0.7)'],
};

function FaceBtn({ lit, sym, color }: { lit: boolean; sym: string; color: string }) {
  const [bgDim, bgLit, shadow] = FACE_COLORS[color];
  return (
    <div
      className="w-7 h-7 rounded-full flex items-center justify-center text-[11px] font-bold border-2 transition-all"
      style={{
        background: lit ? bgLit : bgDim,
        borderColor: lit ? bgLit : '#2a2a30',
        boxShadow: lit ? `0 0 12px ${shadow}` : 'none',
        color: lit ? '#fff' : '#9a9aa6',
      }}
    >
      {sym}
    </div>
  );
}

function Stick({ x, y, pressed }: { x: number; y: number; pressed: boolean }) {
  return (
    <div className="relative w-[44px] h-[44px] rounded-full bg-[#1c1c22] border border-[#2a2a30] flex items-center justify-center">
      <div
        className={`w-5 h-5 rounded-full transition-all ${pressed ? 'bg-[#6ea2ff] shadow-[0_0_8px_rgba(110,162,255,0.6)]' : 'bg-[#404048]'}`}
        style={{ transform: `translate(${x}px, ${y}px)` }}
      />
    </div>
  );
}

// Raw button + axis dump. The diagram + binding table assume a
// recognizable layout, but if your controller routes D-pad / shoulders
// through unusual indices, the only way to find them is to press
// physically and see which row down here flashes. Press your D-pad
// while watching this section and tell me which index moves.
function RawSignals({ snap }: { snap: PadSnapshot }) {
  const buttons = snap.buttons;
  const values = snap.buttonValues;
  const axes = snap.axes;
  return (
    <details className="mt-4" open>
      <summary className="text-[10px] uppercase tracking-widest opacity-50 cursor-pointer select-none">
        Raw signals
      </summary>
      <div className="mt-2 grid grid-cols-2 gap-3">
        <div>
          <div className="text-[10px] opacity-50 mb-1">Buttons ({buttons.length})</div>
          <div className="grid grid-cols-6 gap-1">
            {buttons.map((b, i) => (
              <div
                key={i}
                className={`text-center text-[10px] py-1 rounded border ${
                  b ? 'bg-[#4a8aff] border-[#6ea2ff] text-white' : 'bg-[#1c1c22] border-[#2a2a30]'
                }`}
                title={values[i] > 0 ? `value=${values[i].toFixed(2)}` : undefined}
              >
                <div className="font-bold">{i}</div>
                {values[i] > 0 && values[i] < 1 ? (
                  <div className="text-[8px] opacity-70">{values[i].toFixed(2)}</div>
                ) : null}
              </div>
            ))}
          </div>
        </div>
        <div>
          <div className="text-[10px] opacity-50 mb-1">Axes ({axes.length})</div>
          <div className="flex flex-col gap-1">
            {axes.map((v, i) => {
              const active = Math.abs(v) > 0.05;
              return (
                <div key={i} className="grid grid-cols-[24px_1fr_40px] gap-2 items-center text-[10px]">
                  <div className={`text-right ${active ? 'text-[#9be7ff]' : 'opacity-50'}`}>{i}</div>
                  <div className="relative h-2 bg-[#1c1c22] border border-[#2a2a30] rounded-sm overflow-hidden">
                    <div className="absolute left-1/2 top-0 bottom-0 w-px bg-[#2a2a30]" />
                    <div
                      className="absolute top-0 bottom-0 min-w-px"
                      style={{
                        left: v < 0 ? `${50 + v * 50}%` : '50%',
                        width: `${Math.abs(v) * 50}%`,
                        background: v < 0 ? '#5ba8ff' : '#ff7858',
                      }}
                    />
                  </div>
                  <div className={`font-mono ${active ? 'text-white' : 'opacity-50'}`}>{v.toFixed(2)}</div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </details>
  );
}

function BindingTable({
  snap, bindings, editingKey, onEdit, onReset,
}: {
  snap: PadSnapshot;
  bindings: Record<number, number>;
  editingKey: number | null;
  onEdit: (key: number | null) => void;
  onReset: () => void;
}) {
  return (
    <div className="mt-4">
      <div className="flex items-center justify-between mb-2">
        <div className="text-[10px] uppercase tracking-widest opacity-50">Bindings</div>
        <button
          onClick={onReset}
          className="text-[10px] uppercase tracking-wider opacity-50 hover:opacity-100 bg-transparent border-0 cursor-pointer"
        >reset to defaults</button>
      </div>
      <div className="grid grid-cols-2 gap-x-3 gap-y-1.5">
        {GBA_KEYS.map(({ key, name }) => {
          const buttonIdx = bindings[key];
          const isEditing = editingKey === key;
          // D-pad rows also light up from the two-axis hat (6/7) or the
          // single-axis HID hat (4 / 9) when the controller routes D-pad
          // through axes instead of discrete buttons.
          const hx = snap.axes[6] ?? 0;
          const hy = snap.axes[7] ?? 0;
          const hatPos = decodeHat(snap.axes[4]) ?? decodeHat(snap.axes[9]);
          const axisLit =
            (name === 'D-pad Up'    && (hy < -0.5 || hatPos === 'U' || hatPos === 'UR' || hatPos === 'UL')) ||
            (name === 'D-pad Down'  && (hy >  0.5 || hatPos === 'D' || hatPos === 'DR' || hatPos === 'DL')) ||
            (name === 'D-pad Left'  && (hx < -0.5 || hatPos === 'L' || hatPos === 'UL' || hatPos === 'DL')) ||
            (name === 'D-pad Right' && (hx >  0.5 || hatPos === 'R' || hatPos === 'UR' || hatPos === 'DR'));
          const isLit = (buttonIdx !== undefined && snap.buttons[buttonIdx]) || axisLit;
          return (
            <button
              key={key}
              onClick={() => onEdit(isEditing ? null : key)}
              className={`flex justify-between items-center px-3 py-2 rounded-md text-[11px] border transition-all cursor-pointer text-left ${
                isEditing
                  ? 'bg-[#3a3a5a] border-[#5060a0] animate-pulse'
                  : isLit
                  ? 'bg-[#2a4a3a] border-[#4a8a6a]'
                  : 'bg-[#1c1c22] border-[#2a2a30] hover:bg-[#24242a]'
              }`}
            >
              <span className="font-medium">{name}</span>
              <span className={`text-[10px] ${isEditing ? 'text-[#ffeb70]' : 'opacity-70'}`}>
                {isEditing
                  ? 'press any button…'
                  : buttonIdx !== undefined
                  ? labelFor(buttonIdx, snap.mapping)
                  : '— unbound —'}
              </span>
            </button>
          );
        })}
      </div>
      <div className="mt-4 text-[10px] opacity-50 leading-relaxed">
        Click a binding to rebind it, then press any button on the controller.
        Esc unbinds the key. If you bind a button that's already in use, it's
        auto-removed from its previous binding.
      </div>
    </div>
  );
}

// Keyboard remapping table — mirrors BindingTable, but each row binds a
// physical keyboard key (KeyboardEvent.key) to a GBA key. Clicking a row
// enters "press a key…" mode; the next keydown binds it.
function KeyboardBindingTable({
  bindings, editingKey, onEdit, onReset,
}: {
  bindings: Record<number, string>;
  editingKey: number | null;
  onEdit: (key: number | null) => void;
  onReset: () => void;
}) {
  return (
    <div className="mt-6 pt-4 border-t border-[#1c1c20]">
      <div className="flex items-center justify-between mb-2">
        <div className="text-[10px] uppercase tracking-widest opacity-50">Keyboard</div>
        <button
          onClick={onReset}
          className="text-[10px] uppercase tracking-wider opacity-50 hover:opacity-100 bg-transparent border-0 cursor-pointer"
        >reset to defaults</button>
      </div>
      <div className="grid grid-cols-2 gap-x-3 gap-y-1.5">
        {GBA_KEYS.map(({ key, name }) => {
          const isEditing = editingKey === key;
          return (
            <button
              key={key}
              onClick={() => onEdit(isEditing ? null : key)}
              className={`flex justify-between items-center px-3 py-2 rounded-md text-[11px] border transition-all cursor-pointer text-left ${
                isEditing
                  ? 'bg-[#3a3a5a] border-[#5060a0] animate-pulse'
                  : 'bg-[#1c1c22] border-[#2a2a30] hover:bg-[#24242a]'
              }`}
            >
              <span className="font-medium">{name}</span>
              <span className={`text-[10px] ${isEditing ? 'text-[#ffeb70]' : 'opacity-70'}`}>
                {isEditing ? 'press a key…' : keyLabel(bindings[key])}
              </span>
            </button>
          );
        })}
      </div>
      <div className="mt-4 text-[10px] opacity-50 leading-relaxed">
        Click a binding to rebind it, then press a key. Esc cancels. Binding a
        key that's already in use moves it from its previous action. Tab,
        F2/F4, “.” and Backspace are reserved for emulator controls.
      </div>
    </div>
  );
}
