import { useRef, useState } from 'react';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import { defaultShouldDehydrateQuery } from '@tanstack/react-query';
import type { Emulator } from '../emulator';
import { WasmEmulator } from './wasmEmulator';
import { AudioSink } from './audio';
import { EmuContext } from './EmuContext';
import { HomeScreen } from './HomeScreen';
import { PlayerPage } from './PlayerPage';
import { NdsPlayer } from './NdsPlayer';
import { NesPlayer } from './NesPlayer';
import { SmsPlayer } from './SmsPlayer';
import { GbcPlayer } from './GbcPlayer';
import { Ps1Player } from './Ps1Player';
import { XboxPlayer } from './XboxPlayer';
import { SnesPlayer } from './SnesPlayer';
import { GenesisPlayer } from './GenesisPlayer';
import { PcePlayer } from './PcePlayer';
import { Atari2600Player } from './Atari2600Player';
import { NgpcPlayer } from './NgpcPlayer';
import { WonderSwanPlayer } from './WonderSwanPlayer';
import { VirtualBoyPlayer } from './VirtualBoyPlayer';
import { N64Player } from './N64Player';
import { DuoGbaPlayer } from './DuoGbaPlayer';
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

  // Dev/local route: `?duo` opens the single-page two-player GBA link harness
  // (two cores, one clock, in-memory cable — Pokémon trading on one machine).
  // Kept out of the main flow so it can't affect normal play.
  if (typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('duo')) {
    return (
      <ToastProvider>
        <DuoGbaPlayer onExit={() => { window.location.href = window.location.pathname; }} />
      </ToastProvider>
    );
  }

  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{
        persister,
        maxAge: 7 * 24 * 60 * 60 * 1000,
        // Keep queries flagged meta.persist: false out of localStorage (e.g. the
        // ~512 KB PS1 BIOS bytes, which would blow the quota and serialize poorly).
        dehydrateOptions: {
          shouldDehydrateQuery: (q) =>
            q.meta?.persist !== false && defaultShouldDehydrateQuery(q),
        },
      }}
    >
      <EmuContext.Provider value={{ emu: emuRef.current, audio: audioRef.current }}>
        <ToastProvider>
          {playing ? (
            playing.system === 'nds' ? (
              <NdsPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'nes' ? (
              <NesPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'sms' || playing.system === 'gg' ? (
              <SmsPlayer romId={playing.id} system={playing.system} onExit={() => setPlaying(null)} />
            ) : playing.system === 'gbc' || playing.system === 'gb' ? (
              <GbcPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'ps1' ? (
              <Ps1Player romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'xbox' ? (
              <XboxPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'snes' ? (
              <SnesPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'genesis' ? (
              <GenesisPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'pce' ? (
              <PcePlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'atari2600' ? (
              <Atari2600Player romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'ngpc' ? (
              <NgpcPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'wonderswan' ? (
              <WonderSwanPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'virtualboy' ? (
              <VirtualBoyPlayer romId={playing.id} onExit={() => setPlaying(null)} />
            ) : playing.system === 'n64' ? (
              <N64Player romId={playing.id} onExit={() => setPlaying(null)} />
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
