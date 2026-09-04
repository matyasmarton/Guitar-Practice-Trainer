//! Content library: bundled + user-supplied licks and pieces.
//!
//! Bundled items borrow from the static tables ([`crate::licks`],
//! [`crate::pieces`]); custom items are **owned** (parsed from TOML) so they
//! survive independent of statics. Merging is by name: a custom item with the
//! same name as a bundled item **overrides** it; other custom items are
//! appended. Bundled items survive only when not overridden.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::licks::{Lick, LICKS};
use crate::pieces::{midi_of, Piece, PIECES};

/// An owned lick (name + intervals relative to a root).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LickItem {
    pub name: String,
    pub intervals: Vec<i8>,
}

/// An owned piece (name + `(pitch_class, octave)` pitch pairs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PieceItem {
    pub name: String,
    pub notes: Vec<(u8, u8)>,
}

/// The merged content library the engine draws licks/pieces from.
#[derive(Clone, Debug, Default)]
pub struct ContentLibrary {
    pub licks: Vec<LickItem>,
    pub pieces: Vec<PieceItem>,
}

impl ContentLibrary {
    /// Build the library from the bundled statics.
    pub fn bundled() -> Self {
        let licks = LICKS
            .iter()
            .map(|l: &Lick| LickItem {
                name: l.name.to_string(),
                intervals: l.intervals.to_vec(),
            })
            .collect();
        let pieces = PIECES
            .iter()
            .map(|p: &Piece| PieceItem {
                name: p.name.to_string(),
                notes: p.notes.to_vec(),
            })
            .collect();
        ContentLibrary { licks, pieces }
    }

    /// Merge `custom` into `self`: per name, custom overrides; others appended.
    pub fn merge_custom(&mut self, custom: CustomContent) {
        // Override bundled by name.
        for c in &custom.licks {
            if let Some(b) = self.licks.iter_mut().find(|b| b.name == c.name) {
                *b = c.clone();
            } else {
                self.licks.push(c.clone());
            }
        }
        for c in &custom.pieces {
            if let Some(b) = self.pieces.iter_mut().find(|b| b.name == c.name) {
                *b = c.clone();
            } else {
                self.pieces.push(c.clone());
            }
        }
    }

    /// Convenience: bundled + custom loaded from a path.
    pub fn load(path: &Path) -> Result<Self> {
        let mut lib = Self::bundled();
        if path.exists() {
            let custom = load_custom(path)?;
            lib.merge_custom(custom);
        }
        Ok(lib)
    }
}

/// Custom content loaded from a TOML file (owned, before merging).
#[derive(Clone, Debug, Default)]
pub struct CustomContent {
    pub licks: Vec<LickItem>,
    pub pieces: Vec<PieceItem>,
}

// ---------------------------------------------------------------------------
// TOML schema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CustomLickFile {
    #[serde(default)]
    licks: Vec<CustomLick>,
}

#[derive(Deserialize)]
struct CustomLick {
    name: String,
    intervals: Vec<i8>,
}

#[derive(Deserialize)]
struct CustomPieceFile {
    #[serde(default)]
    pieces: Vec<CustomPiece>,
}

#[derive(Deserialize)]
struct CustomPiece {
    name: String,
    notes: Vec<[u8; 2]>,
}

/// Parse a custom-content TOML file (licks and/or pieces). A file may contain
/// either table; unknown top-level keys are ignored for forward-compat.
pub fn load_custom(path: &Path) -> Result<CustomContent> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading custom content {}", path.display()))?;
    parse_custom(&text)
}

/// Parse custom content from TOML text (split out for testability).
pub fn parse_custom(text: &str) -> Result<CustomContent> {
    let mut out = CustomContent::default();

    // Try the licks table (absent is fine).
    if let Ok(file) = toml::from_str::<CustomLickFile>(text) {
        for c in file.licks {
            if c.name.is_empty() || c.intervals.is_empty() {
                anyhow::bail!("custom lick '{}' is empty", c.name);
            }
            out.licks.push(LickItem {
                name: c.name,
                intervals: c.intervals,
            });
        }
    } else {
        // If the document has a `[[licks]]` table but failed to parse, surface
        // the error rather than silently dropping licks.
        if text.contains("[[licks]]") {
            toml::from_str::<CustomLickFile>(text).context("parsing custom licks")?;
        }
    }

    if let Ok(file) = toml::from_str::<CustomPieceFile>(text) {
        for c in file.pieces {
            if c.name.is_empty() || c.notes.is_empty() {
                anyhow::bail!("custom piece '{}' is empty", c.name);
            }
            // Validate pitch_class < 12 to fail fast on bad input.
            let mut notes = Vec::with_capacity(c.notes.len());
            for [pc, oct] in c.notes {
                if pc >= 12 {
                    anyhow::bail!(
                        "custom piece '{}' has pitch_class {} (must be 0..11)",
                        c.name,
                        pc
                    );
                }
                notes.push((pc, oct));
                let _ = midi_of(pc, oct); // sanity (no overflow expected)
            }
            out.pieces.push(PieceItem {
                name: c.name,
                notes,
            });
        }
    } else if text.contains("[[pieces]]") {
        toml::from_str::<CustomPieceFile>(text).context("parsing custom pieces")?;
    }

    if out.licks.is_empty() && out.pieces.is_empty() {
        // Distinguish "nothing to merge" from "malformed".
        // If the file had any lick/piece tables, parsing above already errored.
        // An otherwise-empty file is not an error here (no-op merge).
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_nonempty() {
        let lib = ContentLibrary::bundled();
        assert!(!lib.licks.is_empty());
        assert!(!lib.pieces.is_empty());
    }

    #[test]
    fn custom_lick_parses() {
        let toml = r#"
[[licks]]
name = "my blues run"
intervals = [0, 3, 5, 7, 10, 12]
"#;
        let c = parse_custom(toml).unwrap();
        assert_eq!(c.licks.len(), 1);
        assert_eq!(c.licks[0].name, "my blues run");
        assert_eq!(c.licks[0].intervals, vec![0, 3, 5, 7, 10, 12]);
    }

    #[test]
    fn custom_piece_parses() {
        let toml = r#"
[[pieces]]
name = "my riff"
notes = [ [0,4], [3,4], [5,4], [7,4] ]
"#;
        let c = parse_custom(toml).unwrap();
        assert_eq!(c.pieces.len(), 1);
        assert_eq!(c.pieces[0].notes, vec![(0, 4), (3, 4), (5, 4), (7, 4)]);
    }

    #[test]
    fn merge_overrides_by_name() {
        let mut lib = ContentLibrary::bundled();
        let bundled_blues = lib
            .licks
            .iter()
            .find(|l| l.name == "Blues turnaround")
            .cloned()
            .unwrap();
        assert!(!bundled_blues.intervals.is_empty());

        let custom = CustomContent {
            licks: vec![LickItem {
                name: "Blues turnaround".to_string(),
                intervals: vec![0, 5, 7, 12],
            }],
            pieces: vec![],
        };
        lib.merge_custom(custom);

        let after = lib
            .licks
            .iter()
            .find(|l| l.name == "Blues turnaround")
            .unwrap();
        assert_eq!(after.intervals, vec![0, 5, 7, 12]);
        // Override must not duplicate the entry.
        assert_eq!(
            lib.licks
                .iter()
                .filter(|l| l.name == "Blues turnaround")
                .count(),
            1
        );
    }

    #[test]
    fn malformed_toml_is_err_not_panic() {
        let bad = "this is = = not toml";
        let res = parse_custom(bad);
        // toml may accept some junk; ensure at least it doesn't panic and the
        // explicit-table cases error. Use a definitely-broken lick table.
        let bad2 = r#"
[[licks]]
name = "broken"
intervals = "not a list"
"#;
        assert!(parse_custom(bad2).is_err());
        let _ = res; // no panic
    }

    #[test]
    fn empty_file_no_panic() {
        assert!(parse_custom("").unwrap().licks.is_empty());
    }
}
