use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const BUNDLED_SKILL_RANK: u16 = 600;
const RUNTIME_SKILL_RANK: u16 = 250;
const PROJECT_DSH_RANK: u16 = 100;
const PROJECT_AGENTS_RANK: u16 = 200;
const CUSTOM_RANK: u16 = 300;
const USER_DSH_RANK: u16 = 400;
const USER_AGENTS_RANK: u16 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvocationPolicy {
    pub model_invocable: bool,
    pub user_invocable: bool,
}

impl Default for SkillInvocationPolicy {
    fn default() -> Self {
        Self {
            model_invocable: true,
            user_invocable: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillResourceBase {
    Directory { path: PathBuf },
    Url { url: String },
    Opaque { description: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
    pub rank: u16,
    pub locator: serde_json::Value,
    pub path: Option<PathBuf>,
    pub metadata: Option<serde_json::Value>,
}

impl SkillCandidate {
    fn summary(&self) -> SkillSummary {
        SkillSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            when_to_use: self.when_to_use.clone(),
            invocation: self.invocation,
            source: self.source.clone(),
            provider: self.provider.clone(),
            resource_base: self.resource_base.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
    pub path: Option<PathBuf>,
    pub metadata: Option<serde_json::Value>,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillLookupOptions {
    pub cwd: Option<PathBuf>,
}

#[async_trait]
pub trait SkillProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn list(&self, options: &SkillLookupOptions) -> Result<Vec<SkillCandidate>>;
    async fn get(
        &self,
        candidate: &SkillCandidate,
        options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>>;
}

#[derive(Debug, Clone)]
struct RuntimeRegistration {
    definition: SkillDefinition,
    order: usize,
}

pub struct SkillRegistry {
    providers: Vec<Arc<dyn SkillProvider>>,
    runtime: HashMap<String, RuntimeRegistration>,
    next_runtime_order: usize,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            runtime: HashMap::new(),
            next_runtime_order: 0,
        }
    }

    pub fn register_provider(&mut self, provider: Arc<dyn SkillProvider>) -> Result<()> {
        let name = provider.name();
        if name == "runtime" {
            bail!("skill provider named \"runtime\" is reserved");
        }
        if self
            .providers
            .iter()
            .any(|existing| existing.name() == name)
        {
            bail!("a skill provider named \"{name}\" is already registered");
        }
        self.providers.push(provider);
        Ok(())
    }

    pub fn register_runtime(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
        invocation: SkillInvocationPolicy,
    ) -> Result<()> {
        let name = name.into();
        let definition = SkillDefinition {
            name: name.clone(),
            description: description.into(),
            when_to_use: None,
            invocation,
            source: "runtime".into(),
            provider: "runtime".into(),
            resource_base: None,
            path: None,
            metadata: None,
            content: content.into(),
        };
        validate_definition(&definition)?;
        if self.runtime.contains_key(&name) {
            bail!("runtime skill \"{name}\" is already registered");
        }
        let order = self.next_runtime_order;
        self.next_runtime_order += 1;
        self.runtime
            .insert(name, RuntimeRegistration { definition, order });
        Ok(())
    }

    pub async fn list(&self, options: &SkillLookupOptions) -> Result<Vec<SkillSummary>> {
        let mut summaries: Vec<_> = self
            .collect(options)
            .await?
            .into_iter()
            .map(|candidate| candidate.summary())
            .collect();
        summaries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(summaries)
    }

    pub async fn get(
        &self,
        name: &str,
        options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>> {
        if !is_skill_name(name) {
            return Ok(None);
        }
        let Some(candidate) = self
            .collect(options)
            .await?
            .into_iter()
            .find(|candidate| candidate.name == name)
        else {
            return Ok(None);
        };

        if candidate.provider == "runtime" {
            return Ok(self
                .runtime
                .get(&candidate.name)
                .map(|registration| registration.definition.clone()));
        }

        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.name() == candidate.provider)
        else {
            return Ok(None);
        };
        let Some(definition) = provider.get(&candidate, options).await? else {
            return Ok(None);
        };
        validate_definition(&definition)?;
        if definition.name != candidate.name {
            return Ok(None);
        }
        Ok(Some(definition))
    }

    async fn collect(&self, options: &SkillLookupOptions) -> Result<Vec<SkillCandidate>> {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
        struct Indexed {
            rank: u16,
            provider_order: usize,
            local_order: usize,
        }

        let mut indexed: Vec<(Indexed, SkillCandidate)> = Vec::new();
        let mut runtime: Vec<_> = self.runtime.values().cloned().collect();
        runtime.sort_by(|left, right| {
            left.definition
                .name
                .cmp(&right.definition.name)
                .then(left.order.cmp(&right.order))
        });
        for (local_order, registration) in runtime.into_iter().enumerate() {
            let definition = registration.definition;
            let candidate = SkillCandidate {
                name: definition.name.clone(),
                description: definition.description.clone(),
                when_to_use: definition.when_to_use.clone(),
                invocation: definition.invocation,
                source: definition.source.clone(),
                provider: definition.provider.clone(),
                resource_base: definition.resource_base.clone(),
                rank: RUNTIME_SKILL_RANK,
                locator: serde_json::Value::Null,
                path: definition.path.clone(),
                metadata: definition.metadata.clone(),
            };
            indexed.push((
                Indexed {
                    rank: RUNTIME_SKILL_RANK,
                    provider_order: usize::MAX,
                    local_order,
                },
                candidate,
            ));
        }

        for (provider_order, provider) in self.providers.iter().enumerate() {
            for (local_order, candidate) in provider.list(options).await?.into_iter().enumerate() {
                validate_candidate(&candidate, provider.name())?;
                indexed.push((
                    Indexed {
                        rank: candidate.rank,
                        provider_order,
                        local_order,
                    },
                    candidate,
                ));
            }
        }

        indexed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut seen = HashSet::new();
        let mut winners = Vec::new();
        for (_, candidate) in indexed {
            if seen.insert(candidate.name.clone()) {
                winners.push(candidate);
            }
        }
        Ok(winners)
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SkillFileSystemConfig {
    pub include_default_roots: bool,
    pub dsh_home: PathBuf,
    pub agents_home: PathBuf,
    pub custom_skill_dirs: Vec<PathBuf>,
    pub bundled_skill_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct FileSystemSkillProvider {
    config: SkillFileSystemConfig,
}

impl FileSystemSkillProvider {
    pub fn new(config: SkillFileSystemConfig) -> Result<Self> {
        if config.dsh_home.as_os_str().is_empty() || config.agents_home.as_os_str().is_empty() {
            bail!("skill filesystem homes cannot be empty");
        }
        Ok(Self { config })
    }

    async fn roots(&self, cwd: Option<&Path>) -> Result<Vec<SkillRoot>> {
        let mut roots = Vec::new();
        if self.config.include_default_roots {
            if let Some(cwd) = cwd {
                let cwd = absolute_path(cwd)?;
                let project = find_project_root(&cwd).await;
                roots.push(SkillRoot {
                    path: project.join(".dsh/skills"),
                    source: "project-dsh".into(),
                    rank: PROJECT_DSH_RANK,
                    skip_system: false,
                });
                roots.push(SkillRoot {
                    path: project.join(".agents/skills"),
                    source: "project-agents".into(),
                    rank: PROJECT_AGENTS_RANK,
                    skip_system: false,
                });
            }
        }

        for path in &self.config.custom_skill_dirs {
            roots.push(SkillRoot {
                path: absolute_path(path)?,
                source: "custom".into(),
                rank: CUSTOM_RANK,
                skip_system: false,
            });
        }

        if self.config.include_default_roots {
            roots.push(SkillRoot {
                path: absolute_path(&self.config.dsh_home)?.join("skills"),
                source: "user-dsh".into(),
                rank: USER_DSH_RANK,
                skip_system: true,
            });
            roots.push(SkillRoot {
                path: absolute_path(&self.config.agents_home)?.join("skills"),
                source: "user-agents".into(),
                rank: USER_AGENTS_RANK,
                skip_system: false,
            });
        }

        if let Some(path) = &self.config.bundled_skill_dir {
            roots.push(SkillRoot {
                path: absolute_path(path)?,
                source: "bundled".into(),
                rank: BUNDLED_SKILL_RANK,
                skip_system: false,
            });
        }
        Ok(roots)
    }
}

#[derive(Debug)]
struct SkillRoot {
    path: PathBuf,
    source: String,
    rank: u16,
    skip_system: bool,
}

#[async_trait]
impl SkillProvider for FileSystemSkillProvider {
    fn name(&self) -> &str {
        "filesystem"
    }

    async fn list(&self, options: &SkillLookupOptions) -> Result<Vec<SkillCandidate>> {
        let mut candidates = Vec::new();
        for root in self.roots(options.cwd.as_deref()).await? {
            let entries = list_skill_entries(&root.path).await?;
            for entry in entries {
                if root.skip_system && entry.name == ".system" {
                    continue;
                }
                let (path, directory) = if entry.metadata.is_dir() {
                    (entry.path.join("SKILL.md"), entry.path)
                } else if entry.metadata.is_file() && entry.name.ends_with(".md") {
                    (entry.path.clone(), root.path.clone())
                } else {
                    continue;
                };
                let Some(parsed) = parse_skill_file(&path).await? else {
                    continue;
                };
                let resource_base = SkillResourceBase::Directory {
                    path: directory.clone(),
                };
                candidates.push(SkillCandidate {
                    name: parsed.name,
                    description: parsed.description,
                    when_to_use: parsed.when_to_use,
                    invocation: parsed.invocation,
                    source: root.source.clone(),
                    provider: self.name().into(),
                    resource_base: Some(resource_base.clone()),
                    rank: root.rank,
                    locator: serde_json::to_value(LocalLocator {
                        path: path.clone(),
                        directory: directory.clone(),
                    })?,
                    path: Some(path),
                    metadata: parsed.metadata,
                });
            }
        }
        Ok(candidates)
    }

    async fn get(
        &self,
        candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>> {
        let Some(path) = &candidate.path else {
            return Ok(None);
        };
        let Some(parsed) = parse_skill_file(path).await? else {
            return Ok(None);
        };
        if parsed.name != candidate.name {
            return Ok(None);
        }
        Ok(Some(SkillDefinition {
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: candidate.source.clone(),
            provider: candidate.provider.clone(),
            resource_base: candidate.resource_base.clone(),
            path: Some(path.clone()),
            metadata: parsed.metadata,
            content: parsed.content,
        }))
    }
}

#[derive(Debug, Clone, Serialize)]
struct LocalLocator {
    path: PathBuf,
    directory: PathBuf,
}

#[derive(Debug, Clone)]
struct RootEntry {
    name: String,
    path: PathBuf,
    metadata: std::fs::Metadata,
}

#[derive(Debug, Clone)]
struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    metadata: Option<serde_json::Value>,
    content: String,
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    when_to_use: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<serde_yaml::Value>,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<serde_yaml::Value>,
    metadata: Option<serde_json::Value>,
}

pub fn is_skill_name(value: &str) -> bool {
    if value.is_empty() || value.starts_with('-') || value.ends_with('-') {
        return false;
    }
    let mut previous_hyphen = false;
    for character in value.chars() {
        let is_segment = character.is_ascii_lowercase() || character.is_ascii_digit();
        if character == '-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if is_segment {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    true
}

pub fn render_skill_content(
    name: &str,
    provider: &str,
    resource_base: Option<SkillResourceBase>,
    content: &str,
) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "<skill_content name=\"{}\">",
        escape_attribute(name)
    ));
    output.push_str("\n<skill_resources>\n");
    match resource_base {
        Some(SkillResourceBase::Directory { path }) => {
            output.push_str(&format!(
                "Base directory for this skill: {}\n",
                escape_text(&path.display().to_string())
            ));
            output.push_str(
                "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n",
            );
        }
        Some(SkillResourceBase::Url { url }) => {
            output.push_str(&format!("Base URL for this skill: {}\n", escape_text(&url)));
            output.push_str(
                "Resolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed.\n",
            );
        }
        Some(SkillResourceBase::Opaque { description }) => {
            output.push_str(&format!(
                "Resources for this skill: {}\n",
                escape_text(&description)
            ));
            output.push_str("Load referenced resources only as needed.\n");
        }
        None => {
            output.push_str(&format!(
                "Resources for this skill are managed by provider \"{}\".\n",
                escape_text(provider)
            ));
            output.push_str("Load referenced resources only as needed.\n");
        }
    }
    output.push_str("</skill_resources>\n\n<skill_instructions>\n");
    output.push_str(content);
    output.push_str("\n</skill_instructions>\n</skill_content>");
    output
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn validate_candidate(candidate: &SkillCandidate, provider_name: &str) -> Result<()> {
    if !is_skill_name(&candidate.name) {
        bail!(
            "skill provider \"{provider_name}\" returned invalid skill name \"{}\"",
            candidate.name
        );
    }
    if candidate.description.is_empty() {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" without a description",
            candidate.name
        );
    }
    if candidate.source.is_empty() {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" without a source",
            candidate.name
        );
    }
    if candidate.provider != provider_name {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" for provider \"{}\"",
            candidate.name,
            candidate.provider
        );
    }
    Ok(())
}

fn validate_definition(definition: &SkillDefinition) -> Result<()> {
    if !is_skill_name(&definition.name) {
        bail!("invalid skill name \"{}\"", definition.name);
    }
    if definition.description.is_empty() {
        bail!("skill \"{}\" requires a description", definition.name);
    }
    if definition.source.is_empty() || definition.provider.is_empty() {
        bail!(
            "skill \"{}\" requires a source and provider",
            definition.name
        );
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current = std::env::current_dir().context("failed to resolve relative skill root")?;
    Ok(current.join(path))
}

async fn find_project_root(cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        if tokio::fs::try_exists(current.join(".git"))
            .await
            .unwrap_or(false)
        {
            return current;
        }
        let Some(parent) = current.parent() else {
            return cwd.to_path_buf();
        };
        if parent == current {
            return cwd.to_path_buf();
        }
        current = parent.to_path_buf();
    }
}

async fn list_skill_entries(path: &Path) -> Result<Vec<RootEntry>> {
    let mut entries = Vec::new();
    let mut directory = match tokio::fs::read_dir(path).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill root {}", path.display()))
        }
    };
    while let Some(entry) = directory
        .next_entry()
        .await
        .with_context(|| format!("failed to read skill root {}", path.display()))?
    {
        let Ok(metadata) = tokio::fs::metadata(entry.path()).await else {
            continue;
        };
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        entries.push(RootEntry {
            name,
            path: entry.path(),
            metadata,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

async fn parse_skill_file(path: &Path) -> Result<Option<ParsedSkill>> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill file {}", path.display()))
        }
    };
    let Some((yaml, body)) = split_frontmatter(&raw) else {
        return Ok(None);
    };
    let Ok(frontmatter) = parse_frontmatter(yaml) else {
        return Ok(None);
    };
    let Some(frontmatter) = frontmatter else {
        return Ok(None);
    };
    if !is_skill_name(&frontmatter.name) || frontmatter.description.is_empty() {
        return Ok(None);
    }
    let disable_model = match frontmatter_boolean(
        frontmatter.disable_model_invocation,
        "disable-model-invocation",
    ) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let user_invocable = match frontmatter_boolean(frontmatter.user_invocable, "user-invocable") {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    Ok(Some(ParsedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        when_to_use: frontmatter.when_to_use.filter(|value| !value.is_empty()),
        invocation: SkillInvocationPolicy {
            model_invocable: disable_model != Some(true),
            user_invocable: user_invocable != Some(false),
        },
        metadata: frontmatter.metadata.filter(|value| value.is_object()),
        content: body.trim().to_string(),
    }))
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let first_end = raw.find('\n')?;
    if raw[..first_end].trim_end_matches('\r') != "---" {
        return None;
    }
    let start = first_end + 1;
    let mut line_start = start;
    loop {
        let relative_end = raw[line_start..].find('\n')?;
        let line_end = line_start + relative_end;
        let line = raw[line_start..line_end].trim_end_matches('\r');
        if line == "---" {
            return Some((&raw[start..line_start], &raw[line_end + 1..]));
        }
        line_start = line_end + 1;
    }
}

fn parse_frontmatter(yaml: &str) -> Result<Option<Frontmatter>> {
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(mapping) = value.as_mapping() else {
        return Ok(None);
    };
    for legacy in ["disableModelInvocation", "modelInvocable", "userInvocable"] {
        if mapping.contains_key(serde_yaml::Value::String(legacy.into())) {
            return Ok(None);
        }
    }
    serde_yaml::from_value(value)
        .context("invalid skill frontmatter")
        .map(Some)
}

fn frontmatter_boolean(value: Option<serde_yaml::Value>, field: &str) -> Result<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let rendered = match &value {
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.to_lowercase(),
        _ => bail!("frontmatter field \"{field}\" must be a boolean"),
    };
    match rendered.as_str() {
        "true" | "yes" | "on" | "1" => Ok(Some(true)),
        "false" | "no" | "off" | "0" => Ok(Some(false)),
        _ => bail!("frontmatter field \"{field}\" must be a boolean"),
    }
}
