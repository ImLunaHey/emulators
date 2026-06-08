import { useRef } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { Emulator } from '../emulator';
import { AudioSink } from './audio';
import { EmuContext } from './EmuContext';
import { LibraryPage } from './LibraryPage';
import { PlayerPage } from './PlayerPage';

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
    <EmuContext.Provider value={{ emu: emuRef.current, audio: audioRef.current }}>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<LibraryPage />} />
          <Route path="/play/:romId" element={<PlayerPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </BrowserRouter>
    </EmuContext.Provider>
  );
}
