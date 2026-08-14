use std::sync::Arc;

use crate::input::ChatInput;
use gpui::{
    actions, div, prelude::*, px, rgb, Context, Entity, FontWeight, MouseButton, SharedString,
    Window,
};
use harness_core::agent::AgentLoop;
use harness_core::approval::{ApprovalPolicy, ApprovalRequest, ChannelApprover, GatedApprover};
use harness_core::cancellation::TurnCancellation;
use harness_core::remote::ModelSelection;
use harness_core::session::{SessionView, TranscriptEntry};
use harness_core::store::{SessionStore, SessionSummary};
use harness_core::tools::fs::{ListDirTool, ReadFileTool, ScopedFs, WriteFileTool};
use harness_core::tools::shell::{CommandTool, ShellPolicy};
use harness_core::tools::{EchoTool, ToolRegistry};

actions!(workspace, [Submit, NewSession, CancelTurn]);

pub struct Workspace {
    store: Arc<SessionStore>,
    agent: AgentLoop,
    pub(crate) input: Entity<ChatInput>,
    summaries: Vec<SessionSummary>,
    selected: Option<harness_core::session::SharedSessionLog>,
    selected_view: Option<SessionView>,
    busy: bool,
    status: SharedString,
    model: String,
    pending_approval: Option<ApprovalRequest>,
    cancellation: Option<TurnCancellation>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(crate::input::ChatInput::new);
        let root = Self::sessions_root();
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let selection =
            ModelSelection::select_from_environment(Some(&home)).unwrap_or_else(|error| {
                eprintln!("model selection failed: {error}");
                ModelSelection::select(None, &home.join(".dsh-rs/credentials"), None)
                    .expect("local fallback adapter")
            });
        let approval_path = home.join(".dsh-rs").join("approvals.json");
        if !approval_path.exists() {
            let _ = ApprovalPolicy::default().save(&approval_path);
        }
        let approval_policy = match ApprovalPolicy::load(&approval_path) {
            Ok(policy) => policy,
            Err(error) => {
                eprintln!("approval policy load failed ({error}); using fail-closed defaults");
                ApprovalPolicy::default()
            }
        };
        let (approval_channel, mut approval_requests) = ChannelApprover::channel();
        let approver = Arc::new(GatedApprover::new(approval_policy, approval_channel));
        let (store, status) = match SessionStore::open(&root) {
            Ok(store) => {
                let model_status = if selection.is_remote {
                    format!("Ready. DeepSeek model {} is connected.", selection.model)
                } else {
                    "Ready. Using the built-in local adapter.".to_string()
                };
                (Arc::new(store), SharedString::from(model_status))
            }
            Err(error) => {
                let fallback = std::env::temp_dir().join("dsh-rs-sessions");
                let store = SessionStore::open(&fallback).expect("temporary session store");
                (
                    Arc::new(store),
                    SharedString::from(format!(
                        "Home session store failed ({error}); using {}",
                        fallback.display()
                    )),
                )
            }
        };

        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        if let Ok(filesystem) = ScopedFs::new(home.join(".dsh-rs").join("workspace")) {
            let filesystem = Arc::new(filesystem);
            tools.register(Arc::new(ReadFileTool::new(filesystem.clone())));
            tools.register(Arc::new(ListDirTool::new(filesystem.clone())));
            tools.register(Arc::new(WriteFileTool::new(filesystem.clone())));

            let shell_path = home.join(".dsh-rs").join("shell.json");
            if !shell_path.exists() {
                let _ = ShellPolicy::default()
                    .canonicalize()
                    .unwrap()
                    .save(&shell_path);
            }
            match ShellPolicy::load(&shell_path) {
                Ok(shell_policy) if !shell_policy.allowed_binaries.is_empty() => {
                    tools.register(Arc::new(CommandTool::new(filesystem, shell_policy)));
                }
                Ok(_) => {}
                Err(error) => eprintln!("shell policy load failed ({error}); shell stays disabled"),
            }
        }
        let agent = AgentLoop::new(selection.adapter.clone(), Arc::new(tools))
            .with_default_model(Some(selection.model.clone()))
            .with_approver(approver)
            .with_max_steps(8);

        let mut workspace = Self {
            store,
            agent,
            input,
            summaries: Vec::new(),
            selected: None,
            selected_view: None,
            busy: false,
            status,
            model: selection.model.clone(),
            pending_approval: None,
            cancellation: None,
        };
        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            while let Some(request) = approval_requests.recv().await {
                if cx
                    .update(|cx| {
                        workspace_handle.update(cx, |workspace, cx| {
                            workspace.pending_approval = Some(request);
                            workspace.status = SharedString::from(format!(
                                "Approval required for {} ({})",
                                workspace.pending_approval.as_ref().unwrap().tool_name,
                                workspace.pending_approval.as_ref().unwrap().call_id
                            ));
                            cx.notify();
                        });
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        workspace.refresh();
        if workspace.selected.is_none() {
            workspace.create_session(cx);
        }
        workspace
    }

    fn sessions_root() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".dsh-rs")
            .join("sessions")
    }

    fn refresh(&mut self) {
        self.summaries = self.store.list();
        self.selected_view = self.selected.as_ref().map(|log| log.view());
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        match self.store.create("New session", Some(self.model.clone())) {
            Ok(log) => {
                self.selected = Some(log);
                self.status = SharedString::from("New session created.");
                self.refresh();
            }
            Err(error) => {
                self.status = SharedString::from(format!("Could not create session: {error}"))
            }
        }
        cx.notify();
    }

    fn select(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
        } else if let Some(log) = self.store.get(id) {
            self.selected = Some(log);
            self.status = SharedString::from("Session loaded.");
            self.refresh();
        } else {
            self.status = SharedString::from("Session no longer exists.");
        }
        cx.notify();
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(log) = self.selected.clone() else {
            self.status = SharedString::from("No selected session.");
            cx.notify();
            return;
        };
        let text = self.input.read(cx).text();
        if text.trim().is_empty() {
            self.status = SharedString::from("Message is empty.");
            cx.notify();
            return;
        }

        self.busy = true;
        let cancellation = TurnCancellation::new();
        self.cancellation = Some(cancellation.clone());
        self.status = SharedString::from("Turn running...");
        self.input.update(cx, |input, cx| input.clear(cx));
        cx.notify();

        let agent = self.agent.clone();
        let cancellation_for_turn = cancellation;
        let workspace = cx.entity();
        let mut sequence = log.subscribe();
        cx.spawn(async move |_, cx| {
            let turn = cx.background_spawn(async move {
                agent
                    .run_turn_with_cancellation(log, text, cancellation_for_turn)
                    .await
            });
            let mut turn = std::pin::pin!(turn);
            let result = loop {
                tokio::select! {
                    result = &mut *turn => break result,
                    changed = sequence.changed() => {
                        if changed.is_ok() {
                            let _ = cx.update(|cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.refresh();
                                    cx.notify();
                                });
                            });
                        }
                    }
                }
            };

            let _ = cx.update(|cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.busy = false;
                    workspace.cancellation = None;
                    workspace.refresh();
                    workspace.status = match result {
                        Ok(harness_core::agent::TurnOutcome::Completed) => {
                            SharedString::from("Turn complete.")
                        }
                        Ok(harness_core::agent::TurnOutcome::Cancelled) => {
                            SharedString::from("Turn cancelled.")
                        }
                        Err(error) => SharedString::from(format!("Turn failed: {error}")),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn cancel_turn(&mut self, _: &CancelTurn, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
            self.status = SharedString::from("Cancelling turn...");
        }
        if let Some(request) = self.pending_approval.take() {
            let _ = request.responder.send(false);
        }
        cx.notify();
    }

    fn resolve_approval(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(request) = self.pending_approval.take() {
            let tool_name = request.tool_name.clone();
            let call_id = request.call_id.clone();
            let _ = request.responder.send(approved);
            self.status = SharedString::from(format!(
                "Approval {call_id} for {tool_name} {}",
                if approved { "granted" } else { "denied" }
            ));
        }
        cx.notify();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(px(280.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x13161c))
            .border_r_1()
            .border_color(rgb(0x252b34))
            .p_3()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x8f9aa8))
                            .child("SESSIONS"),
                    )
                    .children(self.busy.then(|| {
                        div()
                            .id("cancel-turn")
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(0x753030))
                            .text_size(px(13.))
                            .text_color(rgb(0xffe8e8))
                            .hover(|style| style.bg(rgb(0x8e3a3a)).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    workspace.cancel_turn(&CancelTurn, window, cx)
                                }),
                            )
                            .child("Cancel")
                    }))
                    .child(
                        div()
                            .id("new-session")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x246b53))
                            .text_size(px(12.))
                            .text_color(rgb(0xe8fff6))
                            .hover(|style| style.bg(rgb(0x2d8465)).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.create_session(cx)),
                            )
                            .child("New"),
                    ),
            )
            .children(self.summaries.iter().map(|summary| {
                let id = summary.id;
                let selected = self.selected.as_ref().is_some_and(|log| log.id() == id);
                div()
                    .id(("session", id.as_u64_pair().1))
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .bg(if selected {
                        rgb(0x20262e)
                    } else {
                        rgb(0x171a20)
                    })
                    .hover(|style| style.bg(rgb(0x20262e)).cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _, _, cx| workspace.select(id, cx)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(0xe7e9ee))
                            .child(summary.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(0x818d9a))
                            .child(format!(
                                "{} · {} events{}",
                                summary.model.as_deref().unwrap_or("no model"),
                                summary.event_count,
                                if summary.turn_active {
                                    " · active"
                                } else {
                                    ""
                                }
                            )),
                    )
            }))
    }

    fn render_transcript(&self) -> impl IntoElement {
        let transcript = self
            .selected_view
            .as_ref()
            .map(|view| view.transcript.as_slice())
            .unwrap_or(&[]);

        div()
            .id("transcript")
            .flex_1()
            .overflow_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .children(transcript.iter().map(|entry| {
                let (role, text, color) = match entry {
                    TranscriptEntry::User { content, .. } => ("You", content, rgb(0xdce9ff)),
                    TranscriptEntry::Assistant { content, .. } => {
                        ("Assistant", content, rgb(0xe7e9ee))
                    }
                    TranscriptEntry::ToolCall { name, .. } => ("Tool call", name, rgb(0xffd9a0)),
                    TranscriptEntry::ToolResult { output, .. } => {
                        ("Tool result", output, rgb(0xd8f5e4))
                    }
                    TranscriptEntry::System { message } => ("System", message, rgb(0xffb4b4)),
                };
                div()
                    .max_w(px(820.))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x171a20))
                    .border_1()
                    .border_color(rgb(0x252b34))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x818d9a))
                            .child(role),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(color)
                            .child(text.clone()),
                    )
            }))
    }

    fn render_approval(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (tool_name, call_id, arguments) = self
            .pending_approval
            .as_ref()
            .map(|request| {
                (
                    request.tool_name.clone(),
                    request.call_id.clone(),
                    request.arguments.to_string(),
                )
            })
            .unwrap_or_default();

        div()
            .id("approval")
            .mx_6()
            .my_3()
            .px_4()
            .py_3()
            .rounded_md()
            .bg(rgb(0x2b2114))
            .border_1()
            .border_color(rgb(0x6f5220))
            .flex()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0xffd9a0))
                            .child(format!("Approve {tool_name}?")),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(0xa89168))
                            .child(format!("{call_id} {arguments}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("approve-tool")
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x246b53))
                            .text_size(px(12.))
                            .text_color(rgb(0xe8fff6))
                            .hover(|style| style.bg(rgb(0x2d8465)).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.resolve_approval(true, cx)
                                }),
                            )
                            .child("Approve"),
                    )
                    .child(
                        div()
                            .id("deny-tool")
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x753030))
                            .text_size(px(12.))
                            .text_color(rgb(0xffe8e8))
                            .hover(|style| style.bg(rgb(0x8e3a3a)).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.resolve_approval(false, cx)
                                }),
                            )
                            .child("Deny"),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Workspace")
            .id("workspace-root")
            .size_full()
            .flex()
            .bg(rgb(0x0f1115))
            .text_color(rgb(0xe7e9ee))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(|workspace, _: &NewSession, _, cx| workspace.create_session(cx)))
            .on_action(cx.listener(Self::cancel_turn))
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_transcript())
                    .children(
                        self.pending_approval
                            .as_ref()
                            .map(|_| self.render_approval(cx)),
                    )
                    .child(
                        div()
                            .px_6()
                            .pb_5()
                            .pt_3()
                            .flex()
                            .gap_2()
                            .border_t_1()
                            .border_color(rgb(0x252b34))
                            .child(div().flex_1().child(self.input.clone()))
                            .child(
                                div()
                                    .id("send")
                                    .px_4()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .bg(if self.busy {
                                        rgb(0x39404a)
                                    } else {
                                        rgb(0x246b53)
                                    })
                                    .text_size(px(13.))
                                    .text_color(rgb(0xe8fff6))
                                    .hover(|style| {
                                        if self.busy {
                                            style
                                        } else {
                                            style.bg(rgb(0x2d8465)).cursor_pointer()
                                        }
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.submit(&Submit, window, cx)
                                        }),
                                    )
                                    .child(if self.busy { "..." } else { "Send" }),
                            ),
                    )
                    .child(
                        div()
                            .px_6()
                            .pb_3()
                            .text_size(px(11.))
                            .text_color(rgb(0x77828f))
                            .child(self.status.clone()),
                    ),
            )
    }
}
