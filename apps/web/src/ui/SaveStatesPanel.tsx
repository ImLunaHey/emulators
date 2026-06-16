import { useEffect, useState } from 'react';
import { Modal } from './Modal';
import type { Emulator } from '../emulator';
import { saveState, loadState } from '../savestate';
import { ErrorBoundary } from './ErrorBoundary';
import { putState, getStateBlob, listStates, deleteState, type StateMeta } from './stateStore';

interface Props {
  open: boolean;
  emu: Emulator;
  gameCode: string | null;
  onNotify: (msg: string, kind?: 'info' | 'success' | 'error') => void;
  onClose: () => void;
}

// Slot 0 is the "quick" slot (toolbar / F2-F4); 1..6 are manual slots
// shown in the grid here.
const MANUAL_SLOTS = [1, 2, 3, 4, 5, 6];

// Render the current framebuffer to a small PNG data URL for the slot
// preview. The PPU frame is the raw 240×160 RGBA buffer the Screen
// blits each frame.
export function captureThumb(emu: Emulator): string {
  const src = document.createElement('canvas');
  src.width = 240; src.height = 160;
  const sctx = src.getContext('2d');
  if (!sctx) return '';
  const img = sctx.createImageData(240, 160);
  img.data.set(emu.ppu.frame);
  sctx.putImageData(img, 0, 0);
  // Downscale to keep the stored thumbnail tiny.
  const dst = document.createElement('canvas');
  dst.width = 120; dst.height = 80;
  const dctx = dst.getContext('2d');
  if (!dctx) return src.toDataURL('image/png');
  dctx.imageSmoothingEnabled = false;
  dctx.drawImage(src, 0, 0, 120, 80);
  return dst.toDataURL('image/png');
}

function timeAgo(ts: number): string {
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 60) return 'just now';
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

export function SaveStatesPanel({ open, emu, gameCode, onNotify, onClose }: Props) {
  const [slots, setSlots] = useState<Map<number, StateMeta>>(new Map());
  const [loading, setLoading] = useState(false);

  const refresh = async () => {
    if (!gameCode) { setSlots(new Map()); return; }
    setLoading(true);
    try {
      const list = await listStates(gameCode);
      setSlots(new Map(list.map((m) => [m.slot, m])));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (open) refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, gameCode]);

  const onSave = async (slot: number) => {
    if (!gameCode) return;
    try {
      const blob = saveState(emu);
      const thumb = captureThumb(emu);
      await putState(gameCode, slot, blob, thumb);
      onNotify(`Saved to slot ${slot}`, 'success');
      refresh();
    } catch (e) {
      onNotify('Save state failed: ' + (e as Error).message, 'error');
    }
  };

  const onLoad = async (slot: number) => {
    if (!gameCode) return;
    try {
      const blob = await getStateBlob(gameCode, slot);
      if (!blob) { onNotify(`Slot ${slot} is empty`, 'error'); return; }
      loadState(emu, blob);
      onNotify(`Loaded slot ${slot}`, 'success');
      onClose();
    } catch (e) {
      onNotify('Load state failed: ' + (e as Error).message, 'error');
    }
  };

  const onDelete = async (slot: number) => {
    if (!gameCode) return;
    await deleteState(gameCode, slot);
    onNotify(`Cleared slot ${slot}`);
    refresh();
  };

  return (
    <Modal open={open} onClose={onClose} title="Save States" subtitle={gameCode ? `game code ${gameCode}` : 'no ROM loaded'} size="md">
      <ErrorBoundary label="Save States" onClose={onClose} variant="inline">
        {!gameCode ? (
          <div className="py-10 text-center opacity-50 text-xs">Load a ROM to use save states.</div>
        ) : (
          <>
            <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
              {MANUAL_SLOTS.map((slot) => {
                const meta = slots.get(slot);
                return (
                  <div key={slot} className="well overflow-hidden flex flex-col">
                    <div className="relative aspect-[3/2] bg-black">
                      {meta ? (
                        <img src={meta.thumb} alt={`Slot ${slot}`} className="absolute inset-0 w-full h-full object-cover" style={{ imageRendering: 'pixelated' }} />
                      ) : (
                        <div className="absolute inset-0 grid place-items-center text-[var(--color-faint)] text-2xl">＋</div>
                      )}
                      <div className="absolute top-1 left-1 text-[10px] font-bold px-1.5 py-0.5 rounded bg-black/60 text-[var(--color-accent)]">
                        {slot}
                      </div>
                    </div>
                    <div className="p-2">
                      <div className="text-[10px] opacity-60 mb-1.5 h-3.5">
                        {meta ? timeAgo(meta.savedAt) : 'empty'}
                      </div>
                      <div className="flex gap-1.5">
                        <button onClick={() => onSave(slot)} className="btn !text-[10px] flex-1 !px-2">{meta ? 'Overwrite' : 'Save'}</button>
                        <button onClick={() => onLoad(slot)} disabled={!meta} className="btn btn-primary !text-[10px] flex-1 !px-2">Load</button>
                        {meta && (
                          <button onClick={() => onDelete(slot)} className="btn-icon !w-7 !h-7 !text-sm hover:!text-red-400" title="Delete slot">🗑</button>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
            <div className="mt-4 text-[10px] opacity-50 leading-relaxed">
              {loading ? 'Loading slots…' : 'Slots are stored locally in IndexedDB, per game. The toolbar ⚡ buttons and F2 / F4 use a separate quick slot. Save states also capture cartridge save data.'}
            </div>
          </>
        )}
      </ErrorBoundary>
    </Modal>
  );
}
