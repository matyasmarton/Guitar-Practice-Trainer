//! Guitar tuning model and fret arithmetic.
//!
//! [`TuningId`] enumerates the tunings this trainer supports. Each variant's
//! open strings are exactly 5 semitones (a perfect fourth) apart *except*
//! [`TuningId::Standard`], which has the traditional major-third break between
//! the G and B strings. Movable chord/scale shapes only hold across every
//! root for the all-fourths tunings.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::note::{Note, MIDI_MAX};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningId {
    Standard,
    AllFourths,
    DropDAllFourths,
}

impl TuningId {
    pub const ALL: [TuningId; 3] = [TuningId::Standard, TuningId::AllFourths, TuningId::DropDAllFourths];

    /// Open-string MIDI notes, low→high.
    pub const fn open_strings(self) -> [u8; 6] {
        match self {
            TuningId::Standard => [40, 45, 50, 55, 59, 64],        // E2 A2 D3 G3 B3 E4
            TuningId::AllFourths => [40, 45, 50, 55, 60, 65],      // E2 A2 D3 G3 C4 F4
            TuningId::DropDAllFourths => [38, 43, 48, 53, 58, 63], // D2 G2 C3 F3 A#3 D#4
        }
    }

    /// Inclusive MIDI range playable on this tuning: from the open low
    /// string up to the highest string fretted to [`FRET_COUNT`]. Adjacent
    /// strings are always ≤5 semitones apart (a perfect fourth, or the
    /// major third on `Standard`), which is less than `FRET_COUNT`, so each
    /// string's own reachable span overlaps the next and their union has no
    /// gaps — every MIDI value in this range is reachable on *some* string.
    pub fn range(self) -> RangeInclusive<u8> {
        let strings = self.open_strings();
        let lo = strings[0];
        let hi = strings[strings.len() - 1].saturating_add(FRET_COUNT);
        lo..=hi
    }

    /// Human-readable label; also the FFI/UI wire string (mirrors `ChallengeType::label`).
    pub const fn label(self) -> &'static str {
        match self {
            TuningId::Standard => "Standard (E A D G B E)",
            TuningId::AllFourths => "All Fourths (E A D G C F)",
            TuningId::DropDAllFourths => "Drop-D All Fourths (D G C F Bb Eb)",
        }
    }
}

impl Default for TuningId {
    fn default() -> Self {
        TuningId::AllFourths
    }
}

/// Number of frets on the instrument (22-fret default).
pub const FRET_COUNT: u8 = 22;

/// MIDI note produced by `fret`ting `string` (0 = open) on `tuning`.
///
/// Returns `None` when the string index is invalid or the resulting note would
/// exceed the playable range `MIDI_MIN..=MIDI_MAX`.
pub fn string_midi(tuning: TuningId, string: usize, fret: u8) -> Option<u8> {
    let open = tuning.open_strings().get(string).copied()?;
    let midi = open.saturating_add(fret);
    if midi > MIDI_MAX {
        return None;
    }
    // A 22-fret board cannot exceed this; defensive clamp.
    if fret > FRET_COUNT {
        return None;
    }
    Some(midi)
}

/// Like [`string_midi`] but returns a [`Note`].
pub fn string_note(tuning: TuningId, string: usize, fret: u8) -> Option<Note> {
    string_midi(tuning, string, fret).and_then(Note::from_midi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fourths_perfect_fourth_spacing() {
        for w in TuningId::AllFourths.open_strings().windows(2) {
            assert_eq!(w[1] - w[0], 5, "adjacent strings must be a P4 apart");
        }
        for w in TuningId::DropDAllFourths.open_strings().windows(2) {
            assert_eq!(w[1] - w[0], 5, "adjacent strings must be a P4 apart");
        }
    }

    #[test]
    fn standard_tuning_has_major_third_break() {
        let strings = TuningId::Standard.open_strings();
        let gaps: Vec<u8> = strings.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps, vec![5, 5, 5, 4, 5], "standard tuning breaks the P4 pattern between G and B");
    }

    #[test]
    fn string_midi_basic() {
        assert_eq!(string_midi(TuningId::AllFourths, 0, 0), Some(40)); // E2
        assert_eq!(string_midi(TuningId::AllFourths, 0, 5), Some(45)); // A2 on the E string
        assert_eq!(string_midi(TuningId::AllFourths, 5, 0), Some(65)); // F4 top string open
        assert_eq!(string_midi(TuningId::AllFourths, 5, 22), Some(87));
        assert!(string_midi(TuningId::AllFourths, 6, 0).is_none());
        assert!(string_midi(TuningId::AllFourths, 5, 25).is_none());
    }

    #[test]
    fn range_matches_open_low_to_fretted_high() {
        assert_eq!(TuningId::Standard.range(), 40..=86);
        assert_eq!(TuningId::AllFourths.range(), 40..=87);
        assert_eq!(TuningId::DropDAllFourths.range(), 38..=85);
    }

    #[test]
    fn range_has_no_unreachable_gaps() {
        // Every MIDI value the generators might pick from `range()` must be
        // reachable on at least one string — otherwise a "playable" pick
        // could still be an impossible fretting for the active tuning.
        for tuning in TuningId::ALL {
            for midi in tuning.range() {
                assert!(
                    (0..6).any(|s| string_midi(tuning, s, 0)
                        .map_or(false, |open| midi >= open && midi - open <= FRET_COUNT)),
                    "{tuning:?} MIDI {midi} in range() but not reachable on any string"
                );
            }
        }
    }
}
