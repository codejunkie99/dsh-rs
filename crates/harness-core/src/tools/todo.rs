use crate::events::{EventKind, TodoItem, TodoStatus};
use crate::session::SharedSessionLog;
use crate::tools::{Tool, ToolInvocation, ToolOutput, ToolSpec};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashSet;

pub struct TodoTool {
    allow_parallel_in_progress: bool,
}

impl TodoTool {
    pub fn new(allow_parallel_in_progress: bool) -> Self {
        Self {
            allow_parallel_in_progress,
        }
    }

    fn description(&self) -> String {
        let active_policy = if self.allow_parallel_in_progress {
            "Mark every todo being actively worked on `in_progress` — several at once when work \
             genuinely runs in parallel (e.g. concurrent subagents or background commands), one \
             for sequential work; while work remains, at least one task should be `in_progress`. "
        } else {
            "Keep AT MOST ONE todo `in_progress` at a time; while work remains, exactly one \
             active task should be `in_progress`. "
        };
        format!(
            "Record and update a structured task list for the current work. Send the ENTIRE list \
             every call — it REPLACES the previous list (there are no partial updates, no per-item \
             edits). Use it to plan multi-step work and show progress: add one todo per concrete \
             step before you start. {active_policy}Mark a todo `completed` the moment it is done \
             (do not batch completions), and allow no `in_progress` item only once all work is \
             complete. Skip the list for trivial single-step tasks. Statuses: `pending` (not \
             started), `in_progress` (being worked on now), `completed` (finished)."
        )
    }

    fn canonicalize(&self, raw: Vec<TodoInput>) -> Result<Vec<TodoItem>> {
        let mut todos = Vec::with_capacity(raw.len());
        let mut seen = HashSet::new();
        let mut active = 0;
        for item in raw {
            let content = item.content.trim().to_string();
            if content.is_empty() {
                bail!("invalid todo: `content` must be a non-empty string");
            }
            if !seen.insert(content.clone()) {
                bail!("invalid todos: duplicate content {:?}", content);
            }
            if item.status == TodoStatus::InProgress {
                active += 1;
            }
            todos.push(TodoItem {
                content,
                status: item.status,
            });
        }
        if !self.allow_parallel_in_progress && active > 1 {
            bail!("invalid todos: at most one task may be in_progress (got {active})");
        }
        Ok(todos)
    }

    async fn write(
        &self,
        invocation: ToolInvocation,
        session: Option<SharedSessionLog>,
    ) -> Result<ToolOutput> {
        let arguments: TodoWriteArguments = serde_json::from_value(invocation.arguments.clone())?;
        let todos = self.canonicalize(arguments.todos)?;
        let Some(session) = session else {
            bail!("todo_write requires an owning agent session");
        };
        session.append(EventKind::TodoWrite {
            todos: todos.clone(),
        })?;

        let completed = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        let in_progress = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count();
        let pending = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Pending)
            .count();
        // Upstream `tool-todo` renders the model-facing result as a single
        // human-readable line, not the raw canonical value JSON
        // (`packages/todo/tool-todo/src/index.ts`, `output.render`):
        // `Updated todo list: {pending} pending, {inProgress} in progress,
        // {completed} completed.`
        let output = format!(
            "Updated todo list: {pending} pending, {in_progress} in progress, {completed} completed."
        );
        Ok(ToolOutput { ok: true, output })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoWriteArguments {
    todos: Vec<TodoInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoInput {
    content: String,
    status: TodoStatus,
}

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo_write".into(),
            description: self.description(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The COMPLETE task list, replacing any previous list.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "What the task is — a short imperative line."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "pending (not started) | in_progress (now) | completed (done)."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        self.write(invocation, None).await
    }

    async fn execute_with_session(
        &self,
        invocation: ToolInvocation,
        session: Option<SharedSessionLog>,
    ) -> Result<ToolOutput> {
        self.write(invocation, session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventKind, TodoItem, TodoStatus};
    use crate::session::SessionLog;
    use crate::tools::{Tool, ToolInvocation, ToolRegistry};
    use std::sync::Arc;
    use uuid::Uuid;

    fn invocation(todos: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            call_id: "call_1".into(),
            name: "todo_write".into(),
            arguments: serde_json::json!({ "todos": todos }),
        }
    }

    fn execute_through_registry(
        allow_parallel: bool,
        todos: serde_json::Value,
        session: Option<Arc<SessionLog>>,
    ) -> crate::tools::ToolOutput {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TodoTool::new(allow_parallel)));
        futures::executor::block_on(registry.execute_with_session(invocation(todos), session))
    }

    #[test]
    fn todo_schema_matches_the_dsh_tool_contract() {
        let spec = TodoTool::new(true).spec();
        assert_eq!(spec.name, "todo_write");
        assert!(spec.description.contains("ENTIRE list"));
        assert!(spec.description.contains("several at once"));
        assert!(TodoTool::new(false)
            .spec()
            .description
            .contains("AT MOST ONE"));
        assert_eq!(
            spec.parameters,
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The COMPLETE task list, replacing any previous list.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "What the task is — a short imperative line."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "pending (not started) | in_progress (now) | completed (done)."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            })
        );
    }

    #[tokio::test]
    async fn todo_write_trims_replaces_and_persists_the_whole_list() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let first = execute_through_registry(
            true,
            serde_json::json!([{"content": "  plan the work  ", "status": "pending"}]),
            Some(log.clone()),
        );
        assert!(first.ok);
        assert_eq!(
            log.events()[0].kind,
            EventKind::TodoWrite {
                todos: vec![TodoItem {
                    content: "plan the work".into(),
                    status: TodoStatus::Pending
                }]
            }
        );

        let second = execute_through_registry(
            true,
            serde_json::json!([
                {"content": "plan the work", "status": "completed"},
                {"content": "build", "status": "in_progress"}
            ]),
            Some(log.clone()),
        );
        assert!(second.ok);
        assert_eq!(
            second.output,
            "Updated todo list: 0 pending, 1 in progress, 1 completed."
        );
        assert_eq!(
            log.view().todos.as_deref().unwrap()[0].status,
            TodoStatus::Completed
        );
        assert_eq!(log.events().len(), 2);
    }

    #[tokio::test]
    async fn todo_write_rejects_blank_duplicate_and_unknown_fields() {
        for todos in [
            serde_json::json!([{"content": "   ", "status": "pending"}]),
            serde_json::json!([
                {"content": "dup", "status": "pending"},
                {"content": "dup", "status": "completed"}
            ]),
            serde_json::json!([{"content": "a", "status": "pending", "children": []}]),
            serde_json::json!([{"content": "a", "status": "doing"}]),
        ] {
            let output = execute_through_registry(
                true,
                todos,
                Some(Arc::new(SessionLog::in_memory(Uuid::new_v4()))),
            );
            assert!(!output.ok, "expected invalid todo list to fail");
        }
    }

    #[tokio::test]
    async fn single_active_policy_rejects_parallel_lists_before_logging() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let output = execute_through_registry(
            false,
            serde_json::json!([
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"}
            ]),
            Some(log.clone()),
        );

        assert!(!output.ok);
        assert!(output
            .output
            .contains("at most one task may be in_progress"));
        assert!(log.events().is_empty());
    }

    #[tokio::test]
    async fn todo_write_requires_an_owning_session() {
        let output = execute_through_registry(
            true,
            serde_json::json!([{"content": "a", "status": "pending"}]),
            None,
        );
        assert!(!output.ok);
        assert!(output.output.contains("owning agent session"));
    }

    #[test]
    fn todo_projection_is_last_write_wins_and_clears_on_turn_start() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        assert_eq!(log.view().todos, None);

        log.append(EventKind::TodoWrite {
            todos: vec![TodoItem {
                content: "first".into(),
                status: TodoStatus::Pending,
            }],
        })
        .unwrap();
        log.append(EventKind::TodoWrite {
            todos: vec![TodoItem {
                content: "second".into(),
                status: TodoStatus::InProgress,
            }],
        })
        .unwrap();
        assert_eq!(
            log.view().todos,
            Some(vec![TodoItem {
                content: "second".into(),
                status: TodoStatus::InProgress,
            }])
        );

        log.append(EventKind::TurnStarted).unwrap();
        assert_eq!(log.view().todos, None);
    }

    #[test]
    fn durable_todo_snapshots_reject_incoherent_history() {
        let log = SessionLog::in_memory(Uuid::new_v4());
        let result = log.append(EventKind::TodoWrite {
            todos: vec![
                TodoItem {
                    content: "duplicate".into(),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    content: "duplicate".into(),
                    status: TodoStatus::Completed,
                },
            ],
        });

        assert!(result.is_err());
        assert!(log.events().is_empty());
    }
}
