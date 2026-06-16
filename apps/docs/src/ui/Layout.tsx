import type { ReactNode } from 'react';
import { Link } from 'react-router-dom';

const REPO_URL = 'https://github.com/ImLunaHey/emulators';

// Shared chrome around every page: a sticky header that links home + to the
// repo, and a footer noting where the data comes from.
export function Layout({ children }: { children: ReactNode }) {
  return (
    <div style={{ minHeight: '100vh', display: 'flex', flexDirection: 'column' }}>
      <header
        style={{
          position: 'sticky',
          top: 0,
          zIndex: 10,
          backdropFilter: 'blur(10px)',
          background: 'rgba(10,10,12,0.7)',
          borderBottom: '1px solid var(--color-border)',
        }}
      >
        <div className="mx-auto flex items-center justify-between gap-4" style={{ maxWidth: 1040, padding: '14px 20px' }}>
          <Link to="/" className="flex items-baseline gap-2">
            <span style={{ fontWeight: 700, letterSpacing: '0.04em' }}>emulators</span>
            <span className="eyebrow">docs</span>
          </Link>
          <nav className="flex items-center gap-4" style={{ fontSize: 12, color: 'var(--color-muted)' }}>
            <Link to="/" className="hover:text-[var(--color-fg)]">Cores</Link>
            <a href={REPO_URL} target="_blank" rel="noreferrer" className="hover:text-[var(--color-fg)]">
              GitHub ↗
            </a>
          </nav>
        </div>
      </header>

      <main className="mx-auto w-full" style={{ maxWidth: 1040, padding: '32px 20px 64px', flex: 1 }}>
        {children}
      </main>

      <footer style={{ borderTop: '1px solid var(--color-border)' }}>
        <div
          className="mx-auto"
          style={{ maxWidth: 1040, padding: '20px', fontSize: 11, color: 'var(--color-faint)', lineHeight: 1.7 }}
        >
          Every core is a dependency-free Rust crate, compiled to WebAssembly for the browser and to a native
          library for the macOS app. Support details here are derived from each core's source, tests, and{' '}
          <code>CONTRACT.md</code>; status reflects what actually runs today, gaps included.
        </div>
      </footer>
    </div>
  );
}
