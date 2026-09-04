//! Custom (user-defined) guitar tunings, loaded from a TOML file separate
//! from `config.toml` — mirrors `crate::content`'s custom-content pattern.
//! The three built-in tunings in `crate::tuning::TuningId` are never read,
//! parsed, or modified by anything in this module; a custom tuning is
//! purely additive data merged in by name at runtime.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::note::{Note, MIDI_MAX, MIDI_MIN};
use crate::tuning::{Tuning, TuningId, FRET_COUNT};

/// A user-defined tuning: a unique display name + six open-string MIDI
/// notes, low → high (same shape as `TuningId::open_strings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomTuning {
    pub name: String,
    pub open_strings: [u8; 6],
}

impl CustomTuning {
    pub fn to_tuning(&self) -> Tuning {
        Tuning {
            label: self.name.clone(),
            open_strings: self.open_strings,
        }
    }
}

/// Which tuning is active: one of the three built-ins, or a named entry
/// from the custom-tuning file. `#[serde(untagged)]` with `Builtin` listed
/// first keeps existing `config.toml` files (`tuning = "all_fourths"`)
/// parsing byte-identically — that value matches `TuningId`'s own
/// snake_case string form before `Custom` is ever tried, and a `Builtin`
/// value serializes back out as that same bare string (untagged newtype
/// variants serialize transparently).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActiveTuning {
    Builtin(TuningId),
    Custom { name: String },
}

impl Default for ActiveTuning {
    fn default() -> Self {
        ActiveTuning::Builtin(TuningId::default())
    }
}

/// Resolve a selection against the loaded custom tunings. Falls back to the
/// default built-in tuning (logging a warning) if `Custom{name}` no longer
/// matches anything in `custom_tunings` — e.g. the file was edited or the
/// path changed after the selection was saved.
pub fn resolve_tuning(active: &ActiveTuning, custom_tunings: &[CustomTuning]) -> Tuning {
    match active {
        ActiveTuning::Builtin(id) => Tuning::builtin(*id),
        ActiveTuning::Custom { name } => custom_tunings
            .iter()
            .find(|t| &t.name == name)
            .map(CustomTuning::to_tuning)
            .unwrap_or_else(|| {
                tracing::warn!("custom tuning '{name}' not found; falling back to default");
                Tuning::builtin(TuningId::default())
            }),
    }
}

// ---------------------------------------------------------------------------
// TOML schema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CustomTuningFile {
    #[serde(default)]
    tunings: Vec<CustomTuningEntry>,
}

#[derive(Deserialize)]
struct CustomTuningEntry {
    name: String,
    strings: [String; 6],
}

/// Parse a custom-tuning TOML file from disk.
pub fn load_custom_tunings(path: &Path) -> Result<Vec<CustomTuning>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading custom tunings {}", path.display()))?;
    parse_custom_tunings(&text)
}

/// Parse + validate custom-tuning TOML text (split out for testability).
///
/// Validation (every failure names the offending tuning/string so a user
/// authoring the file gets an actionable error):
/// - `name` non-empty and unique within the file.
/// - Each of the 6 `strings` entries parses via `Note::from_name`
///   (scientific pitch notation, e.g. `"D2"`, `"F#3"`, `"Bb3"`).
/// - Strings are strictly ascending low → high (every fret-arithmetic
///   function in `music.rs`/`tuning.rs` assumes this).
/// - Adjacent strings are ≤ `FRET_COUNT` (22) semitones apart, so
///   `Tuning::range` has no unreachable gaps — mirrors the invariant
///   `tuning.rs`'s `range_has_no_unreachable_gaps` test already pins for
///   the 3 built-ins.
/// - Every reachable note (open string .. top string + `FRET_COUNT`) stays
///   within `note::MIDI_MIN..=note::MIDI_MAX` (D2..F6) — this app's
///   supported instrument range, which every other code path (pitch
///   matching, `Note` construction) already assumes globally.
pub fn parse_custom_tunings(text: &str) -> Result<Vec<CustomTuning>> {
    let file: CustomTuningFile = toml::from_str(text).context("parsing custom tunings")?;
    let mut out: Vec<CustomTuning> = Vec::with_capacity(file.tunings.len());
    for entry in file.tunings {
        if entry.name.trim().is_empty() {
            anyhow::bail!("custom tuning has an empty name");
        }
        if out.iter().any(|t| t.name == entry.name) {
            anyhow::bail!("duplicate custom tuning name '{}'", entry.name);
        }
        let mut open_strings = [0u8; 6];
        for (i, s) in entry.strings.iter().enumerate() {
            let note = Note::from_name(s).with_context(|| {
                format!(
                    "custom tuning '{}': invalid note '{}' (want e.g. \"D2\", \"F#3\")",
                    entry.name, s
                )
            })?;
            open_strings[i] = note.midi();
        }
        for w in open_strings.windows(2) {
            if w[1] <= w[0] {
                anyhow::bail!(
                    "custom tuning '{}': strings must be strictly ascending low→high ({} then {})",
                    entry.name,
                    Note::from_midi_clamped(w[0]).name(),
                    Note::from_midi_clamped(w[1]).name()
                );
            }
            if w[1] - w[0] > FRET_COUNT {
                anyhow::bail!(
                    "custom tuning '{}': adjacent strings {} and {} are more than {} frets apart",
                    entry.name,
                    Note::from_midi_clamped(w[0]).name(),
                    Note::from_midi_clamped(w[1]).name(),
                    FRET_COUNT
                );
            }
        }
        let hi = open_strings[5].saturating_add(FRET_COUNT);
        if open_strings[0] < MIDI_MIN || hi > MIDI_MAX {
            anyhow::bail!(
                "custom tuning '{}': range {}..{} falls outside the supported {}..{} (D2..F6)",
                entry.name,
                Note::from_midi_clamped(open_strings[0]).name(),
                Note::from_midi_clamped(hi).name(),
                Note::from_midi_clamped(MIDI_MIN).name(),
                Note::from_midi_clamped(MIDI_MAX).name(),
            );
        }
        out.push(CustomTuning {
            name: entry.name,
            open_strings,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_open_d() {
        let text = "[[tunings]]\nname = \"Open D\"\nstrings = [\"D2\", \"A2\", \"D3\", \"F#3\", \"A3\", \"D4\"]\n";
        let v = parse_custom_tunings(text).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "Open D");
        assert_eq!(v[0].open_strings, [38, 45, 50, 54, 57, 62]);
    }

    #[test]
    fn rejects_duplicate_names() {
        let text = "[[tunings]]\nname = \"X\"\nstrings = [\"D2\",\"A2\",\"D3\",\"G3\",\"B3\",\"D4\"]\n[[tunings]]\nname = \"X\"\nstrings = [\"D2\",\"A2\",\"D3\",\"G3\",\"B3\",\"D4\"]\n";
        assert!(parse_custom_tunings(text).is_err());
    }

    #[test]
    fn rejects_descending_strings() {
        let text =
            "[[tunings]]\nname = \"X\"\nstrings = [\"D3\",\"A2\",\"D3\",\"G3\",\"B3\",\"D4\"]\n";
        assert!(parse_custom_tunings(text).is_err());
    }

    #[test]
    fn rejects_out_of_range_low_string() {
        // B1 = MIDI 23, below MIDI_MIN (38).
        let text = "[[tunings]]\nname = \"7-string\"\nstrings = [\"B1\",\"E2\",\"A2\",\"D3\",\"G3\",\"B3\"]\n";
        assert!(parse_custom_tunings(text).is_err());
    }

    #[test]
    fn resolve_falls_back_when_custom_name_missing() {
        let active = ActiveTuning::Custom {
            name: "Nope".to_string(),
        };
        let t = resolve_tuning(&active, &[]);
        assert_eq!(t.open_strings, TuningId::default().open_strings());
    }

    #[test]
    fn resolve_finds_custom_by_name() {
        let custom = CustomTuning {
            name: "Open D".to_string(),
            open_strings: [38, 45, 50, 54, 57, 62],
        };
        let active = ActiveTuning::Custom {
            name: "Open D".to_string(),
        };
        let t = resolve_tuning(&active, &[custom]);
        assert_eq!(t.open_strings, [38, 45, 50, 54, 57, 62]);
    }
}
