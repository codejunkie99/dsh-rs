# Native GPUI Harness Design

## Goal

Replace the Electron/web direction with one native Rust desktop application built with GPUI. The first milestone is a usable native chat surface backed by the same event-sourced model as DeepSeek Harness: append-only session logs, transcript projection, a tool registry, and an adapter-driven agent loop.

This milestone is not feature parity with upstream DeepSeek Harness. It establishes the native desktop shell and the durable core that later model providers, tools, and approval policies plug into.

## Architecture

- `crates/harness-core` is a UI-independent Rust library. It owns session events, JSONL persistence, transcript projection, LLM streaming traits, tools, and the turn/step agent loop.
- `crates/dsh-app` is the sole desktop executable. It renders with GPUI 0.2.2 and calls `harness-core` directly. There is no Electron, Chromium, Node runtime, HTTP server, or browser bridge.
- Sessions live under a local harness home (`~/.dsh-rs/sessions/<uuid>.jsonl` by default). The append-only log remains the source of truth; the UI is a projection and can be reconstructed after restart.
- The initial provider is `NullAdapter`, which completes one deterministic assistant response without network access. This keeps the app testable before credentials or a DeepSeek-compatible API adapter are added.
- The initial tool is `EchoTool`, proving the call/result path and allowing a later tool loop to run without privileged filesystem access.

## Desktop flow

1. On launch, the app loads existing JSONL sessions from the harness home.
2. The sidebar lists session title, model, event count, and turn state.
3. Selecting a session projects its event log into the transcript pane.
4. Submitting input appends durable events through `AgentLoop::run_turn`.
5. While a turn runs, the input remains available but submit is disabled.
6. A completed turn refreshes the transcript from the log, never from ephemeral UI state.

## Error handling

- Corrupt or unreadable session files are skipped with an entry in the app status area; they do not prevent other sessions from loading.
- Errors returned by an adapter or tool become durable `error_noted` and `tool_result` events where appropriate.
- A failed turn closes with `TurnCompletionReason::Errored`; the app remains responsive and the session remains reopenable.

## Verification

- `cargo fmt --all`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo build --release`
- Launch `target/release/dsh-app` and verify window creation, session creation, input submission, transcript rendering, and persistence after relaunch.

## Smaller and faster

- Single native executable.
- No web server, browser process, React tree, or Node dependency.
- JSONL session storage avoids a database service while preserving replay.
- Release builds use LTO, one codegen unit, and symbol stripping.
