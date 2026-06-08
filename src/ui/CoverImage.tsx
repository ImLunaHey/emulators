import { useEffect, useState } from 'react';
import { CartArt } from './CartArt';

// Try the LibRetro thumbnail URLs in order. First one that loads wins;
// if all fail (404 / network error), fall back to the styled CartArt
// placeholder. Each successful load is cached in sessionStorage so a
// second mount doesn't repeat the probe.

interface Props {
  title: string;
  subtitle?: string;
  thumbnails: string[];
  className?: string;
}

const CACHE_PREFIX = 'gba-recomp:cover:';

function readCached(title: string): string | null | undefined {
  try {
    const raw = sessionStorage.getItem(CACHE_PREFIX + title);
    if (raw === null) return undefined;
    if (raw === '') return null;
    return raw;
  } catch {
    return undefined;
  }
}
function writeCached(title: string, url: string | null): void {
  try { sessionStorage.setItem(CACHE_PREFIX + title, url ?? ''); } catch { /* quota */ }
}

export function CoverImage({ title, subtitle, thumbnails, className }: Props) {
  const [resolved, setResolved] = useState<string | null | undefined>(() => readCached(title));

  useEffect(() => {
    if (resolved !== undefined) return;
    if (thumbnails.length === 0) { setResolved(null); writeCached(title, null); return; }
    let cancelled = false;
    (async () => {
      for (const url of thumbnails) {
        // Probe via Image() so the browser caches it for the eventual
        // <img src>. HEAD via fetch() doesn't work for these (CORS).
        const ok = await new Promise<boolean>((res) => {
          const img = new Image();
          img.onload = () => res(true);
          img.onerror = () => res(false);
          img.src = url;
        });
        if (cancelled) return;
        if (ok) { setResolved(url); writeCached(title, url); return; }
      }
      if (cancelled) return;
      setResolved(null);
      writeCached(title, null);
    })();
    return () => { cancelled = true; };
  }, [title, thumbnails, resolved]);

  if (resolved) {
    return (
      <div
        className={`relative overflow-hidden rounded-md bg-black ${className ?? ''}`}
        style={{ aspectRatio: '2 / 3' }}
      >
        <img
          src={resolved}
          alt={title}
          loading="lazy"
          className="absolute inset-0 w-full h-full object-cover"
        />
      </div>
    );
  }
  // No cover available — fall back to the stylized placeholder.
  return <CartArt title={title} subtitle={subtitle} className={className} />;
}
