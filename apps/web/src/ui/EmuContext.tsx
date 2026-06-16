import { createContext, useContext } from 'react';
import type { Emulator } from '../emulator';
import type { AudioSink } from './audio';

// Sharing the emulator + audio sink across the router. The root App
// creates them via useRef so they survive navigation; both LibraryPage
// and PlayerPage read them through this context.
export interface EmuContextValue {
  emu: Emulator;
  audio: AudioSink;
}

export const EmuContext = createContext<EmuContextValue | null>(null);

export function useEmu(): EmuContextValue {
  const v = useContext(EmuContext);
  if (!v) throw new Error('useEmu() must be used inside <EmuContext.Provider>');
  return v;
}
