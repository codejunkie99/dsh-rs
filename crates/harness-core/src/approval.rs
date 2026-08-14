use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRule {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Ask,
    Deny,
}

#[async_trait]
pub trait ToolApprover: Send + Sync {
    async fn decide(
        &self,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> ApprovalDecision;
}

#[derive(Debug, Clone)]
pub struct PolicyApprover {
    policy: ApprovalPolicy,
}

impl PolicyApprover {
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self { policy }
    }
}

#[async_trait]
impl ToolApprover for PolicyApprover {
    async fn decide(
        &self,
        _call_id: &str,
        tool_name: &str,
        _arguments: &serde_json::Value,
    ) -> ApprovalDecision {
        self.policy.decide(tool_name)
    }
}

pub struct ApprovalRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub responder: oneshot::Sender<bool>,
}

#[derive(Clone)]
pub struct ChannelApprover {
    requests: mpsc::Sender<ApprovalRequest>,
}

impl ChannelApprover {
    pub fn channel() -> (Self, mpsc::Receiver<ApprovalRequest>) {
        let (requests, receiver) = mpsc::channel(1);
        (Self { requests }, receiver)
    }
}

#[async_trait]
impl ToolApprover for ChannelApprover {
    async fn decide(
        &self,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> ApprovalDecision {
        let (responder, response) = oneshot::channel();
        let request = ApprovalRequest {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            responder,
        };
        if self.requests.send(request).await.is_err() {
            return ApprovalDecision::Deny;
        }
        match response.await {
            Ok(true) => ApprovalDecision::Allow,
            _ => ApprovalDecision::Deny,
        }
    }
}

#[derive(Clone)]
pub struct GatedApprover {
    policy: ApprovalPolicy,
    delegate: ChannelApprover,
}

impl GatedApprover {
    pub fn new(policy: ApprovalPolicy, delegate: ChannelApprover) -> Self {
        Self { policy, delegate }
    }
}

#[async_trait]
impl ToolApprover for GatedApprover {
    async fn decide(
        &self,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> ApprovalDecision {
        match self.policy.decide(tool_name) {
            ApprovalDecision::Allow => ApprovalDecision::Allow,
            ApprovalDecision::Deny => ApprovalDecision::Deny,
            ApprovalDecision::Ask => self.delegate.decide(call_id, tool_name, arguments).await,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    pub default: ApprovalRule,
    pub tools: BTreeMap<String, ApprovalRule>,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            default: ApprovalRule::Deny,
            tools: BTreeMap::from([
                ("echo".into(), ApprovalRule::Allow),
                ("read_file".into(), ApprovalRule::Allow),
                ("list_dir".into(), ApprovalRule::Allow),
                ("todo_write".into(), ApprovalRule::Allow),
                ("skill".into(), ApprovalRule::Allow),
                ("write_file".into(), ApprovalRule::Ask),
                ("run_command".into(), ApprovalRule::Ask),
            ]),
        }
    }
}

impl ApprovalPolicy {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).context("invalid approval policy")
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref()).with_context(|| {
            format!("failed to read approval policy {}", path.as_ref().display())
        })?;
        Self::parse(&raw)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create approval policy parent {}",
                    parent.display()
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("approvals.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("failed to publish approval policy {}", path.display()))?;
        Ok(())
    }

    pub fn decide(&self, tool_name: &str) -> ApprovalDecision {
        let rule = self.tools.get(tool_name).copied().unwrap_or(self.default);
        match rule {
            ApprovalRule::Allow => ApprovalDecision::Allow,
            ApprovalRule::Ask => ApprovalDecision::Ask,
            ApprovalRule::Deny => ApprovalDecision::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_evaluates_explicit_tool_rules_and_fails_closed_for_unknown_tools() {
        let policy = ApprovalPolicy::parse(
            r#"{
              "default": "deny",
              "tools": {
                "read_file": "allow",
                "list_dir": "allow",
                "write_file": "ask",
                "run_command": "ask"
              }
            }"#,
        )
        .unwrap();

        assert_eq!(policy.decide("read_file"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("list_dir"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("write_file"), ApprovalDecision::Ask);
        assert_eq!(policy.decide("run_command"), ApprovalDecision::Ask);
        assert_eq!(policy.decide("shell"), ApprovalDecision::Deny);
    }

    #[test]
    fn policy_rejects_invalid_rules_instead_of_guessing() {
        assert!(ApprovalPolicy::parse(r#"{"default":"always"}"#).is_err());
        assert!(ApprovalPolicy::parse(r#"{"tools":{"read_file":"maybe"}}"#).is_err());
        assert!(ApprovalPolicy::parse("not json").is_err());
    }

    #[test]
    fn policy_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.json");
        let policy = ApprovalPolicy::default();
        policy.save(&path).unwrap();
        let loaded = ApprovalPolicy::load(&path).unwrap();
        assert_eq!(loaded, policy);
    }

    #[test]
    fn default_policy_allows_readonly_tools_and_asks_before_write() {
        let policy = ApprovalPolicy::default();
        assert_eq!(policy.decide("echo"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("read_file"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("list_dir"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("todo_write"), ApprovalDecision::Allow);
        assert_eq!(policy.decide("write_file"), ApprovalDecision::Ask);
        assert_eq!(policy.decide("shell"), ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn channel_approver_waits_for_human_decision() {
        let (approver, mut requests) = ChannelApprover::channel();
        let task = tokio::spawn(async move {
            approver
                .decide(
                    "call_channel",
                    "write_file",
                    &serde_json::json!({"path": "workspace/a.txt"}),
                )
                .await
        });

        let request = requests.recv().await.unwrap();
        assert_eq!(request.tool_name, "write_file");
        assert_eq!(request.call_id, "call_channel");
        assert_eq!(request.arguments["path"], "workspace/a.txt");
        request.responder.send(true).unwrap();
        assert_eq!(task.await.unwrap(), ApprovalDecision::Allow);
    }

    #[tokio::test]
    async fn channel_approver_fails_closed_when_ui_disappears() {
        let (approver, mut requests) = ChannelApprover::channel();
        let task = tokio::spawn(async move {
            approver
                .decide("call_channel", "write_file", &serde_json::json!({}))
                .await
        });
        let request = requests.recv().await.unwrap();
        drop(request);
        assert_eq!(task.await.unwrap(), ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn gated_approver_only_prompts_for_ask_rules() {
        let (channel, mut requests) = ChannelApprover::channel();
        let approver = GatedApprover::new(ApprovalPolicy::default(), channel);

        let read = tokio::spawn({
            let approver = approver.clone();
            async move {
                approver
                    .decide("call_read", "read_file", &serde_json::json!({}))
                    .await
            }
        });
        assert_eq!(read.await.unwrap(), ApprovalDecision::Allow);

        let denied = tokio::spawn({
            let approver = approver.clone();
            async move {
                approver
                    .decide("call_shell", "shell", &serde_json::json!({}))
                    .await
            }
        });
        assert_eq!(denied.await.unwrap(), ApprovalDecision::Deny);

        let write = tokio::spawn({
            let approver = approver.clone();
            async move {
                approver
                    .decide(
                        "call_write",
                        "write_file",
                        &serde_json::json!({"path": "a.txt"}),
                    )
                    .await
            }
        });
        let request = requests.recv().await.unwrap();
        assert_eq!(request.call_id, "call_write");
        request.responder.send(true).unwrap();
        assert_eq!(write.await.unwrap(), ApprovalDecision::Allow);
    }
}
