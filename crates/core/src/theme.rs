//! Theme (design-element) configuration, loaded from `theme.toml` next to
//! `config.toml`. Colors are plain hex strings (`#RRGGBB`) so both the TUI
//! (ratatui `Color`) and Android (Compose `Color`) can parse the same schema
//! into their own native color type — this module stays UI-agnostic.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub accent: String,
    pub success: String,
    pub danger: String,
    pub secondary: String,
    pub selection_bg: String,
    pub selection_fg: String,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: "#FFFF00".into(),
            success: "#00FF00".into(),
            danger: "#FF0000".into(),
            secondary: "#00FFFF".into(),
            selection_bg: "#FFFF00".into(),
            selection_fg: "#000000".into(),
        }
    }
}

pub fn theme_path() -> Option<PathBuf> {
    crate::config::project_dirs().map(|d| d.config_dir().join("theme.toml"))
}

/// Load the theme from the default path, falling back to defaults on any
/// error (missing file, malformed TOML) so the UI never fails to render.
pub fn load() -> Theme {
    let Some(path) = theme_path() else {
        return Theme::default();
    };
    load_from(&path).unwrap_or_else(|e| {
        tracing::warn!("theme load failed ({e:?}); using defaults");
        Theme::default()
    })
}

/// Load the theme from an explicit path.
pub fn load_from(path: &Path) -> Result<Theme> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading theme {}", path.display()))?;
    let theme: Theme = toml::from_str(&text).context("parsing theme.toml")?;
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_hex() {
        let t = Theme::default();
        for hex in [
            &t.accent,
            &t.success,
            &t.danger,
            &t.secondary,
            &t.selection_bg,
            &t.selection_fg,
        ] {
            assert!(hex.starts_with('#'), "{hex} missing '#'");
            assert_eq!(hex.len(), 7, "{hex} not #RRGGBB length");
        }
    }

    #[test]
    fn round_trip_toml() {
        let mut t = Theme::default();
        t.accent = "#123456".into();
        t.selection_fg = "#abcdef".into();
        let text = toml::to_string_pretty(&t).unwrap();
        let back: Theme = toml::from_str(&text).unwrap();
        assert_eq!(back.accent, "#123456");
        assert_eq!(back.selection_fg, "#abcdef");
    }

    #[test]
    fn malformed_toml_falls_back_gracefully() {
        let dir = std::env::temp_dir().join("gtt_theme_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(&p, "this is = = not toml").unwrap();
        assert!(load_from(&p).is_err());
    }
}
