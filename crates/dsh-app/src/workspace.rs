use std::sync::Arc;

use crate::input::ChatInput;
use gpui::{
    actions, div, prelude::*, px, rgb, Context, Entity, FontWeight, MouseButton, SharedString,
    Window,
};
use harness_core::agent::AgentLoop;
use harness_core::llm::NullAdapter;
use harness_core::session::{SessionView, TranscriptEntry};
use harness_core::store::{SessionStore, SessionSummary};
use harness_core::tools::{EchoTool, ToolRegistry};

actions!(workspace, [Submit, NewSession]);

pub struct Workspace {
    store: Arc<SessionStore>,
    agent: AgentLoop,
    pub(crate) input: Entity<ChatInput>,
    summaries: Vec<SessionSummary>,
    selected: Option<harness_core::session::SharedSessionLog>,
    selected_view: Option<SessionView>,
    busy: bool,
    status: SharedString,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(crate::input::ChatInput::new);
        let root = Self::sessions_root();
        let (store, status) = match SessionStore::open(&root) {
            Ok(store) => (
                Arc::new(store),
                SharedString::from("Ready. Using the built-in local adapter."),
            ),
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
        let agent = AgentLoop::new(Arc::new(NullAdapter), Arc::new(tools))
            .with_default_model(Some("local-null".into()))
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
        };
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
        match self.store.create("New session", Some("local-null".into())) {
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
        self.status = SharedString::from("Turn running...");
        self.input.update(cx, |input, cx| input.clear(cx));
        cx.notify();

        let agent = self.agent.clone();
        let workspace = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move { agent.run_turn(log, text).await })
                .await;
            let _ = cx.update(|cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.busy = false;
                    workspace.refresh();
                    workspace.status = match result {
                        Ok(()) => SharedString::from("Turn complete."),
                        Err(error) => SharedString::from(format!("Turn failed: {error}")),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
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
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_transcript())
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
