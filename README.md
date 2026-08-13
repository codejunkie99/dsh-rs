# DeepSeek Harness RS

A native Rust + GPUI desktop milestone for the DeepSeek Harness agent model. It intentionally contains no Electron, Chromium, Node.js, web server, or browser bridge.

## Current capabilities

- GPU-accelerated native macOS interface built with GPUI 0.2.2.
- Append-only, replayable JSONL session logs.
- Session sidebar with model, event count, and active-turn state.
- Transcript projection for user, assistant, tool, and system events.
- Adapter-driven turn/step agent loop.
- Tool registry and echo tool execution path.
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

## Runtime notes

This build uses GPUI's `runtime_shaders` feature so it can build and run with the Command Line Tools renderer path. Full Xcode can precompile GPUI's Metal pipeline, but it is not required for this bundle.

## Current limits

- The production model adapter is intentionally not wired to a remote API yet; the local adapter keeps the app deterministic until credentials and request policy are added.
- The editor is a focused native single-line chat input, not a full multiline code editor.
- The tool surface contains the echo tool only; filesystem, shell, and approval seams remain future adapter work.
