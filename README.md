# DeepSeek Harness RS

A native Rust + GPUI desktop milestone for the DeepSeek Harness agent model. It intentionally contains no Electron, Chromium, Node.js, web server, or browser bridge.

## Current capabilities

- GPU-accelerated native macOS interface built on the exact pinned GPUI fork used by Comet.
- Comet-fidelity three-pane developer workbench: a 256 px spaces/sessions rail, unified 38 px titlebar, center transcript and composer, and a persisted 520 px context pane.
- Read-only Git changes scanning for the scoped workspace with branch, tracking, staged/worktree, line totals, and clickable per-file diffs.
- Append-only, replayable JSONL session logs.
- Automatic bounded session titles from the first user message.
- Session sidebar with model, event count, and active-turn state.
- Native sidebar search and rename fields.
- Session forking at the current boundary and Markdown export.
- Case-insensitive session title/transcript search.
- Transcript projection for user, assistant, tool, and system events.
- Adapter-driven turn/step agent loop.
- Durable system-prompt configuration with per-session prompt snapshots and deterministic model history projection.
- DeepSeek OpenAI-compatible streaming adapter with environment and private credential-file key resolution.
- Native masked API-key management with atomic owner-only storage and immediate model reload.
- Transient model-request retry handling for network errors, timeouts, HTTP 408/429, and 5xx responses.
- Live transcript and session-state refresh driven by append-only event notifications.
- Tool registry and echo tool execution path.
- Multiple configured spaces with durable per-session space associations. Read/list filesystem tools are confined to the selected space; traversal and symlink escapes are rejected.
- Keyboard space cycling with `Cmd+Shift+Right` and `Cmd+Shift+Left`.
- Durable harness setups with prompt, tool allowlist, and step-budget primitives. New sessions and forks preserve the selected setup.
- Keyboard harness cycling with `Cmd+Shift+Down` and `Cmd+Shift+Up`.
- Native bottom command dock for direct, allowlisted commands with bounded output and history.
- Durable fail-closed approval policy with native GPUI Approve/Deny prompts for `ask` tools.
- Responsive turn cancellation with a durable cancelled turn event and native Cancel control.
- Optional direct-execution shell tool with canonical binary allowlist, cleared environment, workspace cwd, timeout, and bounded output.
- Durable session reopening and corrupt-session isolation.
- Keyboard text editing with clipboard, selection, cursor, and IME hooks.
- Built-in local null adapter for deterministic, network-free verification.

## Progress

See [PROGRESS.md](PROGRESS.md) for the continuously refreshed parity board. The
loop runner updates it after each cycle through
`scripts/update-progress-board.py`; evidence-backed runners can change an
estimate with a one-line `PROGRESS_JSON:` record.

## Build

```sh
scripts/bootstrap-gpui-fork.sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

The bootstrap script downloads and checksum-verifies the pinned `wingleeio/zed` GPUI source snapshot used by the current Comet UI. It places the local source under ignored `vendor/`; the archive and source are not committed to the repository.

The release executable is `target/release/dsh-app`.

## Package

```sh
scripts/package-macos.sh
```

This creates `dist/DeepSeek Harness RS.app`. The bundle is ad-hoc signed for local execution. Replace the signing command with a Developer ID identity and provisioning profile before public distribution.

## Data

Sessions are stored in:

```text
~/.dsh-rs/sessions/<uuid>.jsonl
```

Each line is one typed session event. Delete the directory to reset local state. Corrupt files are skipped without preventing valid sessions from loading.

## Spaces

Spaces are defined in `~/.dsh-rs/spaces.json`. Roots must be existing absolute directories and are canonicalized before use:

```json
{
  "spaces": [
    {
      "id": "local",
      "name": "Local harness",
      "root": "/Users/example/.dsh-rs/workspace"
    },
    {
      "id": "project",
      "name": "Project",
      "root": "/Users/example/Projects/project"
    }
  ]
}
```

The first entry is the fail-safe default. Selecting a space records a durable `session_space_changed` event on the current session; new sessions and forks preserve the active space. Filesystem tools, shell cwd, and Git scanning all use that space's root. Invalid or empty configurations fall back to the local harness without widening filesystem scope.

Press `Cmd+Shift+Right` to activate the next space or `Cmd+Shift+Left` to activate the previous one. Space rows are also directly clickable.

## Harness Setups

Harness setups are defined in `~/.dsh-rs/harness-setups.json`. A setup is a native composition of three primitives: a system prompt, an explicit tool allowlist, and a bounded model step budget.

```json
{
  "setups": [
    {
      "id": "research",
      "name": "Research",
      "system_prompt": {
        "include_harness_identity": true,
        "persona": "Read the selected workspace and answer from evidence. Do not mutate files."
      },
      "enabled_tools": ["echo", "read_file", "list_dir"],
      "max_steps": 4
    }
  ]
}
```

Supported tool IDs are:

- `echo`
- `read_file`
- `list_dir`
- `write_file`
- `run_command`

The built-in `standard`, `research`, and `minimal` setups are written on first launch. Invalid, empty, duplicate, unknown-tool, or out-of-range configurations fail closed to those defaults. A customized `system-prompt.json` is migrated into the `standard` setup.

Selecting a setup records a durable `session_harness_changed` event. New sessions use the selected setup, session reopen restores it, and forks preserve it. The setup controls the tools exposed to the model, the prompt sent on the next turn, and the maximum number of model steps. `run_command` still requires the shell policy and approval policy below.

Press `Cmd+Shift+Down` to activate the next setup or `Cmd+Shift+Up` to activate the previous one. Setup rows are also directly clickable.

The sidebar search field filters titles and transcript content. Enter in the rename field updates the selected session title as a durable `session_title_changed` event.

## Workbench layout

The native shell uses a dense dark three-pane layout adapted from the MIT-licensed Comet UI direction:

- Left rail: spaces, session search, sessions, and model access.
- Center pane: unified session titlebar, transcript, tool approvals, composer, and reserved status strip.
- Right pane: selected-session model/event/turn/prompt state plus recent tool activity.
- Git changes: branch and ahead/behind state, bounded changed-file rows, staged/worktree labels, `git diff --numstat` line totals, and clickable unified diffs.

Pane visibility persists in `~/.dsh-rs/ui.json`. Press `Cmd+S` to toggle the left rail and `Cmd+B` to toggle the context pane.

## Command Dock

The bottom center dock runs one direct command at a time in the selected space:

- Press `Cmd+T` to show or hide the dock.
- Press `Cmd+Shift+T` to focus the command field.
- Type an allowlisted executable path plus direct arguments.
- Press `Enter` or click **Run**.
- Keep the newest 20 command results in bounded history.

For example, after adding `/bin/echo` to the shell policy below:

```text
/bin/echo "native command dock"
```

The dock uses the same canonical executable allowlist, selected-space cwd, empty environment, timeout, output cap, and kill-on-limit behavior as the model shell tool. Commands are parsed into direct argv values; no `/bin/sh` is invoked. This is a real direct-command workflow, but it is not yet an interactive PTY terminal.

The Git scanner is UI-only and read-only. It invokes `/usr/bin/git` directly with an empty environment, disables optional locks, fsmonitor, paging, and terminal prompts, and applies a five-second timeout. File paths are rejected if they are absolute or traverse outside the repository. Diff rendering is capped at 500 lines and long lines are truncated. Git data is not exposed to model tools by this scanner.

## System prompt

The config is stored in `~/.dsh-rs/system-prompt.json`:

```json
{
  "include_harness_identity": true,
  "persona": "Be precise and verify changes before claiming completion."
}
```

The rendered prompt is snapshotted into a session when it changes, so reopening and forking preserve the prompt state that governed the session. The next model request receives the latest snapshot as its single leading system message. Unknown fields and malformed JSON are rejected by the loader instead of being silently interpreted.

Fork copies all durable events through the selected boundary into a new session ID and JSONL file. Markdown exports are written to:

```text
~/.dsh-rs/exports/<session-id>.md
```

## Model credentials

Set a key in the environment:

```sh
export DEEPSEEK_API_KEY="..."
```

Or enter a key in the sidebar's **Model Access** field. Saving writes it atomically to:

```sh
~/.dsh-rs/credentials
```

The credential directory is tightened to `0700`, and the file is published with mode `0600`. The field masks and zeroizes its draft; saved keys are never echoed or written to session logs. Removing the stored key immediately returns the app to the local adapter. A nonempty `DEEPSEEK_API_KEY` in the launching environment remains the read-only override and disables in-app writes so a stored replacement cannot appear to take effect.

Optional settings:

```sh
export DEEPSEEK_MODEL="deepseek-chat"   # or deepseek-reasoner
export DEEPSEEK_BASE_URL="https://api.deepseek.com"
```

The app selects DeepSeek automatically when a valid credential exists and otherwise falls back to the deterministic local adapter. Credentials are redacted in formatting and never written to the session log.

## Tool approvals

The durable policy is stored in `~/.dsh-rs/approvals.json`:

```json
{
  "default": "deny",
  "tools": {
    "echo": "allow",
    "read_file": "allow",
    "list_dir": "allow",
    "write_file": "ask",
    "run_command": "ask"
  }
}
```

Rules are `allow`, `ask`, or `deny`. Unknown tools fail closed to `deny`. `ask` opens a native Approve/Deny prompt, and every request/resolution is recorded in the session event log. Invalid policy files are not guessed; the app falls back to its fail-closed defaults.

## Shell policy

Shell execution is disabled until `~/.dsh-rs/shell.json` allowlists at least one canonical absolute executable. The tool never invokes `/bin/sh`; it spawns the selected executable directly.

```json
{
  "timeout_ms": 10000,
  "max_output_bytes": 65536,
  "allowed_binaries": [
    "/bin/ls"
  ]
}
```

Commands run with:

- the canonical selected-space root as cwd
- an empty environment
- no inherited stdin
- piped and bounded stdout/stderr
- process timeout and kill-on-limit behavior
- the approval policy above

Absolute paths are canonicalized before matching. Relative paths, unlisted binaries, and unknown commands are rejected.

## Cancelling

Press `Cmd+.` or click **Cancel** while a turn is active. Cancellation is checked before model requests and while waiting for model streams. Cancelling also denies a pending tool approval. The session log records the turn with `reason=cancelled`, preserving any assistant chunks already appended before cancellation.

## Runtime notes

This build uses GPUI's `runtime_shaders` feature so it can build and run with the Command Line Tools renderer path. Full Xcode can precompile GPUI's Metal pipeline, but it is not required for this bundle.

## Current limits

- Remote streaming retries are implemented, but circuit-break telemetry is not implemented yet.
- The editor is a focused native single-line chat input, not a full multiline code editor.
- The command dock is direct-execution only; interactive PTY sessions and terminal multiplexing are future work.
- Harness setups are selectable and durable, but setup authoring is JSON-file based rather than a full visual editor.
- Read/list/write filesystem tools are scoped and enabled according to the selected setup. Shell execution is direct-execution and explicitly allowlisted, not a general `/bin/sh` sandbox. OS-level sandbox profiles remain future work.
