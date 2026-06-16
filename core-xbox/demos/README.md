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

## Current stop point (as of this commit)

Both XBEs boot (`boot_xbe = true`) but stop at **`STOP KERNEL 192
(NTCREATEMUTANT)` after 53 instructions** — nxdk's CRT calls `NtCreateMutant`
during startup, before any graphics. So `triangle` does not yet reach the
scanout path; the punch list to a first rendered frame begins with
`NtCreateMutant` (192) and the CRT/graphics-init seams that follow it.
