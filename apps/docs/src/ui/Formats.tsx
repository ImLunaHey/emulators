// Monospace file-format chips (.gba, .nes …).
export function Formats({ formats }: { formats: string[] }) {
  return (
    <span className="flex flex-wrap gap-1.5">
      {formats.map((f) => (
        <code key={f} className="chip">
          {f}
        </code>
      ))}
    </span>
  );
}
