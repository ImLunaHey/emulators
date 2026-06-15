import { useQuery } from '@tanstack/react-query';
import { getBios } from '../biosStore';
// Bundle the open-source BIOS through Vite's asset pipeline (like the WASM):
// a hashed, real module-graph URL that's served in dev and emitted in the
// build — never the SPA fallback you'd get fetching a bare /public path.
import openbiosUrl from '../../assets/openbios.bin?url';

// Fetch the bundled open-source BIOS. Throws on failure rather than returning
// null, so the query never caches a falsy "success" that staleTime: Infinity
// would then wedge forever — a failure becomes a retried error instead.
async function fetchBundledBios(): Promise<Uint8Array> {
  const res = await fetch(openbiosUrl);
  if (!res.ok) throw new Error(`bundled BIOS fetch failed: HTTP ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

// usePs1Bios — resolves the BIOS the PS1 should boot with: a user-supplied ROM
// (best compatibility, stored in IndexedDB) if present, else the bundled
// open-source BIOS. On success `data` is always real bytes; if neither is
// available the query is in an error state (the UI then prompts for a BIOS).
//
// meta.persist: false keeps the ~512 KB ROM bytes out of the localStorage cache
// persister (see App.tsx) — it would blow the quota and serialize poorly.
export function usePs1Bios() {
  return useQuery<Uint8Array>({
    queryKey: ['ps1-bios'],
    staleTime: Infinity,
    gcTime: Infinity,
    meta: { persist: false },
    queryFn: async () => (await getBios('ps1')) ?? (await fetchBundledBios()),
  });
}
