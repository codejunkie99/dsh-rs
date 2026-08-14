use crate::skills::{is_skill_name, render_skill_content, SkillLookupOptions, SkillRegistry};
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

impl SkillTool {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills, cwd: None }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

#[derive(Debug, Deserialize)]
struct SkillArguments {
    name: String,
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
