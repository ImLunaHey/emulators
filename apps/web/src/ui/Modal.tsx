import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react';

// Shared modal shell. Every panel (Debug, Controller, Cheats, Link) and
// the confirm dialog render through this so they all share the same
// backdrop, entrance animation, Esc-to-close, body-scroll lock, and
// header layout — previously each one reimplemented this with small
// inconsistencies (different close-button sizes, no Esc handling, no
// scroll lock).

const SIZES = {
  sm: 'max-w-[460px]',
  md: 'max-w-[680px]',
  lg: 'max-w-[760px]',
  xl: 'max-w-[860px]',
} as const;

interface Props {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  subtitle?: ReactNode;
  /** Extra controls rendered in the header, left of the close button. */
  headerExtra?: ReactNode;
  size?: keyof typeof SIZES;
  /** When false, the body doesn't scroll itself (the panel content
      manages its own scrolling regions). Defaults to true. */
  scrollBody?: boolean;
  children: ReactNode;
}

export function Modal({
  open,
  onClose,
  title,
  subtitle,
  headerExtra,
  size = 'md',
  scrollBody = true,
  children,
}: Props) {
  // Esc closes; lock the page scroll while open so the backdrop doesn't
  // scroll the library/player behind it.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      window.removeEventListener('keydown', onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [open, onClose]);

  // Swipe-to-dismiss for the mobile bottom-sheet form. Dragging the
  // header down past a threshold closes; a smaller drag snaps back (the
  // CSS transition on the panel handles the snap animation).
  const dragStart = useRef<number | null>(null);
  const [dragY, setDragY] = useState(0);
  const [dragging, setDragging] = useState(false);
  const isSheet = () => typeof window !== 'undefined' && window.matchMedia('(max-width: 560px)').matches;

  const onHeaderPointerDown = (e: ReactPointerEvent) => {
    // Don't hijack taps on the close button / header controls.
    if (!isSheet() || (e.target as HTMLElement).closest('button')) return;
    dragStart.current = e.clientY;
    setDragging(true);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onHeaderPointerMove = (e: ReactPointerEvent) => {
    if (dragStart.current === null) return;
    setDragY(Math.max(0, e.clientY - dragStart.current));
  };
  const endDrag = () => {
    if (dragStart.current === null) return;
    const shouldClose = dragY > 100;
    dragStart.current = null;
    setDragging(false);
    setDragY(0);
    if (shouldClose) onClose();
  };

  if (!open) return null;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className={`modal-panel ${SIZES[size]}`}
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
        style={{
          transform: dragY > 0 ? `translateY(${dragY}px)` : undefined,
          transition: dragging ? 'none' : undefined,
        }}
      >
        <div
          className="modal-header touch-none"
          onPointerDown={onHeaderPointerDown}
          onPointerMove={onHeaderPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          <div className="min-w-0">
            <div className="modal-title truncate">{title}</div>
            {subtitle != null && <div className="modal-subtitle truncate">{subtitle}</div>}
          </div>
          <div className="flex items-center gap-1 shrink-0">
            {headerExtra}
            <button onClick={onClose} className="btn-icon" aria-label="Close">×</button>
          </div>
        </div>
        <div className={scrollBody ? 'modal-body' : 'modal-body !overflow-hidden flex flex-col'}>
          {children}
        </div>
      </div>
    </div>
  );
}
