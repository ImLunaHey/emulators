import { useEffect, useRef } from 'react';
import initGbc, { WasmGbc } from '../../core-gbc/pkg/gbc_core.js';
import { getRomBytes } from './romStore';

// Game Boy Color (+ Game Boy) player. One Rust core (WasmGbc) runs both — a
// DMG (.gb) ROM auto-runs in CGB DMG-compat mode. Single 160x144 screen.

// GBC controller bits (active-high): A=0 B=1 Select=2 Start=3 Right=4 Left=5 Up=6 Down=7.
const KEY: Record<string, number> = {
  z: 1 << 0,
  x: 1 << 1,
  Shift: 1 << 2,
  Enter: 1 << 3,
  ArrowRight: 1 << 4,
  ArrowLeft: 1 << 5,
  ArrowUp: 1 << 6,
  ArrowDown: 1 << 7,
};

export function GbcPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);

  useEffect(() => {
    let alive = true;
    let raf = 0;
    let gbc: WasmGbc | null = null;
    (async () => {
      await initGbc();
      const bytes = await getRomBytes(romId);
      if (!bytes || !alive) return;
      gbc = new WasmGbc();
      gbc.load_rom(bytes);
      const w = gbc.width();
      const h = gbc.height();
      const canvas = canvasRef.current!;
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext('2d')!;
      const loop = () => {
        if (!alive || !gbc) return;
        gbc.set_keys(keysRef.current >>> 0);
        gbc.run_frame();
        ctx.putImageData(new ImageData(new Uint8ClampedArray(gbc.framebuffer()), w, h), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      gbc?.free();
    };
  }, [romId]);

  useEffect(() => {
    const down = (e: KeyboardEvent) => { if (e.key in KEY) { e.preventDefault(); keysRef.current |= KEY[e.key]; } };
    const up = (e: KeyboardEvent) => { if (e.key in KEY) keysRef.current &= ~KEY[e.key]; };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, []);

  return (
    <div className="relative w-full min-h-screen flex flex-col items-center justify-center gap-2 p-3">
      <button onClick={onExit} className="btn self-start mb-1" title="Home">⌂ Home</button>
      <canvas ref={canvasRef} className="w-[min(94vw,480px)] h-auto bg-black" style={{ imageRendering: 'pixelated' }} />
      <div className="text-[10px] opacity-50 mt-1">z=A x=B · enter=Start shift=Select · arrows</div>
    </div>
  );
}
