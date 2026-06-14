import { useEffect, useRef } from 'react';

// Shared Web Audio sink for the non-GBA players. Each frame the player drains
// the core's samples and pushes them here; we queue them as short AudioBuffers
// scheduled back-to-back with a small latency cushion. Handles mono OR
// interleaved-stereo source at any sample rate (the browser resamples via the
// AudioBuffer's rate). The AudioContext needs a user gesture to start, so the
// hook resumes it on mount + the first pointer/key event.

const AHEAD = 0.06; // seconds of buffered audio kept ahead of playback
const MAX_AHEAD = 0.2; // resync if we drift this far ahead (tab throttling)

export class PlayerAudio {
  private ctx: AudioContext | null = null;
  private gain: GainNode | null = null;
  private nextStart = 0;

  resume(): void {
    if (!this.ctx) {
      if (typeof AudioContext === 'undefined') return;
      try {
        this.ctx = new AudioContext();
        this.gain = this.ctx.createGain();
        this.gain.gain.value = 0.7;
        this.gain.connect(this.ctx.destination);
        this.nextStart = this.ctx.currentTime + AHEAD;
      } catch {
        this.ctx = null;
        return;
      }
    }
    if (this.ctx.state === 'suspended') this.ctx.resume().catch(() => {});
  }

  /** `samples` is mono (channels=1) or interleaved stereo (channels=2). */
  push(samples: Float32Array, sourceRate: number, channels: number): void {
    const ctx = this.ctx;
    if (!ctx || !this.gain || ctx.state !== 'running') return;
    if (sourceRate < 1024 || sourceRate > 96000) return;
    const frames = Math.floor(samples.length / channels);
    if (frames < 1) return;
    if (this.nextStart - ctx.currentTime > MAX_AHEAD) {
      this.nextStart = ctx.currentTime + AHEAD;
    }
    const buf = ctx.createBuffer(channels === 2 ? 2 : 1, frames, sourceRate);
    if (channels === 2) {
      const l = new Float32Array(frames);
      const r = new Float32Array(frames);
      for (let i = 0; i < frames; i++) {
        l[i] = samples[i * 2];
        r[i] = samples[i * 2 + 1];
      }
      buf.copyToChannel(l, 0);
      buf.copyToChannel(r, 1);
    } else {
      buf.copyToChannel(new Float32Array(samples.subarray(0, frames)), 0);
    }
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this.gain);
    const start = Math.max(this.nextStart, ctx.currentTime);
    src.start(start);
    this.nextStart = start + frames / sourceRate;
  }

  close(): void {
    this.ctx?.close().catch(() => {});
    this.ctx = null;
  }
}

/** A PlayerAudio that auto-resumes on user gesture and closes on unmount. */
export function usePlayerAudio(): PlayerAudio {
  const ref = useRef<PlayerAudio | null>(null);
  if (!ref.current) ref.current = new PlayerAudio();
  const audio = ref.current;
  useEffect(() => {
    const a = audio;
    a.resume();
    const onGesture = () => a.resume();
    window.addEventListener('pointerdown', onGesture);
    window.addEventListener('keydown', onGesture);
    return () => {
      window.removeEventListener('pointerdown', onGesture);
      window.removeEventListener('keydown', onGesture);
      a.close();
    };
  }, [audio]);
  return audio;
}
