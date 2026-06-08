// GBA Direct Sound A + B emulation (the two FIFO-driven 8-bit PCM
// channels the m4a sound engine and most modern AGB titles use). The
// older PSG channels 1-4 are not implemented; they're silent.
//
// How DirectSound works on the GBA:
//   1. Game sets up a Timer (0 or 1) with a reload value that makes
//      it overflow at the desired output sample rate, e.g. 32768 Hz.
//   2. Game sets up DMA1 (for FIFO A) or DMA2 (for FIFO B) with
//      timing = special, src = wave buffer in EWRAM, dst = FIFO addr,
//      count = 4 words.
//   3. Timer overflow drains 1 sample (signed 8-bit) from the FIFO.
//   4. When the FIFO has ≤16 of its 32 bytes remaining, DMA fires and
//      pushes 4 words = 16 bytes back in.
//   5. SOUNDCNT_H picks which timer drives each channel, the per-
//      channel volume (50% / 100%), and which sides (L/R) get the
//      output.
//   6. SOUNDCNT_X bit 7 is the master enable.
const FIFO_SIZE = 32;
export class Sound {
    dma;
    // Each FIFO is a 32-entry ring of 8-bit signed PCM samples.
    fifoA = new Int8Array(FIFO_SIZE);
    fifoB = new Int8Array(FIFO_SIZE);
    headA = 0;
    tailA = 0;
    countA = 0;
    headB = 0;
    tailB = 0;
    countB = 0;
    // Most-recently-drained sample value for each channel; held until the
    // next timer overflow drains the FIFO again. Real hardware does the
    // same — DAC output stays at the last sample between drains.
    curA = 0;
    curB = 0;
    soundcntH = 0;
    soundcntX = 0;
    // Per-frame sample buffer the host audio sink reads each runFrame().
    // Allocated big enough for one frame at 32768 Hz (= 547 samples) plus
    // a margin; if a game cranks the sample rate higher we'll truncate.
    output = new Float32Array(2048);
    outputLen = 0;
    // Source sample rate the FIFOs are currently draining at. Recomputed
    // whenever a driving timer overflows. The host audio sink reads this
    // along with drainOutput() to set the AudioBuffer's sample rate so
    // the browser resamples correctly. 32768 Hz is just the seed value;
    // games typically pick anything from 5734 Hz up to ~42048 Hz via the
    // m4a sound engine's frequency table.
    sampleRate = 32768;
    // Filled by Emulator after Timers exists. We need to peek at the
    // driving timer's reload + prescale to recompute the sample rate.
    timers = null;
    constructor(dma) {
        this.dma = dma;
    }
    reset() {
        this.headA = this.tailA = this.countA = 0;
        this.headB = this.tailB = this.countB = 0;
        this.curA = this.curB = 0;
        this.outputLen = 0;
        this.soundcntH = 0;
        this.soundcntX = 0;
    }
    // Push one byte into the FIFO (called when the game writes to the
    // FIFO_A_L/H or FIFO_B_L/H MMIO ports, including via DMA).
    pushA(b) {
        if (this.countA >= FIFO_SIZE)
            return;
        this.fifoA[this.tailA] = (b << 24) >> 24; // sign-extend to int8
        this.tailA = (this.tailA + 1) % FIFO_SIZE;
        this.countA++;
    }
    pushB(b) {
        if (this.countB >= FIFO_SIZE)
            return;
        this.fifoB[this.tailB] = (b << 24) >> 24;
        this.tailB = (this.tailB + 1) % FIFO_SIZE;
        this.countB++;
    }
    // Bit 11 of SOUNDCNT_H is the FIFO A reset; bit 15 is FIFO B reset.
    // Writing the bit clears that FIFO.
    writeSoundcntH(v) {
        this.soundcntH = v & 0xFFFF;
        if (v & 0x0800) {
            this.headA = this.tailA = this.countA = 0;
        }
        if (v & 0x8000) {
            this.headB = this.tailB = this.countB = 0;
        }
    }
    writeSoundcntX(v) {
        this.soundcntX = v & 0xFFFF;
        if (!(v & 0x80)) {
            this.headA = this.tailA = this.countA = 0;
            this.headB = this.tailB = this.countB = 0;
            this.curA = this.curB = 0;
        }
    }
    // Called from Timers.overflow(timerIdx). Drains one sample from any
    // FIFO whose timer-select bit matches this timer, and emits one mixed
    // sample to the output buffer.
    onTimerOverflow(timerIdx) {
        if (!(this.soundcntX & 0x80))
            return; // master disable → silence
        const timerA = (this.soundcntH >> 10) & 1;
        const timerB = (this.soundcntH >> 14) & 1;
        // Recompute the source sample rate from the driving timer's
        // reload + prescale. CPU runs at 16.78 MHz; an overflow happens
        // every (0x10000 - reload) * prescale cycles, so the sample
        // rate the emulator produces is CPU_CLOCK / cyclesPerOverflow.
        if (this.timers && (timerA === timerIdx || timerB === timerIdx)) {
            const t = this.timers.ch[timerIdx];
            const cycles = (0x10000 - t.reload) * t.prescale;
            if (cycles > 0)
                this.sampleRate = 16777216 / cycles;
        }
        if (timerA === timerIdx) {
            if (this.countA > 0) {
                this.curA = this.fifoA[this.headA];
                this.headA = (this.headA + 1) % FIFO_SIZE;
                this.countA--;
            }
            // Refill via DMA1 special-timing if the FIFO is below half-full.
            if (this.countA <= 16)
                this.dma.triggerSoundFifo(1);
        }
        if (timerB === timerIdx) {
            if (this.countB > 0) {
                this.curB = this.fifoB[this.headB];
                this.headB = (this.headB + 1) % FIFO_SIZE;
                this.countB--;
            }
            if (this.countB <= 16)
                this.dma.triggerSoundFifo(2);
        }
        // Mix and emit one sample. Volume bits: SOUNDCNT_H bit 2 = A
        // ratio (0=50%, 1=100%), bit 3 = B ratio.
        const aGain = (this.soundcntH & 0x04) ? 1.0 : 0.5;
        const bGain = (this.soundcntH & 0x08) ? 1.0 : 0.5;
        // The two FIFO samples are signed 8-bit (-128..127). The L/R
        // enable bits select speaker; we mix to mono for now and scale
        // into [-1, 1].
        const mixed = (this.curA * aGain + this.curB * bGain) / 256;
        if (this.outputLen < this.output.length) {
            this.output[this.outputLen++] = Math.max(-1, Math.min(1, mixed));
        }
    }
    // Pop the per-frame samples for the audio sink. Returns a NEW typed
    // array (small copy) so the caller can hand it directly to Web Audio.
    drainOutput() {
        const out = this.output.slice(0, this.outputLen);
        this.outputLen = 0;
        return out;
    }
}
