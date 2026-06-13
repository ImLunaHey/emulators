# gba-core-ffi

C-ABI wrapper around [`../core`](../core) (`gba_core::Gba`) for **native mobile
targets** — primarily React Native. It is **not yet wired into an app**; this
crate exists so the core builds and links for iOS/Android today, ahead of the RN
port.

## Why a separate crate?

React Native's JS engine (Hermes) has **no WebAssembly support**, so the wasm
bundle the web app uses can't run on device. Instead RN links a *native* library
and calls it over a JSI / TurboModule bridge. The pure-Rust core stays
binding-agnostic; the web target keeps using `../core` through `wasm-bindgen`,
and this crate is its second consumer for native.

Standalone (not a Cargo workspace member) on purpose: building it never disturbs
`wasm-pack build core`.

## API

A flat C ABI over an opaque `GbaCore` handle — see
[`include/gba_core_ffi.h`](include/gba_core_ffi.h) and `src/lib.rs`. The
framebuffer is exposed as a pointer+len so the host can hand JS a **zero-copy**
JSI `ArrayBuffer` (mirrors the wasm `framebuffer_ptr` path) — don't marshal it
through the legacy RN bridge.

Typical session: `gba_new` → per frame `gba_set_keys` / `gba_run_frame` / read
`gba_framebuffer_ptr` + `gba_framebuffer_len` → `gba_free`.

## Building

iOS (produces `build/GbaCore.xcframework`):

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
../scripts/build-ios.sh
```

Android (produces `build/jniLibs/<abi>/libgba_core_ffi.so`):

```sh
cargo install cargo-ndk
rustup target add aarch64-linux-android armv7-linux-androideabi \
    x86_64-linux-android i686-linux-android
export ANDROID_NDK_HOME=/path/to/android-ndk
../scripts/build-android.sh
```

Host smoke build (no mobile toolchain needed):

```sh
cargo build --release --manifest-path core-ffi/Cargo.toml
```
