import { CartArt } from './CartArt';
import { useCoverUrl } from './hooks/useCoverUrl';
import { useCustomCover } from './hooks/useCustomCover';

// Renders a game's cover. Preference order: a user-supplied cover (by
// library id) → a probed LibRetro/IGDB thumbnail → the stylized CartArt
// placeholder while probing or when nothing matched.

interface Props {
  title: string;
  subtitle?: string;
  thumbnails: string[];
  /** Library entry id — enables a user-supplied cover override. */
  romId?: string;
  className?: string;
}

export function CoverImage({ title, subtitle, thumbnails, romId, className }: Props) {
  const custom = useCustomCover(romId);
  const { data: resolved } = useCoverUrl(title, thumbnails);
  const src = custom ?? resolved;

  if (src) {
    return (
      <div
        className={`relative overflow-hidden rounded-md bg-[#0a0a0c] ${className ?? ''}`}
        style={{ aspectRatio: '1 / 1' }}
      >
        {/* object-contain so heterogeneous LibRetro thumbnails (some
            512×512 padded, some weird like 256×229) render whole
            instead of getting cropped to fit the card. */}
        <img
          src={src}
          alt={title}
          loading="lazy"
          className="absolute inset-0 w-full h-full object-contain"
        />
      </div>
    );
  }
  return <CartArt title={title} subtitle={subtitle} className={className} />;
}
