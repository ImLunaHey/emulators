import { type RomMeta } from './romStore';
import { CoverImage } from './CoverImage';
import { useRomMd5 } from './hooks/useRomMd5';
import { useHasheousMeta } from './hooks/useHasheousMeta';
import { type SystemId, systemLabel, systemPresentation, isPlayable } from './systems';

// The second navigation level: one console's library. Shows only the ROMs the
// user has imported for `system`, each as a launchable cover card. A back
// button returns to the console grid; the "+ Add" affordance stays reachable.

interface Props {
  system: SystemId;
  roms: RomMeta[];                          // already filtered to this system
  onBack: () => void;
  onPlay: (rom: RomMeta) => void;
  onDelete: (rom: RomMeta, displayName: string) => void;
  onAdd: () => void;
}

export function ConsoleShelf({ system, roms, onBack, onPlay, onDelete, onAdd }: Props) {
  const { accent, tagline } = systemPresentation(system);
  const playable = isPlayable(system);

  return (
    <div className="w-full">
      <header className="flex items-center gap-3 mb-5">
        <button type="button" onClick={onBack} className="btn-ghost" aria-label="Back to consoles">
          ← Consoles
        </button>
        <div className="flex items-baseline gap-2 min-w-0">
          <h2 className="text-xl font-extrabold tracking-tight" style={{ color: accent }}>
            {systemLabel(system)}
          </h2>
          <span className="text-[11px] text-[var(--color-muted)] truncate">{tagline}</span>
          {!playable && (
            <span className="text-[8px] uppercase tracking-[0.12em] font-semibold px-1.5 py-0.5 rounded
                             bg-black/40 text-[var(--color-muted)] border border-[var(--color-border)]">
              coming soon
            </span>
          )}
        </div>
        <span className="ml-auto text-[11px] text-[var(--color-faint)] tabular-nums">
          {roms.length} game{roms.length === 1 ? '' : 's'}
        </span>
        <button type="button" onClick={onAdd} className="btn btn-primary">+ Add</button>
      </header>

      {roms.length === 0 ? (
        <EmptyState accent={accent} playable={playable} onAdd={onAdd} />
      ) : (
        <ul
          className="grid gap-3 sm:gap-4"
          style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(120px, 1fr))' }}
        >
          {roms.map((rom) => (
            <GameCard
              key={rom.id}
              rom={rom}
              accent={accent}
              playable={playable}
              onPlay={() => onPlay(rom)}
              onDelete={(name) => onDelete(rom, name)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function EmptyState({ accent, playable, onAdd }: { accent: string; playable: boolean; onAdd: () => void }) {
  return (
    <div className="grid place-items-center py-20 text-center">
      <div
        className="w-16 h-16 rounded-2xl mb-4 grid place-items-center text-2xl font-black"
        style={{ background: `${accent}22`, color: accent, border: `1px solid ${accent}44` }}
      >▦</div>
      <p className="text-sm text-[var(--color-fg)]">No games here yet</p>
      <p className="text-xs text-[var(--color-muted)] mt-1 max-w-xs">
        {playable
          ? 'Add a ROM for this console to start playing. Files are stored locally in your browser.'
          : 'This console is coming soon — you can still add ROMs now so they’re ready when it lands.'}
      </p>
      <button type="button" onClick={onAdd} className="btn btn-primary mt-4">+ Add a ROM</button>
    </div>
  );
}

function GameCard({
  rom, accent, playable, onPlay, onDelete,
}: {
  rom: RomMeta;
  accent: string;
  playable: boolean;
  onPlay: () => void;
  onDelete: (displayName: string) => void;
}) {
  // Same cover-art resolution chain as CoverCard: md5 → Hasheous meta → cover.
  const md5Query = useRomMd5(rom.id, rom.md5);
  const metaQuery = useHasheousMeta(md5Query.data);
  const m = metaQuery.data ?? null;

  const displayName = m?.name || rom.title || rom.filename;
  const candidates: string[] = [];
  if (m?.igdbId) candidates.push(`/api/igdb/cover/${m.igdbId}`);
  if (m?.thumbnails) candidates.push(...m.thumbnails);

  return (
    <li className="group">
      <button
        type="button"
        onClick={onPlay}
        title={playable ? `Play ${displayName}` : `${displayName} — coming soon`}
        className="relative block w-full rounded-md overflow-hidden cursor-pointer
                   transition-transform duration-150 hover:scale-[1.03]
                   focus-visible:scale-[1.03]"
      >
        <CoverImage title={displayName} subtitle={rom.code} thumbnails={candidates} romId={rom.id} />
        <span
          className="absolute inset-0 grid place-items-center bg-black/45 opacity-0
                     group-hover:opacity-100 focus-within:opacity-100 transition-opacity pointer-events-none"
        >
          <span
            className="grid place-items-center w-10 h-10 rounded-full text-[#052436] text-base shadow-lg"
            style={{ background: playable ? accent : 'var(--color-muted)' }}
          >{playable ? '▶' : '…'}</span>
        </span>
      </button>
      <div className="flex items-center gap-1 mt-1.5 px-0.5">
        <div className="min-w-0 flex-1">
          <div className="text-[11px] font-medium leading-tight line-clamp-2" title={displayName}>
            {displayName}
          </div>
          <div className="text-[9px] text-[var(--color-faint)] truncate">
            {(rom.size / (1024 * 1024)).toFixed(0)}M
          </div>
        </div>
        <button
          type="button"
          onClick={(e) => { e.stopPropagation(); onDelete(displayName); }}
          className="shrink-0 w-6 h-6 grid place-items-center rounded text-[var(--color-faint)]
                     opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-[var(--color-danger)] transition-opacity"
          title="Remove from library"
          aria-label={`Remove ${displayName}`}
        >🗑</button>
      </div>
    </li>
  );
}
