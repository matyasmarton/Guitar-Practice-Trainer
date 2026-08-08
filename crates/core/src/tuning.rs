//! All-fourths guitar tuning constants and fret arithmetic.
//!
//! `ALL_FOURTHS = [E2, A2, D3, G3, C4, F4]` = MIDI `[40, 45, 50, 55, 60, 65]`,
//! indexed low→high (string 0 is the bass string). Every adjacent pair differs
//! by exactly 5 semitones — a perfect fourth — so chord/scale shapes are
//! movable and identical at every root.

use crate::note::{Note, MIDI_MAX};

/// Open-string MIDI notes, low→high.
pub const ALL_FOURTHS: [u8; 6] = [40, 45, 50, 55, 60, 65];

/// Number of frets on the instrument (22-fret default).
pub const FRET_COUNT: u8 = 22;

/// MIDI note produced by `fret`ting `string` (0 = open).
///
/// Returns `None` when the string index is invalid or the resulting note would
/// exceed the playable range `MIDI_MIN..=MIDI_MAX`.
pub fn string_midi(string: usize, fret: u8) -> Option<u8> {
    let open = ALL_FOURTHS.get(string).copied()?;
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
pub fn string_note(string: usize, fret: u8) -> Option<Note> {
    string_midi(string, fret).and_then(Note::from_midi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fourths_perfect_fourth_spacing() {
        for w in ALL_FOURTHS.windows(2) {
            assert_eq!(w[1] - w[0], 5, "adjacent strings must be a P5 apart");
        }
    }

    #[test]
    fn string_midi_basic() {
        assert_eq!(string_midi(0, 0), Some(40)); // E2
        assert_eq!(string_midi(0, 5), Some(45)); // A2 on the E string
        assert_eq!(string_midi(5, 0), Some(65)); // F4 top string open
        assert_eq!(string_midi(5, 22), Some(87));
        assert!(string_midi(6, 0).is_none());
        assert!(string_midi(5, 25).is_none());
    }
}