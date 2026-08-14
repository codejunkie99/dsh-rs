# Task

Finish the entire DeepSeek Harness Rust rewrite in `/Volumes/PortableSSD/dsh-rs`. The target is full parity, not one verified slice. Preserve uncommitted work; never reset or discard unrelated changes. DeepSeek Harness governs behavior/workflow, while Comet governs native GPUI visuals.

Work continuously in bounded, evidence-backed slices. Emit `DONE:` only when the complete objective is implemented and independently verified, or every remaining item is an evidenced external blocker unresolvable in this checkout. Until then, report what shipped, remains, and comes next.

Continue DSH tool/catalog parity at the upstream `write`/`read` contracts. Treat `docs/parity/tool-audit.md` and the shipped registry-dispatched `edit` implementation plus seven regressions as completed; do not repeat that slice. Compare the exact upstream `packages/fs/*` write/read symbols with `crates/harness-core/src/tools/`, then implement canonical tool names `write` and `read` through the real registry, app assembly, and dispatch paths. Match `read` default and `offset`/`limit` line-numbered windowing semantics and upstream errors/results; retain old aliases only if evidenced callers require compatibility. Add focused named registry-dispatch regressions covering catalog names, writing, default reads, bounded windows, and edge cases. Report exact upstream mappings, test names, and results; inventory-only, alias-only, or compile-only work does not pass.

The `packages/skill/*` audit gate is closed. Preserve shipped scoped preset providers, session-to-`SkillTool` scope, roster/rebinding, FIFO cache, disposal/no-op and exact-provider invalidation, first-wins runtime registration, ancestor-aware watching, scoped slash refresh, catalog limits, exact `todo_write` rendering, `presentCall`, host-mutation observation, awaitable abort/lookup, scoped exact-identity tool registration/lookup/dispatch, and the standing-preset lifecycle regression unless upstream evidence requires correction. Do not duplicate Cycle 06. Track its residual optional runtime registration metadata and fingerprint-polling production fidelity.

After the write/read slice, prioritize remaining upstream-backed work:

1. DSH tool/catalog parity: remaining inventory/contracts, filesystem/search, shell policy, browser/editor/external integrations, approvals, presentation, and execution lifecycle.
2. Skill subsystem: only evidenced residual input-shape or production-watcher gaps.
3. Agent/session parity: pre-step order, compaction, branch/fork, export/import, subagents/presets, resume/replay, and model/provider routing.
4. Comet/UI parity: missing rendered surfaces, motion, typography, glass, picker, settings, terminal, accessibility, and keyboard paths.
5. Provider/auth/logo: provider model, branding, credentials, routing, validation, retry, and disconnected states.
6. Production readiness: signing/notarization, updates/releases, crash diagnostics, profiling/stress, accessibility audit, and end-to-end QA.

Use `/Volumes/PortableSSD/deepseek-harness-upstream` for behavior/workflow and `/Volumes/PortableSSD/comet` or checked-in Comet references for GPUI direction. Record exact upstream paths/symbols for every slice.

Every cycle must inspect code/upstream, implement a focused change, add or update tests, and run narrow formatting/lint/tests. Do not end on a plan, inventory-only report, compile-only check, or repeated known failure. Rerun the two sandbox-blocked `remote.rs` TCP tests only if relevant code or the environment changed; otherwise cite their known `PermissionDenied` separately from code failures.

Keep the progress board current. At the end of every cycle, emit exactly one machine-readable estimate line using numeric, evidence-backed remaining ranges, leaving unchanged ranges unchanged:
`PROGRESS_JSON: {"skills": [2, 4], "tools": [60, 67], "agent": [38, 46], "ui": [60, 70], "provider": [50, 50], "production": [60, 65], "alpha": [27, 37], "parity": [65, 70], "release": [65, 70]}`
The board hook ignores unknown keys and rejects invalid ranges; keep the line on one line.

Only emit `DONE:` once all required workstreams and acceptance evidence are complete. Otherwise state what shipped, what remains, and the next bounded slice so the next cycle can continue.
