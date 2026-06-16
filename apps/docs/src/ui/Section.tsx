import type { ReactNode, CSSProperties } from 'react';

// A labeled content section with an uppercase eyebrow heading.
export function Section({ title, children, style }: { title: string; children: ReactNode; style?: CSSProperties }) {
  return (
    <section style={{ marginTop: 28, ...style }}>
      <h2
        style={{
          fontSize: 13,
          letterSpacing: '0.1em',
          textTransform: 'uppercase',
          color: 'var(--color-faint)',
          margin: '0 0 12px',
        }}
      >
        {title}
      </h2>
      {children}
    </section>
  );
}
