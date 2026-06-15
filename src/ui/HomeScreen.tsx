import { useMemo, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useRomList } from './hooks/useRomList';
import { deleteRom, setSelectedRom, type RomMeta } from './romStore';
import { ingestFiles, ingestHandles } from './romIngest';
import { supportsFsa, pickRomHandles, handleFromDropItem } from './fsaccess';
import { ACCEPT, ALL_SYSTEMS, type SystemId } from './systems';
import { useToast } from './Toast';
import { ConsoleGrid } from './ConsoleGrid';
import { ConsoleShelf } from './ConsoleShelf';

// The launcher home screen — a console-first, two-level browser:
//   Level 1: a grid of every console (ConsoleGrid) with per-system game counts.
//   Level 2: one console's library (ConsoleShelf), filtered to that system.
// Selecting a game calls onPlay(romId, system) — the contract App relies on to
// swap in the right player core. The add-ROM file dialog stays reachable from
// both levels and via drag-and-drop anywhere on the page.

export function HomeScreen({ onPlay }: { onPlay: (romId: string, system: string) => void }) {
  const toast = useToast();
  const qc = useQueryClient();
  const { data: roms = [] } = useRomList();
  const fileRef = useRef<HTMLInputElement>(null);
  const [active, setActive] = useState<SystemId | null>(null);
  const [dragging, setDragging] = useState(false);

  // Game counts per system, for the console tiles.
  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const r of roms) c[r.system] = (c[r.system] ?? 0) + 1;
    return c;
  }, [roms]);

  // Total library size — drives the global empty state vs. the grid.
  const total = roms.length;

  // Games for the open console (level 2).
  const shelfRoms = useMemo(
    () => (active ? roms.filter((r) => r.system === active) : []),
    [roms, active],
  );

  const refresh = () => qc.invalidateQueries({ queryKey: ['rom-list'] });

  const launch = (rom: RomMeta) => {
    setSelectedRom(rom.id);
    onPlay(rom.id, rom.system);
  };

  const report = (res: { added: number; failed: number }) => {
    if (res.added) toast.success(`Added ${res.added} game${res.added === 1 ? '' : 's'}`);
    if (res.failed) toast.error(`${res.failed} import${res.failed === 1 ? '' : 's'} failed`);
    if (!res.added && !res.failed) toast.error('No recognized ROMs in selection');
    refresh();
  };

  const onAddFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    report(await ingestFiles(files));
  };

  // Add ROMs by on-disk handle (File System Access API) — the file is referenced
  // in place, not copied into browser storage. Used for the picker and for drops
  // where the browser exposes a handle; large discs never load until launch.
  const onAddHandles = async (handles: FileSystemFileHandle[]) => {
    if (handles.length === 0) return;
    report(await ingestHandles(handles));
  };

  const onDelete = async (rom: RomMeta, displayName: string) => {
    if (!confirm(`Remove "${displayName}" from your library?`)) return;
    await deleteRom(rom.id);
    toast.info(`Removed ${displayName}`);
    refresh();
  };

  // Prefer the File System Access picker (keeps ROMs on disk); fall back to the
  // hidden <input> (byte copy) on browsers that don't support it.
  const openPicker = async () => {
    if (supportsFsa()) {
      const handles = await pickRomHandles(ACCEPT.split(','));
      await onAddHandles(handles);
      return;
    }
    fileRef.current?.click();
  };

  // A drop can carry on-disk handles (Chromium) — use them so a dropped disc
  // stays on disk; otherwise fall back to the byte path.
  const onDropFiles = async (dt: DataTransfer) => {
    if (supportsFsa() && dt.items.length > 0) {
      const handles = (
        await Promise.all(Array.from(dt.items).map((it) => handleFromDropItem(it)))
      ).filter((h): h is FileSystemFileHandle => h !== null);
      if (handles.length > 0) {
        await onAddHandles(handles);
        return;
      }
    }
    await onAddFiles(dt.files);
  };

  return (
    <div
      className="relative w-full max-w-[1100px] mx-auto px-3 sm:px-5 py-4"
      onDragOver={(e) => { e.preventDefault(); setDragging(true); }}
      onDragLeave={(e) => { if (e.currentTarget === e.target) setDragging(false); }}
      onDrop={(e) => { e.preventDefault(); setDragging(false); onDropFiles(e.dataTransfer); }}
    >
      {active === null ? (
        <>
          <header className="flex items-end justify-between gap-3 mb-6">
            <div>
              <h1 className="text-2xl sm:text-3xl font-extrabold tracking-tight">Library</h1>
              <p className="text-xs text-[var(--color-muted)] mt-1">
                {total === 0
                  ? 'Pick a console to get started, or drop a ROM anywhere.'
                  : `${total} game${total === 1 ? '' : 's'} across ${
                      ALL_SYSTEMS.filter((s) => (counts[s] ?? 0) > 0).length
                    } console${ALL_SYSTEMS.filter((s) => (counts[s] ?? 0) > 0).length === 1 ? '' : 's'} · choose a console`}
              </p>
            </div>
            <button type="button" onClick={openPicker} className="btn btn-primary shrink-0">+ Add ROM</button>
          </header>

          <ConsoleGrid counts={counts} onSelect={setActive} />
        </>
      ) : (
        <ConsoleShelf
          system={active}
          roms={shelfRoms}
          onBack={() => setActive(null)}
          onPlay={launch}
          onDelete={onDelete}
          onAdd={openPicker}
        />
      )}

      {/* Drag-and-drop overlay — appears while a file is dragged over the page. */}
      {dragging && (
        <div className="fixed inset-0 z-50 grid place-items-center bg-black/60 backdrop-blur-sm pointer-events-none">
          <div className="px-6 py-4 rounded-xl border-2 border-dashed border-[var(--color-accent-strong)]
                          bg-[var(--color-elevated)] text-sm font-medium">
            Drop ROMs to add them to your library
          </div>
        </div>
      )}

      <input
        ref={fileRef}
        type="file"
        accept={ACCEPT}
        multiple
        className="hidden"
        onChange={(e) => { onAddFiles(e.target.files); e.target.value = ''; }}
      />
    </div>
  );
}
