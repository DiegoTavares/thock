//! The Agent panel (V5 spec §6.3): a right-dock panel hosting the user's own
//! CLI agent in real terminals, one per session. Thock never speaks to
//! the model — it launches a user-owned console program in the vault root and
//! gets out of the way. The panel also hosts the guided "Connect your agent"
//! flow (§6.2).

use anyhow::Result;
use editor::Editor;
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    Action, App, AsyncWindowContext, Context, DismissEvent, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, KeyContext, Pixels, SharedString, Subscription, WeakEntity, Window,
    actions, div, px,
};
use picker::{Picker, PickerDelegate};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use terminal::Terminal;
use terminal_view::TerminalView;
use ui::prelude::*;
use ui::{
    Button, ButtonStyle, Checkbox, Divider, HighlightedLabel, Icon, IconButton, Label, ListItem,
    ListItemSpacing, ToggleState, Tooltip,
};
use util::ResultExt as _;
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::{ModalView, Workspace};

use crate::agent::{self, ConnectedAgent, KnownAgent};
use crate::vault::{Vault, VaultStatus};

const AGENT_PANEL_KEY: &str = "ThockAgentPanel";

actions!(
    thock,
    [
        /// Toggles focus on the Thock agent panel.
        ToggleAgentFocus,
        /// Starts a new conversation with the connected agent.
        NewConversation,
        /// Switches to the next conversation in the Agent panel.
        ActivateNextAgentSession,
        /// Switches to the previous conversation in the Agent panel.
        ActivatePreviousAgentSession,
        /// Closes the conversation shown in the Agent panel, ending the
        /// agent running in it.
        CloseAgentSession,
        /// Opens the guided flow for connecting a CLI agent.
        ConnectAgent,
        /// Sets the language your notes and your agent use, translating the
        /// vault's templates and docs with your agent.
        SetLanguage,
        /// Asks what your weeks are made of and writes it to profile.md, so
        /// the rituals fit your life instead of the defaults.
        SetProfile,
        /// Changes how Thock looks and which keys do what, with your agent
        /// editing the settings and keyboard files for you.
        CustomizeApp,
        /// Brings the rituals you edited up to date with what Thock now
        /// ships, keeping your changes; you approve each file.
        UpdateRituals,
        /// Files what your agent noted about you lately into memory/, so the
        /// next session starts knowing it.
        Reflect,
        /// Reads through the notes you already have and builds memory/ from
        /// them, a fortnight at a time. Can be stopped and resumed.
        RebuildMemory
    ]
);

/// Runs an installed Routine skill with your connected agent. Without data
/// it opens the skill picker; a keybinding can pass a skill id directly (V7
/// §7.5), e.g. `["thock::RunSkill", { "skill": "wrap-today" }]`.
#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = thock)]
#[serde(deny_unknown_fields)]
pub struct RunSkill {
    #[serde(default)]
    pub skill: Option<String>,
}

/// Bumped whenever the connect flow saves a launch command, so other panels
/// (the Getting started checklist) re-check the connection without polling
/// the settings files (V18 §5.4).
#[derive(Default)]
pub struct ConnectionEpoch(pub usize);

impl gpui::Global for ConnectionEpoch {}

pub fn init(cx: &mut App) {
    cx.set_global(ConnectionEpoch::default());
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleAgentFocus, window, cx| {
            workspace.toggle_panel_focus::<AgentPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &NewConversation, window, cx| {
            AgentPanel::launch_in_workspace(workspace, LaunchRequest::conversation(), window, cx);
        });
        workspace.register_action(|workspace, _: &ConnectAgent, window, cx| {
            if let Some(panel) = workspace.focus_panel::<AgentPanel>(window, cx) {
                panel.update(cx, |panel, cx| panel.open_connect(window, cx));
            }
        });
        workspace.register_action(|workspace, action: &RunSkill, window, cx| {
            match action.skill.as_deref() {
                Some(skill_id) => run_skill_by_id(workspace, skill_id, window, cx),
                None => toggle_run_skill_picker(workspace, window, cx),
            }
        });
        workspace.register_action(|workspace, _: &SetLanguage, window, cx| {
            run_core_skill(
                workspace,
                "Set Language",
                crate::routines::SET_LANGUAGE_SKILL_PATH,
                agent::ModelTier::Default,
                "This workspace isn't a Thock vault, so there is no language to set.",
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &SetProfile, window, cx| {
            run_core_skill(
                workspace,
                "Set Profile",
                crate::routines::SET_PROFILE_SKILL_PATH,
                agent::ModelTier::Default,
                "This workspace isn't a Thock vault, so there is no profile to set.",
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &CustomizeApp, window, cx| {
            run_core_skill(
                workspace,
                "Customize App",
                crate::routines::CUSTOMIZE_APP_SKILL_PATH,
                agent::ModelTier::Default,
                "This workspace isn't a Thock vault, so the customize ritual isn't here.",
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &UpdateRituals, window, cx| {
            run_core_skill(
                workspace,
                "Update Rituals",
                crate::routines::UPDATE_RITUALS_SKILL_PATH,
                agent::ModelTier::Default,
                "This workspace isn't a Thock vault, so there are no rituals to update.",
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &Reflect, window, cx| {
            run_core_skill(
                workspace,
                "Reflect",
                crate::memory::REFLECT_SKILL_PATH,
                agent::ModelTier::Fast,
                "This workspace isn't a Thock vault, so there are no notes to reflect on.",
                window,
                cx,
            );
        });
        workspace.register_action(|workspace, _: &RebuildMemory, window, cx| {
            run_core_skill(
                workspace,
                "Rebuild Memory",
                crate::memory::REBUILD_MEMORY_SKILL_PATH,
                agent::ModelTier::Fast,
                "This workspace isn't a Thock vault, so there are no notes to read.",
                window,
                cx,
            );
        });
    })
    .detach();
}

/// The `thock::RunSkill { skill }` keybinding path: launch an enabled
/// Routine's skill by its manifest id. Unknown or disabled ids get a
/// non-blocking toast, never a panic (V7 §7.5).
fn run_skill_by_id(
    workspace: &mut Workspace,
    skill_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let root = workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
    let Some(VaultStatus::Valid(vault)) = root.as_deref().map(Vault::detect) else {
        workspace.show_error(
            "This workspace isn't a Thock vault, so there are no skills to run.".to_string(),
            cx,
        );
        return;
    };
    let skill = crate::routines::enabled_routine_manifests(&vault)
        .into_iter()
        .flat_map(|manifest| manifest.skills)
        .find(|skill| skill.id == skill_id);
    match skill {
        Some(skill) => AgentPanel::launch_in_workspace(
            workspace,
            LaunchRequest::run_skill(&skill.name, &skill.file, skill.model),
            window,
            cx,
        ),
        None => workspace.show_error(
            format!("No enabled Routine skill {skill_id:?} in this vault."),
            cx,
        ),
    }
}

/// Launches a core ritual, one that belongs to the vault itself rather than
/// to any Routine (Set Language, V19 §5.3; Set Profile, V23 §5.2). Available
/// in any vault, any time: both are re-runnable by design.
fn run_core_skill(
    workspace: &mut Workspace,
    title: &'static str,
    skill_path: &'static str,
    tier: agent::ModelTier,
    not_a_vault_message: &'static str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let root = workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
    if !matches!(
        root.as_deref().map(Vault::detect),
        Some(VaultStatus::Valid(_))
    ) {
        workspace.show_error(not_a_vault_message.to_string(), cx);
        return;
    }
    AgentPanel::launch_in_workspace(
        workspace,
        LaunchRequest::run_skill(title, skill_path, tier),
        window,
        cx,
    );
}

/// One agent action to launch in a fresh terminal tab (spec locked decision
/// 3: fresh process per action; continuity is the CLI's own `/resume`).
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRequest {
    /// Tab title — the action that launched it ("Wrap Today", "Conversation").
    pub title: String,
    /// The kickoff prompt passed as a launch argument. `None` for ad-hoc
    /// conversations: the CLI starts idle.
    pub kickoff: Option<String>,
    /// The model tier the skill declared; conversations and onboarding run
    /// on the default tier.
    pub tier: agent::ModelTier,
}

impl LaunchRequest {
    pub fn conversation() -> Self {
        Self {
            title: "Conversation".to_string(),
            kickoff: None,
            tier: agent::ModelTier::Default,
        }
    }

    pub fn run_skill(skill_name: &str, vault_relative_path: &str, tier: agent::ModelTier) -> Self {
        Self {
            title: skill_name.to_string(),
            kickoff: Some(agent::run_skill_kickoff(vault_relative_path)),
            tier,
        }
    }
}

struct AgentSession {
    id: usize,
    title: SharedString,
    terminal_view: Entity<TerminalView>,
    _subscriptions: Vec<Subscription>,
}

/// The connect flow's transient state, alive while the flow is on screen.
struct ConnectFlow {
    /// `None` while the PATH scan is still running.
    detected: Option<Vec<KnownAgent>>,
    command_editor: Entity<Editor>,
    only_this_vault: bool,
}

enum PanelView {
    Sessions,
    Connect(ConnectFlow),
}

pub struct AgentPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    position: DockPosition,
    vault_status: VaultStatus,
    /// The resolved launch command, recomputed when the vault changes or a
    /// connection is saved. `None` = not connected.
    connected: Option<ConnectedAgent>,
    sessions: Vec<AgentSession>,
    active_session: usize,
    next_session_id: usize,
    view: PanelView,
    /// A launch requested while unconnected, continued after the connect flow
    /// succeeds (spec §6.4: connect, then continue the original action).
    pending_launch: Option<LaunchRequest>,
    _subscriptions: Vec<Subscription>,
}

impl AgentPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            AgentPanel::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = workspace.weak_handle();
        cx.new(|cx| {
            let project_subscription = cx.subscribe(&project, |this: &mut Self, _, event, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreeUpdatedEntries(..)
                ) {
                    this.refresh_vault_status(cx);
                }
            });
            let mut this = Self {
                workspace: weak_workspace,
                project,
                focus_handle: cx.focus_handle(),
                position: DockPosition::Right,
                vault_status: VaultStatus::NotAVault,
                connected: None,
                sessions: Vec::new(),
                active_session: 0,
                next_session_id: 0,
                view: PanelView::Sessions,
                pending_launch: None,
                _subscriptions: vec![project_subscription],
            };
            this.refresh_vault_status(cx);
            this
        })
    }

    /// Opens the panel and launches `request`, routing through the connect
    /// flow first when no agent is configured. The one entry point for every
    /// Run/onboarding/conversation action. When the hosted Thock Agent is the
    /// chosen connection (V25 item 9), the launch goes to the chat panel
    /// instead; the terminal rails below are untouched either way.
    pub fn launch_in_workspace(
        workspace: &mut Workspace,
        request: LaunchRequest,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        if crate::chat_panel::hosted_mode_active(workspace, cx) {
            crate::chat_panel::ChatPanel::launch_in_workspace(workspace, request, window, cx);
            return;
        }
        let Some(panel) = workspace.panel::<AgentPanel>(cx) else {
            log::warn!("Thock: the Agent panel isn't registered yet; launch dropped");
            return;
        };
        workspace.open_panel::<AgentPanel>(window, cx);
        panel.update(cx, |panel, cx| panel.launch(request, window, cx));
    }

    fn vault(&self) -> Option<&Vault> {
        match &self.vault_status {
            VaultStatus::Valid(vault) => Some(vault),
            _ => None,
        }
    }

    fn refresh_vault_status(&mut self, cx: &mut Context<Self>) {
        let root = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
        let status = match root {
            Some(root) => Vault::detect(&root),
            None => VaultStatus::NotAVault,
        };
        if status != self.vault_status {
            self.vault_status = status;
            self.refresh_connected(cx);
        }
    }

    /// Re-resolves the launch command off the UI thread and repaints. The
    /// vault side comes from panel state; the global default is re-read from
    /// disk.
    fn refresh_connected(&mut self, cx: &mut Context<Self>) {
        let vault = self.vault().cloned();
        let resolve = cx.background_spawn(async move { agent::resolved_command(vault.as_ref()) });
        cx.spawn(async move |this, cx| {
            let connected = resolve.await;
            this.update(cx, |this, cx| {
                this.connected = connected;
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    fn show_error(&self, message: String, cx: &mut Context<Self>) {
        // Deferred because this is reached synchronously from `launch`, whose
        // callers (action handlers, `TimelinePanel::run_skill`) hold the
        // workspace lease — updating the workspace here would double-lease
        // and panic.
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(message, cx))
                .log_err();
        });
    }

    pub fn launch(&mut self, request: LaunchRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(vault) = self.vault() else {
            self.show_error(
                "Open a Thock vault to use your agent — sessions run in the vault folder."
                    .to_string(),
                cx,
            );
            return;
        };
        let vault_root = vault.root.clone();
        // Resolve at launch time (not from the cached copy) so an edit to
        // either config file is honored without reopening anything.
        let Some(connected) = agent::resolved_command(self.vault()) else {
            self.pending_launch = Some(request);
            self.open_connect(window, cx);
            return;
        };
        self.connected = Some(connected.clone());
        let launch = match agent::build_launch(
            &connected.command_for(request.tier),
            request.kickoff.as_deref(),
        ) {
            Ok(launch) => launch,
            Err(error) => {
                self.show_error(format!("Couldn't launch the agent: {error}"), cx);
                return;
            }
        };

        // Pre-session checkpoint (spec §6.5): a soft dependency — the history
        // service no-ops when unavailable, and the launch never waits on it.
        crate::history::checkpoint_before_ai_write(&self.project, cx);

        self.spawn_session(request.title, launch, vault_root, window, cx);
    }

    fn spawn_session(
        &mut self,
        title: String,
        launch: agent::AgentLaunch,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_id = self.next_session_id;
        self.next_session_id += 1;

        let spawn = task::SpawnInTerminal {
            id: task::TaskId(format!("thock-agent-{session_id}")),
            full_label: title.clone(),
            label: title.clone(),
            command_label: shlex::try_join(
                std::iter::once(launch.program.as_str())
                    .chain(launch.args.iter().map(String::as_str)),
            )
            .unwrap_or_else(|_| launch.program.clone()),
            command: Some(launch.program),
            args: launch.args,
            cwd: Some(cwd),
            // Clean exit auto-closes the tab; a failure keeps the scrollback
            // (spec locked decision 13). The terminal emits `CloseTerminal`
            // only on exit status 0 with this strategy.
            hide: task::HideStrategy::OnSuccess,
            show_rerun: false,
            ..Default::default()
        };
        let terminal_task = self
            .project
            .update(cx, |project, cx| project.create_terminal_task(spawn, cx));

        cx.spawn_in(window, async move |this, cx| {
            let terminal = match terminal_task.await {
                Ok(terminal) => terminal,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.show_error(format!("Couldn't start the agent: {error}"), cx);
                    })
                    .ok();
                    return Err(error);
                }
            };
            this.update_in(cx, |this, window, cx| {
                this.add_session(session_id, title, terminal, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    fn add_session(
        &mut self,
        session_id: usize,
        title: String,
        terminal: Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let project = self.project.downgrade();
        let terminal_view = cx.new(|cx| {
            let mut view =
                TerminalView::new(terminal.clone(), workspace, None, project, window, cx);
            view.set_show_workspace_actions(false, cx);
            view.set_custom_title(Some(title.clone()), cx);
            view
        });
        let close_subscription = cx.subscribe(
            &terminal,
            move |this: &mut Self, _, event: &terminal::Event, cx| {
                if matches!(event, terminal::Event::CloseTerminal) {
                    this.remove_session(session_id, cx);
                }
            },
        );
        self.sessions.push(AgentSession {
            id: session_id,
            title: title.into(),
            terminal_view: terminal_view.clone(),
            _subscriptions: vec![close_subscription],
        });
        self.active_session = self.sessions.len() - 1;
        self.view = PanelView::Sessions;
        window.focus(&terminal_view.focus_handle(cx), cx);
        cx.notify();
    }

    /// Removes a session tab. Dropping the last handle to the terminal entity
    /// shuts its process down — standard terminal semantics for a manual
    /// close; for a clean exit the process is already gone.
    fn remove_session(&mut self, session_id: usize, cx: &mut Context<Self>) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return;
        };
        self.sessions.remove(index);
        if self.active_session >= self.sessions.len() {
            self.active_session = self.sessions.len().saturating_sub(1);
        }
        cx.notify();
    }

    pub fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(
                "Custom command, e.g. my-agent --profile personal",
                window,
                cx,
            );
            if let Some(connected) = &self.connected {
                editor.set_text(connected.command.clone(), window, cx);
            }
            editor
        });
        self.view = PanelView::Connect(ConnectFlow {
            detected: None,
            command_editor,
            only_this_vault: false,
        });
        let scan = cx.background_spawn(async move { agent::detect_installed_agents() });
        cx.spawn(async move |this, cx| {
            let detected = scan.await;
            this.update(cx, |this, cx| {
                if let PanelView::Connect(flow) = &mut this.view {
                    flow.detected = Some(detected);
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
        cx.notify();
    }

    fn cancel_connect(&mut self, cx: &mut Context<Self>) {
        self.view = PanelView::Sessions;
        self.pending_launch = None;
        cx.notify();
    }

    /// `enter` anywhere in the connect flow connects with the field's
    /// command; single-line editors don't consume enter, so the action
    /// bubbles here from the focused command field. With no session open it
    /// presses the empty state's primary button. A running session's
    /// terminal keeps its own enter.
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(flow) => {
                let command = flow.command_editor.read(cx).text(cx);
                self.save_connection(command, window, cx);
            }
            PanelView::Sessions if self.sessions.is_empty() => {
                if self.connected.is_some() {
                    self.launch(LaunchRequest::conversation(), window, cx);
                } else {
                    self.open_connect(window, cx);
                    if let PanelView::Connect(flow) = &self.view {
                        window.focus(&flow.command_editor.focus_handle(cx), cx);
                    }
                }
            }
            PanelView::Sessions => cx.propagate(),
        }
    }

    /// `escape` backs out of the connect flow, and from the empty state hands
    /// focus back to the note. A running session's terminal keeps its own
    /// escape.
    fn cancel_view(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        match &self.view {
            PanelView::Connect(_) => self.cancel_connect(cx),
            PanelView::Sessions if self.sessions.is_empty() => {
                let workspace = self.workspace.clone();
                cx.defer_in(window, move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            if let Some(item) = workspace.active_item(cx) {
                                item.item_focus_handle(cx).focus(window, cx);
                            }
                        })
                        .log_err();
                });
            }
            PanelView::Sessions => cx.propagate(),
        }
    }

    fn activate_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(index) else {
            return;
        };
        self.active_session = index;
        window.focus(&session.terminal_view.focus_handle(cx), cx);
        cx.notify();
    }

    fn activate_next_session(
        &mut self,
        _: &ActivateNextAgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.is_empty() {
            return;
        }
        let next = (self.active_session + 1) % self.sessions.len();
        self.activate_session(next, window, cx);
    }

    fn activate_previous_session(
        &mut self,
        _: &ActivatePreviousAgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.is_empty() {
            return;
        }
        let previous = (self.active_session + self.sessions.len() - 1) % self.sessions.len();
        self.activate_session(previous, window, cx);
    }

    fn close_active_session(
        &mut self,
        _: &CloseAgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self
            .sessions
            .get(self.active_session)
            .map(|session| session.id)
        else {
            return;
        };
        self.remove_session(session_id, cx);
        // The focused terminal just went away; keep the keyboard in the panel.
        if self.sessions.is_empty() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.activate_session(self.active_session, window, cx);
        }
    }

    fn save_connection(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        let command = command.trim().to_string();
        if command.is_empty() {
            self.show_error("Enter a command to connect an agent.".to_string(), cx);
            return;
        }
        // Validate the shape now; whether the command actually works is shown
        // honestly by the first launched terminal (spec §6.2).
        if let Err(error) = agent::build_launch(&command, Some("kickoff")) {
            self.show_error(format!("That command can't be used: {error}"), cx);
            return;
        }
        let only_this_vault = match &self.view {
            PanelView::Connect(flow) => flow.only_this_vault,
            PanelView::Sessions => false,
        };
        let vault_root = self.vault().map(|vault| vault.root.clone());
        let save = cx.background_spawn(async move {
            match (only_this_vault, vault_root) {
                (true, Some(root)) => crate::vault::update_agent_command(&root, Some(command)),
                _ => agent::save_global_command(&command),
            }
        });
        cx.spawn_in(window, async move |this, cx| match save.await {
            Ok(()) => this.update_in(cx, |this, window, cx| {
                this.view = PanelView::Sessions;
                this.refresh_vault_status(cx);
                this.refresh_connected(cx);
                // A global-default save writes outside the vault, so nothing
                // else would tell the Getting started checklist (V18 §5.4).
                cx.update_global::<ConnectionEpoch, ()>(|epoch, _| epoch.0 += 1);
                if let Some(request) = this.pending_launch.take() {
                    this.launch(request, window, cx);
                }
                cx.notify();
            }),
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.show_error(format!("Couldn't save the connection: {error}"), cx);
                })?;
                Err(error)
            }
        })
        .detach_and_log_err(cx);
    }

    /// The connect flow (V5 §6.2), laid out as one choice with one outcome:
    /// the detected agents and the custom command are alternative ways to
    /// fill the same launch command, and a single **Connect** applies it.
    /// Picking a detected agent fills the field rather than saving directly,
    /// so a row's selected state, the field, and the footer never disagree.
    fn render_connect(&self, flow: &ConnectFlow, cx: &Context<Self>) -> AnyElement {
        let editor = flow.command_editor.clone();
        let current_command = editor.read(cx).text(cx).trim().to_string();
        let section_caption = |label: &'static str| {
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .into_any_element()
        };
        let mut content = v_flex()
            .gap_2()
            .p_3()
            .child(Label::new("Connect your agent").size(LabelSize::Large))
            .child(
                Label::new(
                    "Pick the AI assistant Thock should run beside your notes. \
                     It runs in a terminal here, under its own account. Thock \
                     never talks to a model itself.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            );

        content = match &flow.detected {
            None => content.child(
                Label::new("Looking for agents on this computer…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
            Some(detected) if detected.is_empty() => content.child(
                Label::new(
                    "No agents were found on this computer. Type the command \
                     that starts yours below.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            ),
            Some(detected) => content.child(
                v_flex()
                    .gap_1()
                    .child(section_caption("Found on this computer"))
                    .children(detected.iter().map(|agent| {
                        let program = agent.program;
                        let selected = current_command == program;
                        ListItem::new(ElementId::Name(SharedString::from(format!(
                            "thock-connect-{program}"
                        ))))
                        .toggle_state(selected)
                        .start_slot(
                            Icon::new(IconName::Terminal)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Label::new(agent.display_name))
                        .end_slot(if selected {
                            Icon::new(IconName::Check)
                                .size(IconSize::Small)
                                .color(Color::Accent)
                                .into_any_element()
                        } else {
                            Label::new(program)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .into_any_element()
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if let PanelView::Connect(flow) = &this.view {
                                    flow.command_editor.update(cx, |editor, cx| {
                                        editor.set_text(program, window, cx);
                                    });
                                    cx.notify();
                                }
                            },
                        ))
                    })),
            ),
        };

        let custom_caption = match &flow.detected {
            Some(detected) if detected.is_empty() => "Custom command",
            _ => "Or a custom command",
        };
        content
            .child(div().mt_1().child(section_caption(custom_caption)))
            .child(
                div()
                    .child(editor.clone())
                    .px_1()
                    .py_1()
                    .border_1()
                    .rounded_sm(),
            )
            .child(
                Label::new(format!(
                    "Thock starts your agent with this command and tells it what \
                     to do by adding one final argument, or by filling in \
                     {} wherever you put it.",
                    agent::PROMPT_PLACEHOLDER
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
            .child(div().mt_1().child(Divider::horizontal()))
            .child(
                h_flex()
                    .justify_between()
                    .child(if self.vault().is_some() {
                        let only_this_vault = flow.only_this_vault;
                        Checkbox::new(
                            "thock-connect-only-vault",
                            if only_this_vault {
                                ToggleState::Selected
                            } else {
                                ToggleState::Unselected
                            },
                        )
                        .label("Only for this vault")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            if let PanelView::Connect(flow) = &mut this.view {
                                flow.only_this_vault = !flow.only_this_vault;
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Button::new("thock-connect-cancel", "Cancel").on_click(
                                cx.listener(|this, _, _window, cx| this.cancel_connect(cx)),
                            ))
                            .child(
                                Button::new("thock-connect-save", "Connect")
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let command = editor.read(cx).text(cx);
                                        this.save_connection(command, window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The no-sessions state: a centered column, buttons contained at a fixed
    /// width so they read as buttons rather than list rows.
    fn render_empty_state(&self, cx: &Context<Self>) -> AnyElement {
        let content = match &self.connected {
            Some(connected) => {
                let source_caption = match connected.source {
                    agent::CommandSource::Vault => "Connected agent · this vault",
                    agent::CommandSource::Global => "Connected agent",
                };
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::XLarge)
                            .color(Color::Muted),
                    )
                    .child(div().mt_1().child(Label::new(connected.command.clone())))
                    .child(
                        Label::new(source_caption)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        v_flex()
                            .mt_3()
                            .gap_2()
                            .w(rems(13.))
                            .child(
                                Button::new("thock-new-conversation", "New Conversation")
                                    .style(ButtonStyle::Filled)
                                    .full_width()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.launch(LaunchRequest::conversation(), window, cx);
                                    })),
                            )
                            .child(
                                Button::new("thock-reconnect", "Change Agent…")
                                    .style(ButtonStyle::Outlined)
                                    .full_width()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_connect(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div().mt_3().max_w(rems(16.)).child(
                            Label::new("Run skills from the Routines panel.")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                    )
            }
            None => v_flex()
                .items_center()
                .gap_1()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XLarge)
                        .color(Color::Muted),
                )
                .child(div().mt_1().child(Label::new("No agent connected")))
                .child(
                    div().max_w(rems(16.)).child(
                        Label::new(
                            "Thock launches your own CLI agent — Claude Code, \
                             Gemini, Codex — in a terminal beside your notes.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
                )
                .child(
                    div().mt_3().w(rems(13.)).child(
                        Button::new("thock-connect", "Connect Your Agent")
                            .style(ButtonStyle::Filled)
                            .full_width()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_connect(window, cx)),
                            ),
                    ),
                ),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_4()
            .child(content)
            .into_any_element()
    }

    fn render_sessions(&self, cx: &Context<Self>) -> AnyElement {
        let Some(active) = self.sessions.get(self.active_session) else {
            return self.render_empty_state(cx);
        };
        let tabs = h_flex()
            .w_full()
            .gap_1()
            .px_1()
            .py_1()
            .flex_wrap()
            .children(self.sessions.iter().enumerate().map(|(index, session)| {
                let session_id = session.id;
                let is_active = index == self.active_session;
                h_flex()
                    .id(ElementId::Name(SharedString::from(format!(
                        "thock-agent-tab-{session_id}"
                    ))))
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active, |tab| {
                        tab.bg(cx.theme().colors().tab_active_background)
                    })
                    .child(
                        Label::new(session.title.clone())
                            .size(LabelSize::Small)
                            .color(if is_active {
                                Color::Default
                            } else {
                                Color::Muted
                            }),
                    )
                    .child(
                        IconButton::new(
                            ElementId::Name(SharedString::from(format!(
                                "thock-agent-tab-close-{session_id}"
                            ))),
                            IconName::Close,
                        )
                        .icon_size(IconSize::XSmall)
                        .icon_color(Color::Muted)
                        .tooltip(Tooltip::text("Close session"))
                        .on_click(cx.listener(
                            move |this, _, _window, cx| {
                                this.remove_session(session_id, cx);
                            },
                        )),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(index) = this
                            .sessions
                            .iter()
                            .position(|session| session.id == session_id)
                        {
                            this.activate_session(index, window, cx);
                        }
                    }))
            }))
            .child(
                IconButton::new("thock-agent-new-tab", IconName::Plus)
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("New conversation"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch(LaunchRequest::conversation(), window, cx);
                    })),
            );
        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(active.terminal_view.clone()))
            .into_any_element()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match (&self.view, &self.vault_status) {
            (PanelView::Connect(flow), _) => self.render_connect(flow, cx),
            (PanelView::Sessions, VaultStatus::Valid(_)) => self.render_sessions(cx),
            (PanelView::Sessions, _) => v_flex()
                .p_3()
                .child(
                    Label::new("Open a Thock vault to use your agent.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        };
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("ThockAgentPanel");
        // The connect flow and the empty state are menu-shaped: enter acts,
        // escape backs out. A running session is not: its terminal needs
        // every key, so the menu bindings must not shadow it.
        let menu_shaped = match self.view {
            PanelView::Connect(_) => true,
            PanelView::Sessions => self.sessions.is_empty(),
        };
        if menu_shaped {
            key_context.add("menu");
        }
        v_flex()
            .id("thock-agent-panel")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel_view))
            .on_action(cx.listener(Self::activate_next_session))
            .on_action(cx.listener(Self::activate_previous_session))
            .on_action(cx.listener(Self::close_active_session))
            .size_full()
            .child(content)
    }
}

impl EventEmitter<PanelEvent> for AgentPanel {}

impl Focusable for AgentPanel {
    /// Dock activation and `ToggleAgentFocus` focus whatever this returns, so
    /// delegate to the running TUI (or the connect flow's command field) —
    /// focusing the panel wrapper would swallow keystrokes meant for the
    /// terminal.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.view {
            PanelView::Connect(flow) => flow.command_editor.focus_handle(cx),
            PanelView::Sessions => match self.sessions.get(self.active_session) {
                Some(session) => session.terminal_view.focus_handle(cx),
                None => self.focus_handle.clone(),
            },
        }
    }
}

impl Panel for AgentPanel {
    fn persistent_name() -> &'static str {
        "Thock Agent Panel"
    }

    fn panel_key() -> &'static str {
        AGENT_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(480.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Agent Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        ToggleAgentFocus.boxed_clone()
    }

    fn activation_priority(&self) -> u32 {
        // Must be unique across all panels; 0-8 are taken (see the Timeline
        // and Day Planner panels and upstream).
        9
    }
}

/// One runnable skill offered by the `thock: run skill` palette action.
/// Command palette entries can't be minted per skill at runtime (actions are
/// static types), so a single action opens this fuzzy picker over every
/// installed skill instead.
struct RunnableSkill {
    /// "Wrap Today — Daily & Weekly" (what the picker matches against).
    label: String,
    skill_name: String,
    /// Vault-relative skill file path.
    file: String,
    tier: agent::ModelTier,
    summary: String,
}

fn runnable_skills(vault: &Vault) -> Vec<RunnableSkill> {
    crate::routines::enabled_routine_manifests(vault)
        .into_iter()
        .flat_map(|manifest| {
            let routine_name = manifest.name.clone();
            manifest.skills.into_iter().map(move |skill| RunnableSkill {
                label: format!("{} — {}", skill.name, routine_name),
                skill_name: skill.name,
                file: skill.file,
                tier: skill.model,
                summary: skill.summary,
            })
        })
        .collect()
}

fn toggle_run_skill_picker(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let root = workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
    let skills = match root.as_deref().map(Vault::detect) {
        Some(VaultStatus::Valid(vault)) => runnable_skills(&vault),
        _ => {
            workspace.show_error(
                "This workspace isn't a Thock vault, so there are no skills to run.".to_string(),
                cx,
            );
            return;
        }
    };
    if skills.is_empty() {
        workspace.show_error(
            "No Routine skills are installed in this vault.".to_string(),
            cx,
        );
        return;
    }
    let weak_workspace = workspace.weak_handle();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate = RunSkillDelegate {
            picker_entity: cx.entity().downgrade(),
            workspace: weak_workspace,
            skills,
            matches: Vec::new(),
            selected_index: 0,
        };
        RunSkillPicker::new(delegate, window, cx)
    });
}

pub struct RunSkillPicker {
    picker: Entity<Picker<RunSkillDelegate>>,
}

impl RunSkillPicker {
    fn new(delegate: RunSkillDelegate, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for RunSkillPicker {}
impl EventEmitter<DismissEvent> for RunSkillPicker {}

impl Focusable for RunSkillPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for RunSkillPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RunSkillPicker")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

pub struct RunSkillDelegate {
    picker_entity: WeakEntity<RunSkillPicker>,
    workspace: WeakEntity<Workspace>,
    skills: Vec<RunnableSkill>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl PickerDelegate for RunSkillDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "run skill"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Run a skill with your agent…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> gpui::Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .skills
            .iter()
            .enumerate()
            .map(|(id, skill)| StringMatchCandidate::new(id, &skill.label))
            .collect::<Vec<_>>();
        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .map(|candidate| StringMatch {
                        candidate_id: candidate.id,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };
            this.update(cx, |this, cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = this
                    .delegate
                    .selected_index
                    .min(this.delegate.matches.len().saturating_sub(1));
                cx.notify();
            })
            .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let request = self
            .matches
            .get(self.selected_index)
            .and_then(|mat| self.skills.get(mat.candidate_id))
            .map(|skill| LaunchRequest::run_skill(&skill.skill_name, &skill.file, skill.tier));
        if let Some(request) = request {
            self.workspace
                .update(cx, |workspace, cx| {
                    AgentPanel::launch_in_workspace(workspace, request, window, cx);
                })
                .log_err();
        }
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let skill_match = self.matches.get(index)?;
        let skill = self.skills.get(skill_match.candidate_id)?;
        let mut item = ListItem::new(index)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(HighlightedLabel::new(
                skill_match.string.clone(),
                skill_match.positions.clone(),
            ));
        if !skill.summary.is_empty() {
            item = item.tooltip(Tooltip::text(skill.summary.clone()));
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            // The shipped keymap, so a missing or shadowed binding fails here.
            let key_bindings: Vec<KeyBinding> = match KeymapFile::load(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            ) {
                KeymapFileLoadResult::Success { key_bindings }
                | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                KeymapFileLoadResult::JsonParseFailure { error } => {
                    panic!("bad keymap: {error}")
                }
            };
            cx.bind_keys(key_bindings);
        });
    }

    struct Setup {
        panel: Entity<AgentPanel>,
        editor: Entity<Editor>,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault with a note open and the Agent panel docked and focused, with
    /// no agent connected whatever this machine's global config says.
    async fn setup(cx: &mut TestAppContext) -> Setup {
        init_test(cx);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "note.md": "# Note\n",
            }),
        )
        .await;
        let project = Project::test(fs, [root], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path(root.join("note.md"), cx)
            })
            .unwrap();
        let editor = workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        let panel = workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = AgentPanel::new(workspace, window, cx);
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        cx.run_until_parked();
        panel.update(&mut cx, |panel, cx| {
            panel.connected = None;
            cx.notify();
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.toggle_panel_focus::<AgentPanel>(window, cx);
        });
        cx.run_until_parked();
        Setup {
            panel,
            editor,
            cx,
            _vault_dir: vault_dir,
        }
    }

    /// Opens a session on a display-only terminal: no process, same tab.
    fn add_test_session(setup: &mut Setup, title: &str) {
        let terminal = setup.cx.update(|window, cx| {
            let window_id = window.window_handle().window_id().as_u64();
            cx.new(|cx| {
                terminal::TerminalBuilder::new_display_only(
                    terminal::terminal_settings::CursorShape::default(),
                    terminal::terminal_settings::AlternateScroll::On,
                    None,
                    window_id,
                    cx.background_executor(),
                    util::paths::PathStyle::local(),
                )
                .subscribe(cx)
            })
        });
        let title = title.to_string();
        setup.panel.update_in(&mut setup.cx, |panel, window, cx| {
            let session_id = panel.next_session_id;
            panel.next_session_id += 1;
            panel.add_session(session_id, title, terminal, window, cx);
        });
        setup.cx.run_until_parked();
    }

    fn active_title(setup: &Setup) -> Option<String> {
        setup.panel.read_with(&setup.cx, |panel, _| {
            panel
                .sessions
                .get(panel.active_session)
                .map(|session| session.title.to_string())
        })
    }

    fn active_terminal_focused(setup: &mut Setup) -> bool {
        let panel = setup.panel.clone();
        setup.cx.update(|window, cx| {
            let panel = panel.read(cx);
            panel
                .sessions
                .get(panel.active_session)
                .is_some_and(|session| {
                    session
                        .terminal_view
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                })
        })
    }

    #[gpui::test]
    async fn enter_on_the_empty_state_starts_connecting(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        setup.cx.simulate_keystrokes("enter");
        setup.cx.run_until_parked();
        let panel = setup.panel.clone();
        let command_field_focused = setup.cx.update(|window, cx| {
            let PanelView::Connect(flow) = &panel.read(cx).view else {
                return false;
            };
            flow.command_editor.focus_handle(cx).is_focused(window)
        });
        assert!(
            command_field_focused,
            "enter opens the connect flow, ready to type"
        );

        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        assert!(
            setup.panel.read_with(&setup.cx, |panel, _| matches!(
                panel.view,
                PanelView::Sessions
            )),
            "escape backs out of the connect flow"
        );
    }

    #[gpui::test]
    async fn escape_on_the_empty_state_returns_to_the_note(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        setup.cx.simulate_keystrokes("escape");
        setup.cx.run_until_parked();
        let editor = setup.editor.clone();
        assert!(
            setup
                .cx
                .update(|window, cx| editor.focus_handle(cx).contains_focused(window, cx))
        );
    }

    #[gpui::test]
    async fn sessions_switch_and_close_from_the_keyboard(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        add_test_session(&mut setup, "First");
        add_test_session(&mut setup, "Second");
        assert_eq!(active_title(&setup).as_deref(), Some("Second"));
        assert!(active_terminal_focused(&mut setup));

        setup.cx.simulate_keystrokes("ctrl-pageup");
        assert_eq!(active_title(&setup).as_deref(), Some("First"));
        assert!(active_terminal_focused(&mut setup));
        setup.cx.simulate_keystrokes("ctrl-pagedown");
        assert_eq!(active_title(&setup).as_deref(), Some("Second"));
        setup.cx.simulate_keystrokes("ctrl-pagedown");
        assert_eq!(active_title(&setup).as_deref(), Some("First"), "wraps");

        setup.cx.dispatch_action(CloseAgentSession);
        setup.cx.run_until_parked();
        assert_eq!(active_title(&setup).as_deref(), Some("Second"));
        assert!(
            active_terminal_focused(&mut setup),
            "the keyboard stays in the panel"
        );
    }
}
