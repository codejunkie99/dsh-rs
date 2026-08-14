# `packages/skill/*` parity audit

This file maps every contract in the upstream DeepSeek Harness `packages/skill/`
tree to its exact upstream path/symbol, the Rust implementation in
`crates/harness-core`, and the named regression that proves parity. It is the
evidence gate required before resuming tool parity.

Upstream tree audited:

- `packages/skill/skill/src/index.ts` (registry)
- `packages/skill/skill-filesystem/src/index.ts` (filesystem provider)
- `packages/skill/tool-skill/src/index.ts` (model `skill` tool + catalog)
- `packages/skill/skill-badge/src/index.ts` (invariant helper only)

Rust surface audited:

- `crates/harness-core/src/skills.rs` (registry, scopes, filesystem provider, watcher)
- `crates/harness-core/src/tools/skill.rs` (`SkillTool`)
- `crates/harness-core/src/tools.rs` (`ToolRegistry`, `Tool`, `presentCall`)

## 1. Name and rank constants

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| Kebab-case name grammar | `skill/src/index.ts:22` `SKILL_NAME` | `skills.rs` `is_skill_name` | `runtime_registry_rejects_invalid_names_and_duplicate_providers` |
| Bundled rank | `skill/src/index.ts:28` `BUNDLED_SKILL_RANK = 600` | `skills.rs` `BUNDLED_SKILL_RANK` | `filesystem_registry_discovers_and_prioritizes_dsh_skill_roots` |
| Runtime rank | `skill/src/index.ts:20` `RUNTIME_RANK = 250` | `skills.rs` `RUNTIME_SKILL_RANK` | `runtime_skill_outranks_same_rank_provider_candidate` |
| Collect cache cap | `skill/src/index.ts:24` `DEFAULT_COLLECT_CACHE_ENTRIES = 128` | `skills.rs` `DEFAULT_COLLECT_CACHE_MAX_ENTRIES` | `collect_cache_evicts_in_insertion_order_and_respects_capacity` |
| Collect attempts | `skill/src/index.ts:25` `MAX_COLLECT_ATTEMPTS = 2` | `skills.rs` `MAX_COLLECT_ATTEMPTS` | `snapshot_marks_incomplete_observations_and_tolerates_provider_errors` |
| Reserved runtime provider | `skill/src/index.ts:23` `RUNTIME_PROVIDER = 'runtime'` | `skills.rs` `insert_provider`/`insert_provider_with_control` reserve `"runtime"` | `provider_registration_failure_and_reserved_names_publish_no_event` |

## 2. Scope model and layered shadowing

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| `ScopeKey` identity | `core/scope/src/index.ts` `ScopeKey` | `skills.rs` `SkillScope` (id-based identity) | `scoped_runtime_layers_shadow_globals_and_follow_recomposition` |
| Parent link | `core/scope/src/index.ts` `bindScopeParent` | `skills.rs` `SkillScope::bind_parent` | `child_scope_inherits_ancestor_registration` (tools) |
| Rebind seam | `core/scope/src/index.ts` `ScopeParentBinding.rebind` | `skills.rs` `ScopeParentBinding::rebind` | `session_resolved_skills_change_after_preset_recomposition` |
| Nearest-first chain | `core/scope/src/index.ts` `scopeChainOf` | `skills.rs` `SkillScope::chain` | `collect_fresh` layer order in `scoped_runtime_layers_shadow_globals_and_follow_recomposition` |
| Global → ancestor → exact shadow | `skill/src/index.ts:589` `collectFresh` | `skills.rs` `collect_fresh` | `scoped_filesystem_provider_shadows_a_global_provider_of_the_same_name` |

## 3. Provider lifecycle

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| Scoped registration | `skill/src/index.ts:387` `registerProvider` | `skills.rs` `register_provider_for_scope_with_control` | `standing_preset_slash_catalog_follows_session_scope_and_provider_lifecycle` |
| Shared abort signal | `skill/src/index.ts:273` `SkillProviderControl.signal` | `skills.rs` `SkillProviderControl::signal` + shared `AbortSignal` | `provider_control_signal_resolves_on_registration_disposal` |
| Awaitable abort | `skill/src/index.ts:843` `waitWithAbort` | `skills.rs` `wait_with_abort` + `AbortSignal::cancelled` | `lookup_stops_awaiting_a_hung_provider_when_aborted` |
| Silent no-op invalidate after disposal | `skill/src/index.ts:268` `SkillProviderControl.invalidate` | `skills.rs` `SkillProviderControl::invalidate` | `disposed_control_invalidate_is_a_silent_no_op` |
| Exact-provider invalidation | `skill/src/index.ts:673` `invalidateEntry` | `skills.rs` `invalidate_entry` (`Arc::ptr_eq`) | `stale_definition_load_invalidates_while_the_exact_provider_is_live` |
| Disposal publishes unregister | `skill/src/index.ts` effect disposer | `skills.rs` `unregister_provider` | `provider_control_observes_registration_disposal` |

## 4. Runtime registration

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| First-wins duplicate | `skill/src/index.ts:424` `register` | `skills.rs` `insert_runtime` | `runtime_duplicate_registration_is_first_wins` |
| Default invocation/provider | `skill/src/index.ts` `SkillRegistration` | `skills.rs` `insert_runtime` defaults | `runtime_registry_rejects_invalid_names_and_duplicate_providers` |
| Runtime candidate rank | `skill/src/index.ts:728` `runtimeCandidate` | `skills.rs` `collect_layer` runtime branch | `runtime_skill_outranks_same_rank_provider_candidate` |

> Known residual: upstream `SkillRegistration` accepts optional `whenToUse`,
> `source`, `resourceBase`, `path`, and `metadata`; the Rust `register_runtime` /
> `register_runtime_for_scope` helpers hard-code `source: "runtime"` and omit the
> remaining optional fields. This is an input-shape gap, not a lifecycle or
> watcher gap; it is tracked for the tool/agent slice and does not block this gate.

## 5. Observation and cache/invalidation

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| Completed-only cache | `skill/src/index.ts:459` `snapshot` + `collect` | `skills.rs` `snapshot`/`collect` `cacheable` | `snapshot_marks_incomplete_observations_and_tolerates_provider_errors` |
| Chain-bearing cache key | `skill/src/index.ts:686` `collectCacheKey` | `skills.rs` `CollectCacheKey` `(cwd, chain, revision)` | `session_resolved_skills_change_after_preset_recomposition` |
| Insertion-ordered FIFO eviction | `skill/src/index.ts` `Map` + `keys().next()` | `skills.rs` `IndexMap` + `shift_remove` | `collect_cache_evicts_in_insertion_order_and_respects_capacity` |
| Revision-bumped invalidation | `skill/src/index.ts:668` `invalidateCache` | `skills.rs` `publish` | `catalog_cache_is_revisioned_and_invalidation_refreshes_it` |
| `skills/change` event | `skill/src/index.ts:691` `notifyChange` | `skills.rs` `broadcast::Sender` + `SkillRegistryEvent` | `scoped_provider_registration_disposes_and_publishes_revisioned_events` |

## 6. Filesystem provider and watching

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| Root ranks/order | `skill-filesystem/src/index.ts:33-39` | `skills.rs` `PROJECT_DSH_RANK..BUNDLED_SKILL_RANK` | `filesystem_registry_discovers_and_prioritizes_dsh_skill_roots` |
| Directory vs flat discovery | `skill-filesystem/src/index.ts:715` `discoverRoot` | `skills.rs` `list_skill_entries` + `list` | `filesystem_provider_parses_flat_skills_and_ignores_invalid_entries` |
| YAML frontmatter parse | `skill-filesystem/src/index.ts:800` `parseSkillFile` | `skills.rs` `parse_skill_file` | `filesystem_provider_parses_flat_skills_and_ignores_invalid_entries` |
| Legacy invocation keys rejected | `skill-filesystem/src/index.ts` `rejectLegacyInvocationKey` | `skills.rs` `parse_frontmatter` | `filesystem_provider_parses_flat_skills_and_ignores_invalid_entries` |
| Ancestor-aware watch mode | `skill-filesystem/src/index.ts:610` `resolveRootWatchMode` | `skills.rs` `root_fingerprint_sync` | `ancestor_creation_invalidates_through_standing_preset_and_slash_catalog` |
| `observeHostMutation` | `skill-filesystem/src/index.ts:228` | `skills.rs` `FileSystemSkillProvider::observe_host_mutation` | `fs_observed_mutation_invalidates_only_potential_skill_paths` |
| `fs/observed` actor gate | `skill-filesystem/src/index.ts:139` + `mutationToolName:693` | `skills.rs` `observe_fs_event` + `mutation_tool_name` | `fs_observed_mutation_invalidates_only_potential_skill_paths` |
| `isPotentialSkillPath` | `skill-filesystem/src/index.ts:677` | `skills.rs` `is_potential_skill_path` | unit `is_potential_skill_path_matches_upstream_gating` |
| `containedSegments` | `skill-filesystem/src/index.ts:685` | `skills.rs` `contained_segments` | unit `contained_segments_rejects_paths_outside_the_root` |
| Watcher poll invalidation | `skill-filesystem/src/index.ts` `SkillWatchManager` | `skills.rs` `SkillWatch` + `watch_thread` | `filesystem_watcher_invalidates_catalog_on_skill_changes` |

> Known simplification: the Rust watcher is a bounded mtime/fingerprint poll, not
> Chokidar native events + `awaitWriteFinish` stability window. The observable
> contract (ancestor-aware creation invalidation, bounded roots, disposal) is
> preserved; native event fidelity is a production-readiness item.

## 7. Model `skill` tool and presentation

| Contract | Upstream path/symbol | Rust implementation | Test |
|---|---|---|---|
| Tool schema | `tool-skill/src/index.ts:65` `defineTool` | `skills.rs`/`tools/skill.rs` `SkillTool::spec` | `schema_matches_the_dsh_tool_contract` |
| Catalog description cap | `tool-skill/src/index.ts:19` `catalogDescriptionMaxLength` | `tools/skill.rs` `with_catalog_description_max_length` | `catalog_description_truncates_to_a_configured_maximum` |
| Min cap of 3 | `tool-skill/src/index.ts` `assertPositiveInteger(..., 3)` | `tools/skill.rs` `assert!(max_length >= 3)` | `catalog_description_max_length_rejects_values_below_three` |
| `renderSkillContent` | `skill/src/index.ts:154` | `skills.rs` `render_skill_content` | `skill_content_rendering_matches_the_dsh_wire_contract` |
| `presentCall` metadata | `tool-skill/src/index.ts:157` | `tools/skill.rs` `present_call` | `present_call_renders_upstream_skill_load_metadata` |
| Model vs user invocation gate | `skill/src/index.ts:130-140` `isModelInvocable`/`isUserInvocable` | `skills.rs` `SkillInvocationPolicy` fields | `skill_tool_loads_model_skills_and_rejects_unavailable_skills` |
| Catalog render | `tool-skill/src/index.ts:250` `renderCatalogMessage` | `tools/skill.rs` `render_skill_catalog` | `skill_catalog_filters_normalizes_and_renders_the_dsh_contract` |
| Catalog update/tombstone | `tool-skill/src/index.ts:270` `renderCatalogUpdate` | `tools/skill.rs` `render_skill_catalog_update` | `skill_catalog_updates_render_the_exact_dsh_contract` |
| Exact-identity tool visibility | `tool-skill/src/index.ts:215` `ctx.tools.get(name, agent)` | `tools.rs` `ToolRegistry::get(name, scope)` | `skill_catalog_follows_exact_identity_tool_visibility_by_scope` |
| Pre-step injection | `tool-skill/src/index.ts:178` user-explicit invocation | `agent.rs` `prepare_skill_context` | `agent_publishes_catalog_once_and_injects_user_invocable_skills` |

## 8. Named end-to-end regression

`standing_preset_end_to_end_skill_lifecycle_through_session_load_and_presentation`
threads the full chain in one regression: standing filesystem provider →
session scope → slash + model catalog → `presentCall` metadata → session-scoped
`skill` load via `execute_with_session` → `observeHostMutation` behind
`fs/observed` → exact-provider invalidation → disposal/unregister.

## Verification

- `cargo test -p harness-core --test skill_subsystem --test skill_prestep` — 36 passed.
- `cargo clippy -p harness-core --all-targets` clean.
- `cargo fmt --all --check` clean.
- `cargo test -p harness-core --lib` — 2 sandbox-blocked `remote.rs` TCP tests
  (`openai_adapter_*`, `TcpListener::bind` `PermissionDenied`) cited separately
  from code failures.
