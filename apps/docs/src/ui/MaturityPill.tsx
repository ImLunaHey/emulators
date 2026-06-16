import type { Maturity } from '../cores';
import { MATURITY_LABEL } from '../cores';

// A colored maturity pill ("Mature" / "Playable" / "In progress" / "Foundation").
export function MaturityPill({ maturity }: { maturity: Maturity }) {
  return <span className={`pill pill-${maturity}`}>{MATURITY_LABEL[maturity]}</span>;
}
