use crate::session::SharedSessionLog;
use crate::skills::{
    is_skill_name, render_skill_content, SkillRegistry, SkillScope, SkillViewOptions,
};
pub use crate::skills::{SkillCatalogEntry, SkillSlashEntry};
use crate::tools::{Tool, ToolCallKind, ToolCallView, ToolInvocation, ToolOutput, ToolSpec};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SkillTool {
    skills: Arc<SkillRegistry>,
    cwd: Option<PathBuf>,
    catalog_description_max_length: usize,
}

pub const CATALOG_DESCRIPTION_MAX_LENGTH: usize = 500;

/// A revision-consistent slash-menu snapshot used to drive event-driven
/// refresh. Callers remember `revision` and re-read whenever the registry
/// reports a newer revision, or when a provider watcher invalidates the
/// catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlashSnapshot {
    pub revision: u64,
    pub entries: Vec<SkillSlashEntry>,
}

impl SkillTool {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self {
            skills,
            cwd: None,
            catalog_description_max_length: CATALOG_DESCRIPTION_MAX_LENGTH,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set the maximum normalized description length rendered in the session
    /// catalog, mirroring upstream `tool-skill` `Config.catalogDescriptionMaxLength`
    /// (`packages/skill/tool-skill/src/index.ts`). Minimum is 3, matching the
    /// upstream `assertPositiveInteger(..., 3)` validation.
    pub fn with_catalog_description_max_length(mut self, max_length: usize) -> Self {
        assert!(
            max_length >= 3,
            "catalogDescriptionMaxLength must be an integer greater than or equal to 3"
        );
        self.catalog_description_max_length = max_length;
        self
    }

    pub fn registry(&self) -> &Arc<SkillRegistry> {
        &self.skills
    }

    fn options(&self, scope: Option<SkillScope>) -> SkillViewOptions {
        SkillViewOptions {
            cwd: self.cwd.clone(),
            scope,
            ..Default::default()
        }
    }

    pub async fn catalog_entries_for(
        &self,
        scope: Option<SkillScope>,
    ) -> Result<Vec<SkillCatalogEntry>> {
        let options = self.options(scope);
        Ok(self
            .skills
            .list(&options)
            .await?
            .into_iter()
            .filter(|skill| skill.invocation.model_invocable)
            .map(|skill| SkillCatalogEntry {
                name: skill.name,
                description: catalog_description(
                    &skill.description,
                    self.catalog_description_max_length,
                ),
            })
            .collect())
    }

    pub async fn catalog_entries(&self) -> Result<Vec<SkillCatalogEntry>> {
        self.catalog_entries_for(None).await
    }

    pub async fn slash_snapshot_for(
        &self,
        scope: Option<SkillScope>,
    ) -> Result<SkillSlashSnapshot> {
        let options = self.options(scope);
        let entries = self
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
            .collect();
        Ok(SkillSlashSnapshot {
            revision: self.skills.revision(),
            entries,
        })
    }

    pub async fn slash_snapshot(&self) -> Result<SkillSlashSnapshot> {
        self.slash_snapshot_for(None).await
    }

    pub async fn slash_entries(&self) -> Result<Vec<SkillSlashEntry>> {
        Ok(self.slash_snapshot().await?.entries)
    }

    pub async fn slash_entries_for(
        &self,
        scope: Option<SkillScope>,
    ) -> Result<Vec<SkillSlashEntry>> {
        Ok(self.slash_snapshot_for(scope).await?.entries)
    }

    pub async fn invoke_user_for(
        &self,
        name: &str,
        scope: Option<SkillScope>,
    ) -> Result<Option<String>> {
        let options = self.options(scope);
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

    pub async fn invoke_user(&self, name: &str) -> Result<Option<String>> {
        self.invoke_user_for(name, None).await
    }

    async fn execute_scoped(
        &self,
        invocation: ToolInvocation,
        scope: Option<SkillScope>,
    ) -> Result<ToolOutput> {
        let arguments: SkillArguments = serde_json::from_value(invocation.arguments)
            .map_err(|error| anyhow::anyhow!("invalid skill arguments: {error}"))?;
        if !is_skill_name(&arguments.name) {
            bail!("invalid skill name \"{}\"", arguments.name);
        }
        let options = self.options(scope);
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

fn catalog_description(value: &str, max_length: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_length {
        return normalized;
    }
    format!(
        "{}...",
        normalized.chars().take(max_length - 3).collect::<String>()
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

    /// Upstream `packages/skill/tool-skill/src/index.ts:157` renders a skill
    /// load as a read card: `{ card: 'generic', title: 'Load skill <name>',
    /// kind: 'read', rawInput: '<name>' }`.
    fn present_call(&self, arguments: &serde_json::Value) -> Option<ToolCallView> {
        let name = arguments.get("name")?.as_str()?;
        Some(ToolCallView::Generic {
            title: format!("Load skill {name}"),
            kind: Some(ToolCallKind::Read),
            raw_input: Some(serde_json::Value::String(name.to_string())),
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        self.execute_scoped(invocation, None).await
    }

    async fn execute_with_session(
        &self,
        invocation: ToolInvocation,
        session: Option<SharedSessionLog>,
    ) -> Result<ToolOutput> {
        let scope = session.as_ref().and_then(|log| log.skill_scope());
        self.execute_scoped(invocation, scope).await
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

    #[test]
    fn catalog_description_truncates_to_a_configured_maximum() {
        let normalized = catalog_description("one  two\nthree", 500);
        assert_eq!(normalized, "one two three");

        let truncated = catalog_description("a very long description", 8);
        // 8 chars minus the 3-char ellipsis leaves 5 chars.
        assert_eq!(truncated, "a ver...");
    }

    #[test]
    #[should_panic(
        expected = "catalogDescriptionMaxLength must be an integer greater than or equal to 3"
    )]
    fn catalog_description_max_length_rejects_values_below_three() {
        let registry = Arc::new(SkillRegistry::new());
        let _ = SkillTool::new(registry).with_catalog_description_max_length(2);
    }

    #[test]
    fn present_call_renders_upstream_skill_load_metadata() {
        let registry = Arc::new(SkillRegistry::new());
        let tool = SkillTool::new(registry);
        let view = tool
            .present_call(&serde_json::json!({ "name": "project-skill" }))
            .unwrap();
        assert_eq!(
            view,
            crate::tools::ToolCallView::Generic {
                title: "Load skill project-skill".into(),
                kind: Some(crate::tools::ToolCallKind::Read),
                raw_input: Some(serde_json::Value::String("project-skill".into())),
            }
        );
        assert_eq!(
            serde_json::to_value(view).unwrap(),
            serde_json::json!({
                "card": "generic",
                "title": "Load skill project-skill",
                "kind": "read",
                "rawInput": "project-skill"
            })
        );
    }
}
