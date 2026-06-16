import { useEffect, useReducer } from 'react';

// Favourite ROMs — a localStorage-backed set of library ids, observable
// so the library grid and details page stay in sync. Favourites sort to
// the top of the library. Singleton + a thin hook (sync client state,
// like padSelect/haptics — not TanStack Query territory).

const KEY = 'emulators:favorites';

function load(): Set<string> {
  try { return new Set(JSON.parse(localStorage.getItem(KEY) || '[]')); } catch { return new Set(); }
}

let ids = load();
const listeners = new Set<() => void>();

export const favorites = {
  has: (id: string): boolean => ids.has(id),
  toggle: (id: string): void => {
    if (ids.has(id)) ids.delete(id); else ids.add(id);
    try { localStorage.setItem(KEY, JSON.stringify([...ids])); } catch { /* ignore */ }
    listeners.forEach((fn) => fn());
  },
  subscribe: (fn: () => void): (() => void) => {
    listeners.add(fn);
    return () => { listeners.delete(fn); };
  },
};

// Re-renders the caller whenever the favourites set changes.
export function useFavorites(): typeof favorites {
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => favorites.subscribe(force), []);
  return favorites;
}
