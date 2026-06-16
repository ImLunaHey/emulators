import { CORES, MATURITY_LABEL } from '../cores';
import type { Maturity } from '../cores';
import { CoreCard } from './CoreCard';

// Order cores by maturity (most mature first), then keep the registry order.
const MATURITY_RANK: Record<Maturity, number> = { mature: 0, playable: 1, wip: 2, foundation: 3 };
const ORDERED = [...CORES].sort((a, b) => MATURITY_RANK[a.maturity] - MATURITY_RANK[b.maturity]);

const TOTAL_TESTED = CORES.reduce((n, c) => n + c.testedGames.length, 0);
const MATURITIES: Maturity[] = ['mature', 'playable', 'wip', 'foundation'];

export function HomePage() {
  return (
    <div>
      {/* Hero */}
      <section style={{ marginBottom: 40 }}>
        <p className="eyebrow" style={{ marginBottom: 10 }}>Emulator cores</p>
        <h1 style={{ fontSize: 30, lineHeight: 1.2, margin: 0, fontWeight: 700, maxWidth: 720 }}>
          What each core supports
        </h1>
        <p style={{ color: 'var(--color-muted)', fontSize: 14, lineHeight: 1.7, maxWidth: 720, marginTop: 14 }}>
          A reference for the {CORES.length} from-scratch Rust emulator cores in this repo — the CPU, video, audio,
          saves, and input each one implements, which file formats it loads, its test coverage, and the games verified
          on it. Pick a system to see the full breakdown.
        </p>
        <div className="flex flex-wrap gap-2" style={{ marginTop: 18 }}>
          {MATURITIES.map((m) => {
            const count = CORES.filter((c) => c.maturity === m).length;
            if (!count) return null;
            return (
              <span key={m} className={`pill pill-${m}`}>
                {count} {MATURITY_LABEL[m]}
              </span>
            );
          })}
          <span className="pill">{TOTAL_TESTED} games verified</span>
        </div>
      </section>

      {/* Core grid */}
      <div className="grid gap-3" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(300px, 1fr))' }}>
        {ORDERED.map((core) => (
          <CoreCard key={core.id} core={core} />
        ))}
      </div>
    </div>
  );
}
