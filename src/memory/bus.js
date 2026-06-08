import * as R from './regions';
export class Bus {
    bios = new Uint8Array(R.BIOS_SIZE);
    ewram = new Uint8Array(R.EWRAM_SIZE);
    iwram = new Uint8Array(R.IWRAM_SIZE);
    pram = new Uint8Array(R.PRAM_SIZE);
    vram = new Uint8Array(R.VRAM_SIZE);
    oam = new Uint8Array(R.OAM_SIZE);
    rom = new Uint8Array(0);
    ewram16;
    iwram16;
    pram16;
    vram16;
    oam16;
    bios32;
    ewram32;
    iwram32;
    pram32;
    vram32;
    oam32;
    rom16 = new Uint16Array(0);
    rom32 = new Uint32Array(0);
    io;
    save;
    // Last value the BIOS protection register exposes when read while PC ∉ BIOS.
    biosOpenBus = 0xE129F000;
    lastFetched = 0;
    // Approximate cycle counts per region (sequential, 16-bit access).
    // We expose them so the CPU can charge waitstates without per-cycle accuracy.
    static WS_16 = [
        1, 1, 3, 1, 1, 1, 1, 1, // 0x0 BIOS, 0x1, 0x2 EWRAM, 0x3 IWRAM, 0x4 IO, 0x5 PRAM, 0x6 VRAM, 0x7 OAM
        5, 5, 5, 5, 5, 5, // 0x8..0xD ROM (default; updated by WAITCNT later)
        5, 5, // 0xE..0xF SRAM
    ];
    constructor() {
        this.ewram16 = new Uint16Array(this.ewram.buffer);
        this.iwram16 = new Uint16Array(this.iwram.buffer);
        this.pram16 = new Uint16Array(this.pram.buffer);
        this.vram16 = new Uint16Array(this.vram.buffer);
        this.oam16 = new Uint16Array(this.oam.buffer);
        this.bios32 = new Uint32Array(this.bios.buffer);
        this.ewram32 = new Uint32Array(this.ewram.buffer);
        this.iwram32 = new Uint32Array(this.iwram.buffer);
        this.pram32 = new Uint32Array(this.pram.buffer);
        this.vram32 = new Uint32Array(this.vram.buffer);
        this.oam32 = new Uint32Array(this.oam.buffer);
    }
    loadRom(bytes) {
        // Always copy into a plain ArrayBuffer so all views share alignment + type.
        const pad32 = (bytes.length + 3) & ~3;
        const ab = new ArrayBuffer(pad32);
        const copy = new Uint8Array(ab);
        copy.set(bytes);
        this.rom = copy;
        this.rom16 = new Uint16Array(ab);
        this.rom32 = new Uint32Array(ab);
    }
    attachIo(io) { this.io = io; }
    attachSave(save) { this.save = save; }
    // ---------------------------------------------------------------- VRAM masking
    // VRAM is 96 KB but mirrored to a 128 KB region with the upper 32 KB
    // mirrored from the previous 32 KB block.
    vramOff(addr) {
        let off = addr & 0x1FFFF;
        if (off >= 0x18000)
            off -= 0x8000;
        return off;
    }
    // ---------------------------------------------------------------- reads
    read8(addr) {
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_BIOS:
                if (addr < R.BIOS_SIZE)
                    return this.bios[addr];
                return 0;
            case R.REGION_EWRAM: return this.ewram[addr & (R.EWRAM_SIZE - 1)];
            case R.REGION_IWRAM: return this.iwram[addr & (R.IWRAM_SIZE - 1)];
            case R.REGION_IO: return this.io.read8(addr & 0x3FFFFFF);
            case R.REGION_PRAM: return this.pram[addr & (R.PRAM_SIZE - 1)];
            case R.REGION_VRAM: return this.vram[this.vramOff(addr)];
            case R.REGION_OAM: return this.oam[addr & (R.OAM_SIZE - 1)];
            case R.REGION_ROM_0:
            case R.REGION_ROM_1:
            case R.REGION_ROM_2:
            case R.REGION_ROM_3:
            case R.REGION_ROM_4:
            case R.REGION_ROM_5: {
                const off = addr & 0x01FFFFFF;
                return off < this.rom.length ? this.rom[off] : (addr >>> 1) & 0xFF;
            }
            case R.REGION_SRAM:
            case R.REGION_SRAM2:
                return this.save ? this.save.read(addr & 0xFFFF) : 0xFF;
        }
        return 0;
    }
    read16(addr) {
        addr &= ~1;
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_BIOS:
                if (addr < R.BIOS_SIZE)
                    return ((this.bios[addr + 1] << 8) | this.bios[addr]) >>> 0;
                return 0;
            case R.REGION_EWRAM: return this.ewram16[(addr & (R.EWRAM_SIZE - 1)) >>> 1];
            case R.REGION_IWRAM: return this.iwram16[(addr & (R.IWRAM_SIZE - 1)) >>> 1];
            case R.REGION_IO: return this.io.read16(addr & 0x3FFFFFF);
            case R.REGION_PRAM: return this.pram16[(addr & (R.PRAM_SIZE - 1)) >>> 1];
            case R.REGION_VRAM: return this.vram16[this.vramOff(addr) >>> 1];
            case R.REGION_OAM: return this.oam16[(addr & (R.OAM_SIZE - 1)) >>> 1];
            case R.REGION_ROM_0:
            case R.REGION_ROM_1:
            case R.REGION_ROM_2:
            case R.REGION_ROM_3:
            case R.REGION_ROM_4:
            case R.REGION_ROM_5: {
                const off = (addr & 0x01FFFFFF) >>> 1;
                return off < this.rom16.length ? this.rom16[off] : (addr >>> 1) & 0xFFFF;
            }
            case R.REGION_SRAM:
            case R.REGION_SRAM2: {
                const b = this.save ? this.save.read(addr & 0xFFFF) : 0xFF;
                return (b | (b << 8)) & 0xFFFF;
            }
        }
        return 0;
    }
    read32(addr) {
        addr &= ~3;
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_BIOS:
                if (addr < R.BIOS_SIZE)
                    return this.bios32[addr >>> 2] >>> 0;
                return this.biosOpenBus;
            case R.REGION_EWRAM: return this.ewram32[(addr & (R.EWRAM_SIZE - 1)) >>> 2] >>> 0;
            case R.REGION_IWRAM: return this.iwram32[(addr & (R.IWRAM_SIZE - 1)) >>> 2] >>> 0;
            case R.REGION_IO: return this.io.read32(addr & 0x3FFFFFF) >>> 0;
            case R.REGION_PRAM: return this.pram32[(addr & (R.PRAM_SIZE - 1)) >>> 2] >>> 0;
            case R.REGION_VRAM: return this.vram32[this.vramOff(addr) >>> 2] >>> 0;
            case R.REGION_OAM: return this.oam32[(addr & (R.OAM_SIZE - 1)) >>> 2] >>> 0;
            case R.REGION_ROM_0:
            case R.REGION_ROM_1:
            case R.REGION_ROM_2:
            case R.REGION_ROM_3:
            case R.REGION_ROM_4:
            case R.REGION_ROM_5: {
                const off = (addr & 0x01FFFFFF) >>> 2;
                return off < this.rom32.length ? this.rom32[off] >>> 0 : (addr & 0xFFFFFFFF) >>> 0;
            }
            case R.REGION_SRAM:
            case R.REGION_SRAM2: {
                const b = this.save ? this.save.read(addr & 0xFFFF) : 0xFF;
                return ((b << 24) | (b << 16) | (b << 8) | b) >>> 0;
            }
        }
        return 0;
    }
    // ---------------------------------------------------------------- writes
    write8(addr, v) {
        v &= 0xFF;
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_EWRAM:
                this.ewram[addr & (R.EWRAM_SIZE - 1)] = v;
                return;
            case R.REGION_IWRAM:
                this.iwram[addr & (R.IWRAM_SIZE - 1)] = v;
                return;
            case R.REGION_IO:
                this.io.write8(addr & 0x3FFFFFF, v);
                return;
            case R.REGION_PRAM: {
                // 8-bit writes to PRAM/VRAM/OAM broadcast to a halfword.
                const off = addr & (R.PRAM_SIZE - 2);
                this.pram[off] = v;
                this.pram[off + 1] = v;
                return;
            }
            case R.REGION_VRAM: {
                const off = this.vramOff(addr) & ~1;
                // 8-bit writes to OBJ tiles (0x10000+) are ignored.
                if (off >= 0x10000)
                    return;
                this.vram[off] = v;
                this.vram[off + 1] = v;
                return;
            }
            case R.REGION_OAM: return; // OAM ignores byte writes
            case R.REGION_SRAM:
            case R.REGION_SRAM2:
                if (this.save)
                    this.save.write(addr & 0xFFFF, v);
                return;
        }
    }
    write16(addr, v) {
        addr &= ~1;
        v &= 0xFFFF;
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_EWRAM:
                this.ewram16[(addr & (R.EWRAM_SIZE - 1)) >>> 1] = v;
                return;
            case R.REGION_IWRAM:
                this.iwram16[(addr & (R.IWRAM_SIZE - 1)) >>> 1] = v;
                return;
            case R.REGION_IO:
                this.io.write16(addr & 0x3FFFFFF, v);
                return;
            case R.REGION_PRAM:
                this.pram16[(addr & (R.PRAM_SIZE - 1)) >>> 1] = v;
                return;
            case R.REGION_VRAM:
                this.vram16[this.vramOff(addr) >>> 1] = v;
                return;
            case R.REGION_OAM:
                this.oam16[(addr & (R.OAM_SIZE - 1)) >>> 1] = v;
                return;
            case R.REGION_SRAM:
            case R.REGION_SRAM2: {
                const rot = (v >>> ((addr & 1) << 3)) & 0xFF;
                if (this.save)
                    this.save.write(addr & 0xFFFF, rot);
                return;
            }
        }
    }
    write32(addr, v) {
        addr &= ~3;
        v = (v | 0) >>> 0;
        const region = (addr >>> 24) & 0xF;
        switch (region) {
            case R.REGION_EWRAM:
                this.ewram32[(addr & (R.EWRAM_SIZE - 1)) >>> 2] = v;
                return;
            case R.REGION_IWRAM:
                this.iwram32[(addr & (R.IWRAM_SIZE - 1)) >>> 2] = v;
                return;
            case R.REGION_IO:
                this.io.write32(addr & 0x3FFFFFF, v);
                return;
            case R.REGION_PRAM:
                this.pram32[(addr & (R.PRAM_SIZE - 1)) >>> 2] = v;
                return;
            case R.REGION_VRAM:
                this.vram32[this.vramOff(addr) >>> 2] = v;
                return;
            case R.REGION_OAM:
                this.oam32[(addr & (R.OAM_SIZE - 1)) >>> 2] = v;
                return;
            case R.REGION_SRAM:
            case R.REGION_SRAM2: {
                const rot = (v >>> ((addr & 3) << 3)) & 0xFF;
                if (this.save)
                    this.save.write(addr & 0xFFFF, rot);
                return;
            }
        }
    }
    // Code fetch helpers: same as reads but allow open-bus tracking.
    fetch16(addr) {
        const v = this.read16(addr);
        this.lastFetched = v;
        return v;
    }
    fetch32(addr) {
        const v = this.read32(addr);
        this.lastFetched = v;
        return v;
    }
}
