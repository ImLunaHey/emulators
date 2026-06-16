import type { TestedGame } from '../cores';
import { YesNo } from './YesNo';

// The boots / plays / sound matrix of verified titles for a core. Renders a
// prompt to report results when nothing is recorded yet.
export function GamesTable({ games }: { games: TestedGame[] }) {
  if (games.length === 0) {
    return (
      <div className="card" style={{ padding: 20, color: 'var(--color-muted)', fontSize: 13, lineHeight: 1.7 }}>
        No specific titles are recorded as verified for this core yet — correctness is covered by the test suite
        below. Tried a game on it?{' '}
        <a
          href="https://github.com/ImLunaHey/emulators/issues"
          target="_blank"
          rel="noreferrer"
          style={{ color: 'var(--color-accent-strong)' }}
          className="hover:underline"
        >
          Report results ↗
        </a>
      </div>
    );
  }

  return (
    <div className="card" style={{ overflow: 'hidden' }}>
      <table className="games">
        <thead>
          <tr>
            <th>Title</th>
            <th className="col-center">Boots</th>
            <th className="col-center">Plays</th>
            <th className="col-center">Sound</th>
            <th>Notes</th>
          </tr>
        </thead>
        <tbody>
          {games.map((g) => (
            <tr key={g.title}>
              <td style={{ fontWeight: 600 }}>{g.title}</td>
              <td className="col-center"><YesNo value={g.boots} /></td>
              <td className="col-center"><YesNo value={g.plays} /></td>
              <td className="col-center"><YesNo value={g.sound} /></td>
              <td style={{ color: 'var(--color-muted)' }}>{g.notes ?? ''}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
