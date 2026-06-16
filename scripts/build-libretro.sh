#!/usr/bin/env bash
# Build the libretro core and install it into RetroArch (macOS).
#   ./scripts/build-libretro.sh
# Then in RetroArch: Load Core → "imlunahey emulator", Load Content → a ROM.
#
# Builds a UNIVERSAL (arm64 + x86_64) dylib so it loads in both Apple-silicon
# and Intel RetroArch builds (the Homebrew cask is Intel-only, run via Rosetta).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LR="$ROOT/packages/libretro"
NAME="libemu_libretro.dylib"

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true
( cd "$LR" && cargo build --release --target aarch64-apple-darwin )
( cd "$LR" && cargo build --release --target x86_64-apple-darwin )

CORES="$HOME/Library/Application Support/RetroArch/cores"
mkdir -p "$CORES"
lipo -create \
  "$LR/target/aarch64-apple-darwin/release/$NAME" \
  "$LR/target/x86_64-apple-darwin/release/$NAME" \
  -output "$CORES/emu_libretro.dylib"

# Ad-hoc sign so RetroArch can dlopen it, and clear quarantine.
codesign -s - -f "$CORES/emu_libretro.dylib" >/dev/null 2>&1 || true
xattr -d com.apple.quarantine "$CORES/emu_libretro.dylib" 2>/dev/null || true

echo "installed universal core -> $CORES/emu_libretro.dylib"
lipo -archs "$CORES/emu_libretro.dylib"
