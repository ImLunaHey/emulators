import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import { useEffect, useRef, useState } from 'react';
import { GBA_KEYS, labelFor, loadMap, resetMap, saveMap } from './controllerMap';
export function ControllerPanel({ open, onClose, onChange }) {
    const [snap, setSnap] = useState(null);
    const [editingKey, setEditingKey] = useState(null);
    const [mapping, setMapping] = useState('sony');
    const [bindings, setBindings] = useState({});
    const rafRef = useRef(0);
    const lastPressedRef = useRef(new Set());
    // Live poll the gamepad while open.
    useEffect(() => {
        if (!open)
            return;
        let stop = false;
        const tick = () => {
            if (stop)
                return;
            rafRef.current = requestAnimationFrame(tick);
            const pads = navigator.getGamepads ? navigator.getGamepads() : [];
            let pad = null;
            for (const p of pads) {
                if (p && p.connected) {
                    pad = p;
                    break;
                }
            }
            if (!pad) {
                setSnap(null);
                return;
            }
            const pressed = pad.buttons.map((b) => b.pressed);
            const values = pad.buttons.map((b) => b.value);
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
                        const newBindings = {};
                        for (const [k, v] of Object.entries(bindings)) {
                            if (v !== i)
                                newBindings[Number(k)] = v;
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
        if (!snap)
            return;
        const m = snap.mapping;
        if (m !== mapping) {
            setMapping(m);
            setBindings(loadMap(m));
        }
    }, [snap?.mapping, mapping, snap]);
    // Esc while editing UNBINDS the key being edited (and exits edit mode).
    useEffect(() => {
        if (editingKey === null)
            return;
        const onEsc = (e) => {
            if (e.key !== 'Escape')
                return;
            e.preventDefault();
            const next = {};
            for (const [k, v] of Object.entries(bindings)) {
                if (Number(k) !== editingKey)
                    next[Number(k)] = v;
            }
            setBindings(next);
            saveMap(mapping, next);
            setEditingKey(null);
            onChange?.();
        };
        window.addEventListener('keydown', onEsc);
        return () => window.removeEventListener('keydown', onEsc);
    }, [editingKey, bindings, mapping, onChange]);
    if (!open)
        return null;
    const onReset = () => {
        const def = resetMap(mapping);
        setBindings(def);
        onChange?.();
    };
    return (_jsx("div", { className: "fixed inset-0 bg-black/70 flex items-center justify-center z-[1000]", onClick: onClose, children: _jsxs("div", { className: "bg-[#14141a] border border-[#2a2a30] rounded-lg p-5 w-[760px] max-h-[88vh] overflow-y-auto shadow-2xl", onClick: (e) => e.stopPropagation(), children: [_jsxs("div", { className: "flex justify-between items-center mb-4 pb-3 border-b border-[#2a2a30]", children: [_jsxs("div", { children: [_jsx("div", { className: "text-sm font-bold tracking-wider", children: "Controller" }), _jsx("div", { className: "text-[11px] opacity-50 mt-0.5", children: snap ? `${snap.id} · ${snap.mapping}` : 'No controller detected' })] }), _jsx("button", { onClick: onClose, className: "bg-transparent border-0 text-[#d8d8e0] text-xl cursor-pointer px-2 hover:text-white", children: "\u00D7" })] }), !snap ? (_jsxs("div", { className: "py-12 text-center opacity-50 text-xs leading-relaxed", children: ["Connect a gamepad and press any button to wake it.", _jsx("br", {}), _jsx("span", { className: "text-[10px] opacity-70", children: "PS5 DualSense, Xbox controller, or any USB/Bluetooth pad will work." })] })) : (_jsxs(_Fragment, { children: [_jsx(PadDiagram, { snap: snap }), _jsx(BindingTable, { snap: snap, bindings: bindings, editingKey: editingKey, onEdit: setEditingKey, onReset: onReset }), _jsx(RawSignals, { snap: snap })] }))] }) }));
}
// HID single-axis hat decoder. Standard 8-position encoding:
//   -1.00=U, -0.71=UR, -0.43=R, -0.14=DR, 0.14=D, 0.43=DL, 0.71=L, 1.00=UL
// Idle sits out-of-range (|v|>1.05). Returns null when the axis is
// either idle, missing, or doesn't look like a hat.
function decodeHat(v) {
    if (typeof v !== 'number' || Math.abs(v) > 1.05)
        return null;
    const pos = Math.round((v + 1) * 7 / 2);
    return ['U', 'UR', 'R', 'DR', 'D', 'DL', 'L', 'UL'][pos] ?? null;
}
// Stylized PS5/Xbox-like layout. Buttons light up in their physical
// positions when pressed, instead of a flat grid of numbered cells.
function PadDiagram({ snap }) {
    const isStandard = snap.mapping === 'standard';
    const map = (std, sony) => isStandard ? std : sony;
    const lit = (idx) => snap.buttons[idx];
    // D-pad direction is detected from THREE possible encodings:
    //   - discrete buttons (standard / sony) at the canonical indices
    //   - two-axis hat at axes 6/7 (X then Y)
    //   - single-axis HID hat at axis 4 or 9 (PS5 on Safari most notably)
    const hx = snap.axes[6] ?? 0;
    const hy = snap.axes[7] ?? 0;
    const hatPos = decodeHat(snap.axes[4]) ?? decodeHat(snap.axes[9]);
    const dUp = lit(map(12, 14)) || hy < -0.5 || hatPos === 'U' || hatPos === 'UR' || hatPos === 'UL';
    const dDown = lit(map(13, 15)) || hy > 0.5 || hatPos === 'D' || hatPos === 'DR' || hatPos === 'DL';
    const dLeft = lit(map(14, 16)) || hx < -0.5 || hatPos === 'L' || hatPos === 'UL' || hatPos === 'DL';
    const dRight = lit(map(15, 17)) || hx > 0.5 || hatPos === 'R' || hatPos === 'UR' || hatPos === 'DR';
    // Stick deflection (for visualization).
    const ax = snap.axes[0] ?? 0;
    const ay = snap.axes[1] ?? 0;
    const rx = snap.axes[2] ?? 0;
    const ry = snap.axes[3] ?? 0;
    const sx = Math.max(-1, Math.min(1, ax)) * 8;
    const sy = Math.max(-1, Math.min(1, ay)) * 8;
    const sxr = Math.max(-1, Math.min(1, rx)) * 8;
    const syr = Math.max(-1, Math.min(1, ry)) * 8;
    return (_jsx("div", { className: "my-4 px-6 py-8 bg-gradient-to-b from-[#0d0d12] to-[#0a0a0e] rounded-lg border border-[#1c1c20]", children: _jsxs("div", { className: "flex justify-between items-start gap-4", children: [_jsxs("div", { className: "flex flex-col items-center gap-6 pt-2", children: [_jsxs("div", { className: "flex gap-3", children: [_jsx(PadBtn, { lit: lit(map(6, 6)), label: "L2", small: true }), _jsx(PadBtn, { lit: lit(map(4, 4)), label: "L1", small: true })] }), _jsxs("div", { className: "relative w-[80px] h-[80px]", children: [_jsx("div", { className: "absolute left-1/2 top-0 -translate-x-1/2", children: _jsx(DpadBtn, { lit: dUp, dir: "up" }) }), _jsx("div", { className: "absolute left-0 top-1/2 -translate-y-1/2", children: _jsx(DpadBtn, { lit: dLeft, dir: "left" }) }), _jsx("div", { className: "absolute right-0 top-1/2 -translate-y-1/2", children: _jsx(DpadBtn, { lit: dRight, dir: "right" }) }), _jsx("div", { className: "absolute left-1/2 bottom-0 -translate-x-1/2", children: _jsx(DpadBtn, { lit: dDown, dir: "down" }) })] })] }), _jsxs("div", { className: "flex flex-col items-center gap-3 flex-1", children: [_jsxs("div", { className: "flex gap-3 mt-4", children: [_jsx(PadBtn, { lit: lit(map(8, 8)), label: isStandard ? 'View' : 'Share', pill: true }), _jsx(PadBtn, { lit: lit(map(9, 9)), label: isStandard ? 'Menu' : 'Options', pill: true })] }), _jsxs("div", { className: "flex gap-6 mt-4", children: [_jsx(Stick, { x: sx, y: sy, pressed: lit(map(10, 10)) }), _jsx(Stick, { x: sxr, y: syr, pressed: lit(map(11, 11)) })] })] }), _jsxs("div", { className: "flex flex-col items-center gap-6 pt-2", children: [_jsxs("div", { className: "flex gap-3", children: [_jsx(PadBtn, { lit: lit(map(5, 5)), label: "R1", small: true }), _jsx(PadBtn, { lit: lit(map(7, 7)), label: "R2", small: true })] }), _jsxs("div", { className: "relative w-[80px] h-[80px]", children: [_jsx("div", { className: "absolute left-1/2 top-0 -translate-x-1/2", children: _jsx(FaceBtn, { lit: lit(map(3, 3)), sym: isStandard ? 'Y' : '△', color: "green" }) }), _jsx("div", { className: "absolute left-0 top-1/2 -translate-y-1/2", children: _jsx(FaceBtn, { lit: lit(map(2, 0)), sym: isStandard ? 'X' : '□', color: "purple" }) }), _jsx("div", { className: "absolute right-0 top-1/2 -translate-y-1/2", children: _jsx(FaceBtn, { lit: lit(map(1, 2)), sym: isStandard ? 'B' : '○', color: "red" }) }), _jsx("div", { className: "absolute left-1/2 bottom-0 -translate-x-1/2", children: _jsx(FaceBtn, { lit: lit(map(0, 1)), sym: isStandard ? 'A' : '✕', color: "blue" }) })] })] })] }) }));
}
function PadBtn({ lit, label, small, pill }) {
    return (_jsx("div", { className: `flex items-center justify-center font-bold transition-all ${small ? 'w-12 h-7 text-[10px]' : 'h-7 px-3 text-[10px]'} ${pill ? 'rounded-full' : 'rounded-md'} ${lit ? 'bg-[#4a8aff] border border-[#6ea2ff] text-white shadow-[0_0_12px_rgba(74,138,255,0.6)]' : 'bg-[#1c1c22] border border-[#2a2a30] text-[#9a9aa6]'}`, children: label }));
}
function DpadBtn({ lit, dir }) {
    const sym = dir === 'up' ? '▲' : dir === 'down' ? '▼' : dir === 'left' ? '◀' : '▶';
    return (_jsx("div", { className: `w-7 h-7 flex items-center justify-center text-xs font-bold rounded-sm transition-all ${lit ? 'bg-[#4a8aff] border border-[#6ea2ff] text-white shadow-[0_0_8px_rgba(74,138,255,0.6)]' : 'bg-[#1c1c22] border border-[#2a2a30] text-[#9a9aa6]'}`, children: sym }));
}
const FACE_COLORS = {
    blue: ['#2a4060', '#5080c0', 'rgba(80,128,192,0.7)'],
    red: ['#602a2a', '#c05050', 'rgba(192,80,80,0.7)'],
    purple: ['#402a60', '#8050c0', 'rgba(128,80,192,0.7)'],
    green: ['#2a6030', '#50c060', 'rgba(80,192,96,0.7)'],
};
function FaceBtn({ lit, sym, color }) {
    const [bgDim, bgLit, shadow] = FACE_COLORS[color];
    return (_jsx("div", { className: "w-7 h-7 rounded-full flex items-center justify-center text-[11px] font-bold border-2 transition-all", style: {
            background: lit ? bgLit : bgDim,
            borderColor: lit ? bgLit : '#2a2a30',
            boxShadow: lit ? `0 0 12px ${shadow}` : 'none',
            color: lit ? '#fff' : '#9a9aa6',
        }, children: sym }));
}
function Stick({ x, y, pressed }) {
    return (_jsx("div", { className: "relative w-[44px] h-[44px] rounded-full bg-[#1c1c22] border border-[#2a2a30] flex items-center justify-center", children: _jsx("div", { className: `w-5 h-5 rounded-full transition-all ${pressed ? 'bg-[#6ea2ff] shadow-[0_0_8px_rgba(110,162,255,0.6)]' : 'bg-[#404048]'}`, style: { transform: `translate(${x}px, ${y}px)` } }) }));
}
// Raw button + axis dump. The diagram + binding table assume a
// recognizable layout, but if your controller routes D-pad / shoulders
// through unusual indices, the only way to find them is to press
// physically and see which row down here flashes. Press your D-pad
// while watching this section and tell me which index moves.
function RawSignals({ snap }) {
    const buttons = snap.buttons;
    const values = snap.buttonValues;
    const axes = snap.axes;
    return (_jsxs("details", { className: "mt-4", open: true, children: [_jsx("summary", { className: "text-[10px] uppercase tracking-widest opacity-50 cursor-pointer select-none", children: "Raw signals" }), _jsxs("div", { className: "mt-2 grid grid-cols-2 gap-3", children: [_jsxs("div", { children: [_jsxs("div", { className: "text-[10px] opacity-50 mb-1", children: ["Buttons (", buttons.length, ")"] }), _jsx("div", { className: "grid grid-cols-6 gap-1", children: buttons.map((b, i) => (_jsxs("div", { className: `text-center text-[10px] py-1 rounded border ${b ? 'bg-[#4a8aff] border-[#6ea2ff] text-white' : 'bg-[#1c1c22] border-[#2a2a30]'}`, title: values[i] > 0 ? `value=${values[i].toFixed(2)}` : undefined, children: [_jsx("div", { className: "font-bold", children: i }), values[i] > 0 && values[i] < 1 ? (_jsx("div", { className: "text-[8px] opacity-70", children: values[i].toFixed(2) })) : null] }, i))) })] }), _jsxs("div", { children: [_jsxs("div", { className: "text-[10px] opacity-50 mb-1", children: ["Axes (", axes.length, ")"] }), _jsx("div", { className: "flex flex-col gap-1", children: axes.map((v, i) => {
                                    const active = Math.abs(v) > 0.05;
                                    return (_jsxs("div", { className: "grid grid-cols-[24px_1fr_40px] gap-2 items-center text-[10px]", children: [_jsx("div", { className: `text-right ${active ? 'text-[#9be7ff]' : 'opacity-50'}`, children: i }), _jsxs("div", { className: "relative h-2 bg-[#1c1c22] border border-[#2a2a30] rounded-sm overflow-hidden", children: [_jsx("div", { className: "absolute left-1/2 top-0 bottom-0 w-px bg-[#2a2a30]" }), _jsx("div", { className: "absolute top-0 bottom-0 min-w-px", style: {
                                                            left: v < 0 ? `${50 + v * 50}%` : '50%',
                                                            width: `${Math.abs(v) * 50}%`,
                                                            background: v < 0 ? '#5ba8ff' : '#ff7858',
                                                        } })] }), _jsx("div", { className: `font-mono ${active ? 'text-white' : 'opacity-50'}`, children: v.toFixed(2) })] }, i));
                                }) })] })] })] }));
}
function BindingTable({ snap, bindings, editingKey, onEdit, onReset, }) {
    return (_jsxs("div", { className: "mt-4", children: [_jsxs("div", { className: "flex items-center justify-between mb-2", children: [_jsx("div", { className: "text-[10px] uppercase tracking-widest opacity-50", children: "Bindings" }), _jsx("button", { onClick: onReset, className: "text-[10px] uppercase tracking-wider opacity-50 hover:opacity-100 bg-transparent border-0 cursor-pointer", children: "reset to defaults" })] }), _jsx("div", { className: "grid grid-cols-2 gap-x-3 gap-y-1.5", children: GBA_KEYS.map(({ key, name }) => {
                    const buttonIdx = bindings[key];
                    const isEditing = editingKey === key;
                    // D-pad rows also light up from the two-axis hat (6/7) or the
                    // single-axis HID hat (4 / 9) when the controller routes D-pad
                    // through axes instead of discrete buttons.
                    const hx = snap.axes[6] ?? 0;
                    const hy = snap.axes[7] ?? 0;
                    const hatPos = decodeHat(snap.axes[4]) ?? decodeHat(snap.axes[9]);
                    const axisLit = (name === 'D-pad Up' && (hy < -0.5 || hatPos === 'U' || hatPos === 'UR' || hatPos === 'UL')) ||
                        (name === 'D-pad Down' && (hy > 0.5 || hatPos === 'D' || hatPos === 'DR' || hatPos === 'DL')) ||
                        (name === 'D-pad Left' && (hx < -0.5 || hatPos === 'L' || hatPos === 'UL' || hatPos === 'DL')) ||
                        (name === 'D-pad Right' && (hx > 0.5 || hatPos === 'R' || hatPos === 'UR' || hatPos === 'DR'));
                    const isLit = (buttonIdx !== undefined && snap.buttons[buttonIdx]) || axisLit;
                    return (_jsxs("button", { onClick: () => onEdit(isEditing ? null : key), className: `flex justify-between items-center px-3 py-2 rounded-md text-[11px] border transition-all cursor-pointer text-left ${isEditing
                            ? 'bg-[#3a3a5a] border-[#5060a0] animate-pulse'
                            : isLit
                                ? 'bg-[#2a4a3a] border-[#4a8a6a]'
                                : 'bg-[#1c1c22] border-[#2a2a30] hover:bg-[#24242a]'}`, children: [_jsx("span", { className: "font-medium", children: name }), _jsx("span", { className: `text-[10px] ${isEditing ? 'text-[#ffeb70]' : 'opacity-70'}`, children: isEditing
                                    ? 'press any button…'
                                    : buttonIdx !== undefined
                                        ? labelFor(buttonIdx, snap.mapping)
                                        : '— unbound —' })] }, key));
                }) }), _jsx("div", { className: "mt-4 text-[10px] opacity-50 leading-relaxed", children: "Click a binding to rebind it, then press any button on the controller. Esc unbinds the key. If you bind a button that's already in use, it's auto-removed from its previous binding." })] }));
}
