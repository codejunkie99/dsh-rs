use async_trait::async_trait;
use harness_core::approval::ApprovalPolicy;
use harness_core::harness::{HarnessSetupsConfig, SUPPORTED_TOOLS};
use harness_core::preset::StandingProviderFactory;
use harness_core::skills::{
    render_skill_content, AbortSignal, FileSystemSkillProvider, SkillCandidate, SkillDefinition,
    SkillFileSystemConfig, SkillInvocationPolicy, SkillLookupOptions, SkillProvider,
    SkillProviderControl, SkillProviderObservation, SkillRegistry, SkillRegistryEvent,
    SkillRegistryEventKind, SkillResourceBase, SkillScope, SkillViewOptions, SkillWatchConfig,
};
use harness_core::store::SessionStore;
use harness_core::tools::{SkillTool, Tool, ToolCallKind, ToolCallView, ToolInvocation};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn bundled_skill(root: &Path, name: &str, description: &str, body: &str) {
    write(
        &root.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    );
}

#[tokio::test]
async fn filesystem_registry_discovers_and_prioritizes_dsh_skill_roots() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let custom = tempfile::tempdir().unwrap();
    let bundled = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join(".git")).unwrap();
    fs::create_dir(project.path().join("src")).unwrap();

    bundled_skill(
        bundled.path(),
        "bundled-only",
        "Bundled only",
        "Bundled body.",
    );
    bundled_skill(bundled.path(), "same", "Bundled loses", "Bundled body.");
    bundled_skill(custom.path(), "custom-only", "Custom only", "Custom body.");
    bundled_skill(
        custom.path(),
        "runtime-name",
        "Custom loses",
        "Custom body.",
    );
    bundled_skill(
        &project.path().join(".dsh/skills"),
        "same",
        "Project wins",
        "Project body.",
    );
    bundled_skill(
        &project.path().join(".agents/skills"),
        "same",
        "Project agents loses",
        "Body.",
    );
    bundled_skill(
        &home.path().join(".dsh/skills"),
        "same",
        "User DSH loses",
        "User body.",
    );
    bundled_skill(
        &home.path().join(".agents/skills"),
        "same",
        "User agents loses",
        "User body.",
    );
    bundled_skill(
        &home.path().join(".dsh/skills"),
        "runtime-name",
        "User loses",
        "User body.",
    );
    write(
        &home.path().join(".dsh/skills/user-only/SKILL.md"),
        "---\nname: user-only\ndescription: User-only\ndisable-model-invocation: true\n---\n\nUser-only body.\n",
    );

    let registry = SkillRegistry::new();
    let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
        include_default_roots: true,
        dsh_home: home.path().join(".dsh"),
        agents_home: home.path().join(".agents"),
        custom_skill_dirs: vec![custom.path().to_path_buf()],
        bundled_skill_dir: Some(bundled.path().to_path_buf()),
    })
    .unwrap();
    registry.register_provider(Arc::new(provider)).unwrap();
    registry
        .register_runtime(
            "project-name",
            "Runtime loses to project",
            "Project body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "runtime-name",
            "Runtime wins",
            "Runtime body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let options = SkillViewOptions {
        cwd: Some(project.path().join("src")),
        ..Default::default()
    };
    let summaries = registry.list(&options).await.unwrap();
    let names: Vec<_> = summaries.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "bundled-only",
            "custom-only",
            "project-name",
            "runtime-name",
            "same",
            "user-only"
        ]
    );

    let same = summaries.iter().find(|skill| skill.name == "same").unwrap();
    assert_eq!(same.description, "Project wins");
    assert_eq!(same.source, "project-dsh");
    assert_eq!(same.provider, "filesystem");
    assert_eq!(
        same.resource_base,
        Some(SkillResourceBase::Directory {
            path: project.path().join(".dsh/skills/same")
        })
    );

    let project_name = registry
        .get("project-name", &options)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project_name.description, "Runtime loses to project");
    let runtime_name = registry
        .get("runtime-name", &options)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime_name.description, "Runtime wins");
    let user_only = registry.get("user-only", &options).await.unwrap().unwrap();
    assert!(!user_only.invocation.model_invocable);
}

#[tokio::test]
async fn filesystem_provider_parses_flat_skills_and_ignores_invalid_entries() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    write(
        &root.join("flat-skill.md"),
        "---\nname: flat-skill\ndescription: Flat skill\n---\n\nFlat body.\n",
    );
    write(
        &root.join("rich-skill.md"),
        "---\nname: rich-skill\ndescription: Rich skill\nwhen_to_use: Use for rich parsing\ndisable-model-invocation: false\nuser-invocable: true\n---\n\nUse the rich instructions.\n",
    );
    write(
        &root.join("bad-name.md"),
        "---\nname: Bad_Name\ndescription: Bad\n---\n\nBad.\n",
    );
    write(
        &root.join("missing-description.md"),
        "---\nname: missing-description\n---\n\nBad.\n",
    );
    write(&root.join("no-frontmatter.md"), "# Notes\n");
    write(&root.join("ignored.txt"), "not a skill\n");

    let registry = SkillRegistry::new();
    let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
        include_default_roots: true,
        dsh_home: home.path().join(".dsh"),
        agents_home: home.path().join(".agents"),
        custom_skill_dirs: Vec::new(),
        bundled_skill_dir: None,
    })
    .unwrap();
    registry.register_provider(Arc::new(provider)).unwrap();

    let options = SkillViewOptions {
        cwd: None,
        scope: None,
        ..Default::default()
    };
    let summaries = registry.list(&options).await.unwrap();
    let names: Vec<_> = summaries.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, ["flat-skill", "rich-skill"]);

    let rich = registry.get("rich-skill", &options).await.unwrap().unwrap();
    assert_eq!(rich.when_to_use.as_deref(), Some("Use for rich parsing"));
    assert_eq!(rich.content, "Use the rich instructions.");
    assert!(rich.invocation.model_invocable);
    assert!(rich.invocation.user_invocable);
    assert_eq!(rich.path, Some(root.join("rich-skill.md")));
}

#[test]
fn skill_content_rendering_matches_the_dsh_wire_contract() {
    let rendered = render_skill_content(
        "escaped-name",
        "filesystem",
        Some(SkillResourceBase::Directory {
            path: "/skills/a & b".into(),
        }),
        "Trusted instructions.",
    );
    assert_eq!(
        rendered,
        [
            "<skill_content name=\"escaped-name\">",
            "<skill_resources>",
            "Base directory for this skill: /skills/a &amp; b",
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.",
            "</skill_resources>",
            "",
            "<skill_instructions>",
            "Trusted instructions.",
            "</skill_instructions>",
            "</skill_content>"
        ]
        .join("\n")
    );
}

#[tokio::test]
async fn skill_tool_loads_model_skills_and_rejects_unavailable_skills() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "model-skill",
            "A model skill",
            "Follow the model instructions.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "user-skill",
            "A user skill",
            "Follow the user instructions.",
            SkillInvocationPolicy {
                model_invocable: false,
                user_invocable: true,
            },
        )
        .unwrap();
    let tool = SkillTool::new(Arc::new(registry));

    let spec = tool.spec();
    assert_eq!(spec.name, "skill");
    assert_eq!(
        spec.parameters,
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The exact skill name from the available skills list."
                }
            },
            "required": ["name"]
        })
    );

    let loaded = tool
        .execute(ToolInvocation {
            call_id: "call-skill".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "model-skill"}),
        })
        .await
        .unwrap();
    assert!(loaded.ok);
    assert!(loaded
        .output
        .contains("<skill_content name=\"model-skill\">"));
    assert!(loaded.output.contains("Follow the model instructions."));

    let invalid = tool
        .execute(ToolInvocation {
            call_id: "call-invalid".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "Bad_Name"}),
        })
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("invalid skill name"));

    let unavailable = tool
        .execute(ToolInvocation {
            call_id: "call-unavailable".into(),
            name: "skill".into(),
            arguments: serde_json::json!({"name": "user-skill"}),
        })
        .await
        .unwrap_err();
    assert!(unavailable
        .to_string()
        .contains("not available for model invocation"));
}

#[tokio::test]
async fn runtime_registry_rejects_invalid_names_and_duplicate_providers() {
    let registry = SkillRegistry::new();
    let invalid = registry
        .register_runtime(
            "Bad_Name",
            "Invalid",
            "Body",
            SkillInvocationPolicy::default(),
        )
        .unwrap_err();
    assert!(invalid.to_string().contains("invalid skill name"));

    let provider: Arc<dyn SkillProvider> = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: Path::new("/tmp/dsh").into(),
            agents_home: Path::new("/tmp/agents").into(),
            custom_skill_dirs: Vec::new(),
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    registry.register_provider(provider.clone()).unwrap();
    let duplicate = registry.register_provider(provider).unwrap_err();
    assert!(duplicate.to_string().contains("already registered"));
}

#[tokio::test]
async fn scoped_provider_registration_disposes_and_publishes_revisioned_events() {
    let home = tempfile::tempdir().unwrap();
    bundled_skill(home.path(), "scoped", "Scoped skill", "Scoped body.");

    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();
    let provider = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: home.path().join(".dsh"),
            agents_home: home.path().join(".agents"),
            custom_skill_dirs: vec![home.path().to_path_buf()],
            bundled_skill_dir: None,
        })
        .unwrap(),
    );

    let registration = registry.register_provider_scoped(provider).unwrap();
    let registered = events.try_recv().unwrap();
    assert_eq!(registered.kind, SkillRegistryEventKind::ProviderRegistered);
    assert_eq!(registered.provider.as_deref(), Some("filesystem"));
    assert_eq!(registered.revision, registry.revision());

    let names: Vec<_> = registry
        .list(&SkillViewOptions::default())
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, ["scoped"]);

    registration.invalidate().unwrap();
    let invalidated = events.try_recv().unwrap();
    assert_eq!(
        invalidated.kind,
        SkillRegistryEventKind::ProviderInvalidated
    );
    assert!(invalidated.revision > registered.revision);

    drop(registration);
    let removed = events.try_recv().unwrap();
    assert_eq!(removed.kind, SkillRegistryEventKind::ProviderUnregistered);
    assert!(removed.revision > invalidated.revision);
    assert!(registry
        .list(&SkillViewOptions::default())
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn standard_harness_and_default_approval_admit_the_skill_tool() {
    assert!(SUPPORTED_TOOLS.contains(&"skill"));
    let setups = HarnessSetupsConfig::default();
    assert!(setups.get("standard").unwrap().enables("skill"));
    assert_eq!(
        ApprovalPolicy::default().decide("skill"),
        harness_core::approval::ApprovalDecision::Allow
    );
}

#[tokio::test]
async fn catalog_cache_is_revisioned_and_invalidation_refreshes_it() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    bundled_skill(&root, "alpha", "Alpha skill", "Alpha body.");

    let registry = SkillRegistry::new();
    let provider = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: home.path().join(".dsh"),
            agents_home: home.path().join(".agents"),
            custom_skill_dirs: vec![root.clone()],
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    registry.register_provider(provider).unwrap();

    let options = SkillViewOptions {
        cwd: None,
        scope: None,
        ..Default::default()
    };
    assert_eq!(skill_names(&registry, &options).await, ["alpha"]);

    // Without an invalidation the registry keeps the completed catalog cached.
    bundled_skill(&root, "beta", "Beta skill", "Beta body.");
    assert_eq!(skill_names(&registry, &options).await, ["alpha"]);

    registry.invalidate_provider("filesystem").unwrap();
    assert_eq!(skill_names(&registry, &options).await, ["alpha", "beta"]);
}

#[tokio::test]
async fn slash_snapshot_carries_revision_and_refreshes_after_invalidation() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    bundled_skill(&root, "alpha", "Alpha skill", "Alpha body.");

    let registry = SkillRegistry::new();
    let provider = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: home.path().join(".dsh"),
            agents_home: home.path().join(".agents"),
            custom_skill_dirs: vec![root.clone()],
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    registry.register_provider(provider).unwrap();
    let tool = SkillTool::new(Arc::new(registry.clone())).with_cwd(root.clone());

    let first = tool.slash_snapshot().await.unwrap();
    assert_eq!(first.revision, registry.revision());
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );

    bundled_skill(&root, "beta", "Beta skill", "Beta body.");
    registry.invalidate_provider("filesystem").unwrap();
    let second = tool.slash_snapshot().await.unwrap();
    assert!(second.revision > first.revision);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
}

#[tokio::test]
async fn filesystem_watcher_invalidates_catalog_on_skill_changes() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    bundled_skill(&root, "first", "First skill", "First body.");

    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();
    let registration = registry
        .register_provider_scoped_with_control(|control| {
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: home.path().join(".dsh"),
                agents_home: home.path().join(".agents"),
                custom_skill_dirs: vec![root.clone()],
                bundled_skill_dir: None,
            })?
            .with_watching(
                control.clone(),
                SkillWatchConfig {
                    poll_interval: std::time::Duration::from_millis(10),
                },
            );
            Ok(Arc::new(provider))
        })
        .unwrap();

    let options = SkillViewOptions {
        cwd: None,
        scope: None,
        ..Default::default()
    };
    assert_eq!(skill_names(&registry, &options).await, ["first"]);
    assert_eq!(
        events.try_recv().unwrap().kind,
        SkillRegistryEventKind::ProviderRegistered
    );

    bundled_skill(&root, "second", "Second skill", "Second body.");
    let invalidated =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderInvalidated).await;
    assert_eq!(invalidated.provider.as_deref(), Some("filesystem"));
    assert_eq!(skill_names(&registry, &options).await, ["first", "second"]);

    drop(registration);
}

#[tokio::test]
async fn already_aborted_lookup_signal_rejects_before_discovery() {
    let registry = SkillRegistry::new();
    registry
        .register_provider(Arc::new(HangingProvider))
        .unwrap();

    let signal = AbortSignal::new();
    signal.abort();
    let options = SkillViewOptions {
        cwd: None,
        signal: Some(signal),
        scope: None,
    };

    let error = registry.list(&options).await.unwrap_err();
    assert!(error.to_string().contains("skill lookup aborted"));
}

#[tokio::test]
async fn aborted_lookup_rejects_a_runtime_only_registry_on_the_cache_hit_path() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "runtime-only",
            "Runtime only",
            "Runtime body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    // Warm the collect cache so the abort must be observed before the cache is
    // consulted, not only during provider discovery (upstream
    // `SkillRegistry.collect` throws `throwIfAborted` at entry,
    // `packages/skill/skill/src/index.ts:566`).
    assert_eq!(
        registry
            .list(&SkillViewOptions::default())
            .await
            .unwrap()
            .len(),
        1
    );

    let signal = AbortSignal::new();
    signal.abort();
    let options = SkillViewOptions {
        signal: Some(signal),
        ..Default::default()
    };

    let get_error = registry.get("runtime-only", &options).await.unwrap_err();
    assert!(get_error.to_string().contains("skill lookup aborted"));
    let list_error = registry.list(&options).await.unwrap_err();
    assert!(list_error.to_string().contains("skill lookup aborted"));
}

#[tokio::test]
async fn lookup_stops_awaiting_a_hung_provider_when_aborted() {
    let registry = SkillRegistry::new();
    registry
        .register_provider(Arc::new(HangingProvider))
        .unwrap();

    let signal = AbortSignal::new();
    let options = SkillViewOptions {
        cwd: None,
        signal: Some(signal.clone()),
        scope: None,
    };
    let registry_for_spawn = registry.clone();

    let handle = tokio::spawn(async move { registry_for_spawn.list(&options).await });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    signal.abort();

    let error = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("abort did not stop the hung provider")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("skill lookup aborted"));
}

#[tokio::test]
async fn snapshot_marks_incomplete_observations_and_tolerates_provider_errors() {
    let registry = SkillRegistry::new();
    registry
        .register_provider(Arc::new(IncompleteProvider))
        .unwrap();
    let options = SkillViewOptions {
        cwd: None,
        scope: None,
        ..Default::default()
    };
    let snapshot = registry.snapshot(&options).await.unwrap();
    assert!(!snapshot.complete);
    assert_eq!(
        snapshot
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["incomplete-skill"]
    );

    let failing = SkillRegistry::new();
    failing
        .register_provider(Arc::new(FailingProvider))
        .unwrap();
    let snapshot = failing.snapshot(&options).await.unwrap();
    assert!(!snapshot.complete);
    assert!(snapshot.skills.is_empty());
}

struct IncompleteProvider;

#[async_trait]
impl SkillProvider for IncompleteProvider {
    fn name(&self) -> &str {
        "incomplete"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        Ok(SkillProviderObservation {
            candidates: vec![SkillCandidate {
                name: "incomplete-skill".into(),
                description: "Incomplete".into(),
                when_to_use: None,
                invocation: SkillInvocationPolicy::default(),
                source: "test".into(),
                provider: "incomplete".into(),
                resource_base: None,
                rank: 100,
                locator: serde_json::Value::Null,
                path: None,
                metadata: None,
            }],
            complete: false,
        })
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(None)
    }
}

struct HangingProvider;

#[async_trait]
impl SkillProvider for HangingProvider {
    fn name(&self) -> &str {
        "hanging"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        std::future::pending::<()>().await;
        unreachable!()
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(None)
    }
}

struct FailingProvider;

#[async_trait]
impl SkillProvider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        anyhow::bail!("provider failed to list")
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(None)
    }
}

async fn skill_names(registry: &SkillRegistry, options: &SkillViewOptions) -> Vec<String> {
    registry
        .list(options)
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

async fn wait_for_event(
    events: &mut tokio::sync::broadcast::Receiver<SkillRegistryEvent>,
    kind: SkillRegistryEventKind,
) -> SkillRegistryEvent {
    for _ in 0..500 {
        match events.try_recv() {
            Ok(event) if event.kind == kind => return event,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    panic!("did not observe expected skill registry event in time");
}

#[tokio::test]
async fn scoped_runtime_layers_shadow_globals_and_follow_recomposition() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "global-only",
            "Global",
            "Global body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "shared",
            "Global shared",
            "Global shared body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let preset_a = SkillScope::new();
    registry
        .register_runtime_for_scope(
            preset_a.clone(),
            "a-only",
            "Preset A",
            "A body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime_for_scope(
            preset_a.clone(),
            "shared",
            "Preset A shared",
            "A shared body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let preset_b = SkillScope::new();
    registry
        .register_runtime_for_scope(
            preset_b.clone(),
            "b-only",
            "Preset B",
            "B body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    // The agent key is parented to preset A, mirroring the standing mount.
    let agent = SkillScope::new();
    let binding = agent.bind_parent(preset_a.clone()).unwrap();

    let view = SkillViewOptions {
        cwd: None,
        scope: Some(agent.clone()),
        ..Default::default()
    };
    let names: Vec<_> = registry
        .list(&view)
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, ["a-only", "global-only", "shared"]);
    assert_eq!(
        registry
            .get("shared", &view)
            .await
            .unwrap()
            .unwrap()
            .description,
        "Preset A shared"
    );

    // Blank-session recompose re-links the same agent key to preset B without
    // touching the registry; only the chain-bearing cache key changes.
    binding.rebind(preset_b.clone()).unwrap();
    let names: Vec<_> = registry
        .list(&view)
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, ["b-only", "global-only", "shared"]);
    assert_eq!(
        registry
            .get("shared", &view)
            .await
            .unwrap()
            .unwrap()
            .description,
        "Global shared"
    );
}

#[tokio::test]
async fn scoped_filesystem_provider_shadows_a_global_provider_of_the_same_name() {
    let global_root = tempfile::tempdir().unwrap();
    let preset_root = tempfile::tempdir().unwrap();
    bundled_skill(global_root.path(), "shared", "Global", "Global body.");
    bundled_skill(preset_root.path(), "shared", "Preset", "Preset body.");

    let registry = SkillRegistry::new();
    let global_provider = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: Path::new("/tmp/dsh").into(),
            agents_home: Path::new("/tmp/agents").into(),
            custom_skill_dirs: vec![global_root.path().to_path_buf()],
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    registry.register_provider(global_provider).unwrap();

    let preset = SkillScope::new();
    let preset_provider = Arc::new(
        FileSystemSkillProvider::new(SkillFileSystemConfig {
            include_default_roots: false,
            dsh_home: Path::new("/tmp/dsh").into(),
            agents_home: Path::new("/tmp/agents").into(),
            custom_skill_dirs: vec![preset_root.path().to_path_buf()],
            bundled_skill_dir: None,
        })
        .unwrap(),
    );
    let _preset_registration = registry
        .register_provider_for_scope(preset.clone(), preset_provider)
        .unwrap();

    let global_view = SkillViewOptions::default();
    assert_eq!(
        registry
            .get("shared", &global_view)
            .await
            .unwrap()
            .unwrap()
            .description,
        "Global"
    );

    let scoped_view = SkillViewOptions {
        cwd: None,
        scope: Some(preset),
        ..Default::default()
    };
    assert_eq!(
        registry
            .get("shared", &scoped_view)
            .await
            .unwrap()
            .unwrap()
            .description,
        "Preset"
    );
}

struct MismatchedNameProvider;

#[async_trait]
impl SkillProvider for MismatchedNameProvider {
    fn name(&self) -> &str {
        "mismatch-name"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        Ok(SkillProviderObservation::complete(vec![SkillCandidate {
            name: "mismatch".into(),
            description: "Mismatched candidate".into(),
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            source: "test".into(),
            provider: "mismatch-name".into(),
            resource_base: None,
            rank: 100,
            locator: serde_json::Value::Null,
            path: None,
            metadata: None,
        }]))
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(Some(SkillDefinition {
            name: "renamed".into(),
            description: "Renamed definition".into(),
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            source: "test".into(),
            provider: "mismatch-name".into(),
            resource_base: None,
            path: None,
            metadata: None,
            content: "Renamed body.".into(),
        }))
    }
}

struct ReservedNameProvider;

#[async_trait]
impl SkillProvider for ReservedNameProvider {
    fn name(&self) -> &str {
        "runtime"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        Ok(SkillProviderObservation::complete(Vec::new()))
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(None)
    }
}

struct SameRankProvider;

#[async_trait]
impl SkillProvider for SameRankProvider {
    fn name(&self) -> &str {
        "same-rank"
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        // Rank 250 is the upstream `RUNTIME_RANK`
        // (`packages/skill/skill/src/index.ts:20`), so a same-name runtime
        // registration collides with this provider candidate on rank.
        Ok(SkillProviderObservation::complete(vec![SkillCandidate {
            name: "same-rank".into(),
            description: "Provider candidate".into(),
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            source: "test".into(),
            provider: "same-rank".into(),
            resource_base: None,
            rank: 250,
            locator: serde_json::Value::Null,
            path: None,
            metadata: None,
        }]))
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(Some(SkillDefinition {
            name: "same-rank".into(),
            description: "Provider candidate".into(),
            when_to_use: None,
            invocation: SkillInvocationPolicy::default(),
            source: "test".into(),
            provider: "same-rank".into(),
            resource_base: None,
            path: None,
            metadata: None,
            content: "Provider body.".into(),
        }))
    }
}

#[tokio::test]
async fn stale_definition_load_invalidates_while_the_exact_provider_is_live() {
    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();
    registry
        .register_provider(Arc::new(MismatchedNameProvider))
        .unwrap();
    let registered = events.try_recv().unwrap();
    assert_eq!(registered.kind, SkillRegistryEventKind::ProviderRegistered);

    let revision_before = registry.revision();
    let definition = registry
        .get("mismatch", &SkillViewOptions::default())
        .await
        .unwrap();
    assert!(definition.is_none());

    let invalidated = events.try_recv().unwrap();
    assert_eq!(
        invalidated.kind,
        SkillRegistryEventKind::ProviderInvalidated
    );
    assert_eq!(invalidated.provider.as_deref(), Some("mismatch-name"));
    assert!(invalidated.revision > revision_before);
    assert_eq!(registry.revision(), invalidated.revision);
}

#[tokio::test]
async fn runtime_duplicate_registration_is_first_wins() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "dup",
            "First",
            "First body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "dup",
            "Second",
            "Second body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let definition = registry
        .get("dup", &SkillViewOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(definition.description, "First");
}

#[tokio::test]
async fn runtime_skill_outranks_same_rank_provider_candidate() {
    let registry = SkillRegistry::new();
    registry
        .register_provider(Arc::new(SameRankProvider))
        .unwrap();
    registry
        .register_runtime(
            "same-rank",
            "Runtime skill",
            "Runtime body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    // Upstream `compareIndexedCandidates` orders the runtime candidate's
    // `providerOrder: -1` before the provider's `providerOrder: 0` when ranks
    // tie (`packages/skill/skill/src/index.ts:483-486`), so the runtime
    // registration wins the duplicate name.
    let skill = registry
        .get("same-rank", &SkillViewOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(skill.provider, "runtime");
    assert_eq!(skill.content, "Runtime body.");
}

#[tokio::test]
async fn provider_registration_failure_and_reserved_names_publish_no_event() {
    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();

    let failed = registry.register_provider_for_scope_with_control(SkillScope::new(), |_control| {
        anyhow::bail!("provider construction failed")
    });
    assert!(failed.is_err());
    assert!(events.try_recv().is_err());

    let reserved = registry
        .register_provider_for_scope_with_control(SkillScope::new(), |_control| {
            Ok(Arc::new(ReservedNameProvider))
        });
    assert!(reserved.is_err());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn provider_control_observes_registration_disposal() {
    let registry = SkillRegistry::new();
    let control_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<SkillProviderControl>));
    let control_cell_for_create = control_cell.clone();

    let registration = registry
        .register_provider_scoped_with_control(|control| {
            *control_cell_for_create.lock().unwrap() = Some(control.clone());
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: Path::new("/tmp/dsh").into(),
                agents_home: Path::new("/tmp/agents").into(),
                custom_skill_dirs: Vec::new(),
                bundled_skill_dir: None,
            })?;
            Ok(Arc::new(provider))
        })
        .unwrap();

    let control = control_cell.lock().unwrap().clone().unwrap();
    assert!(!control.is_cancelled());

    drop(registration);

    assert!(control.is_cancelled());
}

#[tokio::test]
async fn provider_control_signal_resolves_on_registration_disposal() {
    let registry = SkillRegistry::new();
    let control_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<SkillProviderControl>));
    let control_cell_for_create = control_cell.clone();

    let registration = registry
        .register_provider_scoped_with_control(|control| {
            *control_cell_for_create.lock().unwrap() = Some(control.clone());
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: Path::new("/tmp/dsh").into(),
                agents_home: Path::new("/tmp/agents").into(),
                custom_skill_dirs: Vec::new(),
                bundled_skill_dir: None,
            })?;
            Ok(Arc::new(provider))
        })
        .unwrap();

    let control = control_cell.lock().unwrap().clone().unwrap();
    assert!(!control.is_cancelled());

    // The abort-observable surface: a provider awaiting the signal is woken by
    // disposal instead of re-checking a flag on a timer, mirroring upstream
    // `SkillProviderControl.signal` (`packages/skill/skill/src/index.ts:273`).
    let signal = control.signal().clone();
    let waiter = tokio::spawn(async move { signal.cancelled().await });

    // Give the waiter a chance to arm before disposal so it must be woken.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(!waiter.is_finished());

    drop(registration);

    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("abort-observable signal did not resolve on disposal")
        .unwrap();
    assert!(control.is_cancelled());
}

#[tokio::test]
async fn fs_observed_mutation_invalidates_only_potential_skill_paths() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".dsh/skills");
    fs::create_dir_all(&root).unwrap();
    bundled_skill(&root, "first", "First skill", "First body.");

    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();
    let provider_cell = Arc::new(std::sync::Mutex::new(None::<Arc<FileSystemSkillProvider>>));
    let provider_cell_for_create = provider_cell.clone();
    let root_for_create = root.clone();
    let home_for_create = home.path().to_path_buf();

    let registration = registry
        .register_provider_scoped_with_control(move |control| {
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: home_for_create.join(".dsh"),
                agents_home: home_for_create.join(".agents"),
                custom_skill_dirs: vec![root_for_create.clone()],
                bundled_skill_dir: None,
            })?
            .with_watching(
                control.clone(),
                SkillWatchConfig {
                    poll_interval: std::time::Duration::from_secs(60),
                },
            );
            let provider = Arc::new(provider);
            *provider_cell_for_create.lock().unwrap() = Some(provider.clone());
            Ok(provider as Arc<dyn SkillProvider>)
        })
        .unwrap();
    let provider = provider_cell.lock().unwrap().clone().unwrap();

    let options = SkillViewOptions {
        cwd: None,
        scope: None,
        ..Default::default()
    };
    assert_eq!(skill_names(&registry, &options).await, ["first"]);
    let registered = wait_for_event(&mut events, SkillRegistryEventKind::ProviderRegistered).await;
    assert_eq!(registered.provider.as_deref(), Some("filesystem"));

    // A first-party write to a potential skill entry invalidates, mirroring the
    // upstream `fs/observed` -> `observeHostMutation` fast path.
    let skill_md = root.join("first").join("SKILL.md");
    let before = registry.revision();
    provider.observe_fs_event(&skill_md, "write");
    let invalidated =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderInvalidated).await;
    assert_eq!(invalidated.provider.as_deref(), Some("filesystem"));
    assert!(registry.revision() > before);

    // A non-edit/write actor is ignored.
    let before = registry.revision();
    provider.observe_fs_event(&skill_md, "read");
    assert_eq!(registry.revision(), before);

    // A write to a path that is not a potential skill entry is ignored.
    let before = registry.revision();
    provider.observe_fs_event(&root.join("notes").join("note.md"), "write");
    assert_eq!(registry.revision(), before);

    drop(registration);
}

#[tokio::test]
async fn standing_preset_slash_catalog_follows_session_scope_and_provider_lifecycle() {
    let home = tempfile::tempdir().unwrap();
    let presets = tempfile::tempdir().unwrap();
    bundled_skill(
        &presets.path().join("research"),
        "research-only",
        "Research skill",
        "Research body.",
    );
    bundled_skill(
        &presets.path().join("standard"),
        "standard-only",
        "Standard skill",
        "Standard body.",
    );

    let registry = Arc::new(SkillRegistry::new());
    let mut events = registry.subscribe();
    let registry_for_factory = registry.clone();
    let presets_root = presets.path().to_path_buf();
    let home_root = home.path().to_path_buf();
    let factory: StandingProviderFactory = Arc::new(move |preset_id: &str, scope: &SkillScope| {
        let skill_dir = presets_root.join(preset_id);
        registry_for_factory.register_provider_for_scope_with_control(scope.clone(), |control| {
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: home_root.join(".dsh"),
                agents_home: home_root.join(".agents"),
                custom_skill_dirs: vec![skill_dir.clone()],
                bundled_skill_dir: None,
            })?
            .with_watching(control.clone(), SkillWatchConfig::default());
            Ok(Arc::new(provider))
        })
    });

    let sessions = tempfile::tempdir().unwrap();
    let store = SessionStore::open_with_default_preset_and_provider(
        sessions.path(),
        "standard",
        Some(factory),
    )
    .unwrap();

    let research_session = store
        .create_in_space_and_harness("Research", None, None, Some("research".into()))
        .unwrap();
    let standard_session = store
        .create_in_space_and_harness("Standard", None, None, Some("standard".into()))
        .unwrap();

    let skill_tool = SkillTool::new(registry.clone());
    let research_scope = research_session.skill_scope().unwrap();
    let standard_scope = standard_session.skill_scope().unwrap();

    let research = skill_tool
        .slash_snapshot_for(Some(research_scope.clone()))
        .await
        .unwrap();
    let standard = skill_tool
        .slash_snapshot_for(Some(standard_scope.clone()))
        .await
        .unwrap();

    assert_eq!(
        research
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["research-only"]
    );
    assert_eq!(
        standard
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["standard-only"]
    );
    assert_eq!(research.revision, registry.revision());
    assert_eq!(standard.revision, registry.revision());

    let mut registered = 0usize;
    loop {
        match events.try_recv() {
            Ok(event) if event.kind == SkillRegistryEventKind::ProviderRegistered => {
                assert_eq!(event.provider.as_deref(), Some("filesystem"));
                registered += 1;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert_eq!(registered, 2);

    // Dropping the store drops the roster and unregisters every standing
    // provider; the already-joined session scope then resolves an empty layer.
    drop(store);
    drop(standard_session);
    drop(research_session);

    let mut unregistered = 0usize;
    loop {
        match events.try_recv() {
            Ok(event) if event.kind == SkillRegistryEventKind::ProviderUnregistered => {
                assert_eq!(event.provider.as_deref(), Some("filesystem"));
                unregistered += 1;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert_eq!(unregistered, 2);

    let after = skill_tool
        .slash_snapshot_for(Some(research_scope))
        .await
        .unwrap();
    assert!(after.entries.is_empty());
}

#[tokio::test]
async fn standing_preset_end_to_end_skill_lifecycle_through_session_load_and_presentation() {
    let home = tempfile::tempdir().unwrap();
    let presets = tempfile::tempdir().unwrap();
    let skill_dir = presets.path().join("research");
    bundled_skill(
        &skill_dir,
        "research-only",
        "Research skill",
        "Research body.",
    );

    let registry = Arc::new(SkillRegistry::new());
    let mut events = registry.subscribe();
    let registry_for_factory = registry.clone();
    let provider_cell = Arc::new(std::sync::Mutex::new(None::<Arc<FileSystemSkillProvider>>));
    let provider_cell_for_create = provider_cell.clone();
    let presets_root = presets.path().to_path_buf();
    let home_root = home.path().to_path_buf();

    let factory: StandingProviderFactory = Arc::new(move |preset_id: &str, scope: &SkillScope| {
        let skill_dir = presets_root.join(preset_id);
        let home_root = home_root.clone();
        let provider_cell = provider_cell_for_create.clone();
        registry_for_factory.register_provider_for_scope_with_control(
            scope.clone(),
            move |control| {
                let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                    include_default_roots: false,
                    dsh_home: home_root.join(".dsh"),
                    agents_home: home_root.join(".agents"),
                    custom_skill_dirs: vec![skill_dir.clone()],
                    bundled_skill_dir: None,
                })?
                .with_watching(
                    control.clone(),
                    SkillWatchConfig {
                        poll_interval: std::time::Duration::from_secs(60),
                    },
                );
                let provider = Arc::new(provider);
                *provider_cell.lock().unwrap() = Some(provider.clone());
                Ok(provider as Arc<dyn SkillProvider>)
            },
        )
    });

    let sessions = tempfile::tempdir().unwrap();
    let store = SessionStore::open_with_default_preset_and_provider(
        sessions.path(),
        "standard",
        Some(factory),
    )
    .unwrap();
    let session = store
        .create_in_space_and_harness("Research", None, None, Some("research".into()))
        .unwrap();
    let scope = session.skill_scope().unwrap();
    let skill_tool = Arc::new(SkillTool::new(registry.clone()));

    // Slash and model catalogs both resolve through the session's standing scope.
    let slash = skill_tool
        .slash_snapshot_for(Some(scope.clone()))
        .await
        .unwrap();
    assert_eq!(
        slash
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["research-only"]
    );
    let model = skill_tool
        .catalog_entries_for(Some(scope.clone()))
        .await
        .unwrap();
    assert_eq!(
        model
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["research-only"]
    );

    // Tool-call presentation matches upstream `presentCall`.
    let view = skill_tool
        .present_call(&serde_json::json!({ "name": "research-only" }))
        .unwrap();
    assert_eq!(
        view,
        ToolCallView::Generic {
            title: "Load skill research-only".into(),
            kind: Some(ToolCallKind::Read),
            raw_input: Some(serde_json::Value::String("research-only".into())),
        }
    );

    // Skill load through the session-scoped tool renders the upstream
    // `<skill_content>` block, not just the slash summary.
    let loaded = skill_tool
        .execute_with_session(
            ToolInvocation {
                call_id: "call-skill".into(),
                name: "skill".into(),
                arguments: serde_json::json!({ "name": "research-only" }),
            },
            Some(session.clone()),
        )
        .await
        .unwrap();
    assert!(loaded.ok);
    assert!(loaded
        .output
        .contains("<skill_content name=\"research-only\">"));
    assert!(loaded.output.contains("Research body."));

    // Registry/cache event: the standing provider registered exactly once.
    let registered = wait_for_event(&mut events, SkillRegistryEventKind::ProviderRegistered).await;
    assert_eq!(registered.provider.as_deref(), Some("filesystem"));

    // `observeHostMutation` behind `fs/observed`: a first-party write to the
    // skill file invalidates the exact provider and bumps the revision.
    let provider = provider_cell.lock().unwrap().clone().unwrap();
    let before = registry.revision();
    provider.observe_fs_event(&skill_dir.join("research-only").join("SKILL.md"), "write");
    let invalidated =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderInvalidated).await;
    assert_eq!(invalidated.provider.as_deref(), Some("filesystem"));
    assert!(registry.revision() > before);

    // Disposal unregisters the exact standing provider and empties the scope.
    drop(store);
    drop(session);
    let unregistered =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderUnregistered).await;
    assert_eq!(unregistered.provider.as_deref(), Some("filesystem"));
    let after = skill_tool.slash_snapshot_for(Some(scope)).await.unwrap();
    assert!(after.entries.is_empty());
}

#[tokio::test]
async fn collect_cache_evicts_in_insertion_order_and_respects_capacity() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let registry = SkillRegistry::with_collect_cache_max_entries(2);
    let calls = Arc::new(AtomicUsize::new(0));
    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SkillProvider for CountingProvider {
        fn name(&self) -> &str {
            "counting"
        }

        async fn list(
            &self,
            _options: &SkillLookupOptions,
        ) -> anyhow::Result<SkillProviderObservation> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(SkillProviderObservation::complete(vec![SkillCandidate {
                name: "alpha".into(),
                description: "Alpha".into(),
                when_to_use: None,
                invocation: SkillInvocationPolicy::default(),
                source: "test".into(),
                provider: "counting".into(),
                resource_base: None,
                rank: 100,
                locator: serde_json::Value::Null,
                path: None,
                metadata: None,
            }]))
        }

        async fn get(
            &self,
            _candidate: &SkillCandidate,
            _options: &SkillLookupOptions,
        ) -> anyhow::Result<Option<SkillDefinition>> {
            Ok(None)
        }
    }
    registry
        .register_provider(Arc::new(CountingProvider {
            calls: calls.clone(),
        }))
        .unwrap();

    let a = SkillViewOptions {
        cwd: Some(PathBuf::from("/a")),
        scope: None,
        ..Default::default()
    };
    let b = SkillViewOptions {
        cwd: Some(PathBuf::from("/b")),
        scope: None,
        ..Default::default()
    };
    let c = SkillViewOptions {
        cwd: Some(PathBuf::from("/c")),
        scope: None,
        ..Default::default()
    };

    assert_eq!(skill_names(&registry, &a).await, ["alpha"]);
    assert_eq!(skill_names(&registry, &b).await, ["alpha"]);
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    // A third distinct cwd key overflows capacity 2 and evicts the oldest
    // insertion, `/a`, exactly like upstream's `Map.keys().next()`.
    assert_eq!(skill_names(&registry, &c).await, ["alpha"]);
    assert_eq!(calls.load(Ordering::Relaxed), 3);

    // `/b` was retained (second-oldest); `/a` was evicted and re-collects.
    assert_eq!(skill_names(&registry, &b).await, ["alpha"]);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(skill_names(&registry, &a).await, ["alpha"]);
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[tokio::test]
async fn disposed_control_invalidate_is_a_silent_no_op() {
    let registry = SkillRegistry::new();
    let mut events = registry.subscribe();
    let control_cell = Arc::new(std::sync::Mutex::new(None::<SkillProviderControl>));
    let control_cell_for_create = control_cell.clone();

    let registration = registry
        .register_provider_scoped_with_control(|control| {
            *control_cell_for_create.lock().unwrap() = Some(control.clone());
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: Path::new("/tmp/dsh").into(),
                agents_home: Path::new("/tmp/agents").into(),
                custom_skill_dirs: Vec::new(),
                bundled_skill_dir: None,
            })?;
            Ok(Arc::new(provider))
        })
        .unwrap();

    let registered = wait_for_event(&mut events, SkillRegistryEventKind::ProviderRegistered).await;
    assert_eq!(registered.provider.as_deref(), Some("filesystem"));
    let control = control_cell.lock().unwrap().clone().unwrap();

    registration.close();
    let unregistered =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderUnregistered).await;
    assert_eq!(unregistered.provider.as_deref(), Some("filesystem"));
    let revision_after_disposal = registry.revision();

    assert!(control.is_cancelled());
    // Upstream `SkillProviderControl.invalidate` is a silent no-op once the
    // registration is disposed: no error, no revision bump, no event.
    control.invalidate().unwrap();
    assert_eq!(registry.revision(), revision_after_disposal);
    while let Ok(event) = events.try_recv() {
        assert_ne!(event.kind, SkillRegistryEventKind::ProviderInvalidated);
    }
}

#[tokio::test]
async fn ancestor_creation_invalidates_through_standing_preset_and_slash_catalog() {
    let home = tempfile::tempdir().unwrap();
    let presets_root = home.path().join("presets");
    fs::create_dir_all(&presets_root).unwrap();

    let registry = Arc::new(SkillRegistry::new());
    let mut events = registry.subscribe();
    let registry_for_factory = registry.clone();
    let presets_root_for_factory = presets_root.clone();
    let home_root = home.path().to_path_buf();
    let factory: StandingProviderFactory = Arc::new(move |preset_id: &str, scope: &SkillScope| {
        let skill_dir = presets_root_for_factory.join(preset_id);
        registry_for_factory.register_provider_for_scope_with_control(scope.clone(), |control| {
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: false,
                dsh_home: home_root.join(".dsh"),
                agents_home: home_root.join(".agents"),
                custom_skill_dirs: vec![skill_dir.clone()],
                bundled_skill_dir: None,
            })?
            .with_watching(
                control.clone(),
                SkillWatchConfig {
                    poll_interval: std::time::Duration::from_millis(10),
                },
            );
            Ok(Arc::new(provider))
        })
    });

    let sessions = tempfile::tempdir().unwrap();
    let store = SessionStore::open_with_default_preset_and_provider(
        sessions.path(),
        "research",
        Some(factory),
    )
    .unwrap();
    let session = store
        .create_in_space_and_harness("Research", None, None, Some("research".into()))
        .unwrap();
    let scope = session.skill_scope().unwrap();
    let skill_tool = SkillTool::new(registry.clone());

    // The root directory does not exist yet: the catalog is empty.
    let initial = skill_tool
        .slash_snapshot_for(Some(scope.clone()))
        .await
        .unwrap();
    assert!(initial.entries.is_empty());
    let registered = wait_for_event(&mut events, SkillRegistryEventKind::ProviderRegistered).await;
    assert_eq!(registered.provider.as_deref(), Some("filesystem"));

    // Create the root as an EMPTY directory. A contents-only fingerprint would
    // hash this identically to a missing root; the ancestor-aware watcher must
    // observe the ancestor change and invalidate.
    fs::create_dir_all(presets_root.join("research")).unwrap();
    let invalidated =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderInvalidated).await;
    assert_eq!(invalidated.provider.as_deref(), Some("filesystem"));
    let after_empty = skill_tool
        .slash_snapshot_for(Some(scope.clone()))
        .await
        .unwrap();
    assert!(after_empty.entries.is_empty());
    assert!(after_empty.revision > initial.revision);

    // Adding a skill under the root triggers a second invalidation and the
    // session-scoped slash catalog refreshes to include it.
    bundled_skill(
        &presets_root.join("research"),
        "ancestor-skill",
        "Ancestor skill",
        "Body.",
    );
    let invalidated_again =
        wait_for_event(&mut events, SkillRegistryEventKind::ProviderInvalidated).await;
    assert_eq!(invalidated_again.provider.as_deref(), Some("filesystem"));
    let after_skill = skill_tool
        .slash_snapshot_for(Some(scope.clone()))
        .await
        .unwrap();
    assert_eq!(
        after_skill
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["ancestor-skill"]
    );

    drop(store);
    drop(session);
}
