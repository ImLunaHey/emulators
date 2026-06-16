import { useQuery } from '@tanstack/react-query';
import { md5Hex } from '../hasheous';
import { getRomBytes, updateRomMd5 } from '../romStore';

// useRomMd5 — returns the MD5 hash for a ROM, computing it (and
// backfilling the IndexedDB record) when it isn't already stored.
// The result is keyed by rom id, so each ROM in the library gets its
// own cache entry and we only hash a given ROM once across the app.
export function useRomMd5(romId: string | null, knownMd5: string | undefined) {
  return useQuery<string>({
    queryKey: ['rom-md5', romId],
    enabled: romId !== null,
    // staleTime: Infinity — MD5 of a stored ROM never changes.
    staleTime: Infinity,
    queryFn: async () => {
      if (!romId) throw new Error('romId required');
      if (knownMd5) return knownMd5;
      const bytes = await getRomBytes(romId);
      if (!bytes) throw new Error(`ROM ${romId} not found in IndexedDB`);
      const md5 = await md5Hex(bytes);
      await updateRomMd5(romId, md5);
      return md5;
    },
  });
}
