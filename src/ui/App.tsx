import { useRef } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import { Emulator } from '../emulator';
import { AudioSink } from './audio';
import { EmuContext } from './EmuContext';
import { LibraryPage } from './LibraryPage';
import { PlayerPage } from './PlayerPage';
import { RomDetailsPage } from './RomDetailsPage';
import { ToastProvider } from './Toast';
import { queryClient, persister } from './queryClient';

// Routes:
//   /              ROM library
//   /play/:romId   player view (Screen + controls + modal panels)
//   *              redirects to library
//
// The Emulator + AudioSink live at the root via useRef so they survive
// navigation between the two pages — switching from the player back to
// the library and into a different game doesn't tear down the audio
// context or the WASM JIT cache.
export function App() {
  const emuRef = useRef<Emulator | null>(null);
  if (!emuRef.current) emuRef.current = new Emulator();
  const audioRef = useRef<AudioSink | null>(null);
  if (!audioRef.current) audioRef.current = new AudioSink();

  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{ persister, maxAge: 7 * 24 * 60 * 60 * 1000 }}
    >
      <EmuContext.Provider value={{ emu: emuRef.current, audio: audioRef.current }}>
        <ToastProvider>
          <BrowserRouter>
            <Routes>
              <Route path="/" element={<LibraryPage />} />
              <Route path="/rom/:romId" element={<RomDetailsPage />} />
              <Route path="/play/:romId" element={<PlayerPage />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Routes>
          </BrowserRouter>
        </ToastProvider>
      </EmuContext.Provider>
    </PersistQueryClientProvider>
  );
}
