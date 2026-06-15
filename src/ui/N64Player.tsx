import initN64, { WasmN64 } from '../../core-n64/pkg/n64_core.js';
import { WasmCorePlayer } from './WasmCorePlayer';

// Nintendo 64 player (WasmN64). 320x240. Foundation core — it boots and ticks
// but does not render commercial games yet; wiring it in so it loads + runs.
// Audio is stubbed in the core (drain_audio always empty), but pushing it is
// harmless.
//
// Bit order (si::button): A0 B1 Z2 Start3 Dup4 Ddown5 Dleft6 Dright7 L8 R9
// Cup10 Cdown11 Cleft12 Cright13. Arrows drive the D-pad; ijkl the C-buttons.
const KEY: Record<string, number> = {
  x: 1 << 0,        // A
  z: 1 << 1,        // B
  c: 1 << 2,        // Z
  Enter: 1 << 3,    // Start
  ArrowUp: 1 << 4,
  ArrowDown: 1 << 5,
  ArrowLeft: 1 << 6,
  ArrowRight: 1 << 7,
  q: 1 << 8,        // L
  w: 1 << 9,        // R
  i: 1 << 10,       // C-up
  k: 1 << 11,       // C-down
  j: 1 << 12,       // C-left
  l: 1 << 13,       // C-right
};

export function N64Player({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initN64}
      create={() => new WasmN64()}
      keyMap={KEY}
      audioChannels={1}
      hint="x=A z=B c=Z · q/w=L/R · enter=Start · arrows=D-pad ijkl=C"
    />
  );
}
