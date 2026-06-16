import { useEffect, useRef, useState } from 'react';
import { usePlayerAudio } from './playerAudio';
import { getRomBytes } from './romStore';

// Generic single-screen wasm-core player, factored out of the per-system players
// (NesPlayer/SmsPlayer/…) since the 8 newer cores (snes, genesis, pce, atari2600,
// ngpc, wonderswan, virtualboy, n64) share the exact same shape: init the wasm
// module, construct the core, load the ROM, then a rAF loop that pumps input,
// ticks a frame, blits the RGBA framebuffer to a canvas sized from width()/
// height() (mirroring Ps1Player so odd/variable resolutions display correctly),
// and drains audio. Per-core specifics (module, key map, audio layout, hint)
// come in as props.

// The slice of the wasm core surface this player drives. Every new core exposes
// these (see each crate's src/wasm.rs); save_ram/extra methods are unused here.
export interface WasmCore {
  load_rom(bytes: Uint8Array): void;
  run_frame(): void;
  framebuffer(): Uint8Array;
  width(): number;
  height(): number;
  set_keys(bits: number): void;
  drain_audio(): Float32Array;
  free(): void;
}

export interface WasmCorePlayerProps {
  romId: string;
  onExit: () => void;
  /** wasm-bindgen init (the module's default export). */
  init: () => Promise<unknown>;
  /** Construct the core instance (after init resolves). */
  create: () => WasmCore;
  /** key (KeyboardEvent.key) → pressed-button bitmask. */
  keyMap: Record<string, number>;
  /** Audio channel count: 1 = mono, 2 = interleaved stereo. */
  audioChannels: number;
  /** Source sample rate of drain_audio(). */
  audioRate?: number;
  /** One-line control hint shown under the canvas. */
  hint: string;
  /** CSS max width for the canvas wrapper (default a 4:3-ish 512px). */
  maxWidthClass?: string;
}

export function WasmCorePlayer({
  romId,
  onExit,
  init,
  create,
  keyMap,
  audioChannels,
  audioRate = 44100,
  hint,
  maxWidthClass = 'w-[min(94vw,512px)]',
}: WasmCorePlayerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const keysRef = useRef(0);
  const audio = usePlayerAudio();
  const [error, setError] = useState<string | null>(null);

  // create/init/keyMap come from a parent that defines them inline; they're
  // stable per system, so we intentionally key the boot effect on romId only.
  useEffect(() => {
    let alive = true;
    let raf = 0;
    let core: WasmCore | null = null;
    (async () => {
      try {
        await init();
        const bytes = await getRomBytes(romId);
        if (!bytes || !alive) return;
        core = create();
        core.load_rom(bytes);
        const ctx = canvasRef.current!.getContext('2d')!;
        const loop = () => {
          if (!alive || !core) return;
          core.set_keys(keysRef.current >>> 0);
          core.run_frame();
          audio.push(core.drain_audio(), audioRate, audioChannels);
          const w = core.width();
          const h = core.height();
          const canvas = canvasRef.current!;
          if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
          ctx.putImageData(new ImageData(new Uint8ClampedArray(core.framebuffer()), w, h), 0, 0);
          raf = requestAnimationFrame(loop);
        };
        raf = requestAnimationFrame(loop);
      } catch (e) {
        setError((e as Error).message || String(e));
      }
    })();
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      core?.free();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [romId]);

  useEffect(() => {
    const down = (e: KeyboardEvent) => { if (e.key in keyMap) { e.preventDefault(); keysRef.current |= keyMap[e.key]; } };
    const up = (e: KeyboardEvent) => { if (e.key in keyMap) keysRef.current &= ~keyMap[e.key]; };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="relative w-full min-h-screen flex flex-col items-center justify-center gap-2 p-3">
      <button onClick={onExit} className="btn self-start mb-1" title="Home">⌂ Home</button>
      <canvas
        ref={canvasRef}
        className={`${maxWidthClass} h-auto bg-black`}
        style={{ imageRendering: 'pixelated' }}
      />
      {error && <div className="text-xs text-red-300">{error}</div>}
      <div className="text-[10px] opacity-50 mt-1">{hint}</div>
    </div>
  );
}
