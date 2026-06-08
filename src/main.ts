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

      // Overlay live boot diagnostics on the canvas. When the game hasn't
      // yet written content to VRAM/OAM, show a more visible splash panel.
      const s = emu.cpu.state;
      const pc = s.r[15].toString(16).padStart(8, '0');
      const mode = (s.cpsr & 0x20) ? 'THUMB' : 'ARM';
      const dispcnt = emu.ppu.dispcnt.toString(16).padStart(4, '0');
      const dispstat = emu.ppu.dispstat.toString(16).padStart(4, '0');
      const ie = emu.irq.ie.toString(16).padStart(4, '0');
      const iflag = emu.irq.iflag.toString(16).padStart(4, '0');
      const total = r.interp + r.jit || 1;
      const jitPct = ((r.jit / total) * 100) | 0;
      const vramFill = nonZero(emu.bus.vram);
      const showSplash = vramFill === 0;
      view.overlay([
        `frame ${frameCounter.toString().padStart(5)}   ${fpsAvg.toFixed(1)} fps   ${(280896 * fpsAvg / 1e6).toFixed(2)} MHz`,
        `pc ${pc}  ${mode}  jit ${jitPct}%`,
        `cycles/frame ${(r.interp + r.jit).toString().padStart(6)}   ${(r.jit).toString().padStart(5)} jit`,
        `dispcnt ${dispcnt}  dispstat ${dispstat}`,
        `ie ${ie}  if ${iflag}  ime ${emu.irq.ime}`,
        `vram ${vramFill}  oam ${nonZero(emu.bus.oam)}  pram ${nonZero(emu.bus.pram)}`,
        showSplash
          ? `state: game in init wait loop`
          : `state: game running`,
      ], showSplash);

      if (frameCounter % 30 === 0) {
        stats.textContent = `${fpsAvg.toFixed(1)} fps · ${(280896 * fpsAvg / 1e6).toFixed(2)} Mhz · int ${((r.interp / total) * 100 | 0)}% · jit ${jitPct}%`;
      }
    } catch (e) {
      paused = true;
      log('ERROR', (e as Error).message);
      throw e;
    }
  }

  function nonZero(a: Uint8Array): number {
    let n = 0;
    for (let i = 0; i < a.length; i++) if (a[i]) n++;
    return n;
  }
  requestAnimationFrame(loop);
  log('Emulator running.');
}

main().catch((e) => {
  console.error(e);
  log('Failed to start:', (e as Error).message);
});
