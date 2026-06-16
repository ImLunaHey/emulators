import { Link } from 'react-router-dom';
import type { Core } from '../cores';
import { MaturityPill } from './MaturityPill';
import { Formats } from './Formats';

// A single core's tile on the catalog index: accent dot, name + tagline,
// maturity pill, one-line status, and its accepted formats.
export function CoreCard({ core }: { core: Core }) {
  return (
    <Link to={`/core/${core.id}`} className="card card-hover block" style={{ padding: 18 }}>
      <div className="flex items-start justify-between gap-3">
        <div className="flex items-center gap-3" style={{ minWidth: 0 }}>
          <span
            aria-hidden
            style={{
              width: 12,
              height: 12,
              borderRadius: 4,
              flexShrink: 0,
              background: core.accent,
              boxShadow: `0 0 14px -2px ${core.accent}`,
            }}
          />
          <div style={{ minWidth: 0 }}>
            <div style={{ fontWeight: 700, fontSize: 15 }}>{core.name}</div>
            <div className="eyebrow" style={{ marginTop: 3 }}>{core.tagline}</div>
          </div>
        </div>
        <MaturityPill maturity={core.maturity} />
      </div>

      <p style={{ color: 'var(--color-muted)', fontSize: 12.5, lineHeight: 1.6, marginTop: 14 }}>
        {core.status}
      </p>

      <div className="flex items-center justify-between gap-3" style={{ marginTop: 14 }}>
        <Formats formats={core.formats} />
        {core.testedGames.length > 0 && (
          <span className="eyebrow" style={{ whiteSpace: 'nowrap' }}>
            {core.testedGames.length} tested
          </span>
        )}
      </div>
    </Link>
  );
}
