import { useQuery } from '@tanstack/react-query';
import { listRoms, type RomMeta } from '../romStore';

// useRomList — read the IndexedDB ROM index. Refreshes via
// queryClient.invalidateQueries({ queryKey: ['rom-list'] }) after
// addRom/deleteRom mutations.
export function useRomList() {
  return useQuery<RomMeta[]>({
    queryKey: ['rom-list'],
    queryFn: () => listRoms(),
    // The library is the source of truth — always refetch on mount so
    // a stale cache from an earlier session can't mask a delete.
    staleTime: 0,
  });
}
