# NVIDIA NV2A (Xbox GPU) — Emulation Reference

Engineering reference for implementing the original Xbox's **NV2A** GPU in this emulator
(`core-xbox`). The NV2A is an NV20-family ("Kelvin") part, functioning as both the
northbridge and the GPU. This document is organized around the three units an emulator
must implement to boot a game and put pixels on screen: **PFIFO** (command DMA),
**PGRAPH** (3D engine), and the **display** path (PCRTC/PRAMDAC).

All MMIO offsets below are **relative to the NV2A register base `0xFD000000`** unless
otherwise stated. The physical MMIO window is `0xFD000000 – 0xFDFFFFFF` (16 MB).

> Convention: `REG[a:b]` means bitfield from bit `b` up to bit `a` inclusive. Masks are
> given as raw hex exactly as found in the cited headers.

---

## Sources

These are the authoritative references this document is distilled from. Where a value is
load-bearing it is copied verbatim from the cited source.

- **xemu** — `hw/xbox/nv2a/nv2a_regs.h` (all register/method offsets and bitfields),
  `pfifo.c` (pusher/puller state machines), `pgraph.c` (method handling).
  <https://github.com/xemu-project/xemu/blob/master/hw/xbox/nv2a/nv2a_regs.h>
- **xqemu** — earlier split of the same code (`nv2a_pfifo.c`, `nv2a_pgraph.c`).
  <https://github.com/xqemu/xqemu/blob/master/hw/xbox/nv2a/nv2a_pfifo.c>
- **envytools / nouveau** — NV1:NV4 PFIFO engine, DMA pusher, PMC, NV1 VRAM/RAMHT docs.
  <https://envytools.readthedocs.io/en/latest/hw/fifo/dma-pusher.html>,
  <https://envytools.readthedocs.io/en/latest/hw/fifo/nv1-pfifo.html>,
  <https://envytools.readthedocs.io/en/latest/hw/bus/pmc.html>
- **xboxdevwiki** — NV2A overview, memory map, vertex shader / pixel combiner pages.
  <https://xboxdevwiki.net/NV2A>, <https://xboxdevwiki.net/Memory>
- **OpenXDK / nxdk pbkit** — `lib/pbkit/pbkit.c` (pushbuffer bring-up, kick, drain, flip).
  <https://github.com/XboxDev/nxdk/blob/master/lib/pbkit/pbkit.c>
- **XboxDev nv2a-trace** — `Texture.py` (swizzle / texture formats).
  <https://github.com/XboxDev/nv2a-trace/blob/master/Texture.py>

---

## 0. Xbox unified-memory specifics (UMA)

The Xbox has **64 MB** of unified RAM (`0x00000000 – 0x03FFFFFF`). There is **no
dedicated VRAM**: the framebuffer, textures, pushbuffers, instance memory (RAMIN) and the
RAMHT/RAMFC/RAMRO tables all live in the single 64 MB pool.

Consequences for emulation:

- The NV2A's DMA engines address the same physical RAM the CPU sees. A "DMA object"
  (context DMA) is just a base+limit into main RAM (plus an AGP/PCI variant). The
  pushbuffer, vertex arrays, textures, and the front/back buffers are ordinary RAM
  allocations made by the title (via `MmAllocateContiguousMemoryEx` with
  `PAGE_WRITECOMBINE`).
- DMA addresses written to FIFO registers are masked. The kick register
  (`USER DMA_PUT`) is written as `addr & 0x03FFFFFF` (26-bit, 64 MB). `pb_busy()` compares
  `GET ^ PUT` masked with `0x0FFFFFFF`. Treat GPU memory addresses as 64 MB physical.
- **RAMIN / instance memory** sits in main RAM; the FIFO tables (RAMHT, RAMFC, RAMRO)
  are located relative to the RAMIN base and configured through PFIFO registers (below).
- There is no AGP aperture to a separate card; "AGP" context-DMA classes simply select a
  different translation but still land in the same physical RAM.

---

## 1. Memory map / engine windows

Each engine occupies a 4 KB-aligned window. Bases below are relative to `0xFD000000`.
(Source: `nv2a_regs.h` `NV_*` base defines — note the wiki's "0x..." vs the header's
internal naming; the header is authoritative and is what xemu maps.)

| Engine    | Base (rel) | Role |
|-----------|-----------:|------|
| **PMC**       | `0x000000` | Master control: chip ID, master IRQ status/enable, per-engine enable. |
| **PBUS**      | `0x001000` | PCI config shadow, straps. |
| **PFIFO**     | `0x002000` | Command DMA / FIFO control (pusher + puller + caches). |
| **PFIFO_CACHE** | `0x003000` | CACHE1 method-FIFO window (xemu names this region separately). |
| **PRMA**      | `0x004000` | Real-mode BAR0 access alias. |
| **PVIDEO**    | `0x005000` | Overlay / video scaler. |
| **PTIMER**    | `0x006000` | 56-bit timer + alarm interrupt. |
| **PCOUNTER**  | `0x007000` | Performance counters. |
| **PVPE**      | `0x008000` | MPEG/video processing engine. |
| **PTV**       | `0x009000` | TV encoder interface. |
| **PRMFB**     | `0x0A0000`* | Framebuffer aperture alias. |
| **PRMVIO**    | `0x0B0000`* | VGA sequencer/graphics alias. |
| **PFB**       | `0x100000` | Framebuffer/memory controller: RAM config, tiling regions. |
| **PSTRAPS**   | `0x101000`* | Straps. |
| **PGRAPH**    | `0x400000` | The 2D/3D drawing engine (Kelvin). |
| **PCRTC**     | `0x600000` | CRTC: scanout start address, VBlank interrupt. |
| **PRMCIO**    | `0x601000`* | VGA CRTC index/data alias. |
| **PRAMDAC**   | `0x680000` | RAMDAC: PLLs (clocks), cursor, flat-panel timing. |
| **PRMDIO**    | `0x681000`* | VGA DAC alias. |
| **PRAMIN**    | `0x700000` | Instance memory window (RAMHT/RAMFC/RAMRO, DMA objects, gr ctx). |
| **USER**      | `0x800000` | Per-channel "user" submission area (DMA_PUT/GET). |

> *Windows marked `*` follow the classic NV20 layout; the precise Xbox base for the rarely
> touched VGA-alias windows is not needed for 3D bring-up. The header constants in
> `nv2a_regs.h` are small (`NV_PFB 0x0000C000` etc.) because xemu indexes them through a
> block table; the values in the table column above are the **absolute MMIO offsets** an
> emulator decodes when a guest accesses `0xFD000000 + off`. Implement the decode as a
> range table keyed on the top bits (`0x000xxx`→PMC, `0x002xxx`→PFIFO, `0x400xxx`→PGRAPH,
> `0x600xxx`→PCRTC, `0x680xxx`→PRAMDAC, `0x700xxx`→PRAMIN, `0x800xxx`→USER, etc.).

### Within-engine register offsets are what matter

In the header the engines are zero-based blocks. Below, all offsets are **within the
engine** (add the engine base to get the absolute MMIO offset). E.g. `PMC_INTR_0` is at
absolute `0xFD000100`; `PGRAPH` methods are reached at `0xFD400000+`.

---

## 2. PMC — Master control (base `0x000000`)

| Offset | Register | Notes |
|------:|----------|-------|
| `0x000` | `PMC_BOOT_0` | Chip ID / revision. Return a plausible NV20-family ID. |
| `0x100` | `PMC_INTR_0` | Master interrupt status (per-engine, read-only summary). |
| `0x140` | `PMC_INTR_EN_0` | Master interrupt enable. |
| `0x200` | `PMC_ENABLE` | Per-engine clock/enable gates. |

**`PMC_INTR_0` (0x100)** — a bit is set when the corresponding engine has a pending,
enabled interrupt. The CPU reads this in its top-level ISR to decide which engine to
service. Bits (from `nv2a_regs.h`):

| Bit | Mask | Engine |
|----:|------|--------|
| 8  | `0x00000100` | `PFIFO` |
| 12 | `0x00001000` | `PGRAPH` |
| 24 | `0x01000000` | `PCRTC` (VBlank) |
| 28 | `0x10000000` | `PBUS` |
| 31 | `0x80000000` | `SOFTWARE` |

(PTIMER and PVIDEO also summarize here in the full NV20 map; on Xbox the kernel mainly
cares about PFIFO/PGRAPH/PCRTC.) `PMC_INTR_0` is **not** directly writable to ack — you ack
at the originating engine's `INTR_0`; `PMC_INTR_0` then re-derives.

**`PMC_INTR_EN_0` (0x140)**:
- bit0 `HARDWARE` = `0x1`
- bit1 `SOFTWARE` = `0x2`

**`PMC_ENABLE` (0x200)** — gates engines. Relevant bits:
- `PFIFO` = `1 << 8` (`0x100`)
- `PGRAPH` = `1 << 12` (`0x1000`)

The kernel pulses these during reset (write 0 to disable, then re-enable). When an engine
is disabled in `PMC_ENABLE`, its state machine must be held idle.

---

## 3. PFIFO — Command DMA (base `0x002000`)

This is the heart of the emulator. The CPU never pokes PGRAPH method registers directly in
the normal path; instead it writes a **pushbuffer** in RAM and tells the FIFO to consume
it. The FIFO has two state machines: the **pusher** (reads the pushbuffer DMA stream,
parses command headers, pushes `(subchannel, method, data)` entries into CACHE1) and the
**puller** (drains CACHE1, binds objects via RAMHT, dispatches methods to PGRAPH).

### 3.1 PFIFO control registers

| Offset | Register | Key fields |
|------:|----------|-----------|
| `0x100` | `PFIFO_INTR_0` | interrupt status (write-1-to-clear) |
| `0x140` | `PFIFO_INTR_EN_0` | interrupt enable (same bit layout) |
| `0x210` | `PFIFO_RAMHT` | hash-table base & size |
| `0x214` | `PFIFO_RAMFC` | FIFO-context base & size |
| `0x218` | `PFIFO_RAMRO` | run-out base & size |
| `0x400` | `PFIFO_RUNOUT_STATUS` | run-out fifo status |
| `0x504` | `PFIFO_MODE` | per-channel PIO/DMA mode bitmap |
| `0x508` | `PFIFO_DMA` | per-channel DMA-pending bitmap |

**`PFIFO_INTR_0` (0x100)** bits:

| Bit | Mask | Meaning |
|----:|------|---------|
| 0  | `0x00000001` | `CACHE_ERROR` — puller hit a bad/unbound method |
| 4  | `0x00000010` | `RUNOUT` — a method went to run-out (no channel/space) |
| 8  | `0x00000100` | `RUNOUT_OVERFLOW` |
| 12 | `0x00001000` | `DMA_PUSHER` — pusher parse error |
| 16 | `0x00010000` | `DMA_PT` — page-table/protection error |
| 20 | `0x00100000` | `SEMAPHORE` |
| 24 | `0x01000000` | `ACQUIRE_TIMEOUT` |

**`PFIFO_RAMHT` (0x210)**:
- `BASE_ADDRESS` `0x000001F0` — RAMHT base in instance memory (`<<` to get byte offset).
- `SIZE` `0x00030000` — `0=4K,1=8K,2=16K,3=32K`.
- `SEARCH` `0x03000000` — collision search stride `0=16,1=32,2=64,3=128`.

**`PFIFO_RAMFC` (0x214)**: `BASE_ADDRESS1 0x000001FC`, `SIZE 0x00010000`,
`BASE_ADDRESS2 0x00FE0000`. RAMFC stores per-channel saved FIFO context (the
swap-out/swap-in of CACHE1 state when switching channels).

**`PFIFO_RAMRO` (0x218)**: `BASE_ADDRESS 0x000001FE`, `SIZE 0x00010000`. Run-out is where
methods that cannot be processed (invalid channel) are logged.

### 3.2 CACHE1 registers (the active channel's FIFO)

The Xbox uses **CACHE1** in DMA mode. These offsets are within PFIFO (xemu groups them as
`NV_PFIFO_CACHE1_*` at `0x1200+`):

| Offset | Register | Fields |
|------:|----------|--------|
| `0x1200` | `CACHE1_PUSH0` | `ACCESS` (bit0) — enable pusher→cache writes |
| `0x1204` | `CACHE1_PUSH1` | `CHID 0x1F`; `MODE 0x100` (`0=PIO,1=DMA`) |
| `0x1210` | `CACHE1_PUT` | cache write pointer (pusher side) |
| `0x1214` | `CACHE1_STATUS` | `LOW_MARK` (bit4)=empty, `HIGH_MARK` (bit8)=full |
| `0x1220` | `CACHE1_DMA_PUSH` | pusher control/status (see below) |
| `0x1224` | `CACHE1_DMA_FETCH` | `TRIG 0xF8`, `SIZE 0xE000`, `MAX_REQS 0x1F0000` |
| `0x1228` | `CACHE1_DMA_STATE` | parsed-header state (see below) |
| `0x122C` | `CACHE1_DMA_INSTANCE` | `ADDRESS 0xFFFF` — pushbuffer DMA object instance |
| `0x1240` | `CACHE1_DMA_PUT` | pushbuffer write ptr (mirror of USER DMA_PUT) |
| `0x1244` | `CACHE1_DMA_GET` | pushbuffer read ptr (advanced by pusher) |
| `0x1248` | `CACHE1_REF` | reference counter (REF/semaphore) |
| `0x124C` | `CACHE1_DMA_SUBROUTINE` | `RETURN_OFFSET 0x1FFFFFFC`, `STATE` (bit0=active) |
| `0x1250` | `CACHE1_PULL0` | `ACCESS` (bit0) — enable puller |
| `0x1254` | `CACHE1_PULL1` | `ENGINE 0x3` (which engine the bound object uses) |
| `0x1270` | `CACHE1_GET` | cache read pointer (puller side) |
| `0x1280` | `CACHE1_ENGINE` | per-subchannel engine assignment |
| `0x12A0` | `CACHE1_DMA_DCOUNT` | `VALUE 0x1FFC` — data words consumed in current method run |
| `0x12A4` | `CACHE1_DMA_GET_JMP_SHADOW` | `OFFSET 0x1FFFFFFC` — pre-jump GET (for restart) |
| `0x12A8` | `CACHE1_DMA_RSVD_SHADOW` | last header word |
| `0x12AC` | `CACHE1_DMA_DATA_SHADOW` | last data word |
| `0x1800` | `CACHE1_METHOD` | cache entry: `TYPE`(b0) `ADDRESS 0x1FFC` `SUBCHANNEL 0xE000` |
| `0x1804` | `CACHE1_DATA` | cache entry data |

**`CACHE1_DMA_PUSH` (0x1220)** — the pusher's run/gate register:

| Bit | Mask | Field | Meaning |
|----:|------|-------|---------|
| 0  | `0x00001` | `ACCESS`  | `1` = pusher enabled. **Bring-up sets this.** |
| 4  | `0x00010` | `STATE`   | pusher internal state |
| 8  | `0x00100` | `BUFFER`  | buffer state |
| 12 | `0x01000` | `STATUS`  | `1` = **busy/suspended** (set on error; pusher won't run) |
| 16 | `0x10000` | `ACQUIRE` | semaphore-acquire pending |

The pusher runs only when `PUSH0.ACCESS` **and** `DMA_PUSH.ACCESS` are set and
`DMA_PUSH.STATUS` (busy) is clear. On a parse error the pusher sets `DMA_STATE.ERROR`,
sets `DMA_PUSH.STATUS` (suspends itself) and raises `PFIFO_INTR_0.DMA_PUSHER`.

**`CACHE1_DMA_STATE` (0x1228)** — decoded current-header state:

| Field | Mask | Shift | Meaning |
|-------|------|------:|---------|
| `METHOD_TYPE` | `0x00000001` | 0 | `0=INC`, `1=NON_INC` |
| `METHOD` | `0x00001FFC` | 2 | current method (word offset ×4) |
| `SUBCHANNEL` | `0x0000E000` | 13 | subchannel 0–7 |
| `METHOD_COUNT` | `0x1FFC0000` | 18 | remaining data words |
| `ERROR` | `0xE0000000` | 29 | `0 NONE,1 CALL,2 NON_CACHE,3 RETURN,4 RESERVED_CMD,6 PROTECTION` |

### 3.3 USER / channel registers (base `0x800000`)

Per channel the USER region is at `USER_BASE + (chid << 16)`. Channel 0 = `0x800000`.
Within a channel:

| Offset (abs for ch0) | Register | Meaning |
|------:|----------|---------|
| `0x800040` | `USER_DMA_PUT` | CPU writes the pushbuffer write pointer here → **kicks the pusher** |
| `0x800044` | `USER_DMA_GET` | reads the pusher's current read pointer (drain check) |
| `0x800048` | `USER_REF`     | reference counter |

`USER_DMA_PUT`/`GET` are aliases of `CACHE1_DMA_PUT`/`GET` for the active channel. A write
to `USER_DMA_PUT` must (a) update `CACHE1_DMA_PUT`, (b) wake/run the pusher.

### 3.4 The pusher state machine

Run while `DMA_GET != DMA_PUT`, `PUSH0.ACCESS & DMA_PUSH.ACCESS` set, `DMA_PUSH.STATUS`
clear. Reads 32-bit words from the pushbuffer DMA object (instance in
`CACHE1_DMA_INSTANCE`) at byte address `DMA_GET`.

```
while (DMA_GET != DMA_PUT && !suspended):
    word = read32(pushbuffer_dma_base + DMA_GET)
    DMA_GET += 4
    if DMA_STATE.METHOD_COUNT > 0:               # we are in a data run
        push_cache(subc=DMA_STATE.SUBCHANNEL, mthd=DMA_STATE.METHOD, data=word)
        if !DMA_STATE.METHOD_TYPE:               # increasing
            DMA_STATE.METHOD += 4                 # next method (word offset)
        DMA_STATE.METHOD_COUNT -= 1
        DMA_DCOUNT += 1
        continue
    # else parse a command header (order matters):
    if (word & 0xE0000003) == 0x20000000:        # "old"/short jump (NV4)
        DMA_GET = word & 0x1FFFFFFC
    elif (word & 3) == 1:                         # jump (NV1A)
        DMA_GET = word & 0xFFFFFFFC
    elif (word & 3) == 2:                         # call
        SUBROUTINE.RETURN_OFFSET = DMA_GET; SUBROUTINE.STATE = 1
        DMA_GET = word & 0xFFFFFFFC
    elif word == 0x00020000:                      # return
        DMA_GET = SUBROUTINE.RETURN_OFFSET; SUBROUTINE.STATE = 0
    elif (word & 0xE0030003) == 0x00000000:       # increasing methods
        DMA_STATE.METHOD       = (word >> 2)  & 0x7FF * 4   # i.e. word & 0x1FFC
        DMA_STATE.SUBCHANNEL   = (word >> 13) & 7
        DMA_STATE.METHOD_COUNT = (word >> 18) & 0x7FF
        DMA_STATE.METHOD_TYPE  = INC
    elif (word & 0xE0030003) == 0x40000000:       # non-increasing methods
        ... same fields, METHOD_TYPE = NON_INC
    else:
        DMA_STATE.ERROR = RESERVED_CMD
        DMA_PUSH.STATUS = 1                        # suspend
        raise PFIFO_INTR_0.DMA_PUSHER
```

Key constants (from `pfifo.c`): jump address mask `0x1FFFFFFC` / `0xFFFFFFFC`, return
sentinel `0x00020000`, header field extraction `method = word & 0x1FFC`,
`subc = (word >> 13) & 7`, `count = (word >> 18) & 0x7FF`. The method field is a **word
offset shifted left by 2** — i.e. the raw method byte offset is `word & 0x1FFC`, matching
the `NV097_*` byte offsets in section 5.

A practical emulator may **bypass CACHE1 and call the puller/PGRAPH inline** per word
(xemu effectively does this), as long as the externally visible `DMA_GET`/`STATUS`/`INTR`
behavior matches what the busy-wait in section 3.6 expects.

### 3.5 The puller state machine + RAMHT

When `PULL0.ACCESS` is set, the puller drains entries from CACHE1 (between `CACHE1_GET`
and `CACHE1_PUT`):

```
while CACHE1_GET != CACHE1_PUT:
    e = cache[CACHE1_GET]; CACHE1_GET advance
    subc = e.SUBCHANNEL; mthd = e.METHOD; data = e.DATA
    if mthd == 0:                                  # SET_OBJECT
        instance = ramht_lookup(handle = data)     # resolve gr object
        if !instance: raise PFIFO_INTR_0.CACHE_ERROR
        bind_subchannel(subc, instance)            # remember class (0x97 Kelvin)
        CACHE1_ENGINE[subc] = engine_of(instance)
    else:
        pgraph_method(channel, subc, mthd, data)   # dispatch to PGRAPH
```

**RAMHT** maps a 32-bit *object handle* (what the title chose, e.g. `0xCAFEBABE`) to an
*instance address* (the in-RAMIN gr object that carries the class id and a context-DMA).
Each RAMHT entry is **8 bytes**: word0 = handle; word1 = packed
`instance_address | (engine << 16) | (chid << 24)` (the precise pack matches NV20:
instance in low bits, engine in bits 16–17, channel in bits 24–28; valid bit at bit 31).

**Hash function** (from `ramht_hash`): fold the handle in `bits`-sized chunks with XOR,
then XOR in `chid << (bits-4)`, where `bits = log2(ramht_size_in_entries)`:

```
hash = 0
h = handle
while h:
    hash ^= (h & (size_entries - 1))
    h >>= bits
hash ^= chid << (bits - 4)
entry = ramht_base + hash * 8
```

On collision, linearly probe forward by the `SEARCH` stride until handle matches or a free
slot is hit (free → `CACHE_ERROR`). The class id read from the instance object is `0x97`
("Kelvin" 3D) for all 3D work; `0x62`/`0x9F`/`0x19` etc. exist for 2D blits but the 3D
path is what games use.

### 3.6 FIFO bring-up + busy-wait (what the emulator must satisfy)

The kernel/Direct3D8 (and nxdk's pbkit, which mirrors it) brings the FIFO up roughly as:

1. **Reset/enable engines**: write `PMC_ENABLE` with `PFIFO|PGRAPH` set.
2. **Allocate the pushbuffer** in contiguous, write-combined RAM (nxdk default 512 KB,
   power-of-two). `pb_Put = pb_Head`.
3. **Create the channel's DMA context** (a context-DMA object covering all RAM,
   `base=0, limit=MAXRAM`) and write its instance into `CACHE1_DMA_INSTANCE`. nxdk uses
   **channel 6** for the pushbuffer; D3D uses the kernel's GPU channel — the channel id is
   only relevant in that USER access is at `USER_BASE + (chid<<16)`.
4. **Program `CACHE1_DMA_FETCH`** (`TRIG`,`SIZE`,`MAX_REQS`), set `CACHE1_PUSH1.MODE=DMA`
   and `CHID`, enable `CACHE1_PUSH0.ACCESS`, `CACHE1_PULL0.ACCESS`,
   `CACHE1_DMA_PUSH.ACCESS`.
5. **Submit**: write methods into the pushbuffer, then **write the new write pointer to
   `USER_DMA_PUT` (`0x800040`)** masked `& 0x03FFFFFF`. This is the "kick".
6. **Drain / busy-wait**: poll `USER_DMA_GET` (`0x800044`); the buffer is drained when
   `(GET ^ PUT) & 0x0FFFFFFF == 0`. nxdk's `pb_busy()` also returns busy while
   `PGRAPH_STATUS` (in the PGRAPH window) is non-zero.

**Emulator requirements that fall out of this:**
- A write to `USER_DMA_PUT` must run the pusher until `DMA_GET == DMA_PUT` (or an error
  suspends it), updating `DMA_GET` as it goes so the title's poll terminates.
- `PGRAPH_STATUS` must read back **idle (0)** once methods are processed, or `pb_busy()`
  never returns. Set a "busy" bit while dispatching a batch and clear it when done.
- If you process synchronously inside the `USER_DMA_PUT` write, set `DMA_GET = DMA_PUT`
  before returning so the very first poll sees "drained".
- `NV097_WAIT_FOR_IDLE` (method `0x110`) and `NV097_NO_OPERATION` (`0x100`) must be
  accepted and complete immediately.

### Method-header encoding (the format the title writes)

nxdk builds headers as `EncodeMethod(subc,cmd,n) = (n<<18) | (subc<<13) | cmd`. This is
exactly the **increasing-method** header the pusher parses (count in `[28:18]`/`0x1FFC0000`,
subchannel `[15:13]`/`0xE000`, method `[12:2]`/`0x1FFC`, with `[1:0]=00`,
`[31:29]=000`). See section 4 for the full command grammar.

---

## 4. NV FIFO command format (32-bit method headers)

Every non-data word in the pushbuffer is a command header. Classification (matches the
pusher masks above):

| Command | Test | Payload |
|---------|------|---------|
| **Increasing methods** | `(w & 0xE0030003) == 0x00000000` | `method=w&0x1FFC`, `subc=(w>>13)&7`, `count=(w>>18)&0x7FF`. The next `count` words are data; method advances by 4 each. |
| **Non-increasing methods** | `(w & 0xE0030003) == 0x40000000` | same fields; method does **not** advance — all `count` words go to the same method. |
| **Jump (NV1A)** | `(w & 3) == 1` | `DMA_GET = w & 0xFFFFFFFC` |
| **Old/short jump (NV4)** | `(w & 0xE0000003) == 0x20000000` | `DMA_GET = w & 0x1FFFFFFC` |
| **Call** | `(w & 3) == 2` | save GET→`SUBROUTINE`, `DMA_GET = w & 0xFFFFFFFC` |
| **Return** | `w == 0x00020000` | restore GET from `SUBROUTINE` |

Bit layout of a methods header:

```
 31 30 29 28              18 17    15 14 13 12             2  1  0
  0  0  0 [ count : 11 bits ] [ subc:3 ] [ method>>2 : 11 bits ] 0 0
        ^ bit29=1 -> non-increasing (0x40000000)
```

`count` max `0x7FF` (2047) data words per header. Multi-word methods (matrices,
combiners, vertex programs) use one header with a large count.

---

## 5. PGRAPH — the Kelvin 3D engine, class 0x97 (base `0x400000`)

PGRAPH receives `(subchannel, method, data)` from the puller. Methods are the `NV097_*`
byte offsets below (these equal `word & 0x1FFC` from the header). Bind class `0x97` via
`SET_OBJECT` first. Offsets and field masks are from `nv2a_regs.h`.

### 5.1 Object/sync/context-DMA

| Method | Offset | Notes |
|--------|-------:|-------|
| `SET_OBJECT` | `0x0000` | (handled by puller via RAMHT, not PGRAPH) |
| `NO_OPERATION` | `0x0100` | nop; if data≠0, also triggers a notify error path |
| `WAIT_FOR_IDLE` | `0x0110` | flush; complete immediately in emu |
| `SET_FLIP_READ` | `0x0120` | which buffer is being scanned out |
| `SET_FLIP_WRITE` | `0x0124` | which buffer is the render target |
| `SET_FLIP_MODULO` | `0x0128` | number of buffers (triple buffer = 3) |
| `FLIP_INCREMENT_WRITE` | `0x012C` | advance write buffer index |
| `FLIP_STALL` | `0x0130` | stall pusher until the displayed buffer flips (VBlank sync) |
| `SET_CONTEXT_DMA_NOTIFIES` | `0x0180` | DMA object for notifier writes |
| `SET_CONTEXT_DMA_A` / `_B` | `0x0184` / `0x0188` | texture DMA objects |
| `SET_CONTEXT_DMA_STATE` | `0x0190` | |
| `SET_CONTEXT_DMA_COLOR` | `0x0194` | color-surface DMA object |
| `SET_CONTEXT_DMA_ZETA` | `0x0198` | depth-surface DMA object |
| `SET_CONTEXT_DMA_VERTEX_A` / `_B` | `0x019C` / `0x01A0` | vertex-array DMA objects |
| `SET_CONTEXT_DMA_SEMAPHORE` | `0x01A4` | semaphore DMA object |
| `SET_CONTEXT_DMA_REPORT` | `0x01A8` | occlusion/report DMA object |

A "context DMA" handle here is resolved (via RAMHT/instance) to a base+limit into RAM;
subsequent surface/texture/vertex offsets are added to that base.

### 5.2 Surface setup

| Method | Offset | Fields |
|--------|-------:|--------|
| `SET_SURFACE_CLIP_HORIZONTAL` | `0x0200` | `X 0x0000FFFF`, `WIDTH 0xFFFF0000` |
| `SET_SURFACE_CLIP_VERTICAL` | `0x0204` | `Y 0x0000FFFF`, `HEIGHT 0xFFFF0000` |
| `SET_SURFACE_FORMAT` | `0x0208` | see below |
| `SET_SURFACE_PITCH` | `0x020C` | `COLOR 0x0000FFFF`, `ZETA 0xFFFF0000` |
| `SET_SURFACE_COLOR_OFFSET` | `0x0210` | byte offset into COLOR dma |
| `SET_SURFACE_ZETA_OFFSET` | `0x0214` | byte offset into ZETA dma |

**`SET_SURFACE_FORMAT` (0x0208)**:
- `COLOR` `0x0000000F`:
  `1=X1R5G5B5_Z1R5G5B5`, `2=X1R5G5B5_O1R5G5B5`, `3=R5G6B5`,
  `4=X8R8G8B8_Z8R8G8B8`, `5=X8R8G8B8_O8R8G8B8`, `6=X1A7R8G8B8_Z1A7R8G8B8`,
  `7=X1A7R8G8B8_O1A7R8G8B8`, `8=A8R8G8B8`, `9=B8`, `0xA=G8B8`.
- `ZETA` `0x000000F0`: `1=Z16`, `2=Z24S8`.
- `TYPE` `0x00000F00`: `1=PITCH` (linear), `2=SWIZZLE`.
- `ANTI_ALIASING` `0x0000F000`: `0=CENTER_1`, `1=CENTER_CORNER_2`, `2=SQUARE_OFFSET_4`.
- `WIDTH` `0x00FF0000` (log2), `HEIGHT` `0xFF000000` (log2) — used for swizzled surfaces.

### 5.3 Clear

| Method | Offset | Fields |
|--------|-------:|--------|
| `SET_ZSTENCIL_CLEAR_VALUE` | `0x1D8C` | packed Z24S8 (or Z16) clear value |
| `SET_COLOR_CLEAR_VALUE` | `0x1D90` | ARGB clear color |
| `CLEAR_SURFACE` | `0x1D94` | `Z`(b0) `STENCIL`(b1) `R`(b4) `G`(b5) `B`(b6) `A`(b7); `COLOR=0xF0` |
| `SET_CLEAR_RECT_HORIZONTAL` | `0x1D98` | x0/x1 |
| `SET_CLEAR_RECT_VERTICAL` | `0x1D9C` | y0/y1 |

`CLEAR_SURFACE` clears the region intersected by the clip rect and clear rect, using the
clear values, for whichever planes are flagged. This is the **first thing to implement** —
a game that boots will clear to a color every frame.

### 5.4 Transform / viewport / matrices

| Method | Offset | Notes |
|--------|-------:|-------|
| `SET_VIEWPORT_OFFSET` | `0x0A20` | 4 floats (x,y,z,w offset) |
| `SET_VIEWPORT_SCALE` | `0x0AF0` | 4 floats |
| `SET_PROJECTION_MATRIX` | `0x0440` | 16 floats |
| `SET_MODEL_VIEW_MATRIX` | `0x0480` | 16 floats (×4 blend weights region follows) |
| `SET_INVERSE_MODEL_VIEW_MATRIX` | `0x0580` | 16 floats |
| `SET_COMPOSITE_MATRIX` | `0x0680` | 16 floats (the combined MVP used by fixed-function) |
| `SET_TEXTURE_MATRIX` | `0x06C0` | per-stage texture matrix |
| `SET_TRANSFORM_PROGRAM` | `0x0B00` | vertex-shader microcode upload (4 dwords/instr) |
| `SET_TRANSFORM_CONSTANT` | `0x0B80` | shader constants |
| `SET_TRANSFORM_PROGRAM_LOAD` | `0x1E9C` | program load index |
| `SET_TRANSFORM_PROGRAM_START` | `0x1EA0` | program start index |
| `SET_TRANSFORM_CONSTANT_LOAD` | `0x1EA4` | constant load index |
| `SET_TRANSFORM_EXECUTION_MODE` | `0x1E94` | `MODE 0x3` (fixed-function vs program) |

Vertices flow `object → (model_view) → (projection) → viewport(scale/offset) → screen`. For
shaders, the title uploads microcode via `SET_TRANSFORM_PROGRAM` then sets execution mode.

### 5.5 Lighting / material (fixed-function)

| Method | Offset |
|--------|-------:|
| `SET_LIGHTING_ENABLE` | `0x0314` |
| `SET_LIGHT_ENABLE_MASK` | `0x03BC` (`OFF/INFINITE/LOCAL/SPOT` 2 bits per light) |
| `SET_LIGHT_AMBIENT/DIFFUSE/SPECULAR_COLOR` | `0x1000/0x100C/0x1018` |
| `SET_LIGHT_LOCAL_POSITION` | `0x105C` |
| `SET_LIGHT_INFINITE_DIRECTION` | `0x1034` |
| `SET_SCENE_AMBIENT_COLOR` | `0x0A10` |
| `SET_MATERIAL_EMISSION` | `0x03A8` |
| `SET_MATERIAL_ALPHA` | `0x03B4` |
| `SET_SPECULAR_ENABLE` | `0x03B8` |
| `SET_NORMALIZATION_ENABLE` | `0x03A4` |

### 5.6 Raster/blend/depth/stencil/alpha state

| Method | Offset | Notes |
|--------|-------:|-------|
| `SET_ALPHA_TEST_ENABLE` | `0x0300` | |
| `SET_ALPHA_FUNC` / `SET_ALPHA_REF` | `0x033C` / `0x0340` | |
| `SET_BLEND_ENABLE` | `0x0304` | |
| `SET_BLEND_FUNC_SFACTOR` / `DFACTOR` | `0x0344` / `0x0348` | GL-style enums (`V_ZERO 0`, `V_ONE 1`, `V_SRC_ALPHA 0x302`, `V_ONE_MINUS_SRC_ALPHA 0x303`, …) |
| `SET_BLEND_COLOR` | `0x034C` | |
| `SET_BLEND_EQUATION` | `0x0350` | `ADD 0x8006`, `SUBTRACT 0x800A`, `MIN 0x8007`, `MAX 0x8008` |
| `SET_DEPTH_TEST_ENABLE` | `0x030C` | |
| `SET_DEPTH_FUNC` | `0x0354` | |
| `SET_DEPTH_MASK` | `0x035C` | |
| `SET_COLOR_MASK` | `0x0358` | `B`(b0) `G`(b8) `R`(b16) `A`(b24) write-enables |
| `SET_STENCIL_TEST_ENABLE` | `0x032C` | |
| `SET_STENCIL_FUNC/REF/MASK` | `0x0364/0x0368/0x036C` | |
| `SET_STENCIL_OP_FAIL/ZFAIL/ZPASS` | `0x0370/0x0374/0x0378` | `KEEP 0x1E00`, `ZERO 0`, `REPLACE 0x1E01`, `INCR 0x8507`, `DECR 0x8508`, `INVERT 0x150A` |
| `SET_STENCIL_MASK` | `0x0360` | |
| `SET_CULL_FACE_ENABLE` | `0x0308` | |
| `SET_CULL_FACE` | `0x039C` | `FRONT 0x404`, `BACK 0x405`, `FRONT_AND_BACK 0x408` |
| `SET_FRONT_FACE` | `0x03A0` | `CW 0x900`, `CCW 0x901` |
| `SET_SHADE_MODE` | `0x037C` | `FLAT 0x1D00`, `SMOOTH 0x1D01` |
| `SET_FRONT/BACK_POLYGON_MODE` | `0x038C/0x0390` | `POINT 0x1B00`, `LINE 0x1B01`, `FILL 0x1B02` |
| `SET_DITHER_ENABLE` | `0x0310` | |
| `SET_CLIP_MIN/MAX` | `0x0394/0x0398` | depth range |
| `SET_CONTROL0` | `0x0290` | `STENCIL_WRITE_ENABLE`(b0), `Z_FORMAT`(b12), `Z_PERSPECTIVE_ENABLE`(b16) |

### 5.7 Texture state (per stage; stage stride applies)

| Method | Offset | Fields |
|--------|-------:|--------|
| `SET_TEXTURE_OFFSET` | `0x1B00` | byte offset into texture DMA |
| `SET_TEXTURE_FORMAT` | `0x1B04` | see below |
| `SET_TEXTURE_ADDRESS` | `0x1B08` | wrap modes (U/V/P) |
| `SET_TEXTURE_CONTROL0` | `0x1B0C` | `ENABLE`(b30), `MIN_LOD_CLAMP 0x3FFC0000`, `MAX_LOD_CLAMP 0x0003FFC0` |
| `SET_TEXTURE_CONTROL1` | `0x1B10` | `IMAGE_PITCH 0xFFFF0000` (linear pitch) |
| `SET_TEXTURE_FILTER` | `0x1B14` | `MIN 0x00FF0000`, `MAG 0x0F000000`, `MIPMAP_LOD_BIAS 0x1FFF`, `*SIGNED` bits 28–31 |
| `SET_TEXTURE_IMAGE_RECT` | `0x1B1C` | `WIDTH 0xFFFF0000`, `HEIGHT 0x0000FFFF` (linear sizes) |
| `SET_TEXTURE_PALETTE` | `0x1B20` | `CONTEXT_DMA`(b0), `LENGTH 0xC`, `OFFSET 0xFFFFFFC0` |
| `SET_TEXTURE_BORDER_COLOR` | `0x1B24` | |

**`SET_TEXTURE_FORMAT` (0x1B04)**:
- `CONTEXT_DMA` `0x3` — which texture DMA (A/B).
- `CUBEMAP_ENABLE` `1<<2`; `BORDER_SOURCE` `1<<3` (`0=TEXTURE,1=COLOR`).
- `DIMENSIONALITY` `0x000000F0` (1D/2D/3D).
- `COLOR` `0x0000FF00` — format code (see section 6).
- `MIPMAP_LEVELS` `0x000F0000`.
- `BASE_SIZE_U/V/P` `0x00F00000 / 0x0F000000 / 0xF0000000` — log2 dims (for swizzled).

### 5.8 Register combiners (the "pixel shader")

The NV2A pixel pipeline is NV20 register combiners (up to 8 general stages + a final
combiner), not arbitrary shaders.

| Method | Offset | Notes |
|--------|-------:|-------|
| `SET_COMBINER_CONTROL` | `0x1E60` | number of active stages, flags |
| `SET_COMBINER_COLOR_ICW` | `0x0AC0` | per-stage color input mappings (×stages) |
| `SET_COMBINER_COLOR_OCW` | `0x1E40` | per-stage color output mappings |
| `SET_COMBINER_ALPHA_ICW` | `0x0260` | per-stage alpha input |
| `SET_COMBINER_ALPHA_OCW` | `0x0AA0` | per-stage alpha output |
| `SET_COMBINER_FACTOR0` / `FACTOR1` | `0x0A60` / `0x0A80` | per-stage constants |
| `SET_COMBINER_SPECULAR_FOG_CW0/CW1` | `0x0288 / 0x028C` | final-combiner inputs |
| `SET_SHADER_STAGE_PROGRAM` | `0x1E70` | texture-stage program (dot product, bump, etc.) |
| `SET_SHADER_OTHER_STAGE_INPUT` | `0x1E78` | inter-stage routing |

Each ICW/OCW packs A/B/C/D input selectors, mappings (identity, invert, signed scale),
and an output write/scale/bias. See the xboxdevwiki Pixel Combiner page for the per-field
encoding; implement as a fixed combiner evaluator parameterized by these words.

### 5.9 Primitive / vertex submission

| Method | Offset | Fields |
|--------|-------:|--------|
| `SET_BEGIN_END` | `0x17FC` | `0=END`, `1=POINTS`, `2=LINES`, `3=LINE_LOOP`, `4=LINE_STRIP`, `5=TRIANGLES`, `6=TRIANGLE_STRIP`, `7=TRIANGLE_FAN`, `8=QUADS`, `9=QUAD_STRIP`, `0xA=POLYGON` |
| `DRAW_ARRAYS` | `0x1810` | `START_INDEX 0x00FFFFFF`, `COUNT 0xFF000000` (count is N-1) |
| `INLINE_ARRAY` | `0x1818` | inline interleaved vertex data (per the array formats) |
| `ARRAY_ELEMENT16` | `0x1800` | two 16-bit indices per dword |
| `ARRAY_ELEMENT32` | `0x1808` | one 32-bit index per dword |
| `SET_VERTEX_DATA_ARRAY_OFFSET` | `0x1720` | per-attribute base offset (×16 attribs) |
| `SET_VERTEX_DATA_ARRAY_FORMAT` | `0x1760` | `TYPE 0xF`, `SIZE 0xF0`, `STRIDE 0xFFFFFF00` |

Vertex-array `TYPE` values: `0=UB_D3D`, `1=S1`, `2=F`(float), `4=UB_OGL`, `5=S32K`,
`6=CMP` (packed). There are 16 vertex attributes (position, weight, normal, diffuse,
specular, fog, point-size, back colors, texcoord0–3, etc.).

**Immediate-mode vertex methods** (used between `SET_BEGIN_END` begin and end):

| Method | Offset |
|--------|-------:|
| `SET_VERTEX3F` / `SET_VERTEX4F` | `0x1500 / 0x1518` |
| `SET_NORMAL3F` / `SET_NORMAL3S` | `0x1530 / 0x1540` |
| `SET_DIFFUSE_COLOR4F/3F/4UB` | `0x1550 / 0x1560 / 0x156C` |
| `SET_SPECULAR_COLOR4F/3F/4UB` | `0x1570 / 0x1580 / 0x158C` |
| `SET_TEXCOORD0_2F/4F/2S/4S` | `0x1590 / 0x15A0 / 0x1598 / 0x15B0` |
| `SET_TEXCOORD1..3_*` | `0x15B8 …` (contiguous blocks) |
| `SET_VERTEX_DATA2F_M / 4F_M / 2S / 4S_M / 4UB` | `0x1880 / 0x1A00 / 0x1900 / 0x1980 / 0x1940` |

Drawing flow: set array formats+offsets (or use immediate methods), `SET_BEGIN_END(prim)`,
then either `DRAW_ARRAYS`, `ARRAY_ELEMENT16/32`, or `INLINE_ARRAY`, then
`SET_BEGIN_END(END)` which flushes the primitive. `END` (`0x17FC`=0) is the trigger to
actually rasterize.

### 5.10 Notify, semaphores, reports

| Method | Offset | Notes |
|--------|-------:|-------|
| `SET_SEMAPHORE_OFFSET` | `0x1D6C` | offset into semaphore DMA |
| `BACK_END_WRITE_SEMAPHORE_RELEASE` | `0x1D70` | write value to semaphore after backend done |
| `CLEAR_REPORT_VALUE` | `0x17C8` | reset occlusion counter |
| `SET_ZPASS_PIXEL_COUNT_ENABLE` | `0x17CC` | enable occlusion query |
| `GET_REPORT` | `0x17D0` | `OFFSET 0x00FFFFFF`, `TYPE 0xFF000000` (write counter+timestamp to report DMA) |

---

## 6. Surface & texture formats; scanout

### 6.1 Color surface formats (from `SET_SURFACE_FORMAT.COLOR`)

| Code | Format | bpp |
|-----:|--------|----:|
| 1 | X1R5G5B5_Z1R5G5B5 | 16 |
| 3 | R5G6B5 | 16 |
| 4 | X8R8G8B8_Z8R8G8B8 | 32 |
| 5 | X8R8G8B8_O8R8G8B8 | 32 |
| 7 | X1A7R8G8B8_O1A7R8G8B8 | 32 |
| 8 | A8R8G8B8 | 32 |

Stored little-endian ARGB. Depth (`ZETA`): `Z16` (16-bit) or `Z24S8` (24-bit depth +
8-bit stencil). Surfaces are **either pitch (linear)** or **swizzled** per
`SET_SURFACE_FORMAT.TYPE`.

### 6.2 Linear vs swizzled

- **Linear / pitch**: `addr = base + y*pitch + x*bpp`. Pitch from `SET_SURFACE_PITCH`.
- **Swizzled**: texels stored in **Morton (Z-order)** layout. Address is formed by
  interleaving the bits of X and Y (and Z for 3D). For a 2D texture, interleave
  `x` and `y` bit-by-bit (`...y1 x1 y0 x0`) then multiply by bpp. Non-power-of-two and
  non-square sizes interleave only up to the smaller dimension's bit count, then append the
  remaining major-axis bits linearly. The `BASE_SIZE_U/V/P` log2 fields in
  `SET_TEXTURE_FORMAT` give the dimensions for the swizzle. (See `nv2a-trace/Texture.py`
  for a reference unswizzle.)

The framebuffer the title scans out is normally **linear** (pitch) ARGB; textures are
frequently swizzled or DXT-compressed.

### 6.3 Texture formats (`SET_TEXTURE_FORMAT.COLOR`, the `0xFF00` field >>8)

Common codes: `Y8=0x00`, `A1R5G5B5=0x02`, `X1R5G5B5=0x03`, `A4R4G4B4=0x04`,
`R5G6B5=0x05`, `A8R8G8B8=0x06`, `X8R8G8B8=0x07`, `I8_A8R8G8B8=0x0B` (palettized),
**`DXT1=0x0C`**, **`DXT3=0x0E`**, **`DXT5=0x0F`**, and `IMAGE_*` (`0x10`+) variants that
are the **linear** (non-swizzled) counterparts (e.g. `IMAGE_A8R8G8B8=0x12`,
`IMAGE_R5G6B5=0x11`). The non-`IMAGE_` codes are swizzled.

DXT sizing: DXT1 = `w*h/2` bytes (4 bpp), DXT3/DXT5 = `w*h` bytes (8 bpp). Decode each 4×4
block to RGBA before sampling (or pass through to the host GPU's BC1/2/3).

### 6.4 Scanout (display)

The display is driven by **PCRTC** + **PRAMDAC**:

- `PCRTC_START` (`PCRTC` base + `0x800`, abs `0xFD600800`) — physical RAM address of the
  framebuffer currently scanned out. The VBlank ISR writes the chosen back buffer here to
  flip.
- `PCRTC_CONFIG` (`+0x804`), `PCRTC_RASTER` (`+0x808`) — timing.
- `PCRTC_INTR_0` (`+0x100`) `VBLANK` (b0) / `PCRTC_INTR_EN_0` (`+0x140`) — VBlank interrupt
  (this is `PMC_INTR_0` bit 24).
- **PRAMDAC** PLLs set the pixel/mem/core clocks: `NVPLL_COEFF 0x500`, `MPLL_COEFF 0x504`,
  `VPLL_COEFF 0x508` (each `MDIV 0xFF`, `NDIV 0xFF00`, `PDIV 0x70000`). For emulation you
  mostly ignore the analog timing and just present the framebuffer at `PCRTC_START` with
  the surface format/pitch.

**Emulator present path**: on VBlank (your frame boundary), read `PCRTC_START`, interpret
the bytes there as the active color format/pitch/resolution (from the last
`SET_SURFACE_*` or from PRAMDAC FP timing), convert to RGBA8, and blit to the host
window.

---

## 7. Interrupts & notifiers

### 7.1 Interrupt flow

1. An engine raises a condition and sets its `*_INTR_0` bit (e.g. `PGRAPH_INTR_0`,
   `PFIFO_INTR_0.DMA_PUSHER`, `PCRTC_INTR_0.VBLANK`).
2. If the matching `*_INTR_EN_0` bit is set, the engine bit appears in `PMC_INTR_0`.
3. If `PMC_INTR_EN_0.HARDWARE` is set, the GPU asserts its PCI IRQ line to the CPU.
4. The CPU ISR reads `PMC_INTR_0`, finds the engine, reads that engine's `INTR_0`, services
   it, then **acks by writing the bits back (write-1-to-clear)** to the engine's `INTR_0`.
   `PMC_INTR_0` then clears for that engine automatically.

The emulator must implement write-1-to-clear semantics on every `*_INTR_0`, and re-derive
`PMC_INTR_0` from the per-engine pending&enabled state on every read.

### 7.2 PGRAPH errors

PGRAPH signals problems via `PGRAPH_INTR_0` with an `NSOURCE` register describing the
cause (e.g. `NOTIFICATION`, illegal method, protection). An unhandled method should set
the error bit and (per real HW) **stall** until the driver acks — but for bring-up it is
usually better to log-and-continue so a single unimplemented method doesn't deadlock.

### 7.3 The notifier mechanism

A **notifier** is a small structure in RAM (via `SET_CONTEXT_DMA_NOTIFIES`) that the GPU
writes to signal completion of an operation. `SET_NOTIFY` + the operation cause PGRAPH to
write a **16-byte notification block**: an 8-byte timestamp (PTIMER value) followed by a
result/status word (0 = success). The driver polls the status word (or waits on the
PGRAPH interrupt) to know the op finished. This is how `D3DDevice_BlockUntilIdle` and
fence-style sync work.

**Semaphores/reports** are the higher-throughput equivalent:
`BACK_END_WRITE_SEMAPHORE_RELEASE` writes a value to the semaphore DMA when the backend
drains; `GET_REPORT` writes an occlusion count + timestamp to the report DMA. Implement
both as "write the value/struct to the resolved RAM address when the batch completes."

---

## 8. Emulation roadmap

Implement in this order; each step is independently testable.

1. **MMIO decode + register backing store.** Range-decode `0xFD000000+` into the engine
   windows (section 1). Back every register with storage; implement `PMC_INTR_0`
   derivation, write-1-to-clear on `*_INTR_0`, and `PMC_ENABLE` gating. Stub PRAMDAC PLLs
   to return "locked".

2. **PFIFO bring-up + pusher.** Implement `CACHE1_DMA_*`, `USER_DMA_PUT/GET`. On a write to
   `USER_DMA_PUT`, run the pusher (section 3.4) to parse command headers and emit
   `(subc,method,data)`. **Make the drain busy-wait terminate**: advance `DMA_GET` to
   `DMA_PUT` and keep `PGRAPH_STATUS` readable as idle. This alone gets a title past
   "init the GPU and wait for the FIFO."

3. **Puller + RAMHT + SET_OBJECT.** Resolve object handles to classes; bind `0x97` to a
   subchannel; dispatch other methods to a PGRAPH method table. Accept `NO_OPERATION`,
   `WAIT_FOR_IDLE`, all `SET_CONTEXT_DMA_*`, and `SET_SURFACE_*` (store state).

4. **Clear.** Implement `SET_COLOR/ZSTENCIL_CLEAR_VALUE` + `CLEAR_SURFACE` over the clip/
   clear rect into the color/zeta surface in RAM. Most titles clear every frame — this is
   the first visible output.

5. **Display / flip.** Implement `PCRTC_START`, VBlank interrupt, and the flip methods
   (`SET_FLIP_WRITE/READ`, `FLIP_INCREMENT_WRITE`, `FLIP_STALL`). Present the framebuffer
   at `PCRTC_START`. Now a cleared screen actually shows; `FLIP_STALL` must release on your
   VBlank or the title hangs.

6. **Simple draws.** Surface setup, viewport, `SET_BEGIN_END` + `DRAW_ARRAYS`/
   `INLINE_ARRAY`/`ARRAY_ELEMENT`, vertex-array formats. Start with fixed-function
   transform using `SET_COMPOSITE_MATRIX` (or model_view×projection) and flat/Gouraud
   color. Handle `QUADS`/`TRIANGLE_FAN` by converting to triangles. Implement depth test,
   blend, cull, color mask.

7. **Textures.** `SET_TEXTURE_*`: linear first, then swizzle (Morton), then DXT1/3/5.
   Texcoords, wrap modes, filters. Wire one combiner stage to "modulate texture × diffuse."

8. **Register combiners (full).** Implement the ICW/OCW/final-combiner evaluator so
   multi-texture/lighting effects render correctly.

9. **Vertex shaders.** `SET_TRANSFORM_PROGRAM` microcode: translate the NV2A vertex-shader
   ISA to host (recompile to a host vertex shader or interpret). Needed for most retail 3D.

10. **Sync correctness.** Notifiers, semaphores, `GET_REPORT` occlusion queries, accurate
    `PGRAPH_STATUS`/idle timing. Required for games that fence on GPU completion.

### Known-hard parts

- **Swizzle/Morton addressing** for non-square, non-power-of-two, and mipmapped textures,
  and swizzled render targets that are later sampled.
- **Register combiners → host shader** translation (the final combiner's special inputs,
  per-stage mappings, signed/clamp modes) and **vertex-shader ISA** recompilation.
- **Surface aliasing**: the same RAM is used as a render target then sampled as a texture
  (and CPU-touched). Needs surface tracking / readback / dirty management.
- **Sync timing**: titles busy-wait on `DMA_GET`, `PGRAPH_STATUS`, notifiers, and VBlank.
  If any of these never reaches the expected state the title deadlocks. Get these
  externally-observable values right even if internals are simplified.
- **Z24S8 / depth formats** and `Z_PERSPECTIVE`/W-buffering details, plus float-depth
  variants.
- **FLIP_STALL / triple buffering** timing relative to VBlank.
