#!/usr/bin/env bash
# Build the GBA core as Android shared libraries (.so per ABI) for the (future)
# React Native module.
#
# Output: packages/ffi/build/jniLibs/<abi>/libgba_core_ffi.so for each ABI. Point
# the RN app's android `sourceSets ... jniLibs.srcDirs` at that folder and call
# the `gba_*` C ABI over JNI from the Kotlin TurboModule.
#
# Prereqs (one-time):
#   cargo install cargo-ndk
#   rustup target add aarch64-linux-android armv7-linux-androideabi \
#       x86_64-linux-android i686-linux-android
#   export ANDROID_NDK_HOME=/path/to/android-ndk  (r25+)
set -euo pipefail

cd "$(dirname "$0")/.."
FFI=packages/ffi
OUT=$FFI/build/jniLibs

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to your Android NDK path}"

echo "==> building Android ABIs via cargo-ndk"
cargo ndk \
  -t arm64-v8a \
  -t armeabi-v7a \
  -t x86_64 \
  -t x86 \
  -o "$OUT" \
  build --release --manifest-path $FFI/Cargo.toml

echo "==> done: $OUT/<abi>/libgba_core_ffi.so"
