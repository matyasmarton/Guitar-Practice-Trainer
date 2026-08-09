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
use tui_big_text::{BigText, PixelSize};
use ratatui::Terminal;
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
/// leftover space split evenly above and below. Used to keep the target
/// chips comfortably framed instead of stretching to fill a tall panel.
fn center_v(rect: Rect, content_h: u16) -> Rect {
    if rect.height <= content_h {
        return rect;
    }
    let pad = (rect.height - content_h) / 2;
    Rect { x: rect.x, y: rect.y + pad, width: rect.width, height: content_h }
}

/// The notes to actually play — the single most important piece of
/// information during practice, and the hero panel's largest, boldest
/// element. Each target is its own rounded bordered chip; the label renders
/// as block-glyph big text (4 rows tall per note, ~4× the old single line)
/// so it reads from playing distance — cyan border + cyan glyphs, turning
/// green once matched. Ordered prompts chain the chips with an arrow so the
/// required sequence reads left to right; unordered prompts space them
/// evenly. If the big-text row would not fit the panel width (narrow
/// terminal), it falls back to plain bold labels rather than clip.
fn render_targets_row(f: &mut ratatui::Frame<'_>, area: Rect, ui: &UiState, theme: &Theme) {
    if ui.targets.is_empty() || area.height == 0 || area.width == 0 {
        return;
    }
    let success = Style::default().fg(theme_color(&theme.success, Color::Green)).add_modifier(Modifier::BOLD);
    let idle_border = Style::default().fg(theme_color(&theme.secondary, Color::Cyan));
    let idle_text = Style::default().fg(theme_color(&theme.secondary, Color::Cyan)).add_modifier(Modifier::BOLD);
    let plain_text = Style::default().add_modifier(Modifier::BOLD);

    let gap_w: u16 = if ui.ordered { 5 } else { 3 };
    let n = ui.targets.len() as u16;
    // Quadrant pixel size renders each source character 4 cells wide and 4
    // rows tall — big enough to fill the 9-row chip, small enough to keep
    // the box at its previous size.
    const BIG_COLS: u16 = 4;
    const BIG_ROWS: u16 = 4;
    let inner_h = area.height.saturating_sub(2);

    // Big-text chips are preferred, but only when every chip fits the row;
    // otherwise fall back to plain bold labels rather than clip or wrap.
    let big_box_w = ui
        .targets
        .iter()
        .map(|t| t.chars().count() as u16 * BIG_COLS + 6) // glyphs + borders + 2-col slack
        .max()
        .unwrap_or(1);
    let big_content_w = big_box_w * n + gap_w * n.saturating_sub(1);
    let use_big = inner_h >= BIG_ROWS && big_content_w <= area.width;

    let box_w = if use_big {
        big_box_w
    } else {
        ui.targets.iter().map(|t| t.chars().count() as u16 + 6).max().unwrap_or(1)
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
    for (i, t) in ui.targets.iter().enumerate() {
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
        let (border_style, text_style) = if matched {
            (success, success)
        } else {
            (idle_border, idle_text)
        };
        let chip = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);
        let chip_inner = chip.inner(rect);
        f.render_widget(chip, rect);
        if use_big {
            let big = BigText::builder()
                .pixel_size(PixelSize::Quadrant)
                .style(text_style)
                .centered()
                .lines(vec![Line::from(t.clone())])
                .build();
            f.render_widget(big, center_v(chip_inner, BIG_ROWS));
        } else {
            // Plain fallback keeps the leading checkmark for matched notes
            // (the ✓ glyph isn't in the 8x8 pixel font big text uses).
            let label = if matched { format!("✓ {t}") } else { t.clone() };
            f.render_widget(
                Paragraph::new(label).alignment(Alignment::Center).style(plain_text),
                center_v(chip_inner, 1),
            );
        }
    }
}

/// Renders the current-prompt hero panel: kind title (small, in the
/// border), then the prompt name, match caption, and target-note chips
/// grouped into one tightly-spaced block and centered together in the
/// panel — keeping the name and the notes to play visually adjacent
/// instead of pinning the name near the top with the chips centered far
/// below it. The prompt name and match caption render as block-glyph big
/// text (the same font the target chips use) whenever the panel has
/// enough width and height to hold them without wrapping or clipping;
/// otherwise each falls back independently to compact single-row text —
/// a long Lick/Piece name (which can run 30+ characters with a note list
/// in parentheses) degrades gracefully instead of overflowing. The
/// target notes stay the panel's most heavily framed element (bordered,
/// checkmarked chip boxes) even when the heading reaches the same glyph
/// height, since those are what the player has to act on.
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

    let name = ui.prompt_display.trim();
    let caption = if ui.ordered {
        format!("{} OF {} MATCHED — IN ORDER", ui.matched, ui.targets.len())
    } else {
        format!("{} OF {} MATCHED", ui.matched, ui.targets.len())
    };
    // Heading: accent yellow — the first colored thing the eye lands on.
    let name_style = Style::default().fg(theme_color(&theme.accent, Color::Yellow)).add_modifier(Modifier::BOLD);
    // Subheading: cyan by default, switching to green once progress starts
    // — never a flat white line.
    let caption_style = if ui.matched > 0 {
        Style::default().fg(theme_color(&theme.success, Color::Green)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme_color(&theme.secondary, Color::Cyan)).add_modifier(Modifier::BOLD)
    };

    // Big-text glyphs are 4 terminal columns wide per source character no
    // matter the row height (Quadrant and Sextant both halve the 8-col
    // font horizontally) — only the row count differs. Each string is
    // measured independently against the panel width, so e.g. a short
    // chord name renders big while a long Lick name with a note list
    // falls back to plain text on its own, never wrapping or clipping.
    // Big-text glyphs render via ratatui's plain Block Elements (U+2580
    // range) at Quadrant/HalfHeight — supported by every monospace
    // terminal font. Sextant/ThirdHeight instead depend on the newer
    // Legacy Computing Symbols block (U+1FB00+), which most terminal
    // fonts (including macOS Terminal.app's defaults) ship no glyphs
    // for — that combination is what rendered the subheading as
    // unreadable tofu/`?`-box placeholders, so neither is used here.
    const BIG_COLS_NAME: u16 = 4; // PixelSize::Quadrant — 2 source px per cell horizontally.
    const NAME_BIG_ROWS: u16 = 4;
    const BIG_COLS_CAPTION: u16 = 8; // PixelSize::HalfHeight — 1 source px per cell horizontally.
    const CAPTION_BIG_ROWS: u16 = 4;
    // Below this inner height there isn't reliably room for a 4-row
    // heading plus a 4-row subheading above the chip row's 5-row floor
    // and its own gaps; fall back to compact text rather than risk the
    // group overflowing the panel on a short terminal.
    const MIN_BIG_TEXT_INNER_H: u16 = 24;
    let fits_big = |s: &str, cols_per_char: u16| -> bool {
        !s.is_empty() && (s.chars().count() as u16) * cols_per_char <= inner.width
    };
    let allow_big = inner.height >= MIN_BIG_TEXT_INNER_H;
    let name_big = allow_big && fits_big(name, BIG_COLS_NAME);
    let caption_big = allow_big && fits_big(&caption, BIG_COLS_CAPTION);

    // Size the chip row last so the name + caption + chips group can be
    // measured as a single block and centered together, rather than
    // pinning the name to the top and centering the chips separately in
    // whatever space is left over. Gap above the chips is wider than the
    // gap between name and caption, so the heading cluster reads clearly
    // above the notes without crowding them.
    let name_h: u16 = if name_big { NAME_BIG_ROWS } else { 1 };
    let gap_above_caption: u16 = 1;
    let caption_h: u16 = if caption_big { CAPTION_BIG_ROWS } else { 1 };
    let gap_above_chips: u16 = 4;
    let fixed_h = name_h + gap_above_caption + caption_h + gap_above_chips;
    let box_h = inner.height.saturating_sub(fixed_h).clamp(5, 9);
    let content_h = fixed_h + box_h;

    let group = center_v(inner, content_h);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(name_h),
            Constraint::Length(gap_above_caption),
            Constraint::Length(caption_h),
            Constraint::Length(gap_above_chips),
            Constraint::Length(box_h),
        ])
        .split(group);

    if name_big {
        let big = BigText::builder()
            .pixel_size(PixelSize::Quadrant)
            .style(name_style)
            .centered()
            .lines(vec![Line::from(name.to_string())])
            .build();
        f.render_widget(big, layout[0]);
    } else {
        // Falls back to unspaced text rather than clip if the letter-spaced
        // version wouldn't fit the panel.
        let spaced: String = name.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
        let name_text = if spaced.chars().count() as u16 <= inner.width { spaced } else { name.to_string() };
        f.render_widget(
            Paragraph::new(name_text).alignment(Alignment::Center).style(name_style),
            center_v(layout[0], 1),
        );
    }

    if caption_big {
        let big = BigText::builder()
            .pixel_size(PixelSize::HalfHeight)
            .style(caption_style)
            .centered()
            .lines(vec![Line::from(caption.clone())])
            .build();
        f.render_widget(big, layout[2]);
    } else {
        f.render_widget(
            Paragraph::new(caption).alignment(Alignment::Center).style(caption_style),
            center_v(layout[2], 1),
        );
    }

    render_targets_row(f, layout[4], ui, theme);
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
        // recent attempts) on the right.
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
        // score/device line the screen has always shown here.
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
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(6),
                Constraint::Length(3),
                Constraint::Fill(1),
            ])
            .split(area);
        (rows[0], rows[1], rows[2], SessionSlot::Panel(rows[3]))
    };

    render_hero_prompt(f, hero_area, ui, theme);

    // Timer (L) + detected note (R) — taller than the original fixed
    // 3-row strip, with the content vertically centered inside, so both
    // read at a glance from playing distance instead of hugging the top.
    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(timer_area);
    let success_style = Style::default().fg(theme_color(&theme.success, Color::Green));

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
    let timer_inner = timer_block.inner(mid[0]);
    f.render_widget(timer_block, mid[0]);
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

    let detected_text = ui.detected_note.clone().unwrap_or_else(|| "—".to_string());
    // During the post-match cooldown, blink the Detected panel (alternating
    // reversed/success and plain accent styles every 200ms) as an obvious
    // "matched, hold on" cue; otherwise render with the plain accent style.
    let accent_style = Style::default().fg(theme_color(&theme.accent, Color::Yellow)).add_modifier(Modifier::BOLD);
    let cooldown_active = ui
        .cooldown_started
        .map(|t| t.elapsed() < Duration::from_millis(ui.cooldown_ms))
        .unwrap_or(false);
    let detected_style = if cooldown_active {
        let blink_on = (ui.cooldown_started.unwrap().elapsed().as_millis() / 200) % 2 == 0;
        if blink_on {
            success_style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            accent_style
        }
    } else {
        accent_style
    };
    let detected_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(&theme.secondary, Color::Cyan)))
        .title(" Detected ");
    let detected_inner = detected_block.inner(mid[1]);
    f.render_widget(detected_block, mid[1]);
    let detected_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)])
        .split(detected_inner);
    f.render_widget(
        Paragraph::new(detected_text).alignment(Alignment::Center).style(detected_style),
        detected_rows[1],
    );

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
