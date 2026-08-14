use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::changes::{
    ChangeStatus, DiffLineKind, FileDiff, WorkspaceChanges, WorkspaceChangesState,
};
use crate::edge_fade::edge_faded;
use crate::frost;
use crate::icons::{self, icon};
use crate::input::{ChatInput, InputKind};
use crate::motion;
use crate::settings::appearance::AppearancePage;
use crate::settings::providers::{ProvidersEvent, ProvidersPage};
use crate::settings::{UiSettings, RIGHT_PANE_DEFAULT, SIDEBAR_DEFAULT, TERMINAL_DEFAULT_HEIGHT};
use crate::skill_picker::{entry_replacement, SkillPickerState};
use crate::terminal::{
    parse_argv, run_command, terminal_panel_bg, TerminalEntry, TerminalHistory,
    TERMINAL_TAB_BAR_HEIGHT, TERMINAL_TAB_WIDTH,
};
use crate::theme::{self, Theme};
use gpui::{
    actions, div, linear_color_stop, linear_gradient, prelude::*, px, AnimationExt, AnyElement,
    Context, Entity, FontWeight, MouseButton, SharedString, Subscription, Window,
    WindowControlArea,
};
use harness_core::agent::AgentLoop;
use harness_core::approval::{
    ApprovalPolicy, ApprovalRequest, ChannelApprover, GatedApprover, ToolApprover,
};
use harness_core::cancellation::TurnCancellation;
use harness_core::events::{TodoItem, TodoStatus};
use harness_core::harness::{HarnessSetup, HarnessSetupsConfig};
use harness_core::prompt::SystemPromptConfig;
use harness_core::remote::{CredentialStore, ModelSelection};
use harness_core::session::{SessionView, TranscriptEntry};
use harness_core::skills::{FileSystemSkillProvider, SkillFileSystemConfig, SkillRegistry};
use harness_core::spaces::SpacesConfig;
use harness_core::store::{SessionStore, SessionSummary};
use harness_core::tools::fs::{ListDirTool, ReadFileTool, ScopedFs, WriteFileTool};
use harness_core::tools::shell::{CommandTool, ShellPolicy};
use harness_core::tools::{EchoTool, SkillTool, TodoTool, ToolRegistry};
use zeroize::Zeroize;

const TITLEBAR_CLUSTER_BUTTONS_WIDTH: f32 = 24.0 * 3.0 + 2.0 * 2.0;
const SIDEBAR_GLASS_FADE_BAND: f32 = 32.0;

pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen {
        12.0
    } else {
        88.0
    }
}

pub fn titlebar_spacer_width(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    if !is_macos {
        return 0.0;
    }
    (titlebar_cluster_start(fullscreen) - container_pad).max(0.0)
}

pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else {
        10.0
    }
}

#[allow(dead_code)]
pub fn cluster_clearance(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    (cluster_buttons_start(is_macos, fullscreen) + TITLEBAR_CLUSTER_BUTTONS_WIDTH + Theme::SPACE_SM
        - container_pad)
        .max(0.0)
}

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
        FocusTerminal,
        OpenSettings,
        CloseSettings
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Chat,
    Settings(SettingsSection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarSessionRow {
    id: uuid::Uuid,
    title: String,
    space_name: String,
    harness_name: String,
    time_ago: String,
    working: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpacesMenuRow {
    All,
    Space(String),
    AddSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsSection {
    Providers,
    Appearance,
}

impl SettingsSection {
    const ALL: [Self; 2] = [Self::Providers, Self::Appearance];

    fn label(self) -> &'static str {
        match self {
            Self::Providers => "Providers",
            Self::Appearance => "Appearance",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Providers => icons::DEEPSEEK_MARK,
            Self::Appearance => icons::TUNING,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillRowState {
    Running,
    Ok,
    Error,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillRowModel {
    name: String,
    output: Option<String>,
    error_summary: Option<String>,
    state: SkillRowState,
}

pub struct Workspace {
    store: Arc<SessionStore>,
    agent: AgentLoop,
    pub(crate) input: Entity<ChatInput>,
    search_input: Entity<ChatInput>,
    spaces_input: Entity<ChatInput>,
    project_name_input: Entity<ChatInput>,
    project_path_input: Entity<ChatInput>,
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
    skill_tool: Option<Arc<SkillTool>>,
    skill_picker: SkillPickerState,
    skill_picker_loading: bool,
    skill_picker_error: Option<SharedString>,
    skill_picker_cache_key: Option<String>,
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
    todo_panel_collapsed: bool,
    expanded_skill_calls: HashSet<String>,
    route: Route,
    sidebar_space_filter: Option<String>,
    spaces_menu_open: bool,
    spaces_menu_active: Option<usize>,
    add_space_open: bool,
    add_space_error: Option<String>,
    appearance_page: Option<Entity<AppearancePage>>,
    providers_page: Option<Entity<ProvidersPage>>,
    #[allow(dead_code)]
    providers_subscription: Option<Subscription>,
}

impl Workspace {
    fn spaces_menu_rows(spaces: &SpacesConfig, query: &str) -> Vec<SpacesMenuRow> {
        let query = query.trim();
        let mut rows = Vec::new();
        if query.is_empty() {
            rows.push(SpacesMenuRow::All);
        }
        rows.extend(
            spaces
                .spaces()
                .iter()
                .filter(|space| {
                    query.is_empty() || space.name().to_lowercase().contains(&query.to_lowercase())
                })
                .map(|space| SpacesMenuRow::Space(space.id().to_string())),
        );
        rows.push(SpacesMenuRow::AddSpace);
        rows
    }

    fn spaces_menu_step(active: Option<usize>, count: usize, delta: i32) -> Option<usize> {
        if count == 0 {
            return None;
        }
        let Some(active) = active else {
            return Some(0);
        };
        let active = active as isize;
        if active < 0 || active as usize >= count {
            return Some(0);
        }
        let next = (active + delta as isize).rem_euclid(count as isize);
        Some(next as usize)
    }

    fn todo_items_from_arguments(arguments: &serde_json::Value) -> Option<Vec<TodoItem>> {
        let items = arguments.get("todos")?.clone();
        serde_json::from_value(items).ok()
    }

    fn todo_call_summary(arguments: &serde_json::Value) -> Option<String> {
        let todos = Self::todo_items_from_arguments(arguments)?;
        let done = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        Some(format!("{done}/{} done", todos.len()))
    }

    fn first_line(value: &str) -> &str {
        value.split('\n').next().unwrap_or("")
    }

    fn skill_call_name(call_id: &str, arguments: &serde_json::Value) -> String {
        if let Some(name) = arguments.get("name").and_then(|value| value.as_str()) {
            if !name.is_empty() {
                return Self::first_line(name).to_string();
            }
        }
        let raw = serde_json::to_string(arguments).unwrap_or_default();
        if raw.is_empty() {
            call_id.to_string()
        } else {
            Self::first_line(&raw).to_string()
        }
    }

    fn skill_row_model(
        call_id: &str,
        arguments: &serde_json::Value,
        output: Option<(&str, bool)>,
    ) -> SkillRowModel {
        let Some((output, ok)) = output else {
            return SkillRowModel {
                name: Self::skill_call_name(call_id, arguments),
                output: None,
                error_summary: None,
                state: SkillRowState::Running,
            };
        };

        let output = output.to_string();
        let normalized = output.to_lowercase();
        let state = if ok {
            SkillRowState::Ok
        } else if normalized.contains("cancelled") || normalized.contains("interrupted") {
            SkillRowState::Stopped
        } else {
            SkillRowState::Error
        };
        SkillRowModel {
            name: Self::skill_call_name(call_id, arguments),
            error_summary: (state == SkillRowState::Error)
                .then(|| Self::first_line(&output).to_string()),
            output: Some(output),
            state,
        }
    }

    fn todo_panel_progress(todos: &[TodoItem]) -> String {
        let done = todos
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
        let mut segments = Vec::new();
        if done > 0 {
            segments.push(format!("{done} completed"));
        }
        if in_progress > 0 {
            segments.push(format!("{in_progress} in progress"));
        }
        if pending > 0 {
            segments.push(format!("{pending} pending"));
        }
        segments.join("\u{2002}·\u{2002}")
    }

    fn register_project_space(
        config: &mut SpacesConfig,
        config_path: &std::path::Path,
        name: &str,
        root: &str,
    ) -> Result<String, String> {
        let original = config.clone();
        let space = config
            .add_project(name, root)
            .map_err(|error| format!("Could not register project: {error}"))?;
        if let Err(error) = config.save(config_path) {
            *config = original;
            return Err(format!("Could not publish project: {error}"));
        }
        Ok(space.id().to_string())
    }

    fn sidebar_session_rows(
        summaries: &[SessionSummary],
        spaces: &SpacesConfig,
        setups: &HarnessSetupsConfig,
        selected_space: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<SidebarSessionRow> {
        summaries
            .iter()
            .filter(|summary| match selected_space {
                Some(space_id) => summary.space_id.as_deref() == Some(space_id),
                None => true,
            })
            .map(|summary| SidebarSessionRow {
                id: summary.id,
                title: summary.title.clone(),
                space_name: summary
                    .space_id
                    .as_deref()
                    .and_then(|space_id| spaces.get(space_id))
                    .map(|space| space.name().to_string())
                    .unwrap_or_else(|| "Local harness".to_string()),
                harness_name: summary
                    .harness_id
                    .as_deref()
                    .and_then(|harness_id| setups.get(harness_id))
                    .map(|setup| setup.name().to_string())
                    .unwrap_or_else(|| "Standard".to_string()),
                time_ago: Self::relative_time(summary.updated_at, now),
                working: summary.turn_active,
            })
            .collect()
    }

    fn relative_time(
        timestamp: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> String {
        let seconds = (now - timestamp).num_seconds().max(0);
        if seconds < 60 {
            "now".to_string()
        } else if seconds < 3_600 {
            format!("{}m", seconds / 60)
        } else if seconds < 86_400 {
            format!("{}h", seconds / 3_600)
        } else {
            format!("{}d", seconds / 86_400)
        }
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| ChatInput::new(InputKind::Chat, cx));
        let search_input = cx.new(|cx| ChatInput::new(InputKind::Search, cx));
        let spaces_input = cx.new(|cx| ChatInput::new(InputKind::SpaceSearch, cx));
        let project_name_input = cx.new(|cx| ChatInput::new(InputKind::ProjectName, cx));
        let project_path_input = cx.new(|cx| ChatInput::new(InputKind::ProjectPath, cx));
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

        let (tools, skill_tool) =
            Self::build_tools_with_skills(&home, &workspace_root, &selected_harness);
        let terminal_tool = Self::build_terminal_tool(&home, &workspace_root);
        let agent = AgentLoop::new(selection.adapter.clone(), tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(system_prompt.clone())
            .with_approver(approver.clone())
            .with_max_steps(selected_harness.max_steps().into());
        let agent = match skill_tool.clone() {
            Some(skill_tool) => agent.with_skill_tool(skill_tool),
            None => agent,
        };
        let providers_page = cx.new(|_| {
            ProvidersPage::new(
                credential_input.clone(),
                credential_environment_override,
                credential_file_configured,
                SharedString::from(selection.model.clone()),
            )
        });
        let providers_subscription = cx.subscribe(
            &providers_page,
            |workspace: &mut Workspace, _, event: &ProvidersEvent, cx: &mut Context<Workspace>| {
                match event {
                    ProvidersEvent::SaveRequested(key) => {
                        workspace.save_provider_credential(key.clone(), cx)
                    }
                    ProvidersEvent::RemoveRequested => workspace.remove_credential(cx),
                }
            },
        );

        let mut workspace = Self {
            store,
            agent,
            input,
            search_input,
            spaces_input,
            project_name_input,
            project_path_input,
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
            selected_space_id: selected_space_id.clone(),
            harness_setups,
            selected_harness_id,
            workspace_root,
            credential_path,
            credential_environment_override,
            credential_file_configured,
            tools,
            skill_tool,
            skill_picker: SkillPickerState::default(),
            skill_picker_loading: false,
            skill_picker_error: None,
            skill_picker_cache_key: None,
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
            todo_panel_collapsed: true,
            expanded_skill_calls: HashSet::new(),
            route: Route::Chat,
            sidebar_space_filter: Some(selected_space_id.clone()),
            spaces_menu_open: false,
            spaces_menu_active: None,
            add_space_open: false,
            add_space_error: None,
            appearance_page: None,
            providers_page: Some(providers_page),
            providers_subscription: Some(providers_subscription),
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
        workspace.load_skill_picker(cx);
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

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_section(SettingsSection::Providers, cx);
    }

    fn open_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        if section == SettingsSection::Appearance && self.appearance_page.is_none() {
            self.appearance_page = Some(cx.new(AppearancePage::new));
        }
        self.route = Route::Settings(section);
        cx.notify();
    }

    fn close_settings(&mut self, _: &CloseSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        cx.notify();
    }

    fn toggle_context(&mut self, _: &ToggleContext, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_settings.context_pane_visible = !self.ui_settings.context_pane_visible;
        if let Err(error) = self.ui_settings.save(&self.ui_settings_path) {
            self.status = SharedString::from(format!("Could not save context state: {error}"));
        }
        cx.notify();
    }

    #[cfg(test)]
    fn build_tools(
        home: &std::path::Path,
        workspace_root: &std::path::Path,
        harness: &HarnessSetup,
    ) -> Arc<ToolRegistry> {
        Self::build_tools_with_skills(home, workspace_root, harness).0
    }

    fn build_tools_with_skills(
        home: &std::path::Path,
        workspace_root: &std::path::Path,
        harness: &HarnessSetup,
    ) -> (Arc<ToolRegistry>, Option<Arc<SkillTool>>) {
        let mut tools = ToolRegistry::new();
        let mut skill_tool = None;
        if harness.enables("echo") {
            tools.register(Arc::new(EchoTool));
        }
        if harness.enables("todo_write") {
            tools.register(Arc::new(TodoTool::new(true)));
        }
        if harness.enables("skill") {
            let mut skills = SkillRegistry::new();
            let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                include_default_roots: true,
                dsh_home: home.join(".dsh"),
                agents_home: home.join(".agents"),
                custom_skill_dirs: Vec::new(),
                bundled_skill_dir: None,
            });
            if let Ok(provider) = provider {
                if skills.register_provider(Arc::new(provider)).is_ok() {
                    let tool = Arc::new(SkillTool::new(Arc::new(skills)).with_cwd(workspace_root));
                    tools.register(tool.clone());
                    skill_tool = Some(tool);
                }
            }
        }

        let needs_filesystem = ["read_file", "list_dir", "write_file", "run_command"]
            .iter()
            .any(|tool| harness.enables(tool));
        if needs_filesystem {
            let Ok(filesystem) = ScopedFs::new(workspace_root) else {
                return (Arc::new(tools), skill_tool);
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
        (Arc::new(tools), skill_tool)
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
        let agent = AgentLoop::new(selection.adapter.clone(), self.tools.clone())
            .with_default_model(Some(selection.model.clone()))
            .with_system_prompt(harness.system_prompt().render())
            .with_approver(self.approver.clone())
            .with_max_steps(harness.max_steps().into());
        self.agent = match self.skill_tool.clone() {
            Some(skill_tool) => agent.with_skill_tool(skill_tool),
            None => agent,
        };
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

    fn sync_provider_page(&mut self, status: Option<SharedString>, cx: &mut Context<Self>) {
        let Some(page) = self.providers_page.clone() else {
            return;
        };
        let environment_override = self.credential_environment_override;
        let file_configured = self.credential_file_configured;
        let model = SharedString::from(self.model.clone());
        page.update(cx, |page, cx| {
            page.set_state(environment_override, file_configured, model, status, cx);
        });
    }

    fn save_credential(&mut self, _: &SaveCredential, _: &mut Window, cx: &mut Context<Self>) {
        let key = self.credential_input.read(cx).text();
        self.save_provider_credential(key, cx);
    }

    fn save_provider_credential(&mut self, key: String, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            self.sync_provider_page(Some(self.status.clone()), cx);
            cx.notify();
            return;
        }
        if self.credential_environment_override {
            self.status = SharedString::from(
                "DEEPSEEK_API_KEY is set by the launching environment; stored keys stay read-only.",
            );
            self.sync_provider_page(Some(self.status.clone()), cx);
            cx.notify();
            return;
        }

        let mut key = key;
        let save_result = CredentialStore::save(&key, &self.credential_path);
        key.zeroize();
        if let Err(error) = save_result {
            self.status = SharedString::from(format!("Could not save API key: {error}"));
            self.sync_provider_page(Some(self.status.clone()), cx);
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
        self.sync_provider_page(Some(self.status.clone()), cx);
        cx.notify();
    }

    fn remove_credential(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.status = SharedString::from("Wait for the current turn to finish.");
            self.sync_provider_page(Some(self.status.clone()), cx);
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
        self.sync_provider_page(Some(self.status.clone()), cx);
        cx.notify();
    }

    fn refresh(&mut self) {
        self.summaries = self.store.list();
        self.selected_view = self.selected.as_ref().map(|log| log.view());
    }

    fn skill_picker_cache_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.selected_space_id,
            self.selected_harness_id,
            self.workspace_root.display()
        )
    }

    fn load_skill_picker(&mut self, cx: &mut Context<Self>) {
        let cache_key = self.skill_picker_cache_key();
        if self.skill_picker_cache_key.as_deref() == Some(cache_key.as_str()) {
            return;
        }

        self.skill_picker_cache_key = Some(cache_key.clone());
        self.skill_picker = SkillPickerState::default();
        self.skill_picker_error = None;
        let Some(skill_tool) = self.skill_tool.clone() else {
            self.skill_picker_loading = false;
            self.sync_skill_picker(cx);
            cx.notify();
            return;
        };

        self.skill_picker_loading = true;
        let workspace_handle = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(skill_tool.slash_entries())
                })
                .await;

            cx.update(|cx| {
                workspace_handle.update(cx, |workspace, cx| {
                    if workspace.skill_picker_cache_key.as_deref() != Some(cache_key.as_str()) {
                        return;
                    }
                    workspace.skill_picker_loading = false;
                    match result {
                        Ok(entries) => {
                            workspace
                                .skill_picker
                                .set_catalog(entries.into_iter().map(Into::into).collect());
                        }
                        Err(error) => {
                            workspace.skill_picker_error = Some(SharedString::from(format!(
                                "Could not load skills: {error}"
                            )));
                        }
                    }
                    workspace.sync_skill_picker(cx);
                    cx.notify();
                });
            });
        })
        .detach();
        self.sync_skill_picker(cx);
        cx.notify();
    }

    fn sync_skill_picker(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        let cursor = self.input.read(cx).cursor_offset();
        self.skill_picker.update(&text, cursor);
    }

    fn handle_skill_picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sync_skill_picker(cx);
        if !self.skill_picker.is_open() {
            return false;
        }

        match event.keystroke.key.as_str() {
            "escape" => {
                self.dismiss_skill_picker(cx);
                true
            }
            "up" if self.skill_picker.filtered_len() > 0 => {
                self.skill_picker.move_active(-1);
                true
            }
            "down" if self.skill_picker.filtered_len() > 0 => {
                self.skill_picker.move_active(1);
                true
            }
            "enter" | "return" if self.skill_picker.selected_name().is_some() => {
                self.accept_skill_picker(cx)
            }
            _ => false,
        }
    }

    fn dismiss_skill_picker(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        self.skill_picker.dismiss(&text);
        cx.notify();
    }

    fn accept_skill_picker(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((token, name)) = self.skill_picker.accept() else {
            return false;
        };
        let replacement = entry_replacement(&name);
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &replacement, cx);
        });
        cx.notify();
        true
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
            self.activate_configuration_for_selection(cx);
            self.refresh_changes(cx);
            self.status = SharedString::from("Session loaded.");
            self.refresh();
            self.sync_rename_input(cx);
        } else {
            self.status = SharedString::from("Session no longer exists.");
        }
        cx.notify();
    }

    fn activate_configuration_for_selection(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.selected.as_ref().map(|log| log.view()) else {
            return;
        };
        let space_id = Self::resolve_space_id(&self.spaces_config, view.space_id.as_deref());
        let harness_id = Self::resolve_harness_id(&self.harness_setups, view.harness_id.as_deref());
        if let Some(space) = self.spaces_config.get(&space_id) {
            self.workspace_root = space.root().to_path_buf();
            self.selected_harness_id = harness_id;
            let harness = self.selected_harness();
            let (tools, skill_tool) =
                Self::build_tools_with_skills(&self.home, &self.workspace_root, &harness);
            self.tools = tools;
            self.skill_tool = skill_tool;
            self.load_skill_picker(cx);
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
        let (tools, skill_tool) =
            Self::build_tools_with_skills(&self.home, &self.workspace_root, &harness);
        self.tools = tools;
        self.skill_tool = skill_tool;
        self.load_skill_picker(cx);
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

    fn close_spaces_menu(&mut self, cx: &mut Context<Self>) {
        if self.spaces_menu_open {
            self.spaces_menu_open = false;
            self.spaces_menu_active = None;
            cx.notify();
        }
    }

    fn activate_spaces_menu_row(
        &mut self,
        row: SpacesMenuRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match row {
            SpacesMenuRow::All => {
                self.sidebar_space_filter = None;
                self.close_spaces_menu(cx);
            }
            SpacesMenuRow::Space(space_id) => {
                self.sidebar_space_filter = Some(space_id.clone());
                self.select_space(&space_id, cx);
                self.close_spaces_menu(cx);
            }
            SpacesMenuRow::AddSpace => {
                self.close_spaces_menu(cx);
                self.open_add_space(window, cx);
            }
        }
    }

    fn open_add_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_space_open = true;
        self.add_space_error = None;
        self.project_name_input
            .update(cx, |input, cx| input.clear(cx));
        let home = self.home.to_string_lossy().to_string();
        self.project_path_input
            .update(cx, |input, cx| input.set_text(home, cx));
        let focus_handle = self.project_name_input.read(cx).focus_handle(cx).clone();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    fn close_add_space(&mut self, cx: &mut Context<Self>) {
        if self.add_space_open {
            self.add_space_open = false;
            self.add_space_error = None;
            cx.notify();
        }
    }

    fn submit_add_space(&mut self, cx: &mut Context<Self>) {
        let name = self.project_name_input.read(cx).text();
        let root = self.project_path_input.read(cx).text();
        let config_path = self.home.join(".dsh-rs").join("spaces.json");
        match Self::register_project_space(
            &mut self.spaces_config,
            &config_path,
            name.trim(),
            root.trim(),
        ) {
            Ok(space_id) => {
                self.close_add_space(cx);
                if self.busy {
                    self.status = SharedString::from(
                        "Project registered. Wait for the current turn to activate it.",
                    );
                } else {
                    self.sidebar_space_filter = Some(space_id.clone());
                    self.select_space(&space_id, cx);
                }
            }
            Err(error) => {
                self.add_space_error = Some(error.clone());
                self.status = SharedString::from(error);
                cx.notify();
            }
        }
    }

    fn spaces_menu_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.spaces_menu_open {
            return;
        }
        let key = event.keystroke.key.as_str();
        let query = self.spaces_input.read(cx).text();
        let count = Self::spaces_menu_rows(&self.spaces_config, &query).len();
        match key {
            "escape" => self.close_spaces_menu(cx),
            "up" => {
                self.spaces_menu_active =
                    Self::spaces_menu_step(self.spaces_menu_active, count, -1);
                cx.notify();
            }
            "down" => {
                self.spaces_menu_active = Self::spaces_menu_step(self.spaces_menu_active, count, 1);
                cx.notify();
            }
            "enter" | "return" => {
                let active = self.spaces_menu_active.unwrap_or(0);
                let row = Self::spaces_menu_rows(&self.spaces_config, &query)
                    .get(active)
                    .cloned();
                if let Some(row) = row {
                    self.activate_spaces_menu_row(row, window, cx);
                }
            }
            _ => {}
        }
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
        let (tools, skill_tool) =
            Self::build_tools_with_skills(&self.home, &self.workspace_root, &harness);
        self.tools = tools;
        self.skill_tool = skill_tool;
        self.load_skill_picker(cx);
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

    fn render_add_space(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let error = self.add_space_error.clone();

        let card = div()
            .id("add-space-card")
            .w(px(520.0))
            .p(px(18.0))
            .rounded(px(14.0))
            .border_1()
            .border_color(theme.border.opacity(0.12))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .bg(theme.glass_overlay())
            .shadow_lg()
            .text_color(theme.text)
            .on_mouse_down_out(cx.listener(|workspace, _, _, cx| {
                workspace.close_add_space(cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        icon(icons::FOLDER)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("New project"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_muted.opacity(0.7))
                            .child("Project name"),
                    )
                    .child(self.project_name_input.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_muted.opacity(0.7))
                            .child("Project folder"),
                    )
                    .child(self.project_path_input.clone()),
            )
            .when_some(error, |element, message| {
                element.child(
                    div()
                        .id("add-space-error")
                        .text_size(px(11.0))
                        .text_color(theme.danger_muted)
                        .child(SharedString::from(message)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("add-space-cancel")
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .px(px(12.0))
                            .py(px(7.0))
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.glass_hover()).text_color(theme.text))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.close_add_space(cx);
                                }),
                            )
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .id("add-space-submit")
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .px(px(14.0))
                            .py(px(7.0))
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(theme.accent)
                            .text_color(theme.on_accent)
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.submit_add_space(cx);
                                }),
                            )
                            .child("Add project"),
                    ),
            );

        div()
            .id("add-space-overlay")
            .absolute()
            .size_full()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(118.0))
            .bg(theme.scrim())
            .child(frost::frosted(14.0, 32.0, card))
            .into_any_element()
    }

    fn render_todo_call(arguments: &serde_json::Value, theme: &Theme) -> AnyElement {
        let todos = Self::todo_items_from_arguments(arguments).unwrap_or_default();
        let summary = Self::todo_call_summary(arguments).unwrap_or_else(|| "0/0 done".to_string());

        div()
            .id("todo-tool-card")
            .max_w(px(820.0))
            .px_3()
            .py_2()
            .rounded_md()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        icon(icons::CHECKLIST)
                            .size(px(13.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_faint)
                            .child("Todo"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(summary)),
                    ),
            )
            .children(todos.iter().map(|todo| {
                let (marker, color) = match todo.status {
                    TodoStatus::Completed => ("[x]", theme.text_muted),
                    TodoStatus::InProgress => ("[ ]", theme.text),
                    TodoStatus::Pending => ("[ ]", theme.text_muted.opacity(0.8)),
                };
                div()
                    .text_size(px(12.0))
                    .font_family(theme.font_mono.clone())
                    .text_color(color)
                    .child(format!("{marker} {}", todo.content))
            }))
            .into_any_element()
    }

    fn render_skill_call(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        output: Option<(&str, bool)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model = Self::skill_row_model(call_id, arguments, output);
        let expandable = model.output.is_some();
        let expanded = expandable && self.expanded_skill_calls.contains(call_id);
        let output = model.output.clone();
        let summary = model.error_summary.clone().unwrap_or(model.name.clone());
        let summary_color = match model.state {
            SkillRowState::Error => theme.danger_muted,
            SkillRowState::Stopped => theme.warning_muted,
            _ => theme.text_faint,
        };
        let leading_color = match model.state {
            SkillRowState::Error | SkillRowState::Stopped => summary_color,
            _ => theme.text_faint,
        };
        let leading = match model.state {
            SkillRowState::Error | SkillRowState::Stopped => div()
                .size(px(6.0))
                .rounded_full()
                .bg(leading_color)
                .into_any_element(),
            SkillRowState::Running | SkillRowState::Ok => icon(icons::SKILL)
                .size(px(14.0))
                .text_color(leading_color)
                .into_any_element(),
        };
        let toggle_call_id = call_id.to_string();

        let row = div()
            .id(SharedString::from(format!("skill-row-{call_id}")))
            .min_h(px(24.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .when(expandable, |element| {
                element.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |workspace, _, _, cx| {
                        if workspace.expanded_skill_calls.contains(&toggle_call_id) {
                            workspace.expanded_skill_calls.remove(&toggle_call_id);
                        } else {
                            workspace
                                .expanded_skill_calls
                                .insert(toggle_call_id.clone());
                        }
                        cx.notify();
                    }),
                )
            })
            .child(
                div()
                    .size(px(16.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .child(leading),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(14.0))
                    .text_color(theme.text_muted)
                    .child("Skill"),
            )
            .child(
                div()
                    .flex_none()
                    .size(px(2.0))
                    .rounded_full()
                    .bg(theme.text_faint.opacity(0.6)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(14.0))
                    .text_color(summary_color)
                    .child(SharedString::from(summary)),
            )
            .when(expandable, |element| {
                element.child(
                    icon(icons::ALT_ARROW_DOWN)
                        .size(px(14.0))
                        .text_color(theme.text_faint),
                )
            });
        let row = if model.state == SkillRowState::Running {
            let sweep_bg = theme.bg;
            row.with_animation(
                SharedString::from(format!("skill-row-sweep-{call_id}")),
                motion::SKILL_SWEEP.repeating(),
                move |element, delta| {
                    let transparent = linear_color_stop(sweep_bg.opacity(0.0), 0.0);
                    let tint = linear_color_stop(sweep_bg.opacity(0.6), 1.0);
                    element.relative().overflow_hidden().child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(motion::skill_sweep_left(delta)))
                            .w(px(300.0))
                            .flex()
                            .flex_row()
                            .child(div().h_full().flex_1().bg(linear_gradient(
                                90.0,
                                transparent,
                                tint,
                            )))
                            .child(div().h_full().flex_1().bg(linear_gradient(
                                90.0,
                                tint,
                                transparent,
                            ))),
                    )
                },
            )
            .into_any_element()
        } else {
            row.into_any_element()
        };

        div()
            .id(SharedString::from(format!("skill-call-{call_id}")))
            .w_full()
            .max_w(px(820.0))
            .flex()
            .flex_col()
            .child(row)
            .when(expanded, |element| {
                let output = output.unwrap_or_default();
                element.child(
                    div()
                        .id(SharedString::from(format!("skill-instructions-{call_id}")))
                        .ml(px(4.0))
                        .mt(px(4.0))
                        .max_h(px(260.0))
                        .overflow_hidden()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface_raised)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .px(px(12.0))
                                .py(px(8.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_faint)
                                .child("INSTRUCTIONS"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "skill-instructions-body-{call_id}"
                                )))
                                .max_h(px(216.0))
                                .overflow_scroll()
                                .px(px(12.0))
                                .py(px(10.0))
                                .font_family(theme.font_mono.clone())
                                .text_size(px(12.0))
                                .text_color(if model.state == SkillRowState::Error {
                                    theme.danger_muted
                                } else {
                                    theme.text_muted
                                })
                                .child(SharedString::from(output)),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_skill_picker(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.skill_picker.is_open() {
            return None;
        }

        let has_matches = self.skill_picker.filtered_entries().next().is_some();
        let mut card = div()
            .id("skill-picker-card")
            .w(px(380.0))
            .max_h(px(280.0))
            .overflow_hidden()
            .border_1()
            .border_color(theme::hairline(0.10))
            .rounded(px(12.0))
            .shadow_lg()
            .p(px(4.0))
            .text_size(px(13.0))
            .text_color(theme.text)
            .bg(theme.glass_overlay())
            .on_mouse_down_out(cx.listener(|workspace, _, _, cx| {
                workspace.dismiss_skill_picker(cx);
            }));

        if self.skill_picker_loading && !has_matches {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("Loading skills..."),
            );
        } else if let Some(error) = self.skill_picker_error.clone() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger)
                    .child(error),
            );
        } else if !has_matches {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("No matching skills"),
            );
        } else {
            for (row, entry) in self.skill_picker.filtered_entries().enumerate() {
                let selected = self.skill_picker.is_selected(row);
                let fade_key = SharedString::from(format!("skill-picker-row-{row}"));
                let name: SharedString = format!("/{}", entry.name).into();
                let description: SharedString = entry.menu_description().into();
                let row_background = if selected {
                    theme::glass_selected_bg()
                } else {
                    motion::hover_blend(&fade_key, theme.wash(0.0), theme::glass_selected_bg())
                };

                card = card.child(
                    div()
                        .id(("skill-picker-row", row))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .text_size(px(13.0))
                        .cursor_pointer()
                        .bg(row_background)
                        .when(!selected, |row| {
                            row.on_hover(motion::hover_listener(fade_key.clone()))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.skill_picker.select(row);
                                workspace.accept_skill_picker(cx);
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    icon(icons::SKILL)
                                        .size(px(14.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .truncate()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_muted)
                                        .child(description),
                                ),
                        ),
                );
            }
        }

        let floating_card = motion::menu_in(
            "skill-picker",
            div()
                .occlude()
                .pb(px(6.0))
                .child(frost::frosted(12.0, 24.0, card)),
        );
        let anchored = div()
            .absolute()
            .top_0()
            .left_0()
            .size_0()
            .child(
                gpui::deferred(
                    gpui::anchored()
                        .anchor(gpui::Anchor::BottomLeft)
                        .snap_to_window_with_margin(px(8.0))
                        .child(floating_card),
                )
                .priority(1),
            )
            .into_any_element();
        Some(anchored)
    }

    fn render_todo_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let todos = self
            .selected_view
            .as_ref()
            .and_then(|view| view.todos.clone())?;
        if todos.is_empty() {
            return None;
        }

        let theme = Theme::of(cx).clone();
        let collapsed = self.todo_panel_collapsed;
        let progress = Self::todo_panel_progress(&todos);
        let card = div()
            .id("todo-panel-card")
            .w_full()
            .max_w(px(820.0))
            .overflow_hidden()
            .child(
                div()
                    .id("todo-panel-header")
                    .min_h(px(36.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, _, cx| {
                            workspace.todo_panel_collapsed = !workspace.todo_panel_collapsed;
                            cx.notify();
                        }),
                    )
                    .child(
                        icon(icons::CHECKLIST)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(13.0))
                            .line_height(px(24.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("To-dos"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(progress)),
                    )
                    .child(
                        icon(icons::ALT_ARROW_DOWN)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    ),
            )
            .when(!collapsed, |element| {
                element.child(
                    div()
                        .id("todo-panel-list")
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .max_h(px(180.0))
                        .overflow_y_scroll()
                        .px(px(12.0))
                        .pb(px(10.0))
                        .children(todos.iter().map(|todo| {
                            let (glyph, color) = match todo.status {
                                TodoStatus::Completed => (
                                    icon(icons::CHECK)
                                        .size(px(14.0))
                                        .text_color(theme.success)
                                        .into_any_element(),
                                    theme.text_muted,
                                ),
                                TodoStatus::InProgress => (
                                    div()
                                        .size(px(14.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme.accent)
                                        .into_any_element(),
                                    theme.text,
                                ),
                                TodoStatus::Pending => (
                                    div()
                                        .size(px(14.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme.text_faint.opacity(0.7))
                                        .into_any_element(),
                                    theme.text_muted,
                                ),
                            };

                            div()
                                .id(SharedString::from(format!(
                                    "todo-panel-item-{}",
                                    todo.content
                                )))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(10.0))
                                .min_w_0()
                                .text_size(px(13.0))
                                .line_height(px(20.0))
                                .text_color(color)
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .child(glyph),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(todo.content.clone()),
                                )
                        })),
                )
            });

        Some(
            div()
                .id("todo-panel")
                .w_full()
                .flex()
                .flex_none()
                .justify_center()
                .px_6()
                .pb_2()
                .child(frost::frosted(12.0, 24.0, card))
                .into_any_element(),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let visible_summaries: Vec<SessionSummary> = match &self.search_matches {
            Some(matches) => self
                .summaries
                .iter()
                .filter(|summary| matches.contains(&summary.id))
                .cloned()
                .collect(),
            None => self.summaries.clone(),
        };
        let rows = Self::sidebar_session_rows(
            &visible_summaries,
            &self.spaces_config,
            &self.harness_setups,
            self.sidebar_space_filter.as_deref(),
            chrono::Utc::now(),
        );
        let selected_space = match self.sidebar_space_filter.as_deref() {
            Some(space_id) => self
                .spaces_config
                .get(space_id)
                .map(|space| space.name().to_string())
                .unwrap_or_else(|| "All projects".to_string()),
            None => "All projects".to_string(),
        };
        let spaces_query = self.spaces_input.read(cx).text();
        let spaces_menu_rows = Self::spaces_menu_rows(&self.spaces_config, &spaces_query);
        let spaces_menu_active = self
            .spaces_menu_active
            .filter(|index| *index < spaces_menu_rows.len());
        let menu_row_elements = spaces_menu_rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let (label, leading) = match &row {
                    SpacesMenuRow::All => ("All projects".to_string(), icons::FOLDER),
                    SpacesMenuRow::Space(space_id) => (
                        self.spaces_config
                            .get(space_id)
                            .map(|space| space.name().to_string())
                            .unwrap_or_else(|| "?".to_string()),
                        icons::FOLDER,
                    ),
                    SpacesMenuRow::AddSpace => ("New project...".to_string(), icons::PLUS),
                };
                let selected = match &row {
                    SpacesMenuRow::All => self.sidebar_space_filter.is_none(),
                    SpacesMenuRow::Space(space_id) => {
                        self.sidebar_space_filter.as_deref() == Some(space_id.as_str())
                    }
                    SpacesMenuRow::AddSpace => false,
                };
                let active = spaces_menu_active == Some(index);
                let rest_bg = if selected {
                    theme::glass_selected_bg()
                } else {
                    theme.wash(0.0)
                };
                let hover_bg = if selected {
                    theme::glass_selected_bg()
                } else {
                    theme.glass_hover()
                };

                div()
                    .id(SharedString::from(format!("spaces-menu-row-{index}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(px(12.5))
                    .text_color(if selected || active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .bg(if active { hover_bg } else { rest_bg })
                    .hover(|style| style.bg(theme.glass_hover()))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _, window, cx| {
                            workspace.activate_spaces_menu_row(row.clone(), window, cx)
                        }),
                    )
                    .child(
                        icon(leading)
                            .size(px(15.0))
                            .flex_none()
                            .text_color(theme.text_muted.opacity(0.8)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(label))
            })
            .collect::<Vec<_>>();

        let spaces_menu_popover = self.spaces_menu_open.then(|| {
            let card = div()
                .id("spaces-menu-card")
                .w(px(248.0))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .p(px(8.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(self.spaces_input.clone()),
                )
                .child(
                    div()
                        .id("spaces-menu-list")
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .max_h(px(224.0))
                        .overflow_y_scroll()
                        .p(px(6.0))
                        .children(menu_row_elements),
                );

            div()
                .id("spaces-menu-popover")
                .absolute()
                .left_0()
                .top(px(33.0))
                .w(px(248.0))
                .on_key_down(
                    cx.listener(|workspace, event: &gpui::KeyDownEvent, window, cx| {
                        workspace.spaces_menu_key(event, window, cx);
                    }),
                )
                .child(frost::frosted(Theme::PANEL_RADIUS, 24.0, card))
        });

        let session_rows = rows.into_iter().map(|row| {
            let selected = self.selected.as_ref().is_some_and(|log| log.id() == row.id);
            let fade_key = format!("sidebar-session-{}", row.id);
            let status = if row.working {
                theme.busy.opacity(0.55)
            } else {
                theme.text_muted.opacity(0.5)
            };
            let corner = if row.working {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().size(px(6.0)).rounded_full().bg(status))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(status)
                            .child("Working"),
                    )
            } else {
                div()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(status)
                    .child(row.time_ago.clone())
            };
            let select_id = row.id;
            let rest_bg = if selected {
                theme::glass_selected_bg()
            } else {
                theme.wash(0.0)
            };
            let hover_bg = if selected {
                theme::glass_selected_bg()
            } else {
                theme.glass_hover()
            };

            div()
                .id(SharedString::from(format!("session-{}", row.id)))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(8.0))
                .px(px(Theme::SPACE_SM))
                .py(px(6.0))
                .bg(motion::hover_blend(&fade_key, rest_bg, hover_bg))
                .on_hover(motion::hover_listener(fade_key))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |workspace, _, _, cx| workspace.select(select_id, cx)),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.0))
                                .line_height(px(14.0))
                                .text_color(theme.text_muted.opacity(0.5))
                                .child(row.space_name.clone()),
                        )
                        .child(corner),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(px(13.0))
                        .line_height(px(17.0))
                        .text_color(theme.text)
                        .child(row.title.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_muted.opacity(0.5))
                        .child(row.harness_name.clone()),
                )
                .into_any_element()
        });

        let list = div().relative().flex_1().min_h_0().child(
            div()
                .id("sidebar-lists")
                .size_full()
                .overflow_y_scroll()
                .px(px(Theme::SPACE_SM))
                .flex()
                .flex_col()
                .pt(px(4.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .pb(px(Theme::SPACE_SM))
                        .children(session_rows),
                ),
        );

        div()
            .id("sessions-sidebar")
            .w(px(SIDEBAR_DEFAULT))
            .h_full()
            .flex()
            .flex_col()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .bg(theme.wash(0.05))
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(Theme::SPACE_SM))
                    .pt(px(8.0))
                    .pb(px(4.0))
                    .child(
                        div()
                            .relative()
                            .id("spaces-filter")
                            .flex_1()
                            .min_w_0()
                            .h(px(29.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(Theme::SPACE_SM))
                            .rounded(px(8.0))
                            .px(px(Theme::SPACE_SM))
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(motion::hover_blend(
                                "spaces-filter",
                                theme.text.opacity(0.8),
                                theme.text,
                            ))
                            .bg(motion::hover_blend(
                                "spaces-filter",
                                theme.glass_hover().opacity(0.0),
                                theme.glass_hover(),
                            ))
                            .on_hover(motion::hover_listener("spaces-filter"))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    if workspace.spaces_menu_open {
                                        workspace.close_spaces_menu(cx);
                                    } else {
                                        workspace.spaces_menu_open = true;
                                        workspace.spaces_menu_active = None;
                                        let focus_handle = workspace
                                            .spaces_input
                                            .read(cx)
                                            .focus_handle(cx)
                                            .clone();
                                        window.focus(&focus_handle, cx);
                                    }
                                }),
                            )
                            .child(
                                icon(icons::FOLDER)
                                    .size(px(16.0))
                                    .flex_none()
                                    .text_color(theme.text_muted),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(selected_space))
                            .child(
                                icon(icons::ALT_ARROW_DOWN)
                                    .size(px(14.0))
                                    .flex_none()
                                    .text_color(theme.text_muted.opacity(0.6)),
                            )
                            .when_some(spaces_menu_popover, |element, popover| {
                                element.child(popover)
                            }),
                    )
                    .child(
                        div()
                            .id("sidebar-new-session")
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .bg(motion::hover_blend(
                                "sidebar-new-session",
                                theme.wash(0.0),
                                theme.wash(0.14),
                            ))
                            .on_hover(motion::hover_listener("sidebar-new-session"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| workspace.create_session(cx)),
                            )
                            .child(
                                icon(icons::PLUS)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted.opacity(0.7)),
                            ),
                    ),
            )
            .when(self.search_matches.is_some(), |element| {
                element.child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_SM))
                        .pb(px(4.0))
                        .child(self.search_input.clone()),
                )
            })
            .child(edge_faded(SIDEBAR_GLASS_FADE_BAND, true, true, list))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(Theme::SPACE_SM))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme.wash(0.08))
                            .child(
                                icon(icons::DEEPSEEK_MARK)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.0))
                                    .text_color(theme.text)
                                    .child("Local device"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(10.0))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(self.credential_label()),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_transcript(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let transcript = self
            .selected_view
            .as_ref()
            .map(|view| view.transcript.as_slice())
            .unwrap_or(&[]);
        let skill_call_ids: HashSet<&str> = transcript
            .iter()
            .filter_map(|entry| {
                if let TranscriptEntry::ToolCall { id, name, .. } = entry {
                    (name == "skill").then_some(id.as_str())
                } else {
                    None
                }
            })
            .collect();
        let skill_results: HashMap<&str, (&str, bool)> = transcript
            .iter()
            .filter_map(|entry| {
                if let TranscriptEntry::ToolResult {
                    call_id,
                    output,
                    ok,
                } = entry
                {
                    Some((call_id.as_str(), (output.as_str(), *ok)))
                } else {
                    None
                }
            })
            .collect();

        let list = div()
            .id("transcript")
            .flex_1()
            .overflow_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .children(
                transcript
                    .iter()
                    .filter(|entry| {
                        !matches!(
                            entry,
                            TranscriptEntry::ToolResult { call_id, .. }
                                if skill_call_ids.contains(call_id.as_str())
                        )
                    })
                    .map(|entry| {
                        if let TranscriptEntry::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } = entry
                        {
                            if name == "todo_write" {
                                return Self::render_todo_call(arguments, &theme);
                            }
                            if name == "skill" {
                                return self.render_skill_call(
                                    id,
                                    arguments,
                                    skill_results.get(id.as_str()).copied(),
                                    &theme,
                                    cx,
                                );
                            }
                        }
                        let (role, text, color) = match entry {
                            TranscriptEntry::User { content, .. } => ("You", content, theme.text),
                            TranscriptEntry::Assistant { content, .. } => {
                                ("Assistant", content, theme.text)
                            }
                            TranscriptEntry::ToolCall { name, .. } => {
                                ("Tool call", name, theme.warning)
                            }
                            TranscriptEntry::ToolResult { output, .. } => {
                                ("Tool result", output, theme.success)
                            }
                            TranscriptEntry::System { message } => {
                                ("System", message, theme.danger)
                            }
                        };
                        div()
                            .max_w(px(820.))
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(theme.surface_raised)
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_faint)
                                    .child(role),
                            )
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(color)
                                    .child(text.clone()),
                            )
                            .into_any_element()
                    }),
            );

        edge_faded(Theme::TRANSCRIPT_FADE_BAND, true, true, list)
            .inset_top(Theme::TITLEBAR_HEIGHT)
            .band_top(Theme::TRANSCRIPT_FADE_BAND)
            .band_bottom(Theme::TRANSCRIPT_FADE_BAND)
    }

    fn render_terminal_dock(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = Theme::of(cx).clone();

        div()
            .id("terminal-dock")
            .h(px(TERMINAL_DEFAULT_HEIGHT))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .bg(terminal_panel_bg(&theme))
            .child(
                div()
                    .id("terminal-tab-bar")
                    .h(px(TERMINAL_TAB_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .pl(px(8.0))
                    .pr(px(6.0))
                    .border_b_1()
                    .border_color(theme::hairline(0.07))
                    .child(
                        div()
                            .id("terminal-tab-1")
                            .w(px(TERMINAL_TAB_WIDTH))
                            .h(px(28.0))
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .pl(px(8.0))
                            .pr(px(4.0))
                            .rounded(px(8.0))
                            .bg(motion::hover_blend(
                                "terminal-tab-1",
                                theme::ink(0.08),
                                theme.element_hover,
                            ))
                            .on_hover(motion::hover_listener("terminal-tab-1"))
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(
                                icon(icons::TERMINAL)
                                    .size(px(16.0))
                                    .text_color(theme.text.opacity(0.8)),
                            )
                            .child(div().flex_1().min_w_0().truncate().child("Terminal 1"))
                            .child(
                                div()
                                    .id("terminal-tab-close")
                                    .size(px(20.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::ink(0.09)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, window, cx| {
                                            workspace.toggle_terminal(&ToggleTerminal, window, cx)
                                        }),
                                    )
                                    .child(
                                        icon(icons::CLOSE)
                                            .size(px(12.0))
                                            .text_color(theme.text_muted.opacity(0.8)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("terminal-new-tab")
                            .size(px(28.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .bg(motion::hover_blend(
                                "terminal-new-tab",
                                gpui::transparent_black(),
                                theme::ink(0.05),
                            ))
                            .on_hover(motion::hover_listener("terminal-new-tab"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    workspace.focus_terminal(&FocusTerminal, window, cx)
                                }),
                            )
                            .child(
                                icon(icons::PLUS)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted.opacity(0.6)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("terminal-collapse")
                            .size(px(28.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .bg(motion::hover_blend(
                                "terminal-collapse",
                                gpui::transparent_black(),
                                theme::ink(0.05),
                            ))
                            .on_hover(motion::hover_listener("terminal-collapse"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, window, cx| {
                                    workspace.toggle_terminal(&ToggleTerminal, window, cx)
                                }),
                            )
                            .child(
                                icon(icons::ALT_ARROW_DOWN)
                                    .size(px(13.0))
                                    .text_color(theme.text_muted.opacity(0.55)),
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
                                theme.surface_raised
                            } else {
                                theme.accent
                            })
                            .text_size(px(11.))
                            .text_color(if self.terminal_running {
                                theme.text_faint
                            } else {
                                theme.bg
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
                            .text_color(theme.text_faint)
                            .child("Running")
                    } else {
                        div()
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
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
                                    .text_color(if entry.ok {
                                        theme.text_muted
                                    } else {
                                        theme.danger
                                    })
                                    .child(output),
                            )
                    })),
            )
    }

    fn render_changes_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = Theme::of(cx).clone();
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
                        .text_color(theme.text_faint)
                        .child("CHANGES"),
                ),
            );

        if self.changes_scanning {
            return section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    .child("Scanning repository"),
            );
        }

        match &self.changes {
            WorkspaceChangesState::NotRepository => section.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
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
                                    .text_color(theme.text_faint)
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
                                        theme.surface_raised
                                    } else {
                                        theme.bg
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
                                                        theme.text_faint
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
                                                    .text_color(theme.text_muted)
                                                    .child(counts),
                                            ),
                                    )
                            }),
                    )
                    .when(hidden_count > 0, |element| {
                        element.child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_faint)
                                .child(format!("+{hidden_count} more files")),
                        )
                    })
            }
        }
    }

    fn render_selected_diff_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = Theme::of(cx).clone();
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
                            .text_color(if *staged {
                                theme.warning
                            } else {
                                theme.text_faint
                            })
                            .child(if *staged { "staged" } else { "worktree" }),
                    )
                    .child(
                        div()
                            .id("close-diff")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_size(px(10.))
                            .text_color(theme.text_muted)
                            .hover(|style| style.bg(theme.surface_raised).cursor_pointer())
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
                    .text_color(theme.text_faint)
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
                    DiffLineKind::Meta => theme.text_faint,
                    DiffLineKind::Hunk => theme.accent,
                    DiffLineKind::Context => theme.text_muted,
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
                        .text_color(theme.text_faint)
                        .child("Diff truncated at 500 lines"),
                )
            })
    }

    fn render_context_pane(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = Theme::of(cx).clone();
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
            .w(px(RIGHT_PANE_DEFAULT))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .bg(theme.bg)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(Theme::HEADER_HEIGHT))
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
                            .text_color(theme.text_muted)
                            .hover(|style| style.bg(theme.surface_raised).cursor_pointer())
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
                            .text_color(theme.text_muted)
                            .hover(|style| style.bg(theme.surface_raised).cursor_pointer())
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
                            .flex_row()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("fork-session")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .rounded(px(8.0))
                                    .px(px(10.0))
                                    .py(px(6.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.bg(theme.glass_hover()).text_color(theme.text)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, _, cx| {
                                            workspace.fork_selected(cx)
                                        }),
                                    )
                                    .child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Fork"),
                            )
                            .child(
                                div()
                                    .id("export-session")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .rounded(px(8.0))
                                    .px(px(10.0))
                                    .py(px(6.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.bg(theme.glass_hover()).text_color(theme.text)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|workspace, _, _, cx| {
                                            workspace.export_selected(cx)
                                        }),
                                    )
                                    .child(
                                        icon(icons::DOCUMENT)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Export"),
                            ),
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
                                            .text_color(theme.text_faint)
                                            .child("Model"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.text_muted)
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
                                            .text_color(theme.text_faint)
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
                                                    .text_color(theme.text_muted)
                                                    .child(harness_name),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(theme.text_faint)
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
                                            .text_color(theme.text_faint)
                                            .child("Events"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.text_muted)
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
                                            .text_color(theme.text_faint)
                                            .child("Turn"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(if turn_state == "active" {
                                                theme.accent
                                            } else {
                                                theme.text_muted
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
                                            .text_color(theme.text_faint)
                                            .child("Prompt"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme.text_muted)
                                            .child(prompt),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_faint)
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
                                            .text_color(theme.text_muted)
                                            .child(value),
                                    )
                            }))
                            .when(activity_empty, |el| {
                                el.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.text_faint)
                                        .child("No tool activity"),
                                )
                            }),
                    ),
            )
    }

    fn render_approval(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
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

        let card = div()
            .id("approval")
            .mx_6()
            .my_3()
            .px_4()
            .py_3()
            .rounded_md()
            .bg(theme.surface_raised)
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
                            .text_color(theme.text_muted)
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
                            .text_color(theme.bg)
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
                            .text_color(theme.bg)
                            .hover(|style| style.cursor_pointer())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.resolve_approval(false, cx)
                                }),
                            )
                            .child("Deny"),
                    ),
            );

        frost::frosted(Theme::PANEL_RADIUS, 24.0, card)
    }
}

impl Workspace {
    fn render_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let section = match self.route {
            Route::Settings(section) => section,
            Route::Chat => SettingsSection::Providers,
        };
        let outlet = match section {
            SettingsSection::Providers => self
                .providers_page
                .clone()
                .map(|page| page.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            SettingsSection::Appearance => self
                .appearance_page
                .clone()
                .map(|page| page.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
        };

        div()
            .key_context("Workspace")
            .id("settings-root")
            .size_full()
            .relative()
            .font_family(theme.font_sans.clone())
            .flex()
            .bg(theme.glass())
            .text_color(theme.text)
            .on_action(cx.listener(|workspace, _: &SaveCredential, window, cx| {
                workspace.save_credential(&SaveCredential, window, cx)
            }))
            .on_action(cx.listener(|workspace, _: &CloseSettings, window, cx| {
                workspace.close_settings(&CloseSettings, window, cx)
            }))
            .child(
                div()
                    .w(px(SIDEBAR_DEFAULT))
                    .h_full()
                    .flex()
                    .flex_col()
                    .pt(px(Theme::TITLEBAR_HEIGHT))
                    .child(
                        div()
                            .flex_1()
                            .px(px(Theme::SPACE_SM))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px(px(Theme::SPACE_SM))
                                    .pt(px(12.0))
                                    .pb(px(4.0))
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child("Settings"),
                            )
                            .child(div().flex().flex_col().gap(px(2.0)).children(
                                SettingsSection::ALL.into_iter().map(|item| {
                                    let selected = item == section;
                                    div()
                                                .id(SharedString::from(format!(
                                                    "settings-nav-{}",
                                                    item.label()
                                                )))
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap(px(8.0))
                                                .rounded(px(8.0))
                                                .px(px(Theme::SPACE_SM))
                                                .py(px(6.0))
                                                .text_size(px(13.0))
                                                .when(selected, |element| {
                                                    element
                                                        .bg(theme::glass_selected_bg())
                                                        .font_weight(FontWeight::MEDIUM)
                                                })
                                                .text_color(if selected {
                                                    theme.text
                                                } else {
                                                    theme.text_muted
                                                })
                                                .cursor_pointer()
                                                .hover(|style| {
                                                    style
                                                        .bg(theme.glass_hover())
                                                        .text_color(theme.text)
                                                })
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        move |workspace,
                                                         _: &gpui::MouseDownEvent,
                                                         _,
                                                         cx| {
                                                            workspace.open_settings_section(
                                                                item, cx,
                                                            )
                                                        },
                                                    ),
                                                )
                                                .child(
                                                    icon(item.icon())
                                                        .size(px(16.0))
                                                        .text_color(theme.text_muted),
                                                )
                                                .child(item.label())
                                }),
                            )),
                    )
                    .child(
                        div().px(px(Theme::SPACE_SM)).pb(px(12.0)).child(
                            div()
                                .id("settings-back")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .rounded(px(8.0))
                                .px(px(Theme::SPACE_SM))
                                .py(px(6.0))
                                .text_size(px(13.0))
                                .text_color(theme.text_muted)
                                .cursor_pointer()
                                .hover(|style| style.bg(theme.glass_hover()).text_color(theme.text))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        |workspace, _: &gpui::MouseDownEvent, window, cx| {
                                            workspace.close_settings(&CloseSettings, window, cx)
                                        },
                                    ),
                                )
                                .child(
                                    icon(icons::ALT_ARROW_LEFT)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child("Back"),
                        ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pt(px(Theme::TITLEBAR_HEIGHT))
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(outlet)),
            )
            .into_any_element()
    }

    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let settings_section = match self.route {
            Route::Settings(section) => Some(section),
            Route::Chat => None,
        };
        let settings = settings_section.is_some();
        let title = if let Some(section) = settings_section {
            SharedString::from(section.label())
        } else {
            SharedString::from(
                self.selected_view
                    .as_ref()
                    .map(|view| view.title.clone())
                    .unwrap_or_default(),
            )
        };
        let harness_name = self.selected_harness().name().to_string();
        let target = if settings {
            SharedString::from("")
        } else {
            harness_name.into()
        };
        let cluster_end = cluster_buttons_start(cfg!(target_os = "macos"), false)
            + TITLEBAR_CLUSTER_BUTTONS_WIDTH
            + 10.0;
        let title_left = if self.ui_settings.sidebar_visible {
            SIDEBAR_DEFAULT + Theme::SPACE_LG
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
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(10.0))
            .pt(px(Theme::TITLEBAR_TOP_PAD))
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
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .occlude()
                    .bg(motion::hover_blend(
                        "titlebar-toggle-sidebar",
                        theme.glass_hover().opacity(0.0),
                        theme.glass_hover(),
                    ))
                    .on_hover(motion::hover_listener("titlebar-toggle-sidebar"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, window, cx| {
                            workspace.toggle_sidebar(&ToggleSidebar, window, cx)
                        }),
                    )
                    .child(
                        icon(icons::SIDEBAR_MINIMALISTIC_LEFT)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
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
                            .text_color(theme.text_muted),
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
                            .text_color(theme.text_muted),
                    ),
            )
            .child(div().w(px(Theme::SPACE_LG)).flex_none())
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
                    .when(!settings, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child(target.clone()),
                        )
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("titlebar-open-settings")
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .occlude()
                    .bg(motion::hover_blend(
                        "titlebar-open-settings",
                        theme.glass_hover().opacity(0.0),
                        theme.glass_hover(),
                    ))
                    .on_hover(motion::hover_listener("titlebar-open-settings"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, window, cx| {
                            workspace.open_settings(&OpenSettings, window, cx)
                        }),
                    )
                    .child(
                        icon(icons::SETTINGS_MINIMALISTIC)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .id("titlebar-toggle-context")
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .occlude()
                    .bg(motion::hover_blend(
                        "titlebar-toggle-context",
                        theme.glass_hover().opacity(0.0),
                        theme.glass_hover(),
                    ))
                    .on_hover(motion::hover_listener("titlebar-toggle-context"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|workspace, _, window, cx| {
                            workspace.toggle_context(&ToggleContext, window, cx)
                        }),
                    )
                    .child(
                        icon(icons::SIDEBAR_MINIMALISTIC)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if motion::hover_fades_active() {
            window.request_animation_frame();
        }
        if let Route::Settings(_) = self.route {
            return self.render_settings(cx).into_any_element();
        }
        let theme = Theme::of(cx).clone();
        self.sync_skill_picker(cx);
        let skill_picker_popover = self.render_skill_picker(&theme, cx);
        let send_button = div()
            .id("send")
            .px_4()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(if self.busy {
                theme.surface_raised
            } else {
                theme.accent
            })
            .text_size(px(13.))
            .text_color(if self.busy {
                theme.text_faint
            } else {
                theme.on_accent
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
                cx.listener(|workspace, _, window, cx| workspace.submit(&Submit, window, cx)),
            )
            .child(if self.busy { "..." } else { "Send" });
        let composer = div()
            .relative()
            .px_6()
            .pb_5()
            .pt_3()
            .flex()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .on_key_down(cx.listener(|workspace, event: &gpui::KeyDownEvent, _, cx| {
                if workspace.handle_skill_picker_key(event, cx) {
                    cx.stop_propagation();
                    return;
                }
                workspace.sync_skill_picker(cx);
                cx.notify();
            }))
            .child(div().flex_1().child(self.input.clone()))
            .child(send_button)
            .when_some(skill_picker_popover, |element, popover| {
                element.child(popover)
            });
        let sidebar_visible = self.ui_settings.sidebar_visible;
        let context_visible = self.ui_settings.context_pane_visible;
        let terminal_visible = self.ui_settings.terminal_visible;
        let add_space_open = self.add_space_open;
        let root = div()
            .key_context("Workspace")
            .id("workspace-root")
            .size_full()
            .relative()
            .font_family(theme.font_sans.clone())
            .flex()
            .bg(theme.glass())
            .text_color(theme.text)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(|workspace, _: &OpenSettings, window, cx| {
                workspace.open_settings(&OpenSettings, window, cx)
            }))
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
                    .bg(theme.bg)
                    .child(self.render_transcript(cx))
                    .when(terminal_visible, |el| {
                        el.child(self.render_terminal_dock(cx))
                    })
                    .children(
                        self.pending_approval
                            .as_ref()
                            .map(|_| self.render_approval(cx)),
                    )
                    .children(self.render_todo_panel(cx))
                    .child(composer)
                    .child(
                        div()
                            .px_6()
                            .h(px(Theme::STATUS_STRIP_HEIGHT))
                            .flex()
                            .items_center()
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
                            .child(self.status.clone()),
                    ),
            )
            .when(context_visible, |el| el.child(self.render_context_pane(cx)))
            .child(self.render_titlebar(cx))
            .when(add_space_open, |el| el.child(self.render_add_space(cx)));

        motion::fade_in("phase-app", root).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;
    use harness_core::events::{TodoItem, TodoStatus};
    use harness_core::harness::HarnessSetupsConfig;
    use harness_core::skills::SkillCatalogEntry;
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
    fn build_tools_registers_the_standard_todo_tool() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let setups = HarnessSetupsConfig::default();
        let tools =
            Workspace::build_tools(home.path(), root.path(), setups.get("standard").unwrap());

        assert!(tools.specs().iter().any(|spec| spec.name == "todo_write"));
    }

    #[test]
    fn build_tools_registers_the_standard_skill_loader() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let setups = HarnessSetupsConfig::default();
        let tools =
            Workspace::build_tools(home.path(), root.path(), setups.get("standard").unwrap());

        assert!(tools.specs().iter().any(|spec| spec.name == "skill"));
    }

    #[tokio::test]
    async fn build_tools_returns_the_pre_step_skill_handle() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let skill_path = home.path().join(".dsh/skills/prestep/SKILL.md");
        std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        std::fs::write(
            &skill_path,
            "---\nname: prestep\ndescription: Pre-step skill\n---\n\nFollow it.\n",
        )
        .unwrap();
        let setups = HarnessSetupsConfig::default();

        let (tools, skill_tool) = Workspace::build_tools_with_skills(
            home.path(),
            root.path(),
            setups.get("standard").unwrap(),
        );
        assert!(tools.specs().iter().any(|spec| spec.name == "skill"));
        let entries = skill_tool
            .expect("standard setup retains its pre-step skill handle")
            .catalog_entries()
            .await
            .unwrap();
        assert_eq!(
            entries,
            vec![SkillCatalogEntry {
                name: "prestep".into(),
                description: "Pre-step skill".into(),
            }]
        );

        let (_, missing) = Workspace::build_tools_with_skills(
            home.path(),
            root.path(),
            setups.get("research").unwrap(),
        );
        assert!(missing.is_none());
    }

    #[test]
    fn skill_row_model_is_replay_stable_and_tracks_dsh_lifecycle() {
        let arguments = serde_json::json!({"name": "dsh-manage-issues"});
        let running = Workspace::skill_row_model("call-skill", &arguments, None);
        assert_eq!(running.name, "dsh-manage-issues");
        assert_eq!(running.state, SkillRowState::Running);
        assert_eq!(running.output, None);
        assert_eq!(running.error_summary, None);

        let output = "<skill_content name=\"dsh-manage-issues\">Instructions</skill_content>";
        let ok = Workspace::skill_row_model("call-skill", &arguments, Some((output, true)));
        assert_eq!(ok.state, SkillRowState::Ok);
        assert_eq!(ok.output.as_deref(), Some(output));
        assert_eq!(ok.error_summary, None);

        let error_text = "SkillError: missing resource\nCheck SKILL.md.";
        let error = Workspace::skill_row_model("call-skill", &arguments, Some((error_text, false)));
        assert_eq!(error.state, SkillRowState::Error);
        assert_eq!(
            error.error_summary.as_deref(),
            Some("SkillError: missing resource")
        );
        assert_eq!(error.output.as_deref(), Some(error_text));

        let stopped =
            Workspace::skill_row_model("call-skill", &arguments, Some(("turn cancelled", false)));
        assert_eq!(stopped.state, SkillRowState::Stopped);
    }

    #[test]
    fn skill_row_name_falls_back_to_durable_call_data() {
        assert_eq!(
            Workspace::skill_call_name(
                "call-skill",
                &serde_json::json!({"name": "dsh-manage-issues"})
            ),
            "dsh-manage-issues"
        );
        assert_eq!(
            Workspace::skill_call_name("call-skill", &serde_json::json!({"name": ""})),
            "{\"name\":\"\"}"
        );
        assert_eq!(
            Workspace::skill_call_name("call-skill", &serde_json::json!("raw-name")),
            "\"raw-name\""
        );
        assert_eq!(
            Workspace::skill_call_name("call-skill", &serde_json::json!({"name": "first\nsecond"})),
            "first"
        );
    }

    #[test]
    fn skill_icon_is_available_at_comet_tool_row_scale() {
        let assets = crate::icons::Assets;
        let bytes = assets
            .load(icons::SKILL)
            .expect("skill icon asset resolves")
            .expect("skill icon asset exists");
        let text = std::str::from_utf8(&bytes).expect("skill icon is utf-8");
        assert!(text.contains("viewBox=\"0 0 16 16\""));
        assert!(text.contains("12.5113 15.4067"));
    }

    #[test]
    fn todo_call_summary_matches_comet_transcript_chips() {
        let arguments = serde_json::json!({
            "todos": [
                {"content": "port domain", "status": "completed"},
                {"content": "port panel", "status": "in_progress"},
                {"content": "verify", "status": "pending"}
            ]
        });

        assert_eq!(
            Workspace::todo_call_summary(&arguments),
            Some("1/3 done".to_string())
        );
        assert_eq!(Workspace::todo_call_summary(&serde_json::json!({})), None);
    }

    #[test]
    fn todo_panel_progress_follows_dsh_status_order() {
        let todos = vec![
            TodoItem {
                content: "done".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "active".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "pending".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "active two".into(),
                status: TodoStatus::InProgress,
            },
        ];

        assert_eq!(
            Workspace::todo_panel_progress(&todos),
            "1 completed\u{2002}·\u{2002}2 in progress\u{2002}·\u{2002}1 pending".to_string()
        );
        assert_eq!(Workspace::todo_panel_progress(&[]), String::new());
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

    #[test]
    fn sidebar_rows_filter_spaces_and_resolve_display_names() {
        use chrono::TimeZone;

        let local = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let spaces = SpacesConfig::parse(&format!(
            r#"{{
                "spaces": [
                    {{"id":"local","name":"Local harness","root":{:?}}},
                    {{"id":"project","name":"Project","root":{:?}}}
                ]
            }}"#,
            local.path(),
            project.path()
        ))
        .unwrap();
        let setups = HarnessSetupsConfig::default();
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap();
        let summary = |space_id: Option<&str>, minutes_ago: i64, working: bool| SessionSummary {
            id: uuid::Uuid::new_v4(),
            title: format!("{space_id:?} session"),
            model: Some("deepseek-chat".into()),
            space_id: space_id.map(str::to_string),
            harness_id: Some("standard".into()),
            event_count: 3,
            turn_active: working,
            updated_at: now - chrono::Duration::minutes(minutes_ago),
        };
        let summaries = vec![
            summary(Some("local"), 61, false),
            summary(Some("project"), 2, true),
            summary(None, 1, false),
        ];

        let rows =
            Workspace::sidebar_session_rows(&summaries, &spaces, &setups, Some("project"), now);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].space_name, "Project");
        assert_eq!(rows[0].harness_name, "Standard");
        assert_eq!(rows[0].time_ago, "2m");
        assert!(rows[0].working);
    }

    #[test]
    fn spaces_menu_filters_projects_and_keeps_add_last() {
        let local = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let spaces = SpacesConfig::parse(&format!(
            r#"{{
                "spaces": [
                    {{"id":"local","name":"Local harness","root":{:?}}},
                    {{"id":"project","name":"Project","root":{:?}}}
                ]
            }}"#,
            local.path(),
            project.path()
        ))
        .unwrap();

        let empty = Workspace::spaces_menu_rows(&spaces, "");
        assert_eq!(
            empty,
            vec![
                SpacesMenuRow::All,
                SpacesMenuRow::Space("local".into()),
                SpacesMenuRow::Space("project".into()),
                SpacesMenuRow::AddSpace,
            ]
        );

        let filtered = Workspace::spaces_menu_rows(&spaces, "PRO");
        assert_eq!(
            filtered,
            vec![
                SpacesMenuRow::Space("project".into()),
                SpacesMenuRow::AddSpace,
            ]
        );
    }

    #[test]
    fn spaces_menu_navigation_wraps_in_both_directions() {
        assert_eq!(Workspace::spaces_menu_step(None, 3, 1), Some(0));
        assert_eq!(Workspace::spaces_menu_step(Some(0), 3, 1), Some(1));
        assert_eq!(Workspace::spaces_menu_step(Some(2), 3, 1), Some(0));
        assert_eq!(Workspace::spaces_menu_step(Some(0), 3, -1), Some(2));
        assert_eq!(Workspace::spaces_menu_step(Some(1), 3, -1), Some(0));
    }

    #[test]
    fn project_registration_persists_a_new_space() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let path = dir.path().join("spaces.json");
        let mut config = SpacesConfig::local(&workspace);

        let id = Workspace::register_project_space(
            &mut config,
            &path,
            "Project",
            project.to_str().unwrap(),
        )
        .expect("project registration succeeds");

        assert_eq!(config.get(&id).unwrap().name(), "Project");
        assert_eq!(
            SpacesConfig::load(&path).unwrap().get(&id).unwrap().name(),
            "Project"
        );
    }

    #[test]
    fn project_registration_rolls_back_when_publish_fails() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        let mut config = SpacesConfig::local(&workspace);
        let before = config.clone();

        let error = Workspace::register_project_space(
            &mut config,
            &blocked.join("spaces.json"),
            "Project",
            project.to_str().unwrap(),
        )
        .expect_err("save path is blocked");

        assert!(error.contains("Could not publish project"));
        assert_eq!(config, before);
    }
}
