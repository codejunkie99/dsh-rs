use crate::skills::{is_skill_name, render_skill_content, SkillLookupOptions, SkillRegistry};
pub use crate::skills::{SkillCatalogEntry, SkillSlashEntry};
use crate::tools::{Tool, ToolInvocation, ToolOutput, ToolSpec};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SkillTool {
    skills: Arc<SkillRegistry>,
    cwd: Option<PathBuf>,
}

pub const CATALOG_DESCRIPTION_MAX_LENGTH: usize = 500;

impl SkillTool {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills, cwd: None }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub async fn catalog_entries(&self) -> Result<Vec<SkillCatalogEntry>> {
        let options = SkillLookupOptions {
            cwd: self.cwd.clone(),
        };
        Ok(self
            .skills
            .list(&options)
            .await?
            .into_iter()
            .filter(|skill| skill.invocation.model_invocable)
            .map(|skill| SkillCatalogEntry {
                name: skill.name,
                description: catalog_description(&skill.description),
            })
            .collect())
    }

    pub async fn slash_entries(&self) -> Result<Vec<SkillSlashEntry>> {
        let options = SkillLookupOptions {
            cwd: self.cwd.clone(),
        };
        Ok(self
            .skills
            .list(&options)
            .await?
            .into_iter()
            .filter(|skill| skill.invocation.user_invocable)
            .map(|skill| SkillSlashEntry {
                name: skill.name,
                description: skill.description,
                model_invocable: skill.invocation.model_invocable,
            })
            .collect())
    }

    pub async fn invoke_user(&self, name: &str) -> Result<Option<String>> {
        let options = SkillLookupOptions {
            cwd: self.cwd.clone(),
        };
        let Some(skill) = self.skills.get(name, &options).await? else {
            return Ok(None);
        };
        if !skill.invocation.user_invocable {
            return Ok(None);
        }
        Ok(Some(render_skill_content(
            &skill.name,
            &skill.provider,
            skill.resource_base,
            &skill.content,
        )))
    }
}

#[derive(Debug, Deserialize)]
struct SkillArguments {
    name: String,
}

pub fn render_skill_catalog(entries: &[SkillCatalogEntry]) -> String {
    let mut lines = vec![
        "<system-reminder>".to_string(),
        "A skill is a reusable set of task-specific instructions. The following skills are available in this session:".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    lines.extend(entries.iter().map(|entry| {
        format!(
            "- `{}`: {}",
            entry.name,
            escape_catalog_text(&entry.description)
        )
    }));
    lines.extend([
        "</available_skills>".to_string(),
        String::new(),
        "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.".to_string(),
        "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.".to_string(),
        "</system-reminder>".to_string(),
    ]);
    lines.join("\n")
}

pub fn render_skill_catalog_update(entries: &[SkillCatalogEntry]) -> String {
    let availability: &[&str] = if entries.is_empty() {
        &[
            "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.",
            "A user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it.",
        ]
    } else {
        &[
            "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact skill name before acting.",
            "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
        ]
    };

    let mut lines = vec![
        "<system-reminder>".to_string(),
        "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    lines.extend(entries.iter().map(|entry| {
        format!(
            "- `{}`: {}",
            entry.name,
            escape_catalog_text(&entry.description)
        )
    }));
    lines.extend([
        "</available_skills>".to_string(),
        String::new(),
        availability[0].to_string(),
        availability[1].to_string(),
        "</system-reminder>".to_string(),
    ]);
    lines.join("\n")
}

fn catalog_description(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= CATALOG_DESCRIPTION_MAX_LENGTH {
        return normalized;
    }
    format!(
        "{}...",
        normalized
            .chars()
            .take(CATALOG_DESCRIPTION_MAX_LENGTH - 3)
            .collect::<String>()
    )
}

fn escape_catalog_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description: "Load the full instructions for an available skill. Call this with the exact skill name from the session skill catalog before acting on a task that names or clearly matches that skill.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The exact skill name from the available skills list."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let arguments: SkillArguments = serde_json::from_value(invocation.arguments)
            .map_err(|error| anyhow::anyhow!("invalid skill arguments: {error}"))?;
        if !is_skill_name(&arguments.name) {
            bail!("invalid skill name \"{}\"", arguments.name);
        }
        let options = SkillLookupOptions {
            cwd: self.cwd.clone(),
        };
        let summaries = self.skills.list(&options).await?;
        let Some(summary) = summaries
            .iter()
            .find(|summary| summary.name == arguments.name)
        else {
            bail!(
                "skill \"{}\" is unknown or no longer available",
                arguments.name
            );
        };
        if !summary.invocation.model_invocable {
            bail!(
                "skill \"{}\" is not available for model invocation",
                arguments.name
            );
        }
        let Some(skill) = self.skills.get(&arguments.name, &options).await? else {
            bail!(
                "skill \"{}\" is unknown or no longer available",
                arguments.name
            );
        };
        if !skill.invocation.model_invocable {
            bail!(
                "skill \"{}\" is not available for model invocation",
                arguments.name
            );
        }
        Ok(ToolOutput {
            ok: true,
            output: render_skill_content(
                &skill.name,
                &skill.provider,
                skill.resource_base,
                &skill.content,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_the_dsh_tool_contract() {
        let registry = Arc::new(SkillRegistry::new());
        let spec = SkillTool::new(registry).spec();
        assert_eq!(spec.name, "skill");
        assert!(spec
            .description
            .contains("exact skill name from the session skill catalog"));
        assert_eq!(
            spec.parameters,
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The exact skill name from the available skills list."
                    }
                },
                "required": ["name"]
            })
        );
    }
}
