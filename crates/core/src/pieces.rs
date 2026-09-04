//! Bundled short recognizable riff *openings* (melody fragments, 4–8 notes).
//!
//! Each piece is a literal pitch sequence: `(pitch_class, octave)` tuples giving
//! absolute MIDI via `octave*12 + 12 + pitch_class` (scientific convention). The
//! engine transposes so the lowest note ≥ `MIDI_MIN` before scoring.
//!
//! These are **brief recognizable openings** (not full songs) — kept short both
//! for fair-use tractability and limited authoring scope. The data file is
//! editable so users can add more.

#[derive(Copy, Clone, Debug)]
pub struct Piece {
    pub name: &'static str,
    pub notes: &'static [(u8, u8)],
}

/// Resolve a `(pitch_class, octave)` pair to a MIDI note number.
pub fn midi_of(pitch_class: u8, octave: u8) -> u8 {
    // midi = (octave + 1) * 12 + pc
    octave
        .saturating_mul(12)
        .saturating_add(12)
        .saturating_add(pitch_class)
}

/// ~6 short openings. Intervals/pitches are the recognizable opening motifs.
pub static PIECES: &[Piece] = &[
    Piece {
        // "Smoke on the Water" — iconic opening motif (transposed to D).
        name: "Smoke on the Water",
        notes: &[(2, 4), (5, 4), (7, 4), (2, 4), (5, 4), (8, 4), (7, 4)],
    },
    Piece {
        // "Sunshine of Your Love" opening riff.
        name: "Sunshine of Your Love",
        notes: &[(0, 4), (3, 4), (5, 4), (7, 4), (5, 4), (3, 4), (0, 4)],
    },
    Piece {
        // "Seven Nation Army" — opening riff.
        name: "Seven Nation Army",
        notes: &[(11, 3), (2, 4), (4, 4), (2, 4), (11, 3), (9, 3), (11, 3)],
    },
    Piece {
        // "Iron Man" — opening motif.
        name: "Iron Man",
        notes: &[(7, 3), (7, 3), (10, 3), (7, 3), (5, 3), (7, 3), (2, 4)],
    },
    Piece {
        // "Nothing Else Matters" — intro opening motif.
        name: "Nothing Else Matters",
        notes: &[(11, 3), (10, 3), (11, 3), (2, 4), (0, 4), (11, 3)],
    },
    Piece {
        // "Whole Lotta Love" — opening riff motif.
        name: "Whole Lotta Love",
        notes: &[(10, 3), (10, 3), (1, 4), (3, 4), (10, 3), (10, 3)],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_nonempty_and_short() {
        assert!(PIECES.len() >= 6);
        for p in PIECES {
            assert!(
                !p.notes.is_empty() && p.notes.len() <= 8,
                "{} too long",
                p.name
            );
            for &(pc, oct) in p.notes {
                assert!(pc < 12);
                assert!(midi_of(pc, oct) > 0);
            }
        }
    }

    #[test]
    fn midi_of_convention() {
        // MIDI 60 == C4.
        assert_eq!(midi_of(0, 4), 60);
        assert_eq!(midi_of(9, 4), 69); // A4
    }
}
