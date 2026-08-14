use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const HARNESS_IDENTITY: &str = "You are an AI agent powered by DeepSeek Harness.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemPromptConfig {
    pub include_harness_identity: bool,
    pub persona: String,
}

impl Default for SystemPromptConfig {
    fn default() -> Self {
        Self {
            include_harness_identity: true,
            persona: String::new(),
        }
    }
}

impl SystemPromptConfig {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).context("invalid system prompt config")
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read system prompt config {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create system prompt config parent {}",
                    parent.display()
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("system-prompt.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path).with_context(|| {
            format!("failed to publish system prompt config {}", path.display())
        })?;
        Ok(())
    }

    pub fn render(&self) -> Option<String> {
        let mut sections = Vec::new();
        if self.include_harness_identity {
            sections.push(HARNESS_IDENTITY);
        }
        if !self.persona.trim().is_empty() {
            sections.push(self.persona.trim());
        }
        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_config_round_trips_and_renders_deterministically() {
        let config = SystemPromptConfig {
            include_harness_identity: true,
            persona: "Be precise and cite sources.".into(),
        };

        assert_eq!(
            config.render().as_deref(),
            Some(
                "You are an AI agent powered by DeepSeek Harness.\n\nBe precise and cite sources."
            )
        );

        let raw = serde_json::to_string(&config).unwrap();
        assert_eq!(SystemPromptConfig::parse(&raw).unwrap(), config);
    }

    #[test]
    fn empty_prompts_are_absent_and_extra_fields_are_rejected() {
        assert_eq!(
            SystemPromptConfig {
                include_harness_identity: false,
                persona: String::new(),
            }
            .render(),
            None
        );
        assert_eq!(
            SystemPromptConfig::default().render().as_deref(),
            Some(HARNESS_IDENTITY)
        );
        assert!(SystemPromptConfig::parse(
            r#"{"include_harness_identity":true,"persona":"","extra":"no"}"#
        )
        .is_err());
    }

    #[test]
    fn system_prompt_config_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system-prompt.json");
        let config = SystemPromptConfig {
            include_harness_identity: false,
            persona: "Custom persona".into(),
        };

        config.save(&path).unwrap();
        assert_eq!(SystemPromptConfig::load(&path).unwrap(), config);
        assert!(SystemPromptConfig::load(dir.path().join("missing.json")).is_err());
    }
}
