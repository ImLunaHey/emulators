import { useQuery } from '@tanstack/react-query';

// useCoverUrl — given a candidate list of thumbnail URLs (typically
// IGDB-via-our-worker first, then LibRetro thumbnails), probe each in
// order via Image() and resolve to the first one that loads. Returns
// null if every candidate 404s, or the candidate list is empty.
//
// The query key includes the FULL candidate list (not just the
// title) so that adding/removing/reordering candidates re-runs the
// probe. The previous version keyed only by title, which meant a
// cached LibRetro URL would keep being returned after we prepended
// a higher-quality IGDB URL to the list.
export function useCoverUrl(title: string, candidates: string[]) {
  return useQuery<string | null>({
    queryKey: ['cover-url', title, candidates],
    enabled: candidates.length > 0,
    staleTime: Infinity,
    queryFn: async () => {
      for (const url of candidates) {
        const ok = await new Promise<boolean>((res) => {
          const img = new Image();
          img.onload = () => res(true);
          img.onerror = () => res(false);
          img.src = url;
        });
        if (ok) return url;
      }
      return null;
    },
  });
}
