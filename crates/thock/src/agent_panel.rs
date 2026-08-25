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
    FocusHandle, Focusable, Pixels, SharedString, Subscription, WeakEntity, Window, actions, div,
    px,
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
        /// Opens the guided flow for connecting a CLI agent.
        ConnectAgent
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

pub fn init(cx: &mut App) {
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

    pub fn run_skill(
        skill_name: &str,
        vault_relative_path: &str,
        tier: agent::ModelTier,
    ) -> Self {
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
    /// Run/onboarding/conversation action.
    pub fn launch_in_workspace(
        workspace: &mut Workspace,
        request: LaunchRequest,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
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

    fn render_connect(&self, flow: &ConnectFlow, cx: &Context<Self>) -> AnyElement {
        let editor = flow.command_editor.clone();
        let mut content = v_flex()
            .gap_2()
            .p_3()
            .child(Label::new("Connect your agent").size(LabelSize::Large))
            .child(
                Label::new(
                    "Thock launches your own CLI agent in a terminal — \
                     it never talks to a model itself.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            );

        content = match &flow.detected {
            None => content.child(
                Label::new("Looking for installed agents…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
            Some(detected) if detected.is_empty() => content.child(
                Label::new("No known agents found on PATH. Enter a command below.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
            Some(detected) => content.children(detected.iter().map(|agent| {
                let program = agent.program;
                Button::new(
                    ElementId::Name(SharedString::from(format!("thock-connect-{program}"))),
                    format!("{} ({program})", agent.display_name),
                )
                .style(ButtonStyle::Filled)
                .full_width()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.save_connection(program.to_string(), window, cx);
                }))
            })),
        };

        content
            .child(Divider::horizontal())
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
                    "The kickoff prompt is appended as the last argument, or replaces \
                     {} if present.",
                    agent::PROMPT_PLACEHOLDER
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
            .when(self.vault().is_some(), |this| {
                let only_this_vault = flow.only_this_vault;
                this.child(
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
                    })),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("thock-connect-save", "Save")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let command = editor.read(cx).text(cx);
                                this.save_connection(command, window, cx);
                            })),
                    )
                    .child(
                        Button::new("thock-connect-cancel", "Cancel")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_connect(cx))),
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
                            this.active_session = index;
                            let view = this.sessions[index].terminal_view.clone();
                            window.focus(&view.focus_handle(cx), cx);
                            cx.notify();
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
        v_flex()
            .id("thock-agent-panel")
            .key_context("ThockAgentPanel")
            .track_focus(&self.focus_handle)
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
                "This workspace isn't a Thock vault, so there are no skills to run."
                    .to_string(),
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
