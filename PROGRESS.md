# DeepSeek Harness Rust parity — progress board

> Remaining percentages are conservative engineering estimates, not completion proofs. This file is refreshed by `scripts/update-progress-board.py` after each loop cycle.

**Last update:** `2026-08-15T00:00:00Z`
**Loop:** cycle `1`, status `max-cycles`
**Latest slice:** Shipped one bounded, evidence-backed slice on DSH tool parity this cycle.

## Workstreams

| Workstream | Remaining | Baseline | Confidence | Status |
|---|---:|---:|---|---|
| Skill subsystem | **2–4%** | 50% | medium | in progress |
| DSH tool/catalog parity | **52–60%** | 70–75% | low | in progress |
| Agent/session parity | **38–46%** | 50–55% | medium | in progress |
| Comet/UI exact parity | **60–70%** | 60–70% | low | in progress |
| Provider/auth/logo surface | **50%** | 50% | low | in progress |
| Production readiness | **60–65%** | 60–65% | low | not started |

## Milestones

| Milestone | Remaining | Baseline | Confidence | Status |
|---|---:|---:|---|---|
| Useful native GPUI alpha | **27–37%** | 35–45% | low | in progress |
| Full DSH functional parity | **65–70%** | 70–75% | low | in progress |
| Exact UI/UX plus production release quality | **65–70%** | 70–75% | low | in progress |

### Skill subsystem

Shipped:
- Scoped provider registration and RAII disposal
- Filesystem watcher invalidation and registry observer events
- Scoped layers, chain-aware cache keys, and preset rebind seam
- Event-driven slash-menu snapshot refresh
- Shared cancellation flag between provider control and registration
- Exact-provider-identity stale-definition invalidation
- First-wins runtime duplicate registration
- End-to-end standing-preset slash-catalog regression
- Insertion-ordered FIFO collect-cache eviction (IndexMap)
- Silent no-op SkillProviderControl::invalidate after disposal
- Ancestor-aware filesystem watcher/polling fingerprints
Next:
- Audit any residual observer/cache, provider lifecycle, and watcher edge cases
- Verify scoped registration/recomposition/slash-refresh remain parity-clean
Evidence: `crates/harness-core/src/skills.rs`, `crates/harness-core/src/tools/skill.rs`, `crates/harness-core/tests/skill_subsystem.rs`

### DSH tool/catalog parity

Shipped:
- Skill tool catalog, invocation, visibility, and scoped execution contract
- Filesystem and shell policy foundations already present
- edit literal-replacement tool (EditFileTool): unique-match/replace_all, CRLF restore, upstream result strings
- canonical `read`/`write` schemas and registry/app dispatch
- bounded line-numbered `read` windows with offset/limit validation
- scoped `glob` and `grep` catalog tools with registry regressions
Next:
- Complete write/read sandbox escalation and structured result metadata
- Port remaining search caps/regex semantics and bash shell semantics
- Match approval and execution lifecycle semantics
- Cover browser, editor, external integration, and search behavior
Evidence: `crates/harness-core/src/tools/`, `crates/harness-core/tests/skill_prestep.rs`, `crates/harness-core/tests/tool_edit.rs`, `docs/parity/tool-audit.md`

### Agent/session parity

Shipped:
- Session-owned SkillScope
- AgentLoop-to-SkillTool scoped catalog and invocation path
- End-to-end preset recomposition test
Next:
- Assign scopes during real app session creation and reopen
- Match pre-step ordering and context compaction
- Cover fork, import/export, replay, and provider routing edge cases
Evidence: `crates/harness-core/src/session.rs`, `crates/harness-core/src/agent.rs`, `crates/harness-core/tests/skill_prestep.rs`

### Comet/UI exact parity

Shipped:
- Native GPUI primitives, theme, motion, picker, and workbench foundations
Next:
- Rendered screen-by-screen QA
- Motion, typography, glass, settings, picker, and terminal fidelity
- Design missing DSH surfaces in the Comet visual language
Evidence: `crates/dsh-app/src/`, `docs/parity/`

### Provider/auth/logo surface

Shipped:
- Provider branding and auth foundation
Next:
- DeepSeek Harness logo-based provider model
- Credential lifecycle, routing, validation, retry, and disconnected states
Evidence: `crates/dsh-app/src/settings/providers.rs`, `crates/harness-core/src/remote.rs`

### Production readiness

Next:
- Signing, notarization, installer, update, and release-channel hardening
- Crash reporting, diagnostics, profiling, and large-session stress tests
- Accessibility, keyboard navigation, and full end-to-end user-path QA

## Loop activity

| Cycle | Status | Summary | Recorded |
|---:|---|---|---|
| 2 | in progress | Shipped canonical read/write contracts, read windows, and scoped glob/grep dispatch; CI remains a verification gate. | 2026-08-15T00:00:00Z |
| 1 | max-cycles | Shipped one bounded, evidence-backed slice on DSH tool parity this cycle. | 2026-08-14T19:59:15Z |
| 7 | in progress | Status: Implemented the upstream `edit` tool (EditFileTool) through the real ToolRegistry dispatch path: literal unique-match/replace_all edit, CRLF normalize + line-ending restore, and upstream not-found/ambiguous/validation/result strings. Registered it in SUPPORTED_TOOLS (7→8) | 2026-08-14T19:55:19Z |
| 6 | max-cycles | Closed the skill audit gate with explicit evidence this cycle. | 2026-08-14T19:46:05Z |
| 6 | done | Shipped: closed the packages/skill/* audit gate with evidence, fixed the abort and provider-order mismatches, and added a full-chain end-to-end regression. | 2026-08-14T19:42:41Z |
| 5 | max-cycles | Shipped this cycle, all evidence-backed and tested. | 2026-08-14T19:25:41Z |
| 5 | done | Status: Closed the three named skill contracts (presentCall, observeHostMutation behind fs/observed, abort-observable control), the residual lookup-signal abort race, and shipped scoped/agent-aware ToolRegistry registration with exact-identity get(name, scope) replacing the skill | 2026-08-14T19:22:39Z |
| 4 | max-cycles | Two bounded slices landed this cycle, both evidence-backed and tested. | 2026-08-14T18:46:31Z |
| 10 | done | Status: Closed the skill config-surface gap and landed the first tool-parity contract correction. Shipped SkillTool catalogDescriptionMaxLength (default 500, min 3) matching upstream tool-skill Config, and fixed todo_write to render upstream's human-readable counts line instead o | 2026-08-14T18:43:16Z |
| 3 | max-cycles | The named watcher target is closed with focused regressions. Here's what shipped and where the rewrite stands. | 2026-08-14T18:35:23Z |
| 2 | max-cycles | Completed a focused, tested slice on the named watcher target. Three upstream-backed parity corrections landed in the skill registry, plus an end-to-end regression. | 2026-08-14T18:20:12Z |
| 8 | done | Status: Closed provider-lifecycle and cache-event parity gaps: shared cancellation flag between SkillProviderControl and SkillProviderRegistration, exact-provider-identity stale-definition invalidation (Arc::ptr_eq), and first-wins runtime duplicate registration. Added an end-to- | 2026-08-14T18:15:28Z |
| 1 | max-cycles | I've completed a focused, tested slice on the top-priority workstream. Here's the evidence-backed status. | 2026-08-14T18:03:46Z |

## Update contract

A loop runner can update estimates by emitting one line such as `PROGRESS_JSON: {"skills": [35, 40], "agent": [40, 45]}`. Unknown keys are ignored; invalid ranges are recorded as an error and do not overwrite the last estimate.
