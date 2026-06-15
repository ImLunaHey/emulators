import initAtari, { WasmAtari2600 } from '../../core-atari2600/pkg/atari2600_core.js';
import { WasmCorePlayer } from './WasmCorePlayer';

// Atari 2600 (VCS) player (WasmAtari2600). 160x192.
//
// Bit order: Up, Down, Left, Right, Fire, Reset, Select.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  z: 1 << 4,        // Fire
  Enter: 1 << 5,    // Reset (console switch)
  Shift: 1 << 6,    // Select (console switch)
};

export function Atari2600Player({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initAtari}
      create={() => new WasmAtari2600()}
      keyMap={KEY}
      audioChannels={1}
      hint="z=Fire · enter=Reset shift=Select · arrows"
    />
  );
}
