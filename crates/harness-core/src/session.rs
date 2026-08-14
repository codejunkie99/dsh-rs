use crate::events::{
    validate_todo_snapshot, EventKind, SessionEvent, TodoItem, TurnCompletionReason,
};
use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::RwLock;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;
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
    pub space_id: Option<String>,
    pub harness_id: Option<String>,
    pub system_prompt: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub event_count: u64,
    pub transcript: Vec<TranscriptEntry>,
    pub turn_active: bool,
    pub step_active: bool,
    pub last_error: Option<String>,
    pub todos: Option<Vec<TodoItem>>,
}

#[derive(Debug)]
pub struct SessionLog {
    id: Uuid,
    path: Option<PathBuf>,
    inner: RwLock<SessionInner>,
    sequence_tx: watch::Sender<u64>,
}

#[derive(Debug)]
struct SessionInner {
    events: Vec<SessionEvent>,
    title: String,
    model: Option<String>,
    space_id: Option<String>,
    harness_id: Option<String>,
    system_prompt: Option<String>,
    turn_active: bool,
    step_active: bool,
    last_error: Option<String>,
    todos: Option<Vec<TodoItem>>,
}

impl SessionLog {
    pub fn in_memory(session_id: Uuid) -> Self {
        Self::with_path(session_id, None)
    }

    pub fn with_path(session_id: Uuid, path: Option<PathBuf>) -> Self {
        Self {
            id: session_id,
            path,
            sequence_tx: watch::Sender::new(0),
            inner: RwLock::new(SessionInner {
                events: Vec::new(),
                title: "New session".into(),
                model: None,
                space_id: None,
                harness_id: None,
                system_prompt: None,
                turn_active: false,
                step_active: false,
                last_error: None,
                todos: None,
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
            let latest = log
                .inner
                .read()
                .events
                .last()
                .map(|event| event.seq)
                .unwrap_or(0);
            log.sequence_tx.send(latest).ok();
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

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.sequence_tx.subscribe()
    }

    pub fn append(&self, kind: EventKind) -> Result<SessionEvent> {
        self.append_with_metadata(kind, Default::default())
    }

    pub fn set_title(&self, title: impl Into<String>) -> Result<SessionEvent> {
        let title = title.into();
        if self.view().title == title {
            return Ok(self
                .events()
                .last()
                .cloned()
                .expect("session logs always contain at least one event"));
        }
        self.append(EventKind::SessionTitleChanged { title })
    }

    pub fn set_model(&self, model: impl Into<String>) -> Result<SessionEvent> {
        let model = model.into();
        if self.view().model.as_deref() == Some(model.as_str()) {
            return Ok(self
                .events()
                .last()
                .cloned()
                .expect("session logs always contain at least one event"));
        }
        self.append(EventKind::SessionModelChanged { model })
    }

    pub fn set_space(&self, space_id: impl Into<String>) -> Result<SessionEvent> {
        let space_id = space_id.into();
        if self.view().space_id.as_deref() == Some(space_id.as_str()) {
            return Ok(self
                .events()
                .last()
                .cloned()
                .expect("session logs always contain at least one event"));
        }
        self.append(EventKind::SessionSpaceChanged { space_id })
    }

    pub fn set_harness(&self, harness_id: impl Into<String>) -> Result<SessionEvent> {
        let harness_id = harness_id.into();
        if self.view().harness_id.as_deref() == Some(harness_id.as_str()) {
            return Ok(self
                .events()
                .last()
                .cloned()
                .expect("session logs always contain at least one event"));
        }
        self.append(EventKind::SessionHarnessChanged { harness_id })
    }

    pub fn append_with_metadata(
        &self,
        kind: EventKind,
        metadata: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<SessionEvent> {
        if let EventKind::TodoWrite { todos } = &kind {
            validate_todo_snapshot(todos)?;
        }
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
        self.sequence_tx.send(event.seq).ok();
        Ok(event)
    }

    pub fn view(&self) -> SessionView {
        let inner = self.inner.read();
        SessionView {
            id: self.id,
            title: inner.title.clone(),
            model: inner.model.clone(),
            space_id: inner.space_id.clone(),
            harness_id: inner.harness_id.clone(),
            system_prompt: inner.system_prompt.clone(),
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
            todos: inner.todos.clone(),
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
            EventKind::SessionStarted {
                title,
                model,
                space_id,
                harness_id,
            } => {
                if let Some(title) = title {
                    inner.title = title.clone();
                }
                inner.model = model.clone();
                if space_id.is_some() {
                    inner.space_id = space_id.clone();
                }
                if harness_id.is_some() {
                    inner.harness_id = harness_id.clone();
                }
            }
            EventKind::SessionTitleChanged { title } => inner.title = title.clone(),
            EventKind::SessionModelChanged { model } => inner.model = Some(model.clone()),
            EventKind::SessionSpaceChanged { space_id } => {
                inner.space_id = Some(space_id.clone());
            }
            EventKind::SessionHarnessChanged { harness_id } => {
                inner.harness_id = Some(harness_id.clone());
            }
            EventKind::SystemPromptSnapshot { content } => inner.system_prompt = content.clone(),
            EventKind::TurnStarted => {
                inner.turn_active = true;
                inner.todos = None;
            }
            EventKind::TodoWrite { todos } => inner.todos = Some(todos.clone()),
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
            if let EventKind::TodoWrite { todos } = &event.kind {
                validate_todo_snapshot(todos)?;
            }
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
            space_id: None,
            harness_id: None,
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
            space_id: None,
            harness_id: None,
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
    fn persists_and_replays_system_prompt_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("{id}.jsonl"));
        let log = SessionLog::with_path(id, Some(path.clone()));
        log.append(EventKind::SystemPromptSnapshot {
            content: Some("durable prompt".into()),
        })
        .unwrap();
        log.append(EventKind::SystemPromptSnapshot { content: None })
            .unwrap();
        drop(log);

        let reopened = SessionLog::open(path).unwrap();
        assert_eq!(reopened.view().system_prompt, None);
    }

    #[test]
    fn model_changes_are_durable_and_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("{id}.jsonl"));
        let log = SessionLog::with_path(id, Some(path.clone()));
        log.append(EventKind::SessionStarted {
            title: Some("Model test".into()),
            model: Some("local-null".into()),
            space_id: None,
            harness_id: None,
        })
        .unwrap();
        log.set_model("deepseek-chat").unwrap();
        drop(log);

        let reopened = SessionLog::open(path).unwrap();
        assert_eq!(reopened.view().model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn space_changes_are_durable_and_old_sessions_replay_without_a_space() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("{id}.jsonl"));
        let log = SessionLog::with_path(id, Some(path.clone()));
        log.append(EventKind::SessionStarted {
            title: Some("Space test".into()),
            model: Some("local-null".into()),
            space_id: Some("local".into()),
            harness_id: None,
        })
        .unwrap();
        log.append(EventKind::SessionSpaceChanged {
            space_id: "research".into(),
        })
        .unwrap();
        drop(log);

        let reopened = SessionLog::open(path).unwrap();
        assert_eq!(reopened.view().space_id.as_deref(), Some("research"));

        let legacy_id = Uuid::new_v4();
        let legacy_path = dir.path().join(format!("{legacy_id}.jsonl"));
        std::fs::write(
            &legacy_path,
            format!(
                r#"{{"seq":1,"id":"{}","session_id":"{}","timestamp":"2026-08-14T00:00:00Z","type":"session_started","data":{{"title":"Legacy","model":null}}}}"#,
                Uuid::new_v4(),
                legacy_id
            ),
        )
        .unwrap();
        let legacy = SessionLog::open(legacy_path).unwrap();
        assert_eq!(legacy.view().space_id, None);
    }

    #[test]
    fn harness_changes_are_durable_and_old_sessions_replay_without_a_setup() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("{id}.jsonl"));
        let log = SessionLog::with_path(id, Some(path.clone()));
        log.append(EventKind::SessionStarted {
            title: Some("Harness test".into()),
            model: Some("local-null".into()),
            space_id: Some("local".into()),
            harness_id: Some("standard".into()),
        })
        .unwrap();
        log.append(EventKind::SessionHarnessChanged {
            harness_id: "research".into(),
        })
        .unwrap();
        drop(log);

        let reopened = SessionLog::open(path).unwrap();
        assert_eq!(reopened.view().harness_id.as_deref(), Some("research"));

        let legacy_id = Uuid::new_v4();
        let legacy_path = dir.path().join(format!("{legacy_id}.jsonl"));
        std::fs::write(
            &legacy_path,
            format!(
                r#"{{"seq":1,"id":"{}","session_id":"{}","timestamp":"2026-08-14T00:00:00Z","type":"session_started","data":{{"title":"Legacy","model":null}}}}"#,
                Uuid::new_v4(),
                legacy_id
            ),
        )
        .unwrap();
        let legacy = SessionLog::open(legacy_path).unwrap();
        assert_eq!(legacy.view().harness_id, None);
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

    #[tokio::test]
    async fn append_notifies_subscribers_with_latest_sequence() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        let mut sequence = log.subscribe();

        log.append(EventKind::TurnStarted).unwrap();
        sequence.changed().await.unwrap();
        assert_eq!(*sequence.borrow(), 1);

        log.append(EventKind::TurnCompleted {
            reason: TurnCompletionReason::Natural,
        })
        .unwrap();
        sequence.changed().await.unwrap();
        assert_eq!(*sequence.borrow(), 2);
    }
}
