import { IRQ_TIMER0 } from './irq';
// Four 16-bit timers. Each has:
//   reload (TMxCNT_L on write), counter (TMxCNT_L on read), control bits
// Control TMxCNT_H bits:
//   0..1 prescaler (1, 64, 256, 1024)
//   2    count-up timing (chained to previous timer's overflow)
//   6    IRQ enable
//   7    start
const PRESCALES = [1, 64, 256, 1024];
export class TimerChannel {
    reload = 0;
    counter = 0;
    control = 0;
    // Cycles accumulated toward next tick (only used when not in count-up).
    subCycles = 0;
    enabled = false;
    countUp = false;
    irqEnable = false;
    prescale = 1;
}
export class Timers {
    irq;
    ch = [new TimerChannel(), new TimerChannel(), new TimerChannel(), new TimerChannel()];
    // Set lazily by Emulator after construction (Sound depends on DMA
    // which is also wired up via the constructor chain, so we can't get
    // it via parameter without a refactor).
    sound = null;
    constructor(irq) {
        this.irq = irq;
    }
    writeReload(i, v) { this.ch[i].reload = v & 0xFFFF; }
    readCounter(i) { return this.ch[i].counter & 0xFFFF; }
    readControl(i) { return this.ch[i].control; }
    writeControl(i, v) {
        const c = this.ch[i];
        const wasEnabled = c.enabled;
        c.control = v & 0xFFFF;
        c.prescale = PRESCALES[v & 3];
        c.countUp = i > 0 && (v & 0x04) !== 0;
        c.irqEnable = (v & 0x40) !== 0;
        c.enabled = (v & 0x80) !== 0;
        if (!wasEnabled && c.enabled) {
            c.counter = c.reload;
            c.subCycles = 0;
        }
    }
    // Step all timers by `cycles` CPU cycles.
    step(cycles) {
        for (let i = 0; i < 4; i++) {
            const c = this.ch[i];
            if (!c.enabled || c.countUp)
                continue;
            c.subCycles += cycles;
            while (c.subCycles >= c.prescale) {
                c.subCycles -= c.prescale;
                c.counter = (c.counter + 1) & 0xFFFF;
                if (c.counter === 0)
                    this.overflow(i);
            }
        }
    }
    overflow(i) {
        const c = this.ch[i];
        c.counter = c.reload;
        if (c.irqEnable)
            this.irq.raise(IRQ_TIMER0 << i);
        // Direct Sound A/B are driven by Timer 0 or Timer 1 overflow.
        if (this.sound && (i === 0 || i === 1)) {
            this.sound.onTimerOverflow(i);
        }
        // Cascade to next channel if it is count-up.
        if (i < 3) {
            const next = this.ch[i + 1];
            if (next.enabled && next.countUp) {
                next.counter = (next.counter + 1) & 0xFFFF;
                if (next.counter === 0)
                    this.overflow(i + 1);
            }
        }
    }
}
