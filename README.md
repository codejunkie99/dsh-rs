# DeepSeek Harness RS

A native Rust + GPUI desktop milestone for the DeepSeek Harness agent model. It intentionally contains no Electron, Chromium, Node.js, web server, or browser bridge.

## Current capabilities

- GPU-accelerated native macOS interface built with GPUI 0.2.2.
- Append-only, replayable JSONL session logs.
- Session sidebar with model, event count, and active-turn state.
- Transcript projection for user, assistant, tool, and system events.
- Adapter-driven turn/step agent loop.
- DeepSeek OpenAI-compatible streaming adapter with environment and private credential-file key resolution.
- Tool registry and echo tool execution path.
- Read/list filesystem tools confined to `~/.dsh-rs/workspace`; traversal and symlink escapes are rejected.
- Durable fail-closed approval policy with native GPUI Approve/Deny prompts for `ask` tools.
- Responsive turn cancellation with a durable cancelled turn event and native Cancel control.
- Optional direct-execution shell tool with canonical binary allowlist, cleared environment, workspace cwd, timeout, and bounded output.
- Durable session reopening and corrupt-session isolation.
- Keyboard text editing with clipboard, selection, cursor, and IME hooks.
- Built-in local null adapter for deterministic, network-free verification.

## Build

```sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

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

## Model credentials

Set a key in the environment:

```sh
export DEEPSEEK_API_KEY="..."
```

Or create a private credential file:

```sh
mkdir -p ~/.dsh-rs
printf 'api_key = ...\n' > ~/.dsh-rs/credentials
chmod 600 ~/.dsh-rs/credentials
```

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

- the canonical `~/.dsh-rs/workspace` as cwd
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

- Remote streaming is implemented, but request retries/rate-limit policy and credential management UI are not implemented yet.
- The editor is a focused native single-line chat input, not a full multiline code editor.
- Read/list/write filesystem tools are scoped and enabled. Shell execution is direct-execution and explicitly allowlisted, not a general `/bin/sh` sandbox. OS-level sandbox profiles remain future work.
