import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
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
import { getRomBytes, getSelectedRom, setSelectedRom } from './romStore';
import { AudioSink } from './audio';
function bytesToBase64(bytes) {
    let bin = '';
    for (let i = 0; i < bytes.length; i++)
        bin += String.fromCharCode(bytes[i]);
    return btoa(bin);
}
function base64ToBytes(s) {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++)
        out[i] = bin.charCodeAt(i);
    return out;
}
const KEY_MAP = {
    ArrowUp: Key.UP, ArrowDown: Key.DOWN, ArrowLeft: Key.LEFT, ArrowRight: Key.RIGHT,
    z: Key.A, Z: Key.A,
    x: Key.B, X: Key.B,
    a: Key.L, A: Key.L,
    s: Key.R, S: Key.R,
    Enter: Key.START,
    Shift: Key.SELECT,
};
export function App() {
    const emuRef = useRef(null);
    if (!emuRef.current)
        emuRef.current = new Emulator();
    const emu = emuRef.current;
    const audioRef = useRef(null);
    if (!audioRef.current)
        audioRef.current = new AudioSink();
    const audio = audioRef.current;
    const [paused, setPaused] = useState(false);
    const [stats, setStats] = useState('— fps · — Mhz');
    const [log, setLog] = useState(['gba-recomp — pick a ROM to start']);
    const [headerInfo, setHeaderInfo] = useState('');
    const [showCp, setShowCp] = useState(false);
    const [mapVersion, setMapVersion] = useState(0);
    const [showLib, setShowLib] = useState(false);
    const [currentRom, setCurrentRom] = useState(null);
    const romBufRef = useRef(null);
    const saveKeyRef = useRef('');
    const append = (...args) => setLog((prev) => [...prev, args.map(String).join(' ')]);
    useGamepad({
        keypad: emu.keypad,
        onConnected: (name) => append(`controller connected: ${name}`),
        onDisconnected: (name) => append(`controller disconnected: ${name}`),
        mapVersion,
    });
    useKeypadHighlight(emu.keypad);
    // Boot a ROM by id (= IndexedDB key).
    const loadRomById = async (id, meta) => {
        const bytes = await getRomBytes(id);
        if (!bytes) {
            append(`no ROM stored for "${id}"`);
            return;
        }
        romBufRef.current = bytes;
        const title = new TextDecoder('ascii').decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
        const code = new TextDecoder('ascii').decode(bytes.subarray(0xAC, 0xB0));
        const saveKey = `gba-recomp:save:${code}`;
        saveKeyRef.current = saveKey;
        setHeaderInfo(`${title.trim()} · ${code}`);
        setCurrentRom(meta ?? { id, filename: title, title, code, size: bytes.length, addedAt: 0 });
        emu.loadRom(bytes);
        try {
            const raw = localStorage.getItem(saveKey);
            if (raw) {
                const arr = base64ToBytes(raw);
                emu.flash.loadSave(arr);
                append(`restored save (${arr.length} bytes)`);
            }
        }
        catch (e) {
            append('save restore failed:', e.message);
        }
        setSelectedRom(id);
        append(`loaded "${title.trim() || code}"`);
        let writeTimer = null;
        emu.flash.onChange = () => {
            if (writeTimer !== null)
                return;
            writeTimer = window.setTimeout(() => {
                writeTimer = null;
                try {
                    localStorage.setItem(saveKey, bytesToBase64(emu.flash.data));
                }
                catch (e) {
                    console.warn('Flash persist failed', e);
                }
            }, 250);
        };
    };
    // On first mount, auto-load the previously selected ROM if any.
    useEffect(() => {
        const id = getSelectedRom();
        if (id)
            loadRomById(id);
        else
            setShowLib(true);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);
    // Keyboard bindings + Web Audio unlock. Browsers refuse to start an
    // AudioContext without a user gesture, so we resume on any keypress
    // or pointer-down — the first interaction triggers it transparently.
    useEffect(() => {
        const down = (e) => {
            audio.resume();
            const k = KEY_MAP[e.key];
            if (k !== undefined) {
                emu.keypad.press(k);
                e.preventDefault();
            }
        };
        const up = (e) => {
            const k = KEY_MAP[e.key];
            if (k !== undefined) {
                emu.keypad.release(k);
                e.preventDefault();
            }
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
        if (!romBufRef.current)
            return;
        append('reset');
        emu.loadRom(romBufRef.current);
        try {
            const raw = localStorage.getItem(saveKeyRef.current);
            if (raw)
                emu.flash.loadSave(base64ToBytes(raw));
        }
        catch { /* ignore */ }
    };
    const onDownloadSave = () => {
        const blob = new Blob([emu.flash.data], { type: 'application/octet-stream' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `${currentRom?.code || 'gba'}.sav`;
        a.click();
        URL.revokeObjectURL(url);
        append('downloaded .sav file');
    };
    const onUploadSave = (e) => {
        const file = e.target.files?.[0];
        if (!file)
            return;
        file.arrayBuffer().then((buf) => {
            emu.flash.loadSave(new Uint8Array(buf));
            try {
                localStorage.setItem(saveKeyRef.current, bytesToBase64(emu.flash.data));
            }
            catch { /* ignore */ }
            append(`uploaded save (${buf.byteLength} bytes)`);
        });
        e.target.value = '';
    };
    const onClearSave = () => {
        if (!confirm('Delete the saved game data for this ROM?'))
            return;
        localStorage.removeItem(saveKeyRef.current);
        emu.flash.data.fill(0xFF);
        append('cleared save');
    };
    return (_jsxs(_Fragment, { children: [_jsxs("header", { className: "w-full max-w-[720px] flex justify-between items-baseline", children: [_jsx("h1", { className: "text-sm m-0 tracking-wide opacity-80", children: "GBA-RECOMP \u00B7 Hybrid WASM" }), _jsx("div", { className: "text-xs opacity-60", children: headerInfo || 'no ROM loaded' })] }), _jsx(Screen, { emu: emu, paused: paused, audio: audio, onStats: setStats }), _jsx("div", { className: "w-[720px] px-2 py-1 text-xs text-[var(--color-accent)] opacity-85 text-left", children: stats }), _jsx(Gamepad, { keypad: emu.keypad }), _jsxs("div", { className: "flex gap-3 text-xs opacity-90 items-center w-[720px] flex-wrap", children: [_jsx("button", { onClick: () => setShowLib(true), className: "btn-default", children: "ROM Library\u2026" }), _jsx("button", { onClick: () => setPaused((p) => !p), className: "btn-default", disabled: !currentRom, children: paused ? 'Resume' : 'Pause' }), _jsx("button", { onClick: onReset, className: "btn-default", disabled: !currentRom, children: "Reset" }), _jsx("button", { onClick: onDownloadSave, className: "btn-default", disabled: !currentRom, children: "Export .sav" }), _jsxs("label", { className: `btn-default cursor-pointer ${!currentRom ? 'opacity-50 pointer-events-none' : ''}`, children: ["Import .sav", _jsx("input", { type: "file", accept: ".sav,.bin", onChange: onUploadSave, className: "hidden" })] }), _jsx("button", { onClick: onClearSave, className: "btn-default", disabled: !currentRom, children: "Clear Save" }), _jsx("button", { onClick: () => setShowCp(true), className: "btn-default", children: "Controller\u2026" })] }), _jsx("div", { className: "flex gap-3 text-xs opacity-70 items-center w-[720px]", children: _jsx("span", { children: "keys: arrows \u00B7 z/x \u00B7 a/s \u00B7 enter/shift \u00B7 saves auto-persist to browser storage" }) }), _jsx(LogPane, { lines: log }), _jsx(ControllerPanel, { open: showCp, onClose: () => setShowCp(false), onChange: () => setMapVersion((v) => v + 1) }), _jsx(RomLibrary, { open: showLib, currentId: currentRom?.id ?? null, onClose: () => setShowLib(false), onSelect: (meta) => { setShowLib(false); loadRomById(meta.id, meta); }, onAppend: append })] }));
}
