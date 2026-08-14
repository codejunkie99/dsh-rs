use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashSet;
use uuid::Uuid;

use crate::skills::SkillCatalogEntry;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

pub fn validate_todo_snapshot(todos: &[TodoItem]) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for todo in todos {
        if todo.content.trim() != todo.content || todo.content.is_empty() {
            anyhow::bail!("todo content must be non-empty and already trimmed");
        }
        if !seen.insert(todo.content.as_str()) {
            anyhow::bail!("todo list repeats content {:?}", todo.content);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted {
        title: Option<String>,
        model: Option<String>,
        #[serde(default)]
        space_id: Option<String>,
        #[serde(default)]
        harness_id: Option<String>,
    },
    SessionTitleChanged {
        title: String,
    },
    SessionModelChanged {
        model: String,
    },
    SessionSpaceChanged {
        space_id: String,
    },
    SessionHarnessChanged {
        harness_id: String,
    },
    SystemPromptSnapshot {
        content: Option<String>,
    },
    TurnStarted,
    StepStarted {
        index: u64,
    },
    UserMessage {
        id: Uuid,
        content: String,
    },
    SkillCatalogPublished {
        entries: Vec<SkillCatalogEntry>,
        content: String,
        #[serde(default)]
        update: bool,
    },
    SkillInvocationInjected {
        name: String,
        content: String,
    },
    AssistantChunk {
        message_id: Uuid,
        delta: String,
    },
    AssistantMessage {
        id: Uuid,
        content: String,
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    TodoWrite {
        todos: Vec<TodoItem>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ApprovalRequested {
        call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
        reason: String,
    },
    ToolResult {
        call_id: String,
        ok: bool,
        output: String,
    },
    StepCompleted,
    TurnCompleted {
        reason: TurnCompletionReason,
    },
    ErrorNoted {
        message: String,
        fatal: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnCompletionReason {
    Natural,
    Cancelled,
    Rejected,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    ContentFilter,
    Other(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEvent {
    pub seq: u64,
    pub id: Uuid,
    pub session_id: Uuid,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatch {
    pub events: Vec<SessionEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_user_message_with_snake_case_tag() {
        let event = SessionEvent {
            seq: 1,
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "hello".into(),
            },
            metadata: BTreeMap::new(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["data"]["content"], "hello");
    }

    #[test]
    fn round_trips_assistant_message() {
        let event = SessionEvent {
            seq: 2,
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            kind: EventKind::AssistantMessage {
                id: Uuid::new_v4(),
                content: "hi".into(),
                stop_reason: None,
                usage: Some(Usage {
                    input_tokens: Some(1),
                    output_tokens: Some(2),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                }),
            },
            metadata: BTreeMap::new(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn old_skill_catalog_events_decode_as_first_publications() {
        let event: SessionEvent = serde_json::from_str(
            r#"{
                "seq": 3,
                "id": "0f6a7cd1-58a7-4f8f-9f16-c9fdd73dbbb0",
                "session_id": "6c1d65e3-51bf-48b7-a1a7-f6f2c34cd656",
                "timestamp": "2026-08-14T00:00:00Z",
                "type": "skill_catalog_published",
                "data": {
                    "entries": [{"name": "old-skill", "description": "Old skill"}],
                    "content": "old catalog"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            event.kind,
            EventKind::SkillCatalogPublished {
                entries: vec![SkillCatalogEntry {
                    name: "old-skill".into(),
                    description: "Old skill".into(),
                }],
                content: "old catalog".into(),
                update: false,
            }
        );
    }
}
