use crate::events::{EventKind, SessionEvent, TurnCompletionReason};
use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::RwLock;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "role")]
pub enum TranscriptEntry {
    User {
        id: Uuid,
        content: String,
    },
    Assistant {
        id: Uuid,
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        ok: bool,
        output: String,
    },
    System {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub id: Uuid,
    pub title: String,
    pub model: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub event_count: u64,
    pub transcript: Vec<TranscriptEntry>,
    pub turn_active: bool,
    pub step_active: bool,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct SessionLog {
    id: Uuid,
    path: Option<PathBuf>,
    inner: RwLock<SessionInner>,
}

#[derive(Debug)]
struct SessionInner {
    events: Vec<SessionEvent>,
    title: String,
    model: Option<String>,
    turn_active: bool,
    step_active: bool,
    last_error: Option<String>,
}

impl SessionLog {
    pub fn in_memory(session_id: Uuid) -> Self {
        Self::with_path(session_id, None)
    }

    pub fn with_path(session_id: Uuid, path: Option<PathBuf>) -> Self {
        Self {
            id: session_id,
            path,
            inner: RwLock::new(SessionInner {
                events: Vec::new(),
                title: "New session".into(),
                model: None,
                turn_active: false,
                step_active: false,
                last_error: None,
            }),
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        let log = Self::with_path(session_id, Some(path.clone()));
        if path.exists() {
            log.replay_from_disk()?;
        }
        Ok(log)
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn len(&self) -> usize {
        self.inner.read().events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn events(&self) -> Vec<SessionEvent> {
        self.inner.read().events.clone()
    }

    pub fn events_after(&self, seq: u64) -> Vec<SessionEvent> {
        let inner = self.inner.read();
        inner
            .events
            .iter()
            .filter(|e| e.seq > seq)
            .cloned()
            .collect()
    }

    pub fn append(&self, kind: EventKind) -> Result<SessionEvent> {
        self.append_with_metadata(kind, Default::default())
    }

    pub fn append_with_metadata(
        &self,
        kind: EventKind,
        metadata: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<SessionEvent> {
        let event = {
            let mut inner = self.inner.write();
            let seq = inner.events.last().map(|e| e.seq + 1).unwrap_or(1);
            let event = SessionEvent {
                seq,
                id: Uuid::new_v4(),
                session_id: self.id,
                timestamp: Utc::now(),
                kind,
                metadata,
            };
            Self::apply(&mut inner, &event);
            inner.events.push(event.clone());
            event
        };
        self.persist_append(&event)?;
        Ok(event)
    }

    pub fn view(&self) -> SessionView {
        let inner = self.inner.read();
        SessionView {
            id: self.id,
            title: inner.title.clone(),
            model: inner.model.clone(),
            created_at: inner
                .events
                .first()
                .map(|e| e.timestamp)
                .unwrap_or_else(Utc::now),
            updated_at: inner
                .events
                .last()
                .map(|e| e.timestamp)
                .unwrap_or_else(Utc::now),
            event_count: inner.events.len() as u64,
            transcript: Self::derive_transcript(&inner.events),
            turn_active: inner.turn_active,
            step_active: inner.step_active,
            last_error: inner.last_error.clone(),
        }
    }

    fn derive_transcript(events: &[SessionEvent]) -> Vec<TranscriptEntry> {
        let mut out = Vec::new();
        for event in events {
            match &event.kind {
                EventKind::UserMessage { id, content } => out.push(TranscriptEntry::User {
                    id: *id,
                    content: content.clone(),
                }),
                EventKind::AssistantMessage { id, content, .. } => {
                    out.push(TranscriptEntry::Assistant {
                        id: *id,
                        content: content.clone(),
                    })
                }
                EventKind::ToolCall {
                    id,
                    name,
                    arguments,
                } => out.push(TranscriptEntry::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                EventKind::ApprovalRequested {
                    call_id, tool_name, ..
                } => out.push(TranscriptEntry::System {
                    message: format!("Approval requested for {tool_name} ({call_id})"),
                }),
                EventKind::ApprovalResolved {
                    call_id,
                    approved,
                    reason,
                } => out.push(TranscriptEntry::System {
                    message: format!(
                        "Approval {call_id} {}: {reason}",
                        if *approved { "approved" } else { "denied" }
                    ),
                }),
                EventKind::ToolResult {
                    call_id,
                    ok,
                    output,
                } => out.push(TranscriptEntry::ToolResult {
                    call_id: call_id.clone(),
                    ok: *ok,
                    output: output.clone(),
                }),
                EventKind::ErrorNoted { message, .. } => out.push(TranscriptEntry::System {
                    message: message.clone(),
                }),
                _ => {}
            }
        }
        out
    }

    fn apply(inner: &mut SessionInner, event: &SessionEvent) {
        match &event.kind {
            EventKind::SessionStarted { title, model } => {
                if let Some(title) = title {
                    inner.title = title.clone();
                }
                inner.model = model.clone();
            }
            EventKind::SessionTitleChanged { title } => inner.title = title.clone(),
            EventKind::TurnStarted => inner.turn_active = true,
            EventKind::TurnCompleted { reason } => {
                inner.turn_active = false;
                inner.step_active = false;
                if *reason == TurnCompletionReason::Errored {
                    // last_error is set by ErrorNoted; nothing extra here.
                }
            }
            EventKind::StepStarted { .. } => inner.step_active = true,
            EventKind::StepCompleted => inner.step_active = false,
            EventKind::ErrorNoted { message, .. } => inner.last_error = Some(message.clone()),
            _ => {}
        }
    }

    fn replay_from_disk(&self) -> Result<()> {
        let raw = std::fs::read_to_string(
            self.path
                .as_ref()
                .context("session path required for replay")?,
        )
        .with_context(|| format!("failed to read session log {:?}", self.path))?;
        let mut inner = self.inner.write();
        inner.events.clear();
        for line in raw.lines().filter(|line| !line.trim().is_empty()) {
            let event: SessionEvent =
                serde_json::from_str(line).context("failed to decode session event")?;
            Self::apply(&mut inner, &event);
            inner.events.push(event);
        }
        Ok(())
    }

    fn persist_append(&self, event: &SessionEvent) -> Result<()> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let line = serde_json::to_string(event)?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

pub type SharedSessionLog = Arc<SessionLog>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_derives_view() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        log.append(EventKind::SessionStarted {
            title: Some("Test".into()),
            model: Some("test-model".into()),
        })
        .unwrap();
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: "hello".into(),
        })
        .unwrap();
        log.append(EventKind::AssistantMessage {
            id: Uuid::new_v4(),
            content: "world".into(),
            stop_reason: None,
            usage: None,
        })
        .unwrap();

        let view = log.view();
        assert_eq!(view.title, "Test");
        assert_eq!(view.event_count, 3);
        assert_eq!(view.transcript.len(), 2);
    }

    #[test]
    fn persists_and_replays_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("{id}.jsonl"));
        let log = SessionLog::with_path(id, Some(path.clone()));
        log.append(EventKind::SessionStarted {
            title: Some("Persisted".into()),
            model: None,
        })
        .unwrap();
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: "persist me".into(),
        })
        .unwrap();
        drop(log);

        let reopened = SessionLog::open(path).unwrap();
        assert_eq!(reopened.view().title, "Persisted");
        assert_eq!(reopened.len(), 2);
    }

    #[test]
    fn events_after_filters_by_sequence() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        for i in 0..5 {
            log.append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: format!("m{i}"),
            })
            .unwrap();
        }
        assert_eq!(log.events_after(3).len(), 2);
        assert_eq!(log.events_after(3)[0].seq, 4);
    }
}
