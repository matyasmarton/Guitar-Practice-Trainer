//! MIDI note model for the guitar trainer.
//!
//! A [`Note`] wraps a MIDI note number restricted to the playable range
//! spanning the lowest/highest reachable note across every supported tuning
//! (see `crate::tuning::TuningId`): `MIDI_MIN..=MIDI_MAX` = `38..=89`
//! (D2..F6). The floor comes from Drop-D All Fourths' open low string (D2);
//! the ceiling from All Fourths' top string (F4) fretted to the 24th fret.
//!
//! Accidentals are always rendered as **sharps** (see [`Note::name`]).

use std::fmt;

/// Lowest playable note: Drop-D All Fourths' open low string, D2 = MIDI 38.
pub const MIDI_MIN: u8 = 38;
/// Highest playable note: top string (F4) + 22 frets = 65 + 22 = 87 (also
/// covers 24-fret boards: 65 + 24 = 89). Cap kept generous.
pub const MIDI_MAX: u8 = 89;

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Pitch class letter index used by [`Note::from_name`] (`C=0 .. B=6`).
const LETTER_PC: [(char, u8); 7] = [
    ('C', 0),
    ('D', 2),
    ('E', 4),
    ('F', 5),
    ('G', 7),
    ('A', 9),
    ('B', 11),
];

/// A MIDI note number clamped to the playable range of the instrument.
///
/// Wrap construction through [`Note::from_midi`] / [`Note::from_hz`] /
/// [`Note::from_name`] so the range invariant holds at every construction site.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Note(pub u8);

impl Note {
    /// Build a `Note` from a raw MIDI number, returning `None` if it falls
    /// outside `MIDI_MIN..=MIDI_MAX`.
    pub const fn from_midi(midi: u8) -> Option<Note> {
        if midi >= MIDI_MIN && midi <= MIDI_MAX {
            Some(Note(midi))
        } else {
            None
        }
    }

    /// Same as [`Note::from_midi`] but clamps out-of-range values into the
    /// playable range. Used by generators that may overshoot while voicing.
    pub const fn from_midi_clamped(midi: u8) -> Note {
        if midi < MIDI_MIN {
            Note(MIDI_MIN)
        } else if midi > MIDI_MAX {
            Note(MIDI_MAX)
        } else {
            Note(midi)
        }
    }

    #[inline]
    pub const fn midi(self) -> u8 {
        self.0
    }

    /// Octave number using the scientific-pitch convention (MIDI 69 = A4),
    /// i.e. `octave = (midi / 12) - 1`.
    #[inline]
    pub const fn octave(self) -> i8 {
        (self.0 / 12) as i8 - 1
    }

    /// Pitch class `0..=11` (`C=0`).
    #[inline]
    pub const fn pitch_class(self) -> u8 {
        self.0 % 12
    }

    /// Note name with a sharp accidental, e.g. `69 → "A4"`, `40 → "E2"`.
    pub fn name(self) -> String {
        format!("{}{}", NAMES[self.pitch_class() as usize], self.octave())
    }

    /// Frequency in Hz: `440 * 2^((midi - 69) / 12)`.
    pub fn hz(self) -> f64 {
        440.0 * 2.0_f64.powf((self.0 as f64 - 69.0) / 12.0)
    }

    /// Nearest note for a frequency, `round(69 + 12*log2(f/440))`, clamped to range.
    pub fn from_hz(hz: f64) -> Option<Note> {
        if !hz.is_finite() || hz <= 0.0 {
            return None;
        }
        let midi = (69.0 + 12.0 * (hz / 440.0).log2()).round();
        if !(0.0..=255.0).contains(&midi) {
            return None;
        }
        Note::from_midi(midi as u8)
    }

    /// Parse `^[A-G](#|b)?-?\d{1,2}$` (sharps and flats both accepted on input).
    pub fn from_name(s: &str) -> Option<Note> {
        let s = s.trim();
        let bytes = s.as_bytes();
        if bytes.is_empty() {
            return None;
        }
        // Leading optional '-' for negative octaves (outside range, but allowed).
        let (neg, rest) = if bytes[0] == b'-' {
            (true, &s[1..])
        } else {
            (false, s)
        };
        let rb = rest.as_bytes();
        if rb.is_empty() {
            return None;
        }
        let letter = rb[0] as char;
        let pc = LETTER_PC
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(&letter))?
            .1;
        let mut idx = 1usize;
        let mut pc = pc;
        // Optional accidental.
        if idx < rb.len() {
            match rb[idx] {
                b'#' => {
                    pc = (pc + 1) % 12;
                    idx += 1;
                }
                b'b' => {
                    pc = (pc + 11) % 12;
                    idx += 1;
                }
                _ => {}
            }
        }
        let octave_str = &rest[idx..];
        if octave_str.is_empty() || octave_str.len() > 2 {
            return None;
        }
        if !octave_str.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let octave: i32 = octave_str.parse().ok()?;
        // Scientific pitch → MIDI: midi = (octave + 1) * 12 + pitch_class,
        // so MIDI 60 = C4, MIDI 69 = A4, MIDI 40 = E2.
        let midi = (octave + 1) * 12 + pc as i32;
        let midi = if neg { -midi } else { midi };
        if !(0..=255).contains(&midi) {
            return None;
        }
        Note::from_midi(midi as u8)
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_round_trips() {
        assert_eq!(Note(69).name(), "A4");
        assert_eq!(Note(40).name(), "E2");
        assert_eq!(Note(65).name(), "F4");
        assert_eq!(Note(89).name(), "F6");
        assert_eq!(Note(61).name(), "C#4");
    }
    #[test]
    fn from_hz_nearest() {
        assert!(Note::from_hz(440.0).map(|n| n.0 == 69).unwrap_or(false));
        let e2 = Note::from_hz(82.41).unwrap();
        // 82.41 Hz → MIDI 40 (E2); allow ±1 for float rounding.
        assert!((39..=41).contains(&e2.0), "E2 midi {}", e2.0);
        assert!(Note::from_hz(0.0).is_none());
        assert!(Note::from_hz(f64::NAN).is_none());
    }

    #[test]
    fn from_name_parses() {
        assert_eq!(Note::from_name("A4").map(|n| n.0), Some(69));
        assert_eq!(Note::from_name("E2").map(|n| n.0), Some(40));
        assert_eq!(Note::from_name("F4").map(|n| n.0), Some(65));
        assert_eq!(Note::from_name("C#4").map(|n| n.0), Some(61));
        assert_eq!(Note::from_name("Db4").map(|n| n.0), Some(61));
        assert!(Note::from_name("nope").is_none());
        assert!(Note::from_name("H4").is_none());
        assert!(Note::from_name("C").is_none());
        assert!(Note::from_name("C999").is_none());
    }

    #[test]
    fn range_invariant() {
        assert_eq!(MIDI_MIN, 38);
        assert_eq!(MIDI_MAX, 89);
        assert!(Note::from_midi(37).is_none());
        assert!(Note::from_midi(90).is_none());
        assert!(Note::from_midi(38).is_some());
        assert!(Note::from_midi(89).is_some());
    }
}
