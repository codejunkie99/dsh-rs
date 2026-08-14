pub mod fs;
pub mod shell;
pub mod todo;

pub use todo::TodoTool;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::session::SharedSessionLog;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub ok: bool,
    pub output: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, invocation: ToolInvocation) -> anyhow::Result<ToolOutput>;

    async fn execute_with_session(
        &self,
        invocation: ToolInvocation,
        _session: Option<SharedSessionLog>,
    ) -> anyhow::Result<ToolOutput> {
        self.execute(invocation).await
    }
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let spec = tool.spec();
        self.tools.insert(spec.name, tool);
    }

    pub fn specs(&self) -> Vec<crate::llm::ToolSchema> {
        self.tools
            .values()
            .map(|tool| {
                let spec = tool.spec();
                crate::llm::ToolSchema {
                    name: spec.name,
                    description: spec.description,
                    parameters: spec.parameters,
                }
            })
            .collect()
    }

    pub async fn execute(&self, invocation: ToolInvocation) -> ToolOutput {
        self.execute_with_session(invocation, None).await
    }

    pub async fn execute_with_session(
        &self,
        invocation: ToolInvocation,
        session: Option<SharedSessionLog>,
    ) -> ToolOutput {
        match self.tools.get(&invocation.name) {
            Some(tool) => match tool.execute_with_session(invocation, session).await {
                Ok(output) => output,
                Err(err) => ToolOutput {
                    ok: false,
                    output: err.to_string(),
                },
            },
            None => ToolOutput {
                ok: false,
                output: format!("unknown tool: {}", invocation.name),
            },
        }
    }
}

pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "Echo the provided text back to the model.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        }
    }

    async fn execute(&self, invocation: ToolInvocation) -> anyhow::Result<ToolOutput> {
        let text = invocation
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(ToolOutput {
            ok: true,
            output: text.to_string(),
        })
    }
}
