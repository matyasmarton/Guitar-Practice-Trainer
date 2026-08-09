# Guitar Practice Trainer

A practice app for guitar that calls random prompts (notes, chords, scales,
modes, progressions, licks, pieces), listens to the guitar via the device mic,
scores whether the player hit the expected pitches, and auto-advances when the
per-prompt countdown expires (or immediately on a pass). Every generated
target is a real, fretted note on whichever tuning is active — built-in or
custom.

It runs as a **TUI on macOS** and as a **native Compose app on Android**,
sharing one real-time audio + pitch core (`guitar_trainer_core`).

## Screenshots

| Settings | Practice |
|---|---|
| ![Settings screen: timer, tuning picker, custom tuning, fretboard highlight, random mode, categories, audio device, custom content path](app%20settings.png) | ![Practice screen: prompt, note targets, detected pitch, timer, session summary, live fretboard](app%20playing.png) |

## Tunings

Three built-in tunings, chosen from the Settings screen's **Select tuning**
picker: `Standard`, `All Fourths` (the default), and `Drop-D All Fourths`.
Movable chord/scale shapes hold across every root only on the two
all-fourths tunings — `Standard` has the traditional major-third break
between its G and B strings.

| Tuning | Open strings (low → high) |
|---|---|
| Standard | E2 A2 D3 G3 B3 E4 |
| All Fourths *(default)* | E2 A2 D3 G3 C4 F4 |
| Drop-D All Fourths | D2 G2 C3 F3 A#3 D#4 |

**Custom tunings**: point Settings' **Add custom tuning** row at a TOML file
of your own (see its in-app format guide) and every entry is merged into the
picker by name, alongside the 3 built-ins:

```toml
[[tunings]]
name = "Open D"
strings = ["D2", "A2", "D3", "F#3", "A3", "D4"]

[[tunings]]
name = "DADGAD"
strings = ["D2", "A2", "D3", "G3", "A3", "D4"]
```

Rules: names unique and non-empty; strings strictly ascending low → high;
adjacent strings ≤22 frets apart; every note across the full 22-fret board
must fall within the app's supported D2..F6 range. Whichever tuning is
active drives prompt generation *and* the live fretboard visualizer.

## Practice modes

Seven prompt categories, toggled independently in Settings (at least one
must stay enabled — an empty set falls back to all 7):

| Mode | What it asks for |
|---|---|
| **Note** | A single open or fretted note — the fastest way to drill raw fretboard recall. |
| **Chord** | A full chord voicing from a random root + quality; every note in the shape must sound (order doesn't matter). |
| **Scale** | A scale run from a random root, matched in ascending order. |
| **Mode** | A modal scale run from a random root, matched in ascending order. |
| **Progression** | A chord-degree progression (e.g. I–IV–V) in a random key, matched in order. |
| **Lick** | A short pre-written phrase from the content library, matched in order. |
| **Piece** | An excerpt from a longer piece in the content library, matched in order. |

By default the enabled categories cycle in the order above; **Random
mode** instead draws both the category *and* the per-prompt time window
uniformly at random on every prompt. Every target note in every mode is a
real, fretted pitch on the active tuning (built-in or custom) — chords are
voiced string-by-string, scales/modes/progressions/licks/pieces are
re-voiced degree-by-degree so nothing lands outside the fretboard's
playable range. Custom-content Licks/Pieces can be extended via **Custom
content path** in Settings, pointing at your own TOML library merged
alongside the built-ins by name.

## Fretboard visualizer

On a wide terminal (`≥140` columns), the Practice screen's right-hand
panel renders a full interactive fretboard for the active tuning: a
column per string (labelled with its open note), a row per fret from open
(`0`) through fret 22, with the traditional single/double position-marker
dots (frets 3/5/7/9/15/17/19/21 and the double-dot 12th) for a real-neck
feel. It re-renders instantly whenever the tuning changes — built-in or
custom.

Turning on **Fretboard highlight** in Settings switches it from a static
reference into a live prompt aid: every still-needed target note in the
current prompt is picked out in the accent color across every string/fret
it can be played on, and a legend below the board explains the fret-marker
dot vs. the highlight color.

## Repository layout

```
Cargo.toml                 workspace (crates/core, crates/tui, tools/bindgen-cli)
rust-toolchain.toml        pinned stable
.cargo/config.toml         Android NDK linker fallbacks
Makefile                    tui / test / bindings / android / android-setup
crates/core/                guitar_trainer_core — pure-Rust audio+pitch+engine
  src/note.rs                MIDI note model (D2..F6 playable range)
  src/tuning.rs               built-in tunings (TuningId) + fret arithmetic
  src/custom_tuning.rs         user-defined tunings: TOML parse/validate + resolve-by-name
  src/music.rs                 chord/scale/mode intervals + fret_voicing generators
  src/challenges.rs            prompt generation (the acceptance rule lives here)
  src/progressions.rs ...      content library (progressions/licks/pieces/TOML)
  src/pitch.rs                 vendored YIN pitch detection
  src/audio.rs                 cpal mic capture + detection worker
  src/engine.rs                timer / match / score state machine
  src/config.rs                TOML config persistence
  src/ffi.rs                   UniFFI surface (FfiConfig, FfiError, create_engine)
  uniffi.toml                 Kotlin package `dev.guitartrainer`
crates/tui/                 guitar_trainer_tui — ratatui/crossterm Mac TUI
tools/bindgen-cli/           tiny UniFFI 0.28 library-mode Kotlin generator
android/                     Compose app + Gradle (consumes the cdylib via JNA)
```

## Build & run (macOS)

```sh
make tui      # build & run the terminal UI
make test     # whole-workspace unit tests
```

## TUI navigation

Every screen is driven the same way: `↑`/`↓` (and `←`/`→` on the Practice
action bar) to move the selection, `Enter`/`Space` to activate, `Esc` to
back out (Settings additionally saves + persists on `Esc`). Text-entry
fields (timer seconds, custom-content path, custom-tuning path) accept
typed input and commit on `Enter`.

- **Menu** — Start Practice · Settings · Quit.
- **Practice** — the current prompt, its target-note chips, detected pitch,
  countdown, session summary, and (in a wide terminal) a live fretboard
  panel; the action bar offers Stop, Skip, and Settings.
- **Settings** — top to bottom: prompt timer, **Select tuning** (opens the
  tuning picker below), **Add custom tuning** (opens the TOML format guide
  + path editor), fretboard-highlight toggle, random mode toggle, the 7
  challenge-category toggles (Note/Chord/Scale/Mode/Progression/Lick/Piece),
  audio-device picker, custom-content path, and Back. A contextual "About"
  panel explains whichever row is highlighted.
- **Select tuning** — full-screen list of the 3 built-ins plus every loaded
  custom tuning, current selection marked `✓ current`.
- **Add custom tuning** — shows the TOML schema and rules, then lets you set
  (and immediately load) the path to your tunings file; the status line
  reports how many tunings loaded or why loading failed.

## Android

See [`android/README.md`](android/README.md). One-time NDK setup, then
`make android`. (The Kotlin bindings regenerate via `make bindings`.)
Custom tunings are a TUI-only feature — the Android app currently exposes
only the 3 built-in tunings via `FfiConfig.tuning`.

## Verification status

| Step | Result |
|------|--------|
| **1. Pitch path** (`pitch.rs` unit tests) | ✅ E2 (82.41 Hz) → ≥80 Hz, A4 (440) → 438–442 Hz, silence → None, @48 kHz. |
| **2. Music theory** | ✅ Note round-trips (`69→"A4"`, `40→"E2"`, `65→"F4"`), `string_midi(0,5)=45`, voicings in range on valid frets, interval literals match spec, I–IV–V–I in C → C F G C. |
| **3. Tunings** | ✅ 3 built-ins unchanged (perfect-fourth spacing, no unreachable gaps); custom-tuning TOML parses + validates (name uniqueness, ascending strings, ≤22-fret gaps, D2..F6 range) with actionable errors; unresolved custom selection falls back to the default tuning. |
| **4. Content** | ✅ Bundled non-empty; custom TOML parses + merges (override-by-name); malformed → `Err` (no panic). |
| **5. Engine (seeded)** | ✅ Note prompt: Matched→Passed→new Prompt; timeout→Timeout→new Prompt; random mode durations span `[10s,90s]`; ordered scale rejects out-of-order pitches; every generator stays fret-reachable for every built-in tuning. |
| **6. TUI smoke (Mac)** | ✅ Builds & launches at both narrow and wide (`≥140` cols) layouts; tuning picker, custom-tuning load/reload, and live fretboard update verified end-to-end for a custom "Open D" tuning; an out-of-range custom file is rejected with a clear error and no bogus picker entry. |
| **7. Android smoke (on-device)** | ⏸ Not run — no Android SDK/NDK in this environment. Gradle/Compose/Kotlin scaffold complete; `make bindings` generates Kotlin from the cdylib. |
| **8. Random mode** | ✅ Covered by the engine unit test (durations visibly vary across 16 draws). |

66 `cargo test -p guitar_trainer_core` tests pass; `cargo build -p guitar_trainer_core --features uniffi --lib` and `-p guitar_trainer_tui` both compile clean.

## Monophonic evaluation (v1 scope)

Chords = play each constituent note once (set completion); progressions = chord
roots in order; scales/modes/licks/pieces = ordered sequences. Polyphonic
"strum a chord and detect all notes from one mic" is explicitly out of v1.

## License

MIT.
