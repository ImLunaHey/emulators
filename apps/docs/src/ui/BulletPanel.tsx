// A titled list of bullets — used for both the "Implemented" (good) and
// "Not yet / known gaps" (gap) columns on a core page. `tone` switches the
// bullet color and heading accent.
export function BulletPanel({ title, items, tone }: { title: string; items: string[]; tone: 'good' | 'gap' }) {
  const headingColor = tone === 'good' ? 'var(--color-success)' : 'var(--color-warn)';
  return (
    <div className="card" style={{ padding: 20 }}>
      <h3
        style={{
          fontSize: 12,
          letterSpacing: '0.1em',
          textTransform: 'uppercase',
          color: headingColor,
          margin: '0 0 14px',
        }}
      >
        {title}
      </h3>
      <ul className={`bullets bullets-${tone}`}>
        {items.map((item) => <li key={item}>{item}</li>)}
      </ul>
    </div>
  );
}
