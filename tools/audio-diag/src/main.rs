// Live diagnostic for the guitar-trainer audio pipeline.
//
// Phase 1 (8s): raw cpal stream on the target device, printing peak/RMS
// amplitude per ~150ms chunk. Confirms samples are actually arriving.
//
// Phase 2 (25s): the REAL production path — guitar_trainer_core::audio::AudioInput
// (the exact code the TUI uses) — printing every PitchEvent as it fires,
// including what note (if any) it resolves to.
//
// Usage: audio-diag [device name substring]  (defaults to "SKY")

use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use guitar_trainer_core::audio::{self, AudioInput};
use guitar_trainer_core::note::Note;

fn main() -> anyhow::Result<()> {
    let name_filter = std::env::args().nth(1).unwrap_or_else(|| "SKY".to_string());

    println!("=== Available input devices ===");
    for d in audio::enumerate_input_devices() {
        println!("  {d}");
    }

    let host = cpal::default_host();
    let device = host
        .input_devices()?
        .find(|d| d.name().map(|n| n.contains(&name_filter)).unwrap_or(false))
        .ok_or_else(|| anyhow::anyhow!("no device matching '{name_filter}' found"))?;
    let device_name = device.name().unwrap_or_default();
    println!("\n=== Using device: {device_name} ===\n");

    // ---- Phase 1: raw amplitude check -------------------------------------
    println!("--- Phase 1: raw signal level (8s) — pluck/strum now ---");
    let config = device.default_input_config()?;
    println!(
        "  config: {} ch, {} Hz, {:?}",
        config.channels(),
        config.sample_rate(),
        config.sample_format()
    );
    let (tx, rx) = std::sync::mpsc::channel::<f32>();
    let stream_config: cpal::StreamConfig = config.clone().into();
    let ch = config.channels() as usize;
    let stream = device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &_| {
            let mut peak = 0.0f32;
            let mut sum_sq = 0.0f64;
            let mut n = 0usize;
            for frame in data.chunks(ch.max(1)) {
                let s = frame[0];
                peak = peak.max(s.abs());
                sum_sq += (s as f64) * (s as f64);
                n += 1;
            }
            let rms = if n > 0 {
                (sum_sq / n as f64).sqrt()
            } else {
                0.0
            };
            let _ = tx.send(peak.max(rms as f32));
        },
        |e| eprintln!("stream error: {e}"),
        None,
    )?;
    stream.play()?;

    let start = Instant::now();
    let mut window_peak = 0.0f32;
    let mut last_report = Instant::now();
    let mut any_signal = false;
    while start.elapsed() < Duration::from_secs(8) {
        if let Ok(v) = rx.try_recv() {
            window_peak = window_peak.max(v);
        }
        if last_report.elapsed() >= Duration::from_millis(150) {
            let db = if window_peak > 0.0 {
                20.0 * window_peak.log10()
            } else {
                -100.0
            };
            let bar_len = ((db + 60.0).clamp(0.0, 60.0)) as usize;
            let bar: String = "#".repeat(bar_len);
            println!("  peak={window_peak:.5}  ~{db:6.1} dBFS  {bar}");
            if window_peak > 0.01 {
                any_signal = true;
            }
            window_peak = 0.0;
            last_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(stream);
    println!(
        "\nPhase 1 result: {}\n",
        if any_signal {
            "signal above -40dBFS seen at least once (loud enough for reliable pitch detection)"
        } else {
            "signal stayed very quiet (< -40dBFS peak) the whole time — likely too quiet for YIN to lock on reliably"
        }
    );

    // ---- Phase 2: real production pitch-detection path ---------------------
    println!("--- Phase 2: real AudioInput + YIN pipeline (25s) — keep plucking ---");
    let device2 = audio::find_device_by_name(&device_name)
        .ok_or_else(|| anyhow::anyhow!("could not re-open device by name"))?;
    let (ptx, prx) = crossbeam_channel::bounded(512);
    let input = AudioInput::start(Some(device2), 44100, ptx)?;
    println!(
        "  resolved: {} ch, {} Hz\n",
        input.channels, input.sample_rate
    );

    let start2 = Instant::now();
    let mut total = 0u32;
    let mut voiced = 0u32;
    while start2.elapsed() < Duration::from_secs(25) {
        if let Ok(ev) = prx.recv_timeout(Duration::from_millis(50)) {
            total += 1;
            match ev.hz {
                Some(hz) => {
                    voiced += 1;
                    let note = Note::from_hz(hz);
                    let note_str = note
                        .map(|n| format!("{} (midi {})", n.name(), n.midi()))
                        .unwrap_or_else(|| "out of playable range (dropped)".to_string());
                    println!("  hz={hz:8.2}  -> {note_str}");
                }
                None => println!("  (silence / unvoiced frame)"),
            }
        }
    }
    drop(input);

    println!("\n=== Summary ===");
    println!("Phase 2 frames processed: {total}");
    println!("Frames with a detected pitch: {voiced}");
    if total == 0 {
        println!("!! No PitchEvents arrived at all — the worker thread never accumulated");
        println!(
            "   a full frame. Check the stream actually started / device is producing samples."
        );
    } else if voiced == 0 {
        println!("!! Frames arrived but YIN never returned a pitch — either signal is too quiet");
        println!("   /noisy for the silence or threshold gates, or something about the real");
        println!("   waveform trips up the algorithm.");
    } else {
        println!("Pitch detection IS working on this device.");
    }

    Ok(())
}
