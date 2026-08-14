use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::runtime::Runtime;

const GIT_BIN: &str = "/usr/bin/git";
const SCAN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DIFF_LINES: usize = 500;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Meta,
    Hunk,
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub staged: bool,
    pub lines: Vec<DiffLine>,
    pub truncated: bool,
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

    pub fn parse_diff(raw: &str) -> FileDiff {
        let mut lines = Vec::new();
        let mut truncated = false;
        for line in raw.lines() {
            if lines.len() == MAX_DIFF_LINES {
                truncated = true;
                break;
            }
            let kind = if line.starts_with("diff --git")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("\\ No newline")
            {
                DiffLineKind::Meta
            } else if line.starts_with("@@") {
                DiffLineKind::Hunk
            } else if line.starts_with('+') {
                DiffLineKind::Addition
            } else if line.starts_with('-') {
                DiffLineKind::Deletion
            } else {
                DiffLineKind::Context
            };
            lines.push(DiffLine {
                kind,
                content: line.to_string(),
            });
        }
        FileDiff {
            path: String::new(),
            staged: false,
            lines,
            truncated,
        }
    }

    fn validate_repo_path(path: &str) -> Result<&str, String> {
        if path.is_empty() || path.contains('\0') {
            return Err("Git diff path is empty".into());
        }
        let repository_path = Path::new(path);
        if repository_path.is_absolute()
            || repository_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("Git diff path escapes the repository: {path}"));
        }
        Ok(path)
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

    pub fn diff_blocking(
        root: impl AsRef<Path>,
        path: &str,
        staged: bool,
    ) -> Result<FileDiff, String> {
        let path = Self::validate_repo_path(path)?.to_string();
        let runtime = Runtime::new()
            .map_err(|error| format!("failed to construct Git diff runtime: {error}"))?;
        let raw = runtime.block_on(Self::diff_raw(root.as_ref(), &path, staged))?;
        let mut diff = Self::parse_diff(&raw);
        diff.path = path;
        diff.staged = staged;
        Ok(diff)
    }

    async fn diff_raw(root: &Path, path: &str, staged: bool) -> Result<String, String> {
        if staged {
            return Self::git(root, &["diff", "--cached", "--", path]).await;
        }

        let worktree = Self::git(root, &["diff", "--", path]).await?;
        if !worktree.trim().is_empty() {
            return Ok(worktree);
        }

        let tracked = Self::git(root, &["ls-files", "--error-unmatch", "--", path]).await;
        if tracked.is_ok_and(|files| !files.trim().is_empty()) {
            return Ok(worktree);
        }

        let absolute = root.join(path);
        let absolute = absolute.to_string_lossy().into_owned();
        Self::git_with_success_codes(
            root,
            &["diff", "--no-index", "--", "/dev/null", &absolute],
            &[0, 1],
        )
        .await
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
        Self::git_with_success_codes(root, args, &[0]).await
    }

    async fn git_with_success_codes(
        root: &Path,
        args: &[&str],
        success_codes: &[i32],
    ) -> Result<String, String> {
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
        let status_code = output.status.code().unwrap_or(-1);
        if success_codes.contains(&status_code) {
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

    #[test]
    fn parses_unified_diff_lines() {
        let raw = "\
            diff --git a/example.rs b/example.rs\n\
            index 1234567..89abcde 100644\n\
            --- a/example.rs\n\
            +++ b/example.rs\n\
            @@ -1,3 +1,4 @@\n\
             context\n\
            -old\n\
            +new\n\
            \\ No newline at end of file\n";

        let diff = WorkspaceChanges::parse_diff(raw);

        assert_eq!(diff.lines.len(), 9);
        assert_eq!(diff.lines[0].kind, DiffLineKind::Meta);
        assert_eq!(diff.lines[4].kind, DiffLineKind::Hunk);
        assert_eq!(diff.lines[5].kind, DiffLineKind::Context);
        assert_eq!(diff.lines[6].kind, DiffLineKind::Deletion);
        assert_eq!(diff.lines[7].kind, DiffLineKind::Addition);
        assert_eq!(diff.lines[8].kind, DiffLineKind::Meta);
        assert!(!diff.truncated);
    }

    #[test]
    fn bounds_large_diff_rendering() {
        let raw: String = (0..501).map(|index| format!("+line-{index}\n")).collect();

        let diff = WorkspaceChanges::parse_diff(&raw);

        assert_eq!(diff.lines.len(), 500);
        assert!(diff.truncated);
    }

    #[test]
    fn diff_paths_cannot_escape_the_repository() {
        let root = tempfile::tempdir().unwrap();
        assert!(WorkspaceChanges::diff_blocking(root.path(), "", false).is_err());
        assert!(WorkspaceChanges::diff_blocking(root.path(), "/absolute", false).is_err());
        assert!(WorkspaceChanges::diff_blocking(root.path(), "../escape", false).is_err());
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

    #[test]
    fn reads_real_staged_worktree_and_untracked_diffs() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--initial-branch=main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "DSH Test"]);
        std::fs::write(root.path().join("tracked.txt"), "one\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-m", "initial"]);

        std::fs::write(root.path().join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.path().join("staged.txt"), "staged\n").unwrap();
        std::fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
        git(root.path(), &["add", "staged.txt"]);

        let staged = WorkspaceChanges::diff_blocking(root.path(), "staged.txt", true).unwrap();
        let worktree = WorkspaceChanges::diff_blocking(root.path(), "tracked.txt", false).unwrap();
        let untracked =
            WorkspaceChanges::diff_blocking(root.path(), "untracked.txt", false).unwrap();

        assert!(staged
            .lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Addition && line.content.contains("staged")));
        assert!(worktree
            .lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Addition && line.content.contains("two")));
        assert!(untracked
            .lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Addition && line.content.contains("untracked")));
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
