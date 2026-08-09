//! Guitar Practice Trainer — Mac TUI (`guitar-trainer` binary).
//!
//! Fully arrow-key driven. No memorized hotkeys:
//!   - Up/Down    navigate vertical lists (Menu, Settings, Device picker)
//!   - Left/Right navigate the horizontal action bar during Practice
//!   - Enter/Space activate the highlighted item
//!   - Esc        go back / cancel
//! The only exception is typing the timer seconds or a custom content path,
//! which unavoidably need the keyboard — everything else is pure navigation.

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use tui_big_text::{BigText, PixelSize};
use scopeguard::defer;

use guitar_trainer_core::audio;
use guitar_trainer_core::challenges::ChallengeType;
use guitar_trainer_core::config::{Config, EnabledCategory};
use guitar_trainer_core::engine::{Engine, EngineEvent, EngineListener};
use guitar_trainer_core::note::Note;
use guitar_trainer_core::theme::Theme;
use guitar_trainer_core::tuning::TuningId;

/// One completed prompt, kept for the Practice screen's "recent attempts"
/// list (see `UiState::history`). Built purely from events the engine
/// already streams — no `crates/core` changes.
#[derive(Clone)]
enum AttemptResult {
    Passed,
    TimedOut,
}

#[derive(Clone)]
struct Attempt {
    kind: String,
    display: String,
    result: AttemptResult,
}

/// Bound on `UiState::history` — "last ~8 prompts" per the redesign plan.
const HISTORY_CAP: usize = 8;

/// UI-facing snapshot derived from engine events + `engine.progress()`.
#[derive(Default, Clone)]
struct UiState {
    prompt_display: String,
    prompt_kind: String,
    targets: Vec<String>,
    ordered: bool,
    matched: usize,
    /// Per-target matched flags (same length as `targets`); drives the
    /// green matched-target highlight independent of the aggregate count.
    matched_indices: Vec<bool>,
    detected_note: Option<String>,
    score_passed: u32,
    score_total: u32,
    /// Fraction of time remaining, `[0,1]`.
    time_left_frac: f64,
    time_left_secs: u64,
    prompt_secs: u64,
    running: bool,
    /// One-line status/error banner (e.g. mic permission failure).
    status: Option<String>,
    /// Wall-clock start of the current post-match cooldown, if active.
    cooldown_started: Option<std::time::Instant>,
    /// Duration of the current/most recent cooldown.
    cooldown_ms: u64,
    /// Rolling window of recently completed prompts (pass/timeout), most
    /// recent last. TUI-local bookkeeping for the Practice screen's Session
    /// panel — capped at `HISTORY_CAP`.
    history: VecDeque<Attempt>,
}

/// Channel-backed listener: the render loop drains `rx`.
struct ChannelListener {
    tx: Sender<EngineEvent>,
}
impl EngineListener for ChannelListener {
    fn on_event(&self, ev: EngineEvent) {
        // Non-blocking: dropping on full is fine (render polls anyway).
        let _ = self.tx.send(ev);
    }
}

// ---------------------------------------------------------------------------
// Navigation state
// ---------------------------------------------------------------------------

/// Which full-screen view is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Menu,
    Practice,
    Settings,
    DevicePick,
}

/// What's currently being typed, if anything.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    None,
    Timer,
    Path,
}

const MENU_ITEMS: [&str; 3] = ["Start Practice", "Settings", "Quit"];
const PRACTICE_ACTIONS: [&str; 3] = ["Stop", "Skip", "Settings"];
/// Settings rows: 0=Timer 1=Tuning 2=Random 3..=9=categories(7) 10=Audio Device 11=Custom Path 12=Back.
const SETTINGS_ROW_COUNT: usize = 13;

struct App {
    screen: Screen,
    menu_idx: usize,
    practice_idx: usize,
    settings_idx: usize,
    device_idx: usize,
    edit: Edit,
    edit_buf: String,
    devices: Vec<String>,
    device_scan_rx: Option<mpsc::Receiver<Vec<String>>>,
}

impl App {
    fn new() -> Self {
        App {
            screen: Screen::Menu,
            menu_idx: 0,
            practice_idx: 0,
            settings_idx: 0,
            device_idx: 0,
            edit: Edit::None,
            edit_buf: String::new(),
            devices: Vec::new(),
            device_scan_rx: None,
        }
    }
}

/// Kick off a background scan for input devices; result collected in the main
/// loop via `drain_device_scan`. Runs off-thread so cpal enumeration (which
/// can take noticeable time) never blocks key handling or rendering.
fn start_device_scan(app: &mut App) {
    let (tx, rx) = mpsc::channel();
    app.device_scan_rx = Some(rx);
    std::thread::spawn(move || {
        let devices = audio::enumerate_input_devices();
        let _ = tx.send(devices);
    });
}

fn drain_device_scan(app: &mut App) {
    if let Some(rx) = &app.device_scan_rx {
        if let Ok(devices) = rx.try_recv() {
            app.devices = devices;
            app.device_scan_rx = None;
        }
    }
}

fn main() -> Result<()> {
    // Terminal setup.
    enable_raw_mode()?;
    // This app is a full-screen TUI whose color carries semantics (yellow
    // selection, green matches, red warnings); it is not a plain stdout
    // program, so it opts out of the NO_COLOR convention that crossterm
    // honors by default. Without this, a NO_COLOR env var (even one set by
    // a shell wrapper) silently strips every color from the whole UI.
    crossterm::style::Colored::set_ansi_color_disabled(false);
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    defer! {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Engine + event channel.
    let (tx, rx) = crossbeam_channel::bounded::<EngineEvent>(512);
    let listener = Box::new(ChannelListener { tx });
    let config = guitar_trainer_core::config::load();
    let theme = guitar_trainer_core::theme::load();
    let engine = Engine::new(config, listener)?;
    let engine = Arc::new(engine);

    let mut ui = UiState::default();
    let mut settings = SettingsState::from_engine(&engine);
    let mut app = App::new();

    loop {
        // Drain engine events + background device scan into UI state.
        drain_events(&rx, &mut ui, &engine);
        drain_device_scan(&mut app);

        // Poll for terminal key events (non-blocking, 16 ms).
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(k) = event::read()? {
                // Accept Press + Repeat, reject Release — robust across
                // terminals that report key-up events (e.g. Kitty protocol).
                if k.kind != KeyEventKind::Release {
                    if !handle_key(k, &mut app, &mut settings, &engine, &mut ui) {
                        break; // Quit requested.
                    }
                }
            }
        }

        // Render.
        terminal.draw(|f| {
            let area = f.area();
            match app.screen {
                Screen::Menu => draw_menu(f, area, &app, &ui, &settings, &theme),
                Screen::Practice => draw_practice(f, area, &app, &ui, &settings, &theme),
                Screen::Settings => draw_settings(f, area, &app, &settings, &theme),
                Screen::DevicePick => draw_device_pick(f, area, &app, &settings, &theme),
            }
        })?;
    }

    engine.stop();
    Ok(())
}

// ---------------------------------------------------------------------------
// Event draining
// ---------------------------------------------------------------------------

fn drain_events(rx: &Receiver<EngineEvent>, ui: &mut UiState, engine: &Engine) {
    loop {
        match rx.try_recv() {
            Ok(ev) => apply_event(ui, &ev),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    // Refresh progress every frame (the timer advances on the engine thread).
    let (frac, secs, total) = engine.progress();
    ui.time_left_frac = frac;
    ui.time_left_secs = secs;
    ui.prompt_secs = total;
}

/// Record a just-finished prompt into the rolling history, capped at
/// `HISTORY_CAP`. Called from `apply_event` on `Passed`/`Timeout`, using the
/// prompt kind/display already tracked on `ui` (still the just-completed
/// prompt's — the next `Prompt` event hasn't landed yet).
fn push_attempt(ui: &mut UiState, result: AttemptResult) {
    ui.history.push_back(Attempt {
        kind: ui.prompt_kind.clone(),
        display: ui.prompt_display.clone(),
        result,
    });
    while ui.history.len() > HISTORY_CAP {
        ui.history.pop_front();
    }
}

fn apply_event(ui: &mut UiState, ev: &EngineEvent) {
    match ev {
        EngineEvent::Prompt(v) => {
            ui.prompt_display = v.display.clone();
            ui.prompt_kind = v.kind.clone();
            ui.targets = v.targets.clone();
            ui.ordered = v.ordered;
            ui.matched = 0;
            ui.matched_indices = vec![false; v.targets.len()];
        }
        EngineEvent::DetectedNote(n) => ui.detected_note = n.clone(),
        EngineEvent::Matched { index, total: _ } => {
            if let Some(slot) = ui.matched_indices.get_mut(*index as usize) {
                *slot = true;
            }
            ui.matched = ui.matched_indices.iter().filter(|&&m| m).count();
        }
        EngineEvent::Passed => push_attempt(ui, AttemptResult::Passed),
        EngineEvent::Cooldown { duration_ms } => {
            ui.cooldown_started = Some(std::time::Instant::now());
            ui.cooldown_ms = *duration_ms;
        }
        EngineEvent::Timeout => {
            push_attempt(ui, AttemptResult::TimedOut);
            ui.matched = 0;
        }
        EngineEvent::Score { passed, total } => {
            ui.score_passed = *passed;
            ui.score_total = *total;
        }
    }
}

// ---------------------------------------------------------------------------
// Settings staging
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct SettingsState {
    duration_sec: u32,
    tuning: TuningId,
    enabled: Vec<(ChallengeType, bool)>,
    random_mode: bool,
    custom_path: String,
    audio_device: Option<String>,
    /// Not user-editable in this screen; preserved so saving other settings
    /// never clobbers the persisted cooldown duration back to a default.
    match_pause_ms: u32,
}

impl SettingsState {
    fn from_engine(engine: &Engine) -> Self {
        let cfg = engine.config();
        let enabled = ChallengeType::ALL
            .iter()
            .map(|&c| {
                let ec: EnabledCategory = c.into();
                (c, cfg.enabled.contains(ec))
            })
            .collect();
        SettingsState {
            duration_sec: cfg.default_duration_sec,
            tuning: cfg.tuning,
            enabled,
            random_mode: cfg.random_mode,
            custom_path: cfg
                .custom_content_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            audio_device: cfg.audio_device_name.clone(),
            match_pause_ms: cfg.match_pause_ms,
        }
    }

    fn to_config(&self) -> Config {
        let mut set = enumset::EnumSet::new();
        for (c, on) in &self.enabled {
            if *on {
                set.insert(EnabledCategory::from(*c));
            }
        }
        if set.is_empty() {
            set = enumset::EnumSet::all();
        }
        Config {
            default_duration_sec: self.duration_sec.max(1),
            tuning: self.tuning,
            enabled: set,
            random_mode: self.random_mode,
            custom_content_path: if self.custom_path.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(&self.custom_path))
            },
            audio_device_name: self.audio_device.clone(),
            match_pause_ms: self.match_pause_ms,
        }
    }
}

/// Push staged settings into the engine + persist. Does not touch the audio
/// stream — safe to call for category/timer/random-mode changes while running.
fn apply_settings(settings: &SettingsState, engine: &Engine) {
    let cfg = settings.to_config();
    engine.set_config(cfg.clone());
    let _ = guitar_trainer_core::config::save(&cfg);
}

/// Like `apply_settings`, but also restarts the audio stream if practice is
/// currently running — required when the input device itself changes.
fn apply_device_change(settings: &SettingsState, engine: &Engine, ui: &mut UiState) {
    apply_settings(settings, engine);
    if ui.running {
        engine.stop();
        match engine.start() {
            Ok(()) => ui.status = None,
            Err(e) => {
                ui.running = false;
                ui.status = Some(format!("mic error: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Returns `false` to quit the app, `true` to keep running.
fn handle_key(
    k: KeyEvent,
    app: &mut App,
    settings: &mut SettingsState,
    engine: &Arc<Engine>,
    ui: &mut UiState,
) -> bool {
    // Text-entry mode intercepts all keys until committed (Enter) or
    // cancelled (Esc). This is the only place raw typing happens.
    if app.edit != Edit::None {
        match k.code {
            KeyCode::Enter => commit_edit(app, settings, engine),
            KeyCode::Esc => {
                app.edit = Edit::None;
                app.edit_buf.clear();
            }
            KeyCode::Backspace => {
                app.edit_buf.pop();
            }
            KeyCode::Char(c) => {
                let ok = match app.edit {
                    Edit::Timer => c.is_ascii_digit() && app.edit_buf.len() < 4,
                    Edit::Path => (c.is_ascii_graphic() || c == ' ') && app.edit_buf.len() < 200,
                    Edit::None => false,
                };
                if ok {
                    app.edit_buf.push(c);
                }
            }
            _ => {}
        }
        return true;
    }

    match app.screen {
        Screen::Menu => handle_menu_key(k, app, engine, ui, settings),
        Screen::Practice => handle_practice_key(k, app, engine, ui, settings),
        Screen::Settings => handle_settings_key(k, app, settings, engine, ui),
        Screen::DevicePick => handle_device_pick_key(k, app, settings, engine, ui),
    }
}

fn commit_edit(app: &mut App, settings: &mut SettingsState, engine: &Engine) {
    match app.edit {
        Edit::Timer => {
            if let Ok(n) = app.edit_buf.parse::<u32>() {
                settings.duration_sec = n.max(1);
            }
        }
        Edit::Path => {
            settings.custom_path = app.edit_buf.clone();
        }
        Edit::None => {}
    }
    apply_settings(settings, engine);
    app.edit = Edit::None;
    app.edit_buf.clear();
}

fn handle_menu_key(
    k: KeyEvent,
    app: &mut App,
    engine: &Arc<Engine>,
    ui: &mut UiState,
    settings: &mut SettingsState,
) -> bool {
    match k.code {
        KeyCode::Up => app.menu_idx = (app.menu_idx + MENU_ITEMS.len() - 1) % MENU_ITEMS.len(),
        KeyCode::Down => app.menu_idx = (app.menu_idx + 1) % MENU_ITEMS.len(),
        KeyCode::Enter | KeyCode::Char(' ') => match app.menu_idx {
            0 => match engine.start() {
                Ok(()) => {
                    ui.running = true;
                    ui.status = None;
                    app.screen = Screen::Practice;
                    app.practice_idx = 0;
                }
                Err(e) => ui.status = Some(format!("mic error: {e}")),
            },
            1 => {
                *settings = SettingsState::from_engine(engine);
                app.settings_idx = 0;
                app.screen = Screen::Settings;
            }
            2 => return false,
            _ => {}
        },
        KeyCode::Esc => return false,
        _ => {}
    }
    true
}

fn handle_practice_key(
    k: KeyEvent,
    app: &mut App,
    engine: &Arc<Engine>,
    ui: &mut UiState,
    settings: &mut SettingsState,
) -> bool {
    match k.code {
        KeyCode::Left => {
            app.practice_idx = (app.practice_idx + PRACTICE_ACTIONS.len() - 1) % PRACTICE_ACTIONS.len()
        }
        KeyCode::Right => app.practice_idx = (app.practice_idx + 1) % PRACTICE_ACTIONS.len(),
        KeyCode::Enter | KeyCode::Char(' ') => match app.practice_idx {
            0 => {
                engine.stop();
                ui.running = false;
                app.screen = Screen::Menu;
                app.menu_idx = 0;
            }
            1 => engine.skip(),
            2 => {
                *settings = SettingsState::from_engine(engine);
                app.settings_idx = 0;
                app.screen = Screen::Settings;
            }
            _ => {}
        },
        KeyCode::Esc => {
            engine.stop();
            ui.running = false;
            app.screen = Screen::Menu;
            app.menu_idx = 0;
        }
        _ => {}
    }
    true
}

fn handle_settings_key(
    k: KeyEvent,
    app: &mut App,
    settings: &mut SettingsState,
    engine: &Arc<Engine>,
    ui: &UiState,
) -> bool {
    match k.code {
        KeyCode::Up => {
            app.settings_idx = (app.settings_idx + SETTINGS_ROW_COUNT - 1) % SETTINGS_ROW_COUNT
        }
        KeyCode::Down => app.settings_idx = (app.settings_idx + 1) % SETTINGS_ROW_COUNT,
        KeyCode::Enter | KeyCode::Char(' ') => match app.settings_idx {
            0 => {
                app.edit = Edit::Timer;
                app.edit_buf = settings.duration_sec.to_string();
            }
            1 => {
                let idx = TuningId::ALL.iter().position(|&t| t == settings.tuning).unwrap_or(0);
                settings.tuning = TuningId::ALL[(idx + 1) % TuningId::ALL.len()];
            }
            2 => settings.random_mode = !settings.random_mode,
            3..=9 => {
                let i = app.settings_idx - 3;
                if let Some(slot) = settings.enabled.get_mut(i) {
                    slot.1 = !slot.1;
                }
            }
            10 => {
                start_device_scan(app);
                app.device_idx = settings
                    .audio_device
                    .as_ref()
                    .and_then(|name| app.devices.iter().position(|d| d == name).map(|i| i + 1))
                    .unwrap_or(0);
                app.screen = Screen::DevicePick;
            }
            11 => {
                app.edit = Edit::Path;
                app.edit_buf = settings.custom_path.clone();
            }
            12 => {
                apply_settings(settings, engine);
                app.screen = if ui.running { Screen::Practice } else { Screen::Menu };
            }
            _ => {}
        },
        KeyCode::Esc => {
            apply_settings(settings, engine);
            app.screen = if ui.running { Screen::Practice } else { Screen::Menu };
        }
        _ => {}
    }
    true
}

fn handle_device_pick_key(
    k: KeyEvent,
    app: &mut App,
    settings: &mut SettingsState,
    engine: &Arc<Engine>,
    ui: &mut UiState,
) -> bool {
    let count = app.devices.len() + 1; // +1 for "(Default mic)".
    match k.code {
        KeyCode::Up => app.device_idx = (app.device_idx + count - 1) % count,
        KeyCode::Down => app.device_idx = (app.device_idx + 1) % count,
        KeyCode::Enter | KeyCode::Char(' ') => {
            settings.audio_device = if app.device_idx == 0 {
                None
            } else {
                app.devices.get(app.device_idx - 1).cloned()
            };
            apply_device_change(settings, engine, ui);
            app.screen = Screen::Settings;
        }
        KeyCode::Esc => app.screen = Screen::Settings,
        _ => {}
    }
    true
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Parse a `#RRGGBB` hex string into a ratatui `Color`; any parse failure
/// (missing file, malformed hex) falls back to `fallback` so a broken
/// `theme.toml` never breaks rendering.
fn parse_color(hex: &str, fallback: Color) -> Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return fallback;
    }
    let (Ok(r), Ok(g), Ok(b)) = (
        u8::from_str_radix(&hex[0..2], 16),
        u8::from_str_radix(&hex[2..4], 16),
        u8::from_str_radix(&hex[4..6], 16),
    ) else {
        return fallback;
    };
    Color::Rgb(r, g, b)
}

/// True if the terminal advertises 24-bit color (`COLORTERM=truecolor` /
/// `24bit`). macOS Terminal.app and other 256-color terminals omit this;
/// ratatui 0.28 has no capability probe, so the env check is the standard
/// heuristic (the one used by crossterm's own `supports_color` era, tmux,
/// and most CLI tools).
fn truecolor_terminal() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

/// Theme hex → ratatui `Color`. On truecolor terminals the exact hex value
/// is used; everywhere else the caller's *named* `fallback` is used instead
/// — named ANSI colors (Cyan/Yellow/Green/…) render in every terminal,
/// where raw `38;2` RGB sequences silently fall back to default on
/// non-truecolor emulators. Without this, a missing `COLORTERM` blanks all
/// theme color at once.
fn theme_color(hex: &str, fallback: Color) -> Color {
    if truecolor_terminal() {
        parse_color(hex, fallback)
    } else {
        fallback
    }
}

fn selection_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme_color(&theme.selection_fg, Color::Black))
        .bg(theme_color(&theme.selection_bg, Color::Yellow))
        .add_modifier(Modifier::BOLD)
}

// ---------------------------------------------------------------------------
// Layout thresholds — every screen below chooses a wide (multi-column /
// panel) or narrow (single-column) layout from these, and never leaves more
// than the deliberate Fill-spacer remainder unclaimed at any size.
// ---------------------------------------------------------------------------

const WIDE_COLS: u16 = 140;
const SHORT_ROWS: u16 = 30;

fn is_wide(area: Rect) -> bool {
    area.width >= WIDE_COLS
}

fn is_short(area: Rect) -> bool {
    area.height < SHORT_ROWS
}

// ---------------------------------------------------------------------------
// Hero panel subsection sizing — shared between `draw_practice` (which now
// sizes the panel's outer `Rect` to fill the available column, via
// `Constraint::Fill`) and `render_hero_prompt` (which splits that height
// into three equal subsections — Heading, Notes, Detected — with
// `HERO_SECTION_GAP` between each. Heading and Notes cap their inner
// content (`HERO_HEADING_CONTENT_H`, `HERO_CHIP_H`) and center it inside
// their box so it stays a fixed, comfortable size even as the section
// around it grows on a taller terminal; Detected's box has no cap — it
// fills its whole section directly, same as Heading's border does.
// ---------------------------------------------------------------------------
const HERO_TOP_MARGIN: u16 = 1;
/// Reserved for the prompt name regardless of length — sized to the
/// tallest glyph tier the name can render at (`PixelSize::Full`, 8 rows;
/// see `HERO_NAME_GLYPH_ROWS`), keeping the heading box's content height
/// stable across prompts whether or not a given name qualifies for glyph
/// rendering.
const HERO_NAME_H: u16 = 8;
const HERO_GAP1: u16 = 1;
const HERO_CAPTION_H: u16 = 1;
/// Fixed content height of the heading+subheading group (name + internal
/// gap + caption), centered inside its own bordered box — see
/// `render_heading_box`.
const HERO_HEADING_CONTENT_H: u16 = HERO_NAME_H + HERO_GAP1 + HERO_CAPTION_H;
/// Cap on the target-chip row's height, centered inside the Notes section
/// instead of stretched — keeps individual note chips a comfortable,
/// unchanged size even though the section around them grows on a tall
/// terminal.
const HERO_CHIP_H: u16 = 9;
const HERO_BOTTOM_MARGIN: u16 = 1;
/// Gap between the three subsections (Heading, Notes, Detected) — one
/// constant so every gap between rows is identical ("even padding and
/// margins" between the three, per the one-column/3-row layout request).
const HERO_SECTION_GAP: u16 = 2;

/// Terminal rows/columns spanned by one glyph of the note-chip / Detected
/// value text, rendered via `tui_big_text` at `PixelSize::HalfHeight` (not
/// `Quadrant`, which was tried here before and rejected as too
/// chunky/angular: both are 4 rows tall, but `HalfHeight` samples twice
/// the horizontal detail per character, so diagonal strokes step more
/// finely and read softer). Built from the same safe Block Elements range
/// (▀▄█, U+2580-259F) already verified glitch-free in this terminal — see
/// the Sextant→HalfHeight caption fix.
const HERO_GLYPH_ROWS: u16 = 4;
const HERO_GLYPH_COLS_PER_CHAR: u16 = 8;
/// Narrower fallback tier (`PixelSize::Quadrant`) for content that won't
/// fit `HERO_GLYPH_COLS_PER_CHAR`'s width at the given terminal size —
/// half the columns per glyph, same `HERO_GLYPH_ROWS` height. Used so a
/// wide chord/scale's chip row (many simultaneous targets) still renders
/// as glyph text at *some* size instead of silently dropping to plain
/// crisp text while a 1-2 target prompt next to it renders full-size —
/// that per-prompt size flip was the reported "chip font inconsistent"
/// bug. Plain text remains the last-resort fallback only for the rare
/// case that doesn't fit even this tier.
const HERO_GLYPH_COLS_PER_CHAR_NARROW: u16 = 4;
/// Row height of the prompt name at `PixelSize::Full` (8 rows — literally
/// double `HERO_GLYPH_ROWS`, matching the requested ~56pt-heading vs
/// ~36pt-chip size relationship while keeping the same
/// `HERO_GLYPH_COLS_PER_CHAR`-wide, softest-available horizontal
/// resolution). `render_heading_box` only uses it for names that are pure
/// ASCII: `font8x8::BASIC_FONTS` (the glyph table `tui_big_text` renders
/// from) has no entry for the en dash `–` used in every Progression name
/// ("I–IV–V–I in A2"), so glyph mode would silently render those dashes
/// as blank cells — the same class of bug already hit and fixed for the
/// idle "—" Detected placeholder. Progression names fall back to the
/// existing crisp letter-spaced text instead.
const HERO_NAME_GLYPH_ROWS: u16 = 8;

/// Dot-separated list of the currently enabled challenge categories, e.g.
/// "Note · Chord · Scale". Shown on the Menu and Practice session panels so
/// "what's enabled" is visible without opening Settings.
fn category_chips(settings: &SettingsState) -> String {
    let on: Vec<&str> = settings
        .enabled
        .iter()
        .filter(|(_, on)| *on)
        .map(|(c, _)| c.label())
        .collect();
    if on.is_empty() {
        "(none enabled)".to_string()
    } else {
        on.join(" · ")
    }
}

/// Score / device / tuning / categories — the read-only session facts shown
/// on both the Menu's "Last Session" card and the Practice screen's Session
/// panel, built once so the two can never drift apart.
fn session_summary_lines(ui: &UiState, settings: &SettingsState, theme: &Theme) -> Vec<Line<'static>> {
    let device_name = settings
        .audio_device
        .clone()
        .unwrap_or_else(|| "(default mic)".to_string());
    let label_style = Style::default().fg(Color::DarkGray);
    let value_style = Style::default().fg(theme_color(&theme.secondary, Color::Cyan));
    vec![
        Line::from(vec![
            Span::styled("Score      ", label_style),
            Span::styled(format!("✓ {}/{}", ui.score_passed, ui.score_total), value_style),
        ]),
        Line::from(vec![Span::styled("Device     ", label_style), Span::raw(device_name)]),
        Line::from(vec![
            Span::styled("Tuning     ", label_style),
            Span::raw(settings.tuning.label().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Categories ", label_style),
            Span::raw(category_chips(settings)),
        ]),
    ]
}

/// The Practice screen's session panel: the summary above plus a rolling
/// "recent attempts" list sourced from `ui.history`. Read-only and
/// non-focusable — it never participates in `app.practice_idx`.
/// `show_categories` is dropped in the compact (narrow-terminal) placement
/// to leave more room for the attempts list.
fn render_session_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    ui: &UiState,
    settings: &SettingsState,
    theme: &Theme,
    show_categories: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
        .title(" Session ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut summary = session_summary_lines(ui, settings, theme);
    if !show_categories {
        summary.pop();
    }
    let summary_h = summary.len() as u16;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(summary_h), Constraint::Length(1), Constraint::Fill(1)])
        .split(inner);

    f.render_widget(Paragraph::new(summary).wrap(Wrap { trim: true }), rows[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            "Recent",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )),
        rows[1],
    );

    let success_style = Style::default().fg(theme_color(&theme.success, Color::Green));
    let danger_style = Style::default().fg(theme_color(&theme.danger, Color::Red));
    if ui.history.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No attempts yet this session.",
                Style::default().fg(Color::DarkGray),
            )),
            rows[2],
        );
    } else {
        let items: Vec<ListItem> = ui
            .history
            .iter()
            .rev()
            .map(|a| {
                let (mark, style) = match a.result {
                    AttemptResult::Passed => ("✓", success_style),
                    AttemptResult::TimedOut => ("⏱", danger_style),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), style),
                    Span::styled(format!("{:<11}", a.kind), Style::default().fg(Color::DarkGray)),
                    Span::raw(a.display.clone()),
                ]))
            })
            .collect();
        f.render_widget(List::new(items), rows[2]);
    }
}

fn draw_menu(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    ui: &UiState,
    settings: &SettingsState,
    theme: &Theme,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Fill(1), Constraint::Length(3)])
        .split(area);

    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            "Guitar Practice Trainer",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            settings.tuning.label().to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    f.render_widget(header, chunks[0]);

    let list_h = (MENU_ITEMS.len() as u16 + 2).min(chunks[1].height);
    let wide = is_wide(area);

    let (menu_area, card_area) = if wide {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Fill(1)])
            .split(chunks[1]);
        let menu_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(1), Constraint::Length(list_h), Constraint::Fill(1)])
            .split(cols[0]);
        (menu_rows[1], cols[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_h), Constraint::Fill(1)])
            .split(chunks[1]);
        (rows[0], rows[1])
    };

    let items: Vec<ListItem> = MENU_ITEMS.iter().map(|s| ListItem::new(*s)).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Menu — ↑↓ select, Enter to activate "),
        )
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.menu_idx));
    f.render_stateful_widget(list, menu_area, &mut state);

    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Last Session ");
    let card_inner = card_block.inner(card_area);
    f.render_widget(card_block, card_area);
    f.render_widget(
        Paragraph::new(session_summary_lines(ui, settings, theme)).wrap(Wrap { trim: true }),
        card_inner,
    );

    let device_name = settings
        .audio_device
        .clone()
        .unwrap_or_else(|| "(default mic)".to_string());
    let (footer_text, footer_style) = match &ui.status {
        Some(msg) => (msg.clone(), Style::default().fg(theme_color(&theme.danger, Color::Red))),
        None => (
            format!("mic: {device_name}   ✓{}/{}", ui.score_passed, ui.score_total),
            Style::default().fg(theme_color(&theme.secondary, Color::Cyan)),
        ),
    };
    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .style(footer_style);
    f.render_widget(footer, chunks[2]);
}

/// Vertically centers a `content_h`-row block within `rect`, leaving any
/// leftover space split evenly above and below. Used to keep each hero
/// subsection's actual content (heading text, target chips, Detected box)
/// comfortably framed instead of stretching to fill its larger section.
fn center_v(rect: Rect, content_h: u16) -> Rect {
    if rect.height <= content_h {
        return rect;
    }
    let pad = (rect.height - content_h) / 2;
    Rect { x: rect.x, y: rect.y + pad, width: rect.width, height: content_h }
}

/// Hand-drawn rounded border with heavier top/bottom/side rules than
/// `BorderType::Rounded` — ratatui's box-drawing set has no glyph that is
/// both heavy-weight *and* rounded at the corners, so this pairs the
/// existing light rounded corners (╭╮╰╯, kept because the chips were
/// explicitly approved for their rounded look) with heavy straight lines
/// (━ ┃, the same Box Drawing block already used for the light rules
/// elsewhere in this file — no new/unverified Unicode range). Returns the
/// inner `Rect`, matching `Block::inner`.
fn render_thick_rounded_border(f: &mut ratatui::Frame<'_>, area: Rect, style: Style) -> Rect {
    if area.width < 2 || area.height < 2 {
        return area;
    }
    let buf = f.buffer_mut();
    let (x0, y0) = (area.x, area.y);
    let (x1, y1) = (area.x + area.width - 1, area.y + area.height - 1);
    buf.set_string(x0, y0, "╭", style);
    buf.set_string(x1, y0, "╮", style);
    buf.set_string(x0, y1, "╰", style);
    buf.set_string(x1, y1, "╯", style);
    if x1 > x0 + 1 {
        let h = "━".repeat((x1 - x0 - 1) as usize);
        buf.set_string(x0 + 1, y0, &h, style);
        buf.set_string(x0 + 1, y1, &h, style);
    }
    for y in (y0 + 1)..y1 {
        buf.set_string(x0, y, "┃", style);
        buf.set_string(x1, y, "┃", style);
    }
    Rect::new(x0 + 1, y0 + 1, area.width.saturating_sub(2), area.height.saturating_sub(2))
}

/// The notes to actually play — the single most important piece of
/// information during practice, and the hero panel's largest, boldest
/// element. Each target is its own hand-drawn chip: a heavier-weight
/// rounded border (`render_thick_rounded_border`, per the "thicker chip
/// border" request) framing the note value rendered as glyph text. Tries
/// `PixelSize::HalfHeight` first (the approved "36pt" chip size); if the
/// full chip row wouldn't fit at that width (many simultaneous targets —
/// a 6-7 note scale/mode), steps down to the narrower `PixelSize::Quadrant`
/// tier instead of dropping straight to small crisp text. This is what
/// fixes the reported bug where a 1-2 target prompt rendered big glyphs
/// but a many-target prompt silently fell back to tiny plain text right
/// next to it — every target count now gets *some* glyph tier at the same
/// two sizes, so chip weight reads consistently across prompts; only a
/// genuinely extreme case (very narrow terminal, many long labels) still
/// falls back to plain text so nothing is ever clipped. A checkmark glyph
/// isn't in the underlying 8x8 font, so match state is carried by color
/// alone (border + text turn green), the same convention already used by
/// `render_detected_indicator`. Ordered prompts chain the chips with an
/// arrow so the required sequence reads left to right; unordered prompts
/// space them evenly.
fn render_targets_row(f: &mut ratatui::Frame<'_>, area: Rect, ui: &UiState, theme: &Theme) {
    if ui.targets.is_empty() || area.height == 0 || area.width == 0 {
        return;
    }
    let success = Style::default().fg(theme_color(&theme.success, Color::Green)).add_modifier(Modifier::BOLD);
    let idle_border = Style::default().fg(theme_color(&theme.secondary, Color::Cyan));
    let idle_text = Style::default().fg(theme_color(&theme.secondary, Color::Cyan)).add_modifier(Modifier::BOLD);

    let gap_w: u16 = if ui.ordered { 5 } else { 3 };
    let n = ui.targets.len() as u16;

    let spaced = |t: &str| -> String { t.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ") };
    // Crisp fallback labels keep the checkmark prefix; glyph labels drop
    // it (unsupported by the 8x8 font) and rely on color for match state.
    let plain_labels: Vec<String> = ui
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let matched = ui.matched_indices.get(i).copied().unwrap_or(false);
            if matched { format!("\u{2713} {}", spaced(t)) } else { spaced(t) }
        })
        .collect();
    // Glyph cells already have their own built-in per-character padding
    // (the 8x8 font's own spacing), so — unlike the crisp fallback, which
    // needs manual letter-spacing to avoid reading cramped — glyph labels
    // skip it: keeps chips narrower, so more simultaneous targets qualify
    // for glyph rendering instead of falling back.
    let glyph_labels: Vec<String> = ui.targets.iter().map(|t| t.clone()).collect();
    let max_glyph_chars = glyph_labels.iter().map(|l| l.chars().count() as u16).max().unwrap_or(1);

    let plain_box_w = plain_labels.iter().map(|l| l.chars().count() as u16 + 6).max().unwrap_or(1);
    let full_box_w = max_glyph_chars * HERO_GLYPH_COLS_PER_CHAR + 6;
    let narrow_box_w = max_glyph_chars * HERO_GLYPH_COLS_PER_CHAR_NARROW + 6;
    let fits_height = area.height >= HERO_GLYPH_ROWS + 2;
    let full_fits = fits_height && full_box_w * n + gap_w * n.saturating_sub(1) <= area.width;
    let narrow_fits = fits_height && narrow_box_w * n + gap_w * n.saturating_sub(1) <= area.width;

    let (box_w, glyph_pixel_size) = if full_fits {
        (full_box_w, Some(PixelSize::HalfHeight))
    } else if narrow_fits {
        (narrow_box_w, Some(PixelSize::Quadrant))
    } else {
        (plain_box_w, None)
    };
    let content_w = (box_w * n + gap_w * n.saturating_sub(1)).min(area.width);

    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(content_w), Constraint::Fill(1)])
        .split(area);

    let mut cell_constraints = Vec::with_capacity(ui.targets.len() * 2);
    for i in 0..ui.targets.len() {
        if i > 0 {
            cell_constraints.push(Constraint::Length(gap_w));
        }
        cell_constraints.push(Constraint::Length(box_w));
    }
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(cell_constraints)
        .split(outer[1]);

    let mut ci = 0usize;
    for i in 0..ui.targets.len() {
        if i > 0 {
            if ui.ordered {
                f.render_widget(
                    Paragraph::new("→").alignment(Alignment::Center).style(idle_border),
                    center_v(cells[ci], 1),
                );
            }
            ci += 1;
        }
        let matched = ui.matched_indices.get(i).copied().unwrap_or(false);
        let rect = cells[ci];
        ci += 1;
        let (border_style, text_style) = if matched { (success, success) } else { (idle_border, idle_text) };
        let inner = render_thick_rounded_border(f, rect, border_style);
        if let Some(pixel_size) = glyph_pixel_size {
            let glyph = BigText::builder()
                .pixel_size(pixel_size)
                .style(text_style)
                .alignment(Alignment::Center)
                .lines(vec![Line::from(glyph_labels[i].clone())])
                .build();
            f.render_widget(glyph, center_v(inner, HERO_GLYPH_ROWS.min(inner.height)));
        } else {
            f.render_widget(
                Paragraph::new(plain_labels[i].clone()).alignment(Alignment::Center).style(text_style),
                center_v(inner, 1),
            );
        }
    }
}

/// True if the currently detected note is one of the prompt's still-needed
/// targets — `None` when nothing is detected right now. For ordered
/// prompts only the *next* required target counts (matching
/// `crates/core::engine::required_target_midi`'s rule); for unordered
/// prompts any remaining target counts. This is a TUI-local comparison
/// over note *names* — `EngineEvent::DetectedNote` and
/// `ChallengeView::targets` both come from the same `Note::name()`
/// formatter in `crates/core`, so string equality is exact and no core
/// change is needed.
fn detected_correctness(ui: &UiState) -> Option<bool> {
    let note = ui.detected_note.as_deref()?;
    if ui.ordered {
        let next = ui.matched_indices.iter().position(|m| !m)?;
        Some(ui.targets.get(next).map(String::as_str) == Some(note))
    } else {
        Some(ui.targets.iter().enumerate().any(|(i, t)| {
            !ui.matched_indices.get(i).copied().unwrap_or(false) && t == note
        }))
    }
}

/// What the mic currently hears — placed directly beneath the target-note
/// chips (inside the hero panel), colored by correctness: green once the
/// sounded note is one of the still-needed targets, red when a note is
/// heard but it isn't one of them, neutral while nothing is detected.
/// During the post-match cooldown it keeps the existing blink cue
/// (already-confirmed success) instead of the correctness color. An
/// actual note renders at the same enlarged `PixelSize::HalfHeight` glyph
/// size as the note chips (see `render_targets_row`) so the readout
/// matches their weight; the idle "—" placeholder falls back to plain
/// text — an em dash has no glyph in the underlying 8x8 font and would
/// render blank.
fn render_detected_indicator(f: &mut ratatui::Frame<'_>, area: Rect, ui: &UiState, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let success = Style::default().fg(theme_color(&theme.success, Color::Green)).add_modifier(Modifier::BOLD);
    let danger = Style::default().fg(theme_color(&theme.danger, Color::Red)).add_modifier(Modifier::BOLD);
    let neutral = Style::default().add_modifier(Modifier::BOLD);
    let idle_border = Style::default().fg(theme_color(&theme.secondary, Color::Cyan));

    let cooldown_active = ui
        .cooldown_started
        .map(|t| t.elapsed() < Duration::from_millis(ui.cooldown_ms))
        .unwrap_or(false);
    let (text_style, border_style) = if cooldown_active {
        let blink_on = (ui.cooldown_started.unwrap().elapsed().as_millis() / 200) % 2 == 0;
        let s = if blink_on { success.add_modifier(Modifier::REVERSED) } else { success };
        (s, success)
    } else {
        match detected_correctness(ui) {
            Some(true) => (success, success),
            Some(false) => (danger, danger),
            None => (neutral, idle_border),
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(" Detected ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    match ui.detected_note.as_deref() {
        Some(note) => {
            let glyph_w = note.chars().count() as u16 * HERO_GLYPH_COLS_PER_CHAR;
            if glyph_w <= inner.width && inner.height >= HERO_GLYPH_ROWS {
                let glyph = BigText::builder()
                    .pixel_size(PixelSize::HalfHeight)
                    .style(text_style)
                    .alignment(Alignment::Center)
                    .lines(vec![Line::from(note.to_string())])
                    .build();
                f.render_widget(glyph, center_v(inner, HERO_GLYPH_ROWS));
            } else {
                let spaced: String = note.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
                f.render_widget(
                    Paragraph::new(spaced).alignment(Alignment::Center).style(text_style),
                    center_v(inner, 1),
                );
            }
        }
        None => {
            f.render_widget(
                Paragraph::new("—").alignment(Alignment::Center).style(text_style),
                center_v(inner, 1),
            );
        }
    }
}

/// The prompt name + match-count caption, boxed as its own bordered
/// subsection — the sibling of the Notes chips and the Detected box, all
/// three now spread evenly inside the hero panel (see `render_hero_prompt`).
/// The name renders as glyph text one size class above the note chips
/// (`PixelSize::Full`, 8 rows, falling back to `Quadrant`, 4 rows, if the
/// name is too wide — the same two-tier system `render_targets_row` uses),
/// so heading and chip weight read as one consistent, deliberately-scaled
/// family instead of one being glyph text and the other plain. Names
/// containing non-ASCII characters — every Progression display contains
/// the en dash `–` ("I–IV–V–I in A2"), which has no glyph in the
/// `font8x8::BASIC_FONTS` table `tui_big_text` renders from — skip glyph
/// mode entirely and keep the existing crisp letter-spaced text, since
/// glyph mode would silently render those dashes as blank cells. The
/// caption stays crisp, single-row text at all times: it's a short
/// fixed-format status line ("N OF M MATCHED"), not the prompt's headline.
fn render_heading_box(f: &mut ratatui::Frame<'_>, area: Rect, ui: &UiState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let name = ui.prompt_display.trim();
    let caption = if ui.ordered {
        format!("{} OF {} MATCHED — IN ORDER", ui.matched, ui.targets.len())
    } else {
        format!("{} OF {} MATCHED", ui.matched, ui.targets.len())
    };
    // Heading: accent yellow — the first colored thing the eye lands on.
    let name_style = Style::default().fg(theme_color(&theme.accent, Color::Yellow)).add_modifier(Modifier::BOLD);
    // Subheading: cyan by default, switching to green once progress starts.
    let caption_style = if ui.matched > 0 {
        Style::default().fg(theme_color(&theme.success, Color::Green)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme_color(&theme.secondary, Color::Cyan)).add_modifier(Modifier::BOLD)
    };

    // Reserve the fixed name+gap+caption budget, centered within whatever
    // height this box actually has — equal to the Notes/Detected boxes on
    // a normal terminal, shrinking gracefully on a very short one.
    let content = center_v(inner, HERO_HEADING_CONTENT_H.min(inner.height));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(HERO_NAME_H), Constraint::Length(HERO_GAP1), Constraint::Length(HERO_CAPTION_H)])
        .split(content);

    // Glyph labels skip manual letter-spacing (the 8x8 font already has
    // its own per-character padding), same reasoning as the chip labels —
    // keeps more names qualifying for the bigger tier instead of falling
    // back.
    let ascii_name = !name.is_empty() && name.chars().all(|c| c.is_ascii());
    let name_chars = name.chars().count() as u16;
    let full_w = name_chars * HERO_GLYPH_COLS_PER_CHAR;
    let narrow_w = name_chars * HERO_GLYPH_COLS_PER_CHAR_NARROW;
    let name_glyph_tier = if !ascii_name {
        None
    } else if full_w <= content.width && content.height >= HERO_NAME_GLYPH_ROWS {
        Some((PixelSize::Full, HERO_NAME_GLYPH_ROWS))
    } else if narrow_w <= content.width && content.height >= HERO_GLYPH_ROWS {
        Some((PixelSize::Quadrant, HERO_GLYPH_ROWS))
    } else {
        None
    };

    match name_glyph_tier {
        Some((pixel_size, glyph_rows)) => {
            let glyph = BigText::builder()
                .pixel_size(pixel_size)
                .style(name_style)
                .alignment(Alignment::Center)
                .lines(vec![Line::from(name.to_string())])
                .build();
            f.render_widget(glyph, center_v(rows[0], glyph_rows.min(rows[0].height)));
        }
        None => {
            let spaced: String = name.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
            let name_text = if spaced.chars().count() as u16 <= content.width { spaced } else { name.to_string() };
            f.render_widget(
                Paragraph::new(name_text).alignment(Alignment::Center).style(name_style),
                center_v(rows[0], 1),
            );
        }
    }

    f.render_widget(
        Paragraph::new(caption).alignment(Alignment::Center).style(caption_style),
        center_v(rows[2], 1),
    );
}

/// Renders the current-prompt hero panel as three bordered subsections —
/// Heading/Subheading, Notes, Detected — stacked in one column and spread
/// evenly across the panel's full height via three equal `Fill(1)` rows
/// with identical gaps between them (`HERO_SECTION_GAP`). Previously these
/// three lived at a fixed content height stacked at the panel's top, which
/// either centered as one block (leaving a large dead gap above the
/// heading) or top-anchored (stranding a large dead gap below the
/// Detected box, outside any visible boundary) — both were the
/// most-reported issue. Now `draw_practice` sizes the panel itself to
/// fill the available column, so the three subsections — each its own
/// clearly bounded "card" — always occupy the panel's real height with no
/// unbounded space left over.
fn render_hero_prompt(f: &mut ratatui::Frame<'_>, area: Rect, ui: &UiState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
        .title(Span::styled(
            format!(" {} ", ui.prompt_kind),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HERO_TOP_MARGIN),
            Constraint::Fill(1),
            Constraint::Length(HERO_SECTION_GAP),
            Constraint::Fill(1),
            Constraint::Length(HERO_SECTION_GAP),
            Constraint::Fill(1),
            Constraint::Length(HERO_BOTTOM_MARGIN),
        ])
        .split(inner);
    let heading_section = sections[1];
    let notes_section = sections[3];
    let detected_section = sections[5];

    render_heading_box(f, heading_section, ui, theme);

    // Notes and Detected each get their own bordered box spanning the full
    // section — the same treatment as the Heading box — so all three read
    // as visually consistent, equally-sized cards. Their actual content
    // (the target chips, the detected-note text) stays the same fixed,
    // capped size as before and is centered inside that box; only the
    // surrounding frame now grows with the section instead of hugging the
    // content tightly and leaving unbordered dead space around it.
    let notes_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
        .title(" Notes ");
    let notes_inner = notes_block.inner(notes_section);
    f.render_widget(notes_block, notes_section);
    let notes_h = HERO_CHIP_H.min(notes_inner.height);
    render_targets_row(f, center_v(notes_inner, notes_h), ui, theme);

    render_detected_indicator(f, detected_section, ui, theme);
}

#[cfg(test)]
mod detected_correctness_tests {
    use super::*;

    fn ui_with(targets: &[&str], matched: &[bool], ordered: bool, detected: Option<&str>) -> UiState {
        UiState {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            matched_indices: matched.to_vec(),
            ordered,
            detected_note: detected.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn unordered_correct_when_any_unmatched_target_sounds() {
        let ui = ui_with(&["A2", "B2", "C3"], &[false, true, false], false, Some("C3"));
        assert_eq!(detected_correctness(&ui), Some(true));
    }

    #[test]
    fn unordered_wrong_when_note_is_not_a_target() {
        let ui = ui_with(&["A2", "B2"], &[false, false], false, Some("D3"));
        assert_eq!(detected_correctness(&ui), Some(false));
    }

    #[test]
    fn unordered_wrong_when_note_is_an_already_matched_target() {
        // A2 was already accepted; hearing it again isn't "still needed".
        let ui = ui_with(&["A2", "B2"], &[true, false], false, Some("A2"));
        assert_eq!(detected_correctness(&ui), Some(false));
    }

    #[test]
    fn ordered_only_the_next_target_counts() {
        let ui = ui_with(&["A2", "B2", "C3"], &[true, false, false], true, Some("C3"));
        // next required is B2 (index 1); C3 is a target but out of order.
        assert_eq!(detected_correctness(&ui), Some(false));
        let ui2 = ui_with(&["A2", "B2", "C3"], &[true, false, false], true, Some("B2"));
        assert_eq!(detected_correctness(&ui2), Some(true));
    }

    #[test]
    fn none_when_nothing_detected() {
        let ui = ui_with(&["A2"], &[false], false, None);
        assert_eq!(detected_correctness(&ui), None);
    }
}

/// Where the Session panel (or its collapsed fallback) lands on the
/// Practice screen — depends on the wide/narrow/short thresholds below.
enum SessionSlot {
    Panel(Rect),
    Line(Rect),
}

fn draw_practice(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    ui: &UiState,
    settings: &SettingsState,
    theme: &Theme,
) {
    let wide = is_wide(area);

    let (hero_area, timer_area, action_area, slot) = if wide {
        // Wide: two columns — prompt/timer/actions on the left, a
        // full-height Session panel (score, device, tuning, categories,
        // recent attempts) on the right. Hero now fills the available
        // column height (rather than a fixed content budget with a
        // separate spacer below it) so its three subsections — Heading,
        // Notes, Detected — can spread evenly across the panel's real
        // height; see `render_hero_prompt`.
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
            .split(area);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(1), Constraint::Length(6), Constraint::Length(3)])
            .split(cols[0]);
        (left[0], left[1], left[2], SessionSlot::Panel(cols[1]))
    } else if is_short(area) {
        // Narrow AND short: no room for a panel — collapse to the single
        // score/device line the screen has always shown here. Terminal is
        // already tight, so hero keeps claiming whatever's left rather
        // than a fixed budget that might not fit.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(6),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);
        (rows[0], rows[1], rows[3], SessionSlot::Line(rows[2]))
    } else {
        // Narrow but tall enough: single column, with the Session panel
        // (compact — no categories line) dropped beneath the action bar.
        // Hero and the Session panel now split remaining height evenly
        // (both `Fill(1)`) instead of hero taking a fixed budget and the
        // panel absorbing 100% of what's left — same reasoning as the
        // wide branch: hero's three subsections spread across its real
        // height rather than a fixed content box.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(6),
                Constraint::Length(3),
                Constraint::Fill(1),
            ])
            .split(area);
        (rows[0], rows[2], rows[3], SessionSlot::Panel(rows[4]))
    };

    render_hero_prompt(f, hero_area, ui, theme);

    // Timer — full width. The Detected indicator now lives directly under
    // the target-note chips inside the hero panel (see
    // `render_detected_indicator`), not squeezed into half of this row.
    let gauge_color = if ui.time_left_secs <= 5 {
        theme_color(&theme.danger, Color::Red)
    } else {
        theme_color(&theme.success, Color::Green)
    };
    let timer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
        .title(" Timer ");
    let timer_inner = timer_block.inner(timer_area);
    f.render_widget(timer_block, timer_area);
    let timer_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)])
        .split(timer_inner);
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color))
        .ratio(ui.time_left_frac)
        .label(Span::styled(
            format!("{}s / {}s", ui.time_left_secs, ui.prompt_secs),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    f.render_widget(gauge, timer_rows[1]);

    // Footer action bar: Stop / Skip / Settings. The highlighted action
    // gets a filled background pill plus a "‣ " marker and underline (the
    // same marker the vertical menus use) so the current selection is
    // unmistakable at a glance, not just a subtle color shift.
    let mut spans = Vec::new();
    for (i, label) in PRACTICE_ACTIONS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("     "));
        }
        if i == app.practice_idx {
            spans.push(Span::styled(
                format!(" ‣ {label} "),
                selection_style(theme).add_modifier(Modifier::UNDERLINED),
            ));
        } else {
            spans.push(Span::raw(format!("   {label} ")));
        }
    }
    let footer = Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
                .title(" ←→ select, Enter to activate "),
        );
    f.render_widget(footer, action_area);

    match slot {
        SessionSlot::Panel(rect) => render_session_panel(f, rect, ui, settings, theme, wide),
        SessionSlot::Line(rect) => {
            let device_name = settings
                .audio_device
                .clone()
                .unwrap_or_else(|| "(default mic)".to_string());
            let line = Paragraph::new(format!(
                "✓ {}/{}   device: {}",
                ui.score_passed, ui.score_total, device_name
            ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)));
            f.render_widget(line, rect);
        }
    }
}

fn draw_settings(f: &mut ratatui::Frame<'_>, area: Rect, app: &App, settings: &SettingsState, theme: &Theme) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Settings — ↑↓ select, Enter to toggle/edit, Esc to save & back ");
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let list_h = (SETTINGS_ROW_COUNT as u16).min(inner.height);
    let wide = is_wide(area);

    // List stays top-aligned at its natural content height instead of
    // stretching into the full remaining area — stretching a `List` doesn't
    // fill it with anything, it just leaves blank rows below the last item.
    let rows = if wide {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_h), Constraint::Length(1), Constraint::Fill(1)])
            .split(inner)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_h), Constraint::Fill(1), Constraint::Length(1)])
            .split(inner)
    };

    let items: Vec<ListItem> = (0..SETTINGS_ROW_COUNT)
        .map(|i| ListItem::new(settings_row_label(i, app, settings)))
        .collect();
    let list = List::new(items)
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.settings_idx));
    f.render_stateful_widget(list, rows[0], &mut state);

    if wide {
        let divider = "─".repeat(rows[1].width as usize);
        f.render_widget(
            Paragraph::new(Span::styled(divider, Style::default().fg(Color::DarkGray))),
            rows[1],
        );
        render_settings_help(f, rows[2], app, settings, theme);
    } else {
        f.render_widget(
            Paragraph::new("↑↓ select · Enter toggle/edit · Esc save & back")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            rows[2],
        );
    }
}

/// One-line-to-paragraph contextual help for whichever Settings row is
/// currently highlighted — fills the space the old fixed-height list left
/// blank below its last item with something the highlighted row can
/// actually use.
fn render_settings_help(f: &mut ratatui::Frame<'_>, area: Rect, app: &App, settings: &SettingsState, theme: &Theme) {
    let body = match app.settings_idx {
        0 => "How long a prompt stays on screen before it times out. Longer gives more time to \
              find every target note."
            .to_string(),
        1 => format!(
            "Open strings, low → high: {}",
            tuning_strings_label(settings.tuning)
        ),
        2 => "When ON, each new prompt draws uniformly at random from the enabled categories \
              below, instead of cycling through them in order."
            .to_string(),
        3..=9 => {
            let (c, _) = &settings.enabled[app.settings_idx - 3];
            category_help(*c).to_string()
        }
        10 => format!(
            "{} input device(s) found. Press Enter to rescan and choose one.",
            app.devices.len()
        ),
        11 => "Optional folder of your own licks/pieces content, loaded alongside the built-in \
              library. Leave empty to use only the built-in content."
            .to_string(),
        12 => "Save every change above and return to where you started.".to_string(),
        _ => String::new(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" About ");
    let p = Paragraph::new(body)
        .block(block)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)));
    f.render_widget(p, area);
}

fn tuning_strings_label(tuning: TuningId) -> String {
    tuning
        .open_strings()
        .iter()
        .map(|&m| Note::from_midi_clamped(m).name())
        .collect::<Vec<_>>()
        .join(" ")
}

fn category_help(c: ChallengeType) -> &'static str {
    match c {
        ChallengeType::Note => "Single open or fretted notes — the fastest way to drill raw fretboard recall.",
        ChallengeType::Chord => "A full chord voicing from a random root and quality; every note in the shape must sound.",
        ChallengeType::Scale => "A scale run from a random root, matched in ascending order.",
        ChallengeType::Mode => "A modal scale run from a random root, matched in ascending order.",
        ChallengeType::Progression => "A chord-degree progression (e.g. I–IV–V) in a random key, matched in order.",
        ChallengeType::Lick => "A short pre-written phrase from the content library, matched in order.",
        ChallengeType::Piece => "An excerpt from a longer piece in the content library, matched in order.",
    }
}

fn settings_row_label(i: usize, app: &App, settings: &SettingsState) -> String {
    match i {
        0 => {
            if app.edit == Edit::Timer {
                format!("Timer (seconds): {}█", app.edit_buf)
            } else {
                format!("Timer (seconds): {}", settings.duration_sec)
            }
        }
        1 => format!("Tuning: {}", settings.tuning.label()),
        2 => format!("Random mode: {}", if settings.random_mode { "ON" } else { "off" }),
        3..=9 => {
            let (c, on) = &settings.enabled[i - 3];
            format!("[{}] {}", if *on { "✓" } else { " " }, c.label())
        }
        10 => format!(
            "Audio device: {}",
            settings.audio_device.clone().unwrap_or_else(|| "(default mic)".to_string())
        ),
        11 => {
            if app.edit == Edit::Path {
                format!("Custom content path: {}█", app.edit_buf)
            } else {
                let p = if settings.custom_path.is_empty() {
                    "(none)".to_string()
                } else {
                    settings.custom_path.clone()
                };
                format!("Custom content path: {p}")
            }
        }
        12 => "← Back (save & return)".to_string(),
        _ => String::new(),
    }
}

fn draw_device_pick(f: &mut ratatui::Frame<'_>, area: Rect, app: &App, settings: &SettingsState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Audio Device — ↑↓ select, Enter to choose, Esc to cancel ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let current = settings.audio_device.as_deref();
    let mut items: Vec<ListItem> = Vec::new();
    items.push(ListItem::new(if current.is_none() {
        "(Default mic)  ✓ current".to_string()
    } else {
        "(Default mic)".to_string()
    }));
    for d in &app.devices {
        let label = if current == Some(d.as_str()) {
            format!("{d}  ✓ current")
        } else {
            d.clone()
        };
        items.push(ListItem::new(label));
    }

    // Top-align at natural content height + a `Fill` spacer below — same
    // dead-space fix as Settings, no contextual help panel (nothing
    // meaningfully contextual to show per-device beyond the name already
    // visible).
    let list_h = (items.len() as u16).min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(list_h), Constraint::Fill(1)])
        .split(inner);

    let list = List::new(items)
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.device_idx));
    f.render_stateful_widget(list, rows[0], &mut state);
}
