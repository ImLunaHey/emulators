# Xbox homebrew demo XBEs (NV2A bring-up test content)

Real, freely-licensed third-party XBEs used to validate the core against
**actual** game behavior — they submit real NV2A pushbuffers, unlike the
synthetic `examples/make_test_xbe.rs`. Graded for bring-up:

| file | exercises | purpose |
|---|---|---|
| `hello.xbe` | CPU + HLE kernel only (no GPU) | confirm a real XBE survives init |
| `triangle.xbe` | one NV2A pushbuffer triangle | the minimal real version of our scanout path: `AvSetDisplayMode` → pushbuffer → `nv2a_render` rasterize → `nv2a.scanout` → display |

## Origin & license

Built from the **nxdk** samples (`samples/hello`, `samples/triangle`) —
the open-source Xbox homebrew SDK: <https://github.com/XboxDev/nxdk>.
nxdk and its samples are freely redistributable (see `LICENSES/` in nxdk:
CC0-1.0 / MIT / Apache-2.0 / NCSA). These are NOT retail content.

## Rebuilding

```sh
brew install llvm lld cmake            # macOS toolchain prereqs
git clone --recursive https://github.com/XboxDev/nxdk
cd nxdk && ./bootstrap
export NXDK_DIR=$PWD PATH="$NXDK_DIR/bin:/opt/homebrew/opt/llvm/bin:$PATH"
(cd samples/hello    && make)          # -> bin/default.xbe  == hello.xbe
(cd samples/triangle && make)          # -> bin/default.xbe  == triangle.xbe
```

## Run through the core

```sh
cargo run --release --manifest-path core-xbox/Cargo.toml \
  --example boot_probe -- core-xbox/demos/triangle.xbe 60
```

## Current status

Both XBEs boot and run their real code (53 → tens of millions of instructions).
The CRT-init + video-init seams have been implemented (NtCreateMutant, real
SHA-1, Interlocked atomics, IRQL, TLS, time exports, symlink objects, disk
IOCTLs, events, RDMSR, the AV-encoder GET_SETTINGS reply, and the NV2A GPU
PLL/PFB config registers).

`triangle.xbe` now:
- passes `pb_init` and calls **`AvSetDisplayMode`**;
- drives the **NV2A scanout with its own framebuffer** — the host surface flips
  from the boot diagnostic (`#060E06`) to the game's cleared screen (`#000000`);
- **pushes ~237 real NV2A pushbuffer methods** (Kelvin pipeline setup) through
  the PFIFO/PGRAPH.

Remaining to a visible triangle: the full Kelvin **vertex/draw pipeline**
(`BEGIN_END` + vertex arrays → `nv2a_render` rasterize) and the **double-buffer
flip** (PCRTC_START update). That's focused NV2A PGRAPH work — the scanout +
rasterizer are now proven reachable by real third-party geometry.
