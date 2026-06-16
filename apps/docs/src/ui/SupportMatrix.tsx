import type { MatrixGroup, Support } from '../cores';
import { SUPPORT_LABEL } from '../cores';

const GLYPH: Record<Support, string> = { yes: '✓', partial: '~', testing: '?', no: '✗' };

// A grouped capability matrix: one row per feature, a status cell (Supported /
// Partial / Needs testing / Not yet), and an optional note. Driven by
// `core.matrix`.
export function SupportMatrix({ groups }: { groups: MatrixGroup[] }) {
  const counts = tally(groups);
  return (
    <div>
      <div className="flex flex-wrap gap-2" style={{ marginBottom: 14 }}>
        {(Object.keys(counts) as Support[]).map((s) =>
          counts[s] ? (
            <span key={s} className={`mx-key mx-${s}`}>
              <span className="mx-dot">{GLYPH[s]}</span> {counts[s]} {SUPPORT_LABEL[s]}
            </span>
          ) : null,
        )}
      </div>

      <div className="card" style={{ overflow: 'hidden' }}>
        <table className="matrix">
          <tbody>
            {groups.map((g) => (
              <GroupBlock key={g.group} group={g} />
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function GroupBlock({ group }: { group: MatrixGroup }) {
  return (
    <>
      <tr className="mx-group">
        <th colSpan={3}>{group.group}</th>
      </tr>
      {group.rows.map((r) => (
        <tr key={r.feature}>
          <td className="mx-feature">{r.feature}</td>
          <td className="mx-status">
            <span className={`mx-badge mx-${r.support}`}>
              <span className="mx-dot">{GLYPH[r.support]}</span> {SUPPORT_LABEL[r.support]}
            </span>
          </td>
          <td className="mx-note">{r.note ?? ''}</td>
        </tr>
      ))}
    </>
  );
}

function tally(groups: MatrixGroup[]): Record<Support, number> {
  const c: Record<Support, number> = { yes: 0, partial: 0, testing: 0, no: 0 };
  for (const g of groups) for (const r of g.rows) c[r.support]++;
  return c;
}
