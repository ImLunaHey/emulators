import { useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import initPsx, { WasmPsx } from '../../core-ps1/pkg/ps1_core.js';
import { getRomBytes } from './romStore';
import { setBios } from './biosStore';
import { usePs1Bios } from './hooks/usePs1Bios';
import { usePlayerAudio } from './playerAudio';

// PlayStation player. The PS1 needs a BIOS ROM to boot real discs. We prefer a
// user-supplied BIOS (best compatibility, stored in IndexedDB), but fall back to
// a bundled open-source BIOS (/openbios.bin) when the user hasn't supplied one,
// so commercial discs boot without the user sourcing a file. BIOS resolution
// lives in usePs1Bios. Single screen; size comes from the core (PS1 res varies).

// Digital-pad bits (active-high, psx-spx order).
const KEY: Record<string, number> = {
  Shift: 1 << 0,   // Select
  Enter: 1 << 3,   // Start
  ArrowUp: 1 << 4,
  ArrowRight: 1 << 5,
  ArrowDown: 1 << 6,
  ArrowLeft: 1 << 7,
  q: 1 << 10,      // L1
  w: 1 << 11,      // R1
  s: 1 << 12,      // Triangle
  z: 1 << 13,      // Circle
  x: 1 << 14,      // Cross
  a: 1 << 15,      // Square
};

export function Ps1Player({ romId, onExit }: { romId: string; onExit: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const audio = usePlayerAudio();
  const qc = useQueryClient();
  // Resolved BIOS: user-supplied ROM if present, else the bundled open-source
  // one. While the query is loading we show the canvas; null-once-settled ->
  // the optional "provide a BIOS" prompt.
  const { data: bios, isLoading } = usePs1Bios();
  const phase = isLoading ? 'checking' : bios ? 'ready' : 'needbios';
  // True while the (large) disc image is being read from IndexedDB and handed to
  // the core — a PS1 disc is hundreds of MB, so this takes a noticeable moment.
  const [loadingDisc, setLoadingDisc] = useState(false);

  useEffect(() => {
    if (!bios) return;
    let alive = true;
    let raf = 0;
    let psx: WasmPsx | null = null;
    setLoadingDisc(true);
    (async () => {
      await initPsx();
      const disc = await getRomBytes(romId);
      if (!disc || !alive) return;
      psx = new WasmPsx();
      psx.load_bios(bios);
      psx.load_disc(disc); // moves the bytes into the core (no extra copy)
      if (!alive) return;
      setLoadingDisc(false);
      const ctx = canvasRef.current!.getContext('2d')!;
      const loop = () => {
        if (!alive || !psx) return;
        psx.set_keys(keysRef.current >>> 0);
        psx.run_frame();
        audio.push(psx.drain_audio(), 44100, 2);
        const w = psx.width();
        const h = psx.height();
        const canvas = canvasRef.current!;
        if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
        ctx.putImageData(new ImageData(new Uint8ClampedArray(psx.framebuffer()), w, h), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => { alive = false; cancelAnimationFrame(raf); psx?.free(); };
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
    await setBios('ps1', bytes);
    // Re-resolve so the boot effect picks up the user BIOS (over the bundled one).
    qc.setQueryData(['ps1-bios'], bytes);
  };

  return (
    <div className="relative w-full min-h-screen flex flex-col items-center justify-center gap-2 p-3">
      <button onClick={onExit} className="btn self-start mb-1" title="Home">⌂ Home</button>
      {phase === 'needbios' ? (
        <label className="flex flex-col items-center gap-3 border-2 border-dashed border-[var(--color-border)] rounded-xl px-8 py-10 text-center cursor-pointer max-w-md">
          <div className="text-2xl opacity-60">💿</div>
          <div className="text-sm">PlayStation needs a <b>BIOS ROM</b> (e.g. <code>SCPH1001.bin</code>) to boot.</div>
          <div className="text-xs opacity-50">Stored locally in your browser. Drop or pick one — it's remembered for all PS1 games.</div>
          <input
            type="file"
            className="hidden"
            onChange={(e) => onBiosFile(e.target.files?.[0])}
          />
          <span className="btn btn-primary mt-1">Choose BIOS file</span>
        </label>
      ) : (
        <>
          <div className="relative w-[min(94vw,640px)]">
            <canvas ref={canvasRef} width={640} height={480} className="w-full h-auto bg-black" style={{ imageRendering: 'pixelated' }} />
            {loadingDisc && (
              <div className="absolute inset-0 flex items-center justify-center bg-black/70 text-sm">Loading disc…</div>
            )}
          </div>
          <div className="text-[10px] opacity-50 mt-1">x=✕ z=○ s=△ a=□ · q/w=L1/R1 · enter=Start shift=Select · arrows</div>
        </>
      )}
    </div>
  );
}
