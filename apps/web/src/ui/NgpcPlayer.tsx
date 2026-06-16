import initNgpc, { WasmNgpc } from '@emulators/ngpc';
import { WasmCorePlayer } from './WasmCorePlayer';

// Neo Geo Pocket Color player (WasmNgpc). 160x152.
//
// Bit order: Up, Down, Left, Right, A, B, Option.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  z: 1 << 4,        // A
  x: 1 << 5,        // B
  Enter: 1 << 6,    // Option
};

export function NgpcPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initNgpc}
      create={() => new WasmNgpc()}
      keyMap={KEY}
      audioChannels={1}
      hint="z=A x=B · enter=Option · arrows"
    />
  );
}
