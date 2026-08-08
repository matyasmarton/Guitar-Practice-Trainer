//! `guitar_trainer_core` — real-time audio + pitch core for the all-fourths
//! guitar practice trainer.
//!
//! The crate is intentionally UI-free: it owns note/tuning music theory, the
//! challenge generators, vendored YIN pitch detection, cpal mic capture, and
//! the match/score state machine. Both the Mac TUI (`guitar_trainer_tui`) and
//! the Android Compose app consume it — the latter through UniFFI (see the

pub mod audio;
pub mod challenges;
pub mod config;
pub mod content;
pub mod engine;
pub mod licks;
pub mod music;
pub mod note;
pub mod pieces;
pub mod pitch;
pub mod progressions;
pub mod tuning;

#[cfg(feature = "uniffi")]
pub mod ffi;

// UniFFI scaffolding must live in the crate root so `#[uniffi::export]` items
// can resolve the generated `UniFfiTag`.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("guitar_trainer_core");