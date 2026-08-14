use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tools::fs::ScopedFs;
use crate::tools::{Tool, ToolCallKind, ToolCallView, ToolInvocation, ToolOutput, ToolSpec};

const MAX_RESULTS: usize = 250;

fn relative_files(root: &Path, start: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(start)
        .with_context(|| format!("failed to search {}", start.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if entry.file_name() != ".git" {
                relative_files(root, &path, output)?;
            }
        } else if entry.file_type()?.is_file() {
            output.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

fn files_under(fs: &ScopedFs, path: &str) -> Result<Vec<PathBuf>> {
    let base = if path.trim().is_empty() { "." } else { path };
    let resolved = fs.root().join(base);
    if !resolved.starts_with(fs.root()) {
        anyhow::bail!("search path resolves outside scoped filesystem");
    }
    let mut files = Vec::new();
    relative_files(fs.root(), &resolved.canonicalize()?, &mut files)?;
    files.sort();
    Ok(files)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    fn matches(p: &[u8], v: &[u8]) -> bool {
        if p.is_empty() { return v.is_empty(); }
        if p[0] == b'*' {
            return matches(&p[1..], v) || (!v.is_empty() && matches(p, &v[1..]));
        }
        !v.is_empty() && (p[0] == b'?' || p[0] == v[0]) && matches(&p[1..], &v[1..])
    }
    matches(pattern.as_bytes(), value.as_bytes())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArguments {
    pattern: String,
    #[serde(default)]
    path: String,
}

pub struct GlobTool {
    fs: Arc<ScopedFs>,
}

impl GlobTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self { Self { fs } }
}

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files whose paths match a glob pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern to match file paths against."},
                    "path": {"type": "string", "description": "Directory to search in. Defaults to the workspace."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn present_call(&self, args: &serde_json::Value) -> Option<ToolCallView> {
        Some(ToolCallView::Generic {
            title: format!("Glob {}", args.get("pattern").and_then(|v| v.as_str()).unwrap_or("")),
            kind: Some(ToolCallKind::Search),
            raw_input: None,
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: GlobArguments = serde_json::from_value(invocation.arguments)?;
        if args.pattern.trim().is_empty() { anyhow::bail!("pattern must be a non-empty string"); }
        let anchored = args.pattern.contains('/');
        let mut matches = Vec::new();
        for file in files_under(&self.fs, &args.path)? {
            let display = file.to_string_lossy().replace('\\', "/");
            let candidate = if anchored { display.clone() } else {
                file.file_name().unwrap_or_default().to_string_lossy().into_owned()
            };
            if wildcard_match(&args.pattern, &candidate) {
                matches.push(display);
            }
        }
        if matches.len() > 100 { matches.truncate(100); }
        Ok(ToolOutput { ok: true, output: matches.join("\n") })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArguments {
    pattern: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    include: Option<String>,
}

pub struct GrepTool {
    fs: Arc<ScopedFs>,
}

impl GrepTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self { Self { fs } }
}

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents and return matching lines with line numbers.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "pattern": {"type": "string", "description": "Text pattern to search for."},
                    "path": {"type": "string", "description": "File or directory to search."},
                    "include": {"type": "string", "description": "Optional glob filter for file names."}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn present_call(&self, _args: &serde_json::Value) -> Option<ToolCallView> {
        Some(ToolCallView::Generic {
            title: "Grep workspace".into(),
            kind: Some(ToolCallKind::Search),
            raw_input: None,
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: GrepArguments = serde_json::from_value(invocation.arguments)?;
        if args.pattern.is_empty() { anyhow::bail!("pattern must be a non-empty string"); }
        let mut output = Vec::new();
        let files = files_under(&self.fs, &args.path)?;
        for file in files {
            if let Some(include) = &args.include {
                let name = file.file_name().unwrap_or_default().to_string_lossy();
                if !wildcard_match(include, &name) { continue; }
            }
            let relative = file.to_string_lossy().replace('\\', "/");
            let text = self.fs.read_text(&relative)?;
            for (number, line) in text.lines().enumerate() {
                if line.contains(&args.pattern) {
                    output.push(format!("{relative}:{}: {line}", number + 1));
                    if output.len() == MAX_RESULTS { break; }
                }
            }
            if output.len() == MAX_RESULTS { break; }
        }
        Ok(ToolOutput { ok: true, output: output.join("\n") })
    }
}
