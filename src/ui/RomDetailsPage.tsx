import { useEffect, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { type RomMeta } from './romStore';
import { CoverImage } from './CoverImage';
import { useRomList } from './hooks/useRomList';
import { useRomMd5 } from './hooks/useRomMd5';
import { useHasheousMeta } from './hooks/useHasheousMeta';
import { useRomMutations } from './hooks/useRomMutations';
import { useConfirm } from './ConfirmModal';
import { ErrorBoundary } from './ErrorBoundary';

// /rom/:romId — metadata + notes for a single library entry. Reached by
// clicking a card's title (the cover itself goes straight to /play).
export function RomDetailsPage() {
  const navigate = useNavigate();
  const { romId } = useParams<{ romId: string }>();
  const { data: roms = [], isLoading } = useRomList();
  const { remove } = useRomMutations();
  const confirm = useConfirm();

  const rom = roms.find((r: RomMeta) => r.id === romId) ?? null;

  const md5Query = useRomMd5(rom?.id ?? null, rom?.md5);
  const metaQuery = useHasheousMeta(md5Query.data);
  const m = metaQuery.data ?? null;

  // Per-game notes / known issues — user-editable, stored locally.
  const notesKey = rom ? `gba-recomp:notes:${rom.code}` : '';
  const [notes, setNotes] = useState('');
  useEffect(() => {
    if (!notesKey) return;
    try { setNotes(localStorage.getItem(notesKey) || ''); } catch { setNotes(''); }
  }, [notesKey]);
  const onNotes = (v: string) => {
    setNotes(v);
    try { localStorage.setItem(notesKey, v); } catch { /* ignore */ }
  };

  // If the library finished loading and this ROM isn't in it, bounce home.
  useEffect(() => {
    if (!isLoading && romId && !rom) navigate('/', { replace: true });
  }, [isLoading, romId, rom, navigate]);

  if (!rom) {
    return (
      <div className="w-full max-w-[820px] px-3 py-6 text-xs opacity-60">
        {isLoading ? 'Loading…' : 'ROM not found.'}
      </div>
    );
  }

  const displayName = m?.name || rom.title || rom.filename;
  const candidates: string[] = [];
  if (m?.igdbId) candidates.push(`/api/igdb/cover/${m.igdbId}`);
  if (m?.thumbnails) candidates.push(...m.thumbnails);

  const meta: Array<[string, string | null]> = [
    ['Game code', rom.code],
    ['Platform', m?.platform || 'Game Boy Advance'],
    ['Publisher', m?.publisher ?? null],
    ['Year', m?.year ?? null],
    ['Region', m?.region ?? null],
    ['Size', `${(rom.size / (1024 * 1024)).toFixed(2)} MB`],
    ['File', rom.filename],
  ];

  const onDelete = () => {
    confirm.ask({
      title: 'Delete ROM',
      message: `Remove "${displayName}" from your library?\nThe save file stays — you can re-import the ROM later.`,
      confirmLabel: 'Delete',
      danger: true,
      onConfirm: () => { remove.mutate(rom.id); navigate('/', { replace: true }); },
    });
  };

  return (
    <div className="w-full max-w-[820px] px-3 py-3">
      <header className="flex items-center gap-3 mb-4 pb-3 border-b border-[var(--color-border)]">
        <button onClick={() => navigate('/')} className="btn" title="Back to library">← Library</button>
        <div className="eyebrow">Details</div>
      </header>

      <ErrorBoundary label="Details" variant="inline">
        <div className="flex flex-col sm:flex-row gap-5">
          <div className="w-full sm:w-[200px] shrink-0">
            <div className="rounded-lg overflow-hidden border border-[var(--color-border)]">
              <CoverImage title={displayName} subtitle={m?.year || rom.code} thumbnails={candidates} />
            </div>
            <button onClick={() => navigate(`/play/${rom.id}`)} className="btn btn-primary w-full mt-3 py-2.5">▶ Play</button>
            <button onClick={onDelete} className="btn btn-danger w-full mt-2">Delete from library</button>
          </div>

          <div className="min-w-0 flex-1">
            <h1 className="text-lg font-bold leading-tight m-0">{displayName}</h1>
            <div className="text-xs opacity-50 mt-1">{[rom.code, m?.region, m?.year].filter(Boolean).join(' · ')}</div>

            <div className="grid grid-cols-2 gap-x-4 gap-y-1.5 mt-4 text-xs">
              {meta.map(([label, value]) => (
                <div key={label} className="flex justify-between gap-2 border-b border-[var(--color-border)]/40 pb-1">
                  <span className="opacity-50">{label}</span>
                  <span className="text-right truncate" title={value || ''}>{value || '—'}</span>
                </div>
              ))}
            </div>

            <div className="mt-5">
              <div className="eyebrow mb-1.5">Description</div>
              <p className="text-xs leading-relaxed opacity-80 whitespace-pre-line">
                {m?.description || (metaQuery.isLoading ? 'Loading…' : 'No description available for this title.')}
              </p>
            </div>

            <div className="mt-5">
              <div className="eyebrow mb-1.5">Notes &amp; known issues</div>
              <textarea
                value={notes}
                onChange={(e) => onNotes(e.target.value)}
                rows={4}
                placeholder="Your notes for this game — cheats that work, glitches, save quirks…"
                className="input w-full leading-relaxed"
              />
              <div className="text-[10px] opacity-40 mt-1">Saved locally on this device.</div>
            </div>
          </div>
        </div>
      </ErrorBoundary>

      {confirm.node}
    </div>
  );
}
