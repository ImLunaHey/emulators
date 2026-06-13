#!/usr/bin/env bash
# Build the GBA core as an iOS xcframework for the (future) React Native module.
#
# Output: core-ffi/build/GbaCore.xcframework, bundling the device slice and a
# fat simulator slice (arm64 + x86_64), plus the C header. Drop the xcframework
# into the RN app's iOS project and call the `gba_*` C ABI from the Swift
# TurboModule.
#
# Prereqs (one-time):
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
#   (Xcode command line tools for lipo / xcodebuild)
set -euo pipefail

cd "$(dirname "$0")/.."
FFI=core-ffi
LIB=libgba_core_ffi.a
OUT=$FFI/build
mkdir -p "$OUT"

echo "==> building device (aarch64-apple-ios)"
cargo build --release --manifest-path $FFI/Cargo.toml --target aarch64-apple-ios

echo "==> building simulator slices (aarch64 + x86_64)"
cargo build --release --manifest-path $FFI/Cargo.toml --target aarch64-apple-ios-sim
cargo build --release --manifest-path $FFI/Cargo.toml --target x86_64-apple-ios

echo "==> lipo simulator slices into one fat archive"
mkdir -p "$OUT/sim"
lipo -create \
  "$FFI/target/aarch64-apple-ios-sim/release/$LIB" \
  "$FFI/target/x86_64-apple-ios/release/$LIB" \
  -output "$OUT/sim/$LIB"

echo "==> assembling xcframework"
rm -rf "$OUT/GbaCore.xcframework"
xcodebuild -create-xcframework \
  -library "$FFI/target/aarch64-apple-ios/release/$LIB" -headers "$FFI/include" \
  -library "$OUT/sim/$LIB" -headers "$FFI/include" \
  -output "$OUT/GbaCore.xcframework"

echo "==> done: $OUT/GbaCore.xcframework"
