use crate::events::Usage;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "role")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSchema>,
    pub model: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum StreamFrame {
    Delta {
        message_id: Uuid,
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Done {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    Error {
        message: String,
    },
}

#[async_trait]
pub trait LlmAdapter: Send + Sync {
    async fn stream(&self, request: LlmRequest) -> anyhow::Result<mpsc::Receiver<StreamFrame>>;
}

#[derive(Debug, Clone, Default)]
pub struct NullAdapter;

#[async_trait]
impl LlmAdapter for NullAdapter {
    async fn stream(&self, _request: LlmRequest) -> anyhow::Result<mpsc::Receiver<StreamFrame>> {
        let (tx, rx) = mpsc::channel(16);
        let message_id = Uuid::new_v4();
        tokio::spawn(async move {
            let _ = tx
                .send(StreamFrame::Delta {
                    message_id,
                    text: "No model adapter configured.".into(),
                })
                .await;
            let _ = tx
                .send(StreamFrame::Done {
                    stop_reason: Some("stop".into()),
                    usage: None,
                })
                .await;
        });
        Ok(rx)
    }
}
