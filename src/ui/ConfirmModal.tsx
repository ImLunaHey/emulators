import { useEffect } from 'react';

// Reusable confirm dialog. Replaces window.confirm() so we get a
// themed dark UI + the modal closes cleanly on Esc / backdrop click
// (browser confirm() is also blocking, which doesn't compose with
// async actions like IndexedDB deletes).
interface Props {
  open: boolean;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmModal({
  open,
  title,
  message,
  confirmLabel = 'Confirm',
  cancelLabel = 'Cancel',
  danger = false,
  onConfirm,
  onCancel,
}: Props) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
      if (e.key === 'Enter') onConfirm();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onCancel, onConfirm]);
  if (!open) return null;
  return (
    <div
      className="fixed inset-0 bg-black/70 flex items-center justify-center z-[2000]"
      onClick={onCancel}
    >
      <div
        className="bg-[#14141a] border border-[#2a2a30] rounded-lg p-5 w-full max-w-[420px] mx-3 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="text-sm font-bold mb-2">{title}</div>
        <div className="text-xs opacity-80 mb-4 whitespace-pre-line">{message}</div>
        <div className="flex justify-end gap-2">
          <button onClick={onCancel} className="btn-default !text-[11px]">{cancelLabel}</button>
          <button
            onClick={onConfirm}
            className={`btn-default !text-[11px] ${danger ? '!text-red-300 hover:!bg-red-900/30' : ''}`}
            autoFocus
          >{confirmLabel}</button>
        </div>
      </div>
    </div>
  );
}

// Small hook to drive a single confirm modal from a useState.
import { useState, useCallback } from 'react';
export interface ConfirmPrompt {
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
}
export function useConfirm() {
  const [prompt, setPrompt] = useState<ConfirmPrompt | null>(null);
  const ask = useCallback((p: ConfirmPrompt) => setPrompt(p), []);
  const close = useCallback(() => setPrompt(null), []);
  const node = prompt ? (
    <ConfirmModal
      open
      title={prompt.title}
      message={prompt.message}
      confirmLabel={prompt.confirmLabel}
      cancelLabel={prompt.cancelLabel}
      danger={prompt.danger}
      onConfirm={() => { const fn = prompt.onConfirm; close(); fn(); }}
      onCancel={close}
    />
  ) : null;
  return { ask, node };
}
