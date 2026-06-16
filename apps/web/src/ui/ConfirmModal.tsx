import { useEffect } from 'react';
import { Modal } from './Modal';

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
  // Modal handles Esc; we add Enter = confirm on top.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Enter') onConfirm(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onConfirm]);
  return (
    <Modal open={open} onClose={onCancel} title={title} size="sm" scrollBody={false}>
      <div className="text-xs opacity-80 mb-5 whitespace-pre-line leading-relaxed">{message}</div>
      <div className="flex justify-end gap-2">
        <button onClick={onCancel} className="btn">{cancelLabel}</button>
        <button
          onClick={onConfirm}
          className={danger ? 'btn btn-danger' : 'btn btn-primary'}
          autoFocus
        >{confirmLabel}</button>
      </div>
    </Modal>
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
