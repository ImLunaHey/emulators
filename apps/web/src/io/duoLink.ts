// Shared dual-core link step: advances two GBA cores through one visual frame
// in fine-grained lockstep over a synchronous in-memory cable. Core `a` is the
// link master, `b` the slave. We interleave both cores in small cycle slices so
// the slave services each multiplay transfer's IRQ between the master's sends —
// i.e. it receives the per-frame *burst* of transfers its link watchdog expects
// instead of one-per-frame (which makes Gen-3 throw "link error").
//
// This is the kernel proven by the local duo view and reused by lockstep
// netplay: on every machine the cable stays local and synchronous, and only
// controller inputs cross the network (see io/netplay.ts).

// The slice of the core surface this needs. WasmEmulator satisfies it
// structurally, so callers pass their core directly (no io→ui import).
export interface DualLinkCore {
  runSlice(maxCycles: number): number; // 0 = more to run, 1 = frame done, 2 = paused on link
  linkTakeOutgoing(): number;
  linkPeekOutgoing(): number;
  linkDeliver(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
  linkApplyRemote(m0: number, m1: number, m2: number, m3: number, error: boolean): void;
}

// Cycles per interleave slice. Small enough that the slave gets a slice to
// service each transfer's SIO IRQ between the master's sends; large enough to
// keep the call count modest (~140 slices/core per frame when nothing pends).
const SLICE_CYCLES = 2048;
// Hard stop so a pathological pend-storm can't wedge the caller's frame.
const MAX_STEPS_PER_FRAME = 40000;

// Run both cores through exactly one visual frame. `onTransfer(master, slave)`
// (optional) is called for each resolved Multiplay word-pair, for diagnostics.
export function stepDualFrame(
  a: DualLinkCore,
  b: DualLinkCore,
  onTransfer?: (master: number, slave: number) => void,
): void {
  let aDone = false;
  let bDone = false;
  let steps = 0;
  while (!(aDone && bDone) && steps++ < MAX_STEPS_PER_FRAME) {
    if (!aDone) {
      const st = a.runSlice(SLICE_CYCLES);
      if (st === 1) aDone = true;
      else if (st === 2) {
        const masterRaw = a.linkTakeOutgoing();
        if (masterRaw >= 0) {
          const master = masterRaw & 0xffff;
          const slave = b.linkPeekOutgoing() & 0xffff;
          a.linkDeliver(master, slave, 0xffff, 0xffff, false);
          b.linkApplyRemote(master, slave, 0xffff, 0xffff, false);
          onTransfer?.(master, slave);
        }
      }
    }
    if (!bDone) {
      // Slave never starts its own transfer → run_slice never returns 2.
      if (b.runSlice(SLICE_CYCLES) === 1) bDone = true;
    }
  }
}
