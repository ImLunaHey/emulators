// Tiny haptics helper for the on-screen gamepad. Uses the Vibration
// API (Android Chrome; a no-op on iOS Safari and desktop). The enabled
// flag persists so the user can switch it off. Kept as a module
// singleton so the Gamepad buttons can fire taps without threading a
// prop through every control.

const KEY = 'emulators:haptics';

function load(): boolean {
  try { return localStorage.getItem(KEY) !== '0'; } catch { return true; }
}

export const haptics = {
  enabled: load(),
  // True only where the Vibration API actually exists — used to hide
  // the setting on platforms that can't vibrate.
  supported: typeof navigator !== 'undefined' && typeof navigator.vibrate === 'function',
  setEnabled(v: boolean) {
    this.enabled = v;
    try { localStorage.setItem(KEY, v ? '1' : '0'); } catch { /* ignore */ }
  },
  tap(ms = 12) {
    if (this.enabled && this.supported) {
      try { navigator.vibrate(ms); } catch { /* ignore */ }
    }
  },
};
