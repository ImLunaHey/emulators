// Seiko S-3511A RTC sitting on GPIO at ROM 0x080000C4 / 0xC6 / 0xC8.
// FireRed reads the date/time for berry growth and Pokemon time-based
// events. We bit-bang a minimal subset of the SIO protocol that the
// official RTC library uses: reset, status read, datetime read.

// Pin meanings (from GPIO_DATA register at 0xC4):
//   bit 0 = SCK (clock, in)
//   bit 1 = SIO (data, bidirectional)
//   bit 2 = CS  (chip select, in)

export class Rtc {
  enabled = false;
  selected = false;        // CS high
  clk = 0;
  data = 0;
  dir = 0;                 // GPIO_DIR — bits set = pin is OUT (CPU writes)
  state: 'idle' | 'cmd' | 'reply' | 'recv' = 'idle';
  buffer = 0;
  bits = 0;
  cmd = 0;
  payload: number[] = [];
  cursor = 0;
  // Status byte: bit 6 = 24h mode (matches stock setup).
  status = 0x40;

  read(off: number): number {
    // 0xC4 = data, 0xC6 = dir, 0xC8 = enable.
    switch (off) {
      case 0xC4: {
        if (!this.enabled) return 0;
        const sck = this.clk & 1;
        const sio = this.data & 1;
        const cs  = this.selected ? 1 : 0;
        // Only return bits where the host pin is configured as INPUT.
        const inMask = (~this.dir) & 0x7;
        return ((sck | (sio << 1) | (cs << 2)) & inMask) >>> 0;
      }
      case 0xC6: return this.dir;
      case 0xC8: return this.enabled ? 1 : 0;
    }
    return 0;
  }

  write(off: number, v: number): void {
    switch (off) {
      case 0xC4: {
        if (!this.enabled) return;
        const newCs  = (v >> 2) & 1;
        const newSck = v & 1;
        const newSio = (v >> 1) & 1;

        if (!this.selected && newCs) {
          this.state = 'cmd';
          this.bits = 0;
          this.buffer = 0;
        } else if (this.selected && !newCs) {
          this.state = 'idle';
        }
        this.selected = newCs === 1;

        // Rising edge of SCK while CS high → clock in/out a bit.
        const sckRising = !this.clk && newSck;
        const sckFalling = !!this.clk && !newSck;
        this.clk = newSck;

        if (this.selected) {
          if (this.state === 'cmd' && sckFalling) {
            // Host writes command MSB-first.
            this.buffer = ((this.buffer << 1) | newSio) & 0xFF;
            this.bits++;
            if (this.bits === 8) {
              this.beginCommand(this.buffer);
            }
          } else if (this.state === 'reply' && sckRising) {
            // Device returns the current byte LSB-first across 8 clocks.
            const byte = this.payload[this.cursor] ?? 0;
            this.data = (byte >> this.bits) & 1;
            this.bits++;
            if (this.bits === 8) {
              this.bits = 0;
              this.cursor++;
              if (this.cursor >= this.payload.length) this.state = 'idle';
            }
          } else if (this.state === 'recv' && sckFalling) {
            // Host writes one byte LSB-first.
            this.buffer = (this.buffer | (newSio << this.bits)) & 0xFF;
            this.bits++;
            if (this.bits === 8) {
              this.payload.push(this.buffer);
              this.buffer = 0;
              this.bits = 0;
              this.cursor++;
              if (this.cursor >= this.payloadLen) this.finishWrite();
            }
          }
        }
        // SIO is also an open-line we mirror back.
        this.data = newSio;
        return;
      }
      case 0xC6: this.dir = v & 0x7; return;
      case 0xC8: this.enabled = (v & 1) === 1; return;
    }
  }

  payloadLen = 0;

  private beginCommand(cmd: number) {
    this.cmd = cmd;
    this.cursor = 0;
    this.bits = 0;
    this.payload = [];

    // S-3511A command byte: bits 7..4 = 0110, bit 3..1 = reg, bit 0 = R/W (1 = read).
    const reg = (cmd >> 1) & 0x7;
    const reading = (cmd & 1) === 1;

    switch (reg) {
      case 0: // Reset / force.
        this.status = 0x40;
        this.state = 'idle';
        return;
      case 1: // Status.
        if (reading) { this.payload = [this.status]; this.state = 'reply'; }
        else         { this.payloadLen = 1; this.state = 'recv'; }
        return;
      case 2: // Date/time (7 bytes BCD).
        if (reading) { this.payload = this.dateTimeBcd(); this.state = 'reply'; }
        else         { this.payloadLen = 7; this.state = 'recv'; }
        return;
      case 3: // Time only (3 bytes).
        if (reading) { this.payload = this.dateTimeBcd().slice(4); this.state = 'reply'; }
        else         { this.payloadLen = 3; this.state = 'recv'; }
        return;
      default:
        this.state = 'idle';
    }
  }

  private finishWrite() {
    // Status writes: store the byte so subsequent reads match. Pokemon
    // Ruby/Sapphire/Emerald write status then read it back; mismatch is
    // reported as "battery has run dry".
    const reg = (this.cmd >> 1) & 0x7;
    if (reg === 1 && this.payload.length >= 1) {
      this.status = this.payload[0];
    }
    // Date/time writes — host wallclock stays authoritative.
    this.state = 'idle';
  }

  private dateTimeBcd(): number[] {
    const d = new Date();
    const bcd = (n: number) => ((Math.floor(n / 10) << 4) | (n % 10)) & 0xFF;
    const year = d.getFullYear() % 100;
    const month = d.getMonth() + 1;
    const day = d.getDate();
    const dow = d.getDay();
    const hour = d.getHours();
    const minute = d.getMinutes();
    const second = d.getSeconds();
    return [bcd(year), bcd(month), bcd(day), bcd(dow), bcd(hour), bcd(minute), bcd(second)];
  }
}
