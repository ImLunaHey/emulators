// Stylized GBA cartridge / box-art placeholder. Used by the library
// list until/unless Hasheous returns a real cover URL (it doesn't —
// IGDB covers require an API key the project doesn't have). The
// design picks a deterministic color from a hash of the title so the
// same game always renders the same shade.

interface Props {
  title: string;
  subtitle?: string;
  className?: string;
}

function colorFor(title: string): { bg: string; fg: string; accent: string } {
  // Cheap deterministic hash → hue.
  let h = 0;
  for (let i = 0; i < title.length; i++) h = (h * 31 + title.charCodeAt(i)) & 0xFFFF;
  const hue = h % 360;
  return {
    bg:     `hsl(${hue} 35% 25%)`,
    fg:     `hsl(${hue} 60% 88%)`,
    accent: `hsl(${(hue + 30) % 360} 60% 45%)`,
  };
}

export function CartArt({ title, subtitle, className }: Props) {
  const c = colorFor(title);
  return (
    <div
      className={`relative overflow-hidden rounded-md flex flex-col justify-end ${className ?? ''}`}
      style={{
        background: `linear-gradient(135deg, ${c.bg} 0%, ${c.accent} 100%)`,
        aspectRatio: '1 / 1',
      }}
      aria-hidden
    >
      {/* Cartridge ridge — a horizontal band near the top, evocative of
          the GBA cart's spine. */}
      <div
        className="absolute inset-x-0 top-0 h-3"
        style={{ background: c.accent, borderBottom: `1px solid ${c.fg}33` }}
      />
      <div
        className="absolute inset-x-2 top-4 h-1 rounded-full"
        style={{ background: `${c.fg}44` }}
      />
      <div
        className="absolute left-3 right-12 top-7 h-3 rounded-sm"
        style={{ background: `${c.fg}22`, border: `1px solid ${c.fg}33` }}
      />
      {/* Title label sticker. */}
      <div
        className="m-2 p-2 rounded-sm"
        style={{
          background: `${c.fg}10`,
          border: `1px solid ${c.fg}33`,
          color: c.fg,
        }}
      >
        <div className="text-[11px] font-bold tracking-wide leading-tight">
          {title}
        </div>
        {subtitle && (
          <div className="text-[9px] opacity-60 mt-1 truncate">{subtitle}</div>
        )}
      </div>
    </div>
  );
}
