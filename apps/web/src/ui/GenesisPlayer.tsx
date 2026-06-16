import initGenesis, { WasmGenesis } from '@emulators/genesis';
import { WasmCorePlayer } from './WasmCorePlayer';

// Genesis / Mega Drive player (WasmGenesis). 320x224 (width can drop to 256).
//
// Bit order (io::KEY_*): Up, Down, Left, Right, A, B, C, Start, X, Y, Z, Mode.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  a: 1 << 4,        // A
  z: 1 << 5,        // B
  x: 1 << 6,        // C
  Enter: 1 << 7,    // Start
  q: 1 << 8,        // X
  s: 1 << 9,        // Y
  w: 1 << 10,       // Z
  Shift: 1 << 11,   // Mode
};

export function GenesisPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initGenesis}
      create={() => new WasmGenesis()}
      keyMap={KEY}
      audioChannels={1}
      hint="a=A z=B x=C · q/s/w=X/Y/Z · enter=Start shift=Mode · arrows"
    />
  );
}
