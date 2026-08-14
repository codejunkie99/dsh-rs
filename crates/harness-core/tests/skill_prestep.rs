use harness_core::agent::AgentLoop;
use harness_core::events::EventKind;
use harness_core::llm::{LlmAdapter, LlmRequest, StreamFrame};
use harness_core::session::SessionLog;
use harness_core::skills::{SkillInvocationPolicy, SkillRegistry};
use harness_core::tools::skill::{render_skill_catalog, SkillCatalogEntry, SkillTool};
use harness_core::tools::ToolRegistry;
use std::sync::Arc;
use uuid::Uuid;

fn skill_registry() -> Arc<SkillRegistry> {
    let mut registry = SkillRegistry::new();
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
