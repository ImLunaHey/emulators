// Minimal Web Audio sink. Each frame the host calls push(samples)
// with the chunk of mono float-PCM that the emulator's Sound module
// produced this frame. We queue those chunks as short AudioBuffers
// scheduled back-to-back, with a small target latency to absorb the
// 60Hz frame jitter. The AudioContext starts SUSPENDED in modern
// browsers — call resume() from a user gesture before any audio is
// heard.
//
// The "source sample rate" depends on the game's Timer setup (the
// rate at which Sound emits samples). 32768 Hz is the standard rate
// the m4a sound engine uses, which covers Pokemon / most AGB titles.
// If a game uses a different rate it'll play slightly too fast or
// slow until we add rate tracking — visible-feature wise it works.
const TARGET_AHEAD_S = 0.06; // 60ms of buffered audio ahead of playback
const MAX_AHEAD_S = 0.15; // drop the queue if we get too far ahead
export class AudioSink {
    ctx = null;
    gain = null;
    nextStart = 0;
    ensure() {
        if (this.ctx)
            return this.ctx;
        if (typeof AudioContext === 'undefined')
            return null;
        try {
            this.ctx = new AudioContext({ sampleRate: 44100 });
            this.gain = this.ctx.createGain();
            this.gain.gain.value = 0.6;
            this.gain.connect(this.ctx.destination);
            this.nextStart = this.ctx.currentTime + TARGET_AHEAD_S;
        }
        catch {
            this.ctx = null;
        }
        return this.ctx;
    }
    // Browsers require a user gesture before audio plays. The UI calls
    // this on any interaction (pointerdown / keydown / button click).
    resume() {
        const ctx = this.ensure();
        if (!ctx)
            return;
        if (ctx.state === 'suspended')
            ctx.resume().catch(() => { });
        this.nextStart = Math.max(this.nextStart, ctx.currentTime + TARGET_AHEAD_S);
    }
    push(samples, sourceRate) {
        const ctx = this.ctx;
        if (!ctx || !this.gain || samples.length === 0 || ctx.state !== 'running')
            return;
        if (sourceRate < 1024 || sourceRate > 96000)
            return; // sanity guard
        // Drop if we've drifted too far ahead — happens when the browser
        // tab is foregrounded after being throttled.
        if (this.nextStart - ctx.currentTime > MAX_AHEAD_S) {
            this.nextStart = ctx.currentTime + TARGET_AHEAD_S;
        }
        const buf = ctx.createBuffer(1, samples.length, sourceRate);
        // Float32Array → AudioBuffer channel. Have to re-copy through a
        // fresh ArrayBuffer-backed array because TS's narrow Float32Array
        // overload for copyToChannel doesn't accept ArrayBufferLike-backed
        // arrays (they could theoretically be SharedArrayBuffer-backed).
        const copy = new Float32Array(samples);
        buf.copyToChannel(copy, 0);
        const src = ctx.createBufferSource();
        src.buffer = buf;
        src.connect(this.gain);
        const start = Math.max(this.nextStart, ctx.currentTime);
        src.start(start);
        this.nextStart = start + samples.length / sourceRate;
    }
}
