import { Component, type ErrorInfo, type ReactNode } from 'react';

// React's error-boundary contract: a class component with either
// componentDidCatch or getDerivedStateFromError will catch errors
// thrown by descendants during render, in lifecycle methods, and in
// constructors of the whole tree below them. Effects/event handlers
// are NOT caught (React doesn't surface those to boundaries) — those
// remain the responsibility of the call site.
//
// We use this in three places:
//   - around the emulator <Screen> ("player") so a render fault in the
//     PPU compositor or hooks doesn't blank the whole UI
//   - around the controls row + virtual gamepad so a controller-map
//     glitch can't take the page down
//   - inside each modal, around the modal body — the overlay + close
//     button stay reachable even if the contents explode
//
// Each instance stays in the error state until the parent passes a
// new `resetKey` (or the user clicks the inline Retry button), which
// flips the boundary's `error` back to null and re-renders children.

interface Props {
  children: ReactNode;
  // Short human-readable name for the failure card, e.g. "Player".
  label: string;
  // When this value changes, the boundary clears its error and re-
  // renders children. Useful for modals where the parent reopens the
  // dialog — the new open transition should retry.
  resetKey?: unknown;
  // Optional "close this" callback, surfaced as a button alongside
  // Retry. Modals pass onClose here so the user can dismiss even when
  // the inner UI is broken.
  onClose?: () => void;
  // When true the fallback collapses to a compact inline card suited
  // for sitting inside a modal body; otherwise it occupies its layout
  // slot with a slightly larger card.
  variant?: 'inline' | 'block';
}

interface State {
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Keep the console actionable — React already logs the error itself
    // in dev, but the component stack from `info` is the bit you
    // actually want when debugging.
    // eslint-disable-next-line no-console
    console.error(`[ErrorBoundary:${this.props.label}]`, error, info.componentStack);
  }

  componentDidUpdate(prevProps: Props): void {
    if (this.state.error && prevProps.resetKey !== this.props.resetKey) {
      this.setState({ error: null });
    }
  }

  private retry = () => this.setState({ error: null });

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    const { label, onClose, variant = 'block' } = this.props;
    const card =
      variant === 'inline'
        ? 'bg-[#2a1414] border border-[#5a2828] rounded-md p-3 text-[11px] text-red-200'
        : 'bg-[#2a1414] border border-[#5a2828] rounded-md p-4 text-xs text-red-200';
    return (
      <div className={card} role="alert">
        <div className="font-bold mb-1">{label} crashed</div>
        <div className="font-mono opacity-80 mb-3 break-all">{error.message || String(error)}</div>
        <div className="flex gap-2">
          <button onClick={this.retry} className="btn-default !text-[10px]">↻ Retry</button>
          {onClose && (
            <button onClick={onClose} className="btn-default !text-[10px]">Close</button>
          )}
        </div>
      </div>
    );
  }
}
