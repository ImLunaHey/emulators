import { useEffect, useRef } from 'react';
import type { Emulator } from '../emulator';

interface Props {
  emu: Emulator;
  paused: boolean;
  onStats: (s: string) => void;
}

// Canvas that blits the PPU frame buffer 60 times a second.
export function Screen({ emu, paused, onStats }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    const imageData = ctx.createImageData(240, 160);

    let lastTs = performance.now();
    let fpsAvg = 0;
    let frameCounter = 0;
    let raf = 0;
    let stop = false;

    const loop = (ts: number) => {
      if (stop) return;
      raf = requestAnimationFrame(loop);
      if (paused) return;
      const r = emu.runFrame();
      imageData.data.set(emu.ppu.frame);
      ctx.putImageData(imageData, 0, 0);
      const dt = ts - lastTs;
      lastTs = ts;
      const inst = 1000 / dt;
      fpsAvg = fpsAvg ? fpsAvg * 0.9 + inst * 0.1 : inst;
      frameCounter++;
      if (frameCounter % 30 === 0) {
        const total = r.interp + r.jit || 1;
        const jitPct = ((r.jit / total) * 100) | 0;
        onStats(
          `${fpsAvg.toFixed(1)} fps · ${(280896 * fpsAvg / 1e6).toFixed(2)} MHz · jit ${jitPct}%`,
        );
      }
    };
    raf = requestAnimationFrame(loop);
    return () => { stop = true; cancelAnimationFrame(raf); };
  }, [emu, paused, onStats]);

  return <canvas ref={canvasRef} id="screen" width={240} height={160} />;
}
