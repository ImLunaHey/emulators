import { useEffect, useRef, useState } from 'react';
import initNes, { WasmNes } from '../../core-nes/pkg/nes_core.js';
import { getRomBytes } from './romStore';

// NES player. Drives the Rust NES core (WasmNes): boots the ROM, runs frames,
// blits its single 256x240 RGBA screen. Single-screen so it's the simplest
// player. Audio (drain_audio, 44.1kHz mono) exists on the core but isn't piped
// to WebAudio yet — video + input first.

const W = 256;
const H = 240;

// NES controller bit order: A=0, B=1, Select=2, Start=3, Up=4, Down=5, Left=6, Right=7.
const KEY: Record<string, number> = {
  z: 1 << 0,
  x: 1 << 1,
  Shift: 1 << 2,
  Enter: 1 << 3,
  ArrowUp: 1 << 4,
  ArrowDown: 1 << 5,
  ArrowLeft: 1 << 6,
  ArrowRight: 1 << 7,
};

export function NesPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    let raf = 0;
    let nes: WasmNes | null = null;
    (async () => {
      await initNes();
      const bytes = await getRomBytes(romId);
      if (!bytes || !alive) return;
      nes = new WasmNes();
      if (!nes.load_rom(bytes)) {
        setError('Unsupported ROM or mapper');
        return;
      }
      const ctx = canvasRef.current!.getContext('2d')!;
      const loop = () => {
        if (!alive || !nes) return;
        nes.set_keys(keysRef.current >>> 0);
        nes.run_frame();
        ctx.putImageData(new ImageData(new Uint8ClampedArray(nes.framebuffer()), W, H), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      nes?.free();
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
      <canvas
        ref={canvasRef}
        width={W}
        height={H}
        className="w-[min(94vw,512px)] bg-black"
        style={{ imageRendering: 'pixelated', aspectRatio: '256 / 240' }}
      />
      {error && <div className="text-xs text-red-300">{error}</div>}
      <div className="text-[10px] opacity-50 mt-1">z=A x=B · enter=Start shift=Select · arrows</div>
    </div>
  );
}
