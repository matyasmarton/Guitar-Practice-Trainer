//! Tone playback for listen-and-repeat (ear training) mode — cpal output.
//!
//! A single background thread owns the cpal output stream and a shared sample
//! buffer. The engine sends a list of MIDI numbers; the thread synthesizes the
//! full sequence up front at the stream's sample rate, swaps it in, sleeps for
//! `total + TAIL_MS`, then clears the caller-provided gate. The gate is the
//! engine's "audio is sounding" flag: the driver freezes countdown/pitch
//! processing while it is set, so playback never overlaps scoring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use parking_lot::Mutex;
use tracing::warn;

use crate::note::Note;

/// Duration of one sounded note.
pub const NOTE_MS: u64 = 350;
/// Silence between consecutive notes of a sequence.
pub const GAP_MS: u64 = 90;
/// Extra gate-hold after the last sample (speaker→mic bleed buffer).
pub const TAIL_MS: u64 = 250;
/// Fixed peak amplitude (no volume control in v1).
const AMP: f32 = 0.25;
/// Linear attack ramp length, in milliseconds.
const ATTACK_MS: f32 = 10.0;

/// Shared synthesis buffer: (samples, play position). The callback copies from
/// it and emits silence past the end.
type SharedBuffer = Arc<Mutex<(Vec<f32>, usize)>>;

/// Handle to the tone-playback thread + output stream. Dropping it disconnects
/// the channel; the player thread exits and the stream drops with it.
pub struct TonePlayer {
    tx: crossbeam_channel::Sender<Vec<u8>>,
}

impl TonePlayer {
    /// Open the default output device and spawn the `gtt-tones` thread. `gate`
    /// is cleared after every finished sequence; the engine sets it before
    /// each send.
    pub fn start(gate: Arc<AtomicBool>) -> Result<TonePlayer> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

        // Prefer mono F32 (mirroring audio::pick_config's preference order);
        // otherwise take the device's default config and convert samples.
        let (config, sample_format, channels) = pick_output_config(&device)?;
        let buf: SharedBuffer = Arc::new(Mutex::new((Vec::new(), 0)));
        let stream = build_output_stream(&device, &config, sample_format, channels, &buf)?;
        stream
            .play()
            .map_err(|e| anyhow::anyhow!("starting output stream: {e}"))?;

        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let rate = config.sample_rate;
        let thread_buf = Arc::clone(&buf);
        // The stream lives on the player thread; dropping the JoinHandle
        let thread_gate = Arc::clone(&gate);
        let _ = std::thread::Builder::new()
            .name("gtt-tones".into())
            .spawn(move || {
                let _keepalive = stream;
                run_player(rx, thread_buf, rate, thread_gate);
            });
        gate.store(false, Ordering::SeqCst);
        Ok(TonePlayer { tx })
    }

    /// Sound `midis` as a back-to-back sequence. The engine must have set the
    /// gate before calling (the player never sets it itself on entry).
    pub fn play(&self, midis: Vec<u8>) {
        let _ = self.tx.send(midis);
    }
}

fn pick_output_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat, u16)> {
    // First choice: a supported F32 config with 1 channel.
    if let Ok(ranges) = device.supported_output_configs() {
        for r in ranges {
            if r.sample_format() == SampleFormat::F32 && r.channels() == 1 {
                let mut cfg: StreamConfig = r.with_max_sample_rate().into();
                cfg.channels = 1;
                return Ok((cfg, SampleFormat::F32, 1));
            }
        }
    }
    // Fallback: the device's default config, whatever its format/channels.
    let def = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("no default output config: {e}"))?;
    let fmt = def.sample_format();
    let channels = def.channels();
    let cfg: StreamConfig = def.into();
    Ok((cfg, fmt, channels))
}

fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    fmt: SampleFormat,
    channels: u16,
    buf: &SharedBuffer,
) -> Result<Stream> {
    let err_fn = |err: cpal::StreamError| warn!("cpal output stream error: {err}");
    let ch = channels.max(1) as usize;
    let b = Arc::clone(buf);
    let result = match fmt {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &_| fill_f32(&mut b.lock(), data, ch),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &_| {
                let mut g = b.lock();
                for frame in data.chunks_mut(ch) {
                    let s = next_sample(&mut g);
                    for out in frame.iter_mut() {
                        *out = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    }
                }
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &_| {
                let mut g = b.lock();
                for frame in data.chunks_mut(ch) {
                    let s = next_sample(&mut g);
                    let v = ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16;
                    for out in frame.iter_mut() {
                        *out = v;
                    }
                }
            },
            err_fn,
            None,
        ),
        other => return Err(anyhow::anyhow!("unsupported output format: {other:?}")),
    };
    result.map_err(|e| anyhow::anyhow!("building cpal output stream: {e}"))
}

/// Next mono sample: the buffer's sample at `position`, or silence past the
/// end (the player thread resets the position to 0 for each new sequence).
fn next_sample(g: &mut (Vec<f32>, usize)) -> f32 {
    let (buf, pos) = g;
    match buf.get(*pos) {
        Some(&s) => {
            *pos += 1;
            s
        }
        None => 0.0,
    }
}

fn fill_f32(g: &mut (Vec<f32>, usize), data: &mut [f32], ch: usize) {
    for frame in data.chunks_mut(ch.max(1)) {
        let s = next_sample(g);
        for out in frame.iter_mut() {
            *out = s;
        }
    }
}

/// Player-thread loop: synthesize each requested sequence, swap it into the
/// shared buffer, hold the gate for `total + TAIL_MS`, then clear it.
fn run_player(
    rx: crossbeam_channel::Receiver<Vec<u8>>,
    buf: SharedBuffer,
    rate: u32,
    gate: Arc<AtomicBool>,
) {
    let rate = rate.max(1) as f32;
    while let Ok(midis) = rx.recv() {
        if midis.is_empty() {
            continue;
        }
        let note_len = (rate * NOTE_MS as f32 / 1000.0) as usize;
        let gap_len = (rate * GAP_MS as f32 / 1000.0) as usize;
        let mut samples: Vec<f32> = Vec::with_capacity(midis.len() * note_len);
        for (i, &m) in midis.iter().enumerate() {
            if i > 0 {
                samples.resize(samples.len() + gap_len, 0.0);
            }
            let hz = Note::from_midi_clamped(m).hz() as f32;
            for t in 0..note_len {
                let ts = t as f32 / rate;
                let attack = (ts / (ATTACK_MS / 1000.0)).min(1.0);
                let decay = (-3.0 * ts / (NOTE_MS as f32 / 1000.0)).exp();
                samples.push(AMP * attack * decay * (2.0 * std::f32::consts::PI * hz * ts).sin());
            }
        }
        let total = Duration::from_millis(
            midis.len() as u64 * NOTE_MS + midis.len().saturating_sub(1) as u64 * GAP_MS,
        );
        {
            let mut g = buf.lock();
            *g = (samples, 0);
        }
        std::thread::sleep(total + Duration::from_millis(TAIL_MS));
        gate.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_timing_math() {
        // 4 notes: 4*NOTE_MS of sound + 3*GAP_MS of gaps, plus TAIL_MS hold.
        let n = 4;
        let total = n * NOTE_MS + (n - 1) * GAP_MS;
        assert_eq!(total, 1400 + 270);
        assert_eq!(total + TAIL_MS, 1920);
    }
}
