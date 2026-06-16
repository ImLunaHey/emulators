import { useQuery } from '@tanstack/react-query';
import { lookupByMd5, type HasheousMeta } from '../hasheous';

// useHasheousMeta — fetch the Hasheous metadata for a single ROM,
// keyed by its MD5. Returns `null` when Hasheous has no match
// (cached so we don't re-ask) and `undefined` while the query is in
// flight. TanStack handles in-memory dedup + persisted cache via the
// app's QueryClient; this hook is just the thin wrapper.
export function useHasheousMeta(md5: string | null | undefined) {
  return useQuery<HasheousMeta | null>({
    queryKey: ['hasheous-meta', md5],
    enabled: !!md5,
    queryFn: () => lookupByMd5(md5!),
    staleTime: Infinity,    // metadata for a given hash never changes
  });
}
