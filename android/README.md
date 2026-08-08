# Guitar Practice Trainer — Android app

A Jetpack Compose front-end that consumes the shared Rust core
(`guitar_trainer_core`) through UniFFI-generated Kotlin bindings.

> **Build status on this machine:** the Rust core + UniFFI Kotlin bindings
> build and generate successfully (`make bindings` writes
> `app/src/main/java/dev/guitartrainer/bindings/…/guitar_trainer_core.kt`).
> The APK itself is **not built here** because the Android SDK + NDK are not
> installed in this environment. The Gradle/Compose/Kotlin scaffold below is
> complete; follow the one-time setup, then `make android`.

## One-time setup

```sh
# 1. Rust Android targets + cargo-ndk
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk

# 2. Android Studio → SDK Manager: install
#      - Android SDK Platform 34
#      - NDK (Side by side)
#    Set environment vars (adjust paths/versions):
export ANDROID_HOME="$HOME/Library/Android/sdk"
export NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"

# 3. Gradle wrapper (one-time; the wrapper jar is not checked in here):
cd android && gradle wrapper --gradle-version 8.9 && cd ..
```

The linkers in `.cargo/config.toml` assume NDK API level 24
(`*-android24-clang`). If your NDK uses a different API level, edit those
lines or let `cargo-ndk` manage them (recommended — it sets the linkers
automatically and the `.cargo/config.toml` entries are only a fallback).

## Build the APK

From the repo root:

```sh
make android     # cross-compiles .so's, regenerates Kotlin bindings, assembles the APK
```

Or by hand:

```sh
# Native libs for each ABI → android/app/src/main/jniLibs/<abi>/
cargo ndk -t aarch64-linux-android -t armv7-linux-androideabi -t x86_64-linux-android \
  -o android/app/src/main/jniLibs build --release -p guitar_trainer_core --features uniffi

# Kotlin bindings (UniFFI library mode; uses the in-tree gtt-bindgen-cli helper)
make bindings

# APK
cd android && ./gradlew assembleDebug
```

Install on a device:

```sh
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

## Runtime

- The app requests `RECORD_AUDIO` at runtime. Deny → prompts still cycle but
  scoring is disabled (no detected-note updates).
- `onPause` calls `engine.ffiStop()` to release the mic and save battery; the
  next `onResume`/Start re-opens it.

## Gradle ↔ cargo wiring note

UniFFI binding generation runs **outside Gradle** (via `make bindings`) rather
than as an in-Gradle task. This is the plan's documented fallback for the
(most uncertain) Gradle↔cargo plumbing: it keeps the build reproducible and
avoids flaky in-Gradle `cargo` invocations. The generated
`guitar_trainer_core.kt` may be checked in if you want Gradle-only builds.