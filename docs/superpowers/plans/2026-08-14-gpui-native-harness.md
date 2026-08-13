# GPUI Native Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native Rust + GPUI desktop milestone for DeepSeek Harness with event-sourced sessions and no Electron dependency.

**Architecture:** `harness-core` remains a pure library. A new `dsh-app` crate embeds it directly, loads JSONL sessions from a local harness home, renders a session sidebar/transcript/input with GPUI, and runs the adapter-driven turn loop as a background task. The temporary server crate is removed because the native milestone needs no HTTP process.

**Tech Stack:** Rust 1.97, GPUI 0.2.2, Tokio for the agent loop, Serde JSONL persistence, Cargo workspace.

---

### Task 1: Stabilize the core

**Files:**
- Modify: `crates/harness-core/src/events.rs`
- Modify: `crates/harness-core/src/session.rs`
- Modify: `crates/harness-core/src/agent.rs`
- Modify: `Cargo.toml`
- Delete: `crates/harness-server`

- [ ] Run `cargo test --workspace` and record compile/test failures.
- [ ] Fix serialization, borrowing, and state-transition defects without weakening tests.
- [ ] Remove the unused server crate and server-only workspace dependencies.
- [ ] Verify: `cargo fmt --all && cargo test --workspace`.

### Task 2: Add the session store

**Files:**
- Create: `crates/harness-core/src/store.rs`
- Modify: `crates/harness-core/src/lib.rs`

- [ ] Add `SessionStore::open(root)`, scanning only `*.jsonl` files.
- [ ] Add `SessionStore::create(title, model)`, writing `<uuid>.jsonl`.
- [ ] Add `SessionStore::list()`, returning sorted `SessionSummary` values.
- [ ] Add tests for create/load/reopen/skip-corrupt-file behavior in a temporary directory.
- [ ] Verify: `cargo test -p harness-core store`.

### Task 3: Scaffold the GPUI executable

**Files:**
- Create: `crates/dsh-app/Cargo.toml`
- Create: `crates/dsh-app/src/main.rs`
- Modify: `Cargo.toml`

- [ ] Add `dsh-app` as a workspace member and remove `harness-server`.
- [ ] Depend on `gpui = "0.2.2"` and `harness-core`.
- [ ] Open a centered 1200x760 window titled `DeepSeek Harness RS`.
- [ ] Render a non-empty dark root layout.
- [ ] Verify: `cargo check -p dsh-app`.

### Task 4: Build the chat workspace UI

**Files:**
- Create: `crates/dsh-app/src/workspace.rs`
- Create: `crates/dsh-app/src/input.rs`
- Modify: `crates/dsh-app/src/main.rs`

- [ ] Implement a 280 px session sidebar and main conversation column.
- [ ] Render session title/model/event count and a New Session button.
- [ ] Render user, assistant, tool, and error transcript entries.
- [ ] Implement a focused GPUI text input using `EntityInputHandler`; Enter submits and Shift+Enter inserts a newline.
- [ ] Disable submit while a turn is active.
- [ ] Verify manually after launch and with `cargo check -p dsh-app`.

### Task 5: Integrate durable turns

**Files:**
- Modify: `crates/dsh-app/src/workspace.rs`
- Modify: `crates/harness-core/src/store.rs`

- [ ] Store the selected `SharedSessionLog` and refresh views from `SessionLog::view()`.
- [ ] On submit, clone the log and spawn `AgentLoop::run_turn` on a background executor.
- [ ] On completion, update the GPUI entity from the persisted log.
- [ ] Preserve the input text if the turn fails, and clear it only after a successful append.
- [ ] Verify by quitting, relaunching, and confirming the transcript reloads from JSONL.

### Task 6: Release verification and snapshot

**Files:**
- Create: `README.md`
- Modify: `Cargo.toml` if release settings need adjustment

- [ ] Document build/run/data paths and current limitations.
- [ ] Run `cargo fmt --all`.
- [ ] Run `cargo test --workspace`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo build --release`.
- [ ] Launch the release binary, create a session, submit input, and relaunch to verify persistence.
- [ ] Record binary size and copy the full workspace plus archive to `/Volumes/PortableSSD` once that volume is mounted.
