//! UniFFI-exported surface for the Android (and iOS) frontends.
//!
//! The core types `ChallengeView`, `EngineEvent`, and the `EngineListener`
//! callback trait carry `#[cfg_attr(feature = "uniffi", derive(uniffi::*))]`
//! in [`crate::engine`]. `Config` cannot be a UniFFI record directly (it holds
//! an `EnumSet`), so this module exposes an FFI-friendly [`FfiConfig`] record
//! with by-value conversion to/from the internal [`Config`].
//!
//! Binding generation (library mode, no UDL):
//! ```sh
//! cargo build -p guitar_trainer_core --features uniffi --lib
//! uniffi-bindgen generate --language kotlin \
//!   --library target/debug/libguitar_trainer_core.dylib \
//!   --out-dir android/app/src/main/java/dev/guitartrainer/bindings
//! ```

use std::sync::Arc;

use crate::challenges::ChallengeType;
use crate::config::{Config, EnabledCategory};
use crate::engine::{Engine, EngineListener};

/// FFI-friendly config record (the `enumset` set is exposed as a `Vec<String>`
/// of category labels: `"Note", "Chord", "Scale", "Mode", "Progression",
/// "Lick", "Piece"`).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiConfig {
    pub default_duration_sec: u32,
    pub enabled: Vec<String>,
    pub random_mode: bool,
    /// Hard difficulty: ordered prompts reset to the first note on any
    /// newly-struck wrong note. Defaults off.
    pub hard_sequence: bool,
    pub custom_content_path: Option<String>,
    pub custom_tuning_path: Option<String>,
    pub audio_device_name: Option<String>,
    pub tuning: String,
    pub match_pause_ms: u32,
}

/// Timer progress reported across the FFI.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiProgress {
    /// Fraction of time remaining for the current prompt, `[0.0, 1.0]`.
    pub frac: f64,
    /// Seconds remaining (rounded).
    pub secs: u64,
    /// Total seconds the current prompt started with.
    pub prompt_secs: u64,
}

/// Error type crossing the FFI. UniFFI 0.28 cannot throw bare `String`, so we
#[derive(Clone, Debug, uniffi::Error)]
pub enum FfiError {
    /// Human-readable error message.
    Message(String),
}
impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::Message(m) => write!(f, "{m}"),
        }
    }
}

impl From<anyhow::Error> for FfiError {
    fn from(e: anyhow::Error) -> Self {
        FfiError::Message(e.to_string())
    }
}

impl From<&Config> for FfiConfig {
    fn from(c: &Config) -> Self {
        let enabled: Vec<String> = c
            .enabled
            .iter()
            .map(<ChallengeType as From<EnabledCategory>>::from)
            .map(|ct| ct.label().to_string())
            .collect();
        FfiConfig {
            default_duration_sec: c.default_duration_sec,
            enabled,
            random_mode: c.random_mode,
            hard_sequence: c.hard_sequence,
            custom_content_path: c.custom_content_path.as_ref().map(|p| p.display().to_string()),
            custom_tuning_path: c.custom_tuning_path.as_ref().map(|p| p.display().to_string()),
            audio_device_name: c.audio_device_name.clone(),
            tuning: match &c.tuning {
                crate::custom_tuning::ActiveTuning::Builtin(id) => id.label().to_string(),
                crate::custom_tuning::ActiveTuning::Custom { name } => name.clone(),
            },
            match_pause_ms: c.match_pause_ms,
        }
    }
}

impl FfiConfig {
    /// Convert to an internal `Config`. Unknown labels are ignored; an empty
    /// enabled set falls back to all categories (see [`Config::active_categories`]).
    pub fn to_config(&self) -> Config {
        let mut set = enumset::EnumSet::new();
        for label in &self.enabled {
            for ct in ChallengeType::ALL {
                if ct.label() == label {
                    set.insert(EnabledCategory::from(ct));
                }
            }
        }
        if set.is_empty() {
            set = enumset::EnumSet::all();
        }
        Config {
            default_duration_sec: self.default_duration_sec.max(1),
            enabled: set,
            random_mode: self.random_mode,
            custom_content_path: self
                .custom_content_path
                .as_ref()
                .map(std::path::PathBuf::from),
            custom_tuning_path: self
                .custom_tuning_path
                .as_ref()
                .map(std::path::PathBuf::from),
            audio_device_name: self.audio_device_name.clone(),
            tuning: crate::tuning::TuningId::ALL
                .iter()
                .find(|t| t.label() == self.tuning)
                .map(|&id| crate::custom_tuning::ActiveTuning::Builtin(id))
                .unwrap_or_else(|| crate::custom_tuning::ActiveTuning::Custom { name: self.tuning.clone() }),
            match_pause_ms: self.match_pause_ms,
            // Not currently exposed over the FFI surface (TUI-only setting;
            // Android has no fretboard visualizer to highlight).
            fretboard_highlight: false,
            hard_sequence: self.hard_sequence,
        }
    }
}

/// Construct an [`Engine`] for the foreign frontend.
#[uniffi::export]
pub fn create_engine(
    config: FfiConfig,
    listener: Box<dyn EngineListener>,
) -> Result<Arc<Engine>, FfiError> {
    Engine::new(config.to_config(), listener).map(Arc::new).map_err(Into::into)
}

/// Load the persisted config (or defaults) without constructing an Engine.
/// Lets a frontend seed its `FfiConfig` from disk before it has an Engine.
#[uniffi::export]
pub fn load_config() -> FfiConfig {
    FfiConfig::from(&crate::config::load())
}

/// Engine methods exported across the FFI. Mirrors the public Rust API; the
/// `ffi_set_config` helper accepts an [`FfiConfig`] so Kotlin never sees the
/// `enumset` type.
#[uniffi::export]
impl Engine {
    /// Begin practice (audio + driver loop + first prompt).
    pub fn ffi_start(&self) -> Result<(), FfiError> {
        self.start().map_err(Into::into)
    }

    /// Stop practice and release the mic.
    pub fn ffi_stop(&self) {
        self.stop();
    }

    /// Skip the current prompt (counts as a timeout).
    pub fn ffi_skip(&self) {
        self.skip();
    }

    /// Snapshot of the current configuration as an [`FfiConfig`].
    pub fn ffi_config(&self) -> FfiConfig {
        FfiConfig::from(&self.config())
    }

    /// Apply a new config from the foreign side and persist it.
    pub fn ffi_set_config(&self, config: FfiConfig) -> Result<(), FfiError> {
        let cfg = config.to_config();
        self.set_config(cfg.clone());
        crate::config::save(&cfg).map_err(Into::into)
    }

    /// Timer progress for the UI.
    pub fn ffi_progress(&self) -> FfiProgress {
        let (frac, secs, prompt_secs) = self.progress();
        FfiProgress {
            frac,
            secs,
            prompt_secs,
        }
    }
}

// `uniffi::setup_scaffolding!()` lives in the crate root (`lib.rs`); it
// generates the `UniFfiTag` alias the `#[uniffi::export]` items reference.