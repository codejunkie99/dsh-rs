use crate::events::EventKind;
use crate::preset::{PresetRoster, StandingProviderFactory};
use crate::session::{SessionLog, SessionView, SharedSessionLog};
use crate::skills::SkillScope;
use anyhow::Result;
use parking_lot::RwLock;
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub model: Option<String>,
    pub space_id: Option<String>,
    pub harness_id: Option<String>,
    pub event_count: u64,
    pub turn_active: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    Title,
    Transcript,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub session_id: Uuid,
    pub title: String,
    pub match_field: SearchField,
}

#[derive(Debug, Default)]
pub struct SessionStore {
    root: PathBuf,
    logs: RwLock<Vec<SharedSessionLog>>,
    load_errors: RwLock<Vec<String>>,
    roster: RwLock<PresetRoster>,
}

impl SessionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_default_preset(root, "standard")
    }

    pub fn open_with_default_preset(
        root: impl Into<PathBuf>,
        default_preset_id: &str,
    ) -> Result<Self> {
        Self::open_with_default_preset_and_provider(root, default_preset_id, None)
    }

    /// Open a session store whose preset roster mounts a per-standing-scope
    /// provider (for example, the filesystem skill provider) whenever a preset
    /// is first composed. The factory is installed before any session joins,
    /// so existing sessions loaded during open receive the provider on reopen.
    pub fn open_with_default_preset_and_provider(
        root: impl Into<PathBuf>,
        default_preset_id: &str,
        provider_factory: Option<StandingProviderFactory>,
    ) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let roster = RwLock::new(match provider_factory {
            Some(factory) => {
                PresetRoster::with_standing_provider_factory(default_preset_id.to_string(), factory)
            }
            None => PresetRoster::new(default_preset_id.to_string()),
        });
        let mut logs = Vec::new();
        let mut load_errors = Vec::new();

        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            match SessionLog::open(&path) {
                Ok(log) => {
                    let log = std::sync::Arc::new(log);
                    let harness_id = log.view().harness_id.clone();
                    if let Err(error) = join_preset(&log, &roster, harness_id.as_deref()) {
                        load_errors.push(format!(
                            "{}: could not join preset scope: {error}",
                            path.display()
                        ));
                    }
                    logs.push(log);
                }
                Err(error) => load_errors.push(format!("{}: {error}", path.display())),
            }
        }

        logs.sort_by_key(|log| std::cmp::Reverse(log.view().updated_at));
        Ok(Self {
            root,
            logs: RwLock::new(logs),
            load_errors: RwLock::new(load_errors),
            roster,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_errors(&self) -> Vec<String> {
        self.load_errors.read().clone()
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.logs
            .read()
            .iter()
            .map(|log| Self::summary(&log.view()))
            .collect()
    }

    pub fn get(&self, id: Uuid) -> Option<SharedSessionLog> {
        self.logs.read().iter().find(|log| log.id() == id).cloned()
    }

    pub fn create(
        &self,
        title: impl Into<String>,
        model: Option<String>,
    ) -> Result<SharedSessionLog> {
        self.create_in_space(title, model, None)
    }

    pub fn create_in_space(
        &self,
        title: impl Into<String>,
        model: Option<String>,
        space_id: Option<String>,
    ) -> Result<SharedSessionLog> {
        self.create_in_space_and_harness(title, model, space_id, None)
    }

    pub fn create_in_space_and_harness(
        &self,
        title: impl Into<String>,
        model: Option<String>,
        space_id: Option<String>,
        harness_id: Option<String>,
    ) -> Result<SharedSessionLog> {
        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.jsonl"));
        let log = SharedSessionLog::new(SessionLog::with_path(id, Some(path)));
        log.append(EventKind::SessionStarted {
            title: Some(title.into()),
            model,
            space_id,
            harness_id: harness_id.clone(),
        })?;
        join_preset(&log, &self.roster, harness_id.as_deref())?;
        self.logs.write().push(log.clone());
        Ok(log)
    }

    pub fn rename(&self, id: Uuid, title: impl Into<String>) -> Result<SharedSessionLog> {
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session {id} does not exist"))?;
        log.set_title(title)?;
        Ok(log)
    }

    pub fn set_space(&self, id: Uuid, space_id: impl Into<String>) -> Result<SharedSessionLog> {
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session {id} does not exist"))?;
        log.set_space(space_id)?;
        Ok(log)
    }

    pub fn set_harness(&self, id: Uuid, harness_id: impl Into<String>) -> Result<SharedSessionLog> {
        let harness_id = harness_id.into();
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session {id} does not exist"))?;
        log.set_harness(harness_id.as_str())?;
        rejoin_preset(&log, &self.roster, &harness_id)?;
        Ok(log)
    }

    pub fn fork(
        &self,
        source_id: Uuid,
        boundary_seq: Option<u64>,
        title: impl Into<String>,
    ) -> Result<SharedSessionLog> {
        let source = self
            .get(source_id)
            .ok_or_else(|| anyhow::anyhow!("source session {source_id} does not exist"))?;
        let events = source.events();
        let boundary =
            boundary_seq.unwrap_or_else(|| events.last().map(|event| event.seq).unwrap_or(0));
        if boundary == 0 {
            anyhow::bail!("cannot fork an empty session");
        }
        if !events.iter().any(|event| event.seq == boundary) {
            anyhow::bail!("fork boundary {boundary} does not exist");
        }

        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.jsonl"));
        let fork = SharedSessionLog::new(SessionLog::with_path(id, Some(path)));
        for event in events.iter().take_while(|event| event.seq <= boundary) {
            fork.append_with_metadata(event.kind.clone(), event.metadata.clone())?;
        }
        fork.set_title(title)?;
        let harness_id = fork.view().harness_id.clone();
        join_preset(&fork, &self.roster, harness_id.as_deref())?;
        self.logs.write().push(fork.clone());
        Ok(fork)
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let mut results = Vec::new();
        for log in self.logs.read().iter() {
            let view = log.view();
            if view.title.to_lowercase().contains(&query) {
                results.push(SearchResult {
                    session_id: view.id,
                    title: view.title.clone(),
                    match_field: SearchField::Title,
                });
                continue;
            }
            if view.transcript.iter().any(|entry| match entry {
                crate::session::TranscriptEntry::User { content, .. }
                | crate::session::TranscriptEntry::Assistant { content, .. }
                | crate::session::TranscriptEntry::System { message: content } => {
                    content.to_lowercase().contains(&query)
                }
                crate::session::TranscriptEntry::ToolCall { name, .. } => {
                    name.to_lowercase().contains(&query)
                }
                crate::session::TranscriptEntry::ToolResult { output, .. } => {
                    output.to_lowercase().contains(&query)
                }
            }) {
                results.push(SearchResult {
                    session_id: view.id,
                    title: view.title.clone(),
                    match_field: SearchField::Transcript,
                });
            }
        }
        results
    }

    pub fn export_markdown(&self, id: Uuid) -> Result<String> {
        let log = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("session {id} does not exist"))?;
        let view = log.view();
        let mut markdown = format!(
            "# {}\n\nModel: `{}`\n\n",
            view.title,
            view.model.as_deref().unwrap_or("unknown")
        );
        for entry in &view.transcript {
            match entry {
                crate::session::TranscriptEntry::User { content, .. } => {
                    markdown.push_str(&format!("## User\n\n{content}\n\n"));
                }
                crate::session::TranscriptEntry::Assistant { content, .. } => {
                    markdown.push_str(&format!("## Assistant\n\n{content}\n\n"));
                }
                crate::session::TranscriptEntry::ToolCall {
                    name, arguments, ..
                } => {
                    markdown.push_str(&format!("## Tool Call\n\n`{name}` `{arguments}`\n\n"));
                }
                crate::session::TranscriptEntry::ToolResult { output, .. } => {
                    markdown.push_str(&format!("## Tool Result\n\n{output}\n\n"));
                }
                crate::session::TranscriptEntry::System { message } => {
                    markdown.push_str(&format!("## System\n\n{message}\n\n"));
                }
            }
        }
        Ok(markdown)
    }

    fn summary(view: &SessionView) -> SessionSummary {
        SessionSummary {
            id: view.id,
            title: view.title.clone(),
            model: view.model.clone(),
            space_id: view.space_id.clone(),
            harness_id: view.harness_id.clone(),
            event_count: view.event_count,
            turn_active: view.turn_active,
            updated_at: view.updated_at,
        }
    }
}

/// Assign a fresh session scope parented to `harness_id`'s standing key (a
/// first bind). Falls back to the roster default when the session names none.
fn join_preset(
    log: &SharedSessionLog,
    roster: &RwLock<PresetRoster>,
    harness_id: Option<&str>,
) -> Result<()> {
    let scope = SkillScope::new();
    roster.write().mount(scope.clone(), harness_id)?;
    log.set_skill_scope(scope);
    Ok(())
}

/// Re-link an already-joined session scope to a different preset's standing
/// key, or first-bind it when no scope was assigned yet.
fn rejoin_preset(
    log: &SharedSessionLog,
    roster: &RwLock<PresetRoster>,
    harness_id: &str,
) -> Result<()> {
    let mut roster = roster.write();
    match log.skill_scope() {
        Some(scope) => roster.recompose(scope, harness_id)?,
        None => {
            let scope = SkillScope::new();
            roster.mount(scope.clone(), Some(harness_id))?;
            log.set_skill_scope(scope);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_lists_and_reopens_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let log = store.create("First", Some("mock".into())).unwrap();
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: "hello".into(),
        })
        .unwrap();

        let summaries = store.list();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "First");
        assert_eq!(summaries[0].event_count, 2);

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].event_count, 2);
        assert!(reopened.get(summaries[0].id).is_some());
    }

    #[test]
    fn skips_corrupt_files_and_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        let valid_id = Uuid::new_v4();
        let valid_path = dir.path().join(format!("{valid_id}.jsonl"));
        SessionLog::with_path(valid_id, Some(valid_path))
            .append(EventKind::SessionStarted {
                title: Some("Valid".into()),
                model: None,
                space_id: None,
                harness_id: None,
            })
            .unwrap();
        let corrupt_id = Uuid::new_v4();
        std::fs::write(dir.path().join(format!("{corrupt_id}.jsonl")), "not json").unwrap();

        let store = SessionStore::open(dir.path()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.load_errors().len(), 1);
        assert!(store.load_errors()[0].contains(&corrupt_id.to_string()));
    }

    #[test]
    fn renames_forks_at_boundary_and_reopens_the_fork() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let source = store.create("Source", Some("local-null".into())).unwrap();
        source
            .append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "before fork".into(),
            })
            .unwrap();
        source.set_title("Renamed source").unwrap();
        source
            .append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "after boundary".into(),
            })
            .unwrap();
        let boundary = source.events()[2].seq;

        let fork = store.fork(source.id(), Some(boundary), "My fork").unwrap();
        assert_eq!(source.view().title, "Renamed source");
        assert_eq!(fork.view().title, "My fork");
        assert_eq!(fork.len(), 4);
        assert_eq!(fork.view().transcript.len(), 1);
        assert!(matches!(
            &fork.view().transcript[0],
            crate::session::TranscriptEntry::User { content, .. } if content == "before fork"
        ));

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert_eq!(reopened.list().len(), 2);
        assert!(reopened
            .list()
            .iter()
            .any(|summary| summary.title == "My fork"));
    }

    #[test]
    fn spaces_are_listed_changed_and_preserved_by_forks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let source = store
            .create_in_space("Source", Some("local-null".into()), Some("local".into()))
            .unwrap();
        assert_eq!(source.view().space_id.as_deref(), Some("local"));
        assert_eq!(store.list()[0].space_id.as_deref(), Some("local"));

        store.set_space(source.id(), "research").unwrap();
        let fork = store.fork(source.id(), None, "Fork").unwrap();
        assert_eq!(fork.view().space_id.as_deref(), Some("research"));

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert!(reopened
            .list()
            .iter()
            .all(|summary| { summary.space_id.as_deref() == Some("research") }));
    }

    #[test]
    fn harness_setups_are_listed_changed_and_preserved_by_forks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let source = store
            .create_in_space_and_harness(
                "Source",
                Some("local-null".into()),
                Some("local".into()),
                Some("standard".into()),
            )
            .unwrap();
        assert_eq!(source.view().harness_id.as_deref(), Some("standard"));
        assert_eq!(store.list()[0].harness_id.as_deref(), Some("standard"));

        store.set_harness(source.id(), "research").unwrap();
        let fork = store.fork(source.id(), None, "Fork").unwrap();
        assert_eq!(fork.view().harness_id.as_deref(), Some("research"));

        let reopened = SessionStore::open(dir.path()).unwrap();
        assert!(reopened
            .list()
            .iter()
            .all(|summary| summary.harness_id.as_deref() == Some("research")));
    }

    #[test]
    fn searches_titles_and_transcripts_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let first = store.create("Release planning", None).unwrap();
        first
            .append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "Ship the GPUI app".into(),
            })
            .unwrap();
        let second = store.create("Unrelated", None).unwrap();
        second
            .append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "Different topic".into(),
            })
            .unwrap();

        let results = store.search("gpui");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, first.id());
        assert_eq!(results[0].title, "Release planning");
        assert_eq!(results[0].match_field, SearchField::Transcript);

        let results = store.search("release");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_field, SearchField::Title);
    }

    #[test]
    fn exports_a_session_as_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let log = store
            .create("Export session", Some("local-null".into()))
            .unwrap();
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: "hello export".into(),
        })
        .unwrap();
        log.append(EventKind::AssistantMessage {
            id: Uuid::new_v4(),
            content: "exported".into(),
            stop_reason: None,
            usage: None,
        })
        .unwrap();

        let markdown = store.export_markdown(log.id()).unwrap();
        assert!(markdown.starts_with("# Export session"));
        assert!(markdown.contains("Model: `local-null`"));
        assert!(markdown.contains("## User"));
        assert!(markdown.contains("hello export"));
        assert!(markdown.contains("## Assistant"));
        assert!(markdown.contains("exported"));
    }

    #[test]
    fn sessions_on_the_same_preset_share_the_standing_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let a = store
            .create_in_space_and_harness("A", None, None, Some("standard".into()))
            .unwrap();
        let b = store
            .create_in_space_and_harness("B", None, None, Some("standard".into()))
            .unwrap();
        let c = store
            .create_in_space_and_harness("C", None, None, Some("research".into()))
            .unwrap();

        assert_eq!(
            a.skill_scope().unwrap().parent(),
            b.skill_scope().unwrap().parent()
        );
        assert_ne!(
            a.skill_scope().unwrap().parent(),
            c.skill_scope().unwrap().parent()
        );
    }

    #[test]
    fn set_harness_relinks_the_session_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store
            .create_in_space_and_harness("A", None, None, Some("standard".into()))
            .unwrap();
        let standard_parent = session.skill_scope().unwrap().parent().unwrap();

        store.set_harness(session.id(), "research").unwrap();

        let research_parent = session.skill_scope().unwrap().parent().unwrap();
        assert_ne!(standard_parent, research_parent);
        assert_eq!(session.view().harness_id.as_deref(), Some("research"));
    }

    #[test]
    fn reopened_sessions_remount_their_recorded_preset_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let standard = store
            .create_in_space_and_harness("A", None, None, Some("standard".into()))
            .unwrap();
        let research = store
            .create_in_space_and_harness("B", None, None, Some("research".into()))
            .unwrap();
        drop(store);

        let reopened = SessionStore::open(dir.path()).unwrap();
        let reopened_standard = reopened.get(standard.id()).unwrap();
        let reopened_research = reopened.get(research.id()).unwrap();
        let fresh_standard = reopened
            .create_in_space_and_harness("C", None, None, Some("standard".into()))
            .unwrap();
        let fresh_research = reopened
            .create_in_space_and_harness("D", None, None, Some("research".into()))
            .unwrap();

        assert_eq!(
            reopened_standard.skill_scope().unwrap().parent(),
            fresh_standard.skill_scope().unwrap().parent()
        );
        assert_eq!(
            reopened_research.skill_scope().unwrap().parent(),
            fresh_research.skill_scope().unwrap().parent()
        );
        assert_ne!(
            reopened_standard.skill_scope().unwrap().parent(),
            reopened_research.skill_scope().unwrap().parent()
        );
    }

    #[test]
    fn forks_join_the_source_preset_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let source = store
            .create_in_space_and_harness("Source", None, None, Some("research".into()))
            .unwrap();
        source
            .append(EventKind::UserMessage {
                id: Uuid::new_v4(),
                content: "hello".into(),
            })
            .unwrap();

        let fork = store.fork(source.id(), None, "Fork").unwrap();

        assert_eq!(fork.view().harness_id.as_deref(), Some("research"));
        assert_eq!(
            fork.skill_scope().unwrap().parent(),
            source.skill_scope().unwrap().parent()
        );
    }
}
