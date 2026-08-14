use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::prompt::SystemPromptConfig;

pub const SUPPORTED_TOOLS: [&str; 5] =
    ["echo", "read_file", "list_dir", "write_file", "run_command"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSetup {
    id: String,
    name: String,
    system_prompt: SystemPromptConfig,
    enabled_tools: Vec<String>,
    max_steps: u32,
}

impl HarnessSetup {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        system_prompt: SystemPromptConfig,
        enabled_tools: Vec<String>,
        max_steps: u32,
    ) -> Result<Self> {
        let setup = Self {
            id: id.into(),
            name: name.into(),
            system_prompt,
            enabled_tools,
            max_steps,
        };
        setup.validate()?;
        Ok(setup)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn system_prompt(&self) -> &SystemPromptConfig {
        &self.system_prompt
    }

    pub fn enabled_tools(&self) -> &[String] {
        &self.enabled_tools
    }

    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    pub fn enables(&self, tool: &str) -> bool {
        self.enabled_tools.iter().any(|enabled| enabled == tool)
    }

    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("harness setup id cannot be empty");
        }
        if self.name.trim().is_empty() {
            bail!("harness setup name cannot be empty");
        }
        if self.max_steps == 0 || self.max_steps > 32 {
            bail!("harness setup max_steps must be between 1 and 32");
        }

        let mut tools = HashSet::new();
        for tool in &self.enabled_tools {
            if !SUPPORTED_TOOLS.contains(&tool.as_str()) {
                bail!("unsupported harness tool: {tool}");
            }
            if !tools.insert(tool.as_str()) {
                bail!("duplicate harness tool: {tool}");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSetupsConfig {
    setups: Vec<HarnessSetup>,
}

impl Default for HarnessSetupsConfig {
    fn default() -> Self {
        Self {
            setups: vec![
                HarnessSetup::new(
                    "standard",
                    "Standard",
                    SystemPromptConfig {
                        include_harness_identity: true,
                        persona: "You are a careful software engineer. Inspect the selected space before changing files and prefer small verified changes.".into(),
                    },
                    vec![
                        "echo".into(),
                        "read_file".into(),
                        "list_dir".into(),
                        "write_file".into(),
                        "run_command".into(),
                    ],
                    8,
                )
                .expect("standard setup is valid"),
                HarnessSetup::new(
                    "research",
                    "Research",
                    SystemPromptConfig {
                        include_harness_identity: true,
                        persona: "Read the selected workspace and answer from evidence. Do not mutate files.".into(),
                    },
                    vec!["echo".into(), "read_file".into(), "list_dir".into()],
                    4,
                )
                .expect("research setup is valid"),
                HarnessSetup::new(
                    "minimal",
                    "Minimal",
                    SystemPromptConfig {
                        include_harness_identity: true,
                        persona: "You are a focused assistant.".into(),
                    },
                    vec!["echo".into()],
                    2,
                )
                .expect("minimal setup is valid"),
            ],
        }
    }
}

impl HarnessSetupsConfig {
    pub fn with_standard_prompt(mut self, prompt: SystemPromptConfig) -> Self {
        if prompt == SystemPromptConfig::default() {
            return self;
        }
        if let Some(setup) = self.setups.iter_mut().find(|setup| setup.id == "standard") {
            setup.system_prompt = prompt;
        }
        self
    }

    pub fn setups(&self) -> &[HarnessSetup] {
        &self.setups
    }

    pub fn get(&self, id: &str) -> Option<&HarnessSetup> {
        self.setups.iter().find(|setup| setup.id == id)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(raw).context("invalid harness setups config")?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read harness setups config {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create harness setups config parent {}",
                    parent.display()
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("harness-setups.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path).with_context(|| {
            format!("failed to publish harness setups config {}", path.display())
        })?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.setups.is_empty() {
            bail!("harness setups config must contain at least one setup");
        }

        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for setup in &self.setups {
            setup.validate()?;
            if !ids.insert(setup.id.trim().to_string()) {
                bail!("duplicate harness setup id: {}", setup.id);
            }
            if !names.insert(setup.name.trim().to_lowercase()) {
                bail!("duplicate harness setup name: {}", setup.name);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validates_and_looks_up_harness_setups() {
        let raw = r#"{
          "setups": [
            {
              "id": "code",
              "name": "Code",
              "system_prompt": {
                "include_harness_identity": true,
                "persona": "Work carefully in the selected repository."
              },
              "enabled_tools": ["echo", "read_file", "list_dir", "write_file"],
              "max_steps": 8
            },
            {
              "id": "research",
              "name": "Research",
              "system_prompt": {
                "include_harness_identity": true,
                "persona": "Answer from files without mutation."
              },
              "enabled_tools": ["echo", "read_file", "list_dir"],
              "max_steps": 4
            }
          ]
        }"#;

        let config = HarnessSetupsConfig::parse(raw).unwrap();
        assert_eq!(config.setups().len(), 2);
        let code = config.get("code").unwrap();
        assert_eq!(code.name(), "Code");
        assert_eq!(
            code.enabled_tools(),
            ["echo", "read_file", "list_dir", "write_file"]
        );
        assert_eq!(code.max_steps(), 8);
        assert!(code.system_prompt().render().is_some());
        assert!(config.get("missing").is_none());
    }

    #[test]
    fn harness_setups_reject_invalid_and_ambiguous_entries() {
        let valid = r#"{
          "setups": [
            {
              "id": "code",
              "name": "Code",
              "system_prompt": {"include_harness_identity": true, "persona": ""},
              "enabled_tools": ["echo"],
              "max_steps": 1
            }
          ]
        }"#;
        assert!(HarnessSetupsConfig::parse(valid).is_ok());
        assert!(HarnessSetupsConfig::parse(r#"{"setups":[]}"#).is_err());
        assert!(HarnessSetupsConfig::parse(&valid.replace("\"code\"", "\"\"")).is_err());
        assert!(HarnessSetupsConfig::parse(
            r#"{"setups":[{"id":"x","name":"X","system_prompt":{"include_harness_identity":true,"persona":""},"enabled_tools":["unknown"],"max_steps":1}]}"#
        )
        .is_err());
        assert!(HarnessSetupsConfig::parse(
            r#"{"setups":[{"id":"x","name":"X","system_prompt":{"include_harness_identity":true,"persona":""},"enabled_tools":["echo"],"max_steps":0}]}"#
        )
        .is_err());
        assert!(HarnessSetupsConfig::parse(
            r#"{"setups":[{"id":"x","name":"X","system_prompt":{"include_harness_identity":true,"persona":""},"enabled_tools":["echo"],"max_steps":33}]}"#
        )
        .is_err());
        let duplicate = r#"{
          "setups": [
            {"id":"x","name":"X","system_prompt":{"include_harness_identity":true,"persona":""},"enabled_tools":["echo"],"max_steps":1},
            {"id":"x","name":"Y","system_prompt":{"include_harness_identity":true,"persona":""},"enabled_tools":["echo"],"max_steps":1}
          ]
        }"#;
        assert!(HarnessSetupsConfig::parse(duplicate).is_err());
    }

    #[test]
    fn default_harness_setups_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness-setups.json");
        let config = HarnessSetupsConfig::default();

        config.save(&path).unwrap();
        assert_eq!(HarnessSetupsConfig::load(&path).unwrap(), config);
        assert!(config.setups().len() >= 3);
        assert!(config.get("standard").is_some());
        assert!(config.get("research").is_some());
        assert!(config.get("minimal").is_some());
    }

    #[test]
    fn customized_global_prompts_replace_the_standard_setup_prompt() {
        let prompt = SystemPromptConfig {
            include_harness_identity: false,
            persona: "Existing local persona.".into(),
        };
        let config = HarnessSetupsConfig::default().with_standard_prompt(prompt.clone());

        assert_eq!(config.get("standard").unwrap().system_prompt(), &prompt);
        assert_ne!(config.get("research").unwrap().system_prompt(), &prompt);
    }

    #[test]
    fn default_global_prompts_do_not_replace_the_standard_setup_prompt() {
        let defaults = HarnessSetupsConfig::default();
        let migrated = defaults
            .clone()
            .with_standard_prompt(SystemPromptConfig::default());

        assert_eq!(migrated, defaults);
    }
}
