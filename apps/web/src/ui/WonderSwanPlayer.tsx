import initWs, { WasmWonderSwan } from '@emulators/wonderswan';
import { WasmCorePlayer } from './WasmCorePlayer';

// WonderSwan / WonderSwan Color player (WasmWonderSwan). 224x144. The core's
// constructor takes a `color` flag selecting WSC (12-bit palette, 64 KiB RAM)
// vs mono WS; we default to Color since it's the superset and most ROMs in the
// catalog are color-capable.
//
// Bit order: Up, Down, Left, Right, A, B, Start.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  x: 1 << 4,        // A
  z: 1 << 5,        // B
  Enter: 1 << 6,    // Start
};

export function WonderSwanPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  return (
    <WasmCorePlayer
      romId={romId}
      onExit={onExit}
      init={initWs}
      create={() => new WasmWonderSwan(true)}
      keyMap={KEY}
      audioChannels={1}
      hint="x=A z=B · enter=Start · arrows"
    />
  );
}
