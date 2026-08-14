use std::sync::Arc;

use crate::changes::{ChangeStatus, WorkspaceChanges, WorkspaceChangesState};
use crate::input::{ChatInput, InputKind};
use crate::settings::UiSettings;
use crate::theme::{Theme, CONTEXT_PANE_WIDTH, HEADER_HEIGHT, SIDEBAR_WIDTH, STATUS_HEIGHT};
use gpui::{
    actions, div, prelude::*, px, Context, Entity, FontWeight, MouseButton, SharedString, Window,
};
use harness_core::agent::AgentLoop;
use harness_core::approval::{
    ApprovalPolicy, ApprovalRequest, ChannelApprover, GatedApprover, ToolApprover,
};
use harness_core::cancellation::TurnCancellation;
use harness_core::prompt::SystemPromptConfig;
use harness_core::remote::{CredentialStore, ModelSelection};
use harness_core::session::{SessionView, TranscriptEntry};
use harness_core::store::{SessionStore, SessionSummary};
use harness_core::tools::fs::{ListDirTool, ReadFileTool, ScopedFs, WriteFileTool};
use harness_core::tools::shell::{CommandTool, ShellPolicy};
use harness_core::tools::{EchoTool, ToolRegistry};
use zeroize::Zeroize;

actions!(
    workspace,
    [
        Submit,
        NewSession,
        CancelTurn,
        SearchSessions,
        RenameSession,
        SaveCredential,
        ToggleSidebar,
        ToggleContext
    ]
);

pub struct Workspace {
    store: Arc<SessionStore>,
    agent: AgentLoop,
    pub(crate) input: Entity<ChatInput>,
    search_input: Entity<ChatInput>,
    rename_input: Entity<ChatInput>,
    credential_input: Entity<ChatInput>,
    summaries: Vec<SessionSummary>,
    selected: Option<harness_core::session::SharedSessionLog>,
    selected_view: Option<SessionView>,
    busy: bool,
    status: SharedString,
    model: String,
    ui_settings: UiSettings,
    ui_settings_path: std::path::PathBuf,
    home: std::path::PathBuf,
    workspace_root: std::path::PathBuf,
    credential_path: std::path::PathBuf,
    credential_environment_override: bool,
    credential_file_configured: bool,
    tools: Arc<ToolRegistry>,
    approver: Arc<dyn ToolApprover>,
    system_prompt: Option<String>,
    changes: WorkspaceChangesState,
    changes_scanning: bool,
    pending_approval: Option<ApprovalRequest>,
    cancellation: Option<TurnCancellation>,
    search_matches: Option<Vec<uuid::Uuid>>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| ChatInput::new(InputKind::Chat, cx));
        let search_input = cx.new(|cx| ChatInput::new(InputKind::Search, cx));
        let rename_input = cx.new(|cx| ChatInput::new(InputKind::Rename, cx));
        let credential_input = cx.new(|cx| ChatInput::new(InputKind::ApiKey, cx));
        let root = Self::sessions_root();
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let credential_path = CredentialStore::credential_path(Some(&home));
        let credential_environment_override = std::env::var("DEEPSEEK_API_KEY")
            .map(|key| !key.trim().is_empty())
            .unwrap_or(false);
        let credential_file_configured = CredentialStore::resolve(None, &credential_path).is_ok();
        let workspace_root = home.join(".dsh-rs").join("workspace");
        let _ = std::fs::create_dir_all(&workspace_root);
        let ui_settings_path = home.join(".dsh-rs").join("ui.json");
        let ui_settings = if ui_settings_path.exists() {
            UiSettings::load(&ui_settings_path).unwrap_or_else(|error| {
                eprintln!("UI settings load failed ({error}); using workbench defaults");
                UiSettings::default()
            })
        } else {
            let settings = UiSettings::default();
            let _ = settings.save(&ui_settings_path);
            settings
        };
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
        let prompt_path = home.join(".dsh-rs").join("system-prompt.json");
        if !prompt_path.exists() {
            let _ = SystemPromptConfig::default().save(&prompt_path);
        }
        let system_prompt = match SystemPromptConfig::load(&prompt_path) {
            Ok(config) => config.render(),
            Err(error) => {
                eprintln!("system prompt config load failed ({error}); using default identity");
                SystemPromptConfig::default().render()
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

        let tools = Self::build_tools(&home);
        let agent = AgentLoop::new(selection.adapter.clone(), tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(system_prompt.clone())
            .with_approver(approver.clone())
            .with_max_steps(8);

        let mut workspace = Self {
            store,
            agent,
            input,
            search_input,
            rename_input,
            credential_input,
            summaries: Vec::new(),
            selected: None,
            selected_view: None,
            busy: false,
            status,
            model: selection.model.clone(),
            ui_settings,
            ui_settings_path,
            home,
            workspace_root,
            credential_path,
            credential_environment_override,
            credential_file_configured,
            tools,
            approver,
            system_prompt,
            changes: WorkspaceChangesState::NotRepository,
            changes_scanning: false,
            pending_approval: None,
            cancellation: None,
            search_matches: None,
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
        workspace.refresh_changes(cx);
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

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_settings.sidebar_visible = !self.ui_settings.sidebar_visible;
        if let Err(error) = self.ui_settings.save(&self.ui_settings_path) {
            self.status = SharedString::from(format!("Could not save sidebar state: {error}"));
        }
        cx.notify();
    }

    fn toggle_context(&mut self, _: &ToggleContext, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_settings.context_pane_visible = !self.ui_settings.context_pane_visible;
        if let Err(error) = self.ui_settings.save(&self.ui_settings_path) {
            self.status = SharedString::from(format!("Could not save context state: {error}"));
        }
        cx.notify();
    }

    fn build_tools(home: &std::path::Path) -> Arc<ToolRegistry> {
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
        Arc::new(tools)
    }

    fn reload_model(&mut self) -> Result<(), String> {
        let selection = ModelSelection::select_from_environment(Some(&self.home))
            .map_err(|error| error.to_string())?;
        self.agent = AgentLoop::new(selection.adapter.clone(), self.tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(self.system_prompt.clone())
            .with_approver(self.approver.clone())
            .with_max_steps(8);
        self.model = selection.model;
        Ok(())
    }

    fn credential_label(&self) -> &'static str {
        if self.credential_environment_override {
            "Environment key active"
        } else if self.credential_file_configured {
            "Stored key active"
        } else {
            "No stored key"
        }
    }

    fn save_credential(&mut self, _: &SaveCredential, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            cx.notify();
            return;
        }
        if self.credential_environment_override {
            self.status = SharedString::from(
                "DEEPSEEK_API_KEY is set by the launching environment; stored keys stay read-only.",
            );
            cx.notify();
            return;
        }

        let mut key = self.credential_input.read(cx).text();
        let save_result = CredentialStore::save(&key, &self.credential_path);
        key.zeroize();
        if let Err(error) = save_result {
            self.status = SharedString::from(format!("Could not save API key: {error}"));
            cx.notify();
            return;
        }

        self.credential_file_configured =
            CredentialStore::resolve(None, &self.credential_path).is_ok();
        self.credential_input
            .update(cx, |input, cx| input.clear(cx));
        match self.reload_model() {
            Ok(()) => {
                if let Some(selected) = &self.selected {
                    let _ = selected.set_model(self.model.clone());
                }
                self.refresh();
                self.status =
                    SharedString::from(format!("API key saved. {} is connected.", self.model));
            }
            Err(error) => {
                self.status =
                    SharedString::from(format!("API key saved, but model reload failed: {error}"));
            }
        }
        cx.notify();
    }

    fn remove_credential(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            cx.notify();
            return;
        }
        match CredentialStore::remove(&self.credential_path) {
            Ok(true) => {
                self.credential_file_configured = false;
                self.credential_input
                    .update(cx, |input, cx| input.clear(cx));
                match self.reload_model() {
                    Ok(()) => {
                        if let Some(selected) = &self.selected {
                            let _ = selected.set_model(self.model.clone());
                        }
                        self.refresh();
                        self.status = SharedString::from(if self.credential_environment_override {
                            "Stored key removed. Environment key remains active."
                        } else {
                            "Stored key removed. Using the local adapter."
                        });
                    }
                    Err(error) => {
                        self.status = SharedString::from(format!("Model reload failed: {error}"));
                    }
                }
            }
            Ok(false) => {
                self.status = SharedString::from("No stored API key to remove.");
            }
            Err(error) => {
                self.status = SharedString::from(format!("Could not remove API key: {error}"));
            }
        }
        cx.notify();
    }

    fn refresh(&mut self) {
        self.summaries = self.store.list();
        self.selected_view = self.selected.as_ref().map(|log| log.view());
    }

    fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if self.changes_scanning {
            return;
        }
        self.changes_scanning = true;
        let root = self.workspace_root.clone();
        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            let state = cx
                .background_spawn(async move { WorkspaceChanges::scan_blocking(root) })
                .await;
            let _ = cx.update(|cx| {
                workspace_handle.update(cx, |workspace, cx| {
                    workspace.changes = state;
                    workspace.changes_scanning = false;
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        match self.store.create("New session", Some(self.model.clone())) {
            Ok(log) => {
                self.selected = Some(log);
                self.search_matches = None;
                self.status = SharedString::from("New session created.");
                self.refresh();
                self.sync_rename_input(cx);
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
            self.sync_rename_input(cx);
        } else {
            self.status = SharedString::from("Session no longer exists.");
        }
        cx.notify();
    }

    fn sync_rename_input(&mut self, cx: &mut Context<Self>) {
        let title = self
            .selected_view
            .as_ref()
            .map(|view| view.title.clone())
            .unwrap_or_default();
        self.rename_input
            .update(cx, |input, cx| input.set_text(title, cx));
    }

    fn search_sessions(&mut self, _: &SearchSessions, _: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text();
        if query.trim().is_empty() {
            self.search_matches = None;
            self.status = SharedString::from("Search cleared.");
        } else {
            let matches = self.store.search(&query);
            let count = matches.len();
            self.search_matches = Some(
                matches
                    .into_iter()
                    .map(|result| result.session_id)
                    .collect(),
            );
            self.status = SharedString::from(format!(
                "{count} session{} matched.",
                if count == 1 { "" } else { "s" }
            ));
        }
        cx.notify();
    }

    fn rename_selected(&mut self, _: &RenameSession, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected.clone() else {
            self.status = SharedString::from("No selected session to rename.");
            cx.notify();
            return;
        };
        let title = self.rename_input.read(cx).text();
        let title = title.trim();
        if title.is_empty() {
            self.status = SharedString::from("Session title cannot be empty.");
            cx.notify();
            return;
        }
        match self.store.rename(selected.id(), title) {
            Ok(_) => {
                self.refresh();
                self.sync_rename_input(cx);
                self.status = SharedString::from("Session renamed.");
            }
            Err(error) => self.status = SharedString::from(format!("Rename failed: {error}")),
        }
        cx.notify();
    }

    fn fork_selected(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            self.status = SharedString::from("No session to fork.");
            cx.notify();
            return;
        };
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            cx.notify();
            return;
        }
        let title = format!("Fork of {}", source.view().title);
        match self.store.fork(source.id(), None, title) {
            Ok(fork) => {
                self.selected = Some(fork);
                self.search_matches = None;
                self.status = SharedString::from("Session forked.");
                self.refresh();
                self.sync_rename_input(cx);
            }
            Err(error) => self.status = SharedString::from(format!("Could not fork: {error}")),
        }
        cx.notify();
    }

    fn export_selected(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            self.status = SharedString::from("No session to export.");
            cx.notify();
            return;
        };
        let export_dir = self
            .store
            .root()
            .parent()
            .map(|root| root.join("exports"))
            .unwrap_or_else(|| std::path::PathBuf::from("exports"));
        match self.store.export_markdown(source.id()) {
            Ok(markdown) => {
                let path = export_dir.join(format!("{}.md", source.id()));
                match std::fs::create_dir_all(&export_dir)
                    .and_then(|()| std::fs::write(&path, markdown))
                {
                    Ok(()) => {
                        self.status = SharedString::from(format!("Exported {}.", path.display()))
                    }
                    Err(error) => {
                        self.status = SharedString::from(format!("Export failed: {error}"))
                    }
                }
            }
            Err(error) => self.status = SharedString::from(format!("Export failed: {error}")),
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
                    workspace.sync_rename_input(cx);
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

    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = Theme::dark();
        let visible_summaries: Vec<SessionSummary> = match &self.search_matches {
            Some(matches) => self
                .summaries
                .iter()
                .filter(|summary| matches.contains(&summary.id))
                .cloned()
                .collect(),
            None => self.summaries.clone(),
        };

        div()
            .id("sessions-sidebar")
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .overflow_scroll()
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
                            .text_color(theme.faint)
                            .child("SPACES"),
                    )
                    .children(self.busy.then(|| {
                        div()
                            .id("cancel-turn")
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.danger)
                            .text_size(px(13.))
                            .text_color(theme.background)
                            .hover(|style| style.cursor_pointer())
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
                            .bg(theme.accent)
                            .text_size(px(12.))
                            .text_color(theme.background)
                            .hover(|style| style.bg(theme.success).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.create_session(cx)),
                            )
                            .child("New"),
                    ),
            )
            .child(
                div()
                    .id("local-space")
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.text)
                            .child("Local harness"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.faint)
                            .child("This Mac"),
                    ),
            )
            .child(self.search_input.clone())
            .child(self.rename_input.clone())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("fork-session")
                            .flex_1()
                            .py_1()
                            .rounded_sm()
                            .bg(theme.raised)
                            .text_size(px(12.))
                            .text_color(theme.text)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.border).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.fork_selected(cx)),
                            )
                            .child("Fork"),
                    )
                    .child(
                        div()
                            .id("export-session")
                            .flex_1()
                            .py_1()
                            .rounded_sm()
                            .bg(theme.raised)
                            .text_size(px(12.))
                            .text_color(theme.success)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.border).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.export_selected(cx)),
                            )
                            .child("Export"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.faint)
                            .child("MODEL ACCESS"),
                    )
                    .child(self.credential_input.clone())
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("save-credential")
                                    .flex_1()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(theme.success)
                                    .text_size(px(12.))
                                    .text_color(theme.background)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|style| style.bg(theme.success).cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.save_credential(&SaveCredential, window, cx)
                                        }),
                                    )
                                    .child("Save Key"),
                            )
                            .child(
                                div()
                                    .id("remove-credential")
                                    .flex_1()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(if self.credential_file_configured {
                                        theme.danger
                                    } else {
                                        theme.raised
                                    })
                                    .text_size(px(12.))
                                    .text_color(theme.background)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|style| style.cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, _, cx| {
                                            workspace.remove_credential(cx)
                                        }),
                                    )
                                    .child("Remove"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(if self.credential_environment_override {
                                theme.warning
                            } else if self.credential_file_configured {
                                theme.success
                            } else {
                                theme.faint
                            })
                            .child(self.credential_label()),
                    ),
            )
            .children(visible_summaries.iter().map(|summary| {
                let id = summary.id;
                let selected = self.selected.as_ref().is_some_and(|log| log.id() == id);
                div()
                    .id(("session", id.as_u64_pair().1))
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .border_l_2()
                    .border_color(if selected { theme.accent } else { theme.border })
                    .bg(if selected {
                        theme.raised
                    } else {
                        theme.surface
                    })
                    .hover(|style| style.bg(theme.raised).cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _, _, cx| workspace.select(id, cx)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.text)
                            .child(summary.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.faint)
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
        let theme = Theme::dark();
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
                    TranscriptEntry::User { content, .. } => ("You", content, theme.text),
                    TranscriptEntry::Assistant { content, .. } => {
                        ("Assistant", content, theme.text)
                    }
                    TranscriptEntry::ToolCall { name, .. } => ("Tool call", name, theme.warning),
                    TranscriptEntry::ToolResult { output, .. } => {
                        ("Tool result", output, theme.success)
                    }
                    TranscriptEntry::System { message } => ("System", message, theme.danger),
                };
                div()
                    .max_w(px(820.))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(theme.raised)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.faint)
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

    fn render_changes_section(&self) -> gpui::Div {
        let theme = Theme::dark();
        let section = div()
            .flex()
            .flex_col()
            .gap_2()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex().items_center().justify_between().child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.faint)
                        .child("CHANGES"),
                ),
            );

        if self.changes_scanning {
            return section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child("Scanning repository"),
            );
        }

        match &self.changes {
            WorkspaceChangesState::NotRepository => section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child("No repository"),
            ),
            WorkspaceChangesState::Failed(error) => {
                section.child(div().text_size(px(11.)).text_color(theme.danger).child(
                    if error.chars().count() > 120 {
                        format!("{}...", error.chars().take(117).collect::<String>())
                    } else {
                        error.clone()
                    },
                ))
            }
            WorkspaceChangesState::Ready(changes) => {
                let branch = changes.branch.clone().unwrap_or_else(|| "detached".into());
                let totals = format!(
                    "{} files  up {}  down {}  +{}  -{}",
                    changes.files.len(),
                    changes.ahead,
                    changes.behind,
                    changes.total_additions.unwrap_or(0),
                    changes.total_deletions.unwrap_or(0),
                );
                let visible_count = changes.files.len().min(10);
                let hidden_count = changes.files.len().saturating_sub(visible_count);

                section
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text)
                                    .child(branch),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.faint)
                                    .child(totals),
                            ),
                    )
                    .children(
                        changes
                            .files
                            .iter()
                            .take(10)
                            .enumerate()
                            .map(|(index, file)| {
                                let (status_label, status_color) = match file.status {
                                    ChangeStatus::Added => ("A", theme.success),
                                    ChangeStatus::Deleted => ("D", theme.danger),
                                    ChangeStatus::Renamed => ("R", theme.warning),
                                    ChangeStatus::Copied => ("C", theme.warning),
                                    ChangeStatus::TypeChanged => ("T", theme.warning),
                                    ChangeStatus::Unmerged => ("U", theme.danger),
                                    ChangeStatus::Untracked => ("?", theme.warning),
                                    ChangeStatus::Modified => ("M", theme.accent),
                                };
                                let counts = match (file.additions, file.deletions) {
                                    (Some(additions), Some(deletions)) => {
                                        format!("+{additions} -{deletions}")
                                    }
                                    (Some(additions), None) => format!("+{additions} binary"),
                                    (None, Some(deletions)) => format!("-{deletions} binary"),
                                    (None, None) => "no text stats".into(),
                                };
                                let path = if file.path.chars().count() > 42 {
                                    format!("{}...", file.path.chars().take(39).collect::<String>())
                                } else {
                                    file.path.clone()
                                };
                                div()
                                    .id(("changed-file", index))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(status_color)
                                                    .child(status_label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.text)
                                                    .child(path),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(if file.staged {
                                                        theme.warning
                                                    } else {
                                                        theme.faint
                                                    })
                                                    .child(if file.staged {
                                                        "staged"
                                                    } else {
                                                        "worktree"
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(theme.muted)
                                                    .child(counts),
                                            ),
                                    )
                            }),
                    )
                    .when(hidden_count > 0, |element| {
                        element.child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.faint)
                                .child(format!("+{hidden_count} more files")),
                        )
                    })
            }
        }
    }

    fn render_context_pane(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = Theme::dark();
        let view = self.selected_view.clone();
        let title = view
            .as_ref()
            .map(|view| view.title.clone())
            .unwrap_or_else(|| "No session".into());
        let model = view
            .as_ref()
            .and_then(|view| view.model.clone())
            .unwrap_or_else(|| "unknown".into());
        let event_count = view.as_ref().map_or(0, |view| view.event_count);
        let prompt = view
            .as_ref()
            .and_then(|view| view.system_prompt.clone())
            .unwrap_or_else(|| "Default identity".into());
        let prompt = if prompt.chars().count() > 80 {
            format!("{}...", prompt.chars().take(77).collect::<String>())
        } else {
            prompt
        };
        let turn_state =
            view.as_ref().map_or(
                "idle",
                |view| {
                    if view.turn_active {
                        "active"
                    } else {
                        "idle"
                    }
                },
            );
        let mut activity: Vec<_> = view
            .as_ref()
            .map(|view| {
                view.transcript
                    .iter()
                    .filter(|entry| {
                        matches!(
                            entry,
                            TranscriptEntry::ToolCall { .. } | TranscriptEntry::ToolResult { .. }
                        )
                    })
                    .rev()
                    .take(8)
                    .collect()
            })
            .unwrap_or_default();
        activity.reverse();
        let activity_empty = activity.is_empty();

        div()
            .id("context-pane")
            .w(px(CONTEXT_PANE_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(HEADER_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Context"),
                    )
                    .child(
                        div()
                            .id("refresh-changes")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .hover(|style| style.bg(theme.raised).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.refresh_changes(cx)),
                            )
                            .child("Refresh"),
                    )
                    .child(
                        div()
                            .id("close-context")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .hover(|style| style.bg(theme.raised).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    workspace.toggle_context(&ToggleContext, window, cx)
                                }),
                            )
                            .child("Close"),
                    ),
            )
            .child(
                div()
                    .id("context-body")
                    .flex_1()
                    .overflow_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.render_changes_section())
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.faint)
                                            .child("Model"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.muted)
                                            .child(model),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.faint)
                                            .child("Events"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.muted)
                                            .child(event_count.to_string()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.faint)
                                            .child("Turn"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(if turn_state == "active" {
                                                theme.accent
                                            } else {
                                                theme.muted
                                            })
                                            .child(turn_state),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.faint)
                                            .child("Prompt"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.muted)
                                            .child(prompt),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.faint)
                            .child("Recent tools"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(activity.into_iter().map(|entry| {
                                let (label, value, color) = match entry {
                                    TranscriptEntry::ToolCall { name, .. } => {
                                        ("call", name.clone(), theme.warning)
                                    }
                                    TranscriptEntry::ToolResult { output, ok, .. } => (
                                        "result",
                                        output.clone(),
                                        if *ok { theme.success } else { theme.danger },
                                    ),
                                    _ => unreachable!("activity filter permits only tool entries"),
                                };
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .pb_2()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(div().text_size(px(10.)).text_color(color).child(label))
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.muted)
                                            .child(value),
                                    )
                            }))
                            .when(activity_empty, |el| {
                                el.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.faint)
                                        .child("No tool activity"),
                                )
                            }),
                    ),
            )
    }

    fn render_approval(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
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
            .bg(theme.raised)
            .border_1()
            .border_color(theme.warning)
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
                            .text_color(theme.warning)
                            .child(format!("Approve {tool_name}?")),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.muted)
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
                            .bg(theme.success)
                            .text_size(px(12.))
                            .text_color(theme.background)
                            .hover(|style| style.bg(theme.success).cursor_pointer())
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
                            .bg(theme.danger)
                            .text_size(px(12.))
                            .text_color(theme.background)
                            .hover(|style| style.cursor_pointer())
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
        let theme = Theme::dark();
        let sidebar_visible = self.ui_settings.sidebar_visible;
        let context_visible = self.ui_settings.context_pane_visible;
        div()
            .key_context("Workspace")
            .id("workspace-root")
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::search_sessions))
            .on_action(cx.listener(Self::rename_selected))
            .on_action(cx.listener(|workspace, _: &NewSession, _, cx| workspace.create_session(cx)))
            .on_action(cx.listener(Self::cancel_turn))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_context))
            .when(sidebar_visible, |el| el.child(self.render_sidebar(cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(theme.background)
                    .child(
                        div()
                            .h(px(HEADER_HEIGHT))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_5()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(self.selected_view.as_ref().map_or_else(
                                                || "New session".to_string(),
                                                |view| view.title.clone(),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.faint)
                                            .child(self.model.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .id("show-context")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .hover(|style| style.bg(theme.raised).cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.toggle_context(&ToggleContext, window, cx)
                                        }),
                                    )
                                    .child(if context_visible { "Hide" } else { "Context" }),
                            ),
                    )
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
                            .border_color(theme.border)
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
                                        theme.raised
                                    } else {
                                        theme.accent
                                    })
                                    .text_size(px(13.))
                                    .text_color(if self.busy {
                                        theme.faint
                                    } else {
                                        theme.background
                                    })
                                    .hover(|style| {
                                        if self.busy {
                                            style
                                        } else {
                                            style.cursor_pointer()
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
                            .h(px(STATUS_HEIGHT))
                            .flex()
                            .items_center()
                            .text_size(px(11.))
                            .text_color(theme.faint)
                            .child(self.status.clone()),
                    ),
            )
            .when(context_visible, |el| el.child(self.render_context_pane(cx)))
    }
}
