import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from 'react';

// Lightweight toast system. Gives ephemeral feedback for user actions
// (save downloaded, state loaded, import failed, …) so the UI no longer
// relies solely on the scrolling debug log pane. Mounted once at the App
// root via <ToastProvider>; any component calls useToast().

export type ToastKind = 'info' | 'success' | 'error';

interface Toast {
  id: number;
  msg: string;
  kind: ToastKind;
  leaving: boolean;
}

interface ToastApi {
  show: (msg: string, kind?: ToastKind) => void;
  info: (msg: string) => void;
  success: (msg: string) => void;
  error: (msg: string) => void;
}

const ToastContext = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error('useToast must be used inside <ToastProvider>');
  return ctx;
}

const ICON: Record<ToastKind, string> = { info: 'ℹ', success: '✓', error: '!' };
// Errors linger a little longer than confirmations.
const TTL: Record<ToastKind, number> = { info: 3000, success: 3000, error: 5000 };
const EXIT_MS = 200;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const idRef = useRef(0);

  // Two-phase removal: mark `leaving` to play the exit animation, then
  // drop from the array once it's finished.
  const dismiss = useCallback((id: number) => {
    setToasts((list) => list.map((t) => (t.id === id ? { ...t, leaving: true } : t)));
    window.setTimeout(() => {
      setToasts((list) => list.filter((t) => t.id !== id));
    }, EXIT_MS);
  }, []);

  const show = useCallback((msg: string, kind: ToastKind = 'info') => {
    const id = ++idRef.current;
    setToasts((list) => [...list, { id, msg, kind, leaving: false }]);
    window.setTimeout(() => dismiss(id), TTL[kind]);
  }, [dismiss]);

  const api = useMemo<ToastApi>(() => ({
    show,
    info: (m) => show(m, 'info'),
    success: (m) => show(m, 'success'),
    error: (m) => show(m, 'error'),
  }), [show]);

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="toast-stack" role="status" aria-live="polite">
        {toasts.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`toast toast-${t.kind} ${t.leaving ? 'toast-leaving' : ''}`}
            onClick={() => dismiss(t.id)}
            title="Dismiss"
          >
            <span className="toast-icon">{ICON[t.kind]}</span>
            <span className="toast-msg">{t.msg}</span>
          </button>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
