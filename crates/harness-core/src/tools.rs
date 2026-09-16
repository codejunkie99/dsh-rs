pub mod fs;
pub mod shell;
pub mod skill;
pub mod search;
pub mod todo;

pub use skill::SkillTool;
pub use todo::TodoTool;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::session::SharedSessionLog;
use crate::skills::SkillScope;

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

/// Category of a tool call, used by a UI to pick an icon or treatment. Mirrors
/// upstream `ToolCallKind` (`packages/core/tools/src/presentation.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Fetch,
    Other,
}

/// Pending-call presentation a tool declares via `present_call`. This is the
/// generic card arm; terminal and diff cards are added only when their tools
/// arrive. Mirrors upstream `GenericCallView`
/// (`packages/core/tools/src/presentation.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "camelCase")]
pub enum ToolCallView {
    #[serde(rename_all = "camelCase")]
    Generic {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ToolCallKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input: Option<serde_json::Value>,
    },
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, invocation: ToolInvocation) -> anyhow::Result<ToolOutput>;

    /// Presentation metadata for a pending call, mirroring upstream
    /// `ToolDefinition.presentCall`. Returns `None` when the tool has no custom
    /// presentation (the UI falls back to the raw arguments).
    fn present_call(&self, _arguments: &serde_json::Value) -> Option<ToolCallView> {
        None
    }

    async fn execute_with_session(
        &self,
        invocation: ToolInvocation,
        _session: Option<SharedSessionLog>,
    ) -> anyhow::Result<ToolOutput> {
        self.execute(invocation).await
    }
}

#[derive(Clone)]
pub struct ToolRegistry {
    inner: Arc<ToolRegistryInner>,
}

struct ToolRegistryInner {
    state: parking_lot::RwLock<ToolRegistryState>,
}

struct ToolRegistryState {
    global: HashMap<String, Arc<dyn Tool>>,
    scoped: HashMap<u64, HashMap<String, Arc<dyn Tool>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ToolRegistryInner {
                state: parking_lot::RwLock::new(ToolRegistryState {
                    global: HashMap::new(),
                    scoped: HashMap::new(),
                }),
            }),
        }
    }

    /// Register globally. Mirrors upstream `ToolRuntime.register` from an
    /// unscoped context (`packages/core/tools/src/index.ts:1037`). A later
    /// same-name global registration replaces the prior one, preserving the
    /// original flat-registry behavior.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let spec = tool.spec();
        self.inner.state.write().global.insert(spec.name, tool);
    }

    /// Register into one agent scope's layer. Scoped tools shadow globals on a
    /// name conflict; a duplicate within the same scope fails, mirroring
    /// upstream `ToolLayer`'s `NamedEntries` throw
    /// (`packages/core/tools/src/index.ts:726`).
    pub fn register_for_scope(
        &mut self,
        scope: SkillScope,
        tool: Arc<dyn Tool>,
    ) -> anyhow::Result<()> {
        let spec = tool.spec();
        let name = spec.name.clone();
        let mut state = self.inner.state.write();
        let layer = state.scoped.entry(scope.id()).or_default();
        if layer.contains_key(&name) {
            anyhow::bail!("tool \"{name}\" is already registered in this scope");
        }
        layer.insert(name, tool);
        Ok(())
    }

    /// Exact-identity visibility: the tool one scope resolves, applying global
    /// registration then chain-layer shadowing with the nearest scope winning.
    /// Mirrors upstream `ToolRuntime.get(name, scope)` through `view(scope)`
    /// (`packages/core/tools/src/index.ts:1204`).
    pub fn get(&self, name: &str, scope: Option<SkillScope>) -> Option<Arc<dyn Tool>> {
        self.view(scope).get(name).cloned()
    }

    fn view(&self, scope: Option<SkillScope>) -> HashMap<String, Arc<dyn Tool>> {
        let state = self.inner.state.read();
        let mut visible = state.global.clone();
        if let Some(scope) = scope {
            // Farthest ancestor first, exact scope last: a nearer scope's
            // same-name entry shadows a farther one.
            for key in scope.chain().into_iter().rev() {
                if let Some(layer) = state.scoped.get(&key.id()) {
                    for (name, tool) in layer {
                        visible.insert(name.clone(), tool.clone());
                    }
                }
            }
        }
        visible
    }

    pub fn specs(&self) -> Vec<crate::llm::ToolSchema> {
        self.specs_for(None)
    }

    /// The model-facing schemas one scope resolves, mirroring upstream
    /// `ToolRuntime.schemas(scope)`.
    pub fn specs_for(&self, scope: Option<SkillScope>) -> Vec<crate::llm::ToolSchema> {
        self.view(scope)
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
        let scope = session.as_ref().and_then(|log| log.skill_scope());
        match self.get(&invocation.name, scope) {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct NamedTool(String);

    #[async_trait]
    impl Tool for NamedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.0.clone(),
                description: "test".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            }
        }

        async fn execute(&self, _invocation: ToolInvocation) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput {
                ok: true,
                output: String::new(),
            })
        }
    }

    fn tool(name: &str) -> Arc<dyn Tool> {
        Arc::new(NamedTool(name.into()))
    }

    #[test]
    fn scoped_tool_shadows_global_by_exact_name() {
        let mut registry = ToolRegistry::new();
        registry.register(tool("echo"));
        let scope = SkillScope::new();
        registry
            .register_for_scope(scope.clone(), tool("echo"))
            .unwrap();

        assert!(registry.get("echo", None).is_some());
        assert!(registry.get("echo", Some(scope)).is_some());
        let other = SkillScope::new();
        assert!(registry.get("echo", Some(other)).is_some());
    }

    #[test]
    fn get_is_exact_identity_across_scopes() {
        let mut registry = ToolRegistry::new();
        let a = SkillScope::new();
        registry
            .register_for_scope(a.clone(), tool("a-tool"))
            .unwrap();

        assert!(registry.get("a-tool", None).is_none());
        assert!(registry.get("a-tool", Some(a)).is_some());

        let b = SkillScope::new();
        assert!(registry.get("a-tool", Some(b)).is_none());
    }

    #[test]
    fn child_scope_inherits_ancestor_registration() {
        let mut registry = ToolRegistry::new();
        let preset = SkillScope::new();
        registry
            .register_for_scope(preset.clone(), tool("preset-tool"))
            .unwrap();

        let agent = SkillScope::new();
        agent.bind_parent(preset.clone()).unwrap();
        assert!(registry.get("preset-tool", Some(agent)).is_some());
    }

    #[test]
    fn nearest_scope_shadows_ancestor() {
        let mut registry = ToolRegistry::new();
        let preset = SkillScope::new();
        let agent = SkillScope::new();
        agent.bind_parent(preset.clone()).unwrap();

        let ancestor_tool = tool("tool");
        let agent_tool = tool("tool");
        registry
            .register_for_scope(preset.clone(), ancestor_tool.clone())
            .unwrap();
        registry
            .register_for_scope(agent.clone(), agent_tool.clone())
            .unwrap();

        let resolved = registry.get("tool", Some(agent)).unwrap();
        assert!(Arc::ptr_eq(&resolved, &agent_tool));
        let ancestor_resolved = registry.get("tool", Some(preset)).unwrap();
        assert!(Arc::ptr_eq(&ancestor_resolved, &ancestor_tool));
        assert!(registry.get("tool", None).is_none());
    }

    #[test]
    fn duplicate_name_in_the_same_scope_is_rejected() {
        let mut registry = ToolRegistry::new();
        let scope = SkillScope::new();
        registry
            .register_for_scope(scope.clone(), tool("dup"))
            .unwrap();
        assert!(registry.register_for_scope(scope, tool("dup")).is_err());
    }

    #[test]
    fn specs_for_reflects_the_scope_view() {
        let mut registry = ToolRegistry::new();
        registry.register(tool("global-tool"));
        let scope = SkillScope::new();
        registry
            .register_for_scope(scope.clone(), tool("scoped-tool"))
            .unwrap();

        let names = |schemas: Vec<crate::llm::ToolSchema>| {
            let mut names: Vec<_> = schemas.into_iter().map(|schema| schema.name).collect();
            names.sort();
            names
        };
        assert_eq!(names(registry.specs()), ["global-tool"]);
        assert_eq!(
            names(registry.specs_for(Some(scope))),
            ["global-tool", "scoped-tool"]
        );
    }
}
