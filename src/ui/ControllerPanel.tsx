import { useEffect, useRef, useState } from 'react';
import { GBA_KEYS, labelFor, loadMap, resetMap, saveMap } from './controllerMap';

interface PadSnapshot {
  id: string;
  mapping: string;
  buttons: boolean[];
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
  const rafRef = useRef(0);
  const lastPressedRef = useRef<Set<number>>(new Set());

  // Live poll the gamepad while open.
  useEffect(() => {
    if (!open) return;
    let stop = false;
    const tick = () => {
      if (stop) return;
      rafRef.current = requestAnimationFrame(tick);
      const pads = navigator.getGamepads ? navigator.getGamepads() : [];
      let pad: Gamepad | null = null;
      for (const p of pads) { if (p && p.connected) { pad = p; break; } }
      if (!pad) { setSnap(null); return; }
      const pressed = pad.buttons.map((b) => b.pressed);
      setSnap({
        id: pad.id,
        mapping: pad.mapping || 'sony',
        buttons: pressed,
        axes: Array.from(pad.axes),
      });
      // If we're in remap mode, look for any newly-pressed button to bind.
      if (editingKey !== null) {
        for (let i = 0; i < pressed.length; i++) {
          if (pressed[i] && !lastPressedRef.current.has(i)) {
            const newBindings = { ...bindings, [editingKey]: i };
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

  // Esc cancels remap mode.
  useEffect(() => {
    if (editingKey === null) return;
    const onEsc = (e: KeyboardEvent) => { if (e.key === 'Escape') setEditingKey(null); };
    window.addEventListener('keydown', onEsc);
    return () => window.removeEventListener('keydown', onEsc);
  }, [editingKey]);

  if (!open) return null;

  const onReset = () => {
    const def = resetMap(mapping);
    setBindings(def);
    onChange?.();
  };

  return (
    <div
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[1000]"
      onClick={onClose}
    >
      <div
        className="bg-[#14141a] border border-[#2a2a30] rounded-lg p-5 w-[760px] max-h-[88vh] overflow-y-auto shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex justify-between items-center mb-4 pb-3 border-b border-[#2a2a30]">
          <div>
            <div className="text-sm font-bold tracking-wider">Controller</div>
            <div className="text-[11px] opacity-50 mt-0.5">
              {snap ? `${snap.id} · ${snap.mapping}` : 'No controller detected'}
            </div>
          </div>
          <button onClick={onClose} className="bg-transparent border-0 text-[#d8d8e0] text-xl cursor-pointer px-2 hover:text-white">×</button>
        </div>

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
          </>
        )}
      </div>
    </div>
  );
}

// Stylized PS5/Xbox-like layout. Buttons light up in their physical
// positions when pressed, instead of a flat grid of numbered cells.
function PadDiagram({ snap }: { snap: PadSnapshot }) {
  const isStandard = snap.mapping === 'standard';
  const map = (std: number, sony: number) => isStandard ? std : sony;
  const lit = (idx: number) => snap.buttons[idx];

  // D-pad direction is detected from EITHER discrete buttons OR the HID
  // hat axes 6/7. Many controllers (PS5 on macOS Safari most notably)
  // skip the discrete-button form entirely and only emit the hat axes.
  const hx = snap.axes[6] ?? 0;
  const hy = snap.axes[7] ?? 0;
  const dUp    = lit(map(12, 14)) || hy < -0.5;
  const dDown  = lit(map(13, 15)) || hy >  0.5;
  const dLeft  = lit(map(14, 16)) || hx < -0.5;
  const dRight = lit(map(15, 17)) || hx >  0.5;

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
          // D-pad rows also light up from the hat-axis fallback so the
          // user can see that direction is reaching the browser even
          // when the controller doesn't expose discrete D-pad buttons.
          const hx = snap.axes[6] ?? 0;
          const hy = snap.axes[7] ?? 0;
          const axisLit =
            (name === 'D-pad Up'    && hy < -0.5) ||
            (name === 'D-pad Down'  && hy >  0.5) ||
            (name === 'D-pad Left'  && hx < -0.5) ||
            (name === 'D-pad Right' && hx >  0.5);
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
        Press Esc to cancel.
      </div>
    </div>
  );
}
