import { type RomMeta } from './romStore';
import { CoverImage } from './CoverImage';
import { useRomMd5 } from './hooks/useRomMd5';
import { useHasheousMeta } from './hooks/useHasheousMeta';

// "Continue playing" hero shown at the top of the library for the most
// recently opened game. Owns the same rom → md5 → Hasheous → cover
// query chain as CoverCard so the artwork matches the grid.
interface Props {
  rom: RomMeta;
  onPlay: () => void;
}

export function ContinueCard({ rom, onPlay }: Props) {
  const md5Query = useRomMd5(rom.id, rom.md5);
  const metaQuery = useHasheousMeta(md5Query.data);
  const m = metaQuery.data ?? null;

  const displayName = m?.name || rom.title || rom.filename;
  const meta = [rom.code, m?.year, `${(rom.size / (1024 * 1024)).toFixed(0)}M`]
    .filter(Boolean).join(' · ');

  const candidates: string[] = [];
  if (m?.igdbId) candidates.push(`/api/igdb/cover/${m.igdbId}`);
  if (m?.thumbnails) candidates.push(...m.thumbnails);

  return (
    <button
      onClick={onPlay}
      className="group w-full mb-4 flex items-center gap-3 sm:gap-4 text-left rounded-xl p-2.5 sm:p-3 border border-[var(--color-border)] bg-[var(--color-card)] hover:border-[var(--color-accent-strong)] hover:bg-[var(--color-card-hover)] transition-colors cursor-pointer"
    >
      <div className="w-16 sm:w-20 shrink-0 rounded-md overflow-hidden">
        <CoverImage title={displayName} subtitle={rom.code} thumbnails={candidates} romId={rom.id} />
      </div>
      <div className="min-w-0 flex-1">
        <div className="eyebrow text-[var(--color-accent)] mb-1">Continue playing</div>
        <div className="text-sm font-bold leading-tight truncate" title={displayName}>{displayName}</div>
        <div className="text-[10px] opacity-50 truncate mt-0.5">{meta}</div>
      </div>
      <span className="grid place-items-center w-10 h-10 shrink-0 rounded-full bg-[var(--color-accent)] text-[#052436] text-base shadow-md group-hover:brightness-110 transition">▶</span>
    </button>
  );
}
