package dev.guitartrainer.app

import dev.guitartrainer.Engine
import dev.guitartrainer.createEngine
import dev.guitartrainer.FfiConfig
import dev.guitartrainer.EngineListener

/**
 * Thin shim over the generated UniFFI bindings.
 *
 * The native library is named `libguitar_trainer_core.so` and is loaded
 * automatically by UniFFI (via JNA) once the jniLibs are packaged into the APK.
 * Build the `.so`s with `make android` (cargo-ndk) before installing.
 */
object Native {
    fun createEngine(config: FfiConfig, listener: EngineListener): Engine =
        dev.guitartrainer.createEngine(config, listener)
}