import { useRef, useState } from 'react';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import type { Emulator } from '../emulator';
import { WasmEmulator } from './wasmEmulator';
import { AudioSink } from './audio';
import { EmuContext } from './EmuContext';
import { HomeScreen } from './HomeScreen';
import { PlayerPage } from './PlayerPage';
import { NdsPlayer } from './NdsPlayer';
import { NesPlayer } from './NesPlayer';
import { ToastProvider } from './Toast';
import { queryClient, persister } from './queryClient';

// Single-view shell. Boots into the Rust-rendered home launcher (HomeScreen →
// WasmHome); selecting a game swaps to the player; the player's Home button
// swaps back. No router — the "route" is one piece of state.
//
// The Emulator + AudioSink live at the root via useRef so they survive the
// home↔player switch (the wasm core instance + audio context aren't torn down).
// react-query is still mounted because the in-game CheatsPanel uses it for the
// (host-side) known-cheats metadata lookup.
export function App() {
  const emuRef = useRef<Emulator | null>(null);
  if (!emuRef.current) emuRef.current = new WasmEmulator() as unknown as Emulator;
  const audioRef = useRef<AudioSink | null>(null);
  if (!audioRef.current) audioRef.current = new AudioSink();

  const [playing, setPlaying] = useState<{ id: string; system: string } | null>(null);

  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{ persister, maxAge: 7 * 24 * 60 * 60 * 1000 }}
    >
      <EmuContext.Provider value={{ emu: emuRef.current, audio: audioRef.current }}>
        <ToastProvider>
          {playing ? (
            playing.system === 'nds' ? (
              <NdsPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'nes' ? (
              <NesPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : (
              <PlayerPage romId={playing.id} onExit={() => setPlaying(null)} />
            )
          ) : (
            <HomeScreen onPlay={(id, system) => setPlaying({ id, system })} />
          )}
        </ToastProvider>
      </EmuContext.Provider>
    </PersistQueryClientProvider>
  );
}
