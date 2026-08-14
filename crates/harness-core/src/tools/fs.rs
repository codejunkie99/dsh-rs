use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::tools::{Tool, ToolCallKind, ToolCallView, ToolInvocation, ToolOutput, ToolSpec};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: DirEntryKind,
}

#[derive(Debug, Clone)]
pub struct ScopedFs {
    root: PathBuf,
    max_read_bytes: u64,
}

impl ScopedFs {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)
            .with_context(|| format!("failed to create scoped root {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize scoped root {}", root.display()))?;
        Ok(Self {
            root,
            max_read_bytes: 1024 * 1024,
        })
    }

    pub fn with_max_read_bytes(mut self, max_read_bytes: u64) -> Self {
        self.max_read_bytes = max_read_bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read_text(&self, relative_path: &str) -> Result<String> {
        let path = self.resolve_existing(relative_path)?;
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if metadata.len() > self.max_read_bytes {
            bail!(
                "file {} is {} bytes and exceeds read limit {max} bytes",
                path.display(),
                metadata.len(),
                max = self.max_read_bytes
            );
        }
        std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub fn write_text(&self, relative_path: &str, contents: &str) -> Result<()> {
        let path = self.resolve_for_write(relative_path)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("scoped file has no parent directory"))?;
        let temporary = parent.join(format!(
            ".dsh-write-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));

        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .with_context(|| {
                    format!("failed to create temporary file {}", temporary.display())
                })?;
            file.write_all(contents.as_bytes()).with_context(|| {
                format!("failed to write temporary file {}", temporary.display())
            })?;
            file.sync_all().with_context(|| {
                format!("failed to sync temporary file {}", temporary.display())
            })?;
            drop(file);
            std::fs::rename(&temporary, &path)
                .with_context(|| format!("failed to publish {}", path.display()))?;
            Ok(())
        })();

        if result.is_err() && temporary.exists() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn list_dir(&self, relative_path: &str) -> Result<Vec<DirEntryInfo>> {
        let path = self.resolve_existing(relative_path)?;
        if !path.is_dir() {
            bail!("scoped path is not a directory: {}", path.display());
        }

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)
            .with_context(|| format!("failed to list {}", path.display()))?
        {
            let entry = entry.with_context(|| format!("failed to enumerate {}", path.display()))?;
            let file_type = entry.file_type()?;
            let kind = if file_type.is_dir() {
                DirEntryKind::Directory
            } else if file_type.is_file() {
                DirEntryKind::File
            } else if file_type.is_symlink() {
                DirEntryKind::Symlink
            } else {
                DirEntryKind::Other
            };
            entries.push(DirEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn lexical_path(&self, relative_path: &str) -> Result<PathBuf> {
        let input = Path::new(relative_path);
        if input.is_absolute() {
            bail!("absolute paths are not allowed in scoped filesystem");
        }

        let mut path = PathBuf::new();
        for component in input.components() {
            match component {
                Component::Normal(part) => path.push(part),
                Component::CurDir => {}
                _ => bail!("path traversal is not allowed in scoped filesystem"),
            }
        }
        Ok(self.root.join(path))
    }

    fn resolve_existing(&self, relative_path: &str) -> Result<PathBuf> {
        let lexical = self.lexical_path(relative_path)?;
        let canonical = lexical.canonicalize().with_context(|| {
            format!("path does not exist in scoped filesystem: {relative_path}")
        })?;
        if !canonical.starts_with(&self.root) {
            bail!("path resolves outside scoped filesystem root");
        }
        Ok(canonical)
    }

    fn resolve_for_write(&self, relative_path: &str) -> Result<PathBuf> {
        let lexical = self.lexical_path(relative_path)?;
        let file_name = lexical
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("invalid scoped file path: {relative_path}"))?;
        if file_name.trim().is_empty() {
            bail!("scoped file path has no file name");
        }

        let parent = lexical
            .parent()
            .ok_or_else(|| anyhow::anyhow!("scoped file path has no parent"))?
            .canonicalize()
            .with_context(|| format!("scoped parent does not exist: {relative_path}"))?;
        if !parent.starts_with(&self.root) {
            bail!("scoped write parent resolves outside root");
        }

        let target = parent.join(file_name);
        if let Ok(metadata) = std::fs::symlink_metadata(&target) {
            if metadata.file_type().is_symlink() {
                bail!("symlink writes are not allowed in scoped filesystem");
            }
            let canonical = target.canonicalize()?;
            if !canonical.starts_with(&self.root) {
                bail!("scoped write resolves outside root");
            }
        }
        Ok(target)
    }
}

const DEFAULT_READ_LIMIT: usize = 2_000;
const MAX_READ_LINE_LENGTH: usize = 2_000;
const MAX_READ_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadToolArguments {
    file_path: String,
    #[serde(default = "default_read_offset")]
    offset: usize,
    #[serde(default = "default_read_limit")]
    limit: usize,
}

fn default_read_offset() -> usize { 1 }
fn default_read_limit() -> usize { DEFAULT_READ_LIMIT }

fn format_read_output(path: &str, offset: usize, lines: &[(usize, String)], total_lines: usize, truncated_by_bytes: bool) -> String {
    let end_line = lines.last().map(|(number, _)| *number).unwrap_or(offset.saturating_sub(1));
    let footer = if truncated_by_bytes {
        format!("(Output capped. Showing lines {offset}-{end_line}. Use offset={} to continue.)", end_line + 1)
    } else if end_line < total_lines {
        format!("(Showing lines {offset}-{end_line} of {total_lines}. Use offset={} to continue.)", end_line + 1)
    } else {
        format!("(End of file - total {total_lines} lines)")
    };
    let body = if lines.is_empty() {
        footer
    } else {
        let numbered = lines.iter()
            .map(|(number, text)| format!("{number}: {text}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{numbered}\n\n{footer}")
    };
    format!("<path>{path}</path>\n<type>file</type>\n<content>\n{body}\n</content>")
}

pub struct ReadFileTool {
    fs: Arc<ScopedFs>,
}

impl ReadFileTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self {
        Self { fs }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a UTF-8 text file and return line-numbered content.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to read, resolved by the filesystem backend."
                    },
                    "offset": {
                        "type": "number",
                        "description": "1-based first line to return. Defaults to 1."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to return. Defaults to 2000."
                    }
                },
                "required": ["file_path"]
            }),
        }
    }

    fn present_call(&self, arguments: &serde_json::Value) -> Option<ToolCallView> {
        let path = arguments.get("file_path").and_then(|value| value.as_str()).unwrap_or_default();
        Some(ToolCallView::Generic {
            title: format!("Read {path}"),
            kind: Some(ToolCallKind::Read),
            raw_input: None,
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let arguments: ReadToolArguments = serde_json::from_value(invocation.arguments)?;
        if arguments.file_path.trim().is_empty() {
            bail!("file_path must be a non-empty string");
        }
        if arguments.offset == 0 {
            bail!("offset must be a positive integer");
        }
        if arguments.limit == 0 {
            bail!("limit must be a positive integer");
        }
        if arguments.limit > DEFAULT_READ_LIMIT {
            bail!("limit must be less than or equal to {DEFAULT_READ_LIMIT}");
        }

        let raw = self.fs.read_text(&arguments.file_path)?;
        let all_lines: Vec<String> = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split_terminator('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
                .collect()
        };
        let total_lines = all_lines.len();
        if arguments.offset > total_lines && !(total_lines == 0 && arguments.offset == 1) {
            bail!(
                "offset {} is out of range for \"{}\" ({} lines)",
                arguments.offset,
                arguments.file_path,
                total_lines
            );
        }

        let mut lines = Vec::new();
        let mut output_bytes = 0usize;
        let mut truncated_by_bytes = false;
        for (index, raw_line) in all_lines.iter().enumerate().skip(arguments.offset.saturating_sub(1)).take(arguments.limit) {
            let mut line = raw_line.clone();
            if line.chars().count() > MAX_READ_LINE_LENGTH {
                line = line.chars().take(MAX_READ_LINE_LENGTH).collect::<String>()
                    + &format!("... (line truncated to {MAX_READ_LINE_LENGTH} chars)");
            }
            let bytes = line.as_bytes().len() + usize::from(!lines.is_empty());
            if output_bytes + bytes > MAX_READ_OUTPUT_BYTES {
                truncated_by_bytes = true;
                break;
            }
            output_bytes += bytes;
            lines.push((index + 1, line));
        }

        Ok(ToolOutput {
            ok: true,
            output: format_read_output(
                &arguments.file_path,
                arguments.offset,
                &lines,
                total_lines,
                truncated_by_bytes,
            ),
        })
    }
}

pub struct WriteFileTool {
    fs: Arc<ScopedFs>,
}

impl WriteFileTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self {
        Self { fs }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Create or fully replace a UTF-8 text file.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to write, resolved by the filesystem backend."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full UTF-8 text content to write."
                    }
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    fn present_call(&self, arguments: &serde_json::Value) -> Option<ToolCallView> {
        let path = arguments.get("file_path").and_then(|value| value.as_str()).unwrap_or_default();
        Some(ToolCallView::Generic {
            title: format!("Write {path}"),
            kind: Some(ToolCallKind::Edit),
            raw_input: None,
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let file_path = required_string(&invocation.arguments, "file_path")?;
        if file_path.trim().is_empty() {
            bail!("file_path must be a non-empty string");
        }
        let content = required_string(&invocation.arguments, "content")?;
        let existed = self.fs.root().join(&file_path).exists();
        self.fs.write_text(&file_path, &content)?;
        let operation = if existed { "Updated" } else { "Created" };
        Ok(ToolOutput {
            ok: true,
            output: format!("<path>{file_path}</path>\n<type>file</type>\n<content>\n{operation} file\n</content>"),
        })
    }
}

/// The model-facing `edit` tool: a literal-text replacement that is unique-match
/// by default. Mirrors upstream `packages/fs/tool-fs/src/edit.ts` plus the
/// literal-edit core in `packages/fs/fs-local/src/fsio.ts` (`applyLiteralEdit`).
pub struct EditFileTool {
    fs: Arc<ScopedFs>,
}

impl EditFileTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self {
        Self { fs }
    }

    /// Collapse `\r\n` to `\n`, the canonical in-memory form upstream uses for
    /// every edit basis; lone `\r` bytes are left untouched.
    fn normalize_line_endings(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    /// Detects the dominant line-ending style, mirroring upstream
    /// `detectLineEndings` (CRLF wins on a tie only when it outnumbers bare LF).
    fn detect_crlf(raw: &str) -> bool {
        let crlf = raw.matches("\r\n").count();
        let lf = raw.matches('\n').count().saturating_sub(crlf);
        crlf > lf
    }

    /// Restores the file's original line-ending style after editing, mirroring
    /// upstream `restoreLineEndings` (re-normalizes first so CRLF is never doubled).
    fn restore_line_endings(text: &str, crlf: bool) -> String {
        if crlf {
            Self::normalize_line_endings(text).replace('\n', "\r\n")
        } else {
            text.to_string()
        }
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }
        haystack.match_indices(needle).count()
    }

    /// Apply the literal edit and return the line-ending-restored result. Errors
    /// use the same model-facing messages as upstream `applyLiteralEdit`.
    fn edit_text(
        &self,
        file_path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String> {
        let raw = self.fs.read_text(file_path)?;
        let content = Self::normalize_line_endings(&raw);
        let old_norm = Self::normalize_line_endings(old_string);
        if old_norm.is_empty() {
            bail!("old_string must be a non-empty string");
        }
        let new_norm = Self::normalize_line_endings(new_string);
        let replacements = Self::count_occurrences(&content, &old_norm);
        if replacements == 0 {
            bail!("old_string was not found in \"{file_path}\"");
        }
        if !replace_all && replacements > 1 {
            bail!(
                "old_string matched {replacements} times in \"{file_path}\"; provide a more specific old_string or set replace_all to true"
            );
        }
        let edited = content.replace(&old_norm, &new_norm);
        Ok(Self::restore_line_endings(&edited, Self::detect_crlf(&raw)))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditToolArguments {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Edit an existing UTF-8 text file by replacing literal text.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to edit, resolved by the filesystem backend."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Literal text to replace. Must match exactly."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Literal replacement text. Use an empty string to delete the match."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once."
                    }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        }
    }

    fn present_call(&self, arguments: &serde_json::Value) -> Option<ToolCallView> {
        let file_path = arguments
            .get("file_path")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        Some(ToolCallView::Generic {
            title: format!("Edit {file_path}"),
            kind: Some(ToolCallKind::Edit),
            raw_input: None,
        })
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let arguments: EditToolArguments = serde_json::from_value(invocation.arguments)?;
        let file_path = arguments.file_path;
        if file_path.trim().is_empty() {
            bail!("file_path must be a non-empty string");
        }
        if arguments.old_string.is_empty() {
            bail!("old_string must be a non-empty string");
        }
        if arguments.old_string == arguments.new_string {
            bail!("old_string and new_string must differ");
        }

        let edited = self.edit_text(
            &file_path,
            &arguments.old_string,
            &arguments.new_string,
            arguments.replace_all,
        )?;
        self.fs.write_text(&file_path, &edited)?;

        let output = if arguments.replace_all {
            format!("The file {file_path} has been updated. All occurrences were successfully replaced.")
        } else {
            format!("The file {file_path} has been updated successfully.")
        };
        Ok(ToolOutput { ok: true, output })
    }
}

pub struct ListDirTool {
    fs: Arc<ScopedFs>,
}

impl ListDirTool {
    pub fn new(fs: Arc<ScopedFs>) -> Self {
        Self { fs }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List immediate entries in a scoped workspace directory.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative directory"}
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let path = required_string(&invocation.arguments, "path")?;
        let entries = self.fs.list_dir(&path)?;
        Ok(ToolOutput {
            ok: true,
            output: serde_json::to_string(&entries)?,
        })
    }
}

fn required_string(arguments: &serde_json::Value, key: &str) -> Result<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .with_context(|| format!("tool argument {key} must be a string"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolInvocation};

    #[test]
    fn scoped_fs_confines_reads_to_its_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("notes")).unwrap();
        std::fs::write(root.path().join("notes/hello.txt"), "hello").unwrap();

        let fs = ScopedFs::new(root.path()).unwrap();
        assert_eq!(fs.read_text("notes/hello.txt").unwrap(), "hello");
        assert!(fs.read_text("../hello.txt").is_err());
        assert!(fs.read_text("/etc/hosts").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scoped_fs_rejects_symlink_escapes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            root.path().join("escape.txt"),
        )
        .unwrap();

        let fs = ScopedFs::new(root.path()).unwrap();
        assert!(fs.read_text("escape.txt").is_err());
    }

    #[test]
    fn scoped_fs_writes_atomically_and_lists_sorted_entries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        let fs = ScopedFs::new(root.path()).unwrap();
        fs.write_text("workspace/b.txt", "b").unwrap();
        fs.write_text("workspace/a.txt", "a").unwrap();

        assert_eq!(fs.read_text("workspace/a.txt").unwrap(), "a");
        let entries = fs.list_dir("workspace").unwrap();
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
        assert!(entries.iter().all(|entry| entry.kind == DirEntryKind::File));
    }

    #[test]
    fn scoped_fs_enforces_read_limits() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("large.txt"), "123456789").unwrap();
        let fs = ScopedFs::new(root.path()).unwrap().with_max_read_bytes(8);
        let error = fs.read_text("large.txt").unwrap_err();
        assert!(error.to_string().contains("exceeds read limit"));
    }

    #[tokio::test]
    async fn filesystem_tools_execute_canonical_read_and_write_requests() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        std::fs::write(
            root.path().join("workspace/hello.txt"),
            "one\ntwo\nthree\nfour\n",
        )
        .unwrap();
        let fs = std::sync::Arc::new(ScopedFs::new(root.path()).unwrap());

        let write = WriteFileTool::new(fs.clone());
        assert_eq!(write.spec().name, "write");
        let output = write
            .execute(ToolInvocation {
                call_id: "call_write".into(),
                name: "write".into(),
                arguments: serde_json::json!({
                    "file_path": "workspace/new.txt",
                    "content": "hello"
                }),
            })
            .await
            .unwrap();
        assert!(output.ok);
        assert!(output.output.contains("<path>workspace/new.txt</path>"));
        assert!(output.output.contains("Created file"));

        let read = ReadFileTool::new(fs.clone());
        assert_eq!(read.spec().name, "read");
        let output = read
            .execute(ToolInvocation {
                call_id: "call_read".into(),
                name: "read".into(),
                arguments: serde_json::json!({
                    "file_path": "workspace/hello.txt",
                    "offset": 2,
                    "limit": 2
                }),
            })
            .await
            .unwrap();
        assert_eq!(
            output.output,
            "<path>workspace/hello.txt</path>\n<type>file</type>\n<content>\n2: two\n3: three\n\n(Showing lines 2-3 of 4. Use offset=4 to continue.)\n</content>"
        );
    }

    #[tokio::test]
    async fn read_rejects_invalid_windows_and_reports_eof() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "one\ntwo\n").unwrap();
        let read = ReadFileTool::new(std::sync::Arc::new(ScopedFs::new(root.path()).unwrap()));

        for (arguments, expected) in [
            (serde_json::json!({"file_path": "a.txt", "offset": 0}), "offset must be a positive integer"),
            (serde_json::json!({"file_path": "a.txt", "limit": 0}), "limit must be a positive integer"),
            (serde_json::json!({"file_path": "a.txt", "limit": 2001}), "limit must be less than or equal to 2000"),
            (serde_json::json!({"file_path": "a.txt", "offset": 3}), "offset 3 is out of range for \"a.txt\" (2 lines)"),
        ] {
            let error = read.execute(ToolInvocation {
                call_id: "call_read".into(),
                name: "read".into(),
                arguments,
            }).await.unwrap_err();
            assert!(error.to_string().contains(expected), "{}", error);
        }
    }

}
