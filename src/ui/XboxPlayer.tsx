import { useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import initXbox, { WasmXbox } from '../../core-xbox/pkg/xbox_core.js';
import { getRomBytes } from './romStore';
import { setBios } from './biosStore';
import { useXboxBios } from './hooks/useXboxBios';
import { usePlayerAudio } from './playerAudio';

// Original Xbox player. The Xbox is an x86 (Pentium III) PC and needs its 256 KB
// flash BIOS to boot — there is no freely-distributable image, so the user must
// supply one (stored in IndexedDB, remembered for all Xbox games). BIOS
// resolution lives in useXboxBios.
//
// NOTE: core-xbox is a FOUNDATION core. With a BIOS it single-steps x86 until it
// hits an unimplemented feature, then paints a green diagnostic crash screen — it
// does not run commercial games yet. The player path is wired end-to-end so the
// rest of the emulator (CPU/bus/GPU/audio) is exercised exactly like the others.

// Duke-controller bits (latched by the core; routed to USB when that lands).
const KEY: Record<string, number> = {
  Enter: 1 << 0, // Start
  Shift: 1 << 1, // Back
  ArrowUp: 1 << 2,
  ArrowDown: 1 << 3,
  ArrowLeft: 1 << 4,
  ArrowRight: 1 << 5,
  z: 1 << 6, // A
  x: 1 << 7, // B
  a: 1 << 8, // X
  s: 1 << 9, // Y
  q: 1 << 10, // White
  w: 1 << 11, // Black
  d: 1 << 12, // Left trigger
  f: 1 << 13, // Right trigger
};

export function XboxPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const audio = usePlayerAudio();
  const qc = useQueryClient();
  const { data: bios, isLoading } = useXboxBios();
  const phase = isLoading ? 'checking' : bios ? 'ready' : 'needbios';
  const [loadingRom, setLoadingRom] = useState(false);

  useEffect(() => {
    if (!bios) return;
    let alive = true;
    let raf = 0;
    let xbox: WasmXbox | null = null;
    setLoadingRom(true);
    (async () => {
      await initXbox();
      const rom = await getRomBytes(romId);
      if (!alive) return;
      xbox = new WasmXbox();
      xbox.load_bios(bios);
      if (rom) xbox.load_rom(rom); // moves the bytes into the core (no extra copy)
      if (!alive) return;
      setLoadingRom(false);
      const ctx = canvasRef.current!.getContext('2d')!;
      const loop = () => {
        if (!alive || !xbox) return;
        xbox.set_keys(keysRef.current >>> 0);
        xbox.run_frame();
        audio.push(xbox.drain_audio(), 48000, 2);
        const w = xbox.width();
        const h = xbox.height();
        const canvas = canvasRef.current!;
        if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
        ctx.putImageData(new ImageData(new Uint8ClampedArray(xbox.framebuffer()), w, h), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => { alive = false; cancelAnimationFrame(raf); xbox?.free(); };
  }, [bios, romId]);

  useEffect(() => {
    const down = (e: KeyboardEvent) => { if (e.key in KEY) { e.preventDefault(); keysRef.current |= KEY[e.key]; } };
    const up = (e: KeyboardEvent) => { if (e.key in KEY) keysRef.current &= ~KEY[e.key]; };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, []);

  const onBiosFile = async (file: File | undefined) => {
    if (!file) return;
    const bytes = new Uint8Array(await file.arrayBuffer());
    await setBios('xbox', bytes);
    // Re-resolve so the boot effect picks up the user BIOS.
    qc.setQueryData(['xbox-bios'], bytes);
  };

  return (
    <div className="relative w-full min-h-screen flex flex-col items-center justify-center gap-2 p-3">
      <button onClick={onExit} className="btn self-start mb-1" title="Home">⌂ Home</button>
      {phase === 'needbios' ? (
        <label className="flex flex-col items-center gap-3 border-2 border-dashed border-[var(--color-border)] rounded-xl px-8 py-10 text-center cursor-pointer max-w-md">
          <div className="text-2xl opacity-60">🟢</div>
          <div className="text-sm">Xbox needs a <b>flash BIOS</b> (a 256 KB <code>.bin</code>) to boot.</div>
          <div className="text-xs opacity-50">Stored locally in your browser. Drop or pick one — it's remembered for all Xbox games.</div>
          <input
            type="file"
            className="hidden"
            onChange={(e) => onBiosFile(e.target.files?.[0])}
          />
          <span className="btn btn-primary mt-1">Choose BIOS file</span>
        </label>
      ) : (
        <>
          <div className="relative w-[min(94vw,720px)]">
            <canvas ref={canvasRef} width={640} height={480} className="w-full h-auto bg-black" style={{ imageRendering: 'pixelated' }} />
            {loadingRom && (
              <div className="absolute inset-0 flex items-center justify-center bg-black/70 text-sm">Loading…</div>
            )}
          </div>
          <div className="text-[10px] opacity-50 mt-1">z=A x=B a=X s=Y · q/w=White/Black d/f=LT/RT · enter=Start shift=Back · arrows</div>
        </>
      )}
    </div>
  );
}
