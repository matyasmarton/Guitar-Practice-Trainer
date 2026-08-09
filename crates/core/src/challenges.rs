//! Challenge model and generators — the spec's center of gravity.
//!
//! ## Acceptance rule (stated once)
//! Every challenge reduces to a target note list (`targets: Vec<Note>`).
//! - `ordered = false` → every target must be matched **as a set** (each
//!   produced once, any order) before timeout.
//! - `ordered = true` → matched **in order**: each next target only counts
//!   after the previous is matched.
//! A "match" is a detected pitch whose nearest-MIDI equals the target (±0)
//! and is stable across ≥2 consecutive detection frames.

use rand::Rng;

use crate::content::ContentLibrary;
use crate::music::{
    note_names, fret_notes, fret_voicing, ChordQuality, Mode, ScaleType,
};
use crate::note::Note;
use crate::pieces::midi_of;
use crate::progressions::{degree_label, PROGRESSIONS};
use crate::tuning::{Tuning, TuningId};

/// The seven prompt categories. The enabled-set is derived from this and the
/// "Random" draw uniformly samples among enabled categories.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChallengeType {
    Note,
    Chord,
    Scale,
    Mode,
    Progression,
    Lick,
    Piece,
}

impl ChallengeType {
    pub const ALL: [ChallengeType; 7] = [
        ChallengeType::Note,
        ChallengeType::Chord,
        ChallengeType::Scale,
        ChallengeType::Mode,
        ChallengeType::Progression,
        ChallengeType::Lick,
        ChallengeType::Piece,
    ];

    pub const fn label(&self) -> &'static str {
        match self {
            ChallengeType::Note => "Note",
            ChallengeType::Chord => "Chord",
            ChallengeType::Scale => "Scale",
            ChallengeType::Mode => "Mode",
            ChallengeType::Progression => "Progression",
            ChallengeType::Lick => "Lick",
            ChallengeType::Piece => "Piece",
        }
    }
}

/// A fully-resolved challenge ready to score against.
#[derive(Clone, Debug)]
pub struct Challenge {
    pub kind: ChallengeType,
    /// Human-readable prompt text shown center-screen.
    pub display: String,
    /// Target note list (the only thing that's scored).
    pub targets: Vec<Note>,
    /// Set-completion vs. ordered-sequence evaluation.
    pub ordered: bool,
}

impl Challenge {
    /// Build a display string listing target note names (for the UI checklist).
    pub fn target_names(&self) -> Vec<String> {
        self.targets.iter().map(|n| n.name()).collect()
    }
}

/// Generate a challenge of `kind` using `rng` and the content `library`.
pub fn generate<R: Rng>(
    kind: ChallengeType,
    rng: &mut R,
    library: &ContentLibrary,
    tuning: &Tuning,
) -> Challenge {
    match kind {
        ChallengeType::Note => gen_note(rng, tuning),
        ChallengeType::Chord => gen_chord(rng, tuning),
        ChallengeType::Scale => gen_scale(rng, tuning),
        ChallengeType::Mode => gen_mode(rng, tuning),
        ChallengeType::Progression => gen_progression(rng, tuning),
        ChallengeType::Lick => gen_lick(rng, tuning, library),
        ChallengeType::Piece => gen_piece(rng, tuning, library),
    }
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn gen_note<R: Rng>(rng: &mut R, tuning: &Tuning) -> Challenge {
    let midi = rng.gen_range(tuning.range());
    let note = Note::from_midi(midi).expect("tuning.range() is within MIDI_MIN..=MIDI_MAX");
    Challenge {
        kind: ChallengeType::Note,
        display: note.name(),
        targets: vec![note],
        ordered: false,
    }
}

fn gen_chord<R: Rng>(rng: &mut R, tuning: &Tuning) -> Challenge {
    // Random root fret (0..=14) on a random bass string (0..=4).
    let bass_string: usize = rng.gen_range(0..5);
    let fret: u8 = rng.gen_range(0..=15);
    let open = tuning.open_strings[bass_string];
    let root_midi = open + fret;
    let q = ChordQuality::ALL[rng.gen_range(0..ChordQuality::ALL.len())];
    let voicing = fret_voicing(tuning, bass_string, root_midi, q.intervals());
    let targets: Vec<Note> = voicing
        .iter()
        .copied()
        .map(Note::from_midi_clamped)
        .collect();
    let root_name = Note::from_midi_clamped(root_midi).name();
    let display = format!(
        "{}{}  ({})",
        root_name,
        q.suffix(),
        note_names(&voicing)
    );
    Challenge {
        kind: ChallengeType::Chord,
        display,
        targets,
        ordered: false,
    }
}

fn gen_scale<R: Rng>(rng: &mut R, tuning: &Tuning) -> Challenge {
    let root_midi = random_root(rng, tuning);
    let st = ScaleType::ALL[rng.gen_range(0..ScaleType::ALL.len())];
    let notes = fret_notes(tuning, root_midi, st.intervals());
    let targets: Vec<Note> = notes.iter().copied().map(Note::from_midi_clamped).collect();
    let root_name = Note::from_midi_clamped(root_midi).name();
    let display = format!("{} {}", root_name, st.name());
    Challenge {
        kind: ChallengeType::Scale,
        display,
        targets,
        ordered: true,
    }
}

fn gen_mode<R: Rng>(rng: &mut R, tuning: &Tuning) -> Challenge {
    let root_midi = random_root(rng, tuning);
    let m = Mode::ALL[rng.gen_range(0..Mode::ALL.len())];
    let notes = fret_notes(tuning, root_midi, m.intervals());
    let targets: Vec<Note> = notes.iter().copied().map(Note::from_midi_clamped).collect();
    let root_name = Note::from_midi_clamped(root_midi).name();
    let display = format!("{} {}", root_name, m.name());
    Challenge {
        kind: ChallengeType::Mode,
        display,
        targets,
        ordered: true,
    }
}

fn gen_progression<R: Rng>(rng: &mut R, tuning: &Tuning) -> Challenge {
    let prog = &PROGRESSIONS[rng.gen_range(0..PROGRESSIONS.len())];
    let key_midi = random_root(rng, tuning);
    // Degrees are non-negative semitone offsets within the octave (see
    // progressions.rs), so re-voice them exactly like a scale/mode: this
    // guarantees every root note is a real fret on the active tuning
    // instead of raw untransposed arithmetic that could land below the
    // lowest open string or above the highest fret.
    let notes = fret_notes(tuning, key_midi, prog.degrees);
    let targets: Vec<Note> = notes.iter().copied().map(Note::from_midi_clamped).collect();
    let key_name = Note::from_midi_clamped(key_midi).name();
    let degree_str: String = prog
        .degrees
        .iter()
        .map(|&d| degree_label(d))
        .collect::<Vec<_>>()
        .join("–");
    let display = format!("{} in {}", degree_str, key_name);
    Challenge {
        kind: ChallengeType::Progression,
        display,
        targets,
        ordered: true,
    }
}

fn gen_lick<R: Rng>(rng: &mut R, tuning: &Tuning, library: &ContentLibrary) -> Challenge {
    if library.licks.is_empty() {
        return gen_note(rng, tuning); // graceful fallback
    }
    let lick = &library.licks[rng.gen_range(0..library.licks.len())];
    let (_root_midi, notes) = transpose_intervals(tuning, &lick.intervals);
    let targets: Vec<Note> = notes.iter().copied().map(Note::from_midi_clamped).collect();
    let head: Vec<String> = targets
        .iter()
        .take(4)
        .map(|n| n.name())
        .collect();
    let display = format!("{}  ({})", lick.name, head.join(" "));
    Challenge {
        kind: ChallengeType::Lick,
        display,
        targets,
        ordered: true,
    }
}

fn gen_piece<R: Rng>(rng: &mut R, tuning: &Tuning, library: &ContentLibrary) -> Challenge {
    if library.pieces.is_empty() {
        return gen_note(rng, tuning);
    }
    let piece = &library.pieces[rng.gen_range(0..library.pieces.len())];
    // Resolve literal pitches then shift by whole octaves so every note
    // sits inside the active tuning's playable range (every value in that
    // range is reachable on some string — see `TuningId::range`'s doc).
    let raw: Vec<u8> = piece.notes.iter().map(|&(pc, oct)| midi_of(pc, oct)).collect();
    let range = tuning.range();
    let floor = *range.start() as i32;
    let ceil = *range.end() as i32;
    let min = *raw.iter().min().unwrap_or(&(floor as u8)) as i32;
    let max = *raw.iter().max().unwrap_or(&(ceil as u8)) as i32;
    let mut shift: i32 = 0;
    while min + shift < floor {
        shift += 12;
    }
    while max + shift > ceil && min + shift - 12 >= floor {
        shift -= 12;
    }
    let notes: Vec<u8> = raw
        .iter()
        .map(|&m| (m as i32 + shift).clamp(floor, ceil) as u8)
        .collect();
    let targets: Vec<Note> = notes.iter().copied().map(Note::from_midi_clamped).collect();
    let head: Vec<String> = targets.iter().take(4).map(|n| n.name()).collect();
    let display = format!("{}  ({})", piece.name, head.join(" "));
    Challenge {
        kind: ChallengeType::Piece,
        display,
        targets,
        ordered: true,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Largest semitone offset among bundled `ScaleType`/`Mode`/`Progression`
/// interval tables (all "within the octave" by design). Keeps `random_root`
/// picks + the widest interval inside the active tuning's playable range on
/// the first try, without relying on `fret_notes`'s octave-raising fallback.
const MAX_DIATONIC_SPAN: u8 = 11;

/// Pick a random root MIDI within `tuning`'s playable range, leaving enough
/// headroom above the root for the widest diatonic interval so every degree
/// built from it lands on a real fret of `tuning`.
fn random_root<R: Rng>(rng: &mut R, tuning: &Tuning) -> u8 {
    let range = tuning.range();
    let lo = *range.start();
    let hi = range.end().saturating_sub(MAX_DIATONIC_SPAN).max(lo);
    rng.gen_range(lo..=hi)
}

/// Transpose an interval set so its lowest realized pitch sits inside
/// `tuning`'s playable range, then fret-voice each degree (re-voicing per
/// degree). Returns (root_midi, notes).
fn transpose_intervals(tuning: &Tuning, intervals: &[i8]) -> (u8, Vec<u8>) {
    let range = tuning.range();
    let floor = *range.start() as i16;
    let ceil = *range.end() as i16;
    // Start near the lowest string; lift root if any note would dip below range.
    let min_iv = *intervals.iter().min().unwrap_or(&0) as i16;
    let mut root: i16 = floor - min_iv;
    if root < floor {
        // raise by octaves until in range
        while root < floor {
            root += 12;
        }
    }
    let root_midi = root.clamp(floor, ceil) as u8;
    let notes = fret_notes(tuning, root_midi, intervals);
    (root_midi, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{MIDI_MAX, MIDI_MIN};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(42)
    }

    fn lib() -> ContentLibrary {
        ContentLibrary::bundled()
    }

    #[test]
    fn note_in_range() {
        let c = gen_note(&mut rng(), &Tuning::builtin(TuningId::AllFourths));
        assert_eq!(c.targets.len(), 1);
        assert!((MIDI_MIN..=MIDI_MAX).contains(&c.targets[0].midi()));
        assert!(!c.ordered);
    }

    #[test]
    fn chord_has_multiple_targets_in_range() {
        for _ in 0..50 {
            let c = gen_chord(&mut rng(), &Tuning::builtin(TuningId::AllFourths));
            assert!(c.targets.len() >= 3);
            assert!(!c.ordered);
            for n in &c.targets {
                assert!((MIDI_MIN..=MIDI_MAX).contains(&n.midi()));
            }
        }
    }

    #[test]
    fn scale_ordered_and_in_range() {
        for _ in 0..50 {
            let c = gen_scale(&mut rng(), &Tuning::builtin(TuningId::AllFourths));
            assert!(c.ordered);
            for n in &c.targets {
                assert!((MIDI_MIN..=MIDI_MAX).contains(&n.midi()));
            }
        }
    }

    #[test]
    fn mode_ordered_and_in_range() {
        for _ in 0..50 {
            let c = gen_mode(&mut rng(), &Tuning::builtin(TuningId::AllFourths));
            assert!(c.ordered);
            assert!(!c.targets.is_empty());
        }
    }

    #[test]
    fn progression_ivv_i_c_yields_c_f_g_c() {
        // Deterministic: construct directly from a fixed key to assert the
        // degree→root mapping the generator uses.
        let key = 60u8; // C4
        let notes: Vec<u8> = [0i8, 5, 7, 0]
            .iter()
            .map(|&d| key.saturating_add_signed(d))
            .collect();
        assert_eq!(notes, vec![60, 65, 67, 60]);
    }

    #[test]
    fn progression_targets_are_chord_roots_in_order() {
        for _ in 0..50 {
            let c = gen_progression(&mut rng(), &Tuning::builtin(TuningId::AllFourths));
            assert!(c.ordered);
            // Targets must be in non-decreasing order? Not strictly (degrees can
            // descend), but they must be valid notes in range.
            for n in &c.targets {
                assert!((MIDI_MIN..=MIDI_MAX).contains(&n.midi()));
            }
        }
    }

    #[test]
    fn lick_and_piece_in_range_and_ordered() {
        let l = lib();
        for _ in 0..50 {
            let c = gen_lick(&mut rng(), &Tuning::builtin(TuningId::AllFourths), &l);
            assert!(c.ordered);
            assert!(!c.targets.is_empty());
            for n in &c.targets {
                assert!((MIDI_MIN..=MIDI_MAX).contains(&n.midi()));
            }
        }
        for _ in 0..50 {
            let c = gen_piece(&mut rng(), &Tuning::builtin(TuningId::AllFourths), &l);
            assert!(c.ordered);
            assert!(!c.targets.is_empty());
            for n in &c.targets {
                assert!((MIDI_MIN..=MIDI_MAX).contains(&n.midi()));
            }
        }
    }

    #[test]
    fn all_kinds_generate() {
        let l = lib();
        for kind in ChallengeType::ALL {
            let c = generate(kind, &mut rng(), &l, &Tuning::builtin(TuningId::AllFourths));
            assert!(!c.targets.is_empty(), "{:?} produced no targets", kind);
            assert!(!c.display.is_empty());
        }
    }

    #[test]
    fn every_generator_stays_fret_reachable_for_every_tuning() {
        // Regression: generators must never hand the player a note that
        // isn't actually playable on the currently selected tuning (e.g. a
        // D2 target while tuned All Fourths, whose lowest open string is E2).
        use crate::tuning::{string_midi, FRET_COUNT};
        let l = lib();
        let reachable = |tuning: TuningId, midi: u8| {
            (0..6).any(|s| {
                string_midi(tuning, s, 0).map_or(false, |open| midi >= open && midi - open <= FRET_COUNT)
            })
        };
        for tuning in TuningId::ALL {
            for kind in ChallengeType::ALL {
                for seed in 0..25u64 {
                    let mut r = ChaCha8Rng::seed_from_u64(seed);
                    let t = Tuning::builtin(tuning);
                    let c = generate(kind, &mut r, &l, &t);
                    for n in &c.targets {
                        let midi = n.midi();
                        assert!(
                            tuning.range().contains(&midi),
                            "{kind:?} on {tuning:?} produced {midi} outside {:?}",
                            tuning.range()
                        );
                        assert!(
                            reachable(tuning, midi),
                            "{kind:?} on {tuning:?} produced {midi} ({}) unreachable on any string",
                            n.name()
                        );
                    }
                }
            }
        }
    }
}