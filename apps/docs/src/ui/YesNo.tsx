// A yes/no glyph for the tested-games table.
export function YesNo({ value }: { value: boolean }) {
  return value ? <span className="yes">✓</span> : <span className="no">·</span>;
}
