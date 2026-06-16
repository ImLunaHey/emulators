import { useEffect, useRef } from 'react';
import initNds, { WasmNds } from '@emulators/nds';
import { getRomBytes } from './romStore';
import { usePlayerAudio } from './playerAudio';

// NDS player. Drives the Rust DS core (WasmNds) directly: boots the ROM, runs
// frames, and blits its two 256x192 RGBA screens (stacked). Separate from the
// GBA PlayerPage because the DS core is a different wasm module with two
// screens + a touchscreen. 2D-first: no audio/savestate yet.

const SCREEN_W = 256;
const SCREEN_H = 192;

// KEYINPUT (0x04000130) bits — identical order to the GBA keypad.
const KEY: Record<string, number> = {
  z: 1 << 0,        // A
  x: 1 << 1,        // B
  Shift: 1 << 2,    // Select
  Enter: 1 << 3,    // Start
  ArrowRight: 1 << 4,
  ArrowLeft: 1 << 5,
  ArrowUp: 1 << 6,
  ArrowDown: 1 << 7,
  s: 1 << 8,        // R
  a: 1 << 9,        // L
};
// EXTKEYIN (0x04000136): X / Y.
const EXT: Record<string, number> = { q: 1 << 0, w: 1 << 1 };

export function NdsPlayer({ romId, onExit }: { romId: string; onExit: () => void }) {
  const topRef = useRef<HTMLCanvasElement>(null);
  const botRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0); // pressed bits (active-high here; inverted for the core)
  const extRef = useRef(0);
  const touchRef = useRef({ pressed: false, x: 0, y: 0 });
  const audio = usePlayerAudio();

  useEffect(() => {
    let alive = true;
    let raf = 0;
    let nds: WasmNds | null = null;
    (async () => {
      await initNds();
      const bytes = await getRomBytes(romId);
      if (!bytes || !alive) return;
      nds = new WasmNds();
      nds.load_rom(bytes);
      const topCtx = topRef.current!.getContext('2d')!;
      const botCtx = botRef.current!.getContext('2d')!;
      const loop = () => {
        if (!alive || !nds) return;
        // The core wants ACTIVE-LOW masks (0 = pressed).
        nds.set_keys((0x3ff & ~keysRef.current) >>> 0, (~extRef.current & 0x3) >>> 0);
        const t = touchRef.current;
        nds.set_touch(t.pressed, t.x, t.y);
        nds.run_frame();
        audio.push(nds.drain_audio(), 44100, 2);
        topCtx.putImageData(new ImageData(new Uint8ClampedArray(nds.top_framebuffer()), SCREEN_W, SCREEN_H), 0, 0);
        botCtx.putImageData(new ImageData(new Uint8ClampedArray(nds.bottom_framebuffer()), SCREEN_W, SCREEN_H), 0, 0);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    })();
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      nds?.free();
    };
  }, [romId]);

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key in KEY) { e.preventDefault(); keysRef.current |= KEY[e.key]; }
      if (e.key in EXT) extRef.current |= EXT[e.key];
    };
    const up = (e: KeyboardEvent) => {
      if (e.key in KEY) keysRef.current &= ~KEY[e.key];
      if (e.key in EXT) extRef.current &= ~EXT[e.key];
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, []);

  const onTouch = (e: React.PointerEvent<HTMLCanvasElement>, pressed: boolean) => {
    const c = botRef.current!;
    const rect = c.getBoundingClientRect();
    const x = Math.max(0, Math.min(SCREEN_W - 1, Math.floor(((e.clientX - rect.left) / rect.width) * SCREEN_W)));
    const y = Math.max(0, Math.min(SCREEN_H - 1, Math.floor(((e.clientY - rect.top) / rect.height) * SCREEN_H)));
    touchRef.current = { pressed, x, y };
  };

  const screenCls = 'w-[min(94vw,512px)] aspect-[4/3] bg-black';
  return (
    <div className="relative w-full min-h-screen flex flex-col items-center justify-center gap-1.5 p-3">
      <button onClick={onExit} className="btn self-start mb-1" title="Home">⌂ Home</button>
      <canvas ref={topRef} width={SCREEN_W} height={SCREEN_H} className={screenCls} style={{ imageRendering: 'pixelated' }} />
      <canvas
        ref={botRef}
        width={SCREEN_W}
        height={SCREEN_H}
        className={`${screenCls} touch-none cursor-pointer`}
        style={{ imageRendering: 'pixelated' }}
        onPointerDown={(e) => onTouch(e, true)}
        onPointerMove={(e) => { if (touchRef.current.pressed) onTouch(e, true); }}
        onPointerUp={(e) => onTouch(e, false)}
        onPointerLeave={(e) => onTouch(e, false)}
      />
      <div className="text-[10px] opacity-50 mt-1">z/x a/s · q/w=X/Y · enter/shift · arrows · tap bottom screen</div>
    </div>
  );
}
