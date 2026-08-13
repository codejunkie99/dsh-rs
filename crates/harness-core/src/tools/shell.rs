use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::tools::fs::ScopedFs;
use crate::tools::{Tool, ToolInvocation, ToolOutput, ToolSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellPolicy {
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub allowed_binaries: BTreeSet<String>,
}

impl Default for ShellPolicy {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            max_output_bytes: 64 * 1024,
            allowed_binaries: BTreeSet::new(),
        }
    }
}

impl ShellPolicy {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read shell policy {}", path.as_ref().display()))?;
        let policy: Self = serde_json::from_str(&raw).context("invalid shell policy")?;
        policy.canonicalize()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create shell policy parent {}", parent.display())
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("shell.json"),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, raw)?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("failed to publish shell policy {}", path.display()))?;
        Ok(())
    }

    pub fn canonicalize(self) -> Result<Self> {
        let mut allowed_binaries = BTreeSet::new();
        for binary in &self.allowed_binaries {
            let path = Path::new(binary);
            if !path.is_absolute() {
                bail!("shell allowlist entries must be absolute paths: {binary}");
            }
            let canonical = path
                .canonicalize()
                .with_context(|| format!("allowed shell binary does not exist: {binary}"))?;
            allowed_binaries.insert(canonical.to_string_lossy().into_owned());
        }
        Ok(Self {
            timeout_ms: self.timeout_ms,
            max_output_bytes: self.max_output_bytes,
            allowed_binaries,
        })
    }

    pub fn is_allowed(&self, binary: &str) -> bool {
        Path::new(binary)
            .canonicalize()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string))
            .is_some_and(|path| self.allowed_binaries.contains(&path))
    }
}

pub struct CommandTool {
    filesystem: Arc<ScopedFs>,
    policy: ShellPolicy,
}

impl CommandTool {
    pub fn new(filesystem: Arc<ScopedFs>, policy: ShellPolicy) -> Self {
        Self { filesystem, policy }
    }

    fn parse_argv(arguments: &serde_json::Value) -> Result<Vec<String>> {
        let argv = arguments
            .get("argv")
            .and_then(|value| value.as_array())
            .context("run_command requires argv as an array")?;
        let mut parsed = Vec::with_capacity(argv.len());
        for value in argv {
            let argument = value
                .as_str()
                .with_context(|| "every run_command argv entry must be a string")?;
            parsed.push(argument.to_string());
        }
        if parsed.is_empty() {
            bail!("run_command argv cannot be empty");
        }
        Ok(parsed)
    }

    async fn read_bounded<R>(
        mut reader: R,
        max_bytes: usize,
        limit_hit: mpsc::Sender<()>,
    ) -> Result<Vec<u8>>
    where
        R: AsyncReadExt + Unpin,
    {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let count = reader.read(&mut chunk).await?;
            if count == 0 {
                return Ok(output);
            }
            if output.len() + count > max_bytes {
                let _ = limit_hit.send(()).await;
                bail!("process output exceeds output limit {max_bytes} bytes");
            }
            output.extend_from_slice(&chunk[..count]);
        }
    }

    fn collect_stream(
        reader: impl AsyncReadExt + Unpin + Send + 'static,
        max_bytes: usize,
        limit_hit: mpsc::Sender<()>,
    ) -> tokio::task::JoinHandle<Result<Vec<u8>>> {
        tokio::spawn(Self::read_bounded(reader, max_bytes, limit_hit))
    }
}

#[async_trait]
impl Tool for CommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "Run one explicitly allowlisted executable directly in the scoped workspace. No shell is invoked.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Absolute executable path followed by direct arguments"
                    }
                },
                "required": ["argv"]
            }),
        }
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let argv = Self::parse_argv(&invocation.arguments)?;
        let binary = PathBuf::from(&argv[0]);
        if !self.policy.is_allowed(&argv[0]) {
            bail!("executable is not allowed by shell policy: {}", argv[0]);
        }

        let mut command = Command::new(&binary);
        command
            .args(&argv[1..])
            .current_dir(self.filesystem.root())
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn {}", binary.display()))?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;
        let stderr = child.stderr.take().context("child stderr was not piped")?;

        let (limit_sender, mut limit_receiver) = mpsc::channel(1);
        let stdout_task =
            Self::collect_stream(stdout, self.policy.max_output_bytes, limit_sender.clone());
        let stderr_task = Self::collect_stream(stderr, self.policy.max_output_bytes, limit_sender);

        let status = tokio::select! {
            status = child.wait() => status,
            limit_signal = limit_receiver.recv() => {
                if limit_signal.is_some() {
                    child.start_kill().ok();
                    let _ = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    bail!("process output exceeds output limit {}", self.policy.max_output_bytes);
                }
                match tokio::time::timeout(
                    Duration::from_millis(self.policy.timeout_ms),
                    child.wait(),
                )
                .await
                {
                    Ok(status) => status,
                    Err(_) => {
                        child.start_kill().ok();
                        let _ = child.wait().await;
                        let _ = stdout_task.await;
                        let _ = stderr_task.await;
                        bail!("process timed out after {}ms", self.policy.timeout_ms);
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(self.policy.timeout_ms)) => {
                child.start_kill().ok();
                let status = child.wait().await?;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                bail!("process timed out after {}ms with status {status}", self.policy.timeout_ms);
            }
        }
        .with_context(|| format!("failed to wait for {}", binary.display()))?;

        let stdout = stdout_task.await??;
        let stderr = stderr_task.await??;
        Ok(ToolOutput {
            ok: status.success(),
            output: format!(
                "exit status: {}\nstdout:\n{}\nstderr:\n{}",
                status.code().unwrap_or(-1),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolInvocation};
    use std::sync::Arc;

    fn invocation(argv: &[&str]) -> ToolInvocation {
        ToolInvocation {
            call_id: "call_shell".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({ "argv": argv }),
        }
    }

    #[test]
    fn shell_policy_only_allows_canonical_absolute_binaries() {
        let mut policy = ShellPolicy::default();
        policy.allowed_binaries.insert("/bin/echo".into());
        let policy = policy.canonicalize().unwrap();

        assert!(policy.is_allowed("/bin/echo"));
        assert!(!policy.is_allowed("/bin/sh"));
        assert!(!policy.is_allowed("echo"));
    }

    #[test]
    fn shell_policy_round_trips_and_rejects_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.json");
        let mut policy = ShellPolicy {
            timeout_ms: 250,
            max_output_bytes: 128,
            allowed_binaries: BTreeSet::from(["/bin/echo".into()]),
        };
        policy = policy.canonicalize().unwrap();
        policy.save(&path).unwrap();
        assert_eq!(ShellPolicy::load(&path).unwrap(), policy);
        assert!(ShellPolicy::load(dir.path().join("missing.json")).is_err());
        std::fs::write(dir.path().join("invalid.json"), "{}").unwrap();
        assert!(ShellPolicy::load(dir.path().join("invalid.json")).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tool_runs_allowlisted_executable_in_scoped_root() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = Arc::new(ScopedFs::new(root.path()).unwrap());
        let mut policy = ShellPolicy::default();
        policy.allowed_binaries.insert("/bin/echo".into());
        let policy = policy.canonicalize().unwrap();
        let tool = CommandTool::new(filesystem, policy);

        assert_eq!(tool.spec().name, "run_command");
        let output = tool
            .execute(invocation(&["/bin/echo", "hello"]))
            .await
            .unwrap();
        assert!(output.ok);
        assert!(output.output.contains("exit status: 0"));
        assert!(output.output.contains("stdout:"));
        assert!(output.output.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tool_rejects_binaries_outside_the_allowlist() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = Arc::new(ScopedFs::new(root.path()).unwrap());
        let tool = CommandTool::new(filesystem, ShellPolicy::default());

        let error = tool
            .execute(invocation(&["/bin/echo", "no"]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tool_times_out_and_stops_the_process() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = Arc::new(ScopedFs::new(root.path()).unwrap());
        let mut policy = ShellPolicy::default();
        policy.allowed_binaries.insert("/bin/sleep".into());
        policy.timeout_ms = 10;
        let policy = policy.canonicalize().unwrap();
        let tool = CommandTool::new(filesystem, policy);

        let started = std::time::Instant::now();
        let error = tool
            .execute(invocation(&["/bin/sleep", "5"]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tool_enforces_bounded_output() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = Arc::new(ScopedFs::new(root.path()).unwrap());
        let mut policy = ShellPolicy::default();
        policy.allowed_binaries.insert("/bin/echo".into());
        policy.max_output_bytes = 10;
        let policy = policy.canonicalize().unwrap();
        let tool = CommandTool::new(filesystem, policy);
        let large_input = "A".repeat(100);

        let error = tool
            .execute(invocation(&["/bin/echo", &large_input]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds output limit"));
    }
}
