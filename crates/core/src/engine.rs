//! Engine runtime: the timer / match / score state machine both frontends bind.
//!
//! Design: [`Engine`] owns a shared [`EngineInner`] (mutex-guarded) plus a
//! driver thread that, every ~10 ms, drains incoming [`PitchEvent`]s, advances
//! the countdown, and emits [`EngineEvent`]s through the registered
//! [`EngineListener`]. The core matching logic lives in plain methods
//! (`on_pitch`, `on_tick`) so the unit suite drives the engine **headlessly**
//! with scripted pitch streams (see verification plan §4).
//!
//! UniFFI note: the types here are plain Rust; the Android `ffi.rs` wrapper
//! re-exports them with `#[uniffi::export]` behind the `uniffi` feature.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tracing::{debug, warn};

use crate::audio::{AudioInput, PitchEvent};
use crossbeam_channel::Receiver;
use crate::challenges::{generate, Challenge, ChallengeType};
use crate::config::Config;
use crate::content::ContentLibrary;
use crate::note::Note;

/// UI-facing challenge record (owned, no `Note` leakage — UniFFI-friendly).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ChallengeView {
    pub kind: String,
    pub display: String,
    pub targets: Vec<String>,
    pub ordered: bool,
}

impl ChallengeView {
    fn from(c: &Challenge) -> Self {
        ChallengeView {
            kind: c.kind.label().to_string(),
            display: c.display.clone(),
            targets: c.target_names(),
            ordered: c.ordered,
        }
    }
}

/// All events the engine emits to a listener.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum EngineEvent {
    /// A new prompt to display.
    Prompt(ChallengeView),
    /// Last stable detected note name (or `None`).
    DetectedNote(Option<String>),
    /// One target was matched.
    Matched { index: u64, total: u64 },
    /// All targets matched before the timer expired.
    Passed,
    /// The timer expired with targets still outstanding.
    Timeout,
    /// Running score.
    Score { passed: u32, total: u32 },
}

/// Implement to receive engine events. Both the TUI and the Android Kotlin
/// callback implement this and bridge to their UI.
///
/// **Warning:** implementations must not call back into the [`Engine`] from
#[cfg_attr(feature = "uniffi", uniffi::export(callback_interface))]
pub trait EngineListener: Send + Sync {
    fn on_event(&self, ev: EngineEvent);
}

// ---------------------------------------------------------------------------
// Inner state
// ---------------------------------------------------------------------------

struct EngineInner {
    config: Config,
    library: ContentLibrary,
    rng: ChaCha8Rng,
    /// Current prompt (set on start + every advance).
    current: Challenge,
    /// Per-target matched flags (same length as `current.targets`).
    matched: Vec<bool>,
    /// For `ordered=true`: index of the next required target.
    next_idx: usize,
    /// Number of targets matched so far.
    matched_count: usize,
    remaining: Duration,
    /// Per-prompt default duration (random mode overrides per prompt).
    default_duration: Duration,
    /// Duration the current prompt started with (for UI progress).
    prompt_duration: Duration,
    /// Stability: consecutive detection frames matching the current required
    /// target. A match is accepted once this reaches ≥2.
    stable_for_target: u8,
    /// Last detected MIDI (nearest) for stable-note display.
    last_detected_midi: Option<u8>,
    last_emitted_note: Option<String>,
    score_passed: u32,
    score_total: u32,
}

impl EngineInner {
    fn total(&self) -> usize {
        self.current.targets.len()
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct Engine {
    inner: Arc<Mutex<EngineInner>>,
    listener: Arc<dyn EngineListener>,
    shutdown: Arc<AtomicBool>,
    audio: Mutex<Option<AudioInput>>,
    pitch_rx: Mutex<Option<Receiver<PitchEvent>>>,
    driver: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Engine {
    /// Create the engine. Loads the bundled + custom content library and the
    /// persisted config, but does **not** start audio or the tick loop.
    pub fn new(config: Config, listener: Box<dyn EngineListener>) -> Result<Self, anyhow::Error> {
        Self::new_with_rng_seed(config, listener, None)
    }

    /// Create with an explicit RNG seed (tests). `seed==None` seeds from entropy.
    pub fn new_with_rng_seed(
        config: Config,
        listener: Box<dyn EngineListener>,
        seed: Option<u64>,
    ) -> Result<Self, anyhow::Error> {
        let library = match &config.custom_content_path {
            Some(p) if !p.as_os_str().is_empty() => {
                match ContentLibrary::load(p) {
                    Ok(l) => l,
                    Err(e) => {
                        warn!("custom content load failed ({e:?}); using bundled only");
                        ContentLibrary::bundled()
                    }
                }
            }
            _ => ContentLibrary::bundled(),
        };
        let rng = match seed {
            Some(s) => ChaCha8Rng::seed_from_u64(s),
            None => ChaCha8Rng::from_entropy(),
        };
        let default_duration = Duration::from_secs(config.default_duration_sec.max(1) as u64);
        let cats = config.active_categories();
        let listener = Arc::from(listener);

        let mut inner = EngineInner {
            config: config.clone(),
            library,
            rng,
            current: Challenge {
                kind: ChallengeType::Note,
                display: String::new(),
                targets: vec![],
                ordered: false,
            },
            matched: vec![],
            next_idx: 0,
            matched_count: 0,
            remaining: default_duration,
            default_duration,
            prompt_duration: default_duration,
            stable_for_target: 0,
            last_detected_midi: None,
            last_emitted_note: None,
            score_passed: 0,
            score_total: 0,
        };
        // Pick an initial prompt so the engine always has a `current`.
        pick_new_prompt(&mut inner, &cats);
        // Do not start the timer until `start()`.
        Ok(Engine {
            inner: Arc::new(Mutex::new(inner)),
            listener,
            shutdown: Arc::new(AtomicBool::new(false)),
            audio: Mutex::new(None),
            pitch_rx: Mutex::new(None),
            driver: Mutex::new(None),
        })
    }

    /// Begin practice: start audio capture + the driver loop + emit first prompt.
    pub fn start(&self) -> Result<(), anyhow::Error> {
        self.shutdown.store(false, Ordering::SeqCst);

        // Audio.
        let (tx, rx) = crossbeam_channel::bounded::<PitchEvent>(256);
        let device = self
            .inner
            .lock()
            .config
            .audio_device_name
            .as_deref()
            .and_then(crate::audio::find_device_by_name);
        let audio = AudioInput::start(device, 44100, tx)?;
        *self.audio.lock() = Some(audio);
        *self.pitch_rx.lock() = Some(rx);

        // Pick the first prompt + reset timer, then emit.
        {
            let mut inner = self.inner.lock();
            let cats = inner.config.active_categories();
            pick_new_prompt(&mut inner, &cats);
            reset_timer(&mut inner);
            emit_prompt(&self.listener, &inner);
            emit_score(&self.listener, &inner);
        }

        // Driver loop.
        let inner = Arc::clone(&self.inner);
        let listener = Arc::clone(&self.listener);
        let shutdown = Arc::clone(&self.shutdown);
        let driver = self.pitch_rx.lock().take().unwrap();
        let handle = std::thread::Builder::new()
            .name("gtt-engine".into())
            .spawn(move || run_driver(driver, inner, listener, shutdown))
            .map_err(|e| anyhow::anyhow!("spawning engine driver: {e}"))?;
        *self.driver.lock() = Some(handle);

        Ok(())
    }

    /// Stop practice: stop audio + the driver loop (mic released, battery saved).
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Drop audio first so the pitch stream ends.
        *self.audio.lock() = None;
        // Draining the rx lets the driver's try_recv see no more events.
        if let Some(h) = self.driver.lock().take() {
            let _ = h.join();
        }
    }

    /// Update config at runtime; reloads content library if the path changed.
    pub fn set_config(&self, cfg: Config) {
        let mut inner = self.inner.lock();
        let path_changed = inner.config.custom_content_path != cfg.custom_content_path;
        inner.config = cfg.clone();
        inner.default_duration = Duration::from_secs(cfg.default_duration_sec.max(1) as u64);
        if path_changed {
            match &cfg.custom_content_path {
                Some(p) if !p.as_os_str().is_empty() => {
                    match ContentLibrary::load(p) {
                        Ok(l) => inner.library = l,
                        Err(e) => warn!("custom content reload failed: {e:?}"),
                    }
                }
                _ => inner.library = ContentLibrary::bundled(),
            }
        }
    }

    /// Skip the current prompt (counts as a timeout): emit Timeout + advance.
    pub fn skip(&self) {
        let cats = {
            let inner = self.inner.lock();
            inner.config.active_categories()
        };
        let mut inner = self.inner.lock();
        emit(&self.listener, EngineEvent::Timeout);
        inner.score_total += 1;
        pick_new_prompt(&mut inner, &cats);
        reset_timer(&mut inner);
        emit_prompt(&self.listener, &inner);
        emit_score(&self.listener, &inner);
    }

    /// Configuration snapshot (for UI display).
    pub fn config(&self) -> Config {
        self.inner.lock().config.clone()
    }

    /// Time remaining for the current prompt as a fraction in `[0,1]`,
    /// plus the remaining seconds (rounded) and total prompt seconds.
    pub fn progress(&self) -> (f64, u64, u64) {
        let g = self.inner.lock();
        let total = g.prompt_duration.as_secs_f64().max(1e-3);
        let frac = (g.remaining.as_secs_f64() / total).clamp(0.0, 1.0);
        (frac, g.remaining.as_secs(), g.prompt_duration.as_secs())
    }

    // -- Headless pump API (used by the driver thread and by tests) -------------

    /// Process one detected pitch (Hz, or `None` for unvoiced).
    pub fn on_pitch(&self, hz: Option<f64>) {
        let cats = {
            let inner = self.inner.lock();
            inner.config.active_categories()
        };
        let mut inner = self.inner.lock();
        handle_pitch(&mut inner, &self.listener, &cats, hz);
    }

    /// Advance the countdown by `dt`. On expiry: Timeout + new prompt.
    pub fn on_tick(&self, dt: Duration) {
        let cats = {
            let inner = self.inner.lock();
            inner.config.active_categories()
        };
        let mut inner = self.inner.lock();
        handle_tick(&mut inner, &self.listener, &cats, dt);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Driver loop
// ---------------------------------------------------------------------------

fn run_driver(
    rx: Receiver<PitchEvent>,
    inner: Arc<Mutex<EngineInner>>,
    listener: Arc<dyn EngineListener>,
    shutdown: Arc<AtomicBool>,
) {
    let tick = Duration::from_millis(10);
    while !shutdown.load(Ordering::SeqCst) {
        let start = std::time::Instant::now();

        // Drain any pitch events.
        while let Ok(ev) = rx.try_recv() {
            let cats = {
                let g = inner.lock();
                g.config.active_categories()
            };
            let mut g = inner.lock();
            handle_pitch(&mut g, &listener, &cats, ev.hz);
        }

        // Advance countdown.
        {
            let cats = {
                let g = inner.lock();
                g.config.active_categories()
            };
            let mut g = inner.lock();
            handle_tick(&mut g, &listener, &cats, tick);
        }

        let elapsed = start.elapsed();
        if elapsed < tick {
            std::thread::sleep(tick - elapsed);
        }
    }
    debug!("engine driver stopped");
}

// ---------------------------------------------------------------------------
// Core logic (pure, testable)
// ---------------------------------------------------------------------------

fn pick_new_prompt(inner: &mut EngineInner, cats: &[ChallengeType]) {
    if cats.is_empty() {
        return;
    }
    let kind = cats[inner.rng.gen_range(0..cats.len())];
    let challenge = generate(kind, &mut inner.rng, &inner.library);
    inner.current = challenge;
    inner.matched = vec![false; inner.current.targets.len()];
    inner.next_idx = 0;
    inner.matched_count = 0;
    inner.stable_for_target = 0;
    debug!(
        "new prompt: {:?} '{}' ({} targets, ordered={})",
        inner.current.kind,
        inner.current.display,
        inner.total(),
        inner.current.ordered
    );
}

fn reset_timer(inner: &mut EngineInner) {
    if inner.config.random_mode {
        let secs = inner.rng.gen_range(10..=90);
        inner.remaining = Duration::from_secs(secs);
        inner.prompt_duration = inner.remaining;
    } else {
        inner.remaining = inner.default_duration;
        inner.prompt_duration = inner.default_duration;
    }
}

fn handle_pitch(
    inner: &mut EngineInner,
    listener: &Arc<dyn EngineListener>,
    cats: &[ChallengeType],
    hz: Option<f64>,
) {
    let midi = match hz.and_then(Note::from_hz) {
        Some(n) => Some(n.midi()),
        None => None,
    };
    // Track the latest detected pitch for stable-note display + set matching.
    inner.last_detected_midi = midi;
    // Always emit the detected note so the UI can show what the mic hears,
    // even when it doesn't match the current prompt.
    if let Some(m) = midi {
        if let Some(name) = note_name(m) {
            if inner.last_emitted_note.as_deref() != Some(name.as_str()) {
                inner.last_emitted_note = Some(name.clone());
                emit(listener, EngineEvent::DetectedNote(Some(name)));
            }
        }
    }
    // Match logic: does the detected MIDI match the current required target?
    let required_midi: Option<u8> = required_target_midi(inner);
    match (midi, required_midi) {
        (Some(m), Some(req)) if m == req => {
            inner.stable_for_target = inner.stable_for_target.saturating_add(1).min(u8::MAX);

            if inner.stable_for_target >= 2 {
                // Accept the match.
                inner.stable_for_target = 0;
                accept_match(inner, listener, cats);
            }
        }
        _ => {
            // Mismatch or no target: reset the stability counter.
            inner.stable_for_target = 0;
        }
    }
}

fn required_target_midi(inner: &EngineInner) -> Option<u8> {
    if inner.current.targets.is_empty() {
        return None;
    }
    if inner.current.ordered {
        let idx = inner.next_idx.min(inner.current.targets.len() - 1);
        if inner.matched.get(idx).copied().unwrap_or(false) {
            return None;
        }
        Some(inner.current.targets[idx].midi())
    } else {
        // Pick the first not-yet-matched target whose MIDI equals the last
        // detected pitch if possible; otherwise the first unmatched one. The
        // stability counter only advances when the *required* frame midi
        // actually equals the detection — see handle_pitch's check.
        inner
            .matched
            .iter()
            .position(|m| !m)
            .map(|i| inner.current.targets[i].midi())
    }
}

fn accept_match(
    inner: &mut EngineInner,
    listener: &Arc<dyn EngineListener>,
    cats: &[ChallengeType],
) {
    let total = inner.total();
    let idx = if inner.current.ordered {
        let i = inner.next_idx.min(total.saturating_sub(1));
        inner.matched[i] = true;
        inner.next_idx = i + 1;
        inner.matched_count += 1;
        i
    } else {
        let detected = inner.last_detected_midi;
        let i = inner
            .matched
            .iter()
            .position(|m| !m)
            .and_then(|i| {
                detected.and_then(|d| {
                    if inner.current.targets[i].midi() == d {
                        Some(i)
                    } else {
                        None
                    }
                })
            });
        match i {
            Some(i) => {
                inner.matched[i] = true;
                inner.matched_count += 1;
                i
            }
            None => return,
        }
    };
    emit(
        listener,
        EngineEvent::Matched {
            index: idx as u64,
            total: total as u64,
        },
    );

    inner.stable_for_target = 0;

    if inner.matched_count >= total {
        emit(listener, EngineEvent::Passed);
        inner.score_passed += 1;
        inner.score_total += 1;
        emit_score(listener, inner);
        pick_new_prompt(inner, cats);
        reset_timer(inner);
        emit_prompt(listener, inner);
        emit_score(listener, inner);
    }
}

fn handle_tick(
    inner: &mut EngineInner,
    listener: &Arc<dyn EngineListener>,
    cats: &[ChallengeType],
    dt: Duration,
) {
    if inner.remaining == Duration::ZERO {
        return;
    }
    inner.remaining = inner.remaining.saturating_sub(dt);
    if inner.remaining == Duration::ZERO {
        emit(listener, EngineEvent::Timeout);
        inner.score_total += 1;
        emit_score(listener, inner);
        pick_new_prompt(inner, cats);
        reset_timer(inner);
        emit_prompt(listener, inner);
        emit_score(listener, inner);
    }
}

// ---------------------------------------------------------------------------
// Event emission helpers
// ---------------------------------------------------------------------------

fn emit(listener: &Arc<dyn EngineListener>, ev: EngineEvent) {
    listener.on_event(ev);
}

fn emit_prompt(listener: &Arc<dyn EngineListener>, inner: &EngineInner) {
    emit(listener, EngineEvent::Prompt(ChallengeView::from(&inner.current)));
}

fn emit_score(listener: &Arc<dyn EngineListener>, inner: &EngineInner) {
    emit(
        listener,
        EngineEvent::Score {
            passed: inner.score_passed,
            total: inner.score_total,
        },
    );
}

fn note_name(midi: u8) -> Option<String> {
    Some(Note::from_midi_clamped(midi).name())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A listener that records all events in order.
    type Evs = Arc<StdMutex<Vec<EngineEvent>>>;
    /// A listener that records all events in order.
    struct Recorder {
        events: Evs,
    }
    impl Recorder {
        fn new() -> (Arc<Self>, Evs) {
            let evs: Evs = Arc::new(StdMutex::new(Vec::new()));
            let rec = Arc::new(Recorder {
                events: Arc::clone(&evs),
            });
            (rec, evs)
        }
    }
    impl EngineListener for Recorder {
        fn on_event(&self, ev: EngineEvent) {
            self.events.lock().unwrap().push(ev);
        }
    }

    fn make_engine(rec: &Arc<Recorder>, config: Config) -> Engine {
        Engine::new_with_rng_seed(
            config,
            Box::new(ListenerShim(Arc::clone(rec))),
            Some(7),
        )
        .unwrap()
    }

    // Bridge an Arc<Recorder> into a Box<dyn EngineListener> for Engine::new.
    struct ListenerShim(Arc<Recorder>);
    impl EngineListener for ListenerShim {
        fn on_event(&self, ev: EngineEvent) {
            self.0.on_event(ev);
        }
    }

    fn collect(evs: &Arc<StdMutex<Vec<EngineEvent>>>) -> Vec<EngineEvent> {
        evs.lock().unwrap().clone()
    }

    fn emit_prompt_once(eng: &Engine) {
        // Tests invoke on_pitch/on_tick headlessly; the first Prompt is only
        // emitted by start(). For headless tests we manually pick a prompt via
        // on_tick forcing a timeout, which emits Prompt. Helper: start the
        // prompt chain by setting an initial prompt (mirrors start() minus audio).
        let cats = eng.inner.lock().config.active_categories();
        let mut inner = eng.inner.lock();
        pick_new_prompt(&mut inner, &cats);
        reset_timer(&mut inner);
        emit_prompt(&eng.listener, &inner);
        emit_score(&eng.listener, &inner);
    }

    #[test]
    fn new_engine_emits_no_events_until_started() {
        // Engine::new must not start the loop; we only assert it constructs.
        let (rec, evs) = Recorder::new();
        let _eng = make_engine(&rec, Config::default());
        assert!(evs.lock().unwrap().is_empty(), "no events before start()");
    }

    #[test]
    fn note_prompt_matched_then_passed_and_new_prompt() {
        let (rec, evs) = Recorder::new();
        let mut cfg = Config::default();
        cfg.enabled = crate::config::EnabledCategory::Note.into();
        let eng = make_engine(&rec, cfg);
        emit_prompt_once(&eng);

        let target_hz = {
            let g = eng.inner.lock();
            g.current.targets[0].hz()
        };

        // Drive a single matching pitch twice (stability ≥2 → accept).
        eng.on_pitch(Some(target_hz));
        eng.on_pitch(Some(target_hz));
        // In-hand: the note prompt is unordered with one target; one accept → Passed.
        // Note: Note challenges may sometimes be ordered? They are `ordered=false`.
        let got = collect(&evs);
        assert!(
            got.iter().any(|e| matches!(e, EngineEvent::Matched { .. })),
            "expected Matched: {got:?}"
        );
        assert!(
            got.iter().any(|e| matches!(e, EngineEvent::Passed)),
            "expected Passed: {got:?}"
        );
        let prompt_count = got
            .iter()
            .filter(|e| matches!(e, EngineEvent::Prompt(_)))
            .count();
        assert!(prompt_count >= 2, "expected a new Prompt after Pass");
        // Latest Score: passed=1, total=1.
        let score = got
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Score { passed, total } => Some((*passed, *total)),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(score, (1, 1));
    }

    #[test]
    fn timeout_emits_timeout_and_new_prompt() {
        let (rec, evs) = Recorder::new();
        let mut cfg = Config::default();
        cfg.default_duration_sec = 1; // 1 second prompts
        let eng = make_engine(&rec, cfg);
        emit_prompt_once(&eng);

        eng.on_tick(Duration::from_millis(1100));
        let got = collect(&evs);
        assert!(
            got.iter().any(|e| matches!(e, EngineEvent::Timeout)),
            "expected Timeout: {got:?}"
        );
        let prompt_count = got
            .iter()
            .filter(|e| matches!(e, EngineEvent::Prompt(_)))
            .count();
        assert!(prompt_count >= 2, "expected a new Prompt after Timeout");
    }

    #[test]
    fn random_mode_durations_span_10_90s() {
        let mut cfg = Config::default();
        cfg.random_mode = true;
        let (rec, _) = Recorder::new();
        let eng = make_engine(&rec, cfg);
        emit_prompt_once(&eng);

        let mut durations = Vec::new();
        for _ in 0..16 {
            eng.on_tick(Duration::from_secs(120));
            let g = eng.inner.lock();
            durations.push(g.remaining.as_secs());
        }
        for d in &durations {
            assert!((10..=90).contains(d), "random duration {d}s out of [10,90]");
        }
        let distinct: std::collections::HashSet<_> = durations.iter().collect();
        assert!(
            distinct.len() > 1,
            "random durations all identical: {durations:?}"
        );
    }

    #[test]
    fn ordered_scale_requires_in_order_matching() {
        let (rec, evs) = Recorder::new();
        let mut cfg = Config::default();
        cfg.enabled =
            enumset::EnumSet::from(crate::config::EnabledCategory::Scale);
        let eng = make_engine(&rec, cfg);
        // Force a Scale prompt by trying until current is a scale. With only Scale
        // enabled, pick_new_prompt always yields a Scale.
        let ordered = eng.inner.lock().current.ordered;
        assert!(ordered, "Scale must be ordered");

        // Need a prompt with at least 2 targets. Scales have 5-7; require one.
        let targets = {
            let g = eng.inner.lock();
            g.current.targets.clone()
        };
        if targets.len() < 2 {
            // Re-roll by emitting a new prompt; deterministic seed: re-pick.
            let cats = eng.inner.lock().config.active_categories();
            let mut g = eng.inner.lock();
            pick_new_prompt(&mut g, &cats);
        }
        let targets = eng.inner.lock().current.targets.clone();
        assert!(targets.len() >= 2, "need ≥2 scale targets for ordering test");

        let second_hz = targets[1].hz();
        eng.on_pitch(Some(second_hz));
        eng.on_pitch(Some(second_hz));
        // Out-of-order pitch must not count.
        {
            let after = eng.inner.lock();
            assert_eq!(after.next_idx, 0);
            assert_eq!(after.matched_count, 0);
        }

        let first_hz = targets[0].hz();
        eng.on_pitch(Some(first_hz));
        eng.on_pitch(Some(first_hz));
        let got = collect(&evs);
        {
            let after = eng.inner.lock();
            assert_eq!(after.next_idx, 1);
            assert_eq!(after.matched_count, 1);
        }
        assert!(got
            .iter()
            .any(|e| matches!(e, EngineEvent::Matched { index: 0, .. })));
    }
}