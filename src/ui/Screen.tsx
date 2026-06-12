import { useEffect, useRef, type MutableRefObject } from 'react';
import type { Emulator } from '../emulator';
import type { AudioSink } from './audio';
import { getGbaColorLut } from './colorCorrect';

interface Props {
  emu: Emulator;
  paused: boolean;
  audio: AudioSink;
  onStats: (s: string) => void;
  /** Emulation speed multiplier (1 = realtime). >1 = fast-forward. */
  speed?: number;
  /** When false, the canvas is bilinear-smoothed instead of pixelated. */
  smooth?: boolean;
  /** Apply GBA LCD color correction to the output. */
  colorCorrect?: boolean;
  /** Screen writes a single-frame-advance fn here for the parent's
      frame-step control to call while paused. */
  stepRef?: MutableRefObject<(() => void) | null>;
}

// GBA refreshes at 59.7275 Hz. On a 120 Hz / ProMotion display rAF
// fires faster than that, so running one emulator frame per rAF would
// run the game at 2× speed. Accumulate elapsed real time and only step
// as many emulator frames as fit, with a safety cap so backgrounding
// the tab doesn't produce a runaway catch-up burst on resume.
const GBA_FRAME_MS = 1000 / 59.7275;
const MAX_FRAMES_PER_RAF = 4;

export function Screen({ emu, paused, audio, onStats, speed = 1, smooth = false, colorCorrect = false, stepRef }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    const imageData = ctx.createImageData(240, 160);
    // 32-bit view over the same buffer, for the color-correction path.
    const out32 = new Uint32Array(imageData.data.buffer);
    const lut = colorCorrect ? getGbaColorLut() : null;
    const blit = () => {
      if (lut) {
        // Recover the 5-bit channels (PPU expanded via (c<<3)|(c>>2), so
        // >>3 is exact) → BGR555 index → corrected packed RGBA.
        const f = emu.ppu.frame;
        for (let i = 0, p = 0; i < 240 * 160; i++, p += 4) {
          out32[i] = lut[(f[p] >> 3) | ((f[p + 1] >> 3) << 5) | ((f[p + 2] >> 3) << 10)];
        }
      } else {
        imageData.data.set(emu.ppu.frame);
      }
      ctx.putImageData(imageData, 0, 0);
    };

    // Expose a one-frame advance for the parent's frame-step button.
    // Drains (but doesn't play) audio so the FIFO doesn't back up.
    if (stepRef) {
      stepRef.current = () => {
        emu.runFrame();
        emu.sound.drainOutput();
        blit();
      };
    }

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

      // Advance emulator time at `speed`× wall-clock. Fast-forward
      // raises the per-rAF frame cap so 2×/4× actually keep up instead
      // of being clamped to realtime.
      accumMs += dt * speed;
      const frameCap = GBA_FRAME_MS * MAX_FRAMES_PER_RAF * Math.max(1, Math.ceil(speed));
      if (accumMs > frameCap) accumMs = frameCap;
      let didFrame = false;
      while (accumMs >= GBA_FRAME_MS) {
        lastR = emu.runFrame();
        accumMs -= GBA_FRAME_MS;
        didFrame = true;
        blitted++;
        // Drain the sound output for this frame. At non-realtime speeds
        // we still drain (so the FIFO doesn't back up) but skip pushing
        // to the sink — feeding it faster/slower than realtime just
        // thrashes the look-ahead buffer into garbled audio.
        const samples = emu.sound.drainOutput();
        if (speed === 1 && samples.length > 0) audio.push(samples, emu.sound.sampleRate);
      }
      if (didFrame) blit();
      const sinceStat = ts - lastStatTs;
      if (sinceStat >= 500) {
        const inst = blitted * 1000 / sinceStat;
        fpsAvg = fpsAvg ? fpsAvg * 0.6 + inst * 0.4 : inst;
        lastStatTs = ts;
        blitted = 0;
        // Only show JIT share when the recompiler is enabled — otherwise
        // every frame would read "jit 0%" which is just noise.
        let jitBit = '';
        if (emu.recomp.enabled) {
          const total = lastR.interp + lastR.jit || 1;
          const jitPct = ((lastR.jit / total) * 100) | 0;
          jitBit = ` · jit ${jitPct}%`;
        }
        onStats(
          `${fpsAvg.toFixed(1)} fps · ${(280896 * fpsAvg / 1e6).toFixed(2)} MHz${jitBit}`,
        );
      }
    };
    raf = requestAnimationFrame(loop);
    return () => {
      stop = true;
      cancelAnimationFrame(raf);
      if (stepRef) stepRef.current = null;
    };
  }, [emu, paused, onStats, speed, colorCorrect, stepRef]);

  return <canvas ref={canvasRef} id="screen" className={smooth ? 'smooth' : ''} width={240} height={160} />;
}
