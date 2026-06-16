#!/usr/bin/env bash
# Build the libretro core and install it into RetroArch (macOS).
#   ./scripts/build-libretro.sh
# Then in RetroArch: Load Core → "imlunahey emulator", Load Content → a ROM.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
( cd "$ROOT/packages/libretro" && cargo build --release )
DYLIB="$ROOT/packages/libretro/target/release/libemu_libretro.dylib"
CORES="$HOME/Library/Application Support/RetroArch/cores"
mkdir -p "$CORES"
cp "$DYLIB" "$CORES/emu_libretro.dylib"
# Ad-hoc sign so RetroArch can dlopen it, and clear quarantine.
codesign -s - -f "$CORES/emu_libretro.dylib" >/dev/null 2>&1 || true
xattr -d com.apple.quarantine "$CORES/emu_libretro.dylib" 2>/dev/null || true
echo "installed -> $CORES/emu_libretro.dylib"
