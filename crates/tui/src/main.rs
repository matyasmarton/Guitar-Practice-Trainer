//! Guitar Practice Trainer — Mac TUI (`guitar-trainer` binary).
//!
//! Fully arrow-key driven. No memorized hotkeys:
//!   - Up/Down    navigate vertical lists (Menu, Settings, Device picker)
//!   - Left/Right navigate the horizontal action bar during Practice
//!   - Enter/Space activate the highlighted item
//!   - Esc        go back / cancel
//! The only exception is typing the timer seconds or a custom content path,
//! which unavoidably need the keyboard — everything else is pure navigation.

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
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use scopeguard::defer;

use guitar_trainer_core::audio;
use guitar_trainer_core::challenges::ChallengeType;
use guitar_trainer_core::config::{Config, EnabledCategory};
use guitar_trainer_core::engine::{Engine, EngineEvent, EngineListener};
use guitar_trainer_core::theme::Theme;
use guitar_trainer_core::tuning::TuningId;

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
        EngineEvent::Passed => {}
        EngineEvent::Cooldown { duration_ms } => {
            ui.cooldown_started = Some(std::time::Instant::now());
            ui.cooldown_ms = *duration_ms;
        }
        EngineEvent::Timeout => {
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

fn selection_style(theme: &Theme) -> Style {
    Style::default()
        .fg(parse_color(&theme.selection_fg, Color::Black))
        .bg(parse_color(&theme.selection_bg, Color::Yellow))
        .add_modifier(Modifier::BOLD)
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
        .constraints([Constraint::Length(3), Constraint::Min(6), Constraint::Length(3)])
        .split(area);

    let title = Paragraph::new("Guitar Practice Trainer")
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::BOLD));
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = MENU_ITEMS.iter().map(|s| ListItem::new(*s)).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Menu — ↑↓ select, Enter to activate "),
        )
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.menu_idx));
    f.render_stateful_widget(list, chunks[1], &mut state);

    let device_name = settings
        .audio_device
        .clone()
        .unwrap_or_else(|| "(default mic)".to_string());
    let (footer_text, footer_style) = match &ui.status {
        Some(msg) => (msg.clone(), Style::default().fg(parse_color(&theme.danger, Color::Red))),
        None => (
            format!("mic: {device_name}   ✓{}/{}", ui.score_passed, ui.score_total),
            Style::default().fg(parse_color(&theme.secondary, Color::Cyan)),
        ),
    };
    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .style(footer_style);
    f.render_widget(footer, chunks[2]);
}

fn draw_practice(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    ui: &UiState,
    settings: &SettingsState,
    theme: &Theme,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area);

    // Prompt block.
    let prompt_block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", ui.prompt_kind),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let success_style = Style::default().fg(parse_color(&theme.success, Color::Green));
    let targets_line: Line = if ui.ordered {
        let mut spans = vec![Span::raw(format!("[{}/{}]  ", ui.matched, ui.targets.len()))];
        for (i, t) in ui.targets.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" → "));
            }
            if ui.matched_indices.get(i).copied().unwrap_or(false) {
                spans.push(Span::styled(t.clone(), success_style));
            } else {
                spans.push(Span::raw(t.clone()));
            }
        }
        Line::from(spans)
    } else {
        let mut spans = vec![Span::raw(format!("{}  ", ui.matched))];
        for (i, t) in ui.targets.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            if ui.matched_indices.get(i).copied().unwrap_or(false) {
                spans.push(Span::styled(format!("[✓{}]", t), success_style));
            } else {
                spans.push(Span::raw(format!("[  {}]", t)));
            }
        }
        Line::from(spans)
    };
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(ui.prompt_display.clone()),
        Line::from(""),
        targets_line,
    ])
    .block(prompt_block)
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true });
    f.render_widget(p, chunks[0]);

    // Timer (L) + detected note (R).
    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);
    let gauge_color = if ui.time_left_secs <= 5 {
        parse_color(&theme.danger, Color::Red)
    } else {
        parse_color(&theme.success, Color::Green)
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" Timer "))
        .gauge_style(Style::default().fg(gauge_color))
        .ratio(ui.time_left_frac)
        .label(format!("{}s / {}s", ui.time_left_secs, ui.prompt_secs));
    f.render_widget(gauge, mid[0]);
    let detected_text = ui.detected_note.clone().unwrap_or_else(|| "—".to_string());
    // During the post-match cooldown, blink the Detected panel (alternating
    // reversed/success and plain accent styles every 200ms) as an obvious
    // "matched, hold on" cue; otherwise render with the plain accent style.
    let accent_style = Style::default().fg(parse_color(&theme.accent, Color::Yellow)).add_modifier(Modifier::BOLD);
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
    let det = Paragraph::new(format!(" {}", detected_text))
        .block(Block::default().borders(Borders::ALL).title(" Detected "))
        .style(detected_style);
    f.render_widget(det, mid[1]);

    // Score + device line.
    let device_name = settings
        .audio_device
        .clone()
        .unwrap_or_else(|| "(default mic)".to_string());
    let score_line = Paragraph::new(format!(
        "✓ {}/{}   device: {}",
        ui.score_passed, ui.score_total, device_name
    ))
    .alignment(Alignment::Center)
    .style(Style::default().fg(parse_color(&theme.secondary, Color::Cyan)));
    f.render_widget(score_line, chunks[2]);

    // Footer action bar: Stop / Skip / Settings, current one highlighted.
    let mut spans = Vec::new();
    for (i, label) in PRACTICE_ACTIONS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("    "));
        }
        if i == app.practice_idx {
            spans.push(Span::styled(format!(" {label} "), selection_style(theme)));
        } else {
            spans.push(Span::raw(format!(" {label} ")));
        }
    }
    let footer = Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ←→ select, Enter to activate "),
        );
    f.render_widget(footer, chunks[3]);
}

fn draw_settings(f: &mut ratatui::Frame<'_>, area: Rect, app: &App, settings: &SettingsState, theme: &Theme) {
    let block = Block::default().borders(Borders::ALL).title(
        " Settings — ↑↓ select, Enter to toggle/edit, Esc to save & back ",
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = (0..SETTINGS_ROW_COUNT)
        .map(|i| ListItem::new(settings_row_label(i, app, settings)))
        .collect();
    let list = List::new(items)
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.settings_idx));
    f.render_stateful_widget(list, inner, &mut state);
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
    let block = Block::default().borders(Borders::ALL).title(
        " Audio Device — ↑↓ select, Enter to choose, Esc to cancel ",
    );
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

    let list = List::new(items)
        .highlight_style(selection_style(theme))
        .highlight_symbol("‣ ");
    let mut state = ListState::default();
    state.select(Some(app.device_idx));
    f.render_stateful_widget(list, inner, &mut state);
}
