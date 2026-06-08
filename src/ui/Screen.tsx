import { useEffect, useRef } from 'react';
import type { Emulator } from '../emulator';
import type { AudioSink } from './audio';

interface Props {
  emu: Emulator;
  paused: boolean;
  audio: AudioSink;
  onStats: (s: string) => void;
}

// GBA refreshes at 59.7275 Hz. On a 120 Hz / ProMotion display rAF
// fires faster than that, so running one emulator frame per rAF would
// run the game at 2× speed. Accumulate elapsed real time and only step
// as many emulator frames as fit, with a safety cap so backgrounding
// the tab doesn't produce a runaway catch-up burst on resume.
const GBA_FRAME_MS = 1000 / 59.7275;
const MAX_FRAMES_PER_RAF = 4;

export function Screen({ emu, paused, audio, onStats }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    const imageData = ctx.createImageData(240, 160);

    let lastTs = performance.now();
    let accumMs = 0;
    let fpsAvg = 0;
    let lastStatTs = performance.now();
    let blitted = 0;
    let raf = 0;
    let stop = false;
    let lastR = { interp: 0, jit: 0, frames: 0 };

    const loop = (ts: number) => {
      if (stop) return;
      raf = requestAnimationFrame(loop);
      const dt = ts - lastTs;
      lastTs = ts;
      if (paused) { accumMs = 0; return; }

      accumMs += dt;
      if (accumMs > GBA_FRAME_MS * MAX_FRAMES_PER_RAF) {
        accumMs = GBA_FRAME_MS * MAX_FRAMES_PER_RAF;
      }
      let didFrame = false;
      while (accumMs >= GBA_FRAME_MS) {
        lastR = emu.runFrame();
        accumMs -= GBA_FRAME_MS;
        didFrame = true;
        blitted++;
        // Drain the sound output for this frame into the Web Audio sink.
        const samples = emu.sound.drainOutput();
        if (samples.length > 0) audio.push(samples, emu.sound.sampleRate);
      }
      if (didFrame) {
        imageData.data.set(emu.ppu.frame);
        ctx.putImageData(imageData, 0, 0);
      }
      const sinceStat = ts - lastStatTs;
      if (sinceStat >= 500) {
        const inst = blitted * 1000 / sinceStat;
        fpsAvg = fpsAvg ? fpsAvg * 0.6 + inst * 0.4 : inst;
        lastStatTs = ts;
        blitted = 0;
        const total = lastR.interp + lastR.jit || 1;
        const jitPct = ((lastR.jit / total) * 100) | 0;
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
