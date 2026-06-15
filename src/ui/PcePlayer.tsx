import initPce, { WasmPce } from '../../core-pce/pkg/pce_core.js';
import { WasmCorePlayer } from './WasmCorePlayer';

// PC Engine / TurboGrafx-16 player (WasmPce). 256x224.
//
// Bit order: Up, Down, Left, Right, I, II, Select, Run.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  x: 1 << 4,        // I
  z: 1 << 5,        // II
  Shift: 1 << 6,    // Select
  Enter: 1 << 7,    // Run
};

export function PcePlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initPce}
      create={() => new WasmPce()}
      keyMap={KEY}
      audioChannels={1}
      hint="x=I z=II · enter=Run shift=Select · arrows"
    />
  );
}
