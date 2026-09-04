//! Bundled chord progressions.
//!
//! Degrees are encoded as **`i8` semitone offsets** from the tonic within the
//! octave: I=0, ii=2, IV=5, V=7, vi=9, bVII=10, etc. This single encoding
//! cleanly handles flatted degrees (signed) and makes root-note mapping trivial
//! (`root_midi + degree`).

/// A bundled progression: ordered degree offsets + a human name.
#[derive(Copy, Clone, Debug)]
pub struct Progression {
    pub degrees: &'static [i8],
    pub name: &'static str,
}

/// Roman-numeral labels per degree index, for display (e.g. `0 -> "I"`).
/// Indices look up `degree` values; labels cover the common diatonic set.
pub const DEGREE_LABELS: [(&str, i8); 12] = [
    ("I", 0),
    ("bII", 1),
    ("ii", 2),
    ("bIII", 3),
    ("iii", 4),
    ("IV", 5),
    ("bV", 6),
    ("V", 7),
    ("bVI", 8),
    ("vi", 9),
    ("bVII", 10),
    ("vii", 11),
];

/// Format a degree offset as its roman-numeral label.
pub fn degree_label(degree: i8) -> &'static str {
    DEGREE_LABELS
        .iter()
        .find(|(_, d)| *d == degree)
        .map(|(l, _)| *l)
        .unwrap_or("?")
}

/// Bundled progressions (degrees are signed semitone offsets from the tonic).
pub static PROGRESSIONS: &[Progression] = &[
    Progression {
        degrees: &[0, 5, 7, 0],
        name: "I–IV–V–I",
    },
    Progression {
        degrees: &[2, 7, 0],
        name: "ii–V–I",
    },
    Progression {
        degrees: &[0, 9, 5, 7],
        name: "I–vi–IV–V",
    },
    Progression {
        degrees: &[7, 9, 5, 0],
        name: "V–vi–IV–I",
    },
    Progression {
        degrees: &[0, 7, 9, 5],
        name: "I–V–vi–IV",
    },
    Progression {
        degrees: &[10, 5, 0],
        name: "bVII–IV–I",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progressions_nonempty() {
        assert!(!PROGRESSIONS.is_empty());
    }

    #[test]
    fn ivv_i_in_c_yields_c_f_g_c() {
        // Root C = MIDI 60. Degrees [0,5,7,0] -> [60, 65, 67, 60] = C F G C.
        let root: u8 = 60;
        let notes: Vec<u8> = PROGRESSIONS[0]
            .degrees
            .iter()
            .map(|&d| root.saturating_add_signed(d))
            .collect();
        assert_eq!(notes, vec![60, 65, 67, 60]);
        assert_eq!(
            notes
                .iter()
                .map(|&n| crate::note::Note::from_midi(n).unwrap().name())
                .collect::<Vec<_>>(),
            vec!["C4", "F4", "G4", "C4"]
        );
    }
}
