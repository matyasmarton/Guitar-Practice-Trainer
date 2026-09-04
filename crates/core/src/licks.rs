//! Bundled practice licks.
//!
//! A lick is an ordered sequence of **semitone intervals relative to a root**.
//! When a lick challenge is generated, the engine picks a root and transposes
//! the intervals so the lowest note ≥ `MIDI_MIN`, then fret-voices each note
//! via [`crate::music::fret_notes`].
//!
//! Intervals are authored from standard minor-pentatonic / major vocabulary;
//! values >11 wrap implicitly across octaves (e.g. `12` = root one octave up).

#[derive(Copy, Clone, Debug)]
pub struct Lick {
    pub name: &'static str,
    pub intervals: &'static [i8],
}

/// ~8 bundled licks spanning common blues/rock/jazz vocabulary.
pub static LICKS: &[Lick] = &[
    Lick {
        name: "Blues turnaround",
        intervals: &[0, 3, 5, 7, 10, 12],
    },
    Lick {
        name: "Minor pentatonic run",
        intervals: &[0, 3, 5, 7, 10, 12, 10, 7, 5, 3, 0],
    },
    Lick {
        name: "Major pentatonic run",
        intervals: &[0, 2, 4, 7, 9, 12, 9, 7, 4, 2, 0],
    },
    Lick {
        name: "A-minor classical run",
        intervals: &[0, 2, 4, 5, 7, 9, 11, 12],
    },
    Lick {
        name: "Hammer-on lick",
        intervals: &[0, 2, 4, 2, 0, 2, 4, 7],
    },
    Lick {
        name: "Sliding 4ths run",
        intervals: &[0, 5, 12, 17, 7, 12],
    },
    Lick {
        name: "Chromatic approach",
        intervals: &[0, 1, 2, 3, 4, 5, 7],
    },
    Lick {
        name: "Pedal-tone",
        intervals: &[0, 12, 0, 7, 0, 10, 0, 12],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licks_nonempty() {
        assert!(LICKS.len() >= 8);
        for l in LICKS {
            assert!(!l.intervals.is_empty());
        }
    }
}
