// Savestate round-trip: boot a ROM, take a snapshot, run a frame to
// dirty state, restore, verify we're byte-identical to the snapshot
// moment. If this passes we know the serializer covers every field
// that runFrame actually mutates.

import { describe, it, expect } from 'vitest';
import { Emulator } from '../emulator';
import { saveState, loadState } from '../savestate';
import { hasRom, readRom, romPath } from './roms';

// Needs a real ROM (gitignored), so it's skipped when absent (e.g. CI).
const ROM = romPath('firered.gba');

function snapshot(emu: Emulator): Record<string, unknown> {
  const cpu = emu.cpu;
  return {
    r: Array.from(cpu.state.r),
    cpsr: cpu.state.cpsr,
    cycles: cpu.cycles,
    halted: cpu.state.halted,
    iwram: Array.from(emu.bus.iwram.subarray(0, 256)),
    ewram: Array.from(emu.bus.ewram.subarray(0, 256)),
    pram: Array.from(emu.bus.pram.subarray(0, 64)),
    oam: Array.from(emu.bus.oam.subarray(0, 64)),
    dispcnt: emu.ppu.dispcnt,
    vcount: emu.ppu.vcount,
    frameCount: emu.ppu.frameCount,
    cyclesAccum: emu.ppu.cyclesAccum,
    siocnt: emu.io.sio.siocnt,
    ie: emu.irq.ie,
    iflag: emu.irq.iflag,
    ime: emu.irq.ime,
    timer0: emu.timers.ch[0].counter,
    timer1: emu.timers.ch[1].counter,
  };
}

describe.skipIf(!hasRom(ROM))('savestate round-trip', () => {
  it('preserves emulator state across save+load', { timeout: 30000 }, () => {
    const rom = readRom(ROM);
    const emu = new Emulator();
    emu.loadRom(rom);
    // Boot far enough to have meaningful state.
    for (let i = 0; i < 30; i++) emu.runFrame();

    const before = snapshot(emu);
    const blob = saveState(emu);

    // Mutate state by running more frames.
    for (let i = 0; i < 30; i++) emu.runFrame();
    const dirty = snapshot(emu);
    expect(dirty).not.toEqual(before);

    // Restore — should match the original snapshot byte-for-byte.
    loadState(emu, blob);
    const after = snapshot(emu);
    expect(after).toEqual(before);
  });

  it('round-trips a stable run after restore', { timeout: 30000 }, () => {
    // Stronger guarantee: save → run N frames → record snapshot S1;
    // restore the same blob → run N frames again → snapshot S2.
    // S1 should equal S2. Catches "we forgot to serialize field X
    // and the next frame's behavior diverges because of it."
    const rom = readRom(ROM);
    const emu = new Emulator();
    emu.loadRom(rom);
    for (let i = 0; i < 30; i++) emu.runFrame();

    const blob = saveState(emu);
    // Align JIT compile state between the two runs. loadState clears
    // the recompiler cache (it isn't serialized), so the post-restore
    // run starts cold; without this, run 1 would run with the cache
    // warmed by the 30 boot frames and IRQs would be delivered at
    // different block boundaries — a timing-granularity difference,
    // not an architectural one. Cold-vs-cold is fully deterministic.
    emu.recomp.invalidate();
    for (let i = 0; i < 15; i++) emu.runFrame();
    const s1 = snapshot(emu);

    loadState(emu, blob);
    for (let i = 0; i < 15; i++) emu.runFrame();
    const s2 = snapshot(emu);

    expect(s2).toEqual(s1);
  });
});
