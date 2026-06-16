import { useEffect, useState } from 'react';
import { unzipSync } from 'fflate';
import { addRom, deleteRom, listRoms, type RomMeta } from './romStore';
import { ErrorBoundary } from './ErrorBoundary';

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

interface Props {
  open: boolean;
  currentId: string | null;
  onClose: () => void;
  onSelect: (meta: RomMeta) => void;
  onAppend: (msg: string) => void;
}

export function RomLibrary({ open, currentId, onClose, onSelect, onAppend }: Props) {
  const [roms, setRoms] = useState<RomMeta[]>([]);
  const [busy, setBusy] = useState(false);
  const [dragging, setDragging] = useState(false);
  // Multi-select for bulk delete. The checkbox column appears whenever
  // the user clicks "Select" or directly toggles a checkbox; the
  // action bar at the bottom shows count + bulk operations.
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const refresh = async () => {
    setRoms(await listRoms());
    setSelected(new Set());
  };

  useEffect(() => { if (open) refresh(); }, [open]);

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
              onAppend(`${file.name}: no .gba files inside`);
              continue;
            }
            queue.push(...extracted);
          } catch (e) {
            onAppend(`${file.name}: zip extraction failed (${(e as Error).message})`);
          }
        } else if (lower.endsWith('.gba')) {
          queue.push({ filename: file.name, bytes: raw });
        } else {
          onAppend(`skipped ${file.name} (need .gba or .zip)`);
        }
      }
      for (const { filename, bytes } of queue) {
        if (bytes.length < 0xC0) {
          onAppend(`skipped ${filename} (too small to be a GBA ROM)`);
          continue;
        }
        const meta = await addRom(filename, bytes);
        onAppend(`imported ${meta.title || meta.code} (${(bytes.length / (1024 * 1024)).toFixed(1)} MB)`);
      }
      await refresh();
    } catch (e) {
      onAppend(`ROM import failed: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  const onDeleteOne = async (id: string, title: string) => {
    if (!confirm(`Remove "${title}" from your library?`)) return;
    await deleteRom(id);
    onAppend(`removed ${title}`);
    await refresh();
  };

  const onDeleteSelected = async () => {
    const count = selected.size;
    if (count === 0) return;
    if (!confirm(`Remove ${count} ROM${count > 1 ? 's' : ''} from your library?`)) return;
    for (const id of selected) await deleteRom(id);
    onAppend(`removed ${count} ROM${count > 1 ? 's' : ''}`);
    await refresh();
    setSelectMode(false);
  };

  const onClearAll = async () => {
    if (roms.length === 0) return;
    if (!confirm(`Remove ALL ${roms.length} ROMs from your library? This cannot be undone.`)) return;
    for (const r of roms) await deleteRom(r.id);
    onAppend(`cleared library (${roms.length} ROMs)`);
    await refresh();
    setSelectMode(false);
  };

  const toggleSelected = (id: string) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setSelected(next);
    if (next.size > 0) setSelectMode(true);
  };

  const selectAll = () => {
    if (selected.size === roms.length) setSelected(new Set());
    else setSelected(new Set(roms.map((r) => r.id)));
  };

  if (!open) return null;
  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-[1000]" onClick={onClose}>
      <div
        className="bg-[#14141a] border border-[#2a2a30] rounded-lg p-4 w-[640px] max-h-[80vh] overflow-y-auto shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex justify-between items-center mb-3 pb-2 border-b border-[#2a2a30]">
          <div className="flex items-center gap-3">
            <div className="text-sm font-bold tracking-wider">ROM Library</div>
            {roms.length > 0 && (
              <button
                onClick={() => { setSelectMode(!selectMode); if (selectMode) setSelected(new Set()); }}
                className="text-[10px] uppercase tracking-wider opacity-70 hover:opacity-100 bg-transparent border-0 cursor-pointer"
              >{selectMode ? 'Cancel select' : 'Select'}</button>
            )}
          </div>
          <button onClick={onClose} className="bg-transparent border-0 text-[#d8d8e0] text-lg cursor-pointer px-2 hover:text-white">×</button>
        </div>

        <ErrorBoundary label="ROM Library" onClose={onClose} variant="inline">
        <label
          className={`block border-2 border-dashed rounded-md p-6 text-center cursor-pointer mb-3 transition-colors ${
            dragging ? 'border-[#5060a0] bg-[#1c1c2a]' : 'border-[#2a2a30] hover:border-[#404050]'
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
          <div className="text-xs opacity-80">
            {busy ? 'Importing…' : 'Drop a .gba or .zip ROM here, or click to pick one'}
          </div>
          <div className="text-[10px] opacity-50 mt-1">
            Stored locally in your browser via IndexedDB — never uploaded anywhere
          </div>
        </label>

        {roms.length === 0 ? (
          <div className="py-8 text-center opacity-50 text-xs">
            No ROMs imported yet.<br />
            Drop a .gba file above to get started.
          </div>
        ) : (
          <ul className="space-y-1">
            {roms.map((rom) => {
              const isSelected = selected.has(rom.id);
              return (
                <li
                  key={rom.id}
                  className={`flex items-center gap-3 p-2 rounded-md transition-colors ${
                    selectMode
                      ? isSelected
                        ? 'bg-[#3a3a5a] border border-[#5060a0]'
                        : 'bg-[#1c1c22] border border-[#2a2a30] hover:bg-[#24242a]'
                      : rom.id === currentId
                      ? 'bg-[#2a3a5a] border border-[#4060a0] cursor-pointer'
                      : 'bg-[#1c1c22] border border-[#2a2a30] hover:bg-[#24242a] cursor-pointer'
                  }`}
                  onClick={() => selectMode ? toggleSelected(rom.id) : onSelect(rom)}
                >
                  {selectMode && (
                    <input
                      type="checkbox"
                      checked={isSelected}
                      readOnly
                      className="w-3.5 h-3.5 accent-[#5060a0] pointer-events-none"
                    />
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="text-xs font-medium truncate">{rom.title || rom.filename}</div>
                    <div className="text-[10px] opacity-60 truncate">
                      {rom.code} · {(rom.size / (1024 * 1024)).toFixed(1)} MB · {rom.filename}
                    </div>
                  </div>
                  {rom.id === currentId && !selectMode && (
                    <div className="text-[10px] text-[#9be7ff] tracking-wider">PLAYING</div>
                  )}
                  {!selectMode && (
                    <button
                      onClick={(e) => { e.stopPropagation(); onDeleteOne(rom.id, rom.title || rom.filename); }}
                      className="bg-transparent border-0 text-[#9a9aa6] text-sm cursor-pointer px-2 hover:text-red-400"
                      title="Remove from library"
                    >🗑</button>
                  )}
                </li>
              );
            })}
          </ul>
        )}

        {selectMode && roms.length > 0 && (
          <div className="mt-3 pt-3 border-t border-[#2a2a30] flex items-center justify-between text-xs">
            <button
              onClick={selectAll}
              className="bg-transparent border-0 text-[#9a9aa6] cursor-pointer hover:text-white"
            >{selected.size === roms.length ? 'Deselect all' : 'Select all'}</button>
            <div className="flex gap-2">
              <span className="opacity-60 self-center mr-2">{selected.size} selected</span>
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
        </ErrorBoundary>
      </div>
    </div>
  );
}
