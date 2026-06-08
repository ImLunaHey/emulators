import { useEffect, useState } from 'react';
import type { Cheat } from '../io/cheats';
import { parseCheat } from '../io/cheats';
import { ErrorBoundary } from './ErrorBoundary';

interface Props {
  open: boolean;
  gameCode: string | null;
  cheats: Cheat[];
  onChange: (cheats: Cheat[]) => void;
  onClose: () => void;
}

// Cheats are persisted in localStorage keyed by the cart's 4-letter
// game code so each ROM has its own list. We rehydrate here whenever a
// new game is loaded — App passes the gameCode + current emu.cheats
// array in and we hand back updates via onChange.
const STORAGE_KEY_PREFIX = 'gba-recomp:cheats:';

export function loadCheatsFor(code: string): Cheat[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + code);
    if (raw) return JSON.parse(raw);
  } catch { /* ignore */ }
  return [];
}
export function saveCheatsFor(code: string, cheats: Cheat[]): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + code, JSON.stringify(cheats));
}

export function CheatsPanel({ open, gameCode, cheats, onChange, onClose }: Props) {
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState<Cheat>({ name: '', code: '', enabled: true });

  // Reset the draft / editor whenever the user navigates between games.
  useEffect(() => { setEditing(null); }, [gameCode]);

  const persist = (next: Cheat[]) => {
    onChange(next);
    if (gameCode) saveCheatsFor(gameCode, next);
  };

  const startAdd = () => {
    setDraft({ name: '', code: '', enabled: true });
    setEditing(-1);  // -1 sentinel = "new"
  };
  const startEdit = (i: number) => {
    setDraft({ ...cheats[i] });
    setEditing(i);
  };
  const cancelEdit = () => {
    setEditing(null);
    setDraft({ name: '', code: '', enabled: true });
  };
  const commit = () => {
    if (!draft.code.trim()) return;
    if (editing === -1) {
      persist([...cheats, { ...draft, name: draft.name.trim() || 'Untitled' }]);
    } else if (editing !== null) {
      persist(cheats.map((c, i) => (i === editing ? { ...draft, name: draft.name.trim() || 'Untitled' } : c)));
    }
    cancelEdit();
  };
  const toggleEnabled = (i: number) => {
    persist(cheats.map((c, j) => (j === i ? { ...c, enabled: !c.enabled } : c)));
  };
  const remove = (i: number) => {
    if (!confirm(`Delete cheat "${cheats[i].name}"?`)) return;
    persist(cheats.filter((_, j) => j !== i));
    if (editing === i) cancelEdit();
  };

  if (!open) return null;

  // Parse the current draft so we can warn about syntax problems before
  // the user saves a code that does nothing.
  const draftParsed = parseCheat(draft.code);
  const supportedLines = draftParsed.filter((l) => l.type !== 'unsupported').length;
  const unsupportedLines = draftParsed.filter((l) => l.type === 'unsupported').length;

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-[1000]" onClick={onClose}>
      <div
        className="bg-[#14141a] border border-[#2a2a30] rounded-lg p-4 w-[680px] max-h-[88vh] overflow-y-auto shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex justify-between items-center mb-3 pb-2 border-b border-[#2a2a30]">
          <div>
            <div className="text-sm font-bold tracking-wider">Cheats</div>
            <div className="text-[10px] opacity-50 mt-0.5">
              {gameCode ? `game code ${gameCode}` : 'no ROM loaded'}
            </div>
          </div>
          <button onClick={onClose} className="bg-transparent border-0 text-[#d8d8e0] text-lg cursor-pointer px-2 hover:text-white">×</button>
        </div>

        <ErrorBoundary label="Cheats" onClose={onClose} variant="inline">
        {!gameCode ? (
          <div className="py-8 text-center opacity-50 text-xs">
            Load a ROM to manage its cheats.
          </div>
        ) : (
          <>
            <ul className="space-y-1 mb-3">
              {cheats.length === 0 ? (
                <li className="py-6 text-center opacity-50 text-xs">No cheats yet — add one below.</li>
              ) : (
                cheats.map((c, i) => (
                  <li
                    key={i}
                    className={`flex items-center gap-3 p-2 rounded-md border ${
                      c.enabled
                        ? 'bg-[#2a4a3a] border-[#4a8a6a]'
                        : 'bg-[#1c1c22] border-[#2a2a30]'
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={c.enabled}
                      onChange={() => toggleEnabled(i)}
                      className="w-3.5 h-3.5 accent-[#5060a0]"
                    />
                    <div className="flex-1 min-w-0 cursor-pointer" onClick={() => startEdit(i)}>
                      <div className="text-xs font-medium truncate">{c.name}</div>
                      <div className="text-[10px] opacity-60 truncate font-mono">{c.code.split('\n')[0]}{c.code.includes('\n') ? ' …' : ''}</div>
                    </div>
                    <button
                      onClick={() => startEdit(i)}
                      className="bg-transparent border-0 text-[#9a9aa6] text-xs cursor-pointer px-2 hover:text-white"
                    >Edit</button>
                    <button
                      onClick={() => remove(i)}
                      className="bg-transparent border-0 text-[#9a9aa6] text-sm cursor-pointer px-2 hover:text-red-400"
                      title="Remove cheat"
                    >🗑</button>
                  </li>
                ))
              )}
            </ul>

            {editing === null ? (
              <button onClick={startAdd} className="btn-default w-full">+ Add cheat</button>
            ) : (
              <div className="bg-[#0e0e12] border border-[#2a2a30] rounded-md p-3 space-y-2">
                <div className="text-[10px] uppercase tracking-widest opacity-50">{editing === -1 ? 'New cheat' : 'Editing'}</div>
                <input
                  type="text"
                  placeholder="Name (e.g. Infinite money)"
                  value={draft.name}
                  onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                  className="w-full bg-[#1c1c22] border border-[#2a2a30] rounded p-2 text-xs text-[#d8d8e0]"
                />
                <textarea
                  placeholder={`Paste cheat codes — one per line.\nFormat: XXXXXXXX YYYYYYYY\n\nExample:\n02001234 000000FF`}
                  value={draft.code}
                  onChange={(e) => setDraft({ ...draft, code: e.target.value })}
                  rows={5}
                  className="w-full bg-[#1c1c22] border border-[#2a2a30] rounded p-2 text-[11px] text-[#d8d8e0] font-mono"
                />
                <div className="flex items-center justify-between text-[10px]">
                  <div className="opacity-60">
                    {supportedLines > 0 && <span>✓ {supportedLines} line{supportedLines > 1 ? 's' : ''} parsed</span>}
                    {unsupportedLines > 0 && <span className="text-amber-300 ml-2">⚠ {unsupportedLines} unsupported opcode{unsupportedLines > 1 ? 's' : ''}</span>}
                    {draftParsed.length === 0 && draft.code.trim() && <span className="text-red-300">malformed code</span>}
                  </div>
                  <div className="flex gap-2">
                    <button onClick={cancelEdit} className="btn-default !text-[10px]">Cancel</button>
                    <button onClick={commit} className="btn-default !text-[10px]">Save</button>
                  </div>
                </div>
              </div>
            )}

            <div className="mt-4 text-[10px] opacity-50 leading-relaxed">
              Supports the standard 8-byte GameShark / CodeBreaker / Action Replay format
              for write opcodes (8/16/32-bit) and conditional equality checks.
              Encrypted codes need to be decrypted first.
              Cheats fire once per frame, persisting their value through normal game logic.
            </div>
          </>
        )}
        </ErrorBoundary>
      </div>
    </div>
  );
}
