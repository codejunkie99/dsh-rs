use harness_core::agent::AgentLoop;
use harness_core::events::EventKind;
use harness_core::llm::{LlmAdapter, LlmRequest, StreamFrame};
use harness_core::session::SessionLog;
use harness_core::skills::{
    FileSystemSkillProvider, SkillFileSystemConfig, SkillInvocationPolicy, SkillRegistry,
    SkillScope, SkillViewOptions, SkillWatchConfig,
};
use harness_core::tools::skill::{
    render_skill_catalog, render_skill_catalog_update, SkillCatalogEntry, SkillSlashEntry,
    SkillTool,
};
use harness_core::tools::ToolRegistry;
use std::sync::Arc;
use uuid::Uuid;

fn skill_registry() -> Arc<SkillRegistry> {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "a-skill",
            "Use   <models> & carefully.",
            "Follow the A instructions.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    registry
        .register_runtime(
            "user-only",
            "Only a human may invoke this.",
            "Follow the user-only instructions.",
            SkillInvocationPolicy {
                model_invocable: false,
                user_invocable: true,
            },
        )
        .unwrap();
    registry
        .register_runtime(
            "model-only",
            "Only the model may invoke this.",
            "Follow the model-only instructions.",
            SkillInvocationPolicy {
                model_invocable: true,
                user_invocable: false,
            },
        )
        .unwrap();
    Arc::new(registry)
}

#[tokio::test]
async fn skill_catalog_filters_normalizes_and_renders_the_dsh_contract() {
    let tool = SkillTool::new(skill_registry());
    let entries = tool.catalog_entries().await.unwrap();
    assert_eq!(
        entries,
        vec![
            SkillCatalogEntry {
                name: "a-skill".into(),
                description: "Use <models> & carefully.".into(),
            },
            SkillCatalogEntry {
                name: "model-only".into(),
                description: "Only the model may invoke this.".into(),
            },
        ]
    );

    let rendered = render_skill_catalog(&entries);
    assert_eq!(
        rendered,
        [
            "<system-reminder>",
            "A skill is a reusable set of task-specific instructions. The following skills are available in this session:",
            "",
            "<available_skills>",
            "- `a-skill`: Use &lt;models&gt; &amp; carefully.",
            "- `model-only`: Only the model may invoke this.",
            "</available_skills>",
            "",
            "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.",
            "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
            "</system-reminder>",
        ]
        .join("\n")
    );
}

#[tokio::test]
async fn slash_entries_include_user_invocable_skills_and_mark_model_visibility() {
    let tool = SkillTool::new(skill_registry());
    let entries = tool.slash_entries().await.unwrap();

    assert_eq!(
        entries,
        vec![
            SkillSlashEntry {
                name: "a-skill".into(),
                description: "Use   <models> & carefully.".into(),
                model_invocable: true,
            },
            SkillSlashEntry {
                name: "user-only".into(),
                description: "Only a human may invoke this.".into(),
                model_invocable: false,
            },
        ]
    );
}

#[test]
fn skill_catalog_updates_render_the_exact_dsh_contract() {
    let replacement = render_skill_catalog_update(&[SkillCatalogEntry {
        name: "replacement".into(),
        description: "Replacement & guide".into(),
    }]);
    assert_eq!(
        replacement,
        [
            "<system-reminder>",
            "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:",
            "",
            "<available_skills>",
            "- `replacement`: Replacement &amp; guide",
            "</available_skills>",
            "",
            "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact skill name before acting.",
            "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
            "</system-reminder>",
        ]
        .join("\n")
    );

    let tombstone = render_skill_catalog_update(&[]);
    assert_eq!(
        tombstone,
        [
            "<system-reminder>",
            "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:",
            "",
            "<available_skills>",
            "</available_skills>",
            "",
            "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.",
            "A user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it.",
            "</system-reminder>",
        ]
        .join("\n")
    );
}

struct RecordingAdapter {
    requests: std::sync::Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmAdapter for RecordingAdapter {
    async fn stream(
        &self,
        request: LlmRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamFrame>> {
        self.requests.lock().unwrap().push(request);
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let message_id = Uuid::new_v4();
        tokio::spawn(async move {
            let _ = sender
                .send(StreamFrame::Delta {
                    message_id,
                    text: "ok".into(),
                })
                .await;
            let _ = sender
                .send(StreamFrame::Done {
                    stop_reason: Some("stop".into()),
                    usage: None,
                })
                .await;
        });
        Ok(receiver)
    }
}

fn user_contents(request: &LlmRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            harness_core::llm::ChatMessage::User { content } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn agent_publishes_catalog_once_and_injects_user_invocable_skills() {
    let registry = skill_registry();
    let skill_tool = Arc::new(SkillTool::new(registry));
    let mut tools = ToolRegistry::new();
    tools.register(skill_tool.clone());
    let adapter = Arc::new(RecordingAdapter {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::new(adapter.clone(), Arc::new(tools))
        .with_skill_tool(skill_tool)
        .with_max_steps(1);
    let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
    let input = "Use /a-skill and /user-only not /model-only or /no-skill";

    agent.run_turn(log.clone(), input.into()).await.unwrap();

    let events = log.events();
    let catalog_events = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::SkillCatalogPublished { .. }))
        .count();
    assert_eq!(catalog_events, 1);
    let invocations: Vec<String> = events
        .iter()
        .filter_map(|event| {
            if let EventKind::SkillInvocationInjected { name, .. } = &event.kind {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(invocations, ["a-skill", "user-only"]);

    let requests = adapter.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let users = user_contents(&requests[0]);
    assert_eq!(users.len(), 4);
    assert_eq!(users[0], input);
    assert!(users[1].contains("<available_skills>"));
    assert!(users[1].contains("- `a-skill`: Use &lt;models&gt; &amp; carefully."));
    assert!(users[2].contains("<skill_content name=\"a-skill\">"));
    assert!(users[2].contains("Follow the A instructions."));
    assert!(users[3].contains("<skill_content name=\"user-only\">"));
    assert!(!users
        .join("\n")
        .contains("<skill_content name=\"model-only\">"));

    agent
        .run_turn(log.clone(), "again /a-skill".into())
        .await
        .unwrap();
    let events = log.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::SkillCatalogPublished { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::SkillInvocationInjected { .. }))
            .count(),
        3
    );
    let requests = adapter.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(user_contents(&requests[1])
        .join("\n")
        .contains("<skill_content name=\"a-skill\">"));
}

#[tokio::test]
async fn agent_replaces_changed_catalogs_and_publishes_an_empty_tombstone() {
    let home = tempfile::tempdir().unwrap();
    let skills = home.path().join(".dsh/skills");
    std::fs::create_dir_all(skills.join("first-skill")).unwrap();
    std::fs::write(
        skills.join("first-skill/SKILL.md"),
        "---\nname: first-skill\ndescription: First skill\n---\n\nFirst body.\n",
    )
    .unwrap();

    let registry = SkillRegistry::new();
    let _registration = registry
        .register_provider_scoped_with_control(|control| {
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: true,
                dsh_home: home.path().join(".dsh"),
                agents_home: home.path().join(".agents"),
                custom_skill_dirs: Vec::new(),
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
    let registry_for_wait = registry.clone();
    let skill_tool = Arc::new(SkillTool::new(Arc::new(registry)));
    let mut tools = ToolRegistry::new();
    tools.register(skill_tool.clone());
    let adapter = Arc::new(RecordingAdapter {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let adapter_handle = adapter.clone();
    let agent = AgentLoop::new(adapter, Arc::new(tools))
        .with_skill_tool(skill_tool)
        .with_max_steps(1);
    let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));

    agent
        .run_turn(log.clone(), "first turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 1);
    assert_eq!(
        catalogs[0],
        SkillCatalogPublished {
            update: false,
            entries: vec![SkillCatalogEntry {
                name: "first-skill".into(),
                description: "First skill".into(),
            }],
            content: render_skill_catalog(&[SkillCatalogEntry {
                name: "first-skill".into(),
                description: "First skill".into(),
            }]),
        }
    );

    std::fs::create_dir_all(skills.join("second-skill")).unwrap();
    std::fs::write(
        skills.join("second-skill/SKILL.md"),
        "---\nname: second-skill\ndescription: Second skill\n---\n\nSecond body.\n",
    )
    .unwrap();
    wait_for_catalog(&registry_for_wait, 2).await;
    agent
        .run_turn(log.clone(), "second turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 2);
    assert_eq!(
        catalogs[1],
        SkillCatalogPublished {
            update: true,
            entries: vec![
                SkillCatalogEntry {
                    name: "first-skill".into(),
                    description: "First skill".into(),
                },
                SkillCatalogEntry {
                    name: "second-skill".into(),
                    description: "Second skill".into(),
                }
            ],
            content: render_skill_catalog_update(&[
                SkillCatalogEntry {
                    name: "first-skill".into(),
                    description: "First skill".into(),
                },
                SkillCatalogEntry {
                    name: "second-skill".into(),
                    description: "Second skill".into(),
                }
            ]),
        }
    );
    let requests = adapter_handle.requests.lock().unwrap().clone();
    let model_catalogs = user_contents(&requests[1])
        .into_iter()
        .filter(|content| {
            content.contains("A skill is a reusable")
                || content.contains("The available skill catalog changed")
        })
        .collect::<Vec<_>>();
    assert_eq!(model_catalogs.len(), 1);
    assert!(model_catalogs[0].contains("The available skill catalog changed"));
    assert!(!model_catalogs[0].contains("A skill is a reusable"));

    std::fs::remove_dir_all(skills.join("first-skill")).unwrap();
    std::fs::remove_dir_all(skills.join("second-skill")).unwrap();
    wait_for_catalog(&registry_for_wait, 0).await;
    agent
        .run_turn(log.clone(), "third turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 3);
    assert_eq!(
        catalogs[2],
        SkillCatalogPublished {
            update: true,
            entries: Vec::new(),
            content: render_skill_catalog_update(&[]),
        }
    );
    assert!(catalogs[2]
        .content
        .contains("No skills are currently available"));
    assert!(!catalogs[2].content.contains("first-skill"));
    assert!(!catalogs[2].content.contains("second-skill"));
    let requests = adapter_handle.requests.lock().unwrap().clone();
    let model_catalogs = user_contents(&requests[2])
        .into_iter()
        .filter(|content| {
            content.contains("A skill is a reusable")
                || content.contains("The available skill catalog changed")
        })
        .collect::<Vec<_>>();
    assert_eq!(model_catalogs.len(), 1);
    assert!(model_catalogs[0].contains("No skills are currently available"));

    agent
        .run_turn(log.clone(), "fourth turn".into())
        .await
        .unwrap();
    assert_eq!(published_catalogs(&log).len(), 3);
}

async fn wait_for_catalog(registry: &SkillRegistry, expected: usize) {
    for _ in 0..200 {
        let summaries = registry.list(&SkillViewOptions::default()).await.unwrap();
        if summaries.len() == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("skill catalog did not refresh to {expected} entries in time");
}

fn published_catalogs(log: &Arc<SessionLog>) -> Vec<SkillCatalogPublished> {
    log.events()
        .iter()
        .filter_map(|event| {
            if let EventKind::SkillCatalogPublished {
                entries,
                content,
                update,
            } = &event.kind
            {
                Some(SkillCatalogPublished {
                    entries: entries.clone(),
                    content: content.clone(),
                    update: *update,
                })
            } else {
                None
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillCatalogPublished {
    entries: Vec<SkillCatalogEntry>,
    content: String,
    update: bool,
}

#[tokio::test]
async fn skill_catalog_and_injection_follow_tool_visibility() {
    let tools = ToolRegistry::new();
    let adapter = Arc::new(RecordingAdapter {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::new(adapter, Arc::new(tools)).with_max_steps(1);
    let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));

    agent
        .run_turn(log.clone(), "use /a-skill".into())
        .await
        .unwrap();

    assert!(log.events().iter().all(|event| {
        !matches!(
            event.kind,
            EventKind::SkillCatalogPublished { .. } | EventKind::SkillInvocationInjected { .. }
        )
    }));
}

#[tokio::test]
async fn skill_catalog_follows_exact_identity_tool_visibility_by_scope() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "a-skill",
            "A skill",
            "Body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();
    let skill_tool = Arc::new(SkillTool::new(Arc::new(registry)));

    let preset_a = SkillScope::new();
    let preset_b = SkillScope::new();

    let mut tools = ToolRegistry::new();
    // The `skill` tool is registered only in preset B's scope. A session on
    // preset A must not publish the catalog, even though the skill tool object
    // exists — exact-identity `get(name, scope)` reads it as absent.
    tools
        .register_for_scope(preset_b.clone(), skill_tool.clone())
        .unwrap();

    let adapter = Arc::new(RecordingAdapter {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::new(adapter, Arc::new(tools))
        .with_skill_tool(skill_tool)
        .with_max_steps(1);

    let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
    let agent_scope = SkillScope::new();
    let binding = agent_scope.bind_parent(preset_a.clone()).unwrap();
    log.set_skill_scope(agent_scope.clone());

    agent
        .run_turn(log.clone(), "first turn".into())
        .await
        .unwrap();
    assert!(published_catalogs(&log).is_empty());

    // Re-linking the session to preset B makes the scoped `skill` tool visible,
    // so the catalog publishes.
    binding.rebind(preset_b.clone()).unwrap();
    agent
        .run_turn(log.clone(), "second turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 1);
    assert!(!catalogs[0].update);
    assert_eq!(
        catalogs[0]
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["a-skill"]
    );
}

#[tokio::test]
async fn session_resolved_skills_change_after_preset_recomposition() {
    let registry = SkillRegistry::new();
    registry
        .register_runtime(
            "global",
            "Global skill",
            "Global body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let preset_a = SkillScope::new();
    registry
        .register_runtime_for_scope(
            preset_a.clone(),
            "a-only",
            "Preset A skill",
            "A body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    let preset_b = SkillScope::new();
    registry
        .register_runtime_for_scope(
            preset_b.clone(),
            "b-only",
            "Preset B skill",
            "B body.",
            SkillInvocationPolicy::default(),
        )
        .unwrap();

    // The session owns its agent scope key, initially parented to preset A,
    // mirroring the standing mount join installed during agent creation.
    let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
    let agent_scope = SkillScope::new();
    let binding = agent_scope.bind_parent(preset_a.clone()).unwrap();
    log.set_skill_scope(agent_scope.clone());

    let skill_tool = Arc::new(SkillTool::new(Arc::new(registry)));
    let mut tools = ToolRegistry::new();
    tools.register(skill_tool.clone());
    let adapter = Arc::new(RecordingAdapter {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::new(adapter.clone(), Arc::new(tools))
        .with_skill_tool(skill_tool)
        .with_max_steps(1);

    agent
        .run_turn(log.clone(), "first turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 1);
    assert!(!catalogs[0].update);
    assert_eq!(
        catalogs[0]
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["a-only", "global"]
    );

    // Recomposition re-links the still-blank session's scope key to preset B.
    binding.rebind(preset_b.clone()).unwrap();

    agent
        .run_turn(log.clone(), "second turn".into())
        .await
        .unwrap();
    let catalogs = published_catalogs(&log);
    assert_eq!(catalogs.len(), 2);
    assert!(catalogs[1].update);
    assert_eq!(
        catalogs[1]
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["b-only", "global"]
    );

    // The recomposed catalog reaches the model as the replacement list.
    let requests = adapter.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let model_catalog = user_contents(&requests[1])
        .into_iter()
        .find(|content| content.contains("The available skill catalog changed"))
        .expect("recomposed catalog should reach the model");
    assert!(model_catalog.contains("- `b-only`: Preset B skill"));
    assert!(model_catalog.contains("- `global`: Global skill"));
    assert!(!model_catalog.contains("a-only"));
}
