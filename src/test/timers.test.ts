// Timer + IRQ-on-overflow + count-up cascade tests.

import { describe, it, expect } from 'vitest';
import { Timers } from '../io/timers';
import { Irq } from '../io/irq';

describe('Timer: prescaler', () => {
  it('prescale=1 ticks every CPU cycle', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0);
    t.writeControl(0, 0x80);  // enable, prescale 1
    t.step(100);
    expect(t.readCounter(0)).toBe(100);
  });
  it('prescale=64 ticks once per 64 cycles', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0);
    t.writeControl(0, 0x81);  // enable, prescale 64
    t.step(128);
    expect(t.readCounter(0)).toBe(2);
  });
  it('prescale=1024', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0);
    t.writeControl(0, 0x83);  // enable, prescale 1024
    t.step(3072);
    expect(t.readCounter(0)).toBe(3);
  });
});

describe('Timer: overflow + reload', () => {
  it('overflow restores reload value', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0xFFFD);
    t.writeControl(0, 0x80);  // enable, prescale 1
    t.step(5);
    // counter starts at reload (0xFFFD) on enable; then 5 ticks advance:
    // 0xFFFD → 0xFFFE (tick 1) → 0xFFFF (tick 2) → 0x0000 → reload 0xFFFD
    // → 0xFFFE (tick 4) → 0xFFFF (tick 5)
    expect(t.readCounter(0)).toBe(0xFFFF);
  });
  it('overflow raises IRQ when irqEnable set', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0xFFFF);
    t.writeControl(0, 0xC0);  // enable, IRQ
    t.step(1);
    // 0xFFFF → 0x0000 (overflow) → IRQ.
    expect((irq.iflag & (1 << 3)) !== 0).toBe(true);
  });
  it('overflow does NOT raise IRQ when irqEnable clear', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0xFFFF);
    t.writeControl(0, 0x80);  // enable, NO IRQ
    t.step(1);
    expect(irq.iflag).toBe(0);
  });
});

describe('Timer: count-up cascade', () => {
  it('Timer 1 in count-up ticks on Timer 0 overflow', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0xFFFF);
    t.writeControl(0, 0x80);  // T0: enable, prescale 1
    t.writeReload(1, 0);
    t.writeControl(1, 0x84);  // T1: enable, count-up
    // T0 overflows every cycle starting from 0xFFFF.
    t.step(3);
    expect(t.readCounter(1)).toBe(3);
  });
  it('Timer 2 cascade from Timer 1 overflow', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0xFFFF);
    t.writeControl(0, 0x80);
    t.writeReload(1, 0xFFFF);
    t.writeControl(1, 0x84);
    t.writeReload(2, 0);
    t.writeControl(2, 0x84);
    t.step(1);
    // T0 overflows → T1 ticks → T1 overflows → T2 ticks.
    expect(t.readCounter(2)).toBe(1);
  });
});

describe('Timer: enable starts from reload', () => {
  it('writing control with enable bit reloads counter from reload value', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0x1234);
    expect(t.readCounter(0)).toBe(0);  // not yet enabled
    t.writeControl(0, 0x80);
    expect(t.readCounter(0)).toBe(0x1234);
  });
  it('writing control without flipping enable does NOT reload', () => {
    const irq = new Irq();
    const t = new Timers(irq);
    t.writeReload(0, 0x1234);
    t.writeControl(0, 0x80);
    t.step(10);
    expect(t.readCounter(0)).toBe(0x1234 + 10);
    // Write control again with same enable bit → counter must not reset.
    t.writeReload(0, 0x9999);
    t.writeControl(0, 0x80);
    expect(t.readCounter(0)).toBe(0x1234 + 10);
  });
});
