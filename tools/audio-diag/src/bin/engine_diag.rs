// Live diagnostic that drives the REAL production `Engine` (the same class
// the TUI uses), not just raw AudioInput. This exercises Engine::start,
// the driver thread, handle_pitch's stability/dedup logic, and the
// EngineListener callback path end to end — everything audio-diag's direct
// AudioInput test skipped.
//
// Usage: engine_diag  (loads the same persisted config.toml the TUI uses)

use std::sync::Arc;
use std::time::Duration;

use guitar_trainer_core::config;
use guitar_trainer_core::engine::{Engine, EngineEvent, EngineListener};
use parking_lot::Mutex;

struct PrintListener {
    log: Arc<Mutex<Vec<String>>>,
}
impl EngineListener for PrintListener {
    fn on_event(&self, ev: EngineEvent) {
        let line = match &ev {
            EngineEvent::Prompt(v) => format!("PROMPT: {} ({})", v.display, v.kind),
            EngineEvent::DetectedNote(n) => format!("DETECTED: {:?}", n),
            EngineEvent::Matched { index, total } => format!("MATCHED: {}/{}", index, total),
            EngineEvent::Passed => "PASSED".to_string(),
            EngineEvent::Cooldown { duration_ms } => format!("COOLDOWN: {}ms", duration_ms),
            EngineEvent::Timeout => "TIMEOUT".to_string(),
            EngineEvent::Score { passed, total } => format!("SCORE: {}/{}", passed, total),
        };
        println!("  {line}");
        self.log.lock().push(line);
    }
}

fn main() -> anyhow::Result<()> {
    let cfg = config::load();
    println!("=== Loaded config ===");
    println!("  audio_device_name: {:?}", cfg.audio_device_name);
    println!("  default_duration_sec: {}", cfg.default_duration_sec);
    println!("  enabled categories: {:?}", cfg.active_categories());

    let log = Arc::new(Mutex::new(Vec::new()));
    let listener = Box::new(PrintListener { log: log.clone() });
    let engine = Engine::new(cfg, listener)?;

    println!("\n=== Calling engine.start() ===");
    match engine.start() {
        Ok(()) => println!("  engine.start() -> Ok\n"),
        Err(e) => {
            println!("  engine.start() -> Err: {e}");
            println!("  (this IS the bug if you see this — mic/device failed to open)");
            return Ok(());
        }
    }

    println!("--- running for 25s, keep plucking ---\n");
    std::thread::sleep(Duration::from_secs(25));

    engine.stop();

    let log = log.lock();
    let detected_count = log.iter().filter(|l| l.starts_with("DETECTED: Some")).count();
    let prompt_count = log.iter().filter(|l| l.starts_with("PROMPT")).count();
    println!("\n=== Summary ===");
    println!("Total engine events: {}", log.len());
    println!("Prompts shown: {prompt_count}");
    println!("DetectedNote(Some(..)) events: {detected_count}");
    if log.is_empty() {
        println!("!! ZERO events emitted at all — listener wiring or driver thread never ran.");
    } else if detected_count == 0 {
        println!("!! Engine ran (prompts/score fired) but NEVER emitted a detected note.");
        println!("   Bug is specifically in the Engine layer (handle_pitch / driver thread),");
        println!("   since raw AudioInput+YIN was already proven working directly.");
    } else {
        println!("Engine-level detection IS working.");
    }

    Ok(())
}
