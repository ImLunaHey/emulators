import { useEffect, useRef } from 'react';

interface Props { lines: string[]; }

export function LogPane({ lines }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [lines]);
  return (
    <div className="log" ref={ref}>
      {lines.map((l, i) => <div key={i}>{l}</div>)}
    </div>
  );
}
