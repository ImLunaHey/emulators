import { useEffect, useRef } from 'react';

interface Props { lines: string[]; }

export function LogPane({ lines }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [lines]);
  return (
    <div
      ref={ref}
      className="w-full max-w-[720px] h-[120px] overflow-auto well p-2.5 text-[11px] text-[var(--color-muted)] whitespace-pre-wrap leading-relaxed"
    >
      {lines.map((l, i) => <div key={i}>{l}</div>)}
    </div>
  );
}
