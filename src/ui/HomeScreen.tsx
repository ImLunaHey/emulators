import { useEffect, useRef, useState } from 'react';
import { WasmHome } from '../../core/pkg/gba_core.js';
import { useEmu } from './EmuContext';
import { listRoms, deleteRom, type RomMeta } from './romStore';
import { ingestFiles } from './romIngest';
import { systemLabel, isPlayable, ACCEPT } from './systems';
import { usePersistedBool } from './usePersistedState';
import { useToast } from './Toast';

// The console home launcher — the home screen is *rendered by the Rust core*
// (core/src/home.rs → WasmHome). This component is the thin host shell: it
// blits the launcher's framebuffer to a canvas, feeds it input, owns storage
// (the IndexedDB game list + the add-game file dialog), and reports the chosen
// game back up so App can swap to the player.

// Active-high, GBA-layout button bits the Rust home screen reads.
const K_A = 1 << 0;
const K_B = 1 << 1;
const K_SELECT = 1 << 2;
const K_RIGHT = 1 << 4;
const K_LEFT = 1 << 5;
const K_UP = 1 << 6;
const K_DOWN = 1 << 7;

export function HomeScreen({ onPlay }: { onPlay: (romId: string, system: string) => void }) {
  const { emu } = useEmu();
  const toast = useToast();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const homeRef = useRef<WasmHome | null>(null);
  const keysRef = useRef(0);
  const romsRef = useRef<RomMeta[]>([]);
  const [ready, setReady] = useState(false);
  const [crisp, setCrisp] = usePersistedBool('settings:crispPixels', true);

  // Route a launcher "play:<id>" to App with the game's system so it picks the
  // right core (GBA vs NDS).
  const play = (id: string) => onPlay(id, romsRef.current.find((r) => r.id === id)?.system ?? 'gba');

  // Push the installed-game list into the launcher (id + display title).
  const refresh = async () => {
    const roms = await listRoms();
    romsRef.current = roms;
    const home = homeRef.current;
    if (!home) return;
    // Push embedded icons (NDS banners) first — they're keyed by id and read
    // during the set_games render.
    for (const r of roms) {
      if (r.icon && r.icon.length === 32 * 32 * 4) home.set_icon(r.id, r.icon);
    }
    const ids = roms.map((r) => r.id).join('\n');
    const titles = roms.map((r) => r.title || r.filename || 'Untitled').join('\n');
    const systems = roms.map((r) => systemLabel(r.system)).join('\n');
    const playables = roms.map((r) => (isPlayable(r.system) ? '1' : '0')).join('\n');
    home.set_games(ids, titles, systems, playables);
  };

  // Keyboard → button mask.
  useEffect(() => {
    const map: Record<string, number> = {
      ArrowRight: K_RIGHT, ArrowLeft: K_LEFT, ArrowUp: K_UP, ArrowDown: K_DOWN,
      Enter: K_A, ' ': K_A, z: K_A, Z: K_A,
      Tab: K_SELECT, x: K_B, X: K_B,
    };
    const down = (e: KeyboardEvent) => { const b = map[e.key]; if (b) { e.preventDefault(); keysRef.current |= b; } };
    const up = (e: KeyboardEvent) => { const b = map[e.key]; if (b) keysRef.current &= ~b; };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => { window.removeEventListener('keydown', down); window.removeEventListener('keyup', up); };
  }, []);

  // Build the launcher once wasm is ready, then run the render/input loop.
  useEffect(() => {
    let raf = 0;
    let alive = true;
    emu.ready.then(async () => {
      if (!alive) return;
      const home = new WasmHome();
      homeRef.current = home;
      home.set_crisp(crisp);
      await refresh();
      setReady(true);

      const canvas = canvasRef.current!;
      const w = home.width();
      const h = home.height();
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext('2d')!;

      const loop = () => {
        if (!alive) return;
        const mask = keysRef.current | readGamepad();
        const action = home.run_frame(mask);
        ctx.putImageData(new ImageData(new Uint8ClampedArray(home.framebuffer()), w, h), 0, 0);
        if (action.startsWith('play:')) {
          play(action.slice(5));
          return; // stop the loop; App unmounts us
        }
        handleAction(action);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    });
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
      homeRef.current?.free();
      homeRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onAddFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    const res = await ingestFiles(files);
    if (res.added) toast.success(`Added ${res.added} game${res.added === 1 ? '' : 's'}`);
    if (res.failed) toast.error(`${res.failed} import${res.failed === 1 ? '' : 's'} failed`);
    if (!res.added && !res.failed) toast.error('No .gba ROMs found in selection');
    await refresh();
  };

  // Tap a tile on the canvas → select + launch (or open the + dialog).
  const onCanvasPointer = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const home = homeRef.current;
    const canvas = canvasRef.current;
    if (!home || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = Math.floor(((e.clientX - rect.left) / rect.width) * home.width());
    const y = Math.floor(((e.clientY - rect.top) / rect.height) * home.height());
    const action = home.pointer(x, y);
    if (action.startsWith('play:')) play(action.slice(5));
    else handleAction(action);
  };

  // Mouse wheel scrolls the game grid.
  const onCanvasWheel = (e: React.WheelEvent<HTMLCanvasElement>) => {
    const home = homeRef.current;
    const canvas = canvasRef.current;
    if (!home || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    home.scroll_by(Math.round((e.deltaY / rect.height) * home.height()));
  };

  // Non-play launcher actions (add / coming-soon toast / Rust settings).
  const handleAction = (action: string) => {
    if (action === 'add') fileRef.current?.click();
    else if (action.startsWith('soon:')) toast.info(`${action.slice(5)} — coming soon`);
    else if (action === 'crisp:1') setCrisp(true);
    else if (action === 'crisp:0') setCrisp(false);
    else if (action === 'clearall') clearAllGames();
  };

  const clearAllGames = async () => {
    const roms = await listRoms();
    for (const r of roms) await deleteRom(r.id);
    await refresh();
    toast.info(`Removed ${roms.length} game${roms.length === 1 ? '' : 's'}`);
  };

  // On-screen control: hold a button to set its bit (Rust edge-detects it).
  const hold = (bit: number) => ({
    onPointerDown: (e: React.PointerEvent) => { e.preventDefault(); keysRef.current |= bit; },
    onPointerUp: () => { keysRef.current &= ~bit; },
    onPointerLeave: () => { keysRef.current &= ~bit; },
    onPointerCancel: () => { keysRef.current &= ~bit; },
  });

  const dpadBtn = 'w-11 h-11 grid place-items-center rounded-lg bg-[var(--color-accent-deep)] text-[var(--color-accent)] text-lg select-none active:brightness-150';

  return (
    <div
      className="relative w-full min-h-screen flex flex-col items-center justify-center gap-4 p-4"
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => { e.preventDefault(); onAddFiles(e.dataTransfer.files); }}
    >
      <canvas
        ref={canvasRef}
        onPointerDown={onCanvasPointer}
        onWheel={onCanvasWheel}
        className="w-full max-w-[960px] aspect-[3/2] rounded-lg shadow-lg cursor-pointer touch-none"
        style={{ imageRendering: crisp ? 'pixelated' : 'auto' }}
      />
      {!ready && <div className="absolute text-xs opacity-50">loading…</div>}

      {/* On-screen controls (touch / no-keyboard). */}
      <div className="flex items-center gap-6 select-none">
        <div className="grid grid-cols-3 grid-rows-3 gap-1 w-[140px]">
          <span />
          <button aria-label="Up" className={dpadBtn} {...hold(K_UP)}>▲</button>
          <span />
          <button aria-label="Left" className={dpadBtn} {...hold(K_LEFT)}>◀</button>
          <span />
          <button aria-label="Right" className={dpadBtn} {...hold(K_RIGHT)}>▶</button>
          <span />
          <button aria-label="Down" className={dpadBtn} {...hold(K_DOWN)}>▼</button>
          <span />
        </div>
        <div className="flex flex-col gap-2">
          <button className="btn btn-primary !px-5" {...hold(K_A)}>Open</button>
          <button className="btn !px-5" onClick={() => fileRef.current?.click()}>+ Add</button>
        </div>
      </div>

      <input
        ref={fileRef}
        type="file"
        accept={ACCEPT}
        multiple
        className="hidden"
        onChange={(e) => { onAddFiles(e.target.files); e.target.value = ''; }}
      />
    </div>
  );
}

// Standard-mapping gamepad → the same button mask (d-pad + A).
function readGamepad(): number {
  const pads = navigator.getGamepads?.() ?? [];
  let mask = 0;
  for (const p of pads) {
    if (!p) continue;
    const b = p.buttons;
    if (b[12]?.pressed) mask |= K_UP;
    if (b[13]?.pressed) mask |= K_DOWN;
    if (b[14]?.pressed) mask |= K_LEFT;
    if (b[15]?.pressed) mask |= K_RIGHT;
    if (b[0]?.pressed) mask |= K_A;
    const ax = p.axes;
    if (ax[0] < -0.5) mask |= K_LEFT;
    if (ax[0] > 0.5) mask |= K_RIGHT;
    if (ax[1] < -0.5) mask |= K_UP;
    if (ax[1] > 0.5) mask |= K_DOWN;
  }
  return mask;
}
