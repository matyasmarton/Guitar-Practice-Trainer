//! Configuration model and TOML persistence via `directories`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use enumset::{EnumSet, EnumSetType};
use serde::{Deserialize, Serialize};

use crate::challenges::ChallengeType;

/// The set of enabled challenge categories, serde-friendly via `enumset`.
#[derive(EnumSetType, Serialize, Deserialize, Debug)]
#[enumset(serialize_repr = "list")]
#[serde(crate = "serde")]
pub enum EnabledCategory {
    Note,
    Chord,
    Scale,
    Mode,
    Progression,
    Lick,
    Piece,
}

impl From<ChallengeType> for EnabledCategory {
    fn from(c: ChallengeType) -> Self {
        match c {
            ChallengeType::Note => EnabledCategory::Note,
            ChallengeType::Chord => EnabledCategory::Chord,
            ChallengeType::Scale => EnabledCategory::Scale,
            ChallengeType::Mode => EnabledCategory::Mode,
            ChallengeType::Progression => EnabledCategory::Progression,
            ChallengeType::Lick => EnabledCategory::Lick,
            ChallengeType::Piece => EnabledCategory::Piece,
        }
    }
}

impl From<EnabledCategory> for ChallengeType {
    fn from(c: EnabledCategory) -> Self {
        match c {
            EnabledCategory::Note => ChallengeType::Note,
            EnabledCategory::Chord => ChallengeType::Chord,
            EnabledCategory::Scale => ChallengeType::Scale,
            EnabledCategory::Mode => ChallengeType::Mode,
            EnabledCategory::Progression => ChallengeType::Progression,
            EnabledCategory::Lick => ChallengeType::Lick,
            EnabledCategory::Piece => ChallengeType::Piece,
        }
    }
}

/// Persisted user configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// Default per-prompt countdown, in seconds.
    pub default_duration_sec: u32,
    /// Which challenge categories are active.
    pub enabled: EnumSet<EnabledCategory>,
    /// Random mode: randomize the time window *and* content per prompt.
    pub random_mode: bool,
    /// Optional path to a custom-content TOML file.
    #[serde(default)]
    pub custom_content_path: Option<PathBuf>,
    /// Optional preferred input audio device name (Mac picker).
    #[serde(default)]
    pub audio_device_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_duration_sec: 30,
            enabled: EnumSet::all(),
            random_mode: false,
            custom_content_path: None,
            audio_device_name: None,
        }
    }
}

impl Config {
    /// Resolve the active category list as `ChallengeType`s (non-empty guard
    /// falls back to all enabled so the engine never has nothing to draw).
    pub fn active_categories(&self) -> Vec<ChallengeType> {
        let v: Vec<ChallengeType> = self.enabled.iter().map(Into::into).collect();
        if v.is_empty() {
            ChallengeType::ALL.to_vec()
        } else {
            v
        }
    }
}

/// Locate the config directory for this project (`dev.guitartrainer.guitar-trainer`).
pub fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "guitartrainer", "guitar-trainer")
}

/// Path to the persisted `config.toml`.
pub fn config_path() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().join("config.toml"))
}

/// Load config from the default path, falling back to defaults on any error
/// (malformed file, IO failure) so the engine never crashes on bad config.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    load_from(&path).unwrap_or_else(|e| {
        tracing::warn!("config load failed ({e:?}); using defaults");
        Config::default()
    })
}

/// Load config from an explicit path.
pub fn load_from(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
    Ok(cfg)
}

/// Persist config to the default path (creating the directory if needed).
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path()
        .ok_or_else(|| anyhow::anyhow!("no config directory available on this platform"))?;
    save_to(cfg, &path)
}

/// Persist config to an explicit path.
pub fn save_to(cfg: &Config, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(cfg).context("serializing config")?;
    std::fs::write(path, text)
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_all_categories() {
        let c = Config::default();
        assert_eq!(c.default_duration_sec, 30);
        assert!(!c.random_mode);
        assert_eq!(c.enabled, EnumSet::all());
    }

    #[test]
    fn active_categories_round_trip() {
        let mut c = Config::default();
        c.enabled = EnabledCategory::Note | EnabledCategory::Chord;
        let v = c.active_categories();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn empty_enabled_falls_back_to_all() {
        let mut c = Config::default();
        c.enabled = EnumSet::empty();
        assert_eq!(c.active_categories().len(), ChallengeType::ALL.len());
    }

    #[test]
    fn round_trip_toml() {
        let mut c = Config::default();
        c.default_duration_sec = 5;
        c.random_mode = true;
        c.enabled = EnabledCategory::Mode | EnabledCategory::Lick;
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.default_duration_sec, 5);
        assert!(back.random_mode);
        assert_eq!(back.enabled, EnabledCategory::Mode | EnabledCategory::Lick);
    }

    #[test]
    fn malformed_toml_falls_back_gracefully() {
        // save_to then load_from a bad file: load_from should error, caller
        // falls back to defaults.
        let dir = std::env::temp_dir().join("gtt_cfg_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(&p, "this is = = not toml").unwrap();
        assert!(load_from(&p).is_err());
    }
}