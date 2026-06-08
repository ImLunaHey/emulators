import { useRef, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react';
import { Key, Keypad } from '../io/keypad';

interface Props { keypad: Keypad; }

interface BtnProps {
  keypad: Keypad;
  k: Key;
  className?: string;
  children?: ReactNode;
  ariaLabel?: string;
}

function HoldButton({ keypad, k, className, children, ariaLabel }: BtnProps) {
  const ref = useRef<HTMLButtonElement>(null);
  const press = (e: ReactPointerEvent) => {
    e.preventDefault();
    keypad.press(k);
    ref.current?.classList.add('pressed');
    ref.current?.setPointerCapture(e.pointerId);
  };
  const release = (e: ReactPointerEvent) => {
    e.preventDefault();
    keypad.release(k);
    ref.current?.classList.remove('pressed');
  };
  return (
    <button
      ref={ref}
      type="button"
      className={`gp-btn ${className ?? ''}`}
      aria-label={ariaLabel}
      onPointerDown={press}
      onPointerUp={release}
      onPointerCancel={release}
      onPointerLeave={release}
    >
      {children}
    </button>
  );
}

export function Gamepad({ keypad }: Props) {
  return (
    <div className="gamepad">
      <div className="dpad">
        <HoldButton keypad={keypad} k={Key.UP} ariaLabel="up">▲</HoldButton>
        <div className="dpad-row">
          <HoldButton keypad={keypad} k={Key.LEFT} ariaLabel="left">◀</HoldButton>
          <button type="button" className="gp-btn dpad-mid" disabled />
          <HoldButton keypad={keypad} k={Key.RIGHT} ariaLabel="right">▶</HoldButton>
        </div>
        <HoldButton keypad={keypad} k={Key.DOWN} ariaLabel="down">▼</HoldButton>
      </div>
      <div className="shoulder">
        <HoldButton keypad={keypad} k={Key.L} className="gp-shoulder">L</HoldButton>
        <HoldButton keypad={keypad} k={Key.R} className="gp-shoulder">R</HoldButton>
      </div>
      <div className="middle">
        <HoldButton keypad={keypad} k={Key.SELECT} className="gp-pill">SELECT</HoldButton>
        <HoldButton keypad={keypad} k={Key.START} className="gp-pill">START</HoldButton>
      </div>
      <div className="ab">
        <HoldButton keypad={keypad} k={Key.B} className="gp-ab gp-b">B</HoldButton>
        <HoldButton keypad={keypad} k={Key.A} className="gp-ab gp-a">A</HoldButton>
      </div>
    </div>
  );
}
