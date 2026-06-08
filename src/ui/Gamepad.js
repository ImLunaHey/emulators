import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useRef } from 'react';
import { Key } from '../io/keypad';
function HoldButton({ keypad, k, className, children, ariaLabel }) {
    const ref = useRef(null);
    const press = (e) => {
        e.preventDefault();
        keypad.press(k);
        ref.current?.setPointerCapture(e.pointerId);
    };
    const release = (e) => {
        e.preventDefault();
        keypad.release(k);
    };
    // data-key carries the Key enum NAME ("A", "UP", etc.) so the
    // useKeypadHighlight() loop can re-derive which button corresponds
    // to which bit and update .pressed for any input source.
    return (_jsx("button", { ref: ref, type: "button", className: `gp-btn ${className ?? ''}`, "data-key": Key[k], "aria-label": ariaLabel, onPointerDown: press, onPointerUp: release, onPointerCancel: release, onPointerLeave: release, children: children }));
}
export function Gamepad({ keypad }) {
    return (_jsxs("div", { className: "gamepad", children: [_jsxs("div", { className: "dpad", children: [_jsx(HoldButton, { keypad: keypad, k: Key.UP, ariaLabel: "up", children: "\u25B2" }), _jsxs("div", { className: "dpad-row", children: [_jsx(HoldButton, { keypad: keypad, k: Key.LEFT, ariaLabel: "left", children: "\u25C0" }), _jsx("button", { type: "button", className: "gp-btn dpad-mid", disabled: true }), _jsx(HoldButton, { keypad: keypad, k: Key.RIGHT, ariaLabel: "right", children: "\u25B6" })] }), _jsx(HoldButton, { keypad: keypad, k: Key.DOWN, ariaLabel: "down", children: "\u25BC" })] }), _jsxs("div", { className: "shoulder", children: [_jsx(HoldButton, { keypad: keypad, k: Key.L, className: "gp-shoulder", children: "L" }), _jsx(HoldButton, { keypad: keypad, k: Key.R, className: "gp-shoulder", children: "R" })] }), _jsxs("div", { className: "middle", children: [_jsx(HoldButton, { keypad: keypad, k: Key.SELECT, className: "gp-pill", children: "SELECT" }), _jsx(HoldButton, { keypad: keypad, k: Key.START, className: "gp-pill", children: "START" })] }), _jsxs("div", { className: "ab", children: [_jsx(HoldButton, { keypad: keypad, k: Key.B, className: "gp-ab gp-b", children: "B" }), _jsx(HoldButton, { keypad: keypad, k: Key.A, className: "gp-ab gp-a", children: "A" })] })] }));
}
