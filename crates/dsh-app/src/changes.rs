use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::runtime::Runtime;

const GIT_BIN: &str = "/usr/bin/git";
const SCAN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
    pub staged: bool,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceChanges {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<ChangedFile>,
    pub total_additions: Option<u64>,
    pub total_deletions: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceChangesState {
    NotRepository,
    Ready(WorkspaceChanges),
    Failed(String),
}

impl WorkspaceChanges {
    pub fn parse(status: &str, staged_stats: &str, unstaged_stats: &str) -> Self {
        let mut changes = Self::default();
        for line in status.lines() {
            if let Some(head) = line.strip_prefix("# branch.head ") {
                let head = head.trim();
                if head != "(detached)" {
                    changes.branch = Some(head.to_string());
                }
            } else if let Some(tracking) = line.strip_prefix("# branch.ab ") {
                for value in tracking.split_whitespace() {
                    if let Some(value) = value.strip_prefix('+') {
                        changes.ahead = value.parse().unwrap_or(0);
                    } else if let Some(value) = value.strip_prefix('-') {
                        changes.behind = value.parse().unwrap_or(0);
                    }
                }
            } else if let Some(files) = Self::parse_status_line(line) {
                changes.files.extend(files);
            }
        }

        let staged = Self::parse_numstat(staged_stats);
        let unstaged = Self::parse_numstat(unstaged_stats);
        for file in &mut changes.files {
            let stats = if file.staged {
                staged.get(&file.path)
            } else {
                unstaged.get(&file.path)
            };
            if let Some((additions, deletions)) = stats {
                file.additions = *additions;
                file.deletions = *deletions;
            }
        }

        let mut numeric = false;
        let mut additions = 0;
        let mut deletions = 0;
        for file in &changes.files {
            if let Some(value) = file.additions {
                additions += value;
                numeric = true;
            }
            if let Some(value) = file.deletions {
                deletions += value;
                numeric = true;
            }
        }
        if numeric {
            changes.total_additions = Some(additions);
            changes.total_deletions = Some(deletions);
        }
        changes
    }

    fn parse_status_line(line: &str) -> Option<Vec<ChangedFile>> {
        let marker = line.split(' ').next()?;
        match marker {
            "?" => {
                let path = line.split_once(' ')?.1.trim();
                Some(vec![ChangedFile {
                    path: path.to_string(),
                    status: ChangeStatus::Untracked,
                    staged: false,
                    additions: None,
                    deletions: None,
                }])
            }
            "1" => {
                let fields = line.splitn(9, ' ').collect::<Vec<_>>();
                if fields.len() != 9 {
                    return None;
                }
                Self::files_from_codes(fields[8].trim(), fields[1])
            }
            "2" => {
                let fields = line.splitn(10, ' ').collect::<Vec<_>>();
                if fields.len() != 10 {
                    return None;
                }
                Self::files_from_codes(fields[9].trim(), fields[1])
            }
            "u" => {
                let fields = line.splitn(10, ' ').collect::<Vec<_>>();
                if fields.len() != 10 {
                    return None;
                }
                Some(vec![ChangedFile {
                    path: fields[9].trim().to_string(),
                    status: ChangeStatus::Unmerged,
                    staged: false,
                    additions: None,
                    deletions: None,
                }])
            }
            _ => None,
        }
    }

    fn files_from_codes(path: &str, codes: &str) -> Option<Vec<ChangedFile>> {
        let index = codes.chars().next()?;
        let worktree = codes.chars().nth(1)?;
        let mut files = Vec::new();
        for (code, staged) in [(index, true), (worktree, false)] {
            if code == '.' {
                continue;
            }
            files.push(ChangedFile {
                path: path.to_string(),
                status: Self::status(code)?,
                staged,
                additions: None,
                deletions: None,
            });
        }
        Some(files)
    }

    fn status(code: char) -> Option<ChangeStatus> {
        match code {
            'A' => Some(ChangeStatus::Added),
            'M' => Some(ChangeStatus::Modified),
            'D' => Some(ChangeStatus::Deleted),
            'R' => Some(ChangeStatus::Renamed),
            'C' => Some(ChangeStatus::Copied),
            'T' => Some(ChangeStatus::TypeChanged),
            'U' => Some(ChangeStatus::Unmerged),
            _ => None,
        }
    }

    fn parse_numstat(raw: &str) -> HashMap<String, (Option<u64>, Option<u64>)> {
        let mut stats = HashMap::new();
        for line in raw.lines() {
            let mut fields = line.splitn(3, '\t');
            let Some(additions) = fields.next().map(Self::numstat_value) else {
                continue;
            };
            let Some(deletions) = fields.next().map(Self::numstat_value) else {
                continue;
            };
            let Some(path) = fields.next().map(str::trim) else {
                continue;
            };
            stats.insert(path.to_string(), (additions, deletions));
        }
        stats
    }

    fn numstat_value(value: &str) -> Option<u64> {
        if value == "-" {
            None
        } else {
            value.parse().ok()
        }
    }

    pub async fn scan(root: impl AsRef<Path>) -> WorkspaceChangesState {
        let root = root.as_ref().to_path_buf();
        match Self::git(&root, &["rev-parse", "--is-inside-work-tree"]).await {
            Ok(output) if output.trim() != "true" => WorkspaceChangesState::NotRepository,
            Ok(_) => Self::scan_repository(&root).await,
            Err(error) if error.to_lowercase().contains("not a git repository") => {
                WorkspaceChangesState::NotRepository
            }
            Err(error) => WorkspaceChangesState::Failed(error),
        }
    }

    pub fn scan_blocking(root: impl AsRef<Path>) -> WorkspaceChangesState {
        let runtime = match Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                return WorkspaceChangesState::Failed(format!(
                    "failed to construct Git scan runtime: {error}"
                ))
            }
        };
        runtime.block_on(Self::scan(root))
    }

    async fn scan_repository(root: &Path) -> WorkspaceChangesState {
        let commands: [(&str, &[&str]); 3] = [
            (
                "status",
                &[
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "--untracked-files=all",
                ],
            ),
            ("staged diff", &["diff", "--cached", "--numstat"]),
            ("unstaged diff", &["diff", "--numstat"]),
        ];
        let mut outputs = Vec::new();
        for (label, args) in commands {
            match Self::git(root, args).await {
                Ok(output) => outputs.push(output),
                Err(error) => return WorkspaceChangesState::Failed(format!("{label}: {error}")),
            }
        }
        let mut changes = Self::parse(&outputs[0], &outputs[1], &outputs[2]);
        changes.root = root.to_path_buf();
        WorkspaceChangesState::Ready(changes)
    }

    async fn git(root: &Path, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(GIT_BIN);
        command
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(root)
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("core.pager=cat")
            .args(args)
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("HOME", root);
        let output = tokio::time::timeout(SCAN_TIMEOUT, command.output())
            .await
            .map_err(|_| format!("git {} timed out", args[0]))?
            .map_err(|error| format!("git {} failed: {error}", args[0]))?;
        if output.status.success() {
            String::from_utf8(output.stdout)
                .map_err(|_| format!("git {} returned invalid UTF-8", args[0]))
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_branch_tracking_and_changed_files() {
        let status = "\
            # branch.oid 0123456789012345678901234567890123456789\n\
            # branch.head feature/test\n\
            # branch.upstream origin/main\n\
            # branch.ab +2 -1\n\
            1 M. N... 100644 100644 100644 SHA SHA staged.txt\n\
            1 .M N... 100644 100644 100644 SHA SHA worktree.txt\n\
            1 MM N... 100644 100644 100644 SHA SHA both.txt\n\
            1 .M N... 100644 100644 100644 SHA SHA binary.bin\n\
            ? untracked.txt\n";
        let staged_stats = "1\t2\tstaged.txt\n3\t4\tboth.txt\n";
        let unstaged_stats = "5\t1\tworktree.txt\n-\t-\tbinary.bin\n7\t8\tboth.txt\n";

        let changes = WorkspaceChanges::parse(status, staged_stats, unstaged_stats);

        assert_eq!(changes.branch.as_deref(), Some("feature/test"));
        assert_eq!(changes.ahead, 2);
        assert_eq!(changes.behind, 1);
        assert_eq!(changes.files.len(), 6);
        assert_eq!(
            changes.files[0],
            ChangedFile {
                path: "staged.txt".into(),
                status: ChangeStatus::Modified,
                staged: true,
                additions: Some(1),
                deletions: Some(2),
            }
        );
        assert_eq!(
            changes.files[1],
            ChangedFile {
                path: "worktree.txt".into(),
                status: ChangeStatus::Modified,
                staged: false,
                additions: Some(5),
                deletions: Some(1),
            }
        );
        assert!(changes.files.iter().any(|file| file.path == "binary.bin"
            && file.additions.is_none()
            && file.deletions.is_none()));
        assert_eq!(changes.total_additions, Some(16));
        assert_eq!(changes.total_deletions, Some(15));
    }

    #[test]
    fn parses_detached_and_clean_repositories() {
        let changes =
            WorkspaceChanges::parse("# branch.oid abc\n# branch.head (detached)\n", "", "");

        assert_eq!(changes.branch, None);
        assert_eq!(changes.ahead, 0);
        assert_eq!(changes.behind, 0);
        assert!(changes.files.is_empty());
    }

    #[tokio::test]
    async fn scans_a_real_repository_without_modifying_it() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--initial-branch=main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "DSH Test"]);
        std::fs::write(root.path().join("tracked.txt"), "one\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-m", "initial"]);

        std::fs::write(root.path().join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.path().join("staged.txt"), "new\n").unwrap();
        std::fs::write(root.path().join("untracked.txt"), "next\n").unwrap();
        git(root.path(), &["add", "staged.txt"]);

        let state = WorkspaceChanges::scan(root.path()).await;
        let WorkspaceChangesState::Ready(changes) = state else {
            panic!("expected a ready repository scan");
        };

        assert_eq!(changes.branch.as_deref(), Some("main"));
        assert_eq!(changes.files.len(), 3);
        assert!(changes
            .files
            .iter()
            .any(|file| file.path == "tracked.txt" && !file.staged && file.additions == Some(1)));
        assert!(changes.files.iter().any(|file| file.path == "staged.txt"
            && file.staged
            && file.status == ChangeStatus::Added));
        assert!(changes
            .files
            .iter()
            .any(|file| file.path == "untracked.txt" && file.status == ChangeStatus::Untracked));
    }

    #[tokio::test]
    async fn reports_non_repositories_without_claiming_an_error() {
        let root = tempfile::tempdir().unwrap();
        let state = WorkspaceChanges::scan(root.path()).await;
        assert_eq!(state, WorkspaceChangesState::NotRepository);
    }

    #[test]
    fn blocking_scan_bridge_works_outside_a_tokio_runtime() {
        let root = tempfile::tempdir().unwrap();
        let state = WorkspaceChanges::scan_blocking(root.path());
        assert_eq!(state, WorkspaceChangesState::NotRepository);
    }
}
