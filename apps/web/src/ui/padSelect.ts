// Active-controller selection. The GBA has a single keypad, so with
// several pads connected we either let ANY of them drive input (the
// default — hand the controller around, or just plug in whatever) or
// pin input to one specific pad. `null` = any. Selection persists and
// is observable so the Controller panel and the input poller stay in
// sync without prop-drilling.

const KEY = 'emulators:activePad';

let activeId: string | null = (() => {
  try { return localStorage.getItem(KEY); } catch { return null; }
})();
const listeners = new Set<() => void>();

export const padSelect = {
  /** Selected pad id, or null for "any connected pad". */
  get(): string | null { return activeId; },
  set(id: string | null): void {
    if (id === activeId) return;
    activeId = id;
    try {
      if (id) localStorage.setItem(KEY, id);
      else localStorage.removeItem(KEY);
    } catch { /* ignore */ }
    listeners.forEach((fn) => fn());
  },
  subscribe(fn: () => void): () => void {
    listeners.add(fn);
    return () => { listeners.delete(fn); };
  },
};
