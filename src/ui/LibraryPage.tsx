import { useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { unzipSync } from 'fflate';
import { ErrorBoundary } from './ErrorBoundary';
import { type RomMeta, getSelectedRom } from './romStore';
import { useConfirm } from './ConfirmModal';
import { CoverCard } from './CoverCard';
import { ContinueCard } from './ContinueCard';
import { useRomList } from './hooks/useRomList';
import { useRomMutations } from './hooks/useRomMutations';
import { useToast } from './Toast';

// Pull every .gba member out of a ZIP archive.
function extractGbaFromZip(zipBytes: Uint8Array): Array<{ filename: string; bytes: Uint8Array }> {
  const out: Array<{ filename: string; bytes: Uint8Array }> = [];
  const entries = unzipSync(zipBytes, {
    filter: (file) => file.name.toLowerCase().endsWith('.gba'),
  });
  for (const [path, bytes] of Object.entries(entries)) {
    const filename = path.split('/').pop() || path;
    out.push({ filename, bytes });
  }
  return out;
}

// / — full-page ROM library. Card grid with Hasheous metadata + cover
// art (LibRetro thumbnails). Click a card → /play/:romId.
export function LibraryPage() {
  const navigate = useNavigate();
  const { data: roms = [], isLoading } = useRomList();
  const { add, remove } = useRomMutations();
  const confirm = useConfirm();
  const toast = useToast();

  const [busy, setBusy] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [log, setLog] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<'recent' | 'name' | 'size'>('recent');
  const append = (msg: string) => setLog((p) => [...p, msg]);

  // Search + sort the library client-side. `recent` = newest import
  // first (addedAt desc), `name` = alphabetical by display title,
  // `size` = largest cart first.
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? roms.filter((r: RomMeta) =>
          (r.title || '').toLowerCase().includes(q) ||
          (r.filename || '').toLowerCase().includes(q) ||
          (r.code || '').toLowerCase().includes(q))
      : roms.slice();
    filtered.sort((a: RomMeta, b: RomMeta) => {
      if (sort === 'name') return (a.title || a.filename).localeCompare(b.title || b.filename);
      if (sort === 'size') return b.size - a.size;
      return (b.addedAt || 0) - (a.addedAt || 0);
    });
    return filtered;
  }, [roms, query, sort]);

  // Most recently opened game (set by the player on load) — surfaced as
  // a "Continue playing" hero when it's still in the library.
  const recent = useMemo(() => {
    const id = getSelectedRom();
    return id ? roms.find((r: RomMeta) => r.id === id) ?? null : null;
  }, [roms]);

  const handleFiles = async (files: FileList | File[]) => {
    setBusy(true);
    try {
      const arr = Array.from(files);
      const queue: Array<{ filename: string; bytes: Uint8Array }> = [];
      for (const file of arr) {
        const lower = file.name.toLowerCase();
        const raw = new Uint8Array(await file.arrayBuffer());
        if (lower.endsWith('.zip')) {
          try {
            const extracted = extractGbaFromZip(raw);
            if (extracted.length === 0) {
              append(`${file.name}: no .gba files inside`);
              continue;
            }
            queue.push(...extracted);
          } catch (e) {
            append(`${file.name}: zip read failed — ${(e as Error).message}`);
          }
        } else if (lower.endsWith('.gba')) {
          queue.push({ filename: file.name, bytes: raw });
        } else {
          append(`${file.name}: unsupported file type (.gba or .zip only)`);
        }
      }
      let added = 0;
      let failed = 0;
      for (const { filename, bytes } of queue) {
        try {
          await add.mutateAsync({ filename, bytes });
          append(`added ${filename}`);
          added++;
        } catch (e) {
          append(`add ${filename} failed: ${(e as Error).message}`);
          failed++;
        }
      }
      if (added > 0) toast.success(`Added ${added} game${added === 1 ? '' : 's'}`);
      if (failed > 0) toast.error(`${failed} import${failed === 1 ? '' : 's'} failed — see log`);
      if (added === 0 && failed === 0) toast.error('No .gba ROMs found in selection');
    } finally {
      setBusy(false);
    }
  };

  const onDeleteOne = (id: string, name: string) => {
    confirm.ask({
      title: 'Delete ROM',
      message: `Remove "${name}" from your library?\nThe save file stays — you can re-import the ROM later.`,
      confirmLabel: 'Delete',
      danger: true,
      onConfirm: () => { remove.mutate(id); toast.info(`Removed “${name}”`); },
    });
  };
  const onDeleteSelected = () => {
    if (selected.size === 0) return;
    const n = selected.size;
    confirm.ask({
      title: 'Delete selected',
      message: `Remove ${n} ROM${n === 1 ? '' : 's'} from your library? Save files stay intact.`,
      confirmLabel: `Delete ${n}`,
      danger: true,
      onConfirm: async () => {
        for (const id of selected) await remove.mutateAsync(id);
        toast.info(`Removed ${n} game${n === 1 ? '' : 's'}`);
        setSelectMode(false);
        setSelected(new Set());
      },
    });
  };
  const onClearAll = () => {
    confirm.ask({
      title: 'Clear library',
      message: `Remove ALL ${roms.length} ROMs from your library? Save files stay intact.`,
      confirmLabel: 'Clear all',
      danger: true,
      onConfirm: async () => {
        const n = roms.length;
        for (const r of roms) await remove.mutateAsync(r.id);
        toast.info(`Cleared ${n} game${n === 1 ? '' : 's'}`);
      },
    });
  };
  const toggleSelected = (id: string) => {
    const s = new Set(selected);
    if (s.has(id)) s.delete(id); else s.add(id);
    setSelected(s);
  };
  const selectAll = () => {
    if (selected.size === roms.length) setSelected(new Set());
    else setSelected(new Set(roms.map((r: RomMeta) => r.id)));
  };

  return (
    <div className="w-full max-w-[920px] px-3 py-3">
      <header className="flex flex-wrap gap-3 justify-between items-center mb-4 pb-3 border-b border-[var(--color-border)]">
        <div className="flex items-center gap-3">
          <div className="grid place-items-center w-9 h-9 rounded-lg bg-[var(--color-accent-deep)] text-[var(--color-accent)] text-sm font-bold shadow-[inset_0_0_0_1px_rgba(95,208,255,0.25)]">GB</div>
          <div>
            <h1 className="text-base font-bold tracking-wider m-0 leading-none">gba-recomp</h1>
            <div className="eyebrow mt-1">
              library{roms.length > 0 && ` · ${roms.length} game${roms.length === 1 ? '' : 's'}`}
            </div>
          </div>
        </div>
        {roms.length > 0 && (
          <button
            onClick={() => { setSelectMode(!selectMode); if (selectMode) setSelected(new Set()); }}
            className={selectMode ? 'btn btn-primary' : 'btn'}
          >{selectMode ? 'Done' : 'Select'}</button>
        )}
      </header>

      <ErrorBoundary label="Library" variant="inline">
        <label
          className={`flex items-center justify-center gap-3 border-2 border-dashed rounded-xl px-5 py-4 text-center cursor-pointer mb-4 transition-colors ${
            dragging ? 'border-[var(--color-accent-strong)] bg-[rgba(95,208,255,0.06)]' : 'border-[var(--color-border)] hover:border-[var(--color-border-strong)]'
          } ${busy ? 'opacity-50 pointer-events-none' : ''}`}
          onDragOver={(e) => { e.preventDefault(); setDragging(true); }}
          onDragLeave={() => setDragging(false)}
          onDrop={(e) => { e.preventDefault(); setDragging(false); handleFiles(e.dataTransfer.files); }}
        >
          <input
            type="file"
            accept=".gba,.zip"
            multiple
            className="hidden"
            disabled={busy}
            onChange={(e) => { if (e.target.files) { handleFiles(e.target.files); e.target.value = ''; } }}
          />
          <span className="text-xl opacity-60 leading-none">{busy ? '⏳' : '📥'}</span>
          <div className="text-left">
            <div className="text-xs opacity-90">
              {busy ? 'Importing…' : 'Drop a .gba or .zip ROM here, or tap to pick one'}
            </div>
            <div className="text-[10px] opacity-50 mt-0.5">
              Stored locally in your browser via IndexedDB — never uploaded anywhere
            </div>
          </div>
        </label>

        {recent && !selectMode && !query.trim() && (
          <ContinueCard rom={recent} onPlay={() => navigate(`/play/${recent.id}`)} />
        )}

        {roms.length > 0 && (
          <div className="flex flex-wrap gap-2 items-center mb-4">
            <div className="relative flex-1 min-w-[180px]">
              <span className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[var(--color-faint)] text-xs pointer-events-none">⌕</span>
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search by title, file, or code…"
                className="input w-full !pl-7"
              />
            </div>
            <select
              value={sort}
              onChange={(e) => setSort(e.target.value as typeof sort)}
              className="input"
              aria-label="Sort"
            >
              <option value="recent">Recently added</option>
              <option value="name">Name (A→Z)</option>
              <option value="size">Size (large→small)</option>
            </select>
          </div>
        )}

        {isLoading ? (
          <div className="py-12 text-center opacity-50 text-xs">Loading library…</div>
        ) : roms.length === 0 ? (
          <div className="py-16 text-center">
            <div className="text-3xl opacity-30 mb-3">🎮</div>
            <div className="text-xs opacity-60">No ROMs imported yet.</div>
            <div className="text-[10px] opacity-40 mt-1">Drop a .gba file above to get started.</div>
          </div>
        ) : shown.length === 0 ? (
          <div className="py-12 text-center opacity-50 text-xs">
            No games match “{query}”.
          </div>
        ) : (
          <ul className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
            {shown.map((rom: RomMeta) => (
              <CoverCard
                key={rom.id}
                rom={rom}
                selectMode={selectMode}
                selected={selected.has(rom.id)}
                onActivate={() => selectMode ? toggleSelected(rom.id) : navigate(`/play/${rom.id}`)}
                onDetails={() => navigate(`/rom/${rom.id}`)}
                onDelete={(displayName) => onDeleteOne(rom.id, displayName)}
              />
            ))}
          </ul>
        )}

        {selectMode && roms.length > 0 && (
          <div className="mt-4 pt-3 border-t border-[#2a2a30] flex flex-wrap items-center justify-between gap-2 text-xs">
            <button
              onClick={selectAll}
              className="bg-transparent border-0 text-[#9a9aa6] cursor-pointer hover:text-white"
            >{selected.size === roms.length ? 'Deselect all' : 'Select all'}</button>
            <div className="flex gap-2 items-center">
              <span className="opacity-60 mr-2">{selected.size} selected</span>
              <button
                onClick={onDeleteSelected}
                disabled={selected.size === 0}
                className="btn-default text-red-300 hover:!bg-red-900/30"
              >Delete selected</button>
            </div>
          </div>
        )}

        {!selectMode && roms.length > 1 && (
          <div className="mt-4 pt-3 border-t border-[#2a2a30] flex justify-end">
            <button
              onClick={onClearAll}
              className="text-[10px] uppercase tracking-wider opacity-50 hover:opacity-100 bg-transparent border-0 cursor-pointer text-red-300"
            >Clear entire library</button>
          </div>
        )}

        {log.length > 0 && (
          <div className="mt-4 pt-3 border-t border-[#2a2a30] text-[10px] opacity-50 font-mono space-y-0.5 max-h-32 overflow-y-auto">
            {log.map((line, i) => <div key={i}>{line}</div>)}
          </div>
        )}
      </ErrorBoundary>

      <footer className="mt-6 pt-3 border-t border-[#2a2a30] flex justify-end text-[10px] opacity-50">
        <a
          href="https://github.com/ImLunaHey/gba-recomp/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:opacity-100 hover:text-[var(--color-accent)]"
        >Report an issue ↗</a>
      </footer>

      {confirm.node}
    </div>
  );
}
