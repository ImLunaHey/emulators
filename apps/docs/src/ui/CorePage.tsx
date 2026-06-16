import { useParams, Link } from 'react-router-dom';
import { coreById } from '../cores';
import { MaturityPill } from './MaturityPill';
import { Formats } from './Formats';
import { Section } from './Section';
import { SpecRow } from './SpecRow';
import { BulletPanel } from './BulletPanel';
import { GamesTable } from './GamesTable';

const REPO_TREE = 'https://github.com/ImLunaHey/emulators/tree/main';

export function CorePage() {
  const { id } = useParams();
  const core = coreById(id);

  if (!core) {
    return (
      <div className="card" style={{ padding: 28, textAlign: 'center' }}>
        <p style={{ color: 'var(--color-muted)' }}>No core called “{id}”.</p>
        <Link to="/" className="hover:text-[var(--color-fg)]" style={{ color: 'var(--color-accent-strong)' }}>
          ← Back to all cores
        </Link>
      </div>
    );
  }

  return (
    <article>
      <Link to="/" style={{ color: 'var(--color-muted)', fontSize: 12 }} className="hover:text-[var(--color-fg)]">
        ← All cores
      </Link>

      {/* Header */}
      <header style={{ margin: '18px 0 28px', borderLeft: `3px solid ${core.accent}`, paddingLeft: 18 }}>
        <div className="flex flex-wrap items-center gap-3">
          <h1 style={{ fontSize: 28, margin: 0, fontWeight: 700 }}>{core.name}</h1>
          <MaturityPill maturity={core.maturity} />
        </div>
        <div className="eyebrow" style={{ marginTop: 8 }}>{core.tagline}</div>
        <p style={{ color: 'var(--color-fg)', fontSize: 14, lineHeight: 1.7, marginTop: 14, maxWidth: 760 }}>
          {core.status}
        </p>
        <div className="flex flex-wrap items-center gap-x-5 gap-y-2" style={{ marginTop: 16, fontSize: 12 }}>
          <span style={{ color: 'var(--color-faint)' }}>
            crate <code className="chip">{core.crate}</code>
          </span>
          <a
            href={`${REPO_TREE}/${core.dir}`}
            target="_blank"
            rel="noreferrer"
            style={{ color: 'var(--color-accent-strong)' }}
            className="hover:underline"
          >
            {core.dir} ↗
          </a>
          <span className="flex items-center gap-2" style={{ color: 'var(--color-faint)' }}>
            loads <Formats formats={core.formats} />
          </span>
        </div>
      </header>

      {/* Subsystem support table */}
      <Section title="Hardware support">
        <div className="card" style={{ padding: '4px 20px' }}>
          <SpecRow label="CPU" value={core.cpu} />
          <SpecRow label="Video" value={core.video} />
          <SpecRow label="Audio" value={core.audio} />
          <SpecRow label="Saves" value={core.saves} />
          <SpecRow label="Input" value={core.input} />
        </div>
      </Section>

      {/* Features + limitations side by side */}
      <div
        className="grid gap-3"
        style={{ gridTemplateColumns: 'repeat(auto-fit, minmax(280px, 1fr))', marginTop: 28 }}
      >
        <BulletPanel title="Implemented" items={core.features} tone="good" />
        <BulletPanel title="Not yet / known gaps" items={core.limitations} tone="gap" />
      </div>

      {/* Tested games */}
      <Section title="Tested games" style={{ marginTop: 28 }}>
        <GamesTable games={core.testedGames} />
      </Section>

      {/* Test suite */}
      <Section title="Test coverage" style={{ marginTop: 28 }}>
        <div className="card" style={{ padding: 20, fontSize: 13, lineHeight: 1.7, color: 'var(--color-fg)' }}>
          {core.tests}
        </div>
      </Section>
    </article>
  );
}
