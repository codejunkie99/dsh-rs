use crate::events::EventKind;
use crate::session::{SessionLog, SessionView, SharedSessionLog};
use anyhow::Result;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub model: Option<String>,
    pub event_count: u64,
    pub turn_active: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default)]
pub struct SessionStore {
    root: PathBuf,
    logs: RwLock<Vec<SharedSessionLog>>,
    load_errors: RwLock<Vec<String>>,
}

impl SessionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let mut logs = Vec::new();
        let mut load_errors = Vec::new();

        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            match SessionLog::open(&path) {
                Ok(log) => logs.push(std::sync::Arc::new(log)),
                Err(error) => load_errors.push(format!("{}: {error}", path.display())),
            }
        }

        logs.sort_by_key(|log| std::cmp::Reverse(log.view().updated_at));
        Ok(Self {
            root,
            logs: RwLock::new(logs),
            load_errors: RwLock::new(load_errors),
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
        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.jsonl"));
        let log = SharedSessionLog::new(SessionLog::with_path(id, Some(path)));
        log.append(EventKind::SessionStarted {
            title: Some(title.into()),
            model,
        })?;
        self.logs.write().push(log.clone());
        Ok(log)
    }

    fn summary(view: &SessionView) -> SessionSummary {
        SessionSummary {
            id: view.id,
            title: view.title.clone(),
            model: view.model.clone(),
            event_count: view.event_count,
            turn_active: view.turn_active,
            updated_at: view.updated_at,
        }
    }
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
            })
            .unwrap();
        let corrupt_id = Uuid::new_v4();
        std::fs::write(dir.path().join(format!("{corrupt_id}.jsonl")), "not json").unwrap();

        let store = SessionStore::open(dir.path()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.load_errors().len(), 1);
        assert!(store.load_errors()[0].contains(&corrupt_id.to_string()));
    }
}
