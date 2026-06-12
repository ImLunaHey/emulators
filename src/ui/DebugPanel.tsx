import { useEffect, useRef, useState } from 'react';
import type { Emulator } from '../emulator';
import { ErrorBoundary } from './ErrorBoundary';
import { Modal } from './Modal';

interface Props {
  open: boolean;
  emu: Emulator;
  onClose: () => void;
}

type Tab = 'cpu' | 'mem' | 'palette' | 'tiles' | 'sprites' | 'io' | 'adv';

const TABS: Tab[] = ['cpu', 'mem', 'palette', 'tiles', 'sprites', 'io', 'adv'];

export function DebugPanel({ open, emu, onClose }: Props) {
  const [tab, setTab] = useState<Tab>('cpu');

  return (
    <Modal open={open} onClose={onClose} title="Debug" size="xl" scrollBody={false}>
      <div className="seg mb-3">
        {TABS.map((t) => (
          <button key={t} onClick={() => setTab(t)} data-active={tab === t} className="seg-item font-medium">
            {t.toUpperCase()}
          </button>
        ))}
      </div>
      <div className="flex-1 overflow-y-auto -mx-1 px-1">
        {/* resetKey=tab so switching tabs after a crash clears the
            fallback and gives the new view a clean try. */}
        <ErrorBoundary label={`Debug · ${tab.toUpperCase()}`} resetKey={tab} onClose={onClose} variant="inline">
          {tab === 'cpu'     && <CpuView emu={emu} />}
          {tab === 'mem'     && <MemoryView emu={emu} />}
          {tab === 'palette' && <PaletteView emu={emu} />}
          {tab === 'tiles'   && <TilesView emu={emu} />}
          {tab === 'sprites' && <SpritesView emu={emu} />}
          {tab === 'io'      && <IoView emu={emu} />}
          {tab === 'adv'     && <AdvancedView emu={emu} />}
        </ErrorBoundary>
      </div>
    </Modal>
  );
}

// rAF-driven re-render so live views (CPU regs, palette, etc.) stay
// fresh while the game is running. Returns a tick counter that incre-
// ments at ~10 Hz so the view function re-runs with current data.
function useLiveTick(hz = 10): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let stop = false;
    let last = 0;
    const periodMs = 1000 / hz;
    const loop = (ts: number) => {
      if (stop) return;
      requestAnimationFrame(loop);
      if (ts - last < periodMs) return;
      last = ts;
      setTick((t) => t + 1);
    };
    const raf = requestAnimationFrame(loop);
    return () => { stop = true; cancelAnimationFrame(raf); };
  }, [hz]);
  return tick;
}

function hex(v: number, w = 8): string {
  return v.toString(16).toUpperCase().padStart(w, '0');
}

function CpuView({ emu }: { emu: Emulator }) {
  useLiveTick(20);
  const s = emu.cpu.state;
  const cpsr = s.cpsr >>> 0;
  const mode = ['USR', '???', '???', '???', '???', '???', '???', '???',
                '???', '???', '???', '???', '???', '???', '???', '???',
                'USR', 'FIQ', 'IRQ', 'SVC', '???', '???', '???', 'ABT',
                '???', '???', '???', 'UND', '???', '???', '???', 'SYS'][cpsr & 0x1F] || `0x${(cpsr & 0x1F).toString(16)}`;
  const flag = (mask: number, ch: string) => (cpsr & mask) ? ch : '·';
  return (
    <div className="grid grid-cols-2 gap-4 text-[11px] font-mono">
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-2">Registers</div>
        <div className="grid grid-cols-2 gap-x-3 gap-y-0.5">
          {Array.from({ length: 16 }, (_, i) => (
            <div key={i} className="flex justify-between">
              <span className="opacity-50">{i === 13 ? 'SP' : i === 14 ? 'LR' : i === 15 ? 'PC' : `R${i.toString().padStart(2, ' ')}`}</span>
              <span>{hex(s.r[i] >>> 0)}</span>
            </div>
          ))}
        </div>
      </div>
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-2">Status</div>
        <div className="space-y-0.5">
          <div className="flex justify-between"><span className="opacity-50">CPSR</span><span>{hex(cpsr)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">Mode</span><span>{mode}</span></div>
          <div className="flex justify-between"><span className="opacity-50">Flags</span><span>{flag(0x80000000, 'N')}{flag(0x40000000, 'Z')}{flag(0x20000000, 'C')}{flag(0x10000000, 'V')}</span></div>
          <div className="flex justify-between"><span className="opacity-50">IRQ</span><span>{(cpsr & 0x80) ? 'masked' : 'enabled'}</span></div>
          <div className="flex justify-between"><span className="opacity-50">State</span><span>{(cpsr & 0x20) ? 'THUMB' : 'ARM'}</span></div>
          <div className="flex justify-between"><span className="opacity-50">Halted</span><span>{s.halted ? 'yes' : 'no'}</span></div>
        </div>
      </div>
    </div>
  );
}

// 64-byte-per-row hex dump of any of the GBA's memory regions.
function MemoryView({ emu }: { emu: Emulator }) {
  useLiveTick(5);
  const regions: Array<[string, number, number]> = [
    ['BIOS',   0x00000000, 0x00004000],
    ['EWRAM',  0x02000000, 0x00040000],
    ['IWRAM',  0x03000000, 0x00008000],
    ['IO',     0x04000000, 0x00000400],
    ['PRAM',   0x05000000, 0x00000400],
    ['VRAM',   0x06000000, 0x00018000],
    ['OAM',    0x07000000, 0x00000400],
    ['ROM',    0x08000000, 0x02000000],
  ];
  const [regionIdx, setRegionIdx] = useState(1);
  const [offsetHex, setOffsetHex] = useState('0');
  const [name, base, size] = regions[regionIdx];
  const offset = Math.max(0, Math.min(parseInt(offsetHex, 16) || 0, size - 0x80));
  const ROWS = 16;
  return (
    <div className="text-[11px] font-mono">
      <div className="flex gap-2 items-center mb-2 text-xs">
        <select
          value={regionIdx}
          onChange={(e) => { setRegionIdx(Number(e.target.value)); setOffsetHex('0'); }}
          className="input font-mono"
        >
          {regions.map(([n, b], i) => (
            <option key={i} value={i}>{n.padEnd(6)} 0x{hex(b)}</option>
          ))}
        </select>
        <span className="opacity-50">offset 0x</span>
        <input
          value={offsetHex}
          onChange={(e) => setOffsetHex(e.target.value)}
          className="input font-mono w-20"
        />
        <button
          onClick={() => setOffsetHex(Math.max(0, offset - ROWS * 16).toString(16))}
          className="btn !text-[10px]"
        >◀ Page</button>
        <button
          onClick={() => setOffsetHex(Math.min(size - ROWS * 16, offset + ROWS * 16).toString(16))}
          className="btn !text-[10px]"
        >Page ▶</button>
      </div>
      <div className="well p-2 overflow-x-auto">
        <div className="text-[10px] opacity-50 mb-1">
          {`${name}  0x${hex(base + offset)}  ${name === 'ROM' ? '(read-only)' : ''}`}
        </div>
        {Array.from({ length: ROWS }, (_, row) => {
          const rowAddr = base + offset + row * 16;
          const bytes: number[] = [];
          for (let i = 0; i < 16; i++) bytes.push(emu.bus.read8(rowAddr + i) & 0xFF);
          const ascii = bytes.map((b) => b >= 0x20 && b < 0x7F ? String.fromCharCode(b) : '.').join('');
          return (
            <div key={row} className="whitespace-pre">
              <span className="opacity-50 mr-2">{hex(rowAddr)}</span>
              <span>{bytes.map((b) => b.toString(16).padStart(2, '0').toUpperCase()).join(' ')}</span>
              <span className="opacity-50 ml-2">{ascii}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function PaletteView({ emu }: { emu: Emulator }) {
  useLiveTick(15);
  const pram16 = emu.bus.pram16;
  const swatch = (idx: number) => {
    const c = pram16[idx] & 0x7FFF;
    const r = (c & 0x1F) << 3;
    const g = ((c >> 5) & 0x1F) << 3;
    const b = ((c >> 10) & 0x1F) << 3;
    return (
      <div
        key={idx}
        title={`#${idx.toString().padStart(3, '0')}  0x${hex(c, 4)}  rgb(${r},${g},${b})`}
        className="w-5 h-5 border border-black/30"
        style={{ backgroundColor: `rgb(${r}, ${g}, ${b})` }}
      />
    );
  };
  return (
    <div className="text-[11px]">
      <div className="text-[10px] uppercase tracking-widest opacity-50 mb-2">BG palette (16 × 16 = 256 colors)</div>
      <div className="grid grid-cols-16 gap-px mb-4" style={{ gridTemplateColumns: 'repeat(16, 20px)' }}>
        {Array.from({ length: 256 }, (_, i) => swatch(i))}
      </div>
      <div className="text-[10px] uppercase tracking-widest opacity-50 mb-2">OBJ palette (16 × 16 = 256 colors)</div>
      <div className="grid gap-px" style={{ gridTemplateColumns: 'repeat(16, 20px)' }}>
        {Array.from({ length: 256 }, (_, i) => swatch(256 + i))}
      </div>
      <div className="mt-3 text-[10px] opacity-50 leading-relaxed">
        BG palette occupies PRAM 0x000-0x1FF, OBJ palette 0x200-0x3FF.
        Hover any swatch for its index and BGR555 value.
      </div>
    </div>
  );
}

function TilesView({ emu }: { emu: Emulator }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const tick = useLiveTick(15);
  const [base, setBase] = useState<'bg' | 'obj'>('bg');
  const [bpp, setBpp] = useState<4 | 8>(4);
  const [palBank, setPalBank] = useState(0);
  // 32 tiles per row × 32 rows = 1024 tiles for 4bpp, or 16 × 16 for 8bpp.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    const vram = emu.bus.vram;
    const pram16 = emu.bus.pram16;
    const paletteOffset = base === 'obj' ? 256 : 0;
    if (bpp === 4) {
      const tilesPerRow = 32;
      const tileCount = 1024;
      const tilesPerCol = tileCount / tilesPerRow;
      canvas.width = tilesPerRow * 8;
      canvas.height = tilesPerCol * 8;
      const img = ctx.createImageData(canvas.width, canvas.height);
      const tileBase = base === 'obj' ? 0x10000 : 0;
      for (let t = 0; t < tileCount; t++) {
        const tx = (t % tilesPerRow) * 8;
        const ty = Math.floor(t / tilesPerRow) * 8;
        for (let py = 0; py < 8; py++) {
          for (let px = 0; px < 8; px++) {
            const byte = vram[tileBase + t * 32 + py * 4 + (px >> 1)];
            const pix = (px & 1) ? (byte >> 4) : (byte & 0xF);
            const c = pram16[paletteOffset + palBank * 16 + pix] & 0x7FFF;
            const r = (c & 0x1F) << 3;
            const g = ((c >> 5) & 0x1F) << 3;
            const b = ((c >> 10) & 0x1F) << 3;
            const off = ((ty + py) * canvas.width + tx + px) * 4;
            img.data[off    ] = r;
            img.data[off + 1] = g;
            img.data[off + 2] = b;
            img.data[off + 3] = 0xFF;
          }
        }
      }
      ctx.putImageData(img, 0, 0);
    } else {
      const tilesPerRow = 16;
      const tileCount = 512;
      const tilesPerCol = tileCount / tilesPerRow;
      canvas.width = tilesPerRow * 8;
      canvas.height = tilesPerCol * 8;
      const img = ctx.createImageData(canvas.width, canvas.height);
      const tileBase = base === 'obj' ? 0x10000 : 0;
      for (let t = 0; t < tileCount; t++) {
        const tx = (t % tilesPerRow) * 8;
        const ty = Math.floor(t / tilesPerRow) * 8;
        for (let py = 0; py < 8; py++) {
          for (let px = 0; px < 8; px++) {
            const pix = vram[tileBase + t * 64 + py * 8 + px];
            const c = pram16[paletteOffset + pix] & 0x7FFF;
            const r = (c & 0x1F) << 3;
            const g = ((c >> 5) & 0x1F) << 3;
            const b = ((c >> 10) & 0x1F) << 3;
            const off = ((ty + py) * canvas.width + tx + px) * 4;
            img.data[off    ] = r;
            img.data[off + 1] = g;
            img.data[off + 2] = b;
            img.data[off + 3] = 0xFF;
          }
        }
      }
      ctx.putImageData(img, 0, 0);
    }
  }, [emu, base, bpp, palBank, tick]);

  return (
    <div className="text-[11px]">
      <div className="flex gap-2 items-center mb-3 text-xs">
        <select value={base} onChange={(e) => setBase(e.target.value as 'bg' | 'obj')} className="input">
          <option value="bg">BG VRAM (0x06000000)</option>
          <option value="obj">OBJ VRAM (0x06010000)</option>
        </select>
        <select value={bpp} onChange={(e) => setBpp(Number(e.target.value) as 4 | 8)} className="input">
          <option value={4}>4bpp</option>
          <option value={8}>8bpp</option>
        </select>
        {bpp === 4 && (
          <select value={palBank} onChange={(e) => setPalBank(Number(e.target.value))} className="input">
            {Array.from({ length: 16 }, (_, i) => <option key={i} value={i}>Pal bank {i}</option>)}
          </select>
        )}
      </div>
      <div className="well p-2 inline-block">
        <canvas
          ref={canvasRef}
          style={{ imageRendering: 'pixelated', width: bpp === 4 ? 512 : 256, height: bpp === 4 ? 512 : 256 }}
        />
      </div>
    </div>
  );
}

function SpritesView({ emu }: { emu: Emulator }) {
  useLiveTick(15);
  const SHAPE_NAMES = ['SQR', 'WIDE', 'TALL', '???'];
  const SIZES = [
    [[8, 8], [16, 16], [32, 32], [64, 64]],
    [[16, 8], [32, 8], [32, 16], [64, 32]],
    [[8, 16], [8, 32], [16, 32], [32, 64]],
  ];
  const oam = emu.bus.oam;
  const rows: Array<{ i: number; visible: boolean; a0: number; a1: number; a2: number; w: number; h: number; x: number; y: number; tile: number; prio: number; mode: number; aff: boolean }> = [];
  for (let i = 0; i < 128; i++) {
    const o = i * 8;
    const a0 = oam[o] | (oam[o + 1] << 8);
    const a1 = oam[o + 2] | (oam[o + 3] << 8);
    const a2 = oam[o + 4] | (oam[o + 5] << 8);
    const shape = (a0 >> 14) & 3;
    const size = (a1 >> 14) & 3;
    const aff = (a0 & 0x100) !== 0;
    const disabled = !aff && (a0 & 0x200) !== 0;
    const [w, h] = shape < 3 ? SIZES[shape][size] : [0, 0];
    let y = a0 & 0xFF; if (y >= 160) y -= 256;
    let x = a1 & 0x1FF; if (x >= 240) x -= 512;
    rows.push({
      i,
      visible: !disabled && shape !== 3 && (x + w > 0) && (y + h > 0) && x < 240 && y < 160,
      a0, a1, a2, w, h, x, y,
      tile: a2 & 0x3FF,
      prio: (a2 >> 10) & 3,
      mode: (a0 >> 10) & 3,
      aff,
    });
  }
  const visibleCount = rows.filter((r) => r.visible).length;
  return (
    <div className="text-[11px]">
      <div className="text-[10px] opacity-60 mb-2">
        {visibleCount} / 128 OAM entries visible on screen
      </div>
      <div className="well text-[10px] font-mono overflow-y-auto max-h-[480px]">
        <table className="w-full">
          <thead className="sticky top-0 bg-[var(--color-sunken)] border-b border-[#1c1c20]">
            <tr className="text-left opacity-70">
              <th className="px-2 py-1">#</th>
              <th className="px-2 py-1">Pos</th>
              <th className="px-2 py-1">Size</th>
              <th className="px-2 py-1">Tile</th>
              <th className="px-2 py-1">Prio</th>
              <th className="px-2 py-1">Mode</th>
              <th className="px-2 py-1">A0/A1/A2</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.i} className={r.visible ? 'border-b border-[#1c1c20]' : 'border-b border-[#1c1c20] opacity-30'}>
                <td className="px-2 py-1">{r.i}</td>
                <td className="px-2 py-1">{r.visible ? `(${r.x},${r.y})` : '—'}</td>
                <td className="px-2 py-1">{r.w}×{r.h}{r.aff ? ' AFF' : ''}</td>
                <td className="px-2 py-1">{r.tile}</td>
                <td className="px-2 py-1">{r.prio}</td>
                <td className="px-2 py-1">{['NORM', 'SEMI', 'WIN', '???'][r.mode]}</td>
                <td className="px-2 py-1 opacity-60">{hex(r.a0, 4)} {hex(r.a1, 4)} {hex(r.a2, 4)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// Pretty-print the live state of the most informative IO registers.
function IoView({ emu }: { emu: Emulator }) {
  useLiveTick(10);
  const p = emu.ppu;
  const dispcnt = p.dispcnt;
  const mode = dispcnt & 7;
  const flag = (m: number, ch: string) => (dispcnt & m) ? ch : '·';
  return (
    <div className="text-[11px] font-mono space-y-3">
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-1">PPU</div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
          <div className="flex justify-between"><span className="opacity-50">DISPCNT</span><span>{hex(dispcnt, 4)}  mode={mode} {flag(0x100, '0')}{flag(0x200, '1')}{flag(0x400, '2')}{flag(0x800, '3')}{flag(0x1000, 'O')}{flag(0x2000, 'W')}{flag(0x4000, 'W')}{flag(0x8000, 'O')}</span></div>
          <div className="flex justify-between"><span className="opacity-50">DISPSTAT</span><span>{hex(p.dispstat, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">VCOUNT</span><span>{p.vcount}</span></div>
          <div className="flex justify-between"><span className="opacity-50">MOSAIC</span><span>{hex(p.mosaic, 4)}</span></div>
          {[0, 1, 2, 3].map((b) => (
            <div key={b} className="flex justify-between"><span className="opacity-50">BG{b}CNT</span><span>{hex(p.bgcnt[b], 4)} HOFS={p.bgHOFS[b]} VOFS={p.bgVOFS[b]}</span></div>
          ))}
          <div className="flex justify-between"><span className="opacity-50">WIN0</span><span>H={hex(p.win0H, 4)} V={hex(p.win0V, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">WIN1</span><span>H={hex(p.win1H, 4)} V={hex(p.win1V, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">WIN_IN</span><span>{hex(p.winIn, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">WIN_OUT</span><span>{hex(p.winOut, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">BLDCNT</span><span>{hex(p.bldcnt, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">BLDALPHA</span><span>{hex(p.bldalpha, 4)}</span></div>
        </div>
      </div>
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-1">DMA</div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
          {[0, 1, 2, 3].map((i) => {
            const c = emu.dma.ch[i];
            return (
              <div key={i} className="flex justify-between">
                <span className="opacity-50">DMA{i}</span>
                <span>{c.enabled ? `EN src=${hex(c.src)} dst=${hex(c.dst)} cnt=${c.count}` : 'off'}</span>
              </div>
            );
          })}
        </div>
      </div>
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-1">Timers</div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
          {[0, 1, 2, 3].map((i) => {
            const t = emu.timers.ch[i];
            return (
              <div key={i} className="flex justify-between">
                <span className="opacity-50">TM{i}</span>
                <span>{t.enabled ? `EN reload=${hex(t.reload, 4)} cnt=${hex(t.counter, 4)} /${t.prescale}` : 'off'}</span>
              </div>
            );
          })}
        </div>
      </div>
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-1">IRQ</div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
          <div className="flex justify-between"><span className="opacity-50">IME</span><span>{emu.irq.ime}</span></div>
          <div className="flex justify-between"><span className="opacity-50">IE</span><span>{hex(emu.irq.ie, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">IF</span><span>{hex(emu.irq.iflag, 4)}</span></div>
        </div>
      </div>
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-1">Sound</div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
          <div className="flex justify-between"><span className="opacity-50">SOUNDCNT_X</span><span>{hex(emu.sound.soundcntX, 4)} {(emu.sound.soundcntX & 0x80) ? 'EN' : 'off'}</span></div>
          <div className="flex justify-between"><span className="opacity-50">SOUNDCNT_H</span><span>{hex(emu.sound.soundcntH, 4)}</span></div>
          <div className="flex justify-between"><span className="opacity-50">FIFO A</span><span>{emu.sound.countA}/32</span></div>
          <div className="flex justify-between"><span className="opacity-50">FIFO B</span><span>{emu.sound.countB}/32</span></div>
          <div className="flex justify-between"><span className="opacity-50">Rate</span><span>{emu.sound.sampleRate.toFixed(0)} Hz</span></div>
        </div>
      </div>
    </div>
  );
}

// Experimental/advanced settings — currently just the THUMB recompiler
// (JIT) toggle. The JIT translates hot THUMB basic blocks to WASM; it
// can give a large speedup on register-heavy code but still falls back
// to the interpreter for unsupported instructions and for ARM blocks.
// Toggling reruns hot detection from scratch (the block cache is dropped).
function AdvancedView({ emu }: { emu: Emulator }) {
  useLiveTick(4);
  const r = emu.recomp;
  const total = r.intInsns + r.jitInsns;
  const pct = total === 0 ? 0 : (r.jitInsns * 100) / total;
  // useState mirrors enabled flag to drive re-renders on toggle.
  const [, setBump] = useState(0);
  const toggle = () => {
    r.enabled = !r.enabled;
    if (!r.enabled) r.invalidate();
    setBump((b) => b + 1);
  };
  return (
    <div className="text-[11px] space-y-4">
      <div>
        <div className="text-[10px] uppercase tracking-widest opacity-50 mb-2">Recompiler (THUMB JIT)</div>
        <label className="flex items-center gap-3 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={r.enabled}
            onChange={toggle}
            className="w-4 h-4 accent-[#5060a0]"
          />
          <span>Enable JIT</span>
          <span className="opacity-50 text-[10px]">on by default — uncheck to debug with the pure interpreter</span>
        </label>
        <div className="mt-3 grid grid-cols-2 gap-x-4 gap-y-0.5 font-mono">
          <div className="flex justify-between"><span className="opacity-50">Interpreted insns</span><span>{r.intInsns.toLocaleString()}</span></div>
          <div className="flex justify-between"><span className="opacity-50">JIT insns</span><span>{r.jitInsns.toLocaleString()}</span></div>
          <div className="flex justify-between"><span className="opacity-50">JIT share</span><span>{pct.toFixed(1)}%</span></div>
          <div className="flex justify-between"><span className="opacity-50">Cached blocks</span><span>{r.cache.size}</span></div>
          <div className="flex justify-between"><span className="opacity-50">Profiling</span><span>{r.hits.size} hot PCs</span></div>
        </div>
        <button
          onClick={() => { r.invalidate(); setBump((b) => b + 1); }}
          className="btn-default mt-3 !text-[10px]"
          disabled={r.cache.size === 0 && r.hits.size === 0}
        >Clear JIT cache</button>
      </div>
      <div className="text-[10px] opacity-50 leading-relaxed">
        The JIT compiles hot basic blocks to WebAssembly: every THUMB
        format (1-16, 18, 19) except SWI, and nearly all of ARM:
        data-processing (all operand forms, conditional, PC-relative),
        branches (B/BL/BX), loads/stores (LDR/STR, LDRH/STRH/LDRSB/LDRSH),
        block transfer (LDM/STM), and multiply (incl. 64-bit). Only the
        registers a block touches are synced across the dispatch
        boundary. SWI, MRS/MSR and SWP still interpret. If a game
        misbehaves, turn this off and reload.
      </div>
    </div>
  );
}
