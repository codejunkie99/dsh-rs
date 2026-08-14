use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod appearance;
pub mod providers;
pub mod widgets;

pub const SIDEBAR_MIN: f32 = 208.0;
pub const SIDEBAR_MAX: f32 = 400.0;
pub const SIDEBAR_DEFAULT: f32 = 256.0;
pub const RIGHT_PANE_MIN: f32 = 360.0;
pub const RIGHT_PANE_MAX: f32 = 760.0;
pub const RIGHT_PANE_DEFAULT: f32 = 520.0;
pub const TERMINAL_MIN_HEIGHT: f32 = 160.0;
pub const TERMINAL_MAX_VH: f32 = 0.55;
pub const TERMINAL_ABS_MAX_HEIGHT: f32 = 2000.0;
pub const TERMINAL_DEFAULT_HEIGHT: f32 = 280.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSettings {
    pub sidebar_visible: bool,
    pub context_pane_visible: bool,
    #[serde(default = "default_terminal_visible")]
    pub terminal_visible: bool,
    #[serde(default)]
    pub appearance: crate::appearance::AppearanceMode,
}

fn default_terminal_visible() -> bool {
    true
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            context_pane_visible: true,
            terminal_visible: true,
            appearance: crate::appearance::AppearanceMode::System,
        }
    }
}

impl UiSettings {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).context("invalid UI settings")
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read UI settings {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create UI settings parent {}", parent.display())
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("ui.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("failed to publish UI settings {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_settings_default_to_comet_workbench_panes() {
        let settings = UiSettings::default();
        assert!(settings.sidebar_visible);
        assert!(settings.context_pane_visible);
        assert!(settings.terminal_visible);
    }

    #[test]
    fn ui_settings_round_trip_and_reject_unknown_fields() {
        let path = std::env::temp_dir().join(format!("dsh-rs-ui-{}.json", uuid::Uuid::new_v4()));
        let settings = UiSettings {
            sidebar_visible: false,
            context_pane_visible: false,
            terminal_visible: false,
            appearance: crate::appearance::AppearanceMode::Dark,
        };

        settings.save(&path).unwrap();
        assert_eq!(UiSettings::load(&path).unwrap(), settings);
        assert!(UiSettings::parse("{\"sidebar_visible\":true}").is_err());
    }

    #[test]
    fn legacy_ui_settings_default_the_terminal_dock_to_visible() {
        let settings =
            UiSettings::parse(r#"{"sidebar_visible":true,"context_pane_visible":false}"#).unwrap();

        assert!(settings.terminal_visible);
        assert!(!settings.context_pane_visible);
    }
}
