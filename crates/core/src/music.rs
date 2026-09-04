//! Music-theory primitives for the guitar: chord qualities, scale types,
//! modes, and fretted-voicing generation.
//!
//! Every target pitch produced here is a **real fretted note** on the active
//! tuning's fretboard ([`crate::tuning`]) within `MIDI_MIN..=MIDI_MAX`, so
//! each target corresponds to a real playable shape on that tuning. v1
//! surfaces note names only; a fretboard diagram is deferred (see plan).

use crate::note::{Note, MIDI_MAX, MIDI_MIN};
use crate::tuning::{Tuning, FRET_COUNT};

// ---------------------------------------------------------------------------
// Chord qualities
// ---------------------------------------------------------------------------

/// Common chord qualities. Interval vectors are semitone offsets from the root.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChordQuality {
    Major,
    Minor,
    Diminished,
    Augmented,
    Major7,
    Dominant7,
    Minor7,
    HalfDiminished,
    Sus2,
    Sus4,
    Add9,
}

impl ChordQuality {
    /// Semitone intervals from the root, e.g. Major = `[0, 4, 7]`.
    pub const fn intervals(&self) -> &'static [i8] {
        match self {
            ChordQuality::Major => &[0, 4, 7],
            ChordQuality::Minor => &[0, 3, 7],
            ChordQuality::Diminished => &[0, 3, 6],
            ChordQuality::Augmented => &[0, 4, 8],
            ChordQuality::Major7 => &[0, 4, 7, 11],
            ChordQuality::Dominant7 => &[0, 4, 7, 10],
            ChordQuality::Minor7 => &[0, 3, 7, 10],
            ChordQuality::HalfDiminished => &[0, 3, 6, 10],
            ChordQuality::Sus2 => &[0, 2, 7],
            ChordQuality::Sus4 => &[0, 5, 7],
            ChordQuality::Add9 => &[0, 4, 7, 2],
        }
    }

    /// Short suffix used in prompt display, e.g. `Major -> ""`, `Minor7 -> "m7"`.
    pub const fn suffix(&self) -> &'static str {
        match self {
            ChordQuality::Major => "",
            ChordQuality::Minor => "m",
            ChordQuality::Diminished => "dim",
            ChordQuality::Augmented => "aug",
            ChordQuality::Major7 => "maj7",
            ChordQuality::Dominant7 => "7",
            ChordQuality::Minor7 => "m7",
            ChordQuality::HalfDiminished => "m7b5",
            ChordQuality::Sus2 => "sus2",
            ChordQuality::Sus4 => "sus4",
            ChordQuality::Add9 => "add9",
        }
    }

    /// All qualities, in declaration order, for random selection.
    pub const ALL: [ChordQuality; 11] = [
        ChordQuality::Major,
        ChordQuality::Minor,
        ChordQuality::Diminished,
        ChordQuality::Augmented,
        ChordQuality::Major7,
        ChordQuality::Dominant7,
        ChordQuality::Minor7,
        ChordQuality::HalfDiminished,
        ChordQuality::Sus2,
        ChordQuality::Sus4,
        ChordQuality::Add9,
    ];
}

// ---------------------------------------------------------------------------
// Scale types
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ScaleType {
    Major,
    NaturalMinor,
    HarmonicMinor,
    MelodicMinor,
    PentatonicMajor,
    PentatonicMinor,
    Blues,
}

impl ScaleType {
    pub const fn intervals(&self) -> &'static [i8] {
        match self {
            ScaleType::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleType::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleType::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleType::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleType::PentatonicMajor => &[0, 2, 4, 7, 9],
            ScaleType::PentatonicMinor => &[0, 3, 5, 7, 10],
            ScaleType::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            ScaleType::Major => "Major",
            ScaleType::NaturalMinor => "Natural Minor",
            ScaleType::HarmonicMinor => "Harmonic Minor",
            ScaleType::MelodicMinor => "Melodic Minor",
            ScaleType::PentatonicMajor => "Major Pentatonic",
            ScaleType::PentatonicMinor => "Minor Pentatonic",
            ScaleType::Blues => "Blues",
        }
    }

    pub const ALL: [ScaleType; 7] = [
        ScaleType::Major,
        ScaleType::NaturalMinor,
        ScaleType::HarmonicMinor,
        ScaleType::MelodicMinor,
        ScaleType::PentatonicMajor,
        ScaleType::PentatonicMinor,
        ScaleType::Blues,
    ];
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Mode {
    Ionian,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Aeolian,
    Locrian,
}

impl Mode {
    /// Mode intervals are the major-scale rotation.
    pub const fn intervals(&self) -> &'static [i8] {
        match self {
            Mode::Ionian => &[0, 2, 4, 5, 7, 9, 11],
            Mode::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Mode::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Mode::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Mode::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Mode::Aeolian => &[0, 2, 3, 5, 7, 8, 10],
            Mode::Locrian => &[0, 1, 3, 5, 6, 8, 10],
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Mode::Ionian => "Ionian",
            Mode::Dorian => "Dorian",
            Mode::Phrygian => "Phrygian",
            Mode::Lydian => "Lydian",
            Mode::Mixolydian => "Mixolydian",
            Mode::Aeolian => "Aeolian",
            Mode::Locrian => "Locrian",
        }
    }

    pub const ALL: [Mode; 7] = [
        Mode::Ionian,
        Mode::Dorian,
        Mode::Phrygian,
        Mode::Lydian,
        Mode::Mixolydian,
        Mode::Aeolian,
        Mode::Locrian,
    ];
}

// ---------------------------------------------------------------------------
// Fretted voicing generation
// ---------------------------------------------------------------------------

/// Chord voicing: place each chord tone on the **next higher string starting
/// from `bass_string`** (the string the root was picked from), raising or
/// lowering by octaves until it lands within that specific string's fret
/// range.
///
/// Returns the fretted MIDI note for each interval, in order. Guarantees
/// every output is a real fretted pitch reachable on `tuning` — each note is
/// clamped to its own string's `open..=open+FRET_COUNT`, never to a global
/// constant, so no note can land outside the active tuning's playable range.
pub fn fret_voicing(tuning: &Tuning, bass_string: usize, root_midi: u8, intervals: &[i8]) -> Vec<u8> {
    let strings = tuning.open_strings;
    let mut out = Vec::with_capacity(intervals.len());
    for (i, &iv) in intervals.iter().enumerate() {
        let string = (bass_string + i).min(strings.len() - 1);
        let open = strings[string];
        let string_max = open.saturating_add(FRET_COUNT);
        let mut target = root_midi.saturating_add_signed(iv);
        while target < open {
            target = target.saturating_add(12);
        }
        while target > string_max {
            target = target.saturating_sub(12);
        }
        // Defensive: guarantees a real fret on this exact string even for an
        // interval set wider than the calibrated bass-fret/interval range.
        target = target.clamp(open, string_max);
        out.push(target);
    }
    out
}

/// Per-degree re-voicing used by scales/modes/licks/pieces: for each target
/// pitch, find the string with the **smallest non-negative fret ≤ FRET_COUNT**
/// that produces it, raising the target by octaves until it fits. Returns the
/// fretted MIDI for each interval in order (monophonic, ascending).
pub fn fret_notes(tuning: &Tuning, root_midi: u8, intervals: &[i8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(intervals.len());
    for &iv in intervals {
        let mut target = root_midi.saturating_add_signed(iv);
        loop {
            if let Some(m) = best_string_midi(tuning, target) {
                out.push(m);
                break;
            }
            if target >= MIDI_MAX {
                out.push(MIDI_MAX);
                break;
            }
            target = target.saturating_add(12);
        }
    }
    out
}

/// Return the MIDI note produced by the string whose open pitch is the largest
/// `≤ target` with `fret ≤ FRET_COUNT`; i.e. the lowest-fret playable voicing.
/// Returns `None` if no string can voice `target` without exceeding
/// `FRET_COUNT` (caller then raises an octave).
fn best_string_midi(tuning: &Tuning, target: u8) -> Option<u8> {
    if !(MIDI_MIN..=MIDI_MAX).contains(&target) {
        return None;
    }
    let mut best: Option<(u8, usize)> = None; // (fret, string)
    for (s, &open) in tuning.open_strings.iter().enumerate() {
        if target >= open {
            let fret = target - open;
            if fret <= FRET_COUNT {
                match best {
                    None => best = Some((fret, s)),
                    Some((bf, _)) if fret < bf => best = Some((fret, s)),
                    _ => {}
                }
            }
        }
    }
    best.map(|(_, _)| target)
}

/// Names of a list of MIDI notes, joined by spaces (for chord display).
pub fn note_names(notes: &[u8]) -> String {
    notes
        .iter()
        .map(|&m| Note::from_midi_clamped(m).name())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::MIDI_MIN;
    use crate::tuning::{string_midi, Tuning, TuningId};

    #[test]
    fn chord_intervals_match_spec() {
        assert_eq!(ChordQuality::Major.intervals(), &[0, 4, 7]);
        assert_eq!(ChordQuality::Minor.intervals(), &[0, 3, 7]);
        assert_eq!(ChordQuality::Diminished.intervals(), &[0, 3, 6]);
        assert_eq!(ChordQuality::Augmented.intervals(), &[0, 4, 8]);
        assert_eq!(ChordQuality::Major7.intervals(), &[0, 4, 7, 11]);
        assert_eq!(ChordQuality::Dominant7.intervals(), &[0, 4, 7, 10]);
        assert_eq!(ChordQuality::Minor7.intervals(), &[0, 3, 7, 10]);
        assert_eq!(ChordQuality::HalfDiminished.intervals(), &[0, 3, 6, 10]);
        assert_eq!(ChordQuality::Sus2.intervals(), &[0, 2, 7]);
        assert_eq!(ChordQuality::Sus4.intervals(), &[0, 5, 7]);
        assert_eq!(ChordQuality::Add9.intervals(), &[0, 4, 7, 2]);
    }

    #[test]
    fn scale_intervals_match_spec() {
        assert_eq!(ScaleType::Major.intervals(), &[0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(
            ScaleType::NaturalMinor.intervals(),
            &[0, 2, 3, 5, 7, 8, 10]
        );
        assert_eq!(
            ScaleType::HarmonicMinor.intervals(),
            &[0, 2, 3, 5, 7, 8, 11]
        );
        assert_eq!(
            ScaleType::MelodicMinor.intervals(),
            &[0, 2, 3, 5, 7, 9, 11]
        );
        assert_eq!(
            ScaleType::PentatonicMajor.intervals(),
            &[0, 2, 4, 7, 9]
        );
        assert_eq!(
            ScaleType::PentatonicMinor.intervals(),
            &[0, 3, 5, 7, 10]
        );
        assert_eq!(ScaleType::Blues.intervals(), &[0, 3, 5, 6, 7, 10]);
    }

    #[test]
    fn mode_intervals_match_spec() {
        assert_eq!(Mode::Ionian.intervals(), &[0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(Mode::Dorian.intervals(), &[0, 2, 3, 5, 7, 9, 10]);
        assert_eq!(Mode::Phrygian.intervals(), &[0, 1, 3, 5, 7, 8, 10]);
        assert_eq!(Mode::Lydian.intervals(), &[0, 2, 4, 6, 7, 9, 11]);
        assert_eq!(Mode::Mixolydian.intervals(), &[0, 2, 4, 5, 7, 9, 10]);
        assert_eq!(Mode::Aeolian.intervals(), &[0, 2, 3, 5, 7, 8, 10]);
        assert_eq!(Mode::Locrian.intervals(), &[0, 1, 3, 5, 6, 8, 10]);
    }

    #[test]
    fn fret_voicing_in_range_and_on_valid_frets() {
        for root in [MIDI_MIN, 45, 50, 55, 60] {
            for q in ChordQuality::ALL {
                let notes = fret_voicing(&Tuning::builtin(TuningId::AllFourths), 0, root, q.intervals());
                assert!(!notes.is_empty(), "empty voicing for {:?}", q);
                for &n in &notes {
                    assert!(
                        (MIDI_MIN..=MIDI_MAX).contains(&n),
                        "voicing note {} out of range",
                        n
                    );
                    // Each note is reachable on at least one string.
                    assert!(
                        (0..6).any(|s| string_midi(TuningId::AllFourths, s, 0).map_or(false, |open| {
                            n >= open && n - open <= FRET_COUNT
                        })),
                        "voicing note {} not frettable",
                        n
                    );
                }
            }
        }
    }

    #[test]
    fn fret_notes_in_range() {
        for root in [MIDI_MIN, 45, 50, 55] {
            for s in ScaleType::ALL {
                let notes = fret_notes(&Tuning::builtin(TuningId::AllFourths), root, s.intervals());
                assert_eq!(notes.len(), s.intervals().len());
                for &n in &notes {
                    assert!((MIDI_MIN..=MIDI_MAX).contains(&n));
                    assert!(
                        (0..6).any(|s_idx| string_midi(TuningId::AllFourths, s_idx, 0).map_or(false, |open| {
                            n >= open && n - open <= FRET_COUNT
                        })),
                        "scale note {} not frettable",
                        n
                    );
                }
            }
        }
    }

    #[test]
    fn fret_voicing_major_triad_low_e() {
        // Root E2=40, Major [0,4,7]: string0 fret0=40, string1 fret? target 44 →
        // open 45 too high, so raise to 44+12=56 → string1 open 45 fret 11=56.
        // Then 47 → open50 too high, +12=59 → string2 fret 9. Ascending voicing.
        let v = fret_voicing(&Tuning::builtin(TuningId::AllFourths), 0, 40, ChordQuality::Major.intervals());
        assert_eq!(v.len(), 3);
        assert!(v.windows(2).all(|w| w[1] >= w[0]));
        for &n in &v {
            assert!((40..=89).contains(&n));
        }
    }
}