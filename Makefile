# Guitar Practice Trainer — build orchestration.
#
# Targets:
#   make tui        Build & run the Mac TUI.
#   make test       Run the whole workspace test suite.
#   make bindings   (Re)generate UniFFI Kotlin bindings into android/.
#   make android     Build the Android `.so`s + Kotlin bindings, then assemble the APK.
#   make android-setup   Print the toolchain/NDK setup steps (one-time).
#
# Requires Rust (rustup) on PATH. `android` additionally needs the Android SDK,
# NDK, `cargo-ndk`, and `uniffi-bindgen` (see `android-setup`).

CARGO ?= cargo
NDK_TARGETS ?= arm64-v8a armeabi-v7a x86_64
# Android AAudio (cpal backend) requires API level >= 26.
ANDROID_API ?= 26
UNIFFI_BINDGEN ?= uniffi-bindgen
BINDINGS_OUT := android/app/src/main/java/dev/guitartrainer/bindings

.PHONY: tui test bindings android android-setup check fmt clean

tui:
	$(CARGO) run -p guitar_trainer_tui --release

test:
	$(CARGO) test --workspace

bindings:
	@mkdir -p $(BINDINGS_OUT)
	$(CARGO) build -p guitar_trainer_core --features uniffi --lib
	# UniFFI 0.28 ships no standalone CLI; use the in-tree bindgen helper.
	$(CARGO) run -p gtt-bindgen-cli --release -- \
		target/debug/libguitar_trainer_core.dylib $(BINDINGS_OUT)

android-setup:
	@echo "# 1. Install Android targets:"
	@echo "    rustup target add $(NDK_TARGETS)"
	@echo "# 2. Install cargo-ndk + uniffi-bindgen:"
	@echo "    cargo install cargo-ndk uniffi-bindgen-cli"
	@echo "# 3. Install Android Studio (SDK + NDK side-by-side)."
	@echo "#    Set ANDROID_HOME and NDK_HOME; the linkers in .cargo/config.toml"
	@echo "#    must match your installed NDK toolchain version."

android:
	cargo ndk $(foreach t,$(NDK_TARGETS),-t $(t)) -P $(ANDROID_API) -o android/app/src/main/jniLibs build --release -p guitar_trainer_core --features uniffi
	$(MAKE) bindings
	cd android && ./gradlew assembleDebug

check:
	$(CARGO) check --workspace

fmt:
	$(CARGO) fmt --all

clean:
	$(CARGO) clean
	rm -rf android/app/build