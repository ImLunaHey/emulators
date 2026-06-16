import { ALL_SYSTEMS, type SystemId, systemLabel, systemPresentation, isPlayable } from './systems';

// The landing view: a grid of console tiles. Each tile carries its signature
// accent color, the count of games the user has imported for that system, and a
// "soon" badge for systems whose core isn't playable yet. Selecting a tile is
// the only way into a console's library (the second navigation level).

interface Props {
  /** Game counts keyed by SystemId. Missing/zero means an empty shelf. */
  counts: Record<string, number>;
  onSelect: (system: SystemId) => void;
}

export function ConsoleGrid({ counts, onSelect }: Props) {
  return (
    <ul
      className="grid gap-3 sm:gap-4"
      style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))' }}
    >
      {ALL_SYSTEMS.map((id) => (
        <ConsoleTile key={id} id={id} count={counts[id] ?? 0} onSelect={onSelect} />
      ))}
    </ul>
  );
}

function ConsoleTile({ id, count, onSelect }: { id: SystemId; count: number; onSelect: (s: SystemId) => void }) {
  const { accent, tagline } = systemPresentation(id);
  const label = systemLabel(id);
  const playable = isPlayable(id);

  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(id)}
        aria-label={`${tagline} — ${count} game${count === 1 ? '' : 's'}`}
        className="group relative w-full aspect-[4/3] rounded-xl overflow-hidden text-left
                   border border-[var(--color-border)] bg-[var(--color-card)]
                   transition-[transform,border-color,box-shadow] duration-150
                   hover:-translate-y-0.5 hover:border-[var(--color-border-strong)]
                   focus-visible:-translate-y-0.5"
        style={{ ['--tile' as string]: accent }}
      >
        {/* Signature wash: a diagonal gradient of the console accent that
            intensifies on hover, plus a soft glow. */}
        <span
          aria-hidden
          className="absolute inset-0 opacity-60 group-hover:opacity-90 transition-opacity duration-150"
          style={{ background: `linear-gradient(140deg, ${accent}33 0%, ${accent}10 45%, transparent 75%)` }}
        />
        <span
          aria-hidden
          className="absolute -right-8 -top-8 w-28 h-28 rounded-full blur-2xl opacity-30 group-hover:opacity-60 transition-opacity duration-150"
          style={{ background: accent }}
        />

        <span className="relative flex flex-col justify-between h-full p-3.5">
          <span className="flex items-start justify-between gap-2">
            <span
              className="text-2xl font-extrabold tracking-tight leading-none"
              style={{ color: accent }}
            >{label}</span>
            {!playable && (
              <span className="shrink-0 text-[8px] uppercase tracking-[0.12em] font-semibold
                               px-1.5 py-0.5 rounded bg-black/40 text-[var(--color-muted)] border border-[var(--color-border)]">
                soon
              </span>
            )}
          </span>

          <span className="flex items-end justify-between gap-2">
            <span className="text-[10px] leading-tight text-[var(--color-muted)] truncate">{tagline}</span>
            <span
              className={`shrink-0 text-[11px] font-semibold tabular-nums px-2 py-0.5 rounded-full ${
                count > 0
                  ? 'bg-black/40 text-[var(--color-fg)]'
                  : 'bg-transparent text-[var(--color-faint)]'
              }`}
            >{count}</span>
          </span>
        </span>
      </button>
    </li>
  );
}
