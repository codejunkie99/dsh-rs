use anyhow::{bail, Context, Result};
use harness_core::tools::shell::CommandTool;
use harness_core::tools::{Tool, ToolInvocation};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEntry {
    pub command: String,
    pub output: String,
    pub ok: bool,
}

#[derive(Debug)]
pub struct TerminalHistory {
    entries: Vec<TerminalEntry>,
    capacity: usize,
}

impl TerminalHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&mut self, entry: TerminalEntry) {
        self.entries.insert(0, entry);
        self.entries.truncate(self.capacity);
    }

    pub fn entries(&self) -> &[TerminalEntry] {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub fn parse_argv(command: &str) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(character) = chars.next() {
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else if active_quote == '"' && character == '\\' {
                let escaped = chars
                    .next()
                    .context("double-quoted command ends with an unfinished escape")?;
                current.push(escaped);
            } else {
                current.push(character);
            }
            continue;
        }

        match character {
            ' ' | '\t' | '\n' | '\r' => {
                if !current.is_empty() {
                    argv.push(std::mem::take(&mut current));
                }
            }
            '\'' | '"' => quote = Some(character),
            '\\' => {
                let escaped = chars
                    .next()
                    .context("command ends with an unfinished escape")?;
                current.push(escaped);
            }
            _ => current.push(character),
        }
    }

    if quote.is_some() {
        bail!("command has an unterminated quote");
    }
    if !current.is_empty() {
        argv.push(current);
    }
    if argv.is_empty() {
        bail!("command cannot be empty");
    }
    Ok(argv)
}

pub async fn run_command(
    tool: Arc<CommandTool>,
    command: String,
    argv: Vec<String>,
) -> TerminalEntry {
    let output = tool
        .execute(ToolInvocation {
            call_id: format!("terminal-{command}"),
            name: "run_command".into(),
            arguments: serde_json::json!({ "argv": argv }),
        })
        .await;

    match output {
        Ok(result) => TerminalEntry {
            command,
            output: result.output,
            ok: result.ok,
        },
        Err(error) => TerminalEntry {
            command,
            output: error.to_string(),
            ok: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::tools::fs::ScopedFs;
    use std::collections::BTreeSet;

    #[test]
    fn parses_direct_argv_with_quotes_and_rejects_unterminated_quotes() {
        let argv = parse_argv(
            r#"/bin/echo "hello world" 'fixed string' escaped\ space "quoted \"escape\"""#,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "/bin/echo".to_string(),
                "hello world".to_string(),
                "fixed string".to_string(),
                "escaped space".to_string(),
                r#"quoted "escape""#.to_string(),
            ]
        );

        assert!(parse_argv("").is_err());
        assert!(parse_argv("   ").is_err());
        assert!(parse_argv(r#"/bin/echo "unterminated"#).is_err());
        assert!(parse_argv(r#"/bin/echo 'unterminated"#).is_err());
    }

    #[test]
    fn history_is_bounded_and_newest_first() {
        let mut history = TerminalHistory::new(2);
        history.push(TerminalEntry {
            command: "first".into(),
            output: "one".into(),
            ok: true,
        });
        history.push(TerminalEntry {
            command: "second".into(),
            output: "two".into(),
            ok: true,
        });
        history.push(TerminalEntry {
            command: "third".into(),
            output: "three".into(),
            ok: false,
        });

        assert_eq!(history.entries().len(), 2);
        assert_eq!(history.entries()[0].command, "third");
        assert_eq!(history.entries()[1].command, "second");
    }

    #[tokio::test]
    async fn runs_one_allowlisted_command_in_the_selected_space() {
        let root = tempfile::tempdir().unwrap();
        let policy = harness_core::tools::shell::ShellPolicy {
            timeout_ms: 5_000,
            max_output_bytes: 16 * 1024,
            allowed_binaries: BTreeSet::from(["/bin/echo".to_string()]),
        }
        .canonicalize()
        .unwrap();
        let tool = Arc::new(CommandTool::new(
            Arc::new(ScopedFs::new(root.path()).unwrap()),
            policy,
        ));
        let command = r#"/bin/echo "terminal dock ok""#;
        let entry = run_command(tool, command.to_string(), parse_argv(command).unwrap()).await;

        assert!(entry.ok);
        assert_eq!(entry.command, command);
        assert!(entry.output.contains("terminal dock ok"));
    }
}
