import initSnes, { WasmSnes } from '@emulators/snes';
import { WasmCorePlayer } from './WasmCorePlayer';

// SNES player (WasmSnes). 256x224 base, but height varies; the generic player
// sizes the canvas from width()/height() each frame.
//
// Bit order (input::Key): B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R.
const KEY: Record<string, number> = {
  z: 1 << 0,        // B
  a: 1 << 1,        // Y
  Shift: 1 << 2,    // Select
  Enter: 1 << 3,    // Start
  ArrowUp: 1 << 4,
  ArrowDown: 1 << 5,
  ArrowLeft: 1 << 6,
  ArrowRight: 1 << 7,
  x: 1 << 8,        // A
  s: 1 << 9,        // X
  q: 1 << 10,       // L
  w: 1 << 11,       // R
};

export function SnesPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initSnes}
      create={() => new WasmSnes()}
      keyMap={KEY}
      audioChannels={1}
      hint="z=B x=A a=Y s=X · q/w=L/R · enter=Start shift=Select · arrows"
    />
  );
}
