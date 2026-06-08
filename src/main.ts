import { Emulator } from './emulator';
import { CanvasView } from './ui/canvas';
import { bindKeys } from './ui/input';

const log = (...args: unknown[]) => {
  const el = document.getElementById('log');
  if (!el) return;
  el.textContent += args.map(String).join(' ') + '\n';
  el.scrollTop = el.scrollHeight;
};

async function main() {
  log('Booting GBA WASM recompiler…');
  const canvas = document.getElementById('screen') as HTMLCanvasElement;
  const view = new CanvasView(canvas);
  const stats = document.getElementById('stats') as HTMLElement;

  const emu = new Emulator();
  bindKeys(emu.keypad);

  const romSelect = document.getElementById('rom') as HTMLSelectElement;
  let romPath = romSelect.value;
  log(`Fetching ${romPath}…`);
  let resp = await fetch(romPath);
  if (!resp.ok) throw new Error(`Failed to fetch ROM: ${resp.status}`);
  let buf = new Uint8Array(await resp.arrayBuffer());
  log(`Loaded ROM, ${buf.length} bytes`);
  romSelect.addEventListener('change', async () => {
    romPath = romSelect.value;
    log(`Switching to ${romPath}…`);
    const r = await fetch(romPath);
    buf = new Uint8Array(await r.arrayBuffer());
    emu.loadRom(buf);
    log(`Loaded ${buf.length} bytes; reset.`);
  });

  // Sanity check: read header.
  const title = new TextDecoder('ascii').decode(buf.subarray(0xA0, 0xAC)).replace(/\0/g, '');
  const gameCode = new TextDecoder('ascii').decode(buf.subarray(0xAC, 0xB0));
  log(`Header: title="${title}" code="${gameCode}"`);

  emu.loadRom(buf);

  // Wire RTC into ROM region reads at 0x080000C4/C6/C8 — the cart's GPIO.
  // We intercept by patching the bus's read16/read8 around these addresses.
  const origRead16 = emu.bus.read16.bind(emu.bus);
  const origWrite16 = emu.bus.write16.bind(emu.bus);
  const origRead8 = emu.bus.read8.bind(emu.bus);
  const origWrite8 = emu.bus.write8.bind(emu.bus);
  emu.bus.read16 = (addr: number) => {
    if ((addr & 0xFFFFFFF8) === 0x080000C0) {
      const off = addr & 0xFE;
      if (off === 0xC4 || off === 0xC6 || off === 0xC8) return emu.rtc.read(off);
    }
    return origRead16(addr);
  };
  emu.bus.write16 = (addr: number, v: number) => {
    if ((addr & 0xFFFFFFF8) === 0x080000C0) {
      const off = addr & 0xFE;
      if (off === 0xC4 || off === 0xC6 || off === 0xC8) { emu.rtc.write(off, v); return; }
    }
    origWrite16(addr, v);
  };
  emu.bus.read8 = (addr: number) => {
    if ((addr & 0xFFFFFFF8) === 0x080000C0) {
      const off = addr & 0xFF;
      if (off === 0xC4 || off === 0xC6 || off === 0xC8) return emu.rtc.read(off);
    }
    return origRead8(addr);
  };
  emu.bus.write8 = (addr: number, v: number) => {
    if ((addr & 0xFFFFFFF8) === 0x080000C0) {
      const off = addr & 0xFF;
      if (off === 0xC4 || off === 0xC6 || off === 0xC8) { emu.rtc.write(off, v); return; }
    }
    origWrite8(addr, v);
  };

  let paused = false;
  document.getElementById('pause')?.addEventListener('click', () => {
    paused = !paused;
    (document.getElementById('pause') as HTMLButtonElement).textContent = paused ? 'Resume' : 'Pause';
  });
  document.getElementById('reset')?.addEventListener('click', () => {
    log('Reset');
    emu.loadRom(buf);
  });

  let lastTs = performance.now();
  let fpsAvg = 0;
  let frameCounter = 0;

  function loop(ts: number) {
    requestAnimationFrame(loop);
    if (paused) return;
    try {
      const r = emu.runFrame();
      view.blit(emu.ppu.frame);

      const dt = ts - lastTs;
      lastTs = ts;
      const inst = (1000 / dt);
      fpsAvg = fpsAvg ? fpsAvg * 0.9 + inst * 0.1 : inst;
      frameCounter++;
      if (frameCounter % 30 === 0) {
        const total = r.interp + r.jit || 1;
        stats.textContent = `${fpsAvg.toFixed(1)} fps · ${(280896 * fpsAvg / 1e6).toFixed(2)} Mhz · int ${((r.interp / total) * 100 | 0)}% · jit ${((r.jit / total) * 100 | 0)}%`;
      }
    } catch (e) {
      paused = true;
      log('ERROR', (e as Error).message);
      throw e;
    }
  }
  requestAnimationFrame(loop);
  log('Emulator running.');
}

main().catch((e) => {
  console.error(e);
  log('Failed to start:', (e as Error).message);
});
