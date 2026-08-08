# Guitar Practice Trainer — All-Fourths Tuning

A practice app for an electric guitar tuned **all-fourths** (E2 A2 D3 G3 C4 F4):
every adjacent string is a perfect fourth, so chord/scale shapes are movable and
identical at every root. The engine calls random prompts (notes, chords, scales,
modes, progressions, licks, pieces), listens to the guitar via the device mic,
scores whether the player hit the expected pitches, and auto-advances when the
per-prompt countdown expires (or immediately on a pass).

It runs as a **TUI on macOS** and as a **native Compose app on Android**,
sharing one real-time audio + pitch core (`guitar_trainer_core`).

## Repository layout

```
Cargo.toml                 workspace (crates/core, crates/tui, tools/bindgen-cli)
rust-toolchain.toml        pinned stable
.cargo/config.toml         Android NDK linker fallbacks
Makefile                    tui / test / bindings / android / android-setup
crates/core/                guitar_trainer_core — pure-Rust audio+pitch+engine
  src/note.rs  tuning.rs    MIDI note model + all-fourths fret arithmetic
  src/music.rs               chord/scale/mode intervals + fret_voicing generators
  src/challenges.rs          prompt generation (the acceptance rule lives here)
  src/progressions.rs ...    content library (progressions/licks/pieces/TOML)
  src/pitch.rs               vendored YIN pitch detection
  src/audio.rs               cpal mic capture + detection worker
  src/engine.rs              timer / match / score state machine
  src/config.rs              TOML config persistence
  src/ffi.rs                 UniFFI surface (FfiConfig, FfiError, create_engine)
  uniffi.toml                Kotlin package `dev.guitartrainer`
crates/tui/                 guitar_trainer_tui — ratatui/crossterm Mac TUI
tools/bindgen-cli/           tiny UniFFI 0.28 library-mode Kotlin generator
android/                     Compose app + Gradle (consumes the cdylib via JNA)
```

## Build & run (macOS)

```sh
make tui      # build & run the terminal UI
make test     # whole-workspace unit tests
```

TUI keys: `Space` start/stop, `n` skip prompt, `s` settings, `q` quit.
Settings: `1`–`7` toggle categories, type digits + `Enter` for the timer,
`r` random, `d` audio-device picker, `p` custom-content path.

## Android

See [`android/README.md`](android/README.md). One-time NDK setup, then
`make android`. (The Kotlin bindings regenerate via `make bindings`.)

## Verification status

| Step | Result |
|------|--------|
| **1. Pitch path** (`pitch.rs` unit tests) | ✅ E2 (82.41 Hz) → ≥80 Hz, A4 (440) → 438–442 Hz, silence → None, @48 kHz. |
| **2. Music theory** | ✅ Note round-trips (`69→"A4"`, `40→"E2"`, `65→"F4"`), `string_midi(0,5)=45`, voicings in range on valid frets, interval literals match spec, I–IV–V–I in C → C F G C. |
| **3. Content** | ✅ Bundled non-empty; custom TOML parses + merges (override-by-name); malformed → `Err` (no panic). |
| **4. Engine (seeded)** | ✅ Note prompt: Matched→Passed→new Prompt; timeout→Timeout→new Prompt; random mode durations span `[10s,90s]`; ordered scale rejects out-of-order pitches. |
| **5. TUI smoke (Mac)** | ✅ Builds & launches; renders prompt/timer gauge/detected/status; `q` quits cleanly (exit 0). |
| **6. Android smoke (on-device)** | ⏸ Not run — no Android SDK/NDK in this environment. Gradle/Compose/Kotlin scaffold complete; `make bindings` generates Kotlin from the cdylib. |
| **7. Random mode** | ✅ Covered by the engine unit test (durations visibly vary across 16 draws). |

All 46 `cargo test` tests pass; `cargo build -p guitar_trainer_core --features uniffi` and `-p guitar_trainer_tui` both compile clean (warnings only).

## Monophonic evaluation (v1 scope)

Chords = play each constituent note once (set completion); progressions = chord
roots in order; scales/modes/licks/pieces = ordered sequences. Polyphonic
"strum a chord and detect all notes from one mic" is explicitly out of v1.

## License

MIT.