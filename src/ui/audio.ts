// Minimal Web Audio sink. Each frame the host calls push(samples)
// with the chunk of INTERLEAVED STEREO float-PCM [L, R, L, R, ...]
// that the emulator's Sound module produced this frame. We queue
// those chunks as short AudioBuffers scheduled back-to-back, with a
// small target latency to absorb the 60Hz frame jitter. The
// AudioContext starts SUSPENDED in modern browsers — call resume()
// from a user gesture before any audio is heard.
//
// The source sample rate is the emulator's fixed output rate
// (32768 Hz — Sound emits one stereo pair every 512 CPU cycles); the
// browser resamples to the device rate via the AudioBuffer's rate.

const TARGET_AHEAD_S = 0.06;   // 60ms of buffered audio ahead of playback
const MAX_AHEAD_S = 0.15;      // drop the queue if we get too far ahead

const VOL_KEY = 'emulators:volume';
const MUTE_KEY = 'emulators:muted';

export class AudioSink {
  ctx: AudioContext | null = null;
  gain: GainNode | null = null;
  nextStart = 0;
  // Volume is the user-facing 0..1 level; muted overrides it to 0
  // without losing the stored level. Both persist across sessions.
  volume = 0.6;
  muted = false;

  constructor() {
    try {
      const v = localStorage.getItem(VOL_KEY);
      if (v !== null) this.volume = Math.max(0, Math.min(1, parseFloat(v)));
      this.muted = localStorage.getItem(MUTE_KEY) === '1';
    } catch { /* no localStorage — keep defaults */ }
  }

  // Effective gain applied to the GainNode.
  private effectiveGain(): number {
    return this.muted ? 0 : this.volume;
  }

  setVolume(v: number): void {
    this.volume = Math.max(0, Math.min(1, v));
    if (this.gain) this.gain.gain.value = this.effectiveGain();
    try { localStorage.setItem(VOL_KEY, String(this.volume)); } catch { /* ignore */ }
  }

  setMuted(m: boolean): void {
    this.muted = m;
    if (this.gain) this.gain.gain.value = this.effectiveGain();
    try { localStorage.setItem(MUTE_KEY, m ? '1' : '0'); } catch { /* ignore */ }
  }

  ensure(): AudioContext | null {
    if (this.ctx) return this.ctx;
    if (typeof AudioContext === 'undefined') return null;
    try {
      this.ctx = new AudioContext({ sampleRate: 44100 });
      this.gain = this.ctx.createGain();
      this.gain.gain.value = this.effectiveGain();
      this.gain.connect(this.ctx.destination);
      this.nextStart = this.ctx.currentTime + TARGET_AHEAD_S;
    } catch {
      this.ctx = null;
    }
    return this.ctx;
  }

  // Browsers require a user gesture before audio plays. The UI calls
  // this on any interaction (pointerdown / keydown / button click).
  resume(): void {
    const ctx = this.ensure();
    if (!ctx) return;
    if (ctx.state === 'suspended') ctx.resume().catch(() => { /* user-rejected */ });
    this.nextStart = Math.max(this.nextStart, ctx.currentTime + TARGET_AHEAD_S);
  }

  // `samples` is interleaved stereo: [L, R, L, R, ...].
  push(samples: Float32Array, sourceRate: number): void {
    const ctx = this.ctx;
    if (!ctx || !this.gain || samples.length < 2 || ctx.state !== 'running') return;
    if (sourceRate < 1024 || sourceRate > 96000) return;  // sanity guard
    // Drop if we've drifted too far ahead — happens when the browser
    // tab is foregrounded after being throttled.
    if (this.nextStart - ctx.currentTime > MAX_AHEAD_S) {
      this.nextStart = ctx.currentTime + TARGET_AHEAD_S;
    }
    const frames = samples.length >> 1;
    const buf = ctx.createBuffer(2, frames, sourceRate);
    // De-interleave into per-channel arrays. These are freshly
    // ArrayBuffer-backed, which also satisfies TS's narrow
    // copyToChannel overload (it rejects ArrayBufferLike-backed views).
    const lch = new Float32Array(frames);
    const rch = new Float32Array(frames);
    for (let i = 0; i < frames; i++) {
      lch[i] = samples[i * 2];
      rch[i] = samples[i * 2 + 1];
    }
    buf.copyToChannel(lch, 0);
    buf.copyToChannel(rch, 1);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this.gain);
    const start = Math.max(this.nextStart, ctx.currentTime);
    src.start(start);
    this.nextStart = start + frames / sourceRate;
  }
}
