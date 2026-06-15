#!/usr/bin/env bash
# Build the unified native static lib (emu-native, all cores) for macOS and sync
# the C header into the SwiftPM app's C target. Run this before `swift build` /
# `swift run` in apps/EmuApp.
#
#   ./scripts/build-macos.sh            # host arch only (fast; for `swift run`)
#   ./scripts/build-macos.sh --universal  # arm64 + x86_64 fat archive
#
# Sibling of build-ios.sh (which builds the GBA-only xcframework for iOS).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FFI="$ROOT/core-native"
APP="$ROOT/apps/EmuApp"
LIB="libemu_native.a"

universal=0
[[ "${1:-}" == "--universal" ]] && universal=1

if [[ "$universal" == 1 ]]; then
  echo "==> building emu-native (arm64 + x86_64 universal)…"
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  ( cd "$FFI" && cargo build --release --target aarch64-apple-darwin )
  ( cd "$FFI" && cargo build --release --target x86_64-apple-darwin )
  mkdir -p "$FFI/target/release"
  lipo -create \
    "$FFI/target/aarch64-apple-darwin/release/$LIB" \
    "$FFI/target/x86_64-apple-darwin/release/$LIB" \
    -output "$FFI/target/release/$LIB"
  echo "    universal archive -> $FFI/target/release/$LIB"
else
  echo "==> building emu-native (host arch)…"
  ( cd "$FFI" && cargo build --release )
fi

# Sync the canonical C header into the Swift C target so the package builds
# standalone (SwiftPM requires headers inside the target directory).
mkdir -p "$APP/Sources/CEmuNative/include"
cp "$FFI/include/emu_native.h" "$APP/Sources/CEmuNative/include/emu_native.h"

echo "==> done."
echo "    lib:    $FFI/target/release/$LIB"
echo "    run it: cd apps/EmuApp && swift run EmuApp"
