import { useQuery } from '@tanstack/react-query';
import { getBios } from '../biosStore';

// useXboxBios — resolves the flash/BIOS image the Xbox should boot with. Unlike
// the PS1 (which falls back to a bundled open-source BIOS), there is no
// freely-distributable Xbox BIOS, so this only ever returns a user-supplied ROM
// stored in IndexedDB — or `null` when the user hasn't provided one yet (the UI
// then prompts for a BIOS). Keyed off the shared biosStore under 'xbox'.
//
// meta.persist: false keeps the ROM bytes out of the localStorage query-cache
// persister (see App.tsx) — it would blow the quota and serialize poorly.
export function useXboxBios() {
  return useQuery<Uint8Array | null>({
    queryKey: ['xbox-bios'],
    staleTime: Infinity,
    gcTime: Infinity,
    meta: { persist: false },
    queryFn: async () => (await getBios('xbox')) ?? null,
  });
}
