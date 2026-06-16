import { useState } from 'react';
import { Modal } from './Modal';
import type { AudioSink } from './audio';
import { haptics } from './haptics';
import { Key } from '../io/keypad';

// Buttons offered for autofire (the ones it actually makes sense on).
const TURBO_KEYS: Array<{ key: Key; label: string }> = [
  { key: Key.A, label: 'A' },
  { key: Key.B, label: 'B' },
  { key: Key.L, label: 'L' },
  { key: Key.R, label: 'R' },
];

interface Props {
  open: boolean;
  onClose: () => void;
  audio: AudioSink;
  speed: number;
  onSpeedChange: (s: number) => void;
  smooth: boolean;
  onSmoothChange: (v: boolean) => void;
  crt: boolean;
  onCrtChange: (v: boolean) => void;
  colorCorrect: boolean;
  onColorCorrectChange: (v: boolean) => void;
  turboMask: number;
  onTurboChange: (mask: number) => void;
  rewind: boolean;
  onRewindChange: (v: boolean) => void;
  autoResume: boolean;
  onAutoResumeChange: (v: boolean) => void;
}

// Selectable emulation speeds. 1 is realtime; >1 fast-forwards (audio
// is muted by the Screen loop above 1× to avoid buffer thrash).
const SPEEDS = [0.5, 1, 2, 4];

export function SettingsPanel({ open, onClose, audio, speed, onSpeedChange, smooth, onSmoothChange, crt, onCrtChange, colorCorrect, onColorCorrectChange, turboMask, onTurboChange, rewind, onRewindChange, autoResume, onAutoResumeChange }: Props) {
  // Mirror the sink's persisted values into local state so the slider
  // re-renders; the sink remains the source of truth.
  const [vol, setVol] = useState(audio.volume);
  const [muted, setMuted] = useState(audio.muted);
  const [haptic, setHaptic] = useState(haptics.enabled);

  const applyVol = (v: number) => {
    setVol(v);
    audio.setVolume(v);
    // Dragging the slider off zero implicitly un-mutes.
    if (v > 0 && muted) { setMuted(false); audio.setMuted(false); }
  };
  const toggleMute = () => {
    const m = !muted;
    setMuted(m);
    audio.setMuted(m);
  };

  return (
    <Modal open={open} onClose={onClose} title="Settings" size="sm">
      <div className="space-y-6">
        {/* Audio */}
        <section>
          <div className="eyebrow mb-2">Audio</div>
          <div className="flex items-center gap-3">
            <button onClick={toggleMute} className="btn-icon !w-9 !h-9 !text-base" title={muted ? 'Unmute' : 'Mute'}>
              {muted || vol === 0 ? '🔇' : vol < 0.5 ? '🔈' : '🔊'}
            </button>
            <input
              type="range"
              min={0}
              max={1}
              step={0.01}
              value={muted ? 0 : vol}
              onChange={(e) => applyVol(parseFloat(e.target.value))}
              className="flex-1 accent-[var(--color-accent-strong)]"
              aria-label="Volume"
            />
            <span className="w-10 text-right font-mono text-[11px] opacity-70">
              {Math.round((muted ? 0 : vol) * 100)}%
            </span>
          </div>
        </section>

        {/* Speed */}
        <section>
          <div className="eyebrow mb-2">Emulation speed</div>
          <div className="seg w-full">
            {SPEEDS.map((s) => (
              <button
                key={s}
                onClick={() => onSpeedChange(s)}
                data-active={speed === s}
                className="seg-item flex-1 font-medium"
              >{s === 1 ? '1× (normal)' : `${s}×`}</button>
            ))}
          </div>
          <div className="text-[10px] opacity-50 mt-2 leading-relaxed">
            Fast-forward runs the CPU as fast as the host allows; audio is
            muted above 1× since it can't be resampled cleanly in realtime.
          </div>
        </section>

        {/* Video */}
        <section>
          <div className="eyebrow mb-2">Video</div>
          <label className="flex items-center gap-3 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={!smooth}
              onChange={(e) => onSmoothChange(!e.target.checked)}
              className="w-4 h-4 accent-[var(--color-accent-strong)]"
            />
            <span className="text-xs">Pixel-perfect scaling</span>
            <span className="opacity-50 text-[10px]">off = bilinear smoothing</span>
          </label>
          <label className="flex items-center gap-3 cursor-pointer select-none mt-2.5">
            <input
              type="checkbox"
              checked={crt}
              onChange={(e) => onCrtChange(e.target.checked)}
              className="w-4 h-4 accent-[var(--color-accent-strong)]"
            />
            <span className="text-xs">LCD grid overlay</span>
            <span className="opacity-50 text-[10px]">faux-handheld scanlines</span>
          </label>
          <label className="flex items-center gap-3 cursor-pointer select-none mt-2.5">
            <input
              type="checkbox"
              checked={colorCorrect}
              onChange={(e) => onColorCorrectChange(e.target.checked)}
              className="w-4 h-4 accent-[var(--color-accent-strong)]"
            />
            <span className="text-xs">Color correction</span>
            <span className="opacity-50 text-[10px]">GBA LCD gamma + gamut</span>
          </label>
        </section>

        {/* Touch (only where the device can vibrate) */}
        {haptics.supported && (
          <section>
            <div className="eyebrow mb-2">Touch</div>
            <label className="flex items-center gap-3 cursor-pointer select-none">
              <input
                type="checkbox"
                checked={haptic}
                onChange={(e) => { setHaptic(e.target.checked); haptics.setEnabled(e.target.checked); if (e.target.checked) haptics.tap(); }}
                className="w-4 h-4 accent-[var(--color-accent-strong)]"
              />
              <span className="text-xs">Vibrate on button press</span>
            </label>
          </section>
        )}

        {/* Turbo / autofire */}
        <section>
          <div className="eyebrow mb-2">Turbo / autofire</div>
          <div className="flex flex-wrap gap-2">
            {TURBO_KEYS.map(({ key, label }) => {
              const on = (turboMask & (1 << key)) !== 0;
              return (
                <button
                  key={key}
                  onClick={() => onTurboChange(on ? turboMask & ~(1 << key) : turboMask | (1 << key))}
                  className={on ? 'btn btn-primary' : 'btn'}
                >{label}</button>
              );
            })}
          </div>
          <div className="text-[10px] opacity-50 mt-2 leading-relaxed">
            Selected buttons auto-fire (~30 Hz) while held — on the gamepad,
            keyboard, or on-screen pad.
          </div>
        </section>

        {/* Rewind */}
        <section>
          <div className="eyebrow mb-2">Rewind</div>
          <label className="flex items-center gap-3 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={rewind}
              onChange={(e) => onRewindChange(e.target.checked)}
              className="w-4 h-4 accent-[var(--color-accent-strong)]"
            />
            <span className="text-xs">Enable rewind</span>
            <span className="opacity-50 text-[10px]">hold ⏪ / Backspace to rewind</span>
          </label>
          <div className="text-[10px] opacity-50 mt-2 leading-relaxed">
            Continuously snapshots the last several seconds so you can
            scrub backwards. Uses extra memory while a game is running.
          </div>
        </section>

        {/* Session */}
        <section>
          <div className="eyebrow mb-2">Session</div>
          <label className="flex items-center gap-3 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={autoResume}
              onChange={(e) => onAutoResumeChange(e.target.checked)}
              className="w-4 h-4 accent-[var(--color-accent-strong)]"
            />
            <span className="text-xs">Auto-save &amp; resume</span>
          </label>
          <div className="text-[10px] opacity-50 mt-2 leading-relaxed">
            Periodically saves a hidden state and restores it the next
            time you open this game, so you pick up exactly where you
            left off.
          </div>
        </section>
      </div>
    </Modal>
  );
}
