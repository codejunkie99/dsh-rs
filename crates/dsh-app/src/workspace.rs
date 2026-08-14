use std::sync::Arc;

use crate::changes::{
    ChangeStatus, DiffLineKind, FileDiff, WorkspaceChanges, WorkspaceChangesState,
};
use crate::icons::{self, icon};
use crate::input::{ChatInput, InputKind};
use crate::settings::UiSettings;
use crate::terminal::{parse_argv, run_command, TerminalEntry, TerminalHistory};
use crate::theme::{
    titlebar_spacer_width, Theme, CONTEXT_PANE_WIDTH, CONTROL_RADIUS, HEADER_HEIGHT, SIDEBAR_WIDTH,
    SPACE_LG, STATUS_HEIGHT, TERMINAL_DOCK_HEIGHT, TITLEBAR_HEIGHT, TITLEBAR_TOP_PAD,
};
use gpui::{
    actions, div, prelude::*, px, Context, Entity, FontWeight, MouseButton, SharedString, Window,
    WindowControlArea,
};
use harness_core::agent::AgentLoop;
use harness_core::approval::{
    ApprovalPolicy, ApprovalRequest, ChannelApprover, GatedApprover, ToolApprover,
};
use harness_core::cancellation::TurnCancellation;
use harness_core::harness::{HarnessSetup, HarnessSetupsConfig};
use harness_core::prompt::SystemPromptConfig;
use harness_core::remote::{CredentialStore, ModelSelection};
use harness_core::session::{SessionView, TranscriptEntry};
use harness_core::spaces::SpacesConfig;
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
        ToggleContext,
        NextSpace,
        PreviousSpace,
        NextHarness,
        PreviousHarness,
        RunTerminalCommand,
        ToggleTerminal,
        ClearTerminal,
        FocusTerminal
    ]
);

pub struct Workspace {
    store: Arc<SessionStore>,
    agent: AgentLoop,
    pub(crate) input: Entity<ChatInput>,
    search_input: Entity<ChatInput>,
    rename_input: Entity<ChatInput>,
    credential_input: Entity<ChatInput>,
    terminal_input: Entity<ChatInput>,
    summaries: Vec<SessionSummary>,
    selected: Option<harness_core::session::SharedSessionLog>,
    selected_view: Option<SessionView>,
    busy: bool,
    status: SharedString,
    model: String,
    ui_settings: UiSettings,
    ui_settings_path: std::path::PathBuf,
    home: std::path::PathBuf,
    spaces_config: SpacesConfig,
    selected_space_id: String,
    harness_setups: HarnessSetupsConfig,
    selected_harness_id: String,
    workspace_root: std::path::PathBuf,
    credential_path: std::path::PathBuf,
    credential_environment_override: bool,
    credential_file_configured: bool,
    tools: Arc<ToolRegistry>,
    terminal_tool: Option<Arc<CommandTool>>,
    approver: Arc<dyn ToolApprover>,
    changes: WorkspaceChangesState,
    changes_scanning: bool,
    selected_change: Option<(String, bool)>,
    selected_diff: Option<FileDiff>,
    diff_loading: bool,
    diff_error: Option<String>,
    pending_approval: Option<ApprovalRequest>,
    cancellation: Option<TurnCancellation>,
    search_matches: Option<Vec<uuid::Uuid>>,
    terminal_history: TerminalHistory,
    terminal_running: bool,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| ChatInput::new(InputKind::Chat, cx));
        let search_input = cx.new(|cx| ChatInput::new(InputKind::Search, cx));
        let rename_input = cx.new(|cx| ChatInput::new(InputKind::Rename, cx));
        let credential_input = cx.new(|cx| ChatInput::new(InputKind::ApiKey, cx));
        let terminal_input = cx.new(|cx| ChatInput::new(InputKind::Terminal, cx));
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
        let spaces_path = home.join(".dsh-rs").join("spaces.json");
        let (spaces_config, spaces_error) = Self::load_spaces(&spaces_path, &workspace_root);
        let selected_space_id = spaces_config
            .spaces()
            .first()
            .map(|space| space.id().to_string())
            .unwrap_or_else(|| "local".to_string());
        let workspace_root = spaces_config
            .get(&selected_space_id)
            .map(|space| space.root().to_path_buf())
            .unwrap_or(workspace_root);
        let harness_setups_path = home.join(".dsh-rs").join("harness-setups.json");
        let harness_setups_existed = harness_setups_path.exists();
        let (loaded_harness_setups, harness_load_error) =
            Self::load_harness_setups(&harness_setups_path);
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
        let prompt_config = match SystemPromptConfig::load(&prompt_path) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("system prompt config load failed ({error}); using default identity");
                SystemPromptConfig::default()
            }
        };
        let (harness_setups, harness_migration_error) = Self::prepare_harness_setups(
            &harness_setups_path,
            harness_setups_existed,
            loaded_harness_setups,
            prompt_config,
        );
        let selected_harness_id = harness_setups
            .setups()
            .first()
            .map(|setup| setup.id().to_string())
            .unwrap_or_else(|| "standard".to_string());
        let selected_harness = harness_setups
            .get(&selected_harness_id)
            .cloned()
            .unwrap_or_else(|| {
                HarnessSetupsConfig::default()
                    .setups()
                    .first()
                    .cloned()
                    .expect("default harness setups are nonempty")
            });
        let system_prompt = selected_harness.system_prompt().render();
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
        let status = if let Some(error) = spaces_error {
            SharedString::from(format!(
                "{status}. Spaces config failed ({error}); using Local harness."
            ))
        } else {
            status
        };
        let harness_setups_error = harness_load_error.or(harness_migration_error);
        let status = if let Some(error) = harness_setups_error {
            SharedString::from(format!(
                "{status}. Harness setup config failed ({error}); using built-in setups."
            ))
        } else {
            status
        };

        let tools = Self::build_tools(&home, &workspace_root, &selected_harness);
        let terminal_tool = Self::build_terminal_tool(&home, &workspace_root);
        let agent = AgentLoop::new(selection.adapter.clone(), tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(system_prompt.clone())
            .with_approver(approver.clone())
            .with_max_steps(selected_harness.max_steps().into());

        let mut workspace = Self {
            store,
            agent,
            input,
            search_input,
            rename_input,
            credential_input,
            terminal_input,
            summaries: Vec::new(),
            selected: None,
            selected_view: None,
            busy: false,
            status,
            model: selection.model.clone(),
            ui_settings,
            ui_settings_path,
            home,
            spaces_config,
            selected_space_id,
            harness_setups,
            selected_harness_id,
            workspace_root,
            credential_path,
            credential_environment_override,
            credential_file_configured,
            tools,
            terminal_tool,
            approver,
            changes: WorkspaceChangesState::NotRepository,
            changes_scanning: false,
            selected_change: None,
            selected_diff: None,
            diff_loading: false,
            diff_error: None,
            pending_approval: None,
            cancellation: None,
            search_matches: None,
            terminal_history: TerminalHistory::new(20),
            terminal_running: false,
        };
        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            while let Some(request) = approval_requests.recv().await {
                cx.update(|cx| {
                    workspace_handle.update(cx, |workspace, cx| {
                        workspace.pending_approval = Some(request);
                        workspace.status = SharedString::from(format!(
                            "Approval required for {} ({})",
                            workspace.pending_approval.as_ref().unwrap().tool_name,
                            workspace.pending_approval.as_ref().unwrap().call_id
                        ));
                        cx.notify();
                    });
                });
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

    fn build_tools(
        home: &std::path::Path,
        workspace_root: &std::path::Path,
        harness: &HarnessSetup,
    ) -> Arc<ToolRegistry> {
        let mut tools = ToolRegistry::new();
        if harness.enables("echo") {
            tools.register(Arc::new(EchoTool));
        }

        let needs_filesystem = ["read_file", "list_dir", "write_file", "run_command"]
            .iter()
            .any(|tool| harness.enables(tool));
        if needs_filesystem {
            let Ok(filesystem) = ScopedFs::new(workspace_root) else {
                return Arc::new(tools);
            };
            let filesystem = Arc::new(filesystem);
            if harness.enables("read_file") {
                tools.register(Arc::new(ReadFileTool::new(filesystem.clone())));
            }
            if harness.enables("list_dir") {
                tools.register(Arc::new(ListDirTool::new(filesystem.clone())));
            }
            if harness.enables("write_file") {
                tools.register(Arc::new(WriteFileTool::new(filesystem.clone())));
            }

            let shell_path = home.join(".dsh-rs").join("shell.json");
            if !shell_path.exists() {
                let _ = ShellPolicy::default()
                    .canonicalize()
                    .unwrap()
                    .save(&shell_path);
            }
            match ShellPolicy::load(&shell_path) {
                Ok(shell_policy)
                    if !shell_policy.allowed_binaries.is_empty()
                        && harness.enables("run_command") =>
                {
                    tools.register(Arc::new(CommandTool::new(filesystem, shell_policy)));
                }
                Ok(_) => {}
                Err(error) => eprintln!("shell policy load failed ({error}); shell stays disabled"),
            }
        }
        Arc::new(tools)
    }

    fn build_terminal_tool(
        home: &std::path::Path,
        workspace_root: &std::path::Path,
    ) -> Option<Arc<CommandTool>> {
        let filesystem = ScopedFs::new(workspace_root).ok()?;
        let shell_path = home.join(".dsh-rs").join("shell.json");
        if !shell_path.exists() {
            ShellPolicy::default()
                .canonicalize()
                .ok()?
                .save(&shell_path)
                .ok()?;
        }

        match ShellPolicy::load(&shell_path) {
            Ok(policy) => Some(Arc::new(CommandTool::new(Arc::new(filesystem), policy))),
            Err(error) => {
                eprintln!("shell policy load failed ({error}); terminal dock stays disabled");
                None
            }
        }
    }

    fn load_spaces(
        path: &std::path::Path,
        local_root: &std::path::Path,
    ) -> (SpacesConfig, Option<String>) {
        let local = SpacesConfig::local(local_root);
        if !path.exists() {
            if let Err(error) = local.save(path) {
                return (local, Some(error.to_string()));
            }
            return (local, None);
        }

        match SpacesConfig::load(path) {
            Ok(config) => (config, None),
            Err(error) => (local, Some(error.to_string())),
        }
    }

    fn load_harness_setups(path: &std::path::Path) -> (HarnessSetupsConfig, Option<String>) {
        let defaults = HarnessSetupsConfig::default();
        if !path.exists() {
            if let Err(error) = defaults.save(path) {
                return (defaults, Some(error.to_string()));
            }
            return (defaults, None);
        }

        match HarnessSetupsConfig::load(path) {
            Ok(config) => (config, None),
            Err(error) => (defaults, Some(error.to_string())),
        }
    }

    fn prepare_harness_setups(
        path: &std::path::Path,
        setup_file_existed: bool,
        loaded: HarnessSetupsConfig,
        legacy_prompt: SystemPromptConfig,
    ) -> (HarnessSetupsConfig, Option<String>) {
        if setup_file_existed {
            return (loaded, None);
        }

        let migrated = loaded.with_standard_prompt(legacy_prompt);
        if let Err(error) = migrated.save(path) {
            return (migrated, Some(error.to_string()));
        }
        (migrated, None)
    }

    fn resolve_space_id(config: &SpacesConfig, session_space_id: Option<&str>) -> String {
        session_space_id
            .filter(|id| config.get(id).is_some())
            .map(str::to_string)
            .or_else(|| config.spaces().first().map(|space| space.id().to_string()))
            .unwrap_or_else(|| "local".to_string())
    }

    fn cycled_space_id(
        config: &SpacesConfig,
        current_space_id: &str,
        forward: bool,
    ) -> Option<String> {
        let spaces = config.spaces();
        let index = spaces
            .iter()
            .position(|space| space.id() == current_space_id)?;
        let next = if forward {
            index + 1
        } else {
            index + spaces.len().saturating_sub(1)
        } % spaces.len();
        Some(spaces[next].id().to_string())
    }

    fn resolve_harness_id(
        config: &HarnessSetupsConfig,
        session_harness_id: Option<&str>,
    ) -> String {
        session_harness_id
            .filter(|id| config.get(id).is_some())
            .map(str::to_string)
            .or_else(|| config.setups().first().map(|setup| setup.id().to_string()))
            .unwrap_or_else(|| "standard".to_string())
    }

    fn cycled_harness_id(
        config: &HarnessSetupsConfig,
        current_harness_id: &str,
        forward: bool,
    ) -> Option<String> {
        let setups = config.setups();
        let index = setups
            .iter()
            .position(|setup| setup.id() == current_harness_id)?;
        let next = if forward {
            index + 1
        } else {
            index + setups.len().saturating_sub(1)
        } % setups.len();
        Some(setups[next].id().to_string())
    }

    fn selected_harness(&self) -> HarnessSetup {
        self.harness_setups
            .get(&self.selected_harness_id)
            .cloned()
            .or_else(|| self.harness_setups.setups().first().cloned())
            .expect("harness setups config always has a fail-safe setup")
    }

    fn reload_model(&mut self) -> Result<(), String> {
        let selection = ModelSelection::select_from_environment(Some(&self.home))
            .map_err(|error| error.to_string())?;
        let harness = self.selected_harness();
        self.agent = AgentLoop::new(selection.adapter.clone(), self.tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(harness.system_prompt().render())
            .with_approver(self.approver.clone())
            .with_max_steps(harness.max_steps().into());
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
            let first_change = if let WorkspaceChangesState::Ready(changes) = &state {
                changes.files.first().cloned()
            } else {
                None
            };
            cx.update(|cx| {
                workspace_handle.update(cx, |workspace, cx| {
                    workspace.changes = state;
                    workspace.changes_scanning = false;
                    workspace.selected_change = None;
                    workspace.selected_diff = None;
                    workspace.diff_error = None;
                    workspace.diff_loading = false;
                    cx.notify();
                    if let Some(file) = first_change {
                        workspace.load_change_diff(file.path, file.staged, cx);
                    }
                })
            });
        })
        .detach();
    }

    fn load_change_diff(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        if self.diff_loading {
            return;
        }
        if self
            .selected_change
            .as_ref()
            .is_some_and(|(current, current_staged)| *current == path && *current_staged == staged)
            && self.selected_diff.is_some()
        {
            return;
        }

        self.selected_change = Some((path.clone(), staged));
        self.selected_diff = None;
        self.diff_error = None;
        self.diff_loading = true;
        let root = self.workspace_root.clone();
        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(
                    async move { WorkspaceChanges::diff_blocking(root, &path, staged) },
                )
                .await;
            cx.update(|cx| {
                workspace_handle.update(cx, |workspace, cx| {
                    workspace.diff_loading = false;
                    match result {
                        Ok(diff) => workspace.selected_diff = Some(diff),
                        Err(error) => workspace.diff_error = Some(error),
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        let space_id = self.selected_space_id.clone();
        let harness_id = self.selected_harness_id.clone();
        match self.store.create_in_space_and_harness(
            "New session",
            Some(self.model.clone()),
            Some(space_id),
            Some(harness_id),
        ) {
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
            self.activate_configuration_for_selection();
            self.refresh_changes(cx);
            self.status = SharedString::from("Session loaded.");
            self.refresh();
            self.sync_rename_input(cx);
        } else {
            self.status = SharedString::from("Session no longer exists.");
        }
        cx.notify();
    }

    fn activate_configuration_for_selection(&mut self) {
        let Some(view) = self.selected.as_ref().map(|log| log.view()) else {
            return;
        };
        let space_id = Self::resolve_space_id(&self.spaces_config, view.space_id.as_deref());
        let harness_id = Self::resolve_harness_id(&self.harness_setups, view.harness_id.as_deref());
        if let Some(space) = self.spaces_config.get(&space_id) {
            self.workspace_root = space.root().to_path_buf();
            self.selected_harness_id = harness_id;
            let harness = self.selected_harness();
            self.tools = Self::build_tools(&self.home, &self.workspace_root, &harness);
            self.terminal_tool = Self::build_terminal_tool(&self.home, &self.workspace_root);
            if let Err(error) = self.reload_model() {
                self.status = SharedString::from(format!(
                    "Session configuration activated, but model reload failed: {error}"
                ));
            }
        }
        self.selected_space_id = space_id;
        if self.harness_setups.get(&self.selected_harness_id).is_none() {
            self.selected_harness_id = Self::resolve_harness_id(&self.harness_setups, None);
        }
    }

    fn select_space(&mut self, space_id: &str, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            cx.notify();
            return;
        }
        let Some(space) = self.spaces_config.get(space_id).cloned() else {
            self.status = SharedString::from("Space no longer exists.");
            cx.notify();
            return;
        };

        self.workspace_root = space.root().to_path_buf();
        let harness = self.selected_harness();
        self.tools = Self::build_tools(&self.home, &self.workspace_root, &harness);
        self.terminal_tool = Self::build_terminal_tool(&self.home, &self.workspace_root);
        if let Err(error) = self.reload_model() {
            self.status = SharedString::from(format!("Could not activate space: {error}"));
            cx.notify();
            return;
        }
        self.selected_space_id = space_id.to_string();
        if let Some(selected) = &self.selected {
            if let Err(error) = selected.set_space(space_id) {
                self.status = SharedString::from(format!("Could not save session space: {error}"));
                cx.notify();
                return;
            }
        }
        self.refresh();
        self.refresh_changes(cx);
        self.status = SharedString::from(format!("{} space active.", space.name()));
        cx.notify();
    }

    fn cycle_space(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(next) =
            Self::cycled_space_id(&self.spaces_config, &self.selected_space_id, forward)
        {
            self.select_space(&next, cx);
        }
    }

    fn next_space(&mut self, _: &NextSpace, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_space(true, cx);
    }

    fn previous_space(&mut self, _: &PreviousSpace, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_space(false, cx);
    }

    fn select_harness(&mut self, harness_id: &str, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            cx.notify();
            return;
        }
        let Some(harness) = self.harness_setups.get(harness_id).cloned() else {
            self.status = SharedString::from("Harness setup no longer exists.");
            cx.notify();
            return;
        };

        self.selected_harness_id = harness_id.to_string();
        self.tools = Self::build_tools(&self.home, &self.workspace_root, &harness);
        if let Err(error) = self.reload_model() {
            self.status = SharedString::from(format!("Could not activate setup: {error}"));
            cx.notify();
            return;
        }
        if let Some(selected) = &self.selected {
            if let Err(error) = selected.set_harness(harness_id) {
                self.status = SharedString::from(format!("Could not save session setup: {error}"));
                cx.notify();
                return;
            }
        }
        self.refresh();
        self.status = SharedString::from(format!(
            "{} setup active. {} tools, {} max steps.",
            harness.name(),
            harness.enabled_tools().len(),
            harness.max_steps()
        ));
        cx.notify();
    }

    fn cycle_harness(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(next) =
            Self::cycled_harness_id(&self.harness_setups, &self.selected_harness_id, forward)
        {
            self.select_harness(&next, cx);
        }
    }

    fn next_harness(&mut self, _: &NextHarness, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_harness(true, cx);
    }

    fn previous_harness(&mut self, _: &PreviousHarness, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_harness(false, cx);
    }

    fn toggle_terminal(&mut self, _: &ToggleTerminal, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_settings.terminal_visible = !self.ui_settings.terminal_visible;
        if let Err(error) = self.ui_settings.save(&self.ui_settings_path) {
            self.status = SharedString::from(format!("Could not save terminal state: {error}"));
        }
        cx.notify();
    }

    fn focus_terminal(&mut self, _: &FocusTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ui_settings.terminal_visible {
            self.ui_settings.terminal_visible = true;
            if let Err(error) = self.ui_settings.save(&self.ui_settings_path) {
                self.status = SharedString::from(format!("Could not save terminal state: {error}"));
            }
        }
        let focus = self.terminal_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn clear_terminal(&mut self, _: &ClearTerminal, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal_history.clear();
        self.status = SharedString::from("Terminal history cleared.");
        cx.notify();
    }

    fn run_terminal_command(
        &mut self,
        _: &RunTerminalCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_running {
            self.status = SharedString::from("Wait for the current command to finish.");
            cx.notify();
            return;
        }

        let command = self.terminal_input.read(cx).text();
        if command.trim().is_empty() {
            self.status = SharedString::from("Terminal command is empty.");
            cx.notify();
            return;
        }

        let argv = match parse_argv(&command) {
            Ok(argv) => argv,
            Err(error) => {
                self.terminal_history.push(TerminalEntry {
                    command,
                    output: error.to_string(),
                    ok: false,
                });
                self.status = SharedString::from("Terminal command could not be parsed.");
                cx.notify();
                return;
            }
        };

        let Some(tool) = self.terminal_tool.clone() else {
            self.terminal_history.push(TerminalEntry {
                command,
                output: "Terminal is disabled because the shell policy is unavailable.".into(),
                ok: false,
            });
            self.status = SharedString::from("Terminal dock is disabled.");
            cx.notify();
            return;
        };

        self.terminal_running = true;
        self.status = SharedString::from("Command running...");
        self.terminal_input.update(cx, |input, cx| input.clear(cx));
        cx.notify();

        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            let entry = cx
                .background_spawn(async move { run_command(tool, command, argv).await })
                .await;
            let status = if entry.ok {
                SharedString::from("Command complete.")
            } else {
                SharedString::from("Command failed.")
            };

            cx.update(|cx| {
                workspace_handle.update(cx, |workspace, cx| {
                    workspace.terminal_history.push(entry);
                    workspace.terminal_running = false;
                    workspace.status = status;
                    cx.notify();
                });
            });
        })
        .detach();
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
                            cx.update(|cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.refresh();
                                    cx.notify();
                                });
                            });
                        }
                    }
                }
            };

            cx.update(|cx| {
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
            .pt(px(TITLEBAR_HEIGHT))
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
            .children(
                self.spaces_config
                    .spaces()
                    .iter()
                    .enumerate()
                    .map(|(index, space)| {
                        let id = space.id().to_string();
                        let selected = self.selected_space_id == id;
                        div()
                            .id(("space", index))
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
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.select_space(&id.clone(), cx)
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(theme.text)
                                    .child(space.name().to_string()),
                            )
                            .child(div().text_size(px(11.)).text_color(theme.faint).child({
                                let root = space.root().display().to_string();
                                if root.chars().count() > 34 {
                                    format!(
                                        "{}...{}",
                                        root.chars().take(18).collect::<String>(),
                                        root.chars()
                                            .rev()
                                            .take(13)
                                            .collect::<Vec<_>>()
                                            .into_iter()
                                            .rev()
                                            .collect::<String>()
                                    )
                                } else {
                                    root
                                }
                            }))
                    }),
            )
            .child(
                div()
                    .pt_2()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.faint)
                    .child("HARNESS SETUPS"),
            )
            .children(
                self.harness_setups
                    .setups()
                    .iter()
                    .enumerate()
                    .map(|(index, setup)| {
                        let id = setup.id().to_string();
                        let selected = self.selected_harness_id == id;
                        div()
                            .id(("harness-setup", index))
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
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.select_harness(&id.clone(), cx)
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(theme.text)
                                    .child(setup.name().to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.faint)
                                    .child(format!(
                                        "{} tools · {} max steps",
                                        setup.enabled_tools().len(),
                                        setup.max_steps()
                                    )),
                            )
                    }),
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
                                "{} · {} · {} · {} events{}",
                                self.spaces_config
                                    .get(summary.space_id.as_deref().unwrap_or_default())
                                    .map(|space| space.name())
                                    .unwrap_or("Local harness"),
                                self.harness_setups
                                    .get(summary.harness_id.as_deref().unwrap_or_default())
                                    .map(|setup| setup.name())
                                    .unwrap_or("Standard"),
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

    fn render_terminal_dock(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = Theme::dark();

        div()
            .id("terminal-dock")
            .h(px(TERMINAL_DOCK_HEIGHT))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(
                div()
                    .h(px(32.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.faint)
                            .child("COMMAND DOCK"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("clear-terminal")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .text_size(px(10.))
                                    .text_color(theme.muted)
                                    .hover(|style| style.bg(theme.raised).cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.clear_terminal(&ClearTerminal, window, cx)
                                        }),
                                    )
                                    .child("Clear"),
                            )
                            .child(
                                div()
                                    .id("close-terminal")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .text_size(px(10.))
                                    .text_color(theme.muted)
                                    .hover(|style| style.bg(theme.raised).cursor_pointer())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.toggle_terminal(&ToggleTerminal, window, cx)
                                        }),
                                    )
                                    .child("Close"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().child(self.terminal_input.clone()))
                    .child(
                        div()
                            .id("run-terminal")
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .bg(if self.terminal_running {
                                theme.raised
                            } else {
                                theme.accent
                            })
                            .text_size(px(11.))
                            .text_color(if self.terminal_running {
                                theme.faint
                            } else {
                                theme.background
                            })
                            .hover(|style| {
                                if self.terminal_running {
                                    style
                                } else {
                                    style.cursor_pointer()
                                }
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    workspace.run_terminal_command(&RunTerminalCommand, window, cx)
                                }),
                            )
                            .child(if self.terminal_running { "..." } else { "Run" }),
                    ),
            )
            .child(
                div()
                    .id("terminal-output")
                    .flex_1()
                    .overflow_scroll()
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(if self.terminal_running {
                        div()
                            .text_size(px(11.))
                            .text_color(theme.faint)
                            .child("Running")
                    } else {
                        div()
                            .text_size(px(11.))
                            .text_color(theme.faint)
                            .child("Direct allowlisted commands only")
                    })
                    .children(self.terminal_history.entries().iter().map(|entry| {
                        let output = if entry.output.chars().count() > 3000 {
                            format!("{}...", entry.output.chars().take(2997).collect::<String>())
                        } else {
                            entry.output.clone()
                        };
                        let command = if entry.command.chars().count() > 180 {
                            format!("{}...", entry.command.chars().take(177).collect::<String>())
                        } else {
                            entry.command.clone()
                        };

                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .pb_2()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.warning)
                                    .child(command),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(if entry.ok { theme.muted } else { theme.danger })
                                    .child(output),
                            )
                    })),
            )
    }

    fn render_changes_section(&self, cx: &mut Context<Self>) -> gpui::Div {
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
                                let path = file.path.clone();
                                let click_path = path.clone();
                                let staged = file.staged;
                                let selected = self.selected_change.as_ref().is_some_and(
                                    |(selected, selected_staged)| {
                                        *selected == path && *selected_staged == staged
                                    },
                                );
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
                                    .rounded_sm()
                                    .bg(if selected {
                                        theme.raised
                                    } else {
                                        theme.background
                                    })
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |workspace, _, _, cx| {
                                            workspace.load_change_diff(
                                                click_path.clone(),
                                                staged,
                                                cx,
                                            )
                                        }),
                                    )
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

    fn render_selected_diff_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = Theme::dark();
        let section = div()
            .flex()
            .flex_col()
            .gap_2()
            .pb_3()
            .border_b_1()
            .border_color(theme.border);

        let Some((path, staged)) = &self.selected_change else {
            return section;
        };

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(path.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if *staged { theme.warning } else { theme.faint })
                            .child(if *staged { "staged" } else { "worktree" }),
                    )
                    .child(
                        div()
                            .id("close-diff")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_size(px(10.))
                            .text_color(theme.muted)
                            .hover(|style| style.bg(theme.raised).cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.selected_change = None;
                                    workspace.selected_diff = None;
                                    workspace.diff_error = None;
                                    workspace.diff_loading = false;
                                    cx.notify();
                                }),
                            )
                            .child("Close"),
                    ),
            );

        let section = section.child(header);
        if self.diff_loading {
            return section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child("Loading diff"),
            );
        }
        if let Some(error) = &self.diff_error {
            return section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }

        let Some(diff) = &self.selected_diff else {
            return section;
        };
        section
            .children(diff.lines.iter().map(|line| {
                let color = match line.kind {
                    DiffLineKind::Meta => theme.faint,
                    DiffLineKind::Hunk => theme.accent,
                    DiffLineKind::Context => theme.muted,
                    DiffLineKind::Addition => theme.success,
                    DiffLineKind::Deletion => theme.danger,
                };
                let content = if line.content.chars().count() > 120 {
                    format!("{}...", line.content.chars().take(117).collect::<String>())
                } else {
                    line.content.clone()
                };
                div().text_size(px(10.)).text_color(color).child(content)
            }))
            .when(diff.truncated, |element| {
                element.child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child("Diff truncated at 500 lines"),
                )
            })
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
        let harness = self.selected_harness();
        let harness_name = harness.name().to_string();
        let harness_detail = format!(
            "{} tools · {} max steps",
            harness.enabled_tools().len(),
            harness.max_steps()
        );
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
            .pt(px(TITLEBAR_HEIGHT))
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
                    .child(self.render_changes_section(cx))
                    .child(self.render_selected_diff_section(cx))
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
                                            .child("Setup"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .items_end()
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.muted)
                                                    .child(harness_name),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(theme.faint)
                                                    .child(harness_detail),
                                            ),
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

impl Workspace {
    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let title = self
            .selected_view
            .as_ref()
            .map(|view| view.title.clone())
            .unwrap_or_default();
        let target = SharedString::from(self.selected_harness().name().to_string());
        let cluster_end = 88.0 + 76.0 + SPACE_LG;
        let title_left = if self.ui_settings.sidebar_visible {
            SIDEBAR_WIDTH + SPACE_LG
        } else {
            cluster_end
        };
        let title_gutter = (title_left - cluster_end).max(0.0);

        div()
            .id("comet-titlebar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(TITLEBAR_HEIGHT))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(10.0))
            .pt(px(TITLEBAR_TOP_PAD))
            .window_control_area(WindowControlArea::Drag)
            .child(div().w(px(titlebar_spacer_width(
                cfg!(target_os = "macos"),
                false,
                10.0,
            ))))
            .child(
                div()
                    .id("titlebar-toggle-sidebar")
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(CONTROL_RADIUS))
                    .occlude()
                    .hover(|style| style.bg(theme.element_hover).cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, window, cx| {
                            workspace.toggle_sidebar(&ToggleSidebar, window, cx)
                        }),
                    )
                    .child(
                        icon(icons::SIDEBAR_MINIMALISTIC_LEFT)
                            .size(px(16.0))
                            .text_color(theme.muted),
                    ),
            )
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.35)
                    .child(
                        icon(icons::ARROW_LEFT)
                            .size(px(16.0))
                            .text_color(theme.muted),
                    ),
            )
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.35)
                    .child(
                        icon(icons::ARROW_RIGHT)
                            .size(px(16.0))
                            .text_color(theme.muted),
                    ),
            )
            .child(div().w(px(SPACE_LG)).flex_none())
            .child(div().w(px(title_gutter)).flex_none())
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.0))
                            .text_color(theme.muted)
                            .child(target),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("titlebar-toggle-context")
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(CONTROL_RADIUS))
                    .occlude()
                    .hover(|style| style.bg(theme.element_hover).cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, window, cx| {
                            workspace.toggle_context(&ToggleContext, window, cx)
                        }),
                    )
                    .child(
                        icon(icons::SIDEBAR_MINIMALISTIC)
                            .size(px(16.0))
                            .text_color(theme.muted),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let sidebar_visible = self.ui_settings.sidebar_visible;
        let context_visible = self.ui_settings.context_pane_visible;
        let terminal_visible = self.ui_settings.terminal_visible;
        div()
            .key_context("Workspace")
            .id("workspace-root")
            .size_full()
            .relative()
            .font_family(theme.font_sans.clone())
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
            .on_action(cx.listener(Self::next_space))
            .on_action(cx.listener(Self::previous_space))
            .on_action(cx.listener(Self::next_harness))
            .on_action(cx.listener(Self::previous_harness))
            .on_action(cx.listener(Self::run_terminal_command))
            .on_action(cx.listener(Self::focus_terminal))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(Self::clear_terminal))
            .when(sidebar_visible, |el| el.child(self.render_sidebar(cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .pt(px(TITLEBAR_HEIGHT))
                    .bg(theme.background)
                    .child(self.render_transcript())
                    .when(terminal_visible, |el| {
                        el.child(self.render_terminal_dock(cx))
                    })
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
            .child(self.render_titlebar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::harness::HarnessSetupsConfig;
    use harness_core::spaces::SpacesConfig;
    use harness_core::tools::ToolInvocation;

    #[test]
    fn build_tools_reads_from_the_selected_space_root() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("hello.txt"), "selected root").unwrap();

        let setups = HarnessSetupsConfig::default();
        let harness = setups.get("standard").unwrap();
        let tools = Workspace::build_tools(home.path(), root.path(), harness);
        let output = futures::executor::block_on(tools.execute(ToolInvocation {
            call_id: "test".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "hello.txt"}),
        }));

        assert!(output.ok);
        assert_eq!(output.output, "selected root");
    }

    #[test]
    fn spaces_config_load_fails_closed_to_local() {
        let home = tempfile::tempdir().unwrap();
        let local_root = home.path().join("workspace");
        std::fs::create_dir_all(&local_root).unwrap();
        let path = home.path().join("spaces.json");
        let local = SpacesConfig::local(&local_root);

        let (loaded, error) = Workspace::load_spaces(&path, &local_root);
        assert_eq!(loaded, local);
        assert!(error.is_none());

        std::fs::write(&path, "{invalid").unwrap();
        let (fallback, error) = Workspace::load_spaces(&path, &local_root);
        assert_eq!(fallback, local);
        assert!(error.is_some());
    }

    #[test]
    fn session_space_references_resolve_safely() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let config = SpacesConfig::parse(&format!(
            r#"{{"spaces":[{{"id":"local","name":"Local","root":{:?}}},{{"id":"project","name":"Project","root":{:?}}}]}}"#,
            first.path(),
            second.path()
        ))
        .unwrap();

        assert_eq!(
            Workspace::resolve_space_id(&config, Some("project")),
            "project"
        );
        assert_eq!(Workspace::resolve_space_id(&config, None), "local");
        assert_eq!(
            Workspace::resolve_space_id(&config, Some("missing")),
            "local"
        );
    }

    #[test]
    fn space_selection_cycles_forward_and_backward() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let config = SpacesConfig::parse(&format!(
            r#"{{"spaces":[{{"id":"local","name":"Local","root":{:?}}},{{"id":"project","name":"Project","root":{:?}}}]}}"#,
            first.path(),
            second.path()
        ))
        .unwrap();

        assert_eq!(
            Workspace::cycled_space_id(&config, "local", true),
            Some("project".to_string())
        );
        assert_eq!(
            Workspace::cycled_space_id(&config, "project", true),
            Some("local".to_string())
        );
        assert_eq!(
            Workspace::cycled_space_id(&config, "project", false),
            Some("local".to_string())
        );
        assert_eq!(Workspace::cycled_space_id(&config, "missing", false), None);
    }

    #[test]
    fn build_tools_respects_the_harness_setup_allowlist() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("hello.txt"), "setup root").unwrap();
        let setups = HarnessSetupsConfig::parse(
            r#"{"setups":[{"id":"research","name":"Research","system_prompt":{"include_harness_identity":true,"persona":"Read only."},"enabled_tools":["read_file"],"max_steps":4}]}"#,
        )
        .unwrap();
        let setup = setups.get("research").unwrap();

        let tools = Workspace::build_tools(home.path(), root.path(), setup);
        let names = tools
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn harness_setups_load_fails_closed_to_defaults() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("harness-setups.json");
        let defaults = HarnessSetupsConfig::default();

        let (loaded, error) = Workspace::load_harness_setups(&path);
        assert_eq!(loaded, defaults);
        assert!(error.is_none());

        std::fs::write(&path, "{invalid").unwrap();
        let (fallback, error) = Workspace::load_harness_setups(&path);
        assert_eq!(fallback, defaults);
        assert!(error.is_some());
    }

    #[test]
    fn first_launch_persists_a_customized_global_prompt_into_setups() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("harness-setups.json");
        let prompt = SystemPromptConfig {
            include_harness_identity: false,
            persona: "Legacy local persona.".into(),
        };

        let (loaded, error) = Workspace::prepare_harness_setups(
            &path,
            false,
            HarnessSetupsConfig::default(),
            prompt.clone(),
        );

        assert!(error.is_none());
        assert_eq!(loaded.get("standard").unwrap().system_prompt(), &prompt);
        assert_eq!(HarnessSetupsConfig::load(&path).unwrap(), loaded);
    }

    #[test]
    fn existing_setup_files_own_their_standard_prompt() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("harness-setups.json");
        let setups = HarnessSetupsConfig::default();
        setups.save(&path).unwrap();
        let legacy = SystemPromptConfig {
            include_harness_identity: false,
            persona: "Legacy local persona.".into(),
        };

        let (loaded, error) =
            Workspace::prepare_harness_setups(&path, true, setups.clone(), legacy);

        assert!(error.is_none());
        assert_eq!(loaded, setups);
    }

    #[test]
    fn harness_references_resolve_safely() {
        let config = HarnessSetupsConfig::default();
        let standard = config.get("standard").unwrap().id().to_string();
        let research = config.get("research").unwrap().id().to_string();

        assert_eq!(
            Workspace::resolve_harness_id(&config, Some(&research)),
            research
        );
        assert_eq!(Workspace::resolve_harness_id(&config, None), standard);
        assert_eq!(
            Workspace::resolve_harness_id(&config, Some("missing")),
            standard
        );
    }

    #[test]
    fn harness_selection_cycles_forward_and_backward() {
        let config = HarnessSetupsConfig::default();
        let standard = config.get("standard").unwrap().id().to_string();
        let research = config.get("research").unwrap().id().to_string();
        let minimal = config.get("minimal").unwrap().id().to_string();

        assert_eq!(
            Workspace::cycled_harness_id(&config, &standard, true),
            Some(research)
        );
        assert_eq!(
            Workspace::cycled_harness_id(&config, &minimal, true),
            Some(standard.clone())
        );
        assert_eq!(
            Workspace::cycled_harness_id(&config, &standard, false),
            Some(minimal)
        );
        assert_eq!(Workspace::cycled_harness_id(&config, "missing", true), None);
    }

    #[test]
    fn terminal_focus_action_exists_for_keyboard_reachable_dock() {
        let handler: fn(&mut Workspace, &FocusTerminal, &mut Window, &mut Context<Workspace>) =
            Workspace::focus_terminal;
        assert!(format!("{handler:p}") != "0");
    }
}
