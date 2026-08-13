use crate::approval::{ApprovalDecision, ApprovalPolicy, PolicyApprover, ToolApprover};
use crate::events::{EventKind, TurnCompletionReason, Usage};
use crate::llm::{ChatMessage, LlmAdapter, LlmRequest, StreamFrame, ToolCallRequest};
use crate::session::SessionLog;
use crate::tools::{ToolInvocation, ToolRegistry};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AgentLoop {
    llm: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    approver: Arc<dyn ToolApprover>,
    default_model: Option<String>,
    max_steps: u64,
}

impl AgentLoop {
    pub fn new(llm: Arc<dyn LlmAdapter>, tools: Arc<ToolRegistry>) -> Self {
        Self {
            llm,
            tools,
            approver: Arc::new(PolicyApprover::new(ApprovalPolicy::default())),
            default_model: None,
            max_steps: 24,
        }
    }

    pub fn with_default_model(mut self, model: Option<String>) -> Self {
        self.default_model = model;
        self
    }

    pub fn with_max_steps(mut self, max_steps: u64) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn with_approver(mut self, approver: Arc<dyn ToolApprover>) -> Self {
        self.approver = approver;
        self
    }

    /// Runs one complete turn: one user input, zero or more model steps,
    /// and tool calls until the model stops or the step budget is exhausted.
    pub async fn run_turn(&self, log: Arc<SessionLog>, input: String) -> anyhow::Result<()> {
        log.append(EventKind::TurnStarted)?;
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: input,
        })?;

        let mut completion = TurnCompletionReason::Natural;

        for step in 0..self.max_steps {
            log.append(EventKind::StepStarted { index: step })?;

            let history = Self::derive_model_history(&log.events());
            let request = LlmRequest {
                messages: history,
                tools: self.tools.specs(),
                model: self.default_model.clone(),
                metadata: BTreeMap::new(),
            };

            let mut stream = self.llm.stream(request).await?;
            let mut assistant_text = String::new();
            let mut assistant_id = Uuid::new_v4();
            let mut stop_reason: Option<String> = None;
            let mut usage: Option<Usage> = None;
            let mut tool_calls = Vec::new();
            let mut stream_error: Option<String> = None;

            while let Some(frame) = stream.recv().await {
                match frame {
                    StreamFrame::Delta { message_id, text } => {
                        assistant_id = message_id;
                        assistant_text.push_str(&text);
                        log.append(EventKind::AssistantChunk {
                            message_id,
                            delta: text,
                        })?;
                    }
                    StreamFrame::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        tool_calls.push(crate::events::EventKind::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        });
                    }
                    StreamFrame::Done {
                        stop_reason: reason,
                        usage: token_usage,
                    } => {
                        stop_reason = reason;
                        usage = token_usage;
                    }
                    StreamFrame::Error { message } => {
                        stream_error = Some(message);
                        break;
                    }
                }
            }

            if let Some(message) = stream_error {
                log.append(EventKind::ErrorNoted {
                    message,
                    fatal: false,
                })?;
                completion = TurnCompletionReason::Errored;
                break;
            }

            log.append(EventKind::AssistantMessage {
                id: assistant_id,
                content: assistant_text,
                stop_reason: stop_reason.clone(),
                usage,
            })?;

            if tool_calls.is_empty() {
                log.append(EventKind::StepCompleted)?;
                break;
            }

            for call in tool_calls {
                log.append(call.clone())?;
                if let EventKind::ToolCall {
                    id,
                    name,
                    arguments,
                } = call
                {
                    self.execute_tool(
                        log.clone(),
                        ToolInvocation {
                            call_id: id,
                            name,
                            arguments,
                        },
                    )
                    .await?;
                }
            }

            log.append(EventKind::StepCompleted)?;
        }

        log.append(EventKind::TurnCompleted { reason: completion })?;
        Ok(())
    }

    async fn execute_tool(
        &self,
        log: Arc<SessionLog>,
        invocation: ToolInvocation,
    ) -> anyhow::Result<()> {
        log.append(EventKind::ApprovalRequested {
            call_id: invocation.call_id.clone(),
            tool_name: invocation.name.clone(),
            arguments: invocation.arguments.clone(),
        })?;
        let decision = self
            .approver
            .decide(&invocation.call_id, &invocation.name, &invocation.arguments)
            .await;
        let (approved, reason): (bool, String) = match decision {
            ApprovalDecision::Allow => (true, "allowed by approval policy".into()),
            ApprovalDecision::Ask => (
                false,
                "tool requires explicit approval; no approval was provided".into(),
            ),
            ApprovalDecision::Deny => (false, "denied by approval policy".into()),
        };
        log.append(EventKind::ApprovalResolved {
            call_id: invocation.call_id.clone(),
            approved,
            reason: reason.clone(),
        })?;

        let call_id = invocation.call_id.clone();
        let output = if approved {
            self.tools.execute(invocation).await
        } else {
            crate::tools::ToolOutput {
                ok: false,
                output: reason,
            }
        };
        log.append(EventKind::ToolResult {
            call_id,
            ok: output.ok,
            output: output.output,
        })?;
        Ok(())
    }

    fn derive_model_history(events: &[crate::events::SessionEvent]) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        for event in events {
            match &event.kind {
                EventKind::UserMessage { content, .. } => messages.push(ChatMessage::User {
                    content: content.clone(),
                }),
                EventKind::AssistantMessage { content, .. } => {
                    messages.push(ChatMessage::Assistant {
                        content: content.clone(),
                    });
                }
                EventKind::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    let tool_call = ToolCallRequest {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    };
                    match messages.last_mut() {
                        Some(ChatMessage::Assistant { content }) => {
                            let content = std::mem::take(content);
                            *messages.last_mut().unwrap() = ChatMessage::AssistantToolCall {
                                content,
                                tool_calls: vec![tool_call],
                            };
                        }
                        Some(ChatMessage::AssistantToolCall { tool_calls, .. }) => {
                            tool_calls.push(tool_call);
                        }
                        _ => messages.push(ChatMessage::AssistantToolCall {
                            content: String::new(),
                            tool_calls: vec![tool_call],
                        }),
                    }
                }
                EventKind::ToolResult {
                    call_id, output, ..
                } => messages.push(ChatMessage::Tool {
                    tool_call_id: call_id.clone(),
                    content: output.clone(),
                }),
                _ => {}
            }
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalPolicy, ApprovalRule, PolicyApprover};
    use crate::llm::NullAdapter;
    use crate::tools::EchoTool;
    use crate::tools::ToolInvocation;

    #[tokio::test]
    async fn null_adapter_completes_one_turn() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let loop_ = AgentLoop::new(Arc::new(NullAdapter), Arc::new(tools));
        loop_.run_turn(log.clone(), "hello".into()).await.unwrap();

        let events = log.events();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match &e.kind {
                EventKind::TurnStarted => "turn_started",
                EventKind::UserMessage { .. } => "user_message",
                EventKind::StepStarted { .. } => "step_started",
                EventKind::AssistantChunk { .. } => "assistant_chunk",
                EventKind::AssistantMessage { .. } => "assistant_message",
                EventKind::StepCompleted => "step_completed",
                EventKind::TurnCompleted { .. } => "turn_completed",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"turn_started"));
        assert!(kinds.contains(&"user_message"));
        assert!(kinds.contains(&"assistant_message"));
        assert!(kinds.contains(&"turn_completed"));
    }

    #[test]
    fn model_history_projects_assistant_tool_calls_before_results() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        log.append(EventKind::UserMessage {
            id: Uuid::new_v4(),
            content: "use the tool".into(),
        })
        .unwrap();
        log.append(EventKind::AssistantMessage {
            id: Uuid::new_v4(),
            content: "I will call the tool.".into(),
            stop_reason: Some("tool_calls".into()),
            usage: None,
        })
        .unwrap();
        log.append(EventKind::ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"text": "ok"}),
        })
        .unwrap();
        log.append(EventKind::ToolResult {
            call_id: "call_1".into(),
            ok: true,
            output: "ok".into(),
        })
        .unwrap();

        let history = AgentLoop::derive_model_history(&log.events());
        assert_eq!(history.len(), 3);
        assert!(matches!(
            history[1],
            ChatMessage::AssistantToolCall { ref tool_calls, .. }
                if tool_calls.len() == 1 && tool_calls[0].id == "call_1"
        ));
        assert!(matches!(
            history[2],
            ChatMessage::Tool { ref tool_call_id, .. } if tool_call_id == "call_1"
        ));
    }

    #[tokio::test]
    async fn unresolved_approval_requests_are_recorded_and_fail_closed() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut policy = ApprovalPolicy::default();
        policy.tools.insert("echo".into(), ApprovalRule::Ask);
        let agent = AgentLoop::new(Arc::new(NullAdapter), Arc::new(tools))
            .with_approver(Arc::new(PolicyApprover::new(policy)));

        agent
            .execute_tool(
                log.clone(),
                ToolInvocation {
                    call_id: "call_approval".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "sensitive"}),
                },
            )
            .await
            .unwrap();

        let events = log.events();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ApprovalRequested { call_id, tool_name, .. }
                if call_id == "call_approval" && tool_name == "echo"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ApprovalResolved { call_id, approved, .. }
                if call_id == "call_approval" && !approved
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ToolResult { call_id, ok, output }
                if call_id == "call_approval" && !ok && output.contains("requires explicit approval")
        )));
    }
}
