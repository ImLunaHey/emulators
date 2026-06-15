import initVb, { WasmVb } from '../../core-virtualboy/pkg/virtualboy_core.js';
import { WasmCorePlayer } from './WasmCorePlayer';

// Virtual Boy player (WasmVb). 384x224 (left-eye framebuffer; the core renders a
// single luminance image, so no stereo compositing on the host).
//
// set_keys takes the core's logical input::KEY_* bits directly (it remaps to the
// hardware word internally). The VB has two D-pads; arrows drive the LEFT pad
// (primary movement), i/j/k/l drive the RIGHT pad. Bits: LU0 LD1 LL2 LR3 RU4
// RD5 RL6 RR7 A8 B9 L10 R11 Start12 Select13.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,     // left D-pad up
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  i: 1 << 4,           // right D-pad up
  k: 1 << 5,
  j: 1 << 6,
  l: 1 << 7,
  x: 1 << 8,           // A
  z: 1 << 9,           // B
  q: 1 << 10,          // L
  w: 1 << 11,          // R
  Enter: 1 << 12,      // Start
  Shift: 1 << 13,      // Select
};

export function VirtualBoyPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initVb}
      create={() => new WasmVb()}
      keyMap={KEY}
      audioChannels={1}
      hint="x=A z=B · q/w=L/R · enter=Start shift=Select · arrows=L-pad ijkl=R-pad"
    />
  );
}
