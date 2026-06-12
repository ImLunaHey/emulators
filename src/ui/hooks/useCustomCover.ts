import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { getCover } from '../coverStore';

// Returns an object-URL for a ROM's user-supplied cover (or null). The
// blob is cached by React Query (invalidate ['custom-cover', id] after a
// put/delete); the URL is created here and revoked on change/unmount.
export function useCustomCover(romId: string | null | undefined): string | null {
  const { data: blob } = useQuery<Blob | null>({
    queryKey: ['custom-cover', romId],
    enabled: !!romId,
    staleTime: Infinity,
    queryFn: () => getCover(romId!),
  });

  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!blob) { setUrl(null); return; }
    const u = URL.createObjectURL(blob);
    setUrl(u);
    return () => URL.revokeObjectURL(u);
  }, [blob]);

  return url;
}
