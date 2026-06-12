import { type RomMeta } from './romStore';
import { CoverImage } from './CoverImage';
import { useRomMd5 } from './hooks/useRomMd5';
import { useHasheousMeta } from './hooks/useHasheousMeta';

// A single library card. Owns its own query chain: rom → md5 →
// Hasheous metadata → cover URL (the actual cover URL probing lives
// inside CoverImage via useCoverUrl). Keeping this per-card means
// React Query can dedup + cache each step independently and the
// LibraryPage doesn't have to choreograph anything.

interface Props {
  rom: RomMeta;
  selectMode: boolean;
  selected: boolean;
  onActivate: () => void;             // cover click → play
  onDetails: () => void;              // title click → details page
  onDelete: (displayName: string) => void;
}

export function CoverCard({ rom, selectMode, selected, onActivate, onDetails, onDelete }: Props) {
  const md5Query = useRomMd5(rom.id, rom.md5);
  const metaQuery = useHasheousMeta(md5Query.data);
  const m = metaQuery.data ?? null;

  const displayName = m?.name || rom.title || rom.filename;
  const year = m?.year ?? null;
  const subtitleParts: string[] = [rom.code];
  if (year) subtitleParts.push(year);
  subtitleParts.push(`${(rom.size / (1024 * 1024)).toFixed(0)}M`);

  // Prefer IGDB box art (real cover from Hasheous's IGDB mapping,
  // proxied through our worker) and fall back to LibRetro thumbnails
  // if IGDB doesn't have it. The probe loop in useCoverUrl tries
  // each in order and stops at the first that loads.
  const candidates: string[] = [];
  if (m?.igdbId) candidates.push(`/api/igdb/cover/${m.igdbId}`);
  if (m?.thumbnails) candidates.push(...m.thumbnails);

  return (
    <li
      className={`group rounded-md transition-colors ${
        selectMode
          ? selected
            ? 'ring-2 ring-[#5060a0]'
            : 'opacity-80 hover:opacity-100'
          : 'hover:z-10'
      }`}
      onClick={selectMode ? onActivate : undefined}
    >
      <div
        className={`relative rounded-md overflow-hidden ${selectMode ? '' : 'cursor-pointer hover:scale-[1.02] transition-transform'}`}
        onClick={selectMode ? undefined : onActivate}
        title={selectMode ? undefined : `Play ${displayName}`}
      >
        <CoverImage
          title={displayName}
          subtitle={year || rom.code}
          thumbnails={candidates}
          romId={rom.id}
        />
        {!selectMode && (
          <div className="absolute inset-0 flex items-center justify-center bg-black/45 opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none">
            <span className="grid place-items-center w-10 h-10 rounded-full bg-[var(--color-accent)] text-[#052436] text-base shadow-lg">▶</span>
          </div>
        )}
        {selectMode && (
          <input
            type="checkbox"
            checked={selected}
            readOnly
            className="absolute top-1 left-1 w-4 h-4 accent-[#5060a0] pointer-events-none"
          />
        )}
        {!selectMode && (
          <button
            onClick={(e) => { e.stopPropagation(); onDelete(displayName); }}
            className="absolute top-1 right-1 w-6 h-6 flex items-center justify-center rounded bg-black/60 text-[#9a9aa6] text-xs opacity-0 group-hover:opacity-100 focus:opacity-100 hover:!text-red-400 transition-opacity"
            title="Remove from library"
            aria-label="Remove"
          >🗑</button>
        )}
      </div>
      <div className="min-w-0 mt-1.5 px-0.5">
        <div
          className={`text-[11px] font-medium leading-tight line-clamp-2 ${selectMode ? '' : 'cursor-pointer hover:text-[var(--color-accent)]'}`}
          title={selectMode ? displayName : `${displayName} — details`}
          onClick={selectMode ? undefined : (e) => { e.stopPropagation(); onDetails(); }}
        >{displayName}</div>
        <div className="text-[9px] opacity-50 truncate" title={`${rom.filename} · ${m?.platform || 'GBA'}`}>
          {subtitleParts.join(' · ')}
        </div>
      </div>
    </li>
  );
}
