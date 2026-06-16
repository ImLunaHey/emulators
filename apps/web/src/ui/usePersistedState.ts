import { useCallback, useState } from 'react';

// localStorage-backed useState. Synchronous client settings (toggles,
// small numbers) don't belong in TanStack Query — that's for async/server
// cache (we use it for ROM cover-art lookups). This is the lightweight
// equivalent for persisted UI state: same shape as useState, but the
// value is seeded from and written back to localStorage. All access is
// wrapped in try/catch so private-mode / disabled storage degrades to
// in-memory state instead of throwing.

function read<T>(key: string, initial: T, parse: (raw: string) => T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? initial : parse(raw);
  } catch {
    return initial;
  }
}

function usePersisted<T>(
  key: string, initial: T, parse: (raw: string) => T, serialize: (v: T) => string,
): [T, (v: T) => void] {
  const [value, setValue] = useState<T>(() => read(key, initial, parse));
  const set = useCallback((v: T) => {
    setValue(v);
    try { localStorage.setItem(key, serialize(v)); } catch { /* ignore */ }
  }, [key, serialize]);
  return [value, set];
}

/** Persisted boolean stored as '1' / '0'. */
export function usePersistedBool(key: string, initial: boolean): [boolean, (v: boolean) => void] {
  return usePersisted(key, initial, (r) => r === '1', (v) => (v ? '1' : '0'));
}

/** Persisted integer. */
export function usePersistedNumber(key: string, initial: number): [number, (v: number) => void] {
  return usePersisted(key, initial, (r) => parseInt(r, 10) || 0, String);
}
