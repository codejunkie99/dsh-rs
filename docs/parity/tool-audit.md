# DSH tool/catalog parity inventory

Scope: upstream `packages/fs/*`, `packages/shell/*`, `packages/core/tools/*`
mapped against `crates/harness-core/src/tools/`. Classification is
`implemented`, `partial`, or `missing`, with the exact upstream path and the
Rust mapping when one exists.

## Core tool runtime (`packages/core/tools`)

| Upstream symbol | Upstream path | Status | Rust mapping |
|---|---|---|---|
| `ToolRuntime` register/get/view/schemas/execute | `src/index.ts` | partial | `tools::ToolRegistry` (`register`, `register_for_scope`, `get`, `specs_for`, `execute_with_session`) |
| `ToolDefinition` / `defineTool` | `src/types.ts`, `src/schema.ts` | partial | `tools::Tool` trait (`spec`, `execute`, `present_call`) |
| `ToolCallKind` vocabulary | `src/presentation.ts` | implemented | `tools::ToolCallKind` |
| `ToolCallView` (Generic/Terminal/Diff) | `src/presentation.ts` | partial | `tools::ToolCallView` has only `Generic`; `Terminal` and `Diff` arms missing |
| `ToolResultView` (Generic/Terminal/Diff/Search/Read/Web) | `src/presentation.ts` | missing | no `present_result` on `Tool`; no result-view types |
| `run_code` Code Mode transport (`RUN_CODE_NAME`) | `src/code-mode.ts` | missing | no code runtime bridge |
| `executionMode` / scheduler | `src/index.ts` | missing | no execution-mode classification |

## Filesystem tools (`packages/fs`)

| Upstream tool | Upstream path | Status | Rust mapping |
|---|---|---|---|
| `edit` (literal, unique-match, `replace_all`) | `tool-fs/src/edit.ts`, `fs-local/src/fsio.ts:applyLiteralEdit` | implemented | `tools::fs::EditFileTool` |
| `write` (`file_path`, `content`, sandbox fields) | `tool-fs/src/write.ts` | partial | `tools::fs::WriteFileTool` (`write`: canonical name/schema; sandbox escalation and structured diff metadata remain) |
| `read` (`file_path`, `offset`, `limit`, line-numbered window) | `tool-fs/src/read.ts`, `tool-fs/src/read-render.ts` | partial | `tools::fs::ReadFileTool` (`read`: canonical schema, 1-based windows, line numbers, 2,000-line/2,000-char/50 KiB caps; streaming and structured result metadata remain) |
| `read_image` (conditional on attachments) | `tool-fs/src/read-image.ts` | missing | none |
| `glob` (`tool-fs-search/src/glob.ts`) | `tool-fs-search/src/glob.ts` | partial | `tools::search::GlobTool` (scoped recursive matcher, VCS exclusion, 100-result cap; sampling/spill persistence remain) |
| `grep` (`tool-fs-search/src/grep.ts`) | `tool-fs-search/src/grep.ts` | partial | `tools::search::GrepTool` (scoped bounded line matches and include glob; ripgrep regex/spill persistence remain) |
| `str_replace_editor` | `tool-str-replace-editor/src/index.ts` | missing | none |
| `fs/observed` + read-before-write guard | `fs-observation-policy/src/index.ts` | missing | skill provider has `observe_fs_event`; tool-level `fs/observed` emission absent |
| sandbox escalation (`sandbox_permissions`, `justification`) | `tool-fs/src/sandbox.ts`, `fs-sandbox/src/containment.ts` | missing | `ScopedFs` is root-confined; no escalation fields |
| `list_dir` | no upstream equivalent | extra | `tools::fs::ListDirTool` (no upstream counterpart; upstream uses `read`/`glob`) |

## Shell tools (`packages/shell`)

| Upstream tool | Upstream path | Status | Rust mapping |
|---|---|---|---|
| `bash` (foreground shell, background, terminal card) | `tool-bash/src/index.ts` | partial | `tools::shell::CommandTool` (`run_command`: direct-exec allowlist, no shell, no background, no terminal card) |
| `bash` persistent | `tool-bash-persistent/src/index.ts` | missing | none |
| `pwsh` | `tool-pwsh/src/index.ts` | missing | none |
| shell render/terminal result view | `shell/src/render.ts` | missing | no terminal result card |

## Implemented-this-cycle notes

`edit` was the highest-confidence functional mismatch: it is a pure
literal-text replacement with no external dependency, and was entirely absent.
It now matches upstream `applyLiteralEdit` semantics: CRLF-collapse matching,
0-match `old_string was not found ...`, ambiguous-match
`old_string matched N times ...; provide a more specific old_string or set
replace_all to true`, `old_string === new_string` rejection, atomic write-back
with line-ending restoration, and the exact `The file ... has been updated
successfully.` / `All occurrences were successfully replaced.` result strings.
Regression: `crates/harness-core/tests/tool_edit.rs`.

Remaining highest-confidence gaps, in order: write/read sandbox escalation, structured diff/read result metadata, `glob`/`grep`, then `bash` shell semantics and the missing result-view presentation surface.


## Write/read parity slice

Compared with upstream `packages/fs/tool-fs/src/read.ts`, `read-render.ts`, and `write.ts`:

- Canonical model-facing names are now `read` and `write`; the previous `read_file` and `write_file` names are no longer advertised.
- `read` accepts required `file_path`, optional 1-based `offset` (default 1), and optional positive `limit` (default/max 2,000).
- `read` returns the upstream envelope shape with `N: line` numbering, continuation/end-of-file footers, CRLF normalization, 2,000-character line truncation, and a 50 KiB selected-output cap.
- `write` accepts `file_path` and `content`, performs the existing atomic scoped write, and returns the upstream `<path>/<type>/<content>` confirmation envelope with Created/Updated wording.
- Registry and app assembly use the canonical names; regressions cover schemas, catalog membership, dispatch, bounded windows, invalid windows, and out-of-range offsets in `crates/harness-core/tests/tool_fs.rs` and `crates/harness-core/src/tools/fs.rs`.

Remaining in this area: sandbox escalation fields/policy, structured before/after and read metadata for UI/replay, streaming large reads, `read_image`, search tools, and terminal/result presentation parity.
