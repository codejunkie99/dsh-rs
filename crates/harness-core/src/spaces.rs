use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Space {
    id: String,
    name: String,
    root: PathBuf,
}

impl Space {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let space = Self {
            id: id.into(),
            name: name.into(),
            root: root.into(),
        };
        space.validate()?;
        Ok(space)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("space id cannot be empty");
        }
        if self.name.trim().is_empty() {
            bail!("space name cannot be empty");
        }
        if !self.root.is_absolute() {
            bail!("space root must be absolute: {}", self.root.display());
        }
        let metadata = std::fs::metadata(&self.root)
            .with_context(|| format!("space root is unavailable: {}", self.root.display()))?;
        if !metadata.is_dir() {
            bail!("space root is not a directory: {}", self.root.display());
        }
        self.root.canonicalize().with_context(|| {
            format!("failed to canonicalize space root {}", self.root.display())
        })?;
        Ok(())
    }

    fn canonicalize_root(&mut self) -> Result<()> {
        self.validate()?;
        self.root = self.root.canonicalize().with_context(|| {
            format!("failed to canonicalize space root {}", self.root.display())
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacesConfig {
    spaces: Vec<Space>,
}

impl SpacesConfig {
    pub fn local(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = std::fs::create_dir_all(&root);
        let root = root.canonicalize().unwrap_or(root);
        Self {
            spaces: vec![Space {
                id: "local".into(),
                name: "Local harness".into(),
                root,
            }],
        }
    }

    pub fn spaces(&self) -> &[Space] {
        &self.spaces
    }

    pub fn get(&self, id: &str) -> Option<&Space> {
        self.spaces.iter().find(|space| space.id == id)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(raw).context("invalid spaces config")?;
        config.validate()?;
        config.canonicalized()
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read spaces config {}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let config = self.clone().canonicalized()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create spaces config parent {}", parent.display())
            })?;
        }
        let raw = serde_json::to_string_pretty(&config)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("spaces.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("failed to publish spaces config {}", path.display()))?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.spaces.is_empty() {
            bail!("spaces config must contain at least one space");
        }

        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for space in &self.spaces {
            space.validate()?;
            if !ids.insert(space.id.trim().to_string()) {
                bail!("duplicate space id: {}", space.id);
            }
            if !names.insert(space.name.trim().to_lowercase()) {
                bail!("duplicate space name: {}", space.name);
            }
        }
        Ok(())
    }

    fn canonicalized(mut self) -> Result<Self> {
        self.validate()?;
        for space in &mut self.spaces {
            space.canonicalize_root()?;
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonicalizes_and_looks_up_spaces() {
        let local = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let raw = format!(
            r#"{{
                "spaces": [
                    {{"id":"local","name":"Local harness","root":{:?}}},
                    {{"id":"project","name":"Project","root":{:?}}}
                ]
            }}"#,
            local.path(),
            project.path()
        );

        let config = SpacesConfig::parse(&raw).unwrap();
        assert_eq!(config.spaces().len(), 2);
        assert_eq!(
            config.get("project").unwrap().root(),
            project.path().canonicalize().unwrap()
        );
        assert_eq!(config.get("local").unwrap().name(), "Local harness");
        assert!(config.get("missing").is_none());
    }

    #[test]
    fn spaces_config_rejects_invalid_and_ambiguous_entries() {
        let local = tempfile::tempdir().unwrap();
        let valid = format!(
            r#"{{"spaces":[{{"id":"local","name":"Local","root":{:?}}}]}}"#,
            local.path()
        );
        assert!(SpacesConfig::parse(&valid).is_ok());
        assert!(SpacesConfig::parse(r#"{"spaces":[]}"#).is_err());
        assert!(SpacesConfig::parse(&valid.replace("\"local\"", "\"\"")).is_err());
        assert!(
            SpacesConfig::parse(r#"{"spaces":[{"id":"x","name":"X","root":"relative"}]}"#).is_err()
        );
        assert!(SpacesConfig::parse(&format!(
            r#"{{"spaces":[{{"id":"x","name":"X","root":{:?}}},{{"id":"x","name":"Y","root":{:?}}}]}}"#,
            local.path(),
            local.path()
        ))
        .is_err());
        assert!(SpacesConfig::parse(&format!(
            r#"{{"spaces":[{{"id":"x","name":"X","root":{:?}}}],\"extra\":true}}"#,
            local.path()
        ))
        .is_err());
    }

    #[test]
    fn local_space_defaults_and_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = dir.path().join("spaces.json");

        let config = SpacesConfig::local(&workspace);
        config.save(&path).unwrap();
        assert_eq!(SpacesConfig::load(&path).unwrap(), config);
        assert_eq!(config.spaces()[0].id(), "local");
        assert_eq!(config.spaces()[0].name(), "Local harness");
        assert_eq!(config.spaces()[0].root(), workspace.canonicalize().unwrap());
    }
}
