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
| `write` (`file_path`, `content`, sandbox fields) | `tool-fs/src/write.ts` | partial | `tools::fs::WriteFileTool` (`write_file`: name mismatch, different result envelope) |
| `read` (`file_path`, `offset`, `limit`, line-numbered window) | `tool-fs/src/read.ts` | partial | `tools::fs::ReadFileTool` (`read_file`: name mismatch, no windowing/line numbering) |
| `read_image` (conditional on attachments) | `tool-fs/src/read-image.ts` | missing | none |
| `glob` (`tool-fs-search/src/glob.ts`) | `tool-fs-search/src/glob.ts` | missing | none |
| `grep` (`tool-fs-search/src/grep.ts`) | `tool-fs-search/src/grep.ts` | missing | none |
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

Remaining highest-confidence gaps, in order: `write`/`read` name + contract
alignment, `read` windowing/line numbering, `glob`/`grep`, then `bash`
shell semantics and the missing result-view presentation surface.
