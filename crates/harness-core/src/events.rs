use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted {
        title: Option<String>,
        model: Option<String>,
    },
    SessionTitleChanged {
        title: String,
    },
    TurnStarted,
    StepStarted {
        index: u64,
    },
    UserMessage {
        id: Uuid,
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
}
