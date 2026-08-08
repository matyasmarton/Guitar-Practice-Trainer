//! Microphone capture (cpal) + pitch-detection worker.
//!
//! The only Mac/Android-shared mic path. `AudioInput::start` requests **mono f32
//! at 44100 Hz**; if the device offers another rate, it accepts that and reports
//! the actual `sample_rate` so [`crate::pitch`] adapts (frame math is
//! rate-independent). Stereo feeds are down-mixed to mono by averaging.
//!
//! A dedicated worker thread consumes samples from a lock-free
//! `crossbeam-queue::ArrayQueue`, runs YIN every `HOP_SAMPLES` new samples over a
//! `FRAME_SAMPLES` window, and emits [`PitchEvent`]s on a channel. cpal's Android
//! backend pulls in the NDK `audio` feature automatically on that target (no
//! feature flag needed for cpal 0.17).

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use tracing::{debug, warn};

use crate::pitch::{detect, FRAME_SAMPLES, HOP_SAMPLES};

/// A pitch event from the detection worker.
#[derive(Clone, Debug)]
pub struct PitchEvent {
    /// When the detection was made.
    pub ts: Instant,
    /// Detected f0 in Hz, or `None` for an unvoiced/silent frame.
    pub hz: Option<f64>,
}

/// Owns a running cpal input stream + its detection worker thread.
pub struct AudioInput {
    /// The cpal stream; dropped on `stop()` to release the mic.
    _stream: Stream,
    /// Worker thread handle.
    _worker: std::thread::JoinHandle<()>,
    /// Actual sample rate reported to the engine.
    pub sample_rate: u32,
    /// Channels of the underlying feed (1 or 2).
    pub channels: u16,
}

impl AudioInput {
    /// Start capture from `device` (or the default input device if `None`).
    ///
    /// Emits [`PitchEvent`]s on `sender`. Prefers a 1-channel 44100 Hz F32 feed
    /// if the device supports it; otherwise accepts the device's default config
    /// and down-mixes to mono.
    pub fn start(
        device: Option<cpal::Device>,
        preferred_sample_rate: u32,
        sender: Sender<PitchEvent>,
    ) -> Result<Self> {
        let host = cpal::default_host();
        let device = match device {
            Some(d) => d,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no input device available"))?,
        };
        debug!("audio input device: {}", device.name().unwrap_or_default());

        // Try to find a mono F32 config at the preferred rate.
        let (config, sample_rate, channels) = pick_config(&device, preferred_sample_rate)?;
        let fmt = cpal::SampleFormat::F32;
        if fmt != cpal::SampleFormat::F32 {
            return Err(anyhow!("only F32 sample inputs are supported, got {:?}", fmt));
        }

        // Lock-free sample ring shared between the real-time callback and worker.
        let ring: Arc<ArrayQueue<f32>> = Arc::new(ArrayQueue::new(1 << 16));

        let stream = build_stream(&device, &config, channels, Arc::clone(&ring))?;
        stream
            .play()
            .map_err(|e| anyhow!("cpal play failed: {e}"))?;

        // Detection worker.
        let sr = sample_rate;
        let ring_w = Arc::clone(&ring);
        let sender_w = sender;
        let worker = std::thread::Builder::new()
            .name("gtt-pitch".into())
            .spawn(move || run_worker(ring_w, sr, sender_w))
            .map_err(|e| anyhow!("spawning pitch worker: {e}"))?;

        Ok(AudioInput {
            _stream: stream,
            _worker: worker,
            sample_rate,
            channels,
        })
    }
}

/// Collect input device names for the Mac picker.
pub fn enumerate_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| {
            it.filter_map(|d| d.name().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Look up an input device by friendly name (for the Mac picker).
pub fn find_device_by_name(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.input_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn pick_config(
    device: &cpal::Device,
    preferred: u32,
) -> Result<(StreamConfig, u32, u16)> {
    // First choice: a supported F32 config with 1 channel and the preferred rate.
    if let Ok(ranges) = device.supported_input_configs() {
        for r in ranges {
            if r.sample_format() == SampleFormat::F32
                && r.channels() == 1
                && (r.min_sample_rate()..=r.max_sample_rate()).contains(&preferred)
            {
                let mut cfg: StreamConfig = r.with_sample_rate(preferred).into();
                cfg.channels = 1;
                return Ok((cfg, preferred, 1));
            }
        }
    }

    // Fallback: default config; report its rate & channels (down-mix later).
    let def = device
        .default_input_config()
        .map_err(|e| anyhow!("no default input config: {e}"))?;
    if def.sample_format() != SampleFormat::F32 {
        return Err(anyhow!(
            "device offers {:?}, only F32 is supported",
            def.sample_format()
        ));
    }
    let channels = def.channels();
    let rate = def.sample_rate();
    let cfg: StreamConfig = def.into();
    Ok((cfg, rate, channels))
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: u16,
    ring: Arc<ArrayQueue<f32>>,
) -> Result<Stream> {
    let err_fn = |err: cpal::StreamError| {
        warn!("cpal stream error: {err}");
    };

    let ch = channels as usize;
    if ch == 1 {
        device.build_input_stream(
            config,
            move |data: &[f32], _: &_| {
                for &s in data {
                    let _ = ring.push(s);
                }
            },
            err_fn,
            None,
        )
    } else {
        // Multi-channel: down-mix to mono by averaging all channels per frame.
        device.build_input_stream(
            config,
            move |data: &[f32], _: &_| {
                for frame in data.chunks_exact(ch) {
                    let mut acc = 0.0f32;
                    for &s in frame {
                        acc += s;
                    }
                    let _ = ring.push(acc / ch as f32);
                }
            },
            err_fn,
            None,
        )
    }
    .context("building cpal input stream")
}

fn run_worker(ring: Arc<ArrayQueue<f32>>, sample_rate: u32, sender: Sender<PitchEvent>) {
    let mut buffer: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES);
    // samples seen since the last hop boundary; we analyze when we have at least
    // FRAME_SAMPLES and ≥ HOP_SAMPLES_digest since the last window.
    let mut samples_since_window: usize = 0;

    loop {
        // Drain currently-available samples.
        let mut got = 0usize;
        while let Some(s) = ring.pop() {
            buffer.push(s);
            got += 1;
            samples_since_window += 1;
        }

        if got == 0 {
            // Nothing new; cheap sleep avoids busy-spinning.
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }

        // If the buffer is long enough and we have advanced a hop, analyze.
        if buffer.len() >= FRAME_SAMPLES && samples_since_window >= HOP_SAMPLES {
            let start = buffer.len() - FRAME_SAMPLES;
            let window = &buffer[start..start + FRAME_SAMPLES];
            let hz = detect(window, sample_rate);
            let _ = sender.send(PitchEvent {
                ts: Instant::now(),
                hz,
            });
            // Keep the trailing (FRAME_SAMPLES - HOP_SAMPLES) samples for overlap.
            let keep = FRAME_SAMPLES.saturating_sub(HOP_SAMPLES);
            if buffer.len() > keep {
                buffer.drain(0..(buffer.len() - keep));
            }
            samples_since_window = 0;
        }
    }
}