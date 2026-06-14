import { useEffect, useRef } from 'react';
import initSms, { WasmSms } from '../../core-sms/pkg/sms_core.js';
import { getRomBytes } from './romStore';
import { usePlayerAudio } from './playerAudio';

// Master System + Game Gear player. One Rust core (WasmSms) handles both —
// constructed with game_gear=true for .gg (160x144 crop) or false for .sms
// (256x192). Single screen; width()/height() come from the core.

// SMS/GG controller bits (active-high): Up0 Down1 Left2 Right3 B1=4 B2=5 Start/Pause=6.
const KEY: Record<string, number> = {
  ArrowUp: 1 << 0,
  ArrowDown: 1 << 1,
  ArrowLeft: 1 << 2,
  ArrowRight: 1 << 3,
  z: 1 << 4,
  x: 1 << 5,
  Enter: 1 << 6,
};

export function SmsPlayer({ romId, system, onExit }: { romId: string; system: string; onExit: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const audio = usePlayerAudio();

  useEffect(() => {
    let alive = true;
    let raf = 0;
    let sms: WasmSms | null = null;
    (async () => {
      await initSms();
      const bytes = await getRomBytes(romId);
      if (!bytes || !alive) return;
      sms = new WasmSms(system === 'gg');
      sms.load_rom(bytes);
      const canvas = canvasRef.current!;
      const w = sms.width();
      const h = sms.height();
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext('2d')!;
      const loop = () => {
        if (!alive || !sms) return;
        sms.set_keys(keysRef.current >>> 0);
        sms.run_frame();
        audio.push(sms.drain_audio(), 44100, 1);
        ctx.putImageData(new ImageData(new Uint8ClampedArray(sms.framebuffer()), w, h), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      sms?.free();
    };
  }, [romId, system]);

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
        className="w-[min(94vw,512px)] h-auto bg-black"
        style={{ imageRendering: 'pixelated' }}
      />
      <div className="text-[10px] opacity-50 mt-1">z=1 x=2 · enter=Start/Pause · arrows</div>
    </div>
  );
}
